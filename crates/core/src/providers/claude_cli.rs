//! Claude Code CLI provider (`claude -p`, stream-json).
//!
//! Built-in tools are disabled (`--tools ""`); the only tool the CLI may use is
//! `run_command` on our local MCP server, which goes through the safety gate.
//! Commands therefore never surface here as `CommandProposed`.

use super::{ChatProvider, ProviderError, ProviderTurn, Result};
use crate::history::{to_cli_prompt, trim_history, DEFAULT_MAX_HISTORY_CHARS};
use crate::mcp_server::McpServerHandle;
use crate::runner::{kill_tree, CREATE_NO_WINDOW};
use crate::{Availability, ChatEvent, ProviderId, TurnRequest};
use async_trait::async_trait;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempPath;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::task::JoinHandle;

const SYSTEM_PROMPT: &str = "To run shell commands use the run_command tool from the assistant MCP server; commands are reviewed and may be denied by the user.";
const ALLOWED_TOOL: &str = "mcp__assistant__run_command";
const STDERR_CAP: usize = 16 * 1024;
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// Grace period for the CLI to exit on its own after its `result` line.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// CLI arguments for one turn. Never adds a permission-mode or
/// permission-bypass flag.
pub fn build_args(mcp_config_path: &Path, model: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--mcp-config",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push(mcp_config_path.to_string_lossy().into_owned());
    a.extend(
        [
            "--tools",
            "",
            "--allowedTools",
            ALLOWED_TOOL,
            "--append-system-prompt",
            SYSTEM_PROMPT,
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    if let Some(m) = model {
        a.push("--model".into());
        a.push(m.into());
    }
    a
}

/// Map one stream-json line to an event. Only partial text deltas and the
/// final `result` matter; the complete `assistant` message is ignored so text
/// is not duplicated.
pub fn parse_stream_line(line: &str) -> Option<ChatEvent> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    match v.get("type")?.as_str()? {
        "stream_event" => {
            let ev = v.get("event")?;
            if ev.get("type")?.as_str()? != "content_block_delta" {
                return None;
            }
            let d = ev.get("delta")?;
            if d.get("type")?.as_str()? != "text_delta" {
                return None;
            }
            Some(ChatEvent::TextDelta(d.get("text")?.as_str()?.to_string()))
        }
        "result" => {
            if v.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                let msg = v
                    .get("result")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| v.get("subtype").and_then(Value::as_str))
                    .unwrap_or("claude error");
                Some(ChatEvent::Error(msg.to_string()))
            } else {
                Some(ChatEvent::Done)
            }
        }
        _ => None,
    }
}

pub struct ClaudeCliProvider {
    exe: PathBuf,
    mcp: McpServerHandle,
    model: Option<String>,
}

impl ClaudeCliProvider {
    pub fn new(exe: PathBuf, mcp: McpServerHandle, model: Option<String>) -> Self {
        Self { exe, mcp, model }
    }

    /// Locate `claude` on PATH.
    pub fn from_path(mcp: McpServerHandle, model: Option<String>) -> Option<Self> {
        which::which("claude")
            .ok()
            .map(|exe| Self::new(exe, mcp, model))
    }

    fn write_config(&self, token: &str) -> std::io::Result<TempPath> {
        let cfg = serde_json::json!({
            "mcpServers": {"assistant": {"type": "http", "url": self.mcp.url_for_turn(token)}}
        });
        let mut f = tempfile::Builder::new()
            .prefix("assistant-mcp-")
            .suffix(".json")
            .tempfile()?;
        f.write_all(cfg.to_string().as_bytes())?;
        f.flush()?;
        // Close our handle so the CLI can read it; the path is deleted on drop.
        Ok(f.into_temp_path())
    }
}

#[async_trait]
impl ChatProvider for ClaudeCliProvider {
    fn id(&self) -> ProviderId {
        ProviderId::ClaudeCli
    }

    async fn availability(&self) -> Availability {
        if !self.exe.is_file() {
            return Availability::Unavailable("ไม่พบ Claude CLI".into());
        }
        let unusable = |r: String| Availability::Unavailable(format!("Claude CLI ใช้งานไม่ได้: {r}"));
        let out = Command::new(&self.exe)
            .arg("--version")
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(VERSION_TIMEOUT, out).await {
            Err(_) => unusable("--version ไม่ตอบภายใน 5 วินาที".into()),
            Ok(Err(e)) => unusable(e.to_string()),
            Ok(Ok(o)) if o.status.success() => Availability::Ready,
            Ok(Ok(o)) => {
                let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                let code = o.status.code().map_or("?".into(), |c| c.to_string());
                unusable(format!("exit code {code}: {err}"))
            }
        }
    }

    async fn start(&self, req: TurnRequest) -> Result<Box<dyn ProviderTurn>> {
        let config = self
            .write_config(&req.turn_token)
            .map_err(|e| ProviderError::Other(format!("cannot write MCP config: {e}")))?;
        let prompt = to_cli_prompt(&trim_history(&req.history, DEFAULT_MAX_HISTORY_CHARS));
        let mut child = Command::new(&self.exe)
            .args(build_args(&config, self.model.as_deref()))
            .current_dir(&req.cwd)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ProviderError::Other(format!("cannot start claude: {e}")))?;
        let pid = child.id();

        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdin_task = tokio::spawn(async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
            // `stdin` drops here, closing the pipe.
        });
        let mut stderr = child.stderr.take().expect("piped stderr");
        let stderr_task = tokio::spawn(async move {
            let mut tail: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > STDERR_CAP {
                    tail.drain(..tail.len() - STDERR_CAP);
                }
            }
            String::from_utf8_lossy(&tail).trim().to_string()
        });
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));

        Ok(Box::new(ClaudeCliTurn {
            child: Some(child),
            pid,
            stdout: Some(stdout),
            line: Vec::new(),
            stdin_task: Some(stdin_task),
            stderr_task: Some(stderr_task),
            config: Some(config),
            finished: false,
        }))
    }
}

struct ClaudeCliTurn {
    child: Option<Child>,
    pid: Option<u32>,
    stdout: Option<BufReader<ChildStdout>>,
    /// Partial line; kept across `next()` calls so a dropped future loses nothing.
    line: Vec<u8>,
    stdin_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<String>>,
    /// Temp MCP config; deleted when dropped.
    config: Option<TempPath>,
    finished: bool,
}

impl ClaudeCliTurn {
    /// End the turn after a `result` line. The CLI normally exits right away;
    /// reap it in the background and kill its tree if it lingers.
    fn finish_after_result(&mut self) {
        self.finished = true;
        self.stdout = None;
        self.config = None;
        if let Some(t) = self.stdin_task.take() {
            t.abort();
        }
        if let Some(t) = self.stderr_task.take() {
            t.abort();
        }
        if let Some(mut child) = self.child.take() {
            let pid = self.pid;
            tokio::spawn(async move {
                if tokio::time::timeout(EXIT_GRACE, child.wait())
                    .await
                    .is_err()
                {
                    kill_tree(pid).await;
                    let _ = child.wait().await;
                }
            });
        }
    }

    /// stdout closed without a `result` line: report exit code and stderr.
    async fn finish_without_result(&mut self) -> ChatEvent {
        let code = match self.child.as_mut() {
            Some(c) => match tokio::time::timeout(EXIT_GRACE, c.wait()).await {
                Ok(Ok(s)) => s.code().map_or("?".to_string(), |c| c.to_string()),
                Ok(Err(e)) => format!("? ({e})"),
                Err(_) => {
                    kill_tree(self.pid).await;
                    "? (did not exit)".to_string()
                }
            },
            None => "?".to_string(),
        };
        let stderr = match self.stderr_task.as_mut() {
            Some(t) => tokio::time::timeout(Duration::from_secs(2), t)
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default(),
            None => String::new(),
        };
        self.stderr_task = None;
        self.child = None;
        self.config = None;
        self.finished = true;
        ChatEvent::Error(format!("claude exited with code {code}: {stderr}"))
    }
}

impl Drop for ClaudeCliTurn {
    /// `kill_on_drop` only kills the direct child; also take down its tree
    /// (e.g. MCP helpers) if the turn is dropped while the CLI still runs.
    fn drop(&mut self) {
        if let (Some(_), Some(pid)) = (self.child.as_ref(), self.pid) {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
}

#[async_trait]
impl ProviderTurn for ClaudeCliTurn {
    /// Cancel-safe: `read_until` keeps partial data in `self.line`, and all
    /// other awaits only wait on child exit / task completion.
    async fn next(&mut self) -> Option<ChatEvent> {
        loop {
            if self.finished {
                return None;
            }
            let Some(stdout) = self.stdout.as_mut() else {
                return Some(self.finish_without_result().await);
            };
            match stdout.read_until(b'\n', &mut self.line).await {
                Ok(0) | Err(_) => {
                    self.stdout = None;
                    if !self.line.is_empty() {
                        // Final line without a trailing newline.
                        let l =
                            String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
                        if let Some(ev) = parse_stream_line(&l) {
                            if matches!(ev, ChatEvent::Done | ChatEvent::Error(_)) {
                                self.finish_after_result();
                            }
                            return Some(ev);
                        }
                    }
                }
                Ok(_) => {
                    let l = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
                    if let Some(ev) = parse_stream_line(&l) {
                        if matches!(ev, ChatEvent::Done | ChatEvent::Error(_)) {
                            self.finish_after_result();
                        }
                        return Some(ev);
                    }
                }
            }
        }
    }

    async fn submit_command_result(&mut self, _call_id: &str, _text: String) -> Result<()> {
        Ok(())
    }

    async fn cancel(&mut self) {
        if self.child.is_some() {
            kill_tree(self.pid).await;
            if let Some(mut c) = self.child.take() {
                let _ = c.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(5), c.wait()).await;
            }
        }
        if let Some(t) = self.stdin_task.take() {
            t.abort();
        }
        if let Some(t) = self.stderr_task.take() {
            t.abort();
        }
        self.stdout = None;
        self.line.clear();
        self.config = None;
        self.finished = true;
    }
}
