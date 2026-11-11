#![cfg(windows)]
use assistant_core::command_service::*;
use assistant_core::decision::gate::{CommandCheck, CommandChecker};
use assistant_core::decision::policy::{CommandVerdict, DecisionSource, Thresholds};
use assistant_core::decision::router::Router;
use assistant_core::jev::{JevAnswer, JevApi, JevError, JevQuestion, JevResponse};
use assistant_core::mcp_server::{McpServer, McpServerHandle};
use assistant_core::orchestrator::*;
use assistant_core::providers::{ChatProvider, ProviderTurn};
use assistant_core::runner::RunLimits;
use assistant_core::settings::Settings;
use assistant_core::store::{DecisionKind, Store};
use assistant_core::{Availability, ChatEvent, ProviderId, Role, TurnRequest};
use async_trait::async_trait;
use indexmap::IndexMap;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::RwLock;

const NO_PROVIDER: &str = "ไม่มีตัวคุยที่ใช้งานได้";
const DENIED_TEXT: &str = "คำสั่งถูกปฏิเสธโดยผู้ใช้หรือระบบตรวจสอบ";

// ---------- fakes ----------

struct FakeChecker(CommandVerdict);

#[async_trait]
impl CommandChecker for FakeChecker {
    async fn check(&self, _command: &str, _cwd: &Path) -> CommandCheck {
        CommandCheck {
            verdict: self.0,
            is_safe: Some(0.95),
            risk: Some("read_only".into()),
            risk_p: Some(0.95),
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
    fn events(&self) -> Vec<AppEvent> {
        self.0.lock().unwrap().clone()
    }
}

struct FakeJev {
    choice: String,
    prob: f64,
    calls: AtomicUsize,
}

impl FakeJev {
    fn new(choice: &str, prob: f64) -> Arc<Self> {
        Arc::new(Self {
            choice: choice.into(),
            prob,
            calls: AtomicUsize::new(0),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl JevApi for FakeJev {
    async fn ask(
        &self,
        _state: &str,
        _questions: IndexMap<String, JevQuestion>,
    ) -> std::result::Result<JevResponse, JevError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut probabilities = HashMap::new();
        probabilities.insert(self.choice.clone(), self.prob);
        let mut answers = HashMap::new();
        answers.insert(
            "route".to_string(),
            JevAnswer::Choice {
                choice: self.choice.clone(),
                probabilities,
                confidence: self.prob,
            },
        );
        Ok(JevResponse {
            model: "jev-test".into(),
            answers,
        })
    }
}

#[derive(Clone)]
enum Step {
    Ev(ChatEvent),
    /// Never yields: the turn hangs until cancelled.
    Hang,
    /// `next()` returns `None`.
    End,
    /// Waits until notified, then continues with the next step.
    Gate(Arc<tokio::sync::Notify>),
}

#[derive(Default)]
struct Shared {
    submitted: Mutex<Vec<(String, String)>>,
    cancelled: AtomicBool,
    requests: Mutex<Vec<TurnRequest>>,
}

struct ScriptedProvider {
    id: ProviderId,
    avail: Mutex<Availability>,
    scripts: Mutex<VecDeque<Vec<Step>>>,
    shared: Arc<Shared>,
}

impl ScriptedProvider {
    fn new(id: ProviderId) -> Arc<Self> {
        Arc::new(Self {
            id,
            avail: Mutex::new(Availability::Ready),
            scripts: Mutex::new(VecDeque::new()),
            shared: Arc::new(Shared::default()),
        })
    }
    fn unavailable(id: ProviderId, why: &str) -> Arc<Self> {
        let p = Self::new(id);
        *p.avail.lock().unwrap() = Availability::Unavailable(why.into());
        p
    }
    fn script(&self, steps: Vec<Step>) {
        self.scripts.lock().unwrap().push_back(steps);
    }
    fn last_request(&self) -> Option<TurnRequest> {
        self.shared.requests.lock().unwrap().last().cloned()
    }
    fn starts(&self) -> usize {
        self.shared.requests.lock().unwrap().len()
    }
    fn submitted(&self) -> Vec<(String, String)> {
        self.shared.submitted.lock().unwrap().clone()
    }
    fn was_cancelled(&self) -> bool {
        self.shared.cancelled.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ChatProvider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        self.id
    }
    async fn availability(&self) -> Availability {
        self.avail.lock().unwrap().clone()
    }
    async fn start(
        &self,
        req: TurnRequest,
    ) -> assistant_core::providers::Result<Box<dyn ProviderTurn>> {
        self.shared.requests.lock().unwrap().push(req);
        let steps = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![Step::Ev(ChatEvent::Done)]);
        Ok(Box::new(ScriptedTurn {
            steps: steps.into(),
            shared: self.shared.clone(),
        }))
    }
}

struct ScriptedTurn {
    steps: VecDeque<Step>,
    shared: Arc<Shared>,
}

#[async_trait]
impl ProviderTurn for ScriptedTurn {
    async fn next(&mut self) -> Option<ChatEvent> {
        match self.steps.front().cloned() {
            None | Some(Step::End) => None,
            Some(Step::Hang) => std::future::pending().await,
            Some(Step::Gate(n)) => {
                n.notified().await;
                self.steps.pop_front();
                Box::pin(self.next()).await
            }
            Some(Step::Ev(e)) => {
                self.steps.pop_front();
                Some(e)
            }
        }
    }
    async fn submit_command_result(
        &mut self,
        call_id: &str,
        text: String,
    ) -> assistant_core::providers::Result<()> {
        self.shared
            .submitted
            .lock()
            .unwrap()
            .push((call_id.to_string(), text));
        Ok(())
    }
    async fn cancel(&mut self) {
        self.shared.cancelled.store(true, Ordering::SeqCst);
    }
}

// ---------- rig ----------

struct Rig {
    orch: Arc<Orchestrator>,
    store: Arc<Store>,
    sink: Arc<VecSink>,
    mcp: McpServerHandle,
    conv: i64,
    _dir: tempfile::TempDir,
}

struct Opts {
    providers: Vec<Arc<ScriptedProvider>>,
    jev: Option<Arc<FakeJev>>,
    settings: Settings,
    verdict: CommandVerdict,
}

impl Opts {
    fn new(providers: Vec<Arc<ScriptedProvider>>) -> Self {
        Self {
            providers,
            jev: None,
            settings: Settings::default(),
            verdict: CommandVerdict::AutoRun,
        }
    }
}

async fn rig(o: Opts) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let conv = store.create_conversation("t", dir.path()).unwrap().id;
    let sink = Arc::new(VecSink::default());
    let commands = Arc::new(CommandService::new(
        Arc::new(FakeChecker(o.verdict)),
        Arc::new(DenyApprover),
        store.clone(),
        sink.clone(),
        RunLimits::default(),
        Duration::from_secs(5),
    ));
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mcp = McpServer::start(addr, commands.clone(), "k".repeat(24))
        .await
        .unwrap();
    let jev = o.jev.map(|j| j as Arc<dyn JevApi>);
    let router = Arc::new(RwLock::new(Router::new(jev, Thresholds::default())));
    let providers: HashMap<ProviderId, Arc<dyn ChatProvider>> = o
        .providers
        .into_iter()
        .map(|p| (p.id, p as Arc<dyn ChatProvider>))
        .collect();
    let orch = Arc::new(Orchestrator::new(
        store.clone(),
        router,
        providers,
        commands,
        mcp.clone(),
        sink.clone(),
        Arc::new(RwLock::new(o.settings)),
    ));
    Rig {
        orch,
        store,
        sink,
        mcp,
        conv,
        _dir: dir,
    }
}

fn roles(store: &Store, conv: i64) -> Vec<Role> {
    store
        .messages(conv)
        .unwrap()
        .iter()
        .map(|m| m.role)
        .collect()
}

fn text(s: &str) -> Step {
    Step::Ev(ChatEvent::TextDelta(s.into()))
}

fn done() -> Step {
    Step::Ev(ChatEvent::Done)
}

async fn wait_started(p: &ScriptedProvider, n: usize) {
    for _ in 0..500 {
        if p.starts() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("provider never started");
}

// ---------- brief tests ----------

#[tokio::test]
async fn auto_route_low_confidence_returns_choice_and_keeps_message() {
    let api = ScriptedProvider::new(ProviderId::Api);
    let hermes = ScriptedProvider::new(ProviderId::Hermes);
    let jev = FakeJev::new("api", 0.5);
    let mut o = Opts::new(vec![api.clone(), hermes.clone()]);
    o.jev = Some(jev.clone());
    let r = rig(o).await;

    let out = r
        .orch
        .send(r.conv, "hello".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    let msgs = r.store.messages(r.conv).unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(msgs[0].content, "hello");
    assert_eq!(
        out,
        TurnOutcome::NeedsProviderChoice {
            user_message_id: msgs[0].id,
            ranked: vec![(ProviderId::Api, 0.5), (ProviderId::Hermes, 0.0)],
        }
    );
    assert_eq!(jev.calls(), 1);
    assert_eq!(api.starts() + hermes.starts(), 0);
    let d = r.store.decisions(r.conv).unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].kind, DecisionKind::Route);
    assert_eq!(d[0].message_id, msgs[0].id);
    assert_eq!(d[0].payload["source"]["kind"], "jev");
}

#[tokio::test]
async fn continue_with_does_not_duplicate_user_message() {
    let api = ScriptedProvider::new(ProviderId::Api);
    let hermes = ScriptedProvider::new(ProviderId::Hermes);
    let mut o = Opts::new(vec![api.clone(), hermes.clone()]);
    o.jev = Some(FakeJev::new("api", 0.5));
    let r = rig(o).await;
    let TurnOutcome::NeedsProviderChoice {
        user_message_id, ..
    } = r
        .orch
        .send(r.conv, "hello".into(), ProviderChoice::Auto)
        .await
        .unwrap()
    else {
        panic!("expected choice");
    };
    api.script(vec![text("hi "), text("there"), done()]);
    let out = r
        .orch
        .continue_with(r.conv, user_message_id, ProviderId::Api)
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Completed {
            provider: ProviderId::Api
        }
    );
    let msgs = r.store.messages(r.conv).unwrap();
    assert_eq!(roles(&r.store, r.conv), vec![Role::User, Role::Assistant]);
    assert_eq!(msgs[1].content, "hi there");
    assert_eq!(msgs[1].provider, Some(ProviderId::Api));
    let req = api.last_request().unwrap();
    assert_eq!(req.history.len(), 1);
    assert_eq!(req.history[0].id, user_message_id);
    let d = r.store.decisions(r.conv).unwrap();
    assert_eq!(d.len(), 2);
    assert_eq!(d[1].payload["source"]["kind"], "manual");
    assert!(r
        .sink
        .events()
        .iter()
        .any(|e| matches!(e, AppEvent::TurnFinished { .. })));
}

#[tokio::test]
async fn chosen_provider_unavailable_fails_gracefully() {
    let api = ScriptedProvider::new(ProviderId::Api);
    let hermes = ScriptedProvider::unavailable(ProviderId::Hermes, "hermes ไม่ทำงาน");
    let r = rig(Opts::new(vec![api.clone(), hermes.clone()])).await;
    let out = r
        .orch
        .send(
            r.conv,
            "hi".into(),
            ProviderChoice::Manual(ProviderId::Hermes),
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Failed {
            provider: ProviderId::Hermes,
            error: "hermes ไม่ทำงาน".into()
        }
    );
    let msgs = r.store.messages(r.conv).unwrap();
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(msgs[1].role, Role::Notice);
    assert!(msgs[1].content.contains("hermes ไม่ทำงาน"));
    assert_eq!(hermes.starts(), 0);
    assert!(r.sink.events().iter().any(|e| matches!(
        e,
        AppEvent::TurnFailed {
            provider: ProviderId::Hermes,
            ..
        }
    )));

    // continue_with on the same unavailable provider fails the same way.
    let out = r
        .orch
        .continue_with(r.conv, msgs[0].id, ProviderId::Hermes)
        .await
        .unwrap();
    assert!(matches!(
        out,
        TurnOutcome::Failed {
            provider: ProviderId::Hermes,
            ..
        }
    ));

    let out = r
        .orch
        .continue_with(r.conv, msgs[0].id, ProviderId::Api)
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Completed {
            provider: ProviderId::Api
        }
    );
    let users = r
        .store
        .messages(r.conv)
        .unwrap()
        .iter()
        .filter(|m| m.role == Role::User)
        .count();
    assert_eq!(users, 1);
}

#[tokio::test]
async fn provider_error_mid_stream_keeps_partial_and_notice() {
    let api = ScriptedProvider::new(ProviderId::Api);
    api.script(vec![
        text("partial"),
        Step::Ev(ChatEvent::Error("boom".into())),
    ]);
    let r = rig(Opts::new(vec![api.clone()])).await;
    let out = r
        .orch
        .send(r.conv, "hi".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Failed {
            provider: ProviderId::Api,
            error: "boom".into()
        }
    );
    let msgs = r.store.messages(r.conv).unwrap();
    assert_eq!(
        roles(&r.store, r.conv),
        vec![Role::User, Role::Assistant, Role::Notice]
    );
    assert_eq!(msgs[1].content, "partial");
    assert!(msgs[2].content.contains("boom"));
    let ev = r.sink.events();
    assert!(ev.iter().any(|e| matches!(e,
        AppEvent::AssistantDelta { text, .. } if text == "partial")));
    assert!(ev.iter().any(|e| matches!(e,
        AppEvent::TurnFailed { provider: ProviderId::Api, error, .. } if error == "boom")));
    assert!(!ev
        .iter()
        .any(|e| matches!(e, AppEvent::TurnFinished { .. })));
}

#[tokio::test]
async fn command_proposed_runs_through_service_and_result_is_stored() {
    let api = ScriptedProvider::new(ProviderId::Api);
    api.script(vec![
        Step::Ev(ChatEvent::CommandProposed {
            call_id: "c1".into(),
            command: "Write-Output hello-orch".into(),
            cwd: None,
        }),
        text("done"),
        done(),
    ]);
    let r = rig(Opts::new(vec![api.clone()])).await;
    let out = r
        .orch
        .send(r.conv, "run it".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Completed {
            provider: ProviderId::Api
        }
    );
    let msgs = r.store.messages(r.conv).unwrap();
    assert_eq!(
        roles(&r.store, r.conv),
        vec![Role::User, Role::CommandResult, Role::Assistant]
    );
    assert!(msgs[1].content.contains("> Write-Output hello-orch"));
    assert!(msgs[1].content.contains("hello-orch\n") || msgs[1].content.ends_with("hello-orch"));
    assert!(msgs[1].content.contains("exit: 0"));
    assert_eq!(msgs[1].provider, Some(ProviderId::Api));
    assert_eq!(msgs[2].content, "done");
    let sub = api.submitted();
    assert_eq!(sub.len(), 1);
    assert_eq!(sub[0].0, "c1");
    assert!(sub[0].1.contains("hello-orch"));
    assert_eq!(r.store.commands(r.conv).unwrap().len(), 1);
}

#[tokio::test]
async fn switching_providers_passes_full_history() {
    let api = ScriptedProvider::new(ProviderId::Api);
    let hermes = ScriptedProvider::new(ProviderId::Hermes);
    api.script(vec![text("api answer"), done()]);
    let r = rig(Opts::new(vec![api.clone(), hermes.clone()])).await;
    r.orch
        .send(
            r.conv,
            "one".into(),
            ProviderChoice::Manual(ProviderId::Api),
        )
        .await
        .unwrap();
    let out = r
        .orch
        .send(
            r.conv,
            "two".into(),
            ProviderChoice::Manual(ProviderId::Hermes),
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Completed {
            provider: ProviderId::Hermes
        }
    );
    let h: Vec<String> = hermes
        .last_request()
        .unwrap()
        .history
        .into_iter()
        .map(|m| m.content)
        .collect();
    assert_eq!(h, vec!["one", "api answer", "two"]);
}

#[tokio::test]
async fn manual_mode_skips_jev() {
    let api = ScriptedProvider::new(ProviderId::Api);
    let hermes = ScriptedProvider::new(ProviderId::Hermes);
    let jev = FakeJev::new("api", 0.99);
    let mut o = Opts::new(vec![api.clone(), hermes.clone()]);
    o.jev = Some(jev.clone());
    o.settings.auto_route = false;
    o.settings.default_provider = Some(ProviderId::Hermes);
    let r = rig(o).await;
    let out = r
        .orch
        .send(r.conv, "hi".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Completed {
            provider: ProviderId::Hermes
        }
    );
    assert_eq!(jev.calls(), 0);
    assert_eq!(hermes.starts(), 1);

    // Manual choice also skips Jev even with auto_route on.
    let jev2 = FakeJev::new("api", 0.99);
    let mut o = Opts::new(vec![
        ScriptedProvider::new(ProviderId::Api),
        ScriptedProvider::new(ProviderId::Hermes),
    ]);
    o.jev = Some(jev2.clone());
    let r = rig(o).await;
    r.orch
        .send(r.conv, "hi".into(), ProviderChoice::Manual(ProviderId::Api))
        .await
        .unwrap();
    assert_eq!(jev2.calls(), 0);
}

// ---------- extra tests ----------

#[tokio::test]
async fn auto_route_high_confidence_emits_route_decided() {
    let api = ScriptedProvider::new(ProviderId::Api);
    let hermes = ScriptedProvider::new(ProviderId::Hermes);
    let mut o = Opts::new(vec![api.clone(), hermes.clone()]);
    o.jev = Some(FakeJev::new("hermes", 0.9));
    let r = rig(o).await;
    let out = r
        .orch
        .send(r.conv, "hi".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Completed {
            provider: ProviderId::Hermes
        }
    );
    assert!(r.sink.events().iter().any(|e| matches!(
        e,
        AppEvent::RouteDecided {
            provider: ProviderId::Hermes,
            source: DecisionSource::Jev { .. },
            probability: Some(p),
            ..
        } if (*p - 0.9).abs() < 1e-9
    )));
    assert_eq!(api.starts(), 0);
    let d = r.store.decisions(r.conv).unwrap();
    assert_eq!(d[0].payload["probability"], 0.9);
}

#[tokio::test]
async fn auto_route_off_without_ready_default_asks() {
    let mut o = Opts::new(vec![
        ScriptedProvider::new(ProviderId::Api),
        ScriptedProvider::new(ProviderId::Hermes),
    ]);
    o.settings.auto_route = false;
    let r = rig(o).await;
    let out = r
        .orch
        .send(r.conv, "hi".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    let TurnOutcome::NeedsProviderChoice { ranked, .. } = out else {
        panic!("expected choice: {out:?}");
    };
    assert_eq!(
        ranked,
        vec![(ProviderId::Hermes, 0.0), (ProviderId::Api, 0.0)]
    );
}

#[tokio::test]
async fn cancel_mid_stream_stores_partial_and_notice() {
    let api = ScriptedProvider::new(ProviderId::Api);
    api.script(vec![text("half"), Step::Hang]);
    let r = rig(Opts::new(vec![api.clone()])).await;
    let orch = r.orch.clone();
    let conv = r.conv;
    let h = tokio::spawn(async move { orch.send(conv, "hi".into(), ProviderChoice::Auto).await });
    wait_started(&api, 1).await;
    let token = api.last_request().unwrap().turn_token;
    let ctx = r.mcp.context(&token).expect("turn registered");
    assert!(!ctx.cancel.is_cancelled());
    tokio::time::sleep(Duration::from_millis(50)).await;
    r.orch.cancel(conv);
    let out = tokio::time::timeout(Duration::from_secs(5), h)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Cancelled {
            provider: ProviderId::Api
        }
    );
    assert!(api.was_cancelled());
    assert!(ctx.cancel.is_cancelled());
    let msgs = r.store.messages(conv).unwrap();
    assert_eq!(
        roles(&r.store, conv),
        vec![Role::User, Role::Assistant, Role::Notice]
    );
    assert_eq!(msgs[1].content, "half");
    assert_eq!(msgs[2].content, "ผู้ใช้หยุดการตอบ");
    assert!(r
        .sink
        .events()
        .iter()
        .any(|e| matches!(e, AppEvent::TurnFinished { .. })));
    // A new turn can start afterwards.
    let out = r
        .orch
        .send(conv, "again".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Completed { .. }));
}

#[tokio::test]
async fn second_send_while_running_is_busy() {
    let api = ScriptedProvider::new(ProviderId::Api);
    api.script(vec![Step::Hang]);
    let r = rig(Opts::new(vec![api.clone()])).await;
    let orch = r.orch.clone();
    let conv = r.conv;
    let h = tokio::spawn(async move { orch.send(conv, "hi".into(), ProviderChoice::Auto).await });
    wait_started(&api, 1).await;
    let err = r
        .orch
        .send(conv, "second".into(), ProviderChoice::Auto)
        .await
        .unwrap_err();
    assert!(matches!(err, OrchestratorError::Busy));
    let err = r
        .orch
        .continue_with(conv, 1, ProviderId::Api)
        .await
        .unwrap_err();
    assert!(matches!(err, OrchestratorError::Busy));
    // The busy send stored nothing.
    assert_eq!(roles(&r.store, conv), vec![Role::User]);
    r.orch.cancel(conv);
    h.await.unwrap().unwrap();
}

#[tokio::test]
async fn no_ready_provider_fails_and_keeps_message() {
    let r = rig(Opts::new(vec![
        ScriptedProvider::unavailable(ProviderId::Api, "no key"),
        ScriptedProvider::unavailable(ProviderId::Hermes, "down"),
    ]))
    .await;
    let out = r
        .orch
        .send(r.conv, "hi".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Failed {
            provider: ProviderId::Api,
            error: NO_PROVIDER.into()
        }
    );
    let msgs = r.store.messages(r.conv).unwrap();
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(msgs[0].content, "hi");
    assert!(r.sink.events().iter().any(|e| matches!(e,
        AppEvent::TurnFailed { error, .. } if error == NO_PROVIDER)));

    let out = r
        .orch
        .send(
            r.conv,
            "x".into(),
            ProviderChoice::Manual(ProviderId::Hermes),
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Failed {
            provider: ProviderId::Hermes,
            error: NO_PROVIDER.into()
        }
    );
}

#[tokio::test]
async fn denied_command_is_stored_and_submitted() {
    let api = ScriptedProvider::new(ProviderId::Api);
    api.script(vec![
        Step::Ev(ChatEvent::CommandProposed {
            call_id: "c9".into(),
            command: "Write-Output nope".into(),
            cwd: None,
        }),
        done(),
    ]);
    let mut o = Opts::new(vec![api.clone()]);
    o.verdict = CommandVerdict::AskUser;
    let r = rig(o).await;
    let out = r
        .orch
        .send(r.conv, "x".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Completed { .. }));
    let msgs = r.store.messages(r.conv).unwrap();
    assert_eq!(msgs[1].role, Role::CommandResult);
    assert_eq!(msgs[1].content, "[คำสั่งถูกปฏิเสธ] > Write-Output nope");
    assert_eq!(
        api.submitted(),
        vec![("c9".to_string(), DENIED_TEXT.to_string())]
    );
}

#[tokio::test]
async fn turn_is_registered_with_mcp_only_while_running() {
    let api = ScriptedProvider::new(ProviderId::Api);
    api.script(vec![Step::Hang]);
    api.script(vec![done()]);
    let r = rig(Opts::new(vec![api.clone()])).await;
    let orch = r.orch.clone();
    let conv = r.conv;
    let h = tokio::spawn(async move { orch.send(conv, "hi".into(), ProviderChoice::Auto).await });
    wait_started(&api, 1).await;
    let req = api.last_request().unwrap();
    assert!(uuid_like(&req.turn_token));
    let ctx = r
        .mcp
        .context(&req.turn_token)
        .expect("registered during turn");
    assert_eq!(ctx.conversation_id, conv);
    assert_eq!(ctx.provider, ProviderId::Api);
    assert_eq!(ctx.cwd, req.cwd);
    assert_eq!(ctx.max_per_turn, Settings::default().max_commands_per_turn);
    r.orch.cancel(conv);
    h.await.unwrap().unwrap();
    assert!(r.mcp.context(&req.turn_token).is_none());

    // Completed turns are unregistered too, and get a fresh token.
    r.orch
        .send(conv, "two".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    let req2 = api.last_request().unwrap();
    assert_ne!(req2.turn_token, req.turn_token);
    assert!(r.mcp.context(&req2.turn_token).is_none());
}

fn uuid_like(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

#[tokio::test]
async fn stream_ending_without_done_fails() {
    let api = ScriptedProvider::new(ProviderId::Api);
    api.script(vec![text("cut"), Step::End]);
    let r = rig(Opts::new(vec![api.clone()])).await;
    let out = r
        .orch
        .send(r.conv, "hi".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    assert_eq!(
        out,
        TurnOutcome::Failed {
            provider: ProviderId::Api,
            error: "ตัวคุยหยุดตอบกลางคัน".into()
        }
    );
    assert_eq!(
        roles(&r.store, r.conv),
        vec![Role::User, Role::Assistant, Role::Notice]
    );
}

#[tokio::test]
async fn continue_with_rejects_wrong_message() {
    let api = ScriptedProvider::new(ProviderId::Api);
    api.script(vec![text("a"), done()]);
    let r = rig(Opts::new(vec![api.clone()])).await;
    r.orch
        .send(r.conv, "first".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    let msgs = r.store.messages(r.conv).unwrap();
    // Assistant message, not a user message.
    let err = r
        .orch
        .continue_with(r.conv, msgs[1].id, ProviderId::Api)
        .await
        .unwrap_err();
    assert!(matches!(err, OrchestratorError::NotFound));
    r.orch
        .send(r.conv, "second".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    // Not the latest user message.
    let err = r
        .orch
        .continue_with(r.conv, msgs[0].id, ProviderId::Api)
        .await
        .unwrap_err();
    assert!(matches!(err, OrchestratorError::NotFound));
    // Unknown conversation.
    let err = r
        .orch
        .send(9999, "x".into(), ProviderChoice::Auto)
        .await
        .unwrap_err();
    assert!(matches!(err, OrchestratorError::NotFound));
}

#[tokio::test]
async fn mcp_command_from_cli_turn_is_in_history_for_next_provider() {
    use rmcp::model::{CallToolRequestParams, ClientConfig};
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::ServiceExt;

    let cli = ScriptedProvider::new(ProviderId::ClaudeCli);
    let api = ScriptedProvider::new(ProviderId::Api);
    let gate = Arc::new(tokio::sync::Notify::new());
    cli.script(vec![text("listing"), Step::Gate(gate.clone()), done()]);
    api.script(vec![text("seen"), done()]);
    let r = rig(Opts::new(vec![cli.clone(), api.clone()])).await;

    let orch = r.orch.clone();
    let conv = r.conv;
    let first = tokio::spawn(async move {
        orch.send(
            conv,
            "list files".into(),
            ProviderChoice::Manual(ProviderId::ClaudeCli),
        )
        .await
    });
    wait_started(&cli, 1).await;
    let token = cli.last_request().unwrap().turn_token;
    let transport = StreamableHttpClientTransport::from_uri(r.mcp.url_for_turn(&token));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    let args = serde_json::json!({"command": "Write-Output via-mcp"})
        .as_object()
        .unwrap()
        .clone();
    let res = client
        .call_tool(CallToolRequestParams::new("run_command").with_arguments(args))
        .await
        .unwrap();
    assert_ne!(res.is_error, Some(true));
    let _ = client.cancel().await;
    gate.notify_one();
    assert!(matches!(
        first.await.unwrap().unwrap(),
        TurnOutcome::Completed { .. }
    ));

    r.orch
        .send(
            r.conv,
            "what ran?".into(),
            ProviderChoice::Manual(ProviderId::Api),
        )
        .await
        .unwrap();
    let hist = api.last_request().unwrap().history;
    let cmd = hist
        .iter()
        .find(|m| m.role == Role::CommandResult)
        .expect("MCP command result must be in history");
    assert_eq!(cmd.provider, Some(ProviderId::ClaudeCli));
    assert!(
        cmd.content.starts_with("[คำสั่งที่รันโดย claude_cli]\n"),
        "{}",
        cmd.content
    );
    assert!(cmd.content.contains("> Write-Output via-mcp\nexit: 0\n"));
    assert!(cmd.content.contains("via-mcp"));
}

#[tokio::test]
async fn whitespace_only_assistant_reply_is_not_stored() {
    let api = ScriptedProvider::new(ProviderId::Api);
    api.script(vec![text("\n  \n"), done()]);
    let r = rig(Opts::new(vec![api.clone()])).await;
    let out = r
        .orch
        .send(r.conv, "hi".into(), ProviderChoice::Auto)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Completed { .. }));
    assert_eq!(roles(&r.store, r.conv), vec![Role::User]);
}
