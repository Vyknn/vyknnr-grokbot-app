//! The single authorization and execution point for every shell command.
//!
//! Rule: when in doubt, never run. Any failure to get a clear answer from the
//! user (timeout, dropped channel, cancel) is treated as Deny.

use crate::decision::gate::{CommandCheck, CommandChecker};
use crate::decision::policy::{CommandVerdict, DecisionSource};
use crate::runner::{run_powershell, RunChunk, RunLimits, RunResult};
use crate::store::{Approval, Store};
use crate::ProviderId;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
const EXTERNAL_OUTPUT: &str = "(รันโดยโปรแกรมภายนอก)";

#[derive(Debug, Clone)]
pub struct CommandContext {
    pub conversation_id: i64,
    pub turn_token: String,
    pub cwd: PathBuf,
    pub provider: ProviderId,
    pub count: Arc<AtomicU32>,
    pub max_per_turn: u32,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalReason {
    NeedsReview,
    LimitReached { count: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub conversation_id: i64,
    pub command: String,
    pub cwd: PathBuf,
    pub check: CommandCheck,
    pub reason: ApprovalReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "command", rename_all = "snake_case")]
pub enum ApprovalResponse {
    Run,
    RunEdited(String),
    Deny,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn request(&self, req: ApprovalRequest) -> ApprovalResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppEvent {
    CommandStarted {
        conversation_id: i64,
        command_id: i64,
        command: String,
        cwd: PathBuf,
        approval: Approval,
    },
    CommandOutput {
        command_id: i64,
        chunk: RunChunk,
    },
    CommandFinished {
        command_id: i64,
        exit_code: Option<i32>,
        timed_out: bool,
    },
    CommandDenied {
        conversation_id: i64,
        command: String,
    },
    RouteDecided {
        conversation_id: i64,
        provider: ProviderId,
        source: DecisionSource,
        probability: Option<f64>,
    },
    AssistantDelta {
        conversation_id: i64,
        text: String,
    },
    TurnFinished {
        conversation_id: i64,
    },
    TurnFailed {
        conversation_id: i64,
        provider: ProviderId,
        error: String,
    },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, ev: AppEvent);
}

#[derive(Debug, Clone)]
pub enum Authorization {
    Approved {
        command: String,
        approval: Approval,
        check: CommandCheck,
    },
    Denied,
}

#[derive(Debug, Clone)]
pub enum CommandOutcome {
    Ran { command_id: i64, result: RunResult },
    Denied,
}

pub struct CommandService {
    gate: Arc<dyn CommandChecker>,
    approver: Arc<dyn Approver>,
    store: Arc<Store>,
    sink: Arc<dyn EventSink>,
    limits: RunLimits,
    approval_timeout: Duration,
}

const CANCELLED_BEFORE_RUN: &str = "(ยกเลิกก่อนเริ่มรัน)";

impl CommandService {
    pub fn new(
        gate: Arc<dyn CommandChecker>,
        approver: Arc<dyn Approver>,
        store: Arc<Store>,
        sink: Arc<dyn EventSink>,
        limits: RunLimits,
        approval_timeout: Duration,
    ) -> Self {
        Self {
            gate,
            approver,
            store,
            sink,
            limits,
            approval_timeout,
        }
    }

    /// Output limits used when running commands.
    pub fn limits(&self) -> &RunLimits {
        &self.limits
    }

    /// Ask the user. Cancel, timeout or a dropped approver all mean Deny.
    async fn ask(
        &self,
        ctx: &CommandContext,
        command: &str,
        check: &CommandCheck,
        reason: ApprovalReason,
    ) -> ApprovalResponse {
        if ctx.cancel.is_cancelled() {
            return ApprovalResponse::Deny;
        }
        let req = ApprovalRequest {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: ctx.conversation_id,
            command: command.to_string(),
            cwd: ctx.cwd.clone(),
            check: check.clone(),
            reason,
        };
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => ApprovalResponse::Deny,
            r = tokio::time::timeout(self.approval_timeout, self.approver.request(req)) => {
                r.unwrap_or(ApprovalResponse::Deny)
            }
        }
    }

    /// Record a denied command and emit `CommandDenied`. Returns the record id.
    fn record_denied(
        &self,
        ctx: &CommandContext,
        command: &str,
        check: Option<&CommandCheck>,
    ) -> Option<i64> {
        let check_json = check
            .and_then(|c| serde_json::to_value(c).ok())
            .unwrap_or(serde_json::Value::Null);
        // Recording failure must not turn a Deny into anything else.
        let id = self.store.insert_command(
            ctx.conversation_id,
            command,
            &ctx.cwd,
            &check_json,
            Approval::Denied,
        );
        self.sink.emit(AppEvent::CommandDenied {
            conversation_id: ctx.conversation_id,
            command: command.to_string(),
        });
        id.ok()
    }

    fn deny(
        &self,
        ctx: &CommandContext,
        command: &str,
        check: Option<&CommandCheck>,
    ) -> Authorization {
        self.record_denied(ctx, command, check);
        Authorization::Denied
    }

    /// Turn the user's answer into an authorization. `Run` approves `command`
    /// with `check` as `Approval::User`; a non-blank edit is re-checked by the
    /// gate and approved as `Approval::UserEdited` without asking again.
    /// Anything else (deny, blank edit, timeout, cancel) is Denied.
    async fn resolve(
        &self,
        ctx: &CommandContext,
        command: String,
        check: CommandCheck,
        resp: ApprovalResponse,
    ) -> Authorization {
        let (command, approval, check) = match resp {
            ApprovalResponse::Run => (command, Approval::User, check),
            ApprovalResponse::RunEdited(edited) if !edited.trim().is_empty() => {
                let new_check = self.gate.check(&edited, &ctx.cwd).await;
                (edited, Approval::UserEdited, new_check)
            }
            _ => return self.deny(ctx, &command, Some(&check)),
        };
        if ctx.cancel.is_cancelled() {
            return self.deny(ctx, &command, Some(&check));
        }
        Authorization::Approved {
            command,
            approval,
            check,
        }
    }

    /// Decide whether `command` may run.
    ///
    /// Recording: a `Denied` outcome is written to the store (with
    /// `Approval::Denied`) and emits `CommandDenied` here. An `Approved`
    /// outcome is NOT written here; the caller must record it, either via
    /// [`CommandService::execute`] (which runs it) or
    /// [`CommandService::record_external`] (when something else runs it).
    pub async fn authorize(&self, ctx: &CommandContext, command: &str) -> Authorization {
        if ctx.cancel.is_cancelled() {
            return self.deny(ctx, command, None);
        }
        let count = ctx.count.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        let check = self.gate.check(command, &ctx.cwd).await;
        if ctx.cancel.is_cancelled() {
            return self.deny(ctx, command, Some(&check));
        }

        // Over the per-turn limit: one question carrying the real check,
        // even if the gate would auto-run it.
        let reason = if count > ctx.max_per_turn {
            ApprovalReason::LimitReached { count }
        } else if check.verdict == CommandVerdict::AutoRun {
            return Authorization::Approved {
                command: command.to_string(),
                approval: Approval::Auto,
                check,
            };
        } else {
            ApprovalReason::NeedsReview
        };
        let resp = self.ask(ctx, command, &check, reason).await;
        self.resolve(ctx, command.to_string(), check, resp).await
    }

    /// Authorize, run with PowerShell, and record the result.
    pub async fn execute(&self, ctx: &CommandContext, command: &str) -> CommandOutcome {
        let (command, approval, check) = match self.authorize(ctx, command).await {
            Authorization::Approved {
                command,
                approval,
                check,
            } => (command, approval, check),
            Authorization::Denied => return CommandOutcome::Denied,
        };
        // Stop pressed after approval but before the run: never start it.
        if ctx.cancel.is_cancelled() {
            if let Some(id) = self.record_denied(ctx, &command, Some(&check)) {
                let _ = self
                    .store
                    .finish_command(id, None, CANCELLED_BEFORE_RUN, false);
            }
            return CommandOutcome::Denied;
        }
        let check_json = serde_json::to_value(&check).unwrap_or(serde_json::Value::Null);
        // No audit record means no run.
        let Ok(command_id) = self.store.insert_command(
            ctx.conversation_id,
            &command,
            &ctx.cwd,
            &check_json,
            approval,
        ) else {
            self.sink.emit(AppEvent::CommandDenied {
                conversation_id: ctx.conversation_id,
                command,
            });
            return CommandOutcome::Denied;
        };
        self.sink.emit(AppEvent::CommandStarted {
            conversation_id: ctx.conversation_id,
            command_id,
            command: command.clone(),
            cwd: ctx.cwd.clone(),
            approval,
        });

        let sink = self.sink.clone();
        let on_chunk = move |chunk: RunChunk| {
            sink.emit(AppEvent::CommandOutput { command_id, chunk });
        };
        let result = match run_powershell(
            &command,
            &ctx.cwd,
            &self.limits,
            on_chunk,
            ctx.cancel.clone(),
        )
        .await
        {
            Ok(r) => r,
            Err(e) => RunResult {
                exit_code: None,
                output: e.to_string(),
                truncated: false,
                timed_out: false,
                cancelled: false,
            },
        };
        let _ = self.store.finish_command(
            command_id,
            result.exit_code,
            &result.output,
            result.truncated,
        );
        self.sink.emit(AppEvent::CommandFinished {
            command_id,
            exit_code: result.exit_code,
            timed_out: result.timed_out,
        });
        CommandOutcome::Ran { command_id, result }
    }

    /// Record an approved command that is run by an external program (e.g. Hermes).
    /// Returns `None` for `Denied` (already recorded by `authorize`) or on store error.
    pub async fn record_external(&self, ctx: &CommandContext, auth: &Authorization) -> Option<i64> {
        let Authorization::Approved {
            command,
            approval,
            check,
        } = auth
        else {
            return None;
        };
        let check_json = serde_json::to_value(check).unwrap_or(serde_json::Value::Null);
        let id = self
            .store
            .insert_command(
                ctx.conversation_id,
                command,
                &ctx.cwd,
                &check_json,
                *approval,
            )
            .ok()?;
        self.store
            .finish_command(id, None, EXTERNAL_OUTPUT, false)
            .ok()?;
        Some(id)
    }
}
