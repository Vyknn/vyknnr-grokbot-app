#![cfg(windows)]
use assistant_core::command_service::{
    AppEvent, ApprovalRequest, ApprovalResponse, Approver, CommandService, EventSink,
};
use assistant_core::decision::gate::{CommandCheck, CommandChecker};
use assistant_core::decision::policy::{CommandVerdict, DecisionSource};
use assistant_core::mcp_server::{McpServer, McpServerHandle};
use assistant_core::providers::claude_cli::{build_args, parse_stream_line, ClaudeCliProvider};
use assistant_core::providers::{ChatProvider, ProviderTurn};
use assistant_core::runner::RunLimits;
use assistant_core::store::Store;
use assistant_core::{Availability, ChatEvent, Message, ProviderId, Role, TurnRequest};
use async_trait::async_trait;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const FIXTURE: &str = include_str!("fixtures/claude_stream.jsonl");

#[test]
fn args_disable_builtin_tools_and_allow_only_our_mcp() {
    let a = build_args(Path::new("C:\\t\\m.json"), None);
    for must in [
        "-p",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--allowedTools",
        "mcp__assistant__run_command",
    ] {
        assert!(a.contains(&must.to_string()), "missing {must}");
    }
    let i = a.iter().position(|x| x == "--tools").unwrap();
    assert_eq!(a[i + 1], "");
    assert!(!a
        .iter()
        .any(|x| x.contains("dangerously") || x == "bypassPermissions"));
    assert!(!a.iter().any(|x| x == "--permission-mode"));
    // User/project/local settings (hooks, plugins) must not load: they run
    // shell commands outside our gate.
    let i = a.iter().position(|x| x == "--setting-sources").unwrap();
    assert_eq!(a[i + 1], "");
    let i = a.iter().position(|x| x == "--settings").unwrap();
    assert_eq!(a[i + 1], r#"{"disableAllHooks":true}"#);
    assert!(!a.iter().any(|x| x == "--safe-mode" || x == "--bare"));
    assert!(!a.iter().any(|x| x == "--model"));
}

#[test]
fn args_exact_order_and_model() {
    let a = build_args(Path::new("C:\\t\\m.json"), Some("sonnet"));
    let expected: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--no-session-persistence",
        "--setting-sources",
        "",
        "--settings",
        r#"{"disableAllHooks":true}"#,
        "--strict-mcp-config",
        "--mcp-config",
        "C:\\t\\m.json",
        "--tools",
        "",
        "--allowedTools",
        "mcp__assistant__run_command",
        "--append-system-prompt",
        "To run shell commands use the run_command tool from the assistant MCP server; commands are reviewed and may be denied by the user.",
        "--model",
        "sonnet",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(a, expected);
}

fn parse_all(s: &str) -> Vec<ChatEvent> {
    s.lines().filter_map(parse_stream_line).collect()
}

#[test]
fn parses_fixture_stream() {
    let evs = parse_all(FIXTURE);
    assert_eq!(evs.last(), Some(&ChatEvent::Done));
    let text: String = evs
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "โอเค");
    // Only text deltas then Done: no duplicate from the final `assistant` message.
    assert_eq!(evs.len(), 3);
}

#[test]
fn error_result_maps_to_error() {
    let l = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"API down"}"#;
    assert_eq!(
        parse_stream_line(l),
        Some(ChatEvent::Error("API down".into()))
    );
    let l = r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#;
    assert_eq!(
        parse_stream_line(l),
        Some(ChatEvent::Error("error_max_turns".into()))
    );
    let l = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok"}"#;
    assert_eq!(parse_stream_line(l), Some(ChatEvent::Done));
}

#[test]
fn unknown_lines_ignored() {
    assert_eq!(parse_stream_line("{\"type\":\"system\"}"), None);
    assert_eq!(parse_stream_line("not json"), None);
    assert_eq!(parse_stream_line(""), None);
    let thinking = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"x"}}}"#;
    assert_eq!(parse_stream_line(thinking), None);
    let assistant = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"dup"}]}}"#;
    assert_eq!(parse_stream_line(assistant), None);
}

// ---------- process tests with a fake `claude` (.cmd script) ----------

struct DenyChecker;
#[async_trait]
impl CommandChecker for DenyChecker {
    async fn check(&self, _command: &str, _cwd: &Path) -> CommandCheck {
        CommandCheck {
            verdict: CommandVerdict::AskUser,
            is_safe: None,
            risk: None,
            risk_p: None,
            source: DecisionSource::Jev {
                model: "fake".into(),
            },
        }
    }
}
struct DenyApprover;
#[async_trait]
impl Approver for DenyApprover {
    async fn request(&self, _req: ApprovalRequest) -> ApprovalResponse {
        ApprovalResponse::Deny
    }
}
struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _ev: AppEvent) {}
}

async fn mcp() -> McpServerHandle {
    let svc = Arc::new(CommandService::new(
        Arc::new(DenyChecker),
        Arc::new(DenyApprover),
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(NullSink),
        RunLimits::default(),
        Duration::from_secs(5),
    ));
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    McpServer::start(addr, svc, "k-0123456789abcdef".into())
        .await
        .unwrap()
}

fn req(cwd: &Path) -> TurnRequest {
    TurnRequest {
        conversation_id: 1,
        turn_token: "tok".into(),
        history: vec![Message {
            id: 1,
            conversation_id: 1,
            role: Role::User,
            provider: None,
            content: "hi".into(),
            created_at: chrono::Utc::now(),
        }],
        cwd: cwd.to_path_buf(),
    }
}

/// Writes `fake_claude.cmd` into `dir`. The script records stdin and the
/// `--mcp-config` path (argument 13; four `shift`s make it `%9`), then runs `body`.
fn fake(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("fake_claude.cmd");
    let script = format!(
        "@echo off\r\nfindstr \"^\" > \"{d}\\stdin.txt\"\r\nshift\r\nshift\r\nshift\r\nshift\r\necho %~9> \"{d}\\cfgpath.txt\"\r\n{body}\r\n",
        d = dir.display()
    );
    std::fs::write(&p, script).unwrap();
    p
}

/// The `--mcp-config` path the fake recorded.
fn config_path_in(dir: &Path) -> PathBuf {
    let p = std::fs::read_to_string(dir.join("cfgpath.txt")).unwrap();
    let p = PathBuf::from(p.trim());
    assert_eq!(p.extension().and_then(|e| e.to_str()), Some("json"));
    p
}

async fn collect(turn: &mut Box<dyn ProviderTurn>) -> Vec<ChatEvent> {
    let mut v = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(20), turn.next()).await {
            Ok(Some(e)) => v.push(e),
            Ok(None) => return v,
            Err(_) => panic!("next() hung; got {v:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn process_streams_fixture_and_deletes_config() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude_stream.jsonl");
    let body = format!(
        "copy \"%~9\" \"{d}\\cfg_copy.json\" >nul\r\ntype \"{f}\"\r\nexit /b 0",
        d = dir.path().display(),
        f = fixture.display()
    );
    let exe = fake(dir.path(), &body);
    let mcp = mcp().await;
    let p = ClaudeCliProvider::new(exe, mcp.clone(), None);
    assert_eq!(p.id(), ProviderId::ClaudeCli);
    let mut turn = p.start(req(dir.path())).await.unwrap();
    let evs = collect(&mut turn).await;
    assert_eq!(
        evs,
        vec![
            ChatEvent::TextDelta("โ".into()),
            ChatEvent::TextDelta("อเค".into()),
            ChatEvent::Done
        ]
    );
    // Config existed during the turn with our URL, and is gone after it.
    let copy = std::fs::read_to_string(dir.path().join("cfg_copy.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&copy).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"mcpServers":{"assistant":{"type":"http","url": mcp.url_for_turn("tok")}}})
    );
    let cfg = config_path_in(dir.path());
    assert!(
        !cfg.exists(),
        "temp MCP config not deleted: {}",
        cfg.display()
    );
    // Prompt was written to stdin.
    let stdin = std::fs::read_to_string(dir.path().join("stdin.txt")).unwrap();
    assert!(stdin.contains("hi"), "stdin: {stdin:?}");
    assert_eq!(turn.next().await, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_without_result_is_error_with_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fake(dir.path(), "echo boom 1>&2\r\nexit /b 3");
    let p = ClaudeCliProvider::new(exe, mcp().await, None);
    let mut turn = p.start(req(dir.path())).await.unwrap();
    let evs = collect(&mut turn).await;
    assert_eq!(evs.len(), 1, "{evs:?}");
    match &evs[0] {
        ChatEvent::Error(m) => {
            assert!(m.contains("boom"), "{m}");
            assert!(m.contains('3'), "{m}");
        }
        e => panic!("expected error, got {e:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nonzero_exit_after_result_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fake(
        dir.path(),
        "echo {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"x\"}\r\nexit /b 5",
    );
    let p = ClaudeCliProvider::new(exe, mcp().await, None);
    let mut turn = p.start(req(dir.path())).await.unwrap();
    assert_eq!(collect(&mut turn).await, vec![ChatEvent::Done]);
}

fn pid_alive(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_kills_process_and_ends_turn() {
    let dir = tempfile::tempdir().unwrap();
    // The fake sleeps via a grandchild `ping`; cancel must kill the whole tree.
    let exe = fake(dir.path(), "ping -n 30 127.0.0.1 >nul");
    let p = ClaudeCliProvider::new(exe, mcp().await, None);
    let mut turn = p.start(req(dir.path())).await.unwrap();
    // next() is pending; drop it after a timeout (cancel-safety), then cancel.
    let r = tokio::time::timeout(Duration::from_millis(1500), turn.next()).await;
    assert!(r.is_err(), "next() should still be pending: {r:?}");
    let pings_before = ping_pids();
    assert!(!pings_before.is_empty(), "fake ping not running");
    turn.cancel().await;
    assert_eq!(turn.next().await, None);
    tokio::time::sleep(Duration::from_millis(500)).await;
    for pid in pings_before {
        assert!(!pid_alive(pid), "ping {pid} survived cancel");
    }
    assert!(!config_path_in(dir.path()).exists());
}

/// PIDs of ping.exe processes that are pinging 127.0.0.1 30 times (ours).
fn ping_pids() -> Vec<u32> {
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process -Filter \"Name='PING.EXE'\" | Where-Object { $_.CommandLine -like '*-n 30 127.0.0.1*' } | ForEach-Object { $_.ProcessId }",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn availability_ready_and_missing() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fake(dir.path(), "echo 2.1.302 (Claude Code)\r\nexit /b 0");
    let mcp = mcp().await;
    let p = ClaudeCliProvider::new(exe, mcp.clone(), None);
    assert_eq!(p.availability().await, Availability::Ready);

    let missing = ClaudeCliProvider::new(dir.path().join("nope.exe"), mcp.clone(), None);
    assert_eq!(
        missing.availability().await,
        Availability::Unavailable("ไม่พบ Claude CLI".into())
    );

    let bad = dir.path().join("bad.cmd");
    std::fs::write(&bad, "@echo off\r\necho kaput 1>&2\r\nexit /b 2\r\n").unwrap();
    let p = ClaudeCliProvider::new(bad, mcp, None);
    match p.availability().await {
        Availability::Unavailable(m) => {
            assert!(m.starts_with("Claude CLI ใช้งานไม่ได้: "), "{m}")
        }
        a => panic!("{a:?}"),
    }
}
