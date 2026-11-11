#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
mod approver;
mod commands;
mod parse;
mod sink;
mod turns;

use app_state::{build_jev, build_providers, AppState, SwappableGate};
use approver::TauriApprover;
use assistant_core::command_service::{CommandService, EventSink};
use assistant_core::decision::gate::CommandGate;
use assistant_core::decision::router::Router;
use assistant_core::mcp_server::{McpServer, McpServerHandle};
use assistant_core::orchestrator::Orchestrator;
use assistant_core::runner::RunLimits;
use assistant_core::settings::{KeyringStore, SecretKey, SecretStore, Settings};
use assistant_core::store::Store;
use sink::{TauriApprovalUi, TauriSink};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tauri::{Manager, RunEvent, WindowEvent};

/// 32 random bytes as lowercase hex.
fn random_key() -> Result<String, String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Start the MCP server on the configured port; if that fails, fall back to
/// a free port so the app still works, and report why. The flag is true on fallback.
async fn start_mcp(
    port: u16,
    service: Arc<CommandService>,
    key: String,
    notices: &mut Vec<String>,
) -> Result<(McpServerHandle, bool), String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    match McpServer::start(addr, service.clone(), key.clone()).await {
        Ok(h) => Ok((h, false)),
        Err(e) => {
            let fallback =
                McpServer::start(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), service, key)
                    .await
                    .map_err(|e2| format!("เปิด MCP server ไม่ได้: {e}; {e2}"))?;
            notices.push(format!(
                "เปิด MCP server ที่พอร์ต {port} ไม่ได้ ({e}) จึงใช้พอร์ต {} ชั่วคราว: Claude CLI ใช้ได้ตามปกติ แต่ Hermes จะต่อไม่ได้จนกว่าจะแก้ (เปลี่ยนพอร์ตในหน้าตั้งค่าแล้วเปิดแอปใหม่ และอัปเดต config ของ Hermes)",
                fallback.local_addr().port()
            ));
            Ok((fallback, true))
        }
    }
}

fn setup(app: &tauri::App) -> Result<AppState, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let store = Arc::new(Store::open(&dir.join("assistant.db")).map_err(|e| e.to_string())?);
    let settings_path = dir.join("settings.json");
    let settings = Settings::load(&settings_path);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringStore::new());
    let mut notices = Vec::new();

    let mcp_key = match secrets.get(SecretKey::McpHermes).filter(|k| !k.is_empty()) {
        Some(k) => k,
        None => {
            let k = random_key()?;
            if let Err(e) = secrets.set(SecretKey::McpHermes, &k) {
                notices.push(format!("บันทึก key ของ MCP ลง Credential Manager ไม่ได้: {e}"));
            }
            k
        }
    };

    let handle = app.handle().clone();
    let sink: Arc<dyn EventSink> = Arc::new(TauriSink::new(handle.clone()));
    let approver = Arc::new(TauriApprover::new(Arc::new(TauriApprovalUi::new(handle))));
    let jev = build_jev(&settings, secrets.as_ref());
    let gate = Arc::new(SwappableGate::new(CommandGate::new(
        jev.clone(),
        settings.thresholds.clone(),
    )));
    let limits = RunLimits {
        timeout: Duration::from_secs(settings.run_timeout_secs),
        ..RunLimits::default()
    };
    let commands = Arc::new(CommandService::new(
        gate.clone(),
        approver.clone(),
        store.clone(),
        sink.clone(),
        limits,
        Duration::from_secs(settings.approval_timeout_secs),
    ));

    let (mcp, mcp_port_fallback) = tauri::async_runtime::block_on(start_mcp(
        settings.mcp_port,
        commands.clone(),
        mcp_key,
        &mut notices,
    ))?;

    let providers = build_providers(&settings, secrets.as_ref(), &mcp, &commands);
    let router = Arc::new(tokio::sync::RwLock::new(Router::new(
        jev,
        settings.thresholds.clone(),
    )));
    let settings = Arc::new(tokio::sync::RwLock::new(settings));
    let orchestrator = Arc::new(Orchestrator::new(
        store.clone(),
        router,
        providers.clone(),
        commands.clone(),
        mcp.clone(),
        sink,
        settings.clone(),
    ));

    Ok(AppState {
        store,
        settings,
        settings_path,
        secrets,
        approver,
        gate,
        commands,
        mcp,
        orchestrator,
        providers: tokio::sync::RwLock::new(providers),
        save_lock: tokio::sync::Mutex::new(()),
        turns: Arc::new(turns::TurnRegistry::default()),
        mcp_port_fallback,
        startup_notices: notices,
    })
}

fn shutdown(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        state.shutdown_turns();
    }
}

fn main() {
    let app = tauri::Builder::default()
        // Must be the first plugin: a second launch focuses this window and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = setup(app).map_err(|e| {
                eprintln!("startup failed: {e}");
                e
            })?;
            app.manage(Arc::new(state));
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(
                event,
                WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
            ) {
                shutdown(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_conversations,
            commands::search_conversations,
            commands::create_conversation,
            commands::rename_conversation,
            commands::delete_conversation,
            commands::get_messages,
            commands::get_conversation_detail,
            commands::send_message,
            commands::continue_with,
            commands::cancel_turn,
            commands::answer_approval,
            commands::get_settings,
            commands::save_settings,
            commands::set_secret,
            commands::has_secret,
            commands::provider_status,
            commands::export_conversation,
            commands::hermes_setup_snippet,
            commands::startup_notices,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        if let RunEvent::ExitRequested { .. } | RunEvent::Exit = event {
            shutdown(handle);
        }
    });
}
