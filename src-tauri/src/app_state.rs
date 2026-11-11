//! Shared app state and the parts rebuilt when settings or keys change.

use crate::approver::TauriApprover;
use assistant_core::command_service::CommandService;
use assistant_core::decision::gate::{CommandCheck, CommandChecker, CommandGate};
use assistant_core::decision::router::Router;
use assistant_core::jev::{JevApi, JevClient};
use assistant_core::mcp_server::McpServerHandle;
use assistant_core::orchestrator::Orchestrator;
use assistant_core::providers::anthropic::AnthropicProvider;
use assistant_core::providers::claude_cli::ClaudeCliProvider;
use assistant_core::providers::hermes::HermesProvider;
use assistant_core::providers::ChatProvider;
use assistant_core::settings::{SecretKey, SecretStore, Settings};
use assistant_core::store::Store;
use assistant_core::ProviderId;
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

pub type ProviderMap = HashMap<ProviderId, Arc<dyn ChatProvider>>;

/// The command gate, replaceable after a settings change. The command service
/// (shared with the MCP server and Hermes) is built once, so it holds this.
pub struct SwappableGate {
    inner: std::sync::RwLock<Arc<CommandGate>>,
}

impl SwappableGate {
    pub fn new(gate: CommandGate) -> Self {
        Self {
            inner: std::sync::RwLock::new(Arc::new(gate)),
        }
    }

    pub fn set(&self, gate: CommandGate) {
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(gate);
    }
}

#[async_trait]
impl CommandChecker for SwappableGate {
    async fn check(&self, command: &str, cwd: &Path) -> CommandCheck {
        let gate = self.inner.read().unwrap_or_else(|e| e.into_inner()).clone();
        gate.check(command, cwd).await
    }
}

/// Jev client, only when a Jev key is stored and the base URL is valid.
pub fn build_jev(settings: &Settings, secrets: &dyn SecretStore) -> Option<Arc<dyn JevApi>> {
    let key = secrets.get(SecretKey::Jev).filter(|k| !k.is_empty())?;
    let url = url::Url::parse(&settings.jev_base_url).ok()?;
    Some(Arc::new(JevClient::new(url, key)))
}

pub fn build_providers(
    settings: &Settings,
    secrets: &dyn SecretStore,
    mcp: &McpServerHandle,
    commands: &Arc<CommandService>,
) -> ProviderMap {
    let mut map: ProviderMap = HashMap::new();
    if let Ok(url) = url::Url::parse(&settings.anthropic_base_url) {
        map.insert(
            ProviderId::Api,
            Arc::new(AnthropicProvider::new(
                url,
                secrets.get(SecretKey::Anthropic),
                settings.anthropic_model.clone(),
            )),
        );
    }
    if let Some(cli) = ClaudeCliProvider::from_path(mcp.clone(), settings.claude_model.clone()) {
        map.insert(ProviderId::ClaudeCli, Arc::new(cli));
    }
    if let Ok(url) = url::Url::parse(&settings.hermes_base_url) {
        map.insert(
            ProviderId::Hermes,
            Arc::new(HermesProvider::new(
                url,
                secrets.get(SecretKey::HermesApi),
                mcp.clone(),
                commands.clone(),
                settings.hermes_allowed_toolsets.clone(),
            )),
        );
    }
    map
}

pub struct AppState {
    pub store: Arc<Store>,
    pub settings: Arc<RwLock<Settings>>,
    pub settings_path: PathBuf,
    pub secrets: Arc<dyn SecretStore>,
    pub approver: Arc<TauriApprover>,
    pub gate: Arc<SwappableGate>,
    pub commands: Arc<CommandService>,
    pub mcp: McpServerHandle,
    pub orchestrator: Arc<Orchestrator>,
    /// Our copy of the providers, for `provider_status`.
    pub providers: RwLock<ProviderMap>,
    /// Serialises settings/key saves and the rebuilds that follow.
    pub save_lock: tokio::sync::Mutex<()>,
    /// Conversations with a spawned turn (checked before spawning).
    pub busy: Arc<Mutex<HashSet<i64>>>,
    /// Problems found at startup, shown in the UI.
    pub startup_notices: Vec<String>,
}

impl AppState {
    /// Rebuild Jev-backed parts and providers from current settings and keys.
    pub async fn rebuild(&self) {
        let settings = self.settings.read().await.clone();
        let jev = build_jev(&settings, self.secrets.as_ref());
        self.gate
            .set(CommandGate::new(jev.clone(), settings.thresholds.clone()));
        self.orchestrator
            .set_router(Router::new(jev, settings.thresholds.clone()))
            .await;
        let providers =
            build_providers(&settings, self.secrets.as_ref(), &self.mcp, &self.commands);
        *self.providers.write().await = providers.clone();
        self.orchestrator.set_providers(providers).await;
    }

    /// Cancel every running turn and deny every pending approval.
    pub fn shutdown_turns(&self) {
        self.approver.drop_all();
        let ids: Vec<i64> = self
            .busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect();
        for id in ids {
            self.orchestrator.cancel(id);
        }
    }
}

/// Removes a conversation from the busy set when the spawned turn ends.
pub struct BusyGuard {
    pub busy: Arc<Mutex<HashSet<i64>>>,
    pub conversation_id: i64,
}

impl BusyGuard {
    /// Claim `conversation_id`, or `None` if a turn is already running there.
    pub fn claim(busy: &Arc<Mutex<HashSet<i64>>>, conversation_id: i64) -> Option<Self> {
        let mut set = busy.lock().unwrap_or_else(|e| e.into_inner());
        if !set.insert(conversation_id) {
            return None;
        }
        Some(Self {
            busy: busy.clone(),
            conversation_id,
        })
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.conversation_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_guard_claims_once_and_frees_on_drop() {
        let busy = Arc::new(Mutex::new(HashSet::new()));
        let g = BusyGuard::claim(&busy, 7).unwrap();
        assert!(BusyGuard::claim(&busy, 7).is_none());
        assert!(BusyGuard::claim(&busy, 8).is_some());
        drop(g);
        assert!(BusyGuard::claim(&busy, 7).is_some());
    }
}
