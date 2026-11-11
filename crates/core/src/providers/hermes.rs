//! Hermes Agent provider (Nous Research) via the Hermes API server's Runs API.
//!
//! Hermes runs shell commands only through our MCP server's `run_command`
//! (its own shell/file toolsets must be disabled). Hermes may still raise
//! `approval.request` events; those are answered through [`CommandService`].
//! Rule: when in doubt, never approve.
//!
//! Payload field names inside `message.delta` and `approval.request`, and the
//! shape of `/v1/toolsets`, are not documented; they are parsed tolerantly.

use super::anthropic::{SseEvent, SseParser};
use super::{ChatProvider, ProviderError, ProviderTurn, Result};
use crate::command_service::{Authorization, CommandService};
use crate::history::{to_hermes, trim_history, DEFAULT_MAX_HISTORY_CHARS};
use crate::mcp_server::McpServerHandle;
use crate::store::Approval;
use crate::{Availability, ChatEvent, ProviderId, TurnRequest};
use async_trait::async_trait;
use futures_core::Stream;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use url::Url;

/// Hermes toolsets that must be disabled: they would let Hermes act on the
/// machine without going through our safety gate.
pub const DEFAULT_FORBIDDEN_TOOLSETS: &[&str] = &[
    "terminal",
    "code_execution",
    "file",
    "browser",
    "computer_use",
    "connections",
    "catalog",
    "cronjob",
];

const INSTRUCTIONS: &str = "Use the run_command tool from the assistant MCP server for any shell command. Commands are reviewed and may be denied by the user.";

const NO_KEY: &str = "ยังไม่ได้ตั้ง API key ของ Hermes";
const NO_SERVER: &str = "Hermes ไม่ได้เปิด API server";
const BUSY: &str = "Hermes กำลังทำงานอื่นอยู่";
const INTERRUPTED: &str = "Hermes หยุดทำงานกลางคัน";
const INCOMPLETE_APPROVAL: &str = "คำขออนุมัติจาก Hermes ไม่ครบ";
const EDITED_NOTE: &str = "\n[คำสั่งที่แก้ไขไม่ถูกส่งให้ Hermes เพราะ Hermes จะรันคำสั่งเดิม]\n";
const STREAM_ENDED: &str = "การเชื่อมต่อกับ Hermes ขาดก่อนจบงาน";

const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
const TOOLSETS_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_PERMIT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY: usize = 2048;

/// One meaningful event from a Hermes run stream.
#[derive(Debug, Clone, PartialEq)]
pub enum HermesEvent {
    Delta(String),
    ApprovalRequest {
        request_id: String,
        execution_generation: u64,
        command: String,
    },
    /// An `approval.request` missing its id, generation or command.
    IncompleteApproval,
    Completed,
    Interrupted,
    Other,
}

/// Parse a complete SSE buffer. Comment lines (`: keepalive`) are skipped.
pub fn parse_sse(buf: &str) -> Vec<HermesEvent> {
    SseParser::default()
        .push(buf.as_bytes())
        .into_iter()
        .map(to_hermes_event)
        .collect()
}

fn first_str<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}

/// String id; numbers are accepted and stringified.
fn first_id(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| match v.get(*k)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn first_u64(v: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| match v.get(*k)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    })
}

fn approval_event(v: &Value) -> HermesEvent {
    let request_id = first_id(v, &["request_id", "id", "prompt_id"]);
    let generation = first_u64(v, &["execution_generation", "generation"]);
    let command = first_str(v, &["command", "preview", "description"])
        .or_else(|| {
            v.get("args")
                .and_then(|a| a.get("command"))
                .and_then(Value::as_str)
        })
        .filter(|c| !c.trim().is_empty());
    match (request_id, generation, command) {
        (Some(request_id), Some(execution_generation), Some(command)) => {
            HermesEvent::ApprovalRequest {
                request_id,
                execution_generation,
                command: command.to_string(),
            }
        }
        _ => HermesEvent::IncompleteApproval,
    }
}

fn to_hermes_event(ev: SseEvent) -> HermesEvent {
    let data: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
    // Tolerate a data-only stream carrying the event name in the payload.
    let name = ev
        .event
        .clone()
        .or_else(|| first_str(&data, &["event", "type"]).map(str::to_string));
    match name.as_deref() {
        Some("message.delta") => match first_str(&data, &["delta", "text", "content"]) {
            Some(t) if !t.is_empty() => HermesEvent::Delta(t.to_string()),
            _ => HermesEvent::Other,
        },
        Some("message.interim") => {
            let not_streamed = data.get("already_streamed").and_then(Value::as_bool) == Some(false);
            match data.get("text").and_then(Value::as_str) {
                Some(t) if not_streamed && !t.is_empty() => HermesEvent::Delta(t.to_string()),
                _ => HermesEvent::Other,
            }
        }
        Some("approval.request") => approval_event(&data),
        Some("run.completed") => HermesEvent::Completed,
        Some("run.interrupted") => HermesEvent::Interrupted,
        _ => HermesEvent::Other,
    }
}

/// Interpret `/v1/toolsets` as `(name, enabled)` pairs. Accepts an array of
/// `{name, enabled}`, `{toolsets: [...]}`, or a map `{name: bool | {enabled: bool}}`.
fn parse_toolsets(v: &Value) -> std::result::Result<Vec<(String, bool)>, String> {
    let list = match v {
        Value::Array(items) => items
            .iter()
            .map(|it| {
                let name = it.get("name").and_then(Value::as_str);
                let enabled = it.get("enabled").and_then(Value::as_bool);
                match (name, enabled) {
                    (Some(n), Some(e)) => Ok((n.to_string(), e)),
                    _ => Err(format!("unrecognized toolset entry {it}")),
                }
            })
            .collect::<std::result::Result<Vec<_>, _>>()?,
        Value::Object(map) => {
            if let Some(inner) = map.get("toolsets") {
                return match inner {
                    Value::Array(_) => parse_toolsets(inner),
                    _ => Err("`toolsets` is not a list".into()),
                };
            }
            map.iter()
                .map(|(name, val)| {
                    let enabled = match val {
                        Value::Bool(b) => Some(*b),
                        Value::Object(_) => val.get("enabled").and_then(Value::as_bool),
                        _ => None,
                    };
                    enabled
                        .map(|e| (name.clone(), e))
                        .ok_or_else(|| format!("unrecognized value for toolset {name}"))
                })
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
        _ => return Err("unexpected response shape".into()),
    };
    if list.is_empty() {
        return Err("empty toolset list".into());
    }
    Ok(list)
}

/// Truncate to at most `max` bytes on a char boundary.
fn bounded(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn http_error(status: reqwest::StatusCode, body: &str, api_key: &str) -> String {
    let body = bounded(body, MAX_ERROR_BODY);
    let body = if api_key.is_empty() {
        body.to_string()
    } else {
        body.replace(api_key, "***")
    };
    format!("Hermes API error {}: {}", status.as_u16(), body)
}

pub struct HermesProvider {
    base_url: Url,
    api_key: Option<String>,
    mcp: McpServerHandle,
    commands: Arc<CommandService>,
    forbidden_toolsets: Vec<String>,
    client: reqwest::Client,
    permits: Arc<Semaphore>,
    permit_timeout: Duration,
}

impl HermesProvider {
    /// `base_url` is the API server's `/v1` URL, e.g. `http://127.0.0.1:8642/v1`.
    pub fn new(
        base_url: Url,
        api_key: Option<String>,
        mcp: McpServerHandle,
        commands: Arc<CommandService>,
        forbidden_toolsets: Vec<String>,
    ) -> Self {
        let mut base_url = base_url;
        if !base_url.path().ends_with('/') {
            let p = format!("{}/", base_url.path());
            base_url.set_path(&p);
        }
        Self {
            base_url,
            api_key,
            mcp,
            commands,
            forbidden_toolsets,
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            permits: Arc::new(Semaphore::new(1)),
            permit_timeout: DEFAULT_PERMIT_TIMEOUT,
        }
    }

    /// How long `start` waits for a running Hermes turn to end (default 30s).
    pub fn with_permit_timeout(mut self, timeout: Duration) -> Self {
        self.permit_timeout = timeout;
        self
    }

    fn key(&self) -> Option<&str> {
        self.api_key.as_deref().filter(|k| !k.is_empty())
    }

    fn url(&self, rel: &str) -> std::result::Result<Url, String> {
        self.base_url
            .join(rel)
            .map_err(|e| format!("bad base url: {e}"))
    }

    /// Err with a user-facing message when a forbidden toolset is enabled or
    /// the listing cannot be interpreted (fail closed).
    pub async fn check_toolsets(&self) -> std::result::Result<(), String> {
        let fail = |reason: String| format!("ตรวจสอบเครื่องมือของ Hermes ไม่ได้: {reason}");
        let url = self.url("toolsets").map_err(fail)?;
        let mut req = self.client.get(url).timeout(TOOLSETS_TIMEOUT);
        if let Some(k) = self.key() {
            req = req.bearer_auth(k);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| fail(e.without_url().to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(fail(format!("HTTP {}", status.as_u16())));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| fail(e.without_url().to_string()))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| fail(e.to_string()))?;
        let toolsets = parse_toolsets(&v).map_err(fail)?;
        let enabled: Vec<&str> = self
            .forbidden_toolsets
            .iter()
            .filter(|f| {
                toolsets
                    .iter()
                    .any(|(n, on)| *on && n.eq_ignore_ascii_case(f))
            })
            .map(String::as_str)
            .collect();
        if enabled.is_empty() {
            Ok(())
        } else {
            Err(format!("ต้องปิด toolset: {}", enabled.join(", ")))
        }
    }

    async fn health_ok(&self) -> bool {
        let Ok(url) = self.base_url.join("/health") else {
            return false;
        };
        let mut req = self.client.get(url).timeout(HEALTH_TIMEOUT);
        if let Some(k) = self.key() {
            req = req.bearer_auth(k);
        }
        matches!(
            tokio::time::timeout(HEALTH_TIMEOUT, req.send()).await,
            Ok(Ok(r)) if r.status().is_success()
        )
    }
}

#[async_trait]
impl ChatProvider for HermesProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Hermes
    }

    async fn availability(&self) -> Availability {
        if self.key().is_none() {
            return Availability::Unavailable(NO_KEY.into());
        }
        if !self.health_ok().await {
            return Availability::Unavailable(NO_SERVER.into());
        }
        match self.check_toolsets().await {
            Ok(()) => Availability::Ready,
            Err(m) => Availability::Unavailable(m),
        }
    }

    async fn start(&self, req: TurnRequest) -> Result<Box<dyn ProviderTurn>> {
        let api_key = self
            .key()
            .ok_or_else(|| ProviderError::Other(NO_KEY.into()))?
            .to_string();
        if self.mcp.context(&req.turn_token).is_none() {
            return Err(ProviderError::Other(
                "turn is not registered with the MCP server".into(),
            ));
        }
        let permit =
            tokio::time::timeout(self.permit_timeout, self.permits.clone().acquire_owned())
                .await
                .ok()
                .and_then(|p| p.ok())
                .ok_or_else(|| ProviderError::Other(BUSY.into()))?;
        self.check_toolsets().await.map_err(ProviderError::Other)?;

        let (history, input) = to_hermes(&trim_history(&req.history, DEFAULT_MAX_HISTORY_CHARS));
        let mut turn = HermesTurn {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            api_key,
            mcp: self.mcp.clone(),
            commands: self.commands.clone(),
            token: req.turn_token.clone(),
            run_id: None,
            run_live: false,
            active_set: false,
            stream: None,
            parser: SseParser::default(),
            events: VecDeque::new(),
            out: VecDeque::new(),
            pending: None,
            finished: false,
            _permit: permit,
        };
        // Bind Hermes MCP calls to this turn before the run can call tools.
        self.mcp
            .set_active_hermes_turn(Some(req.turn_token.clone()));
        turn.active_set = true;

        let body = json!({
            "input": input,
            "conversation_history": history,
            "session_id": format!("conv-{}", req.conversation_id),
            "instructions": INSTRUCTIONS,
        });
        match turn.open(body).await {
            Ok(()) => {}
            Err(msg) => turn.fail_and_stop(msg),
        }
        Ok(Box::new(turn))
    }
}

type ByteStream = Pin<Box<dyn Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>;

/// Result of a background step (approval answer or stop).
struct Step {
    events: Vec<ChatEvent>,
    finish: bool,
}

/// Everything a background task needs to talk to the run.
#[derive(Clone)]
struct RunApi {
    client: reqwest::Client,
    base_url: Url,
    api_key: String,
    run_id: String,
}

impl RunApi {
    fn url(&self, tail: &str) -> std::result::Result<Url, String> {
        self.base_url
            .join(&format!("runs/{}/{tail}", self.run_id))
            .map_err(|e| format!("bad base url: {e}"))
    }

    /// Best effort.
    async fn stop(&self) {
        let Ok(url) = self.url("stop") else {
            return;
        };
        let send = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .timeout(STOP_TIMEOUT)
            .send();
        let _ = tokio::time::timeout(STOP_TIMEOUT, send).await;
    }

    /// Ok(true) answered, Ok(false) stale (409), Err otherwise.
    async fn answer(
        &self,
        request_id: &str,
        generation: u64,
        choice: &str,
    ) -> std::result::Result<bool, String> {
        let url = self.url("approval")?;
        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&json!({
                "request_id": request_id,
                "execution_generation": generation,
                "choice": choice,
            }))
            .send()
            .await
            .map_err(|e| format!("request failed: {}", e.without_url()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::CONFLICT {
            return Ok(false);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(http_error(status, &text, &self.api_key));
        }
        Ok(true)
    }
}

/// Authorize the command through our gate and answer Hermes. Runs in its own
/// task so a dropped `next()` neither re-asks the user nor answers twice.
async fn handle_approval(
    api: RunApi,
    mcp: McpServerHandle,
    commands: Arc<CommandService>,
    token: String,
    request_id: String,
    generation: u64,
    command: String,
) -> Step {
    let mut events = Vec::new();
    let mut approved = None;
    let choice = match mcp.context(&token) {
        None => "deny",
        Some(ctx) => {
            let auth = commands.authorize(&ctx, &command).await;
            match &auth {
                Authorization::Approved { approval, .. } if *approval != Approval::UserEdited => {
                    approved = Some((ctx, auth));
                    "once"
                }
                Authorization::Approved { .. } => {
                    // Hermes would run its original text, not the edit.
                    events.push(ChatEvent::TextDelta(EDITED_NOTE.into()));
                    "deny"
                }
                Authorization::Denied => "deny",
            }
        }
    };
    match api.answer(&request_id, generation, choice).await {
        Ok(true) => {
            if let Some((ctx, auth)) = approved {
                commands.record_external(&ctx, &auth).await;
            }
            Step {
                events,
                finish: false,
            }
        }
        Ok(false) => Step {
            events,
            finish: false,
        },
        Err(e) => {
            api.stop().await;
            events.push(ChatEvent::Error(format!("ตอบคำขออนุมัติของ Hermes ไม่ได้: {e}")));
            Step {
                events,
                finish: true,
            }
        }
    }
}

struct HermesTurn {
    client: reqwest::Client,
    base_url: Url,
    api_key: String,
    mcp: McpServerHandle,
    commands: Arc<CommandService>,
    token: String,
    run_id: Option<String>,
    /// The run exists on Hermes and has not completed or been stopped.
    run_live: bool,
    /// We set the MCP active Hermes turn and have not cleared it yet.
    active_set: bool,
    stream: Option<ByteStream>,
    parser: SseParser,
    /// Parsed but not yet handled events.
    events: VecDeque<HermesEvent>,
    /// Events ready for the caller.
    out: VecDeque<ChatEvent>,
    /// Background step whose result must be applied before reading on.
    pending: Option<JoinHandle<Step>>,
    finished: bool,
    _permit: OwnedSemaphorePermit,
}

impl HermesTurn {
    fn api(&self) -> Option<RunApi> {
        Some(RunApi {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            run_id: self.run_id.clone()?,
        })
    }

    /// Create the run and open its event stream.
    async fn open(&mut self, body: Value) -> std::result::Result<(), String> {
        let url = self
            .base_url
            .join("runs")
            .map_err(|e| format!("bad base url: {e}"))?;
        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("request failed: {}", e.without_url()))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(http_error(status, &text, &self.api_key));
        }
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let run_id = first_id(&v, &["run_id", "id"])
            .ok_or_else(|| "Hermes did not return a run_id".to_string())?;
        self.run_id = Some(run_id);
        self.run_live = true;

        let api = self.api().ok_or("run id missing")?;
        let resp = self
            .client
            .get(api.url("events")?)
            .bearer_auth(&self.api_key)
            .header("accept", "text/event-stream")
            .send()
            .await
            .map_err(|e| format!("request failed: {}", e.without_url()))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(http_error(status, &text, &self.api_key));
        }
        self.stream = Some(Box::pin(resp.bytes_stream()));
        Ok(())
    }

    fn clear_active(&mut self) {
        if self.active_set {
            self.active_set = false;
            if self.mcp.active_hermes_turn().as_deref() == Some(self.token.as_str()) {
                self.mcp.set_active_hermes_turn(None);
            }
        }
    }

    /// End the turn. Queued output is kept.
    fn finish(&mut self) {
        self.finished = true;
        self.stream = None;
        self.events.clear();
        self.clear_active();
    }

    /// End the turn with an error, stopping the run first (in the background,
    /// so a dropped `next()` cannot skip the stop).
    fn fail_and_stop(&mut self, msg: String) {
        let api = if self.run_live { self.api() } else { None };
        self.run_live = false;
        self.finish();
        self.pending = Some(tokio::spawn(async move {
            if let Some(api) = api {
                api.stop().await;
            }
            Step {
                events: vec![ChatEvent::Error(msg)],
                finish: true,
            }
        }));
    }

    fn handle(&mut self, ev: HermesEvent) {
        match ev {
            HermesEvent::Delta(t) => self.out.push_back(ChatEvent::TextDelta(t)),
            HermesEvent::Completed => {
                self.run_live = false;
                self.out.push_back(ChatEvent::Done);
                self.finish();
            }
            HermesEvent::Interrupted => {
                self.run_live = false;
                self.out.push_back(ChatEvent::Error(INTERRUPTED.into()));
                self.finish();
            }
            HermesEvent::IncompleteApproval => self.fail_and_stop(INCOMPLETE_APPROVAL.into()),
            HermesEvent::ApprovalRequest {
                request_id,
                execution_generation,
                command,
            } => {
                let Some(api) = self.api() else {
                    self.fail_and_stop("run id missing".into());
                    return;
                };
                self.pending = Some(tokio::spawn(handle_approval(
                    api,
                    self.mcp.clone(),
                    self.commands.clone(),
                    self.token.clone(),
                    request_id,
                    execution_generation,
                    command,
                )));
            }
            HermesEvent::Other => {}
        }
    }
}

#[async_trait]
impl ProviderTurn for HermesTurn {
    async fn next(&mut self) -> Option<ChatEvent> {
        use futures_util::StreamExt;
        loop {
            if let Some(ev) = self.out.pop_front() {
                return Some(ev);
            }
            if let Some(handle) = self.pending.as_mut() {
                // Awaiting by reference: dropping this future leaves the task running.
                let res = handle.await;
                self.pending = None;
                match res {
                    Ok(step) => {
                        self.out.extend(step.events);
                        if step.finish {
                            self.run_live = false;
                            self.finish();
                        }
                    }
                    Err(e) => self.fail_and_stop(format!("Hermes approval task failed: {e}")),
                }
                continue;
            }
            if self.finished {
                return None;
            }
            if let Some(ev) = self.events.pop_front() {
                self.handle(ev);
                continue;
            }
            let Some(stream) = self.stream.as_mut() else {
                self.fail_and_stop(STREAM_ENDED.into());
                continue;
            };
            match stream.next().await {
                Some(Ok(bytes)) => {
                    let parsed = self.parser.push(&bytes);
                    self.events.extend(parsed.into_iter().map(to_hermes_event));
                }
                Some(Err(e)) => {
                    self.fail_and_stop(format!("Hermes stream error: {}", e.without_url()))
                }
                None => self.fail_and_stop(STREAM_ENDED.into()),
            }
        }
    }

    async fn submit_command_result(&mut self, _call_id: &str, _text: String) -> Result<()> {
        Ok(())
    }

    async fn cancel(&mut self) {
        if let Some(h) = self.pending.take() {
            h.abort();
        }
        let api = if self.run_live { self.api() } else { None };
        self.run_live = false;
        self.out.clear();
        self.finish();
        if let Some(api) = api {
            api.stop().await;
        }
    }
}

impl Drop for HermesTurn {
    fn drop(&mut self) {
        if let Some(h) = self.pending.take() {
            h.abort();
        }
        if self.run_live {
            if let (Some(api), Ok(rt)) = (self.api(), tokio::runtime::Handle::try_current()) {
                rt.spawn(async move { api.stop().await });
            }
        }
        self.clear_active();
    }
}
