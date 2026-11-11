//! App-level turn bookkeeping: one running turn per conversation, a cancel
//! intent that exists before the orchestrator claims the turn, and a driver
//! that survives a panicking turn.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How often a pending cancel is re-sent until the turn ends (covers the
/// window before the orchestrator has registered the turn).
const CANCEL_RETRY: Duration = Duration::from_millis(50);

pub const INTERNAL_ERROR_TEXT: &str = "เกิดข้อผิดพลาดภายใน";

#[derive(Default)]
pub struct TurnRegistry {
    inner: Mutex<HashMap<i64, CancellationToken>>,
}

fn lock(
    m: &Mutex<HashMap<i64, CancellationToken>>,
) -> std::sync::MutexGuard<'_, HashMap<i64, CancellationToken>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl TurnRegistry {
    /// Claim `conversation_id`, or `None` if a turn is already running there.
    pub fn claim(self: &Arc<Self>, conversation_id: i64) -> Option<TurnSlot> {
        let mut m = lock(&self.inner);
        if m.contains_key(&conversation_id) {
            return None;
        }
        let token = CancellationToken::new();
        m.insert(conversation_id, token.clone());
        Some(TurnSlot {
            registry: self.clone(),
            conversation_id,
            token,
        })
    }

    /// Record the user's intent to stop. Honoured even before the
    /// orchestrator has started the turn.
    pub fn cancel(&self, conversation_id: i64) {
        if let Some(t) = lock(&self.inner).get(&conversation_id) {
            t.cancel();
        }
    }

    pub fn is_busy(&self, conversation_id: i64) -> bool {
        lock(&self.inner).contains_key(&conversation_id)
    }

    pub fn cancel_all(&self) {
        for t in lock(&self.inner).values() {
            t.cancel();
        }
    }
}

/// A claimed conversation. Frees it on drop.
pub struct TurnSlot {
    registry: Arc<TurnRegistry>,
    conversation_id: i64,
    token: CancellationToken,
}

impl Drop for TurnSlot {
    fn drop(&mut self) {
        lock(&self.registry.inner).remove(&self.conversation_id);
    }
}

#[derive(Debug)]
pub enum Driven<T> {
    Finished(T),
    /// The turn task panicked (or was aborted).
    Crashed,
}

/// Run `turn` as its own task and wait for it. While it runs, a cancel
/// recorded on the slot is forwarded with `cancel_inner` (and re-sent until
/// the turn ends, so a cancel that arrives before the orchestrator has
/// registered the turn still takes effect). A panic becomes `Crashed`.
pub async fn drive<T, F, C>(slot: TurnSlot, turn: F, cancel_inner: C) -> Driven<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
    C: Fn(),
{
    let mut handle = tokio::spawn(turn);
    let token = slot.token.clone();
    let result = tokio::select! {
        r = &mut handle => r,
        _ = token.cancelled() => loop {
            cancel_inner();
            tokio::select! {
                r = &mut handle => break r,
                _ = tokio::time::sleep(CANCEL_RETRY) => {}
            }
        },
    };
    drop(slot);
    match result {
        Ok(v) => Driven::Finished(v),
        Err(_) => Driven::Crashed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn claim_once_and_free_on_drop() {
        let r = Arc::new(TurnRegistry::default());
        let s = r.claim(7).unwrap();
        assert!(r.claim(7).is_none());
        assert!(r.is_busy(7));
        assert!(r.claim(8).is_some());
        drop(s);
        assert!(!r.is_busy(7));
        assert!(r.claim(7).is_some());
    }

    #[tokio::test]
    async fn panicking_turn_is_reported_and_frees_the_conversation() {
        let r = Arc::new(TurnRegistry::default());
        let slot = r.claim(1).unwrap();
        let out: Driven<u32> = drive(slot, async { panic!("boom") }, || {}).await;
        assert!(matches!(out, Driven::Crashed));
        assert!(!r.is_busy(1));
    }

    #[tokio::test]
    async fn finished_turn_returns_its_value() {
        let r = Arc::new(TurnRegistry::default());
        let slot = r.claim(1).unwrap();
        let out = drive(slot, async { 42 }, || {}).await;
        assert!(matches!(out, Driven::Finished(42)));
    }

    /// The turn "claims" only after a delay; cancel arrives before that.
    #[tokio::test]
    async fn cancel_before_the_turn_registers_is_still_honoured() {
        let r = Arc::new(TurnRegistry::default());
        let slot = r.claim(1).unwrap();
        let claimed = Arc::new(AtomicBool::new(false));
        let inner = CancellationToken::new();
        let (c2, i2) = (claimed.clone(), inner.clone());
        let turn = async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            c2.store(true, Ordering::SeqCst);
            tokio::select! {
                _ = i2.cancelled() => "cancelled",
                _ = tokio::time::sleep(Duration::from_secs(10)) => "ran to the end",
            }
        };
        // Like Orchestrator::cancel: a no-op until the turn is registered.
        let (c3, i3) = (claimed.clone(), inner.clone());
        let cancel_inner = move || {
            if c3.load(Ordering::SeqCst) {
                i3.cancel();
            }
        };
        r.cancel(1);
        let out = tokio::time::timeout(Duration::from_secs(3), drive(slot, turn, cancel_inner))
            .await
            .expect("cancel must end the turn");
        assert!(matches!(out, Driven::Finished("cancelled")));
    }
}
