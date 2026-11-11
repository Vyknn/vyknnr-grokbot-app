#![cfg(windows)]
use assistant_core::command_service::*;
use assistant_core::decision::gate::{CommandCheck, CommandChecker};
use assistant_core::decision::policy::{CommandVerdict, DecisionSource};
use assistant_core::runner::RunLimits;
use assistant_core::store::{Approval, Store};
use assistant_core::ProviderId;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

struct FakeChecker {
    verdict: CommandVerdict,
    seen: Mutex<Vec<String>>,
}

impl FakeChecker {
    fn new(verdict: CommandVerdict) -> Arc<Self> {
        Arc::new(Self {
            verdict,
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl CommandChecker for FakeChecker {
    async fn check(&self, command: &str, _cwd: &Path) -> CommandCheck {
        self.seen.lock().unwrap().push(command.to_string());
        CommandCheck {
            verdict: self.verdict,
            is_safe: Some(0.5),
            risk: Some("other".into()),
            risk_p: Some(0.5),
            source: DecisionSource::Jev {
                model: "fake-jev".into(),
            },
        }
    }
}

enum Script {
    Answer(ApprovalResponse),
    Never,
    DroppedSender,
}

struct ScriptedApprover {
    script: Mutex<VecDeque<Script>>,
    reqs: Mutex<Vec<ApprovalRequest>>,
}

impl ScriptedApprover {
    fn new(s: Vec<Script>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(s.into()),
            reqs: Mutex::new(Vec::new()),
        })
    }
    fn calls(&self) -> usize {
        self.reqs.lock().unwrap().len()
    }
    fn reqs(&self) -> Vec<ApprovalRequest> {
        self.reqs.lock().unwrap().clone()
    }
}

#[async_trait]
impl Approver for ScriptedApprover {
    async fn request(&self, req: ApprovalRequest) -> ApprovalResponse {
        self.reqs.lock().unwrap().push(req);
        let next = self.script.lock().unwrap().pop_front();
        match next {
            Some(Script::Answer(a)) => a,
            Some(Script::Never) => std::future::pending().await,
            Some(Script::DroppedSender) => {
                let (tx, rx) = tokio::sync::oneshot::channel::<ApprovalResponse>();
                drop(tx);
                rx.await.unwrap_or(ApprovalResponse::Deny)
            }
            None => panic!("approver asked more times than scripted"),
        }
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

struct Rig {
    svc: CommandService,
    approver: Arc<ScriptedApprover>,
    checker: Arc<FakeChecker>,
    sink: Arc<VecSink>,
    store: Arc<Store>,
    conv: i64,
}

fn rig(verdict: CommandVerdict, script: Vec<Script>, timeout: Duration) -> Rig {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let conv = store
        .create_conversation("t", &std::env::temp_dir())
        .unwrap()
        .id;
    let checker = FakeChecker::new(verdict);
    let approver = ScriptedApprover::new(script);
    let sink = Arc::new(VecSink::default());
    let svc = CommandService::new(
        checker.clone(),
        approver.clone(),
        store.clone(),
        sink.clone(),
        RunLimits::default(),
        timeout,
    );
    Rig {
        svc,
        approver,
        checker,
        sink,
        store,
        conv,
    }
}

fn ctx(conv: i64, max: u32) -> CommandContext {
    CommandContext {
        conversation_id: conv,
        turn_token: "turn-1".into(),
        cwd: std::env::temp_dir(),
        provider: ProviderId::Api,
        count: Arc::new(AtomicU32::new(0)),
        max_per_turn: max,
        cancel: CancellationToken::new(),
    }
}

const T: Duration = Duration::from_secs(300);

#[tokio::test]
async fn auto_run_does_not_ask() {
    let r = rig(CommandVerdict::AutoRun, vec![], T);
    let out = r.svc.execute(&ctx(r.conv, 10), "Write-Output ok").await;
    let CommandOutcome::Ran {
        command_id, result, ..
    } = out
    else {
        panic!("expected Ran");
    };
    assert_eq!(r.approver.calls(), 0);
    assert!(result.output.contains("ok"));
    assert_eq!(result.exit_code, Some(0));
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].id, command_id);
    assert_eq!(recs[0].approval, Approval::Auto);
    assert_eq!(recs[0].exit_code, Some(0));
    assert!(recs[0].output.contains("ok"));
    assert!(recs[0].finished_at.is_some());
    assert_eq!(recs[0].check["verdict"], "auto_run");
    let ev = r.sink.events();
    assert!(
        matches!(&ev[0], AppEvent::CommandStarted { command_id: id, approval: Approval::Auto, .. } if *id == command_id)
    );
    assert!(ev
        .iter()
        .any(|e| matches!(e, AppEvent::CommandOutput { command_id: id, .. } if *id == command_id)));
    assert!(matches!(
        ev.last().unwrap(),
        AppEvent::CommandFinished {
            exit_code: Some(0),
            timed_out: false,
            ..
        }
    ));
}

#[tokio::test]
async fn ask_user_deny_never_runs() {
    let r = rig(
        CommandVerdict::AskUser,
        vec![Script::Answer(ApprovalResponse::Deny)],
        T,
    );
    let out = r.svc.execute(&ctx(r.conv, 10), "Remove-Item x").await;
    assert!(matches!(out, CommandOutcome::Denied));
    assert_eq!(r.approver.calls(), 1);
    let req = &r.approver.reqs()[0];
    assert!(matches!(req.reason, ApprovalReason::NeedsReview));
    assert_eq!(req.command, "Remove-Item x");
    assert_eq!(req.conversation_id, r.conv);
    assert!(uuid_like(&req.id));
    let ev = r.sink.events();
    assert!(ev.iter().any(
        |e| matches!(e, AppEvent::CommandDenied { command, .. } if command == "Remove-Item x")
    ));
    assert!(!ev
        .iter()
        .any(|e| matches!(e, AppEvent::CommandStarted { .. })));
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::Denied);
    assert_eq!(recs[0].command, "Remove-Item x");
}

fn uuid_like(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

#[tokio::test]
async fn ask_user_run_records_user() {
    let r = rig(
        CommandVerdict::AskUser,
        vec![Script::Answer(ApprovalResponse::Run)],
        T,
    );
    let out = r.svc.execute(&ctx(r.conv, 10), "Write-Output ok").await;
    assert!(matches!(out, CommandOutcome::Ran { .. }));
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::User);
}

#[tokio::test]
async fn edited_command_runs_edited_text() {
    let r = rig(
        CommandVerdict::AskUser,
        vec![Script::Answer(ApprovalResponse::RunEdited(
            "Write-Output ok".into(),
        ))],
        T,
    );
    let out = r.svc.execute(&ctx(r.conv, 10), "Remove-Item x").await;
    let CommandOutcome::Ran { result, .. } = out else {
        panic!("expected Ran");
    };
    assert!(result.output.contains("ok"));
    assert_eq!(r.approver.calls(), 1);
    assert_eq!(
        *r.checker.seen.lock().unwrap(),
        vec!["Remove-Item x".to_string(), "Write-Output ok".to_string()]
    );
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::UserEdited);
    assert_eq!(recs[0].command, "Write-Output ok");
}

#[tokio::test]
async fn empty_edit_is_deny() {
    let r = rig(
        CommandVerdict::AskUser,
        vec![Script::Answer(ApprovalResponse::RunEdited("   ".into()))],
        T,
    );
    let out = r.svc.execute(&ctx(r.conv, 10), "Remove-Item x").await;
    assert!(matches!(out, CommandOutcome::Denied));
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::Denied);
}

#[tokio::test]
async fn approver_timeout_denies() {
    let r = rig(
        CommandVerdict::AskUser,
        vec![Script::Never],
        Duration::from_millis(50),
    );
    let out = r.svc.execute(&ctx(r.conv, 10), "Write-Output ok").await;
    assert!(matches!(out, CommandOutcome::Denied));
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::Denied);
}

#[tokio::test]
async fn approver_dropped_denies() {
    let r = rig(CommandVerdict::AskUser, vec![Script::DroppedSender], T);
    let out = r.svc.execute(&ctx(r.conv, 10), "Write-Output ok").await;
    assert!(matches!(out, CommandOutcome::Denied));
    assert_eq!(
        r.store.commands(r.conv).unwrap()[0].approval,
        Approval::Denied
    );
}

#[tokio::test]
async fn limit_reached_asks_even_if_auto() {
    let r = rig(
        CommandVerdict::AutoRun,
        vec![Script::Answer(ApprovalResponse::Run)],
        T,
    );
    let c = ctx(r.conv, 2);
    for _ in 0..2 {
        assert!(matches!(
            r.svc.execute(&c, "Write-Output ok").await,
            CommandOutcome::Ran { .. }
        ));
    }
    assert_eq!(r.approver.calls(), 0);
    let out = r.svc.execute(&c, "Write-Output ok").await;
    assert!(matches!(out, CommandOutcome::Ran { .. }));
    let reqs = r.approver.reqs();
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        reqs[0].reason,
        ApprovalReason::LimitReached { count: 3 }
    ));
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 3);
    assert_eq!(recs[0].approval, Approval::Auto);
    assert_eq!(recs[1].approval, Approval::Auto);
    assert_eq!(recs[2].approval, Approval::User);
}

#[tokio::test]
async fn limit_deny_returns_denied() {
    let r = rig(
        CommandVerdict::AutoRun,
        vec![Script::Answer(ApprovalResponse::Deny)],
        T,
    );
    let c = ctx(r.conv, 0);
    let out = r.svc.execute(&c, "Write-Output ok").await;
    assert!(matches!(out, CommandOutcome::Denied));
    assert_eq!(*r.checker.seen.lock().unwrap(), vec!["Write-Output ok"]);
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::Denied);
    assert_eq!(recs[0].check["source"]["kind"], "jev");
}

#[tokio::test]
async fn limit_then_run_asks_once_with_real_check() {
    let r = rig(
        CommandVerdict::AskUser,
        vec![Script::Answer(ApprovalResponse::Run)],
        T,
    );
    let out = r.svc.execute(&ctx(r.conv, 0), "Write-Output ok").await;
    assert!(matches!(out, CommandOutcome::Ran { .. }));
    let reqs = r.approver.reqs();
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        reqs[0].reason,
        ApprovalReason::LimitReached { count: 1 }
    ));
    assert_eq!(reqs[0].check.verdict, CommandVerdict::AskUser);
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::User);
}

#[tokio::test]
async fn limit_request_carries_gate_source() {
    let r = rig(
        CommandVerdict::AutoRun,
        vec![Script::Answer(ApprovalResponse::Deny)],
        T,
    );
    r.svc.execute(&ctx(r.conv, 0), "Write-Output ok").await;
    let req = &r.approver.reqs()[0];
    assert_eq!(
        req.check.source,
        DecisionSource::Jev {
            model: "fake-jev".into()
        }
    );
    assert_eq!(req.check.verdict, CommandVerdict::AutoRun);
}

#[tokio::test]
async fn limit_edit_with_auto_gate_is_user_edited() {
    let r = rig(
        CommandVerdict::AutoRun,
        vec![Script::Answer(ApprovalResponse::RunEdited(
            "Write-Output edited".into(),
        ))],
        T,
    );
    let out = r.svc.execute(&ctx(r.conv, 0), "Write-Output orig").await;
    let CommandOutcome::Ran { result, .. } = out else {
        panic!("expected Ran");
    };
    assert!(result.output.contains("edited"));
    let reqs = r.approver.reqs();
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        reqs[0].reason,
        ApprovalReason::LimitReached { count: 1 }
    ));
    assert_eq!(reqs[0].command, "Write-Output orig");
    assert_eq!(reqs[0].check.verdict, CommandVerdict::AutoRun);
    assert_eq!(
        *r.checker.seen.lock().unwrap(),
        vec!["Write-Output orig", "Write-Output edited"]
    );
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::UserEdited);
    assert_eq!(recs[0].command, "Write-Output edited");
}

#[tokio::test]
async fn cancel_between_authorize_and_execute_never_starts() {
    let r = rig(CommandVerdict::AutoRun, vec![], T);
    let c = ctx(r.conv, 10);
    let auth = r.svc.authorize(&c, "Write-Output ok").await;
    assert!(matches!(auth, Authorization::Approved { .. }));
    c.cancel.cancel();
    let out = r.svc.execute(&c, "Write-Output ok").await;
    assert!(matches!(out, CommandOutcome::Denied));
    assert!(!r
        .sink
        .events()
        .iter()
        .any(|e| matches!(e, AppEvent::CommandStarted { .. })));
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::Denied);
}

#[tokio::test]
async fn edit_then_cancel_records_edited_text() {
    // Turn is cancelled while the gate re-checks the user's edit.
    struct CancelOnRecheck {
        calls: Mutex<u32>,
        cancel: CancellationToken,
    }
    #[async_trait]
    impl CommandChecker for CancelOnRecheck {
        async fn check(&self, command: &str, _cwd: &Path) -> CommandCheck {
            let mut n = self.calls.lock().unwrap();
            *n += 1;
            if *n == 2 {
                self.cancel.cancel();
            }
            CommandCheck {
                verdict: CommandVerdict::AskUser,
                is_safe: None,
                risk: Some(command.to_string()),
                risk_p: None,
                source: DecisionSource::Manual,
            }
        }
    }
    let store = Arc::new(Store::open_in_memory().unwrap());
    let conv = store
        .create_conversation("t", &std::env::temp_dir())
        .unwrap()
        .id;
    let c = ctx(conv, 10);
    let sink = Arc::new(VecSink::default());
    let svc = CommandService::new(
        Arc::new(CancelOnRecheck {
            calls: Mutex::new(0),
            cancel: c.cancel.clone(),
        }),
        ScriptedApprover::new(vec![Script::Answer(ApprovalResponse::RunEdited(
            "Write-Output edited".into(),
        ))]),
        store.clone(),
        sink.clone(),
        RunLimits::default(),
        T,
    );
    let out = svc.execute(&c, "Write-Output orig").await;
    assert!(matches!(out, CommandOutcome::Denied));
    let recs = store.commands(conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::Denied);
    assert_eq!(recs[0].command, "Write-Output edited");
    assert_eq!(recs[0].check["risk"], "Write-Output edited");
    assert!(!sink
        .events()
        .iter()
        .any(|e| matches!(e, AppEvent::CommandStarted { .. })));
}

#[tokio::test]
async fn cancelled_token_denies_without_asking() {
    let r = rig(CommandVerdict::AskUser, vec![], T);
    let c = ctx(r.conv, 10);
    c.cancel.cancel();
    let out = r.svc.execute(&c, "Write-Output ok").await;
    assert!(matches!(out, CommandOutcome::Denied));
    assert_eq!(r.approver.calls(), 0);
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].approval, Approval::Denied);
    // Even an AutoRun gate must not run after cancel.
    let r = rig(CommandVerdict::AutoRun, vec![], T);
    let c = ctx(r.conv, 10);
    c.cancel.cancel();
    assert!(matches!(
        r.svc.execute(&c, "Write-Output ok").await,
        CommandOutcome::Denied
    ));
}

#[tokio::test]
async fn cancel_during_wait_denies() {
    let r = rig(CommandVerdict::AskUser, vec![Script::Never], T);
    let c = ctx(r.conv, 10);
    let tok = c.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        tok.cancel();
    });
    let out = tokio::time::timeout(Duration::from_secs(5), r.svc.execute(&c, "Write-Output ok"))
        .await
        .expect("cancel must end the wait");
    assert!(matches!(out, CommandOutcome::Denied));
    assert_eq!(
        r.store.commands(r.conv).unwrap()[0].approval,
        Approval::Denied
    );
}

#[tokio::test]
async fn authorize_alone_records_only_denied() {
    let r = rig(CommandVerdict::AutoRun, vec![], T);
    let c = ctx(r.conv, 10);
    let auth = r.svc.authorize(&c, "Write-Output ok").await;
    assert!(matches!(
        auth,
        Authorization::Approved {
            approval: Approval::Auto,
            ..
        }
    ));
    assert!(r.store.commands(r.conv).unwrap().is_empty());
    let id = r.svc.record_external(&c, &auth).await.unwrap();
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].id, id);
    assert_eq!(recs[0].approval, Approval::Auto);
    assert_eq!(recs[0].exit_code, None);
    assert_eq!(recs[0].output, "(รันโดยโปรแกรมภายนอก)");
    assert!(recs[0].finished_at.is_some());
    assert!(r
        .svc
        .record_external(&c, &Authorization::Denied)
        .await
        .is_none());
}

#[tokio::test]
async fn missing_cwd_reports_error_without_panic() {
    let r = rig(CommandVerdict::AutoRun, vec![], T);
    let mut c = ctx(r.conv, 10);
    c.cwd = PathBuf::from(r"Z:\definitely\not\here\9f3a");
    let out = r.svc.execute(&c, "Write-Output ok").await;
    let CommandOutcome::Ran { result, .. } = out else {
        panic!("expected Ran");
    };
    assert_eq!(result.exit_code, None);
    assert!(result.output.contains("cwd not found"));
    let recs = r.store.commands(r.conv).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].exit_code, None);
    assert!(recs[0].output.contains("cwd not found"));
    assert!(r.sink.events().iter().any(|e| matches!(
        e,
        AppEvent::CommandFinished {
            exit_code: None,
            ..
        }
    )));
}

#[test]
fn serde_shapes() {
    let v = serde_json::to_value(ApprovalReason::LimitReached { count: 3 }).unwrap();
    assert_eq!(v, serde_json::json!({"kind":"limit_reached","count":3}));
    let v = serde_json::to_value(ApprovalResponse::RunEdited("x".into())).unwrap();
    let back: ApprovalResponse = serde_json::from_value(v).unwrap();
    assert!(matches!(back, ApprovalResponse::RunEdited(s) if s == "x"));
    let v = serde_json::to_value(AppEvent::CommandDenied {
        conversation_id: 1,
        command: "c".into(),
    })
    .unwrap();
    assert_eq!(v["kind"], "command_denied");
}
