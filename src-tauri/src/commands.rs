//! Tauri commands. Secrets never leave the backend: there is no get_secret.

use crate::app_state::AppState;
use crate::parse;
use crate::sink::TURN_OUTCOME;
use crate::turns::{drive, Driven, INTERNAL_ERROR_TEXT};
use assistant_core::orchestrator::{OrchestratorError, ProviderChoice, TurnOutcome};
use assistant_core::settings::{SecretKey, Settings};
use assistant_core::store::Conversation;
use assistant_core::{Availability, Message, ProviderId};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;

pub type CmdResult<T> = Result<T, String>;

const BUSY_TEXT: &str = "แชตนี้กำลังทำงานอยู่";
const STATUS_TIMEOUT: Duration = Duration::from_secs(8);

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnOutcomeEvent {
    pub conversation_id: i64,
    pub outcome: Option<TurnOutcome>,
    pub error: Option<String>,
}

fn emit_outcome(
    app: &AppHandle,
    conversation_id: i64,
    r: Driven<Result<TurnOutcome, OrchestratorError>>,
) {
    let r = match r {
        Driven::Finished(r) => r,
        Driven::Crashed => {
            let _ = app.emit(
                TURN_OUTCOME,
                TurnOutcomeEvent {
                    conversation_id,
                    outcome: None,
                    error: Some(INTERNAL_ERROR_TEXT.to_string()),
                },
            );
            return;
        }
    };
    let ev = match r {
        Ok(o) => TurnOutcomeEvent {
            conversation_id,
            outcome: Some(o),
            error: None,
        },
        Err(e) => TurnOutcomeEvent {
            conversation_id,
            outcome: None,
            error: Some(match e {
                OrchestratorError::Busy => BUSY_TEXT.to_string(),
                OrchestratorError::NotFound => "ไม่พบแชตหรือข้อความนี้".to_string(),
                e => e.to_string(),
            }),
        },
    };
    let _ = app.emit(TURN_OUTCOME, ev);
}

#[tauri::command]
pub fn list_conversations(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<Conversation>> {
    state.store.list_conversations().map_err(err)
}

#[tauri::command]
pub fn search_conversations(
    state: State<'_, Arc<AppState>>,
    q: String,
) -> CmdResult<Vec<Conversation>> {
    if q.trim().is_empty() {
        return state.store.list_conversations().map_err(err);
    }
    state.store.search(q.trim()).map_err(err)
}

#[tauri::command]
pub fn create_conversation(
    state: State<'_, Arc<AppState>>,
    title: String,
    cwd: String,
) -> CmdResult<Conversation> {
    let dir = PathBuf::from(cwd.trim());
    if !dir.is_absolute() || !dir.is_dir() {
        return Err("โฟลเดอร์นี้ไม่มีอยู่จริง".into());
    }
    let title = if title.trim().is_empty() {
        "แชตใหม่"
    } else {
        title.trim()
    };
    state.store.create_conversation(title, &dir).map_err(err)
}

#[tauri::command]
pub fn rename_conversation(
    state: State<'_, Arc<AppState>>,
    id: i64,
    title: String,
) -> CmdResult<()> {
    if title.trim().is_empty() {
        return Err("ชื่อแชตว่างไม่ได้".into());
    }
    state
        .store
        .rename_conversation(id, title.trim())
        .map_err(err)
}

#[tauri::command]
pub fn delete_conversation(state: State<'_, Arc<AppState>>, id: i64) -> CmdResult<()> {
    // Stop its turn and deny its pending approvals first.
    state.turns.cancel(id);
    state.orchestrator.cancel(id);
    state.approver.drop_conversation(id);
    state.store.delete_conversation(id).map_err(err)
}

#[tauri::command]
pub fn get_messages(state: State<'_, Arc<AppState>>, id: i64) -> CmdResult<Vec<Message>> {
    state.store.messages(id).map_err(err)
}

#[derive(Serialize)]
pub struct ConversationDetail {
    pub messages: Vec<Message>,
    pub commands: Vec<assistant_core::store::CommandRecord>,
    pub decisions: Vec<assistant_core::store::DecisionRecord>,
    pub busy: bool,
}

/// Messages plus command records, for rendering a stored conversation.
#[tauri::command]
pub fn get_conversation_detail(
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> CmdResult<ConversationDetail> {
    let busy = state.turns.is_busy(id);
    Ok(ConversationDetail {
        messages: state.store.messages(id).map_err(err)?,
        commands: state.store.commands(id).map_err(err)?,
        decisions: state.store.decisions(id).map_err(err)?,
        busy,
    })
}

/// Starts the turn in the background and returns at once (outcome arrives
/// as `app://turn-outcome`).
#[tauri::command]
pub fn send_message(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    conversation_id: i64,
    text: String,
    provider: Option<String>,
) -> CmdResult<()> {
    if text.trim().is_empty() {
        return Err("ข้อความว่าง".into());
    }
    let choice: ProviderChoice = parse::provider_choice(provider.as_deref())?;
    state.store.conversation(conversation_id).map_err(err)?;
    let slot = state.turns.claim(conversation_id).ok_or(BUSY_TEXT)?;
    let orch = state.orchestrator.clone();
    tauri::async_runtime::spawn(async move {
        let o2 = orch.clone();
        let turn = async move { o2.send(conversation_id, text, choice).await };
        let r = drive(slot, turn, || orch.cancel(conversation_id)).await;
        emit_outcome(&app, conversation_id, r);
    });
    Ok(())
}

#[tauri::command]
pub fn continue_with(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    conversation_id: i64,
    user_message_id: i64,
    provider: String,
) -> CmdResult<()> {
    let provider: ProviderId = provider.parse()?;
    state.store.conversation(conversation_id).map_err(err)?;
    let slot = state.turns.claim(conversation_id).ok_or(BUSY_TEXT)?;
    let orch = state.orchestrator.clone();
    tauri::async_runtime::spawn(async move {
        let o2 = orch.clone();
        let turn = async move {
            o2.continue_with(conversation_id, user_message_id, provider)
                .await
        };
        let r = drive(slot, turn, || orch.cancel(conversation_id)).await;
        emit_outcome(&app, conversation_id, r);
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_turn(state: State<'_, Arc<AppState>>, conversation_id: i64) {
    // Recorded at the app layer too, so a Stop before the orchestrator has
    // claimed the turn is still honoured.
    state.turns.cancel(conversation_id);
    state.orchestrator.cancel(conversation_id);
}

#[tauri::command]
pub fn answer_approval(
    state: State<'_, Arc<AppState>>,
    id: String,
    response: parse::UiApproval,
) -> CmdResult<()> {
    let resp = parse::approval_response(response)?;
    state.approver.answer(&id, resp).map_err(err)
}

#[tauri::command]
pub async fn get_settings(state: State<'_, Arc<AppState>>) -> CmdResult<Settings> {
    Ok(state.settings.read().await.clone())
}

#[tauri::command]
pub async fn save_settings(state: State<'_, Arc<AppState>>, settings: Settings) -> CmdResult<()> {
    settings.validate()?;
    let _lock = state.save_lock.lock().await;
    settings.save(&state.settings_path).map_err(err)?;
    *state.settings.write().await = settings;
    state.rebuild().await;
    Ok(())
}

/// Store (or clear, with an empty value) a key. All four names are
/// recognised; the MCP key is app-generated and cannot be changed here.
#[tauri::command]
pub async fn set_secret(
    state: State<'_, Arc<AppState>>,
    key: String,
    value: String,
) -> CmdResult<()> {
    let k = parse::writable_secret_key(&key)?;
    let _lock = state.save_lock.lock().await;
    let value = value.trim();
    // Error text from the keyring never contains the value.
    state.secrets.set(k, value)?;
    state.rebuild().await;
    Ok(())
}

#[tauri::command]
pub fn has_secret(state: State<'_, Arc<AppState>>, key: String) -> CmdResult<bool> {
    let k = parse::secret_key(&key)?;
    Ok(state.secrets.get(k).is_some_and(|v| !v.is_empty()))
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub hermes: Availability,
    pub claude_cli: Availability,
    pub api: Availability,
    /// Startup problems (e.g. MCP port in use).
    pub notices: Vec<String>,
}

#[tauri::command]
pub async fn provider_status(state: State<'_, Arc<AppState>>) -> CmdResult<ProviderStatus> {
    let providers = state.providers.read().await.clone();
    let check = |id: ProviderId, missing: &'static str| {
        let p = providers.get(&id).cloned();
        async move {
            match p {
                None => Availability::Unavailable(missing.into()),
                Some(p) => tokio::time::timeout(STATUS_TIMEOUT, p.availability())
                    .await
                    .unwrap_or_else(|_| Availability::Unavailable("ตรวจสอบสถานะไม่ทันเวลา".into())),
            }
        }
    };
    let (hermes, claude_cli, api) = tokio::join!(
        check(ProviderId::Hermes, "ตั้งค่า Hermes base URL ไม่ถูกต้อง"),
        check(ProviderId::ClaudeCli, "ไม่พบโปรแกรม claude ใน PATH"),
        check(ProviderId::Api, "ตั้งค่า Anthropic base URL ไม่ถูกต้อง"),
    );
    Ok(ProviderStatus {
        hermes,
        claude_cli,
        api,
        notices: state.startup_notices.clone(),
    })
}

/// Ask where to save, then write a redacted export. Returns the path, or
/// `None` if the user cancelled the dialog.
#[tauri::command]
pub async fn export_conversation(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: i64,
    format: String,
) -> CmdResult<Option<String>> {
    let (ext, label) = match format.as_str() {
        "md" => ("md", "Markdown"),
        "json" => ("json", "JSON"),
        _ => return Err("รูปแบบ export ต้องเป็น md หรือ json".into()),
    };
    let conv = state.store.conversation(id).map_err(err)?;
    let known = state.secrets.all_values();
    let content = match ext {
        "md" => assistant_core::export::export_markdown(&state.store, id, &known),
        _ => assistant_core::export::export_json(&state.store, id, &known),
    }
    .map_err(err)?;
    let name: String = conv
        .title
        .chars()
        .map(|c| {
            if r#"<>:"/\|?*"#.contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let picked = app
        .dialog()
        .file()
        .set_title("บันทึกแชต")
        .set_file_name(format!("{}.{ext}", name.trim()))
        .add_filter(label, &[ext])
        .blocking_save_file();
    let Some(fp) = picked else { return Ok(None) };
    let path = fp.into_path().map_err(err)?;
    assistant_core::export::write_export(&path, &content).map_err(err)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// The only place the MCP key for Hermes is shown.
#[tauri::command]
pub async fn hermes_setup_snippet(state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    let key = state
        .secrets
        .get(SecretKey::McpHermes)
        .ok_or("ยังไม่มี key ของ MCP")?;
    let s = state.settings.read().await;
    let snippet = parse::hermes_snippet(
        &state.mcp.hermes_url(),
        &key,
        &s.hermes_allowed_toolsets,
        &s.hermes_base_url,
    );
    Ok(if state.mcp_port_fallback {
        format!("{}{snippet}", parse::fallback_port_warning(s.mcp_port))
    } else {
        snippet
    })
}

/// Startup problems to show in the UI.
#[tauri::command]
pub fn startup_notices(state: State<'_, Arc<AppState>>) -> Vec<String> {
    state.startup_notices.clone()
}
