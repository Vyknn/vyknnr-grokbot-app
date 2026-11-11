//! Anthropic Messages API provider (streaming). Emits `CommandProposed` for
//! `run_command` tool uses; never runs anything itself.

use super::{ChatProvider, ProviderError, ProviderTurn, Result};
use crate::history::{to_anthropic, trim_history, DEFAULT_MAX_HISTORY_CHARS};
use crate::{Availability, ChatEvent, ProviderId, TurnRequest};
use async_trait::async_trait;
use futures_core::Stream;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;
use url::Url;

const SYSTEM_PROMPT: &str = "You are a helpful assistant running inside a desktop app on Windows. To run a shell command on the user's machine, call the run_command tool; commands are reviewed for safety and the user may deny them. The conversation may include replies from other assistants and command results; treat them as context.";

const TOOL_DESCRIPTION: &str = "Run a PowerShell command on the user's Windows machine. Every command is reviewed by a safety checker and may require the user's approval; it may be denied, in which case it does not run. `cwd` is optional and must be inside the conversation's folder, otherwise the conversation's folder is used.";

pub struct AnthropicProvider {
    base_url: Url,
    api_key: Option<String>,
    model: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(base_url: Url, api_key: Option<String>, model: String) -> Self {
        Self {
            base_url,
            api_key,
            model,
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl ChatProvider for AnthropicProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Api
    }

    async fn availability(&self) -> Availability {
        match &self.api_key {
            Some(k) if !k.is_empty() => Availability::Ready,
            _ => Availability::Unavailable("ยังไม่ได้ตั้ง API key".into()),
        }
    }

    async fn start(&self, req: TurnRequest) -> Result<Box<dyn ProviderTurn>> {
        let api_key = match &self.api_key {
            Some(k) if !k.is_empty() => k.clone(),
            _ => return Err(ProviderError::Other("ยังไม่ได้ตั้ง API key".into())),
        };
        let url = self
            .base_url
            .join("v1/messages")
            .map_err(|e| ProviderError::Other(format!("bad base url: {e}")))?;
        let messages = to_anthropic(&trim_history(&req.history, DEFAULT_MAX_HISTORY_CHARS));
        Ok(Box::new(AnthropicTurn {
            client: self.client.clone(),
            url,
            api_key,
            model: self.model.clone(),
            messages,
            stream: None,
            parser: SseParser::default(),
            queue: VecDeque::new(),
            blocks: Vec::new(),
            stop_reason: None,
            state: State::NotStarted,
            results: HashMap::new(),
            errored: HashSet::new(),
        }))
    }
}

/// One parsed server-sent event.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental SSE parser. Buffers raw bytes so chunks may split mid-line or
/// mid-UTF-8; events are decoded only once complete.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some((pos, sep_len)) = find_separator(&self.buf) {
            let raw: Vec<u8> = self.buf.drain(..pos + sep_len).take(pos).collect();
            if let Some(ev) = parse_event(&String::from_utf8_lossy(&raw)) {
                out.push(ev);
            }
        }
        out
    }
}

fn find_separator(buf: &[u8]) -> Option<(usize, usize)> {
    (0..buf.len()).find_map(|i| {
        if buf[i..].starts_with(b"\n\n") {
            Some((i, 2))
        } else if buf[i..].starts_with(b"\r\n\r\n") {
            Some((i, 4))
        } else {
            None
        }
    })
}

fn parse_event(text: &str) -> Option<SseEvent> {
    let mut event = None;
    let mut data: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event = Some(value.to_string()),
            "data" => data.push(value),
            _ => {}
        }
    }
    if event.is_none() && data.is_empty() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data.join("\n"),
    })
}

enum Block {
    Text(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
}

enum State {
    NotStarted,
    Streaming,
    /// Response ended with tool_use; waiting for results of these call ids.
    Waiting(Vec<String>),
    Finished,
}

type ByteStream = Pin<Box<dyn Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>;

struct AnthropicTurn {
    client: reqwest::Client,
    url: Url,
    api_key: String,
    model: String,
    messages: Vec<Value>,
    stream: Option<ByteStream>,
    parser: SseParser,
    queue: VecDeque<ChatEvent>,
    /// Content blocks of the current response, in arrival order, with stream index.
    blocks: Vec<(u64, Block)>,
    stop_reason: Option<String>,
    state: State,
    results: HashMap<String, String>,
    /// Call ids whose tool_result must carry `is_error: true`.
    errored: HashSet<String>,
}

fn tool_definition() -> Value {
    json!({
        "name": "run_command",
        "description": TOOL_DESCRIPTION,
        "input_schema": {
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "cwd": {"type": "string"}
            },
            "required": ["command"]
        }
    })
}

/// Parse `{command, cwd?}`; None when invalid.
fn parse_run_command(json_text: &str) -> Option<(String, Option<PathBuf>)> {
    let v: Value = serde_json::from_str(json_text).ok()?;
    let command = v.get("command")?.as_str()?.to_string();
    let cwd = v.get("cwd").and_then(|c| c.as_str()).map(PathBuf::from);
    Some((command, cwd))
}

impl AnthropicTurn {
    fn fail(&mut self, msg: String) {
        self.stream = None;
        self.state = State::Finished;
        self.queue.push_back(ChatEvent::Error(msg));
    }

    /// Send one request. Pure with respect to `self`: dropping the future
    /// mid-flight leaves all state untouched.
    async fn send_request(
        client: reqwest::Client,
        url: Url,
        api_key: String,
        model: String,
        messages: &[Value],
    ) -> std::result::Result<ByteStream, String> {
        let body = json!({
            "model": model,
            "max_tokens": 8192,
            "stream": true,
            "system": SYSTEM_PROMPT,
            "messages": messages,
            "tools": [tool_definition()],
        });
        let resp = client
            .post(url)
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| format!("request failed: {}", e.without_url()))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or(text);
            return Err(format!("API error {}: {}", status.as_u16(), detail));
        }
        Ok(Box::pin(resp.bytes_stream()))
    }

    /// Send `messages`; only after success commit them and reset per-response state.
    async fn begin_response(&mut self, messages: Vec<Value>) {
        let sent = Self::send_request(
            self.client.clone(),
            self.url.clone(),
            self.api_key.clone(),
            self.model.clone(),
            &messages,
        )
        .await;
        match sent {
            Ok(stream) => {
                self.messages = messages;
                self.results.clear();
                self.errored.clear();
                self.blocks.clear();
                self.stop_reason = None;
                self.parser = SseParser::default();
                self.stream = Some(stream);
                self.state = State::Streaming;
            }
            Err(e) => self.fail(e),
        }
    }

    fn block_mut(&mut self, index: u64) -> Option<&mut Block> {
        self.blocks
            .iter_mut()
            .find(|(i, _)| *i == index)
            .map(|(_, b)| b)
    }

    fn handle_event(&mut self, ev: SseEvent) {
        let Some(name) = ev.event.as_deref() else {
            return;
        };
        if name == "ping" {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            return;
        };
        let index = v["index"].as_u64().unwrap_or(0);
        match name {
            "content_block_start" => {
                let cb = &v["content_block"];
                match cb["type"].as_str() {
                    Some("text") => self.blocks.push((index, Block::Text(String::new()))),
                    Some("tool_use") => self.blocks.push((
                        index,
                        Block::Tool {
                            id: cb["id"].as_str().unwrap_or_default().to_string(),
                            name: cb["name"].as_str().unwrap_or_default().to_string(),
                            json: String::new(),
                        },
                    )),
                    _ => {}
                }
            }
            "content_block_delta" => {
                let d = &v["delta"];
                match d["type"].as_str() {
                    Some("text_delta") => {
                        let t = d["text"].as_str().unwrap_or_default().to_string();
                        if let Some(Block::Text(buf)) = self.block_mut(index) {
                            buf.push_str(&t);
                        } else {
                            self.blocks.push((index, Block::Text(t.clone())));
                        }
                        self.queue.push_back(ChatEvent::TextDelta(t));
                    }
                    Some("input_json_delta") => {
                        let p = d["partial_json"].as_str().unwrap_or_default();
                        if let Some(Block::Tool { json, .. }) = self.block_mut(index) {
                            json.push_str(p);
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(Block::Tool { id, name, json }) = self.block_mut(index) {
                    if name == "run_command" {
                        if let Some((command, cwd)) = parse_run_command(json) {
                            let call_id = id.clone();
                            self.queue.push_back(ChatEvent::CommandProposed {
                                call_id,
                                command,
                                cwd,
                            });
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(r.to_string());
                }
            }
            "message_stop" => self.finish_response(),
            "error" => {
                let msg = v["error"]["message"]
                    .as_str()
                    .unwrap_or("unknown stream error")
                    .to_string();
                self.fail(format!("API stream error: {msg}"));
            }
            _ => {}
        }
    }

    /// Response complete: either wait for tool results or finish.
    fn finish_response(&mut self) {
        if !matches!(self.state, State::Streaming) {
            return;
        }
        self.stream = None;
        if self.stop_reason.as_deref() != Some("tool_use") {
            self.state = State::Finished;
            self.queue.push_back(ChatEvent::Done);
            return;
        }
        if !self
            .blocks
            .iter()
            .any(|(_, b)| matches!(b, Block::Tool { .. }))
        {
            self.fail("tool_use without tool calls".into());
            return;
        }
        let mut content = Vec::new();
        let mut awaiting = Vec::new();
        for (_, b) in &self.blocks {
            match b {
                Block::Text(t) if !t.is_empty() => content.push(json!({"type":"text","text":t})),
                Block::Text(_) => {}
                Block::Tool { id, name, json } => {
                    let parsed: Option<Value> = serde_json::from_str(json).ok();
                    content.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": parsed.unwrap_or_else(|| json!({}))
                    }));
                    if name != "run_command" {
                        self.results.insert(id.clone(), "unknown tool".into());
                        self.errored.insert(id.clone());
                    } else if parse_run_command(json).is_none() {
                        self.results
                            .insert(id.clone(), "invalid run_command input".into());
                        self.errored.insert(id.clone());
                    } else {
                        awaiting.push(id.clone());
                    }
                }
            }
        }
        self.messages
            .push(json!({"role": "assistant", "content": content}));
        self.state = State::Waiting(awaiting);
    }

    /// Messages for the follow-up request: history + tool_result user message.
    fn messages_with_tool_results(&self) -> Vec<Value> {
        let mut results = Vec::new();
        for (_, b) in &self.blocks {
            if let Block::Tool { id, .. } = b {
                let text = self.results.get(id).cloned().unwrap_or_default();
                let mut r = json!({"type": "tool_result", "tool_use_id": id, "content": text});
                if self.errored.contains(id) {
                    r["is_error"] = json!(true);
                }
                results.push(r);
            }
        }
        let mut messages = self.messages.clone();
        messages.push(json!({"role": "user", "content": results}));
        messages
    }

    fn missing_results(&self) -> Vec<String> {
        match &self.state {
            State::Waiting(a) => a
                .iter()
                .filter(|id| !self.results.contains_key(*id))
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[async_trait]
impl ProviderTurn for AnthropicTurn {
    async fn next(&mut self) -> Option<ChatEvent> {
        use futures_util::StreamExt;
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Some(ev);
            }
            match self.state {
                State::Finished => return None,
                State::NotStarted => {
                    let messages = self.messages.clone();
                    self.begin_response(messages).await;
                }
                State::Waiting(_) => {
                    let missing = self.missing_results();
                    if !missing.is_empty() {
                        self.fail(format!("tool results missing for: {}", missing.join(", ")));
                        continue;
                    }
                    let messages = self.messages_with_tool_results();
                    self.begin_response(messages).await;
                }
                State::Streaming => {
                    let Some(stream) = self.stream.as_mut() else {
                        self.fail("stream missing".into());
                        continue;
                    };
                    match stream.next().await {
                        Some(Ok(bytes)) => {
                            for ev in self.parser.push(&bytes) {
                                if matches!(self.state, State::Streaming) {
                                    self.handle_event(ev);
                                }
                            }
                        }
                        Some(Err(e)) => self.fail(format!("stream error: {}", e.without_url())),
                        None => {
                            if self.stop_reason.is_some() {
                                self.finish_response();
                            } else {
                                self.fail("stream ended unexpectedly".into());
                            }
                        }
                    }
                }
            }
        }
    }

    async fn submit_command_result(&mut self, call_id: &str, text: String) -> Result<()> {
        self.results.insert(call_id.to_string(), text);
        Ok(())
    }

    async fn cancel(&mut self) {
        self.stream = None;
        self.queue.clear();
        self.state = State::Finished;
    }
}
