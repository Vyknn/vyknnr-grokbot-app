//! Chat providers: each turns a `TurnRequest` into a stream of `ChatEvent`s.
//! Providers only propose commands; they never run them.

use crate::{Availability, ChatEvent, ProviderId, TurnRequest};
use async_trait::async_trait;

pub mod anthropic;

pub type Result<T> = std::result::Result<T, ProviderError>;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    Other(String),
}

#[async_trait]
pub trait ChatProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn availability(&self) -> Availability;
    async fn start(&self, req: TurnRequest) -> Result<Box<dyn ProviderTurn>>;
}

#[async_trait]
pub trait ProviderTurn: Send {
    async fn next(&mut self) -> Option<ChatEvent>;
    /// Only used by providers that emit `CommandProposed`; others return `Ok(())`.
    async fn submit_command_result(&mut self, call_id: &str, text: String) -> Result<()>;
    async fn cancel(&mut self);
}
