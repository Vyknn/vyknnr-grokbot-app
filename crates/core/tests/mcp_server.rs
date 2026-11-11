#![cfg(windows)]
use assistant_core::command_service::*;
use assistant_core::decision::gate::{CommandCheck, CommandChecker};
use assistant_core::decision::policy::{CommandVerdict, DecisionSource};
use assistant_core::mcp_server::{McpServer, McpServerHandle};
use assistant_core::runner::RunLimits;
use assistant_core::store::Store;
use assistant_core::ProviderId;
use async_trait::async_trait;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{RoleClient, ServiceExt};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const HERMES_KEY: &str = "test-hermes-key-0123456789";
const DENIED_TEXT: &str = "คำสั่งถูกปฏิเสธโดยผู้ใช้หรือระบบตรวจสอบ";
const NO_TURN_TEXT: &str = "ไม่มีงานที่กำลังทำอยู่";

struct FakeChecker(CommandVerdict);

#[async_trait]
impl CommandChecker for FakeChecker {
    async fn check(&self, _command: &str, _cwd: &Path) -> CommandCheck {
        CommandCheck {
            verdict: self.0,
            is_safe: Some(0.9),
            risk: Some("other".into()),
            risk_p: Some(0.1),
            source: DecisionSource::Jev {
                model: "fake-jev".into(),
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

#[derive(Default)]
struct VecSink(Mutex<Vec<AppEvent>>);

impl EventSink for VecSink {
    fn emit(&self, ev: AppEvent) {
        self.0.lock().unwrap().push(ev);
    }
}

impl VecSink {
    fn started(&self) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, AppEvent::CommandStarted { .. }))
            .count()
    }
}

struct Rig {
    handle: McpServerHandle,
    sink: Arc<VecSink>,
    store: Arc<Store>,
    conv: i64,
    dir: tempfile::TempDir,
}

async fn rig(verdict: CommandVerdict) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let conv = store.create_conversation("t", dir.path()).unwrap().id;
    let sink = Arc::new(VecSink::default());
    let svc = Arc::new(CommandService::new(
        Arc::new(FakeChecker(verdict)),
        Arc::new(DenyApprover),
        store.clone(),
        sink.clone(),
        RunLimits::default(),
        Duration::from_secs(5),
    ));
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let handle = McpServer::start(addr, svc, HERMES_KEY.to_string())
        .await
        .unwrap();
    Rig {
        handle,
        sink,
        store,
        conv,
        dir,
    }
}

fn ctx(conv: i64, token: &str, cwd: PathBuf) -> CommandContext {
    CommandContext {
        conversation_id: conv,
        turn_token: token.into(),
        cwd,
        provider: ProviderId::ClaudeCli,
        count: Arc::new(AtomicU32::new(0)),
        max_per_turn: 10,
        cancel: CancellationToken::new(),
    }
}

async fn client(url: String, bearer: Option<&str>) -> RunningService<RoleClient, ClientConfig> {
    let mut cfg = StreamableHttpClientTransportConfig::with_uri(url);
    if let Some(b) = bearer {
        cfg = cfg.auth_header(b);
    }
    let transport = StreamableHttpClientTransport::from_config(cfg);
    ClientConfig::default().serve(transport).await.unwrap()
}

async fn call(
    c: &RunningService<RoleClient, ClientConfig>,
    args: serde_json::Value,
) -> CallToolResult {
    let args = args.as_object().unwrap().clone();
    c.call_tool(CallToolRequestParams::new("run_command").with_arguments(args))
        .await
        .unwrap()
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn initialize_body() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "raw", "version": "0" }
        }
    })
}

/// Raw POST of an initialize request; returns the HTTP status.
async fn raw_post(url: &str, headers: &[(&str, &str)]) -> u16 {
    let mut rb = reqwest::Client::new()
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(initialize_body().to_string());
    for (k, v) in headers {
        rb = rb.header(*k, *v);
    }
    rb.send().await.unwrap().status().as_u16()
}

#[tokio::test]
async fn lists_only_run_command() {
    let r = rig(CommandVerdict::AutoRun).await;
    let _g = r
        .handle
        .register_turn(ctx(r.conv, "tok-list", r.dir.path().into()));
    let c = client(r.handle.url_for_turn("tok-list"), None).await;
    let tools = c.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    assert_eq!(names, vec!["run_command".to_string()]);
    let schema = serde_json::Value::Object((*tools[0].input_schema).clone());
    assert_eq!(schema["properties"]["command"]["type"], "string");
    assert!(schema["properties"]["cwd"].is_object());
    assert_eq!(schema["required"], serde_json::json!(["command"]));
    let _ = c.cancel().await;
}

#[tokio::test]
async fn urls_use_real_bound_port() {
    let r = rig(CommandVerdict::AutoRun).await;
    let port = r.handle.local_addr().port();
    assert_ne!(port, 0);
    assert_eq!(
        r.handle.url_for_turn("abc"),
        format!("http://127.0.0.1:{port}/mcp/t/abc")
    );
    assert_eq!(
        r.handle.hermes_url(),
        format!("http://127.0.0.1:{port}/mcp/hermes")
    );
}

#[tokio::test]
async fn unknown_turn_token_rejected() {
    let r = rig(CommandVerdict::AutoRun).await;
    let _g = r
        .handle
        .register_turn(ctx(r.conv, "real-token", r.dir.path().into()));
    assert_eq!(raw_post(&r.handle.url_for_turn("nope"), &[]).await, 404);
    // Prefix of a real token is not a match.
    assert_eq!(
        raw_post(&r.handle.url_for_turn("real-toke"), &[]).await,
        404
    );
    assert_eq!(r.sink.started(), 0);
}

#[tokio::test]
async fn hermes_route_requires_bearer() {
    let r = rig(CommandVerdict::AutoRun).await;
    let url = r.handle.hermes_url();
    assert_eq!(raw_post(&url, &[]).await, 401);
    assert_eq!(
        raw_post(&url, &[("authorization", "Bearer wrong-key")]).await,
        401
    );
    assert_eq!(raw_post(&url, &[("authorization", HERMES_KEY)]).await, 401);
    let ok = format!("Bearer {HERMES_KEY}");
    assert_eq!(raw_post(&url, &[("authorization", &ok)]).await, 200);
}

#[tokio::test]
async fn hermes_without_active_turn_is_tool_error_and_never_runs() {
    let r = rig(CommandVerdict::AutoRun).await;
    // A registered turn exists, but it is not the active Hermes turn.
    let _g = r
        .handle
        .register_turn(ctx(r.conv, "hermes-turn", r.dir.path().into()));
    let c = client(r.handle.hermes_url(), Some(HERMES_KEY)).await;
    let res = call(&c, serde_json::json!({"command": "Write-Output hi"})).await;
    assert_eq!(res.is_error, Some(true));
    assert!(text(&res).contains(NO_TURN_TEXT), "got: {}", text(&res));
    assert_eq!(r.sink.started(), 0);
    assert!(r.store.commands(r.conv).unwrap().is_empty());
    let _ = c.cancel().await;
}

#[tokio::test]
async fn hermes_with_active_turn_runs() {
    let r = rig(CommandVerdict::AutoRun).await;
    let _g = r
        .handle
        .register_turn(ctx(r.conv, "hermes-turn", r.dir.path().into()));
    r.handle
        .set_active_hermes_turn(Some("hermes-turn".to_string()));
    let c = client(r.handle.hermes_url(), Some(HERMES_KEY)).await;
    let res = call(&c, serde_json::json!({"command": "Write-Output hermes-ok"})).await;
    assert_ne!(res.is_error, Some(true));
    assert!(text(&res).contains("hermes-ok"));
    assert_eq!(r.store.commands(r.conv).unwrap().len(), 1);

    r.handle.set_active_hermes_turn(None);
    let res = call(&c, serde_json::json!({"command": "Write-Output again"})).await;
    assert_eq!(res.is_error, Some(true));
    assert!(text(&res).contains(NO_TURN_TEXT));
    assert_eq!(r.store.commands(r.conv).unwrap().len(), 1);
    let _ = c.cancel().await;
}

#[tokio::test]
async fn call_goes_through_command_service() {
    let r = rig(CommandVerdict::AutoRun).await;
    let _g = r
        .handle
        .register_turn(ctx(r.conv, "tok-run", r.dir.path().into()));
    let c = client(r.handle.url_for_turn("tok-run"), None).await;
    let res = call(&c, serde_json::json!({"command": "Write-Output hi"})).await;
    assert_ne!(res.is_error, Some(true));
    assert!(text(&res).contains("hi"));
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].command, "Write-Output hi");
    assert_eq!(r.sink.started(), 1);
    let _ = c.cancel().await;
}

#[tokio::test]
async fn nonzero_exit_is_normal_result() {
    let r = rig(CommandVerdict::AutoRun).await;
    let _g = r
        .handle
        .register_turn(ctx(r.conv, "tok-exit", r.dir.path().into()));
    let c = client(r.handle.url_for_turn("tok-exit"), None).await;
    let res = call(
        &c,
        serde_json::json!({"command": "Write-Output before-exit; exit 3"}),
    )
    .await;
    assert_ne!(res.is_error, Some(true));
    assert!(text(&res).contains("before-exit"));
    let _ = c.cancel().await;
}

#[tokio::test]
async fn denied_is_tool_error() {
    let r = rig(CommandVerdict::AskUser).await;
    let _g = r
        .handle
        .register_turn(ctx(r.conv, "tok-deny", r.dir.path().into()));
    let c = client(r.handle.url_for_turn("tok-deny"), None).await;
    let res = call(&c, serde_json::json!({"command": "Remove-Item x"})).await;
    assert_eq!(res.is_error, Some(true));
    assert!(text(&res).contains(DENIED_TEXT));
    assert_eq!(r.sink.started(), 0);
    let _ = c.cancel().await;
}

#[tokio::test]
async fn dropped_turn_guard_unregisters() {
    let r = rig(CommandVerdict::AutoRun).await;
    let g = r
        .handle
        .register_turn(ctx(r.conv, "tok-drop", r.dir.path().into()));
    assert!(r.handle.context("tok-drop").is_some());
    let url = r.handle.url_for_turn("tok-drop");
    assert_eq!(raw_post(&url, &[]).await, 200);
    drop(g);
    assert!(r.handle.context("tok-drop").is_none());
    assert_eq!(raw_post(&url, &[]).await, 404);
}

#[tokio::test]
async fn non_loopback_addr_rejected() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let svc = Arc::new(CommandService::new(
        Arc::new(FakeChecker(CommandVerdict::AutoRun)),
        Arc::new(DenyApprover),
        store,
        Arc::new(VecSink::default()),
        RunLimits::default(),
        Duration::from_secs(5),
    ));
    let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    assert!(McpServer::start(addr, svc, HERMES_KEY.into())
        .await
        .is_err());
}

#[tokio::test]
async fn foreign_origin_rejected() {
    let r = rig(CommandVerdict::AutoRun).await;
    let _g = r
        .handle
        .register_turn(ctx(r.conv, "tok-origin", r.dir.path().into()));
    let url = r.handle.url_for_turn("tok-origin");
    assert_eq!(
        raw_post(&url, &[("origin", "http://evil.example")]).await,
        403
    );
    let ok = format!("Bearer {HERMES_KEY}");
    assert_eq!(
        raw_post(
            &r.handle.hermes_url(),
            &[("origin", "http://evil.example"), ("authorization", &ok)]
        )
        .await,
        403
    );
    assert_eq!(raw_post(&url, &[("origin", "null")]).await, 200);
}

#[tokio::test]
async fn foreign_host_rejected() {
    let r = rig(CommandVerdict::AutoRun).await;
    let _g = r
        .handle
        .register_turn(ctx(r.conv, "tok-host", r.dir.path().into()));
    let url = r.handle.url_for_turn("tok-host");
    assert_eq!(raw_post(&url, &[("host", "evil.example")]).await, 403);
    let port = r.handle.local_addr().port();
    let lh = format!("localhost:{port}");
    assert_eq!(raw_post(&url, &[("host", &lh)]).await, 200);
}

async fn get_location(r: &Rig, token: &str, cwd: Option<&str>) -> String {
    let c = client(r.handle.url_for_turn(token), None).await;
    let mut args = serde_json::json!({"command": "(Get-Location).Path"});
    if let Some(d) = cwd {
        args["cwd"] = d.into();
    }
    let res = call(&c, args).await;
    assert_ne!(res.is_error, Some(true));
    let _ = c.cancel().await;
    text(&res).trim().to_lowercase()
}

#[tokio::test]
async fn cwd_outside_chat_folder_falls_back() {
    let r = rig(CommandVerdict::AutoRun).await;
    let base = r.dir.path().to_path_buf();
    let _g = r.handle.register_turn(ctx(r.conv, "tok-cwd", base.clone()));
    let want = base.to_string_lossy().to_lowercase();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    assert_eq!(get_location(&r, "tok-cwd", Some(&windir)).await, want);
    let missing = base.join("does-not-exist");
    assert_eq!(
        get_location(&r, "tok-cwd", Some(missing.to_str().unwrap())).await,
        want
    );
    let escape = base.join("..");
    assert_eq!(
        get_location(&r, "tok-cwd", Some(escape.to_str().unwrap())).await,
        want
    );
}

#[tokio::test]
async fn cwd_inside_chat_folder_is_used() {
    let r = rig(CommandVerdict::AutoRun).await;
    let base = r.dir.path().to_path_buf();
    std::fs::create_dir(base.join("Sub")).unwrap();
    let _g = r.handle.register_turn(ctx(r.conv, "tok-sub", base.clone()));
    let want = base.join("Sub").to_string_lossy().to_lowercase();
    // Absolute (different case) and relative forms.
    let abs = base.join("sub").to_string_lossy().to_uppercase();
    assert_eq!(get_location(&r, "tok-sub", Some(&abs)).await, want);
    assert_eq!(get_location(&r, "tok-sub", Some("Sub")).await, want);
}
