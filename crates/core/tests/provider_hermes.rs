use assistant_core::command_service::*;
use assistant_core::decision::gate::{CommandCheck, CommandChecker};
use assistant_core::decision::policy::{CommandVerdict, DecisionSource};
use assistant_core::mcp_server::{McpServer, McpServerHandle, TurnGuard};
use assistant_core::providers::hermes::{
    parse_sse, HermesEvent, HermesProvider, DEFAULT_FORBIDDEN_TOOLSETS,
};
use assistant_core::providers::{ChatProvider, ProviderTurn};
use assistant_core::runner::RunLimits;
use assistant_core::store::{Approval, Store};
use assistant_core::{Availability, ChatEvent, Message, ProviderId, Role, TurnRequest};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "hermes-secret-key-xyz";
const TOKEN: &str = "turn-h1";
const FIXTURE: &str = include_str!("fixtures/hermes_run_events.txt");

// ---------- fakes ----------

struct FakeChecker(CommandVerdict);

#[async_trait]
impl CommandChecker for FakeChecker {
    async fn check(&self, _command: &str, _cwd: &Path) -> CommandCheck {
        CommandCheck {
            verdict: self.0,
            is_safe: Some(0.5),
            risk: Some("other".into()),
            risk_p: Some(0.5),
            source: DecisionSource::Jev {
                model: "fake-jev".into(),
            },
        }
    }
}

/// Answers with `resp`, optionally after `gate` is notified. Counts calls.
struct FakeApprover {
    resp: ApprovalResponse,
    gate: Option<Arc<Notify>>,
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl Approver for FakeApprover {
    async fn request(&self, req: ApprovalRequest) -> ApprovalResponse {
        self.calls.lock().unwrap().push(req.command.clone());
        if let Some(g) = &self.gate {
            g.notified().await;
        }
        self.resp.clone()
    }
}

#[derive(Default)]
struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _ev: AppEvent) {}
}

struct Rig {
    server: MockServer,
    mcp: McpServerHandle,
    commands: Arc<CommandService>,
    approver: Arc<FakeApprover>,
    store: Arc<Store>,
    conv: i64,
    _guard: TurnGuard,
}

async fn rig_with(
    verdict: CommandVerdict,
    resp: ApprovalResponse,
    gate: Option<Arc<Notify>>,
) -> Rig {
    let server = MockServer::start().await;
    let store = Arc::new(Store::open_in_memory().unwrap());
    let conv = store
        .create_conversation("t", &std::env::temp_dir())
        .unwrap()
        .id;
    let approver = Arc::new(FakeApprover {
        resp,
        gate,
        calls: Mutex::new(Vec::new()),
    });
    let commands = Arc::new(CommandService::new(
        Arc::new(FakeChecker(verdict)),
        approver.clone(),
        store.clone(),
        Arc::new(NullSink),
        RunLimits::default(),
        Duration::from_secs(30),
    ));
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mcp = McpServer::start(addr, commands.clone(), "mcp-hermes-key".into())
        .await
        .unwrap();
    let guard = mcp.register_turn(CommandContext {
        conversation_id: conv,
        turn_token: TOKEN.into(),
        cwd: std::env::temp_dir(),
        provider: ProviderId::Hermes,
        count: Arc::new(AtomicU32::new(0)),
        max_per_turn: 10,
        cancel: CancellationToken::new(),
    });
    Rig {
        server,
        mcp,
        commands,
        approver,
        store,
        conv,
        _guard: guard,
    }
}

async fn rig(verdict: CommandVerdict, resp: ApprovalResponse) -> Rig {
    rig_with(verdict, resp, None).await
}

fn provider(r: &Rig, key: Option<&str>) -> HermesProvider {
    HermesProvider::new(
        Url::parse(&format!("{}/v1/", r.server.uri())).unwrap(),
        key.map(str::to_string),
        r.mcp.clone(),
        r.commands.clone(),
        DEFAULT_FORBIDDEN_TOOLSETS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    )
}

fn req(conv: i64) -> TurnRequest {
    TurnRequest {
        conversation_id: conv,
        turn_token: TOKEN.into(),
        history: vec![Message {
            id: 1,
            conversation_id: conv,
            role: Role::User,
            provider: None,
            content: "list files".into(),
            created_at: chrono::Utc::now(),
        }],
        cwd: std::env::temp_dir(),
    }
}

fn sse(events: &[(&str, Value)]) -> String {
    events
        .iter()
        .map(|(e, d)| format!("event: {e}\ndata: {d}\n\n"))
        .collect()
}

fn event_stream(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

async fn mount_toolsets(server: &MockServer, body: Value) {
    Mock::given(method("GET"))
        .and(path("/v1/toolsets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn mount_safe_toolsets(server: &MockServer) {
    mount_toolsets(
        server,
        json!([{"name":"web","enabled":true},{"name":"terminal","enabled":false}]),
    )
    .await;
}

async fn mount_health(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"ok"})))
        .mount(server)
        .await;
}

async fn mount_run(server: &MockServer, events: String) {
    Mock::given(method("POST"))
        .and(path("/v1/runs"))
        .and(header(
            "authorization",
            format!("Bearer {API_KEY}").as_str(),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"run_id":"run_abc123","status":"started"})),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/runs/run_abc123/events"))
        .respond_with(event_stream(events))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/runs/run_abc123/stop"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status":"stopping"})))
        .mount(server)
        .await;
}

async fn mount_approval(server: &MockServer, status: u16) {
    Mock::given(method("POST"))
        .and(path("/v1/runs/run_abc123/approval"))
        .respond_with(ResponseTemplate::new(status).set_body_json(json!({"ok":true})))
        .mount(server)
        .await;
}

async fn bodies(server: &MockServer, p: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == p)
        .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
        .collect()
}

async fn count(server: &MockServer, p: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == p)
        .count()
}

async fn collect(turn: &mut Box<dyn ProviderTurn>) -> Vec<ChatEvent> {
    let mut out = Vec::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(10), turn.next())
        .await
        .expect("next() hung")
    {
        out.push(ev);
    }
    out
}

fn approval(id: &str, generation: u64, command: &str) -> (&'static str, Value) {
    (
        "approval.request",
        json!({"request_id":id,"execution_generation":generation,"command":command}),
    )
}

fn delta(t: &str) -> (&'static str, Value) {
    ("message.delta", json!({"delta": t}))
}

fn completed() -> (&'static str, Value) {
    ("run.completed", json!({"status":"completed"}))
}

// ---------- parse ----------

#[test]
fn sse_skips_keepalive_and_parses_events() {
    let evs = parse_sse(FIXTURE);
    assert!(!evs.is_empty());
    let meaningful: Vec<&HermesEvent> = evs
        .iter()
        .filter(|e| !matches!(e, HermesEvent::Other))
        .collect();
    assert_eq!(
        meaningful,
        vec![
            &HermesEvent::Delta("Let me check ".into()),
            &HermesEvent::Delta("the folder.".into()),
            &HermesEvent::Delta(" Interim note.".into()),
            &HermesEvent::ApprovalRequest {
                request_id: "prompt_1".into(),
                execution_generation: 3,
                command: "Remove-Item old.txt".into(),
            },
            &HermesEvent::ApprovalRequest {
                request_id: "prompt_2".into(),
                execution_generation: 4,
                command: "git push".into(),
            },
            &HermesEvent::Delta(" Done.".into()),
            &HermesEvent::Completed,
        ]
    );
    // tool.*, subagent.*, already-streamed interim: ignored, no empty deltas.
    assert!(!evs
        .iter()
        .any(|e| matches!(e, HermesEvent::Delta(t) if t.is_empty())));
}

#[test]
fn sse_parses_interrupted_and_incomplete_approval() {
    let evs = parse_sse(&sse(&[
        ("approval.request", json!({"command":"rm x"})),
        ("run.interrupted", json!({})),
    ]));
    assert_eq!(
        evs,
        vec![HermesEvent::IncompleteApproval, HermesEvent::Interrupted]
    );
}

// ---------- availability ----------

#[tokio::test]
async fn health_down_is_unavailable() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    // No /health mock -> 404; also try unreachable.
    let p = provider(&r, Some(API_KEY));
    assert_eq!(
        p.availability().await,
        Availability::Unavailable("Hermes ไม่ได้เปิด API server".into())
    );
    let p2 = HermesProvider::new(
        Url::parse("http://127.0.0.1:1/v1/").unwrap(),
        Some(API_KEY.into()),
        r.mcp.clone(),
        r.commands.clone(),
        vec![],
    );
    assert_eq!(
        p2.availability().await,
        Availability::Unavailable("Hermes ไม่ได้เปิด API server".into())
    );
}

#[tokio::test]
async fn no_api_key_is_unavailable() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_health(&r.server).await;
    mount_safe_toolsets(&r.server).await;
    for key in [None, Some("")] {
        let p = provider(&r, key);
        assert_eq!(
            p.availability().await,
            Availability::Unavailable("ยังไม่ได้ตั้ง API key ของ Hermes".into())
        );
        assert!(p.start(req(r.conv)).await.is_err());
    }
}

#[tokio::test]
async fn forbidden_toolset_enabled_is_unavailable() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_health(&r.server).await;
    mount_toolsets(
        &r.server,
        json!([{"name":"terminal","enabled":true},{"name":"file","enabled":true},{"name":"web","enabled":true}]),
    )
    .await;
    let p = provider(&r, Some(API_KEY));
    match p.availability().await {
        Availability::Unavailable(m) => {
            assert_eq!(m, "ต้องปิด toolset: terminal, file");
            assert!(!m.contains(API_KEY));
        }
        a => panic!("expected unavailable, got {a:?}"),
    }
    // start also refuses.
    let err = p.start(req(r.conv)).await.err().expect("start must fail");
    assert!(err.to_string().contains("terminal"));
    assert_eq!(r.mcp.active_hermes_turn(), None);
}

#[tokio::test]
async fn ready_when_health_ok_and_toolsets_safe() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_health(&r.server).await;
    mount_safe_toolsets(&r.server).await;
    assert_eq!(
        provider(&r, Some(API_KEY)).availability().await,
        Availability::Ready
    );
}

#[tokio::test]
async fn toolsets_all_three_shapes_parsed() {
    let shapes = [
        (
            json!([{"name":"terminal","enabled":true},{"name":"web","enabled":false}]),
            true,
        ),
        (json!([{"name":"terminal","enabled":false}]), false),
        (
            json!({"toolsets":[{"name":"browser","enabled":true}]}),
            true,
        ),
        (
            json!({"toolsets":[{"name":"browser","enabled":false}]}),
            false,
        ),
        (json!({"terminal": true, "web": true}), true),
        (json!({"terminal": false, "web": true}), false),
        (
            json!({"cronjob": {"enabled": true}, "web": {"enabled": true}}),
            true,
        ),
        (json!({"cronjob": {"enabled": false}}), false),
    ];
    for (body, forbidden_on) in shapes {
        let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
        mount_toolsets(&r.server, body.clone()).await;
        let res = provider(&r, Some(API_KEY)).check_toolsets().await;
        if forbidden_on {
            let e = res.expect_err(&format!("{body} should fail"));
            assert!(e.starts_with("ต้องปิด toolset: "), "{body}: {e}");
        } else {
            assert_eq!(res, Ok(()), "{body}");
        }
    }
}

#[tokio::test]
async fn toolsets_uninterpretable_fails_closed() {
    let bad = [
        json!("terminal"),
        json!(42),
        json!([{"name":"terminal"}]),
        json!([{"enabled":true}]),
        json!(["terminal"]),
        json!({"terminal": "yes"}),
        json!({"toolsets": 3}),
        json!([]),
    ];
    for body in bad {
        let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
        mount_health(&r.server).await;
        mount_toolsets(&r.server, body.clone()).await;
        match provider(&r, Some(API_KEY)).availability().await {
            Availability::Unavailable(m) => {
                assert!(
                    m.starts_with("ตรวจสอบเครื่องมือของ Hermes ไม่ได้: "),
                    "{body}: {m}"
                )
            }
            a => panic!("{body}: expected unavailable, got {a:?}"),
        }
    }
    // Non-JSON / HTTP error also fails closed.
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_health(&r.server).await;
    Mock::given(method("GET"))
        .and(path("/v1/toolsets"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&r.server)
        .await;
    assert!(matches!(
        provider(&r, Some(API_KEY)).availability().await,
        Availability::Unavailable(m) if m.starts_with("ตรวจสอบเครื่องมือของ Hermes ไม่ได้: ")
    ));
}

// ---------- turns ----------

#[tokio::test]
async fn run_request_and_events_map_to_chat_events() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_safe_toolsets(&r.server).await;
    mount_run(
        &r.server,
        format!(
            ": keepalive\n\n{}",
            sse(&[
                delta("Hel"),
                ("message.delta", json!({"text":"lo"})),
                ("tool.started", json!({"tool":"run_command","preview":"x"})),
                (
                    "message.interim",
                    json!({"text":"dup","already_streamed":true})
                ),
                (
                    "message.interim",
                    json!({"text":"!","already_streamed":false})
                ),
                ("weird.event", json!({})),
                completed(),
            ])
        ),
    )
    .await;
    let p = provider(&r, Some(API_KEY));
    let mut turn = p.start(req(r.conv)).await.unwrap();
    let evs = collect(&mut turn).await;
    assert_eq!(
        evs,
        vec![
            ChatEvent::TextDelta("Hel".into()),
            ChatEvent::TextDelta("lo".into()),
            ChatEvent::TextDelta("!".into()),
            ChatEvent::Done,
        ]
    );
    let runs = bodies(&r.server, "/v1/runs").await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["input"], "list files");
    assert_eq!(runs[0]["session_id"], format!("conv-{}", r.conv));
    assert!(runs[0]["conversation_history"].is_array());
    assert!(runs[0]["instructions"]
        .as_str()
        .unwrap()
        .contains("run_command"));
}

#[tokio::test]
async fn interrupted_is_error() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_safe_toolsets(&r.server).await;
    mount_run(
        &r.server,
        sse(&[delta("a"), ("run.interrupted", json!({}))]),
    )
    .await;
    let mut turn = provider(&r, Some(API_KEY))
        .start(req(r.conv))
        .await
        .unwrap();
    assert_eq!(
        collect(&mut turn).await,
        vec![
            ChatEvent::TextDelta("a".into()),
            ChatEvent::Error("Hermes หยุดทำงานกลางคัน".into())
        ]
    );
}

#[tokio::test]
async fn http_error_includes_status_without_key() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_safe_toolsets(&r.server).await;
    let long = "x".repeat(10_000);
    Mock::given(method("POST"))
        .and(path("/v1/runs"))
        .respond_with(ResponseTemplate::new(401).set_body_string(format!("bad key {long}")))
        .mount(&r.server)
        .await;
    let mut turn = provider(&r, Some(API_KEY))
        .start(req(r.conv))
        .await
        .unwrap();
    let evs = collect(&mut turn).await;
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        ChatEvent::Error(m) => {
            assert!(m.contains("401"), "{m}");
            assert!(m.contains("bad key"), "{m}");
            assert!(m.len() < 2400, "error not bounded: {}", m.len());
            assert!(!m.contains(API_KEY));
        }
        e => panic!("{e:?}"),
    }
    assert_eq!(r.mcp.active_hermes_turn(), None);
}

#[tokio::test]
async fn approval_request_goes_through_authorize() {
    let r = rig(CommandVerdict::AskUser, ApprovalResponse::Deny).await;
    mount_safe_toolsets(&r.server).await;
    mount_approval(&r.server, 200).await;
    mount_run(
        &r.server,
        sse(&[
            approval("prompt_1", 3, "Remove-Item x"),
            delta("ok"),
            completed(),
        ]),
    )
    .await;
    let mut turn = provider(&r, Some(API_KEY))
        .start(req(r.conv))
        .await
        .unwrap();
    let evs = collect(&mut turn).await;
    assert_eq!(evs.last(), Some(&ChatEvent::Done));
    assert_eq!(
        r.approver.calls.lock().unwrap().clone(),
        vec!["Remove-Item x"]
    );
    let answers = bodies(&r.server, "/v1/runs/run_abc123/approval").await;
    assert_eq!(
        answers,
        vec![json!({"request_id":"prompt_1","execution_generation":3,"choice":"deny"})]
    );
    let cmds = r.store.commands(r.conv).unwrap();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].approval, Approval::Denied);
}

#[tokio::test]
async fn approved_answers_once_and_records_external() {
    let r = rig(CommandVerdict::AskUser, ApprovalResponse::Run).await;
    mount_safe_toolsets(&r.server).await;
    mount_approval(&r.server, 200).await;
    mount_run(
        &r.server,
        sse(&[
            (
                "approval.request",
                json!({"id":"p9","generation":7,"args":{"command":"git push"}}),
            ),
            completed(),
        ]),
    )
    .await;
    let mut turn = provider(&r, Some(API_KEY))
        .start(req(r.conv))
        .await
        .unwrap();
    assert_eq!(collect(&mut turn).await, vec![ChatEvent::Done]);
    let answers = bodies(&r.server, "/v1/runs/run_abc123/approval").await;
    assert_eq!(
        answers,
        vec![json!({"request_id":"p9","execution_generation":7,"choice":"once"})]
    );
    let cmds = r.store.commands(r.conv).unwrap();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].command, "git push");
    assert_eq!(cmds[0].approval, Approval::User);
}

#[tokio::test]
async fn edited_approval_answers_deny() {
    let r = rig(
        CommandVerdict::AskUser,
        ApprovalResponse::RunEdited("Remove-Item safer.txt".into()),
    )
    .await;
    mount_safe_toolsets(&r.server).await;
    mount_approval(&r.server, 200).await;
    mount_run(
        &r.server,
        sse(&[approval("prompt_1", 3, "Remove-Item x"), completed()]),
    )
    .await;
    let mut turn = provider(&r, Some(API_KEY))
        .start(req(r.conv))
        .await
        .unwrap();
    let evs = collect(&mut turn).await;
    assert_eq!(
        evs,
        vec![
            ChatEvent::TextDelta("\n[คำสั่งที่แก้ไขไม่ถูกส่งให้ Hermes เพราะ Hermes จะรันคำสั่งเดิม]\n".into()),
            ChatEvent::Done
        ]
    );
    let answers = bodies(&r.server, "/v1/runs/run_abc123/approval").await;
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["choice"], "deny");
    // Nothing recorded as run.
    assert!(r
        .store
        .commands(r.conv)
        .unwrap()
        .iter()
        .all(|c| c.approval == Approval::Denied));
}

#[tokio::test]
async fn approval_missing_fields_stops_run_and_errors() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Run).await;
    mount_safe_toolsets(&r.server).await;
    mount_approval(&r.server, 200).await;
    mount_run(
        &r.server,
        sse(&[
            ("approval.request", json!({"command":"Remove-Item x"})),
            delta("never"),
            completed(),
        ]),
    )
    .await;
    let mut turn = provider(&r, Some(API_KEY))
        .start(req(r.conv))
        .await
        .unwrap();
    assert_eq!(
        collect(&mut turn).await,
        vec![ChatEvent::Error("คำขออนุมัติจาก Hermes ไม่ครบ".into())]
    );
    assert_eq!(count(&r.server, "/v1/runs/run_abc123/stop").await, 1);
    assert_eq!(count(&r.server, "/v1/runs/run_abc123/approval").await, 0);
    assert!(r.approver.calls.lock().unwrap().is_empty());
    assert_eq!(r.mcp.active_hermes_turn(), None);
}

#[tokio::test]
async fn approval_409_is_ignored_and_stream_continues() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_safe_toolsets(&r.server).await;
    mount_approval(&r.server, 409).await;
    mount_run(
        &r.server,
        sse(&[approval("old", 1, "Get-Date"), delta("after"), completed()]),
    )
    .await;
    let mut turn = provider(&r, Some(API_KEY))
        .start(req(r.conv))
        .await
        .unwrap();
    assert_eq!(
        collect(&mut turn).await,
        vec![ChatEvent::TextDelta("after".into()), ChatEvent::Done]
    );
    assert_eq!(count(&r.server, "/v1/runs/run_abc123/approval").await, 1);
}

#[tokio::test]
async fn approval_without_registered_context_is_denied() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Run).await;
    mount_safe_toolsets(&r.server).await;
    mount_approval(&r.server, 200).await;
    mount_run(
        &r.server,
        sse(&[approval("p1", 2, "Get-Date"), completed()]),
    )
    .await;
    let p = provider(&r, Some(API_KEY));
    let mut turn = p.start(req(r.conv)).await.unwrap();
    // Orchestrator unregisters the turn mid-flight.
    drop(r._guard);
    let evs = collect(&mut turn).await;
    assert_eq!(evs, vec![ChatEvent::Done]);
    assert!(r.approver.calls.lock().unwrap().is_empty());
    let answers = bodies(&r.server, "/v1/runs/run_abc123/approval").await;
    assert_eq!(
        answers,
        vec![json!({"request_id":"p1","execution_generation":2,"choice":"deny"})]
    );
}

#[tokio::test]
async fn dropped_next_during_authorize_answers_exactly_once() {
    let gate = Arc::new(Notify::new());
    let r = rig_with(
        CommandVerdict::AskUser,
        ApprovalResponse::Run,
        Some(gate.clone()),
    )
    .await;
    mount_safe_toolsets(&r.server).await;
    mount_approval(&r.server, 200).await;
    mount_run(
        &r.server,
        sse(&[
            approval("p1", 5, "Remove-Item x"),
            delta("after"),
            completed(),
        ]),
    )
    .await;
    let mut turn = provider(&r, Some(API_KEY))
        .start(req(r.conv))
        .await
        .unwrap();
    // next() blocks on the approver; drop it a few times.
    for _ in 0..3 {
        let res = tokio::time::timeout(Duration::from_millis(200), turn.next()).await;
        assert!(res.is_err(), "next() should still be waiting for approval");
    }
    assert_eq!(r.approver.calls.lock().unwrap().len(), 1);
    gate.notify_one();
    let evs = collect(&mut turn).await;
    assert_eq!(
        evs,
        vec![ChatEvent::TextDelta("after".into()), ChatEvent::Done]
    );
    assert_eq!(r.approver.calls.lock().unwrap().len(), 1);
    let answers = bodies(&r.server, "/v1/runs/run_abc123/approval").await;
    assert_eq!(
        answers,
        vec![json!({"request_id":"p1","execution_generation":5,"choice":"once"})]
    );
    assert_eq!(r.store.commands(r.conv).unwrap().len(), 1);
}

#[tokio::test]
async fn active_turn_cleared_after_done_and_on_error() {
    // Done
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_safe_toolsets(&r.server).await;
    mount_run(&r.server, sse(&[delta("x"), completed()])).await;
    let p = provider(&r, Some(API_KEY));
    let mut turn = p.start(req(r.conv)).await.unwrap();
    assert_eq!(r.mcp.active_hermes_turn().as_deref(), Some(TOKEN));
    collect(&mut turn).await;
    assert_eq!(r.mcp.active_hermes_turn(), None);
    drop(turn);

    // Error (stream ends without run.completed)
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_safe_toolsets(&r.server).await;
    mount_run(&r.server, sse(&[delta("x")])).await;
    let p = provider(&r, Some(API_KEY));
    let mut turn = p.start(req(r.conv)).await.unwrap();
    assert_eq!(r.mcp.active_hermes_turn().as_deref(), Some(TOKEN));
    let evs = collect(&mut turn).await;
    assert!(matches!(evs.last(), Some(ChatEvent::Error(_))), "{evs:?}");
    assert_eq!(r.mcp.active_hermes_turn(), None);
    drop(turn);

    // Cancel
    let mut turn = p.start(req(r.conv)).await.unwrap();
    assert_eq!(r.mcp.active_hermes_turn().as_deref(), Some(TOKEN));
    let _ = turn.next().await;
    turn.cancel().await;
    assert_eq!(r.mcp.active_hermes_turn(), None);
    assert_eq!(turn.next().await, None);
    assert!(count(&r.server, "/v1/runs/run_abc123/stop").await >= 1);
    drop(turn);

    // Drop
    let turn = p.start(req(r.conv)).await.unwrap();
    assert_eq!(r.mcp.active_hermes_turn().as_deref(), Some(TOKEN));
    drop(turn);
    assert_eq!(r.mcp.active_hermes_turn(), None);
}

#[tokio::test]
async fn start_without_registered_context_fails() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_safe_toolsets(&r.server).await;
    let mut rq = req(r.conv);
    rq.turn_token = "unregistered".into();
    assert!(provider(&r, Some(API_KEY)).start(rq).await.is_err());
    assert_eq!(r.mcp.active_hermes_turn(), None);
}

#[tokio::test]
async fn only_one_turn_at_a_time() {
    let r = rig(CommandVerdict::AutoRun, ApprovalResponse::Deny).await;
    mount_safe_toolsets(&r.server).await;
    mount_run(&r.server, sse(&[completed()])).await;
    let p = provider(&r, Some(API_KEY)).with_permit_timeout(Duration::from_millis(200));
    let first = p.start(req(r.conv)).await.unwrap();
    let err = p
        .start(req(r.conv))
        .await
        .err()
        .expect("second start must fail");
    assert_eq!(err.to_string(), "Hermes กำลังทำงานอื่นอยู่");
    // The failed attempt must not clear the first turn's binding.
    assert_eq!(r.mcp.active_hermes_turn().as_deref(), Some(TOKEN));
    drop(first);
    let mut second = p.start(req(r.conv)).await.unwrap();
    assert_eq!(collect(&mut second).await, vec![ChatEvent::Done]);
}
