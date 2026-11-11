//! `Approver` that asks the user through the UI.
//!
//! Each request gets a `oneshot::Sender` stored by id and is shown to the UI.
//! The user's answer is routed by id. If the sender is dropped (conversation
//! deleted, window closed, request timed out) the request resolves to Deny.

use assistant_core::command_service::{ApprovalRequest, ApprovalResponse, Approver};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// Shows a request to the user (`app://approval`) or tells the UI one is gone.
pub trait ApprovalUi: Send + Sync {
    fn show(&self, req: &ApprovalRequest);
    /// The request was closed without an answer from the UI (timeout, cancel, dropped).
    fn closed(&self, id: &str);
}

struct Pending {
    conversation_id: i64,
    tx: oneshot::Sender<ApprovalResponse>,
}

type PendingMap = Arc<Mutex<HashMap<String, Pending>>>;

pub struct TauriApprover {
    pending: PendingMap,
    ui: Arc<dyn ApprovalUi>,
}

/// Removes the entry when the request future ends or is dropped.
struct Cleanup {
    pending: PendingMap,
    ui: Arc<dyn ApprovalUi>,
    id: String,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let removed = lock(&self.pending).remove(&self.id).is_some();
        if removed {
            self.ui.closed(&self.id);
        }
    }
}

fn lock(
    m: &Mutex<HashMap<String, Pending>>,
) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, PartialEq, Eq)]
pub struct UnknownApproval;

impl std::fmt::Display for UnknownApproval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ไม่พบคำขออนุมัตินี้ (อาจหมดเวลาหรือถูกยกเลิกแล้ว)")
    }
}

impl TauriApprover {
    pub fn new(ui: Arc<dyn ApprovalUi>) -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            ui,
        }
    }

    /// Deliver the user's answer to the request with `id`.
    pub fn answer(&self, id: &str, resp: ApprovalResponse) -> Result<(), UnknownApproval> {
        let p = lock(&self.pending).remove(id).ok_or(UnknownApproval)?;
        // The requester may have gone away in the meantime; that is a Deny anyway.
        let _ = p.tx.send(resp);
        Ok(())
    }

    /// Drop every pending request of one conversation (each becomes Deny).
    pub fn drop_conversation(&self, conversation_id: i64) {
        let ids: Vec<String> = {
            let mut m = lock(&self.pending);
            let ids: Vec<String> = m
                .iter()
                .filter(|(_, p)| p.conversation_id == conversation_id)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                m.remove(id);
            }
            ids
        };
        for id in ids {
            self.ui.closed(&id);
        }
    }

    /// Drop every pending request (each becomes Deny).
    pub fn drop_all(&self) {
        let ids: Vec<String> = lock(&self.pending).drain().map(|(id, _)| id).collect();
        for id in ids {
            self.ui.closed(&id);
        }
    }

    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        lock(&self.pending).len()
    }
}

#[async_trait]
impl Approver for TauriApprover {
    async fn request(&self, req: ApprovalRequest) -> ApprovalResponse {
        let (tx, rx) = oneshot::channel();
        let id = req.id.clone();
        lock(&self.pending).insert(
            id.clone(),
            Pending {
                conversation_id: req.conversation_id,
                tx,
            },
        );
        let _cleanup = Cleanup {
            pending: self.pending.clone(),
            ui: self.ui.clone(),
            id,
        };
        self.ui.show(&req);
        rx.await.unwrap_or(ApprovalResponse::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assistant_core::command_service::ApprovalReason;
    use assistant_core::decision::gate::CommandCheck;
    use assistant_core::decision::policy::{CommandVerdict, DecisionSource};
    use std::time::Duration;

    #[derive(Default)]
    struct RecordingUi {
        shown: Mutex<Vec<String>>,
        closed: Mutex<Vec<String>>,
    }

    impl ApprovalUi for RecordingUi {
        fn show(&self, req: &ApprovalRequest) {
            self.shown.lock().unwrap().push(req.id.clone());
        }
        fn closed(&self, id: &str) {
            self.closed.lock().unwrap().push(id.to_string());
        }
    }

    fn req(id: &str, conv: i64) -> ApprovalRequest {
        ApprovalRequest {
            id: id.into(),
            conversation_id: conv,
            command: "Remove-Item x".into(),
            cwd: "C:/tmp".into(),
            check: CommandCheck {
                verdict: CommandVerdict::AskUser,
                is_safe: None,
                risk: None,
                risk_p: None,
                source: DecisionSource::Manual,
            },
            reason: ApprovalReason::NeedsReview,
        }
    }

    async fn wait_shown(ui: &RecordingUi, n: usize) {
        for _ in 0..200 {
            if ui.shown.lock().unwrap().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("request not shown");
    }

    fn setup() -> (Arc<TauriApprover>, Arc<RecordingUi>) {
        let ui = Arc::new(RecordingUi::default());
        (Arc::new(TauriApprover::new(ui.clone())), ui)
    }

    #[tokio::test]
    async fn approver_drop_is_deny() {
        let (a, ui) = setup();
        let a2 = a.clone();
        let h = tokio::spawn(async move { a2.request(req("r1", 1)).await });
        wait_shown(&ui, 1).await;
        a.drop_all();
        let r = tokio::time::timeout(Duration::from_secs(2), h)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(r, ApprovalResponse::Deny));
        assert_eq!(a.pending_count(), 0);
        assert_eq!(*ui.closed.lock().unwrap(), vec!["r1".to_string()]);
    }

    #[tokio::test]
    async fn approver_answer_routes_by_id() {
        let (a, ui) = setup();
        let (a1, a2) = (a.clone(), a.clone());
        let h1 = tokio::spawn(async move { a1.request(req("id1", 1)).await });
        let h2 = tokio::spawn(async move { a2.request(req("id2", 1)).await });
        wait_shown(&ui, 2).await;
        a.answer("id2", ApprovalResponse::Run).unwrap();
        let r2 = tokio::time::timeout(Duration::from_secs(2), h2)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(r2, ApprovalResponse::Run));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!h1.is_finished(), "req1 must still be waiting");
        assert_eq!(a.pending_count(), 1);
        a.answer("id1", ApprovalResponse::RunEdited("dir".into()))
            .unwrap();
        let r1 = h1.await.unwrap();
        assert!(matches!(r1, ApprovalResponse::RunEdited(c) if c == "dir"));
    }

    #[tokio::test]
    async fn answer_to_unknown_id_is_error() {
        let (a, _ui) = setup();
        assert_eq!(
            a.answer("nope", ApprovalResponse::Run),
            Err(UnknownApproval)
        );
    }

    #[tokio::test]
    async fn dropping_a_conversation_denies_only_its_requests() {
        let (a, ui) = setup();
        let (a1, a2) = (a.clone(), a.clone());
        let h1 = tokio::spawn(async move { a1.request(req("c1", 1)).await });
        let h2 = tokio::spawn(async move { a2.request(req("c2", 2)).await });
        wait_shown(&ui, 2).await;
        a.drop_conversation(1);
        let r1 = tokio::time::timeout(Duration::from_secs(2), h1)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(r1, ApprovalResponse::Deny));
        assert!(!h2.is_finished());
        a.answer("c2", ApprovalResponse::Deny).unwrap();
        h2.await.unwrap();
    }

    #[tokio::test]
    async fn abandoned_request_is_removed_and_reported_closed() {
        let (a, ui) = setup();
        let r = tokio::time::timeout(Duration::from_millis(30), a.request(req("t1", 1))).await;
        assert!(r.is_err(), "nobody answered");
        assert_eq!(a.pending_count(), 0);
        assert_eq!(*ui.closed.lock().unwrap(), vec!["t1".to_string()]);
        assert!(a.answer("t1", ApprovalResponse::Run).is_err());
    }
}
