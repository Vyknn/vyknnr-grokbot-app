//! Bridges core events and approval prompts to the webview.

use crate::approver::ApprovalUi;
use assistant_core::command_service::{AppEvent, ApprovalRequest, EventSink};
use serde_json::json;
use tauri::{AppHandle, Emitter};

pub const EVENT: &str = "app://event";
pub const APPROVAL: &str = "app://approval";
pub const APPROVAL_CLOSED: &str = "app://approval-closed";
pub const TURN_OUTCOME: &str = "app://turn-outcome";

/// Emits every `AppEvent` as `app://event` (JSON tagged by `kind`).
pub struct TauriSink {
    app: AppHandle,
}

impl TauriSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for TauriSink {
    fn emit(&self, ev: AppEvent) {
        // A closed window cannot receive events; nothing else to do.
        let _ = self.app.emit(EVENT, ev);
    }
}

/// Shows approval prompts in the webview.
pub struct TauriApprovalUi {
    app: AppHandle,
}

impl TauriApprovalUi {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl ApprovalUi for TauriApprovalUi {
    fn show(&self, req: &ApprovalRequest) {
        let _ = self.app.emit(APPROVAL, req);
    }

    fn closed(&self, id: &str) {
        let _ = self.app.emit(APPROVAL_CLOSED, json!({ "id": id }));
    }
}
