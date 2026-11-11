use assistant_core::decision::gate::{CommandChecker, CommandGate};
use assistant_core::decision::policy::{CommandVerdict, DecisionSource, RouteOutcome, Thresholds};
use assistant_core::decision::router::{ProviderProfile, Router};
use assistant_core::jev::{JevAnswer, JevApi, JevError, JevQuestion, JevResponse};
use assistant_core::{Message, ProviderId, Role};
use async_trait::async_trait;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

type Qs = IndexMap<String, JevQuestion>;

enum Reply {
    Ok(JevResponse),
    Err(fn() -> JevError),
}

struct FakeJev {
    reply: Reply,
    calls: Mutex<usize>,
    last: Mutex<Option<(String, Qs)>>,
}

impl FakeJev {
    fn new(reply: Reply) -> Arc<Self> {
        Arc::new(Self {
            reply,
            calls: Mutex::new(0),
            last: Mutex::new(None),
        })
    }
    fn err(f: fn() -> JevError) -> Arc<Self> {
        Self::new(Reply::Err(f))
    }
    fn answers(a: Vec<(&str, JevAnswer)>) -> Arc<Self> {
        Self::new(Reply::Ok(JevResponse {
            model: "jev-test".into(),
            answers: a.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }))
    }
    fn ok_without(missing: &str) -> Arc<Self> {
        let mut a = vec![("is_safe", noul(0.99)), ("risk", choice("read_only", 0.99))];
        a.retain(|(k, _)| *k != missing);
        Self::answers(a)
    }
    fn ok_garbage_type(bad: &str) -> Arc<Self> {
        let mut a = vec![("is_safe", noul(0.99)), ("risk", choice("read_only", 0.99))];
        for (k, v) in a.iter_mut() {
            if *k == bad {
                *v = JevAnswer::Other;
            }
        }
        Self::answers(a)
    }
    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
    fn last_questions(&self) -> Qs {
        self.last.lock().unwrap().as_ref().unwrap().1.clone()
    }
    fn last_state(&self) -> String {
        self.last.lock().unwrap().as_ref().unwrap().0.clone()
    }
}

#[async_trait]
impl JevApi for FakeJev {
    async fn ask(&self, state: &str, questions: Qs) -> Result<JevResponse, JevError> {
        *self.calls.lock().unwrap() += 1;
        *self.last.lock().unwrap() = Some((state.to_string(), questions));
        match &self.reply {
            Reply::Ok(r) => Ok(r.clone()),
            Reply::Err(f) => Err(f()),
        }
    }
}

fn noul(x: f64) -> JevAnswer {
    JevAnswer::Noul { noul: x }
}
fn choice(c: &str, p: f64) -> JevAnswer {
    let mut probabilities = HashMap::new();
    probabilities.insert(c.to_string(), p);
    JevAnswer::Choice {
        choice: c.into(),
        probabilities,
        confidence: p,
    }
}

fn prof(id: ProviderId) -> ProviderProfile {
    ProviderProfile {
        id,
        description: format!("desc {}", id.as_str()),
    }
}

fn msg(role: Role, content: &str) -> Message {
    Message {
        id: 0,
        conversation_id: 1,
        role,
        provider: None,
        content: content.into(),
        created_at: chrono::Utc::now(),
    }
}

fn t() -> Thresholds {
    Thresholds::default()
}

fn dyn_jev(f: &Arc<FakeJev>) -> Option<Arc<dyn JevApi>> {
    Some(f.clone() as Arc<dyn JevApi>)
}

#[tokio::test]
async fn router_without_jev_asks_with_fallback_source() {
    let d = Router::new(None, t())
        .decide(
            "hi",
            &[],
            &[prof(ProviderId::Api), prof(ProviderId::Hermes)],
        )
        .await;
    assert!(matches!(d.outcome, RouteOutcome::AskUser { .. }));
    assert_eq!(
        d.source,
        DecisionSource::Fallback {
            reason: "jev_not_configured".into()
        }
    );
}

#[tokio::test]
async fn router_single_available_provider_skips_jev() {
    let f = FakeJev::err(|| JevError::Transport("x".into()));
    let d = Router::new(dyn_jev(&f), t())
        .decide("hi", &[], &[prof(ProviderId::Api)])
        .await;
    assert_eq!(d.outcome, RouteOutcome::Auto(ProviderId::Api));
    assert_eq!(d.source, DecisionSource::Manual);
    assert_eq!(d.probability, None);
    assert_eq!(f.calls(), 0);
}

#[tokio::test]
async fn router_empty_available_asks_empty_without_jev_call() {
    let f = FakeJev::err(|| JevError::Transport("x".into()));
    let d = Router::new(dyn_jev(&f), t()).decide("hi", &[], &[]).await;
    assert_eq!(d.outcome, RouteOutcome::AskUser { ranked: vec![] });
    assert_eq!(d.source, DecisionSource::Manual);
    assert_eq!(f.calls(), 0);
}

#[tokio::test]
async fn router_jev_error_falls_back_to_ask() {
    let f = FakeJev::err(|| JevError::Http(503, "down".into()));
    let d = Router::new(dyn_jev(&f), t())
        .decide(
            "hi",
            &[],
            &[prof(ProviderId::Api), prof(ProviderId::Hermes)],
        )
        .await;
    assert!(matches!(d.outcome, RouteOutcome::AskUser { .. }));
    match d.source {
        DecisionSource::Fallback { reason } => {
            assert_eq!(reason, "jev_error: jev http error 503: down")
        }
        o => panic!("{o:?}"),
    }
}

#[tokio::test]
async fn router_sends_only_available_providers_as_criteria() {
    let f = FakeJev::answers(vec![("route", choice("api", 0.9))]);
    let d = Router::new(dyn_jev(&f), t())
        .decide(
            "hi",
            &[],
            &[prof(ProviderId::Api), prof(ProviderId::Hermes)],
        )
        .await;
    assert_eq!(d.outcome, RouteOutcome::Auto(ProviderId::Api));
    assert_eq!(d.probability, Some(0.9));
    assert_eq!(
        d.source,
        DecisionSource::Jev {
            model: "jev-test".into()
        }
    );
    let qs = f.last_questions();
    match &qs["route"] {
        JevQuestion::Choice { criteria, .. } => {
            let mut keys: Vec<_> = criteria.keys().cloned().collect();
            keys.sort();
            assert_eq!(keys, ["api", "hermes"]);
            assert_eq!(criteria["api"], "desc api");
        }
        o => panic!("{o:?}"),
    }
}

#[tokio::test]
async fn router_missing_answer_falls_back() {
    let f = FakeJev::answers(vec![("route", noul(0.5))]);
    let d = Router::new(dyn_jev(&f), t())
        .decide(
            "hi",
            &[],
            &[prof(ProviderId::Api), prof(ProviderId::Hermes)],
        )
        .await;
    assert!(matches!(d.outcome, RouteOutcome::AskUser { .. }));
    assert!(matches!(d.source, DecisionSource::Fallback { .. }));
    assert_eq!(d.probability, None);
}

#[tokio::test]
async fn router_low_confidence_has_no_probability() {
    let f = FakeJev::answers(vec![("route", choice("api", 0.5))]);
    let d = Router::new(dyn_jev(&f), t())
        .decide(
            "hi",
            &[],
            &[prof(ProviderId::Api), prof(ProviderId::Hermes)],
        )
        .await;
    assert!(matches!(d.outcome, RouteOutcome::AskUser { .. }));
    assert!(matches!(d.source, DecisionSource::Jev { .. }));
    assert_eq!(d.probability, None);
}

#[tokio::test]
async fn router_state_uses_last_six_then_new_and_truncates() {
    let f = FakeJev::answers(vec![("route", choice("api", 0.9))]);
    let recent: Vec<Message> = (0..8).map(|i| msg(Role::User, &format!("m{i}"))).collect();
    Router::new(dyn_jev(&f), t())
        .decide(
            "NEW",
            &recent,
            &[prof(ProviderId::Api), prof(ProviderId::Hermes)],
        )
        .await;
    let s = f.last_state();
    assert!(!s.contains("m0") && !s.contains("m1"));
    assert!(s.contains("m2") && s.contains("m7"));
    assert!(s.ends_with("NEW"));
    assert!(s.find("m7").unwrap() < s.find("NEW").unwrap());

    let long = "é".repeat(5000);
    Router::new(dyn_jev(&f), t())
        .decide(
            &long,
            &[],
            &[prof(ProviderId::Api), prof(ProviderId::Hermes)],
        )
        .await;
    let s = f.last_state();
    assert_eq!(s.chars().count(), 4000);
    assert!(s.ends_with('é'));
}

#[tokio::test]
async fn gate_without_jev_asks() {
    let c = CommandGate::new(None, t())
        .check("dir", Path::new("C:\\"))
        .await;
    assert_eq!(c.verdict, CommandVerdict::AskUser);
    assert_eq!(
        c.source,
        DecisionSource::Fallback {
            reason: "jev_not_configured".into()
        }
    );
    assert!(c.is_safe.is_none() && c.risk.is_none() && c.risk_p.is_none());
}

#[tokio::test]
async fn gate_error_or_missing_answer_asks() {
    for fake in [
        FakeJev::err(|| JevError::Transport("x".into())),
        FakeJev::ok_without("risk"),
        FakeJev::ok_garbage_type("is_safe"),
    ] {
        let c = CommandGate::new(dyn_jev(&fake), t())
            .check("dir", Path::new("C:\\"))
            .await;
        assert_eq!(c.verdict, CommandVerdict::AskUser);
        assert!(matches!(c.source, DecisionSource::Fallback { .. }));
    }
}

#[tokio::test]
async fn gate_auto_runs_safe_read_only() {
    let f = FakeJev::answers(vec![
        ("is_safe", noul(0.97)),
        ("risk", choice("read_only", 0.95)),
    ]);
    let gate: Arc<dyn CommandChecker> = Arc::new(CommandGate::new(dyn_jev(&f), t()));
    let c = gate.check("dir", Path::new("C:\\")).await;
    assert_eq!(c.verdict, CommandVerdict::AutoRun);
    assert_eq!(
        c.source,
        DecisionSource::Jev {
            model: "jev-test".into()
        }
    );
    assert_eq!(c.is_safe, Some(0.97));
    assert_eq!(c.risk.as_deref(), Some("read_only"));
    assert_eq!(c.risk_p, Some(0.95));
    assert_eq!(f.last_state(), "cwd: C:\\\ncommand: dir");
    let qs = f.last_questions();
    match &qs["is_safe"] {
        JevQuestion::Noul { instructions } => assert_eq!(
            instructions,
            "คำสั่งนี้รันได้อย่างปลอดภัยโดยไม่ทำลายข้อมูล ไม่ส่งข้อมูลออกนอกเครื่อง และไม่เปลี่ยนการตั้งค่าระบบหรือไม่"
        ),
        o => panic!("{o:?}"),
    }
    match &qs["risk"] {
        JevQuestion::Choice { criteria, .. } => assert_eq!(criteria.len(), 6),
        o => panic!("{o:?}"),
    }
}

#[tokio::test]
async fn gate_non_read_only_asks_but_reports_jev_source() {
    let f = FakeJev::answers(vec![
        ("is_safe", noul(0.99)),
        ("risk", choice("destructive", 0.99)),
    ]);
    let c = CommandGate::new(dyn_jev(&f), t())
        .check("del x", Path::new("C:\\"))
        .await;
    assert_eq!(c.verdict, CommandVerdict::AskUser);
    assert!(matches!(c.source, DecisionSource::Jev { .. }));
    assert_eq!(c.risk.as_deref(), Some("destructive"));
}
