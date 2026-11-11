//! Runs one chat turn: store the user message, pick a provider (Jev router or
//! the user's choice), stream the provider's reply, route every proposed
//! command through [`CommandService`], and store the results.
//!
//! The orchestrator owns the turn lifecycle: it builds the turn's
//! [`CommandContext`], registers it with the MCP server for the life of the
//! turn, and is the only place a turn is cancelled.

use crate::command_service::{AppEvent, CommandContext, CommandService, EventSink};
use crate::decision::policy::{DecisionSource, RouteOutcome};
use crate::decision::router::{ProviderProfile, Router};
use crate::mcp_server::{choose_cwd, McpServerHandle};
use crate::providers::{ChatProvider, ProviderTurn};
use crate::settings::Settings;
use crate::store::{Conversation, DecisionKind, Store, StoreError};
use crate::{Availability, ChatEvent, Message, NewMessage, ProviderId, Role, TurnRequest};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub const NO_PROVIDER_TEXT: &str = "ไม่มีตัวคุยที่ใช้งานได้";
pub const STOPPED_MID_STREAM_TEXT: &str = "ตัวคุยหยุดตอบกลางคัน";
pub const USER_STOPPED_TEXT: &str = "ผู้ใช้หยุดการตอบ";
const NOT_CONFIGURED_TEXT: &str = "ยังไม่ได้ตั้งค่าตัวคุยนี้";
const AVAILABILITY_TIMEOUT_TEXT: &str = "ตรวจสอบสถานะไม่ทันเวลา";
const AVAILABILITY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a running command may take to wind down after Stop.
const CANCEL_GRACE: Duration = Duration::from_secs(5);
const RECENT_FOR_ROUTER: usize = 6;
/// Stable provider order used for availability and unranked choices.
const ORDER: [ProviderId; 3] = [ProviderId::Hermes, ProviderId::ClaudeCli, ProviderId::Api];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "provider", rename_all = "snake_case")]
pub enum ProviderChoice {
    Auto,
    Manual(ProviderId),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed {
        provider: ProviderId,
    },
    NeedsProviderChoice {
        user_message_id: i64,
        ranked: Vec<(ProviderId, f64)>,
    },
    Failed {
        provider: ProviderId,
        error: String,
    },
    Cancelled {
        provider: ProviderId,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("a turn is already running in this conversation")]
    Busy,
    #[error("store: {0}")]
    Store(StoreError),
    #[error("not found")]
    NotFound,
}

impl From<StoreError> for OrchestratorError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => Self::NotFound,
            e => Self::Store(e),
        }
    }
}

pub type Result<T> = std::result::Result<T, OrchestratorError>;

type ProviderMap = HashMap<ProviderId, Arc<dyn ChatProvider>>;

pub struct Orchestrator {
    store: Arc<Store>,
    router: Arc<RwLock<Router>>,
    providers: RwLock<ProviderMap>,
    commands: Arc<CommandService>,
    mcp: McpServerHandle,
    sink: Arc<dyn EventSink>,
    settings: Arc<RwLock<Settings>>,
    active: Mutex<HashMap<i64, CancellationToken>>,
}

/// Marks a conversation busy; frees it on drop.
struct ActiveTurn<'a> {
    map: &'a Mutex<HashMap<i64, CancellationToken>>,
    conv_id: i64,
    cancel: CancellationToken,
}

impl Drop for ActiveTurn<'_> {
    fn drop(&mut self) {
        // `claim` refuses while an entry exists, so this entry is ours.
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.conv_id);
    }
}

/// How a streamed turn ended.
enum End {
    Done,
    Failed(String),
    Cancelled,
}

impl Orchestrator {
    pub fn new(
        store: Arc<Store>,
        router: Arc<RwLock<Router>>,
        providers: ProviderMap,
        commands: Arc<CommandService>,
        mcp: McpServerHandle,
        sink: Arc<dyn EventSink>,
        settings: Arc<RwLock<Settings>>,
    ) -> Self {
        Self {
            store,
            router,
            providers: RwLock::new(providers),
            commands,
            mcp,
            sink,
            settings,
            active: Mutex::new(HashMap::new()),
        }
    }

    /// Replace the providers (e.g. after a settings change). Running turns keep theirs.
    pub async fn set_providers(&self, providers: ProviderMap) {
        *self.providers.write().await = providers;
    }

    /// Replace the router (e.g. after a settings change).
    pub async fn set_router(&self, router: Router) {
        *self.router.write().await = router;
    }

    /// Stop the running turn in `conv_id`, if any.
    pub fn cancel(&self, conv_id: i64) {
        let m = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = m.get(&conv_id) {
            t.cancel();
        }
    }

    fn claim(&self, conv_id: i64) -> Result<ActiveTurn<'_>> {
        let mut m = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if m.contains_key(&conv_id) {
            return Err(OrchestratorError::Busy);
        }
        let cancel = CancellationToken::new();
        m.insert(conv_id, cancel.clone());
        Ok(ActiveTurn {
            map: &self.active,
            conv_id,
            cancel,
        })
    }

    pub async fn send(
        &self,
        conv_id: i64,
        text: String,
        choice: ProviderChoice,
    ) -> Result<TurnOutcome> {
        let active = self.claim(conv_id)?;
        let conv = self.store.conversation(conv_id)?;
        let user = self.store.append_message(
            conv_id,
            NewMessage {
                role: Role::User,
                provider: None,
                content: text.clone(),
            },
        )?;

        let (providers, ready) = self.ready_providers().await;
        let settings = self.settings.read().await.clone();
        if ready.is_empty() {
            let p = match choice {
                ProviderChoice::Manual(p) => p,
                ProviderChoice::Auto => settings.default_provider.unwrap_or(ProviderId::Api),
            };
            return Ok(self.fail(conv_id, p, NO_PROVIDER_TEXT.into()));
        }

        let chosen = match choice {
            ProviderChoice::Manual(p) => p,
            ProviderChoice::Auto if ready.len() == 1 => ready[0],
            ProviderChoice::Auto if !settings.auto_route => {
                match settings.default_provider.filter(|p| ready.contains(p)) {
                    Some(p) => p,
                    None => {
                        return Ok(TurnOutcome::NeedsProviderChoice {
                            user_message_id: user.id,
                            ranked: ready.iter().map(|&p| (p, 0.0)).collect(),
                        })
                    }
                }
            }
            ProviderChoice::Auto => {
                let all = self.store.messages(conv_id)?;
                let before = &all[..all.len().saturating_sub(1)];
                let recent = &before[before.len().saturating_sub(RECENT_FOR_ROUTER)..];
                let profiles: Vec<ProviderProfile> = ready
                    .iter()
                    .map(|&id| ProviderProfile {
                        id,
                        description: settings.description(id).to_string(),
                    })
                    .collect();
                let d = self
                    .router
                    .read()
                    .await
                    .decide(&text, recent, &profiles)
                    .await;
                self.store.record_decision(
                    user.id,
                    DecisionKind::Route,
                    &json!({
                        "outcome": d.outcome,
                        "source": d.source,
                        "probability": d.probability,
                    }),
                )?;
                match d.outcome {
                    RouteOutcome::Auto(p) => {
                        self.sink.emit(AppEvent::RouteDecided {
                            conversation_id: conv_id,
                            provider: p,
                            source: d.source,
                            probability: d.probability,
                        });
                        p
                    }
                    RouteOutcome::AskUser { ranked } => {
                        return Ok(TurnOutcome::NeedsProviderChoice {
                            user_message_id: user.id,
                            ranked,
                        })
                    }
                }
            }
        };
        self.run_chosen(&conv, chosen, &providers, &active).await
    }

    /// Run a turn for an already-stored user message after the user picked a provider.
    pub async fn continue_with(
        &self,
        conv_id: i64,
        user_message_id: i64,
        provider: ProviderId,
    ) -> Result<TurnOutcome> {
        let active = self.claim(conv_id)?;
        let conv = self.store.conversation(conv_id)?;
        let msgs = self.store.messages(conv_id)?;
        let latest_user = msgs.iter().rev().find(|m| m.role == Role::User);
        if latest_user.map(|m| m.id) != Some(user_message_id) {
            return Err(OrchestratorError::NotFound);
        }
        self.store.record_decision(
            user_message_id,
            DecisionKind::Route,
            &json!({
                "outcome": RouteOutcome::Auto(provider),
                "source": DecisionSource::Manual,
            }),
        )?;
        let (providers, ready) = self.ready_providers().await;
        if ready.is_empty() {
            return Ok(self.fail(conv_id, provider, NO_PROVIDER_TEXT.into()));
        }
        self.run_chosen(&conv, provider, &providers, &active).await
    }

    /// Check every provider concurrently; anything slower than the timeout is Unavailable.
    async fn ready_providers(
        &self,
    ) -> (
        HashMap<ProviderId, (Arc<dyn ChatProvider>, Availability)>,
        Vec<ProviderId>,
    ) {
        let providers: Vec<(ProviderId, Arc<dyn ChatProvider>)> = {
            let map = self.providers.read().await;
            ORDER
                .iter()
                .filter_map(|id| map.get(id).map(|p| (*id, p.clone())))
                .collect()
        };
        let checks = providers.into_iter().map(|(id, p)| async move {
            let a = tokio::time::timeout(AVAILABILITY_TIMEOUT, p.availability())
                .await
                .unwrap_or_else(|_| Availability::Unavailable(AVAILABILITY_TIMEOUT_TEXT.into()));
            (id, p, a)
        });
        let results = futures_util::future::join_all(checks).await;
        let ready = results
            .iter()
            .filter(|(_, _, a)| *a == Availability::Ready)
            .map(|(id, _, _)| *id)
            .collect();
        let map = results.into_iter().map(|(id, p, a)| (id, (p, a))).collect();
        (map, ready)
    }

    async fn run_chosen(
        &self,
        conv: &Conversation,
        chosen: ProviderId,
        providers: &HashMap<ProviderId, (Arc<dyn ChatProvider>, Availability)>,
        active: &ActiveTurn<'_>,
    ) -> Result<TurnOutcome> {
        let provider = match providers.get(&chosen) {
            Some((p, Availability::Ready)) => p.clone(),
            Some((_, Availability::Unavailable(why))) => {
                return self.fail_with_notice(conv.id, chosen, why.clone(), "")
            }
            None => return self.fail_with_notice(conv.id, chosen, NOT_CONFIGURED_TEXT.into(), ""),
        };
        self.run_turn(conv, chosen, provider, active.cancel.clone())
            .await
    }

    /// Emit `TurnFailed` without storing anything.
    fn fail(&self, conv_id: i64, provider: ProviderId, error: String) -> TurnOutcome {
        self.sink.emit(AppEvent::TurnFailed {
            conversation_id: conv_id,
            provider,
            error: error.clone(),
        });
        TurnOutcome::Failed { provider, error }
    }

    /// Store partial text (if any) and a Notice, then fail.
    fn fail_with_notice(
        &self,
        conv_id: i64,
        provider: ProviderId,
        error: String,
        partial: &str,
    ) -> Result<TurnOutcome> {
        self.store_assistant(conv_id, provider, partial)?;
        self.store.append_message(
            conv_id,
            NewMessage {
                role: Role::Notice,
                provider: Some(provider),
                content: error.clone(),
            },
        )?;
        Ok(self.fail(conv_id, provider, error))
    }

    fn store_assistant(&self, conv_id: i64, provider: ProviderId, text: &str) -> Result<()> {
        if !text.trim().is_empty() {
            self.store.append_message(
                conv_id,
                NewMessage {
                    role: Role::Assistant,
                    provider: Some(provider),
                    content: text.to_string(),
                },
            )?;
        }
        Ok(())
    }

    async fn run_turn(
        &self,
        conv: &Conversation,
        provider_id: ProviderId,
        provider: Arc<dyn ChatProvider>,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome> {
        let max_per_turn = self.settings.read().await.max_commands_per_turn;
        let ctx = CommandContext {
            conversation_id: conv.id,
            turn_token: uuid::Uuid::new_v4().to_string(),
            cwd: conv.cwd.clone(),
            provider: provider_id,
            count: Arc::new(AtomicU32::new(0)),
            max_per_turn,
            cancel,
        };
        let _registration = self.mcp.register_turn(ctx.clone());
        let history: Vec<Message> = self.store.messages(conv.id)?;
        let req = TurnRequest {
            conversation_id: conv.id,
            turn_token: ctx.turn_token.clone(),
            history,
            cwd: ctx.cwd.clone(),
        };
        let mut turn = match provider.start(req).await {
            Ok(t) => t,
            Err(e) => return self.fail_with_notice(conv.id, provider_id, e.to_string(), ""),
        };

        let mut text = String::new();
        let end = self.stream(&ctx, turn.as_mut(), &mut text).await?;
        match end {
            End::Done => {
                self.store_assistant(conv.id, provider_id, &text)?;
                self.sink.emit(AppEvent::TurnFinished {
                    conversation_id: conv.id,
                });
                Ok(TurnOutcome::Completed {
                    provider: provider_id,
                })
            }
            End::Failed(e) => {
                turn.cancel().await;
                self.fail_with_notice(conv.id, provider_id, e, &text)
            }
            End::Cancelled => {
                turn.cancel().await;
                self.store_assistant(conv.id, provider_id, &text)?;
                self.store.append_message(
                    conv.id,
                    NewMessage {
                        role: Role::Notice,
                        provider: Some(provider_id),
                        content: USER_STOPPED_TEXT.into(),
                    },
                )?;
                self.sink.emit(AppEvent::TurnFinished {
                    conversation_id: conv.id,
                });
                Ok(TurnOutcome::Cancelled {
                    provider: provider_id,
                })
            }
        }
    }

    async fn stream(
        &self,
        ctx: &CommandContext,
        turn: &mut dyn ProviderTurn,
        text: &mut String,
    ) -> Result<End> {
        loop {
            let ev = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => return Ok(End::Cancelled),
                ev = turn.next() => ev,
            };
            match ev {
                None => return Ok(End::Failed(STOPPED_MID_STREAM_TEXT.into())),
                Some(ChatEvent::Done) => return Ok(End::Done),
                Some(ChatEvent::Error(e)) => return Ok(End::Failed(e)),
                Some(ChatEvent::TextDelta(t)) => {
                    self.sink.emit(AppEvent::AssistantDelta {
                        conversation_id: ctx.conversation_id,
                        text: t.clone(),
                    });
                    text.push_str(&t);
                }
                Some(ChatEvent::CommandProposed {
                    call_id,
                    command,
                    cwd,
                }) => {
                    let Some(result_text) = self.run_command(ctx, &command, cwd).await? else {
                        return Ok(End::Cancelled);
                    };
                    if let Err(e) = turn.submit_command_result(&call_id, result_text).await {
                        return Ok(End::Failed(e.to_string()));
                    }
                }
            }
        }
    }

    /// Run one proposed command and store its result. Returns the text for
    /// the model, or `None` if the turn was cancelled while it ran.
    async fn run_command(
        &self,
        ctx: &CommandContext,
        command: &str,
        cwd: Option<std::path::PathBuf>,
    ) -> Result<Option<String>> {
        let mut cctx = ctx.clone();
        cctx.cwd = choose_cwd(&ctx.cwd, cwd.as_deref().and_then(|p| p.to_str()));
        let fut = self.commands.execute(&cctx, command);
        tokio::pin!(fut);
        let outcome = tokio::select! {
            biased;
            o = &mut fut => o,
            _ = ctx.cancel.cancelled() => {
                // The service sees the same token; let it record and clean up.
                let _ = tokio::time::timeout(CANCEL_GRACE, fut).await;
                return Ok(None);
            }
        };
        let for_model_text = self.commands.add_result_to_chat(&cctx, command, &outcome)?;
        Ok(Some(for_model_text))
    }
}
