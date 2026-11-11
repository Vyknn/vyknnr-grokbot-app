//! Local MCP server exposing one tool, `run_command`.
//!
//! External agents (Claude CLI, Hermes) have their own shell tools disabled and
//! call this tool instead. Every call goes through [`CommandService::execute`],
//! the app's single safety gate; this server never runs anything itself.
//!
//! Routes (loopback only):
//! - `/mcp/t/{token}`: the token must be a registered turn, otherwise 404.
//! - `/mcp/hermes`: requires `Authorization: Bearer <hermes_key>` (401
//!   otherwise) and uses the turn set by `set_active_hermes_turn`.
//!
//! Before any of that, requests with a foreign `Host` or any `Origin` other
//! than `null` get 403 (DNS rebinding / browser protection).

use crate::command_service::{CommandContext, CommandOutcome, CommandService};
use axum::body::Body;
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;

pub use crate::command_service::DENIED_TEXT;
pub const NO_ACTIVE_TURN_TEXT: &str = "ไม่มีงานที่กำลังทำอยู่";

const TOOL_DESCRIPTION: &str = "Run a PowerShell command on the user's Windows machine. \
Every command is reviewed by a safety checker and may require the user's approval; \
it may be denied, in which case it does not run. `cwd` is optional and must be inside \
the conversation's folder, otherwise the conversation's folder is used.";

#[derive(Debug, thiserror::Error)]
pub enum McpServerError {
    #[error("MCP server must bind to 127.0.0.1, got {0}")]
    NotLoopback(SocketAddr),
    #[error("Hermes key must not be empty")]
    EmptyHermesKey,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type McpResult<T> = std::result::Result<T, McpServerError>;

/// Which turn a request is bound to, decided by the route before MCP runs.
#[derive(Debug, Clone)]
enum Binding {
    Turn(String),
    Hermes,
}

struct Inner {
    addr: SocketAddr,
    service: Arc<CommandService>,
    hermes_key: String,
    turns: Mutex<HashMap<String, (u64, CommandContext)>>,
    active_hermes: Mutex<Option<String>>,
    hermes_lock: Arc<tokio::sync::Semaphore>,
    next_id: AtomicU64,
    shutdown: CancellationToken,
}

impl Inner {
    fn context(&self, token: &str) -> Option<CommandContext> {
        self.turns
            .lock()
            .unwrap()
            .get(token)
            .map(|(_, ctx)| ctx.clone())
    }

    fn host_allowed(&self, headers: &HeaderMap) -> bool {
        let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else {
            return false;
        };
        let port = self.addr.port();
        let host = host.to_ascii_lowercase();
        host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
    }

    fn bearer_ok(&self, headers: &HeaderMap) -> bool {
        let Some(value) = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
        else {
            return false;
        };
        let Some(token) = value.strip_prefix("Bearer ") else {
            return false;
        };
        bool::from(token.as_bytes().ct_eq(self.hermes_key.as_bytes()))
    }
}

pub struct McpServer;

impl McpServer {
    /// Bind to `addr` (must be `127.0.0.1`, port 0 allowed) and serve in the background.
    pub async fn start(
        addr: SocketAddr,
        service: Arc<CommandService>,
        hermes_key: String,
    ) -> McpResult<McpServerHandle> {
        if addr.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err(McpServerError::NotLoopback(addr));
        }
        if hermes_key.is_empty() {
            return Err(McpServerError::EmptyHermesKey);
        }
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let shutdown = CancellationToken::new();
        let inner = Arc::new(Inner {
            addr,
            service,
            hermes_key,
            turns: Mutex::new(HashMap::new()),
            active_hermes: Mutex::new(None),
            hermes_lock: Arc::new(tokio::sync::Semaphore::new(1)),
            next_id: AtomicU64::new(1),
            shutdown: shutdown.clone(),
        });

        let port = addr.port();
        let config = StreamableHttpServerConfig::default()
            .with_allowed_hosts([format!("127.0.0.1:{port}"), format!("localhost:{port}")])
            .with_allowed_origins(["null"])
            .enforce_origin_validation()
            .with_cancellation_token(shutdown.child_token());
        let factory_inner = inner.clone();
        let mcp: StreamableHttpService<RunCommandServer, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(RunCommandServer::new(factory_inner.clone())),
                Default::default(),
                config,
            );

        let app = Router::new()
            .route("/mcp/t/{token}", any(turn_route))
            .route("/mcp/hermes", any(hermes_route))
            .with_state(AppState {
                inner: inner.clone(),
                mcp,
            });

        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
                .await;
        });

        Ok(McpServerHandle { inner })
    }
}

#[derive(Clone)]
struct AppState {
    inner: Arc<Inner>,
    mcp: StreamableHttpService<RunCommandServer, LocalSessionManager>,
}

/// Host / Origin checks shared by both routes.
fn browser_guard(inner: &Inner, headers: &HeaderMap) -> Option<Response> {
    if !inner.host_allowed(headers) {
        return Some(StatusCode::FORBIDDEN.into_response());
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        if origin.as_bytes() != b"null" {
            return Some(StatusCode::FORBIDDEN.into_response());
        }
    }
    None
}

async fn forward(state: &AppState, binding: Binding, mut req: Request) -> Response {
    req.extensions_mut().insert(binding);
    state.mcp.handle(req).await.map(Body::new)
}

async fn turn_route(
    State(state): State<AppState>,
    UrlPath(token): UrlPath<String>,
    req: Request,
) -> Response {
    if let Some(r) = browser_guard(&state.inner, req.headers()) {
        return r;
    }
    if !state.inner.turns.lock().unwrap().contains_key(&token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    forward(&state, Binding::Turn(token), req).await
}

async fn hermes_route(State(state): State<AppState>, req: Request) -> Response {
    if let Some(r) = browser_guard(&state.inner, req.headers()) {
        return r;
    }
    if !state.inner.bearer_ok(req.headers()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    forward(&state, Binding::Hermes, req).await
}

/// Handle to the running server. Cheap to clone.
#[derive(Clone)]
pub struct McpServerHandle {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for McpServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerHandle")
            .field("addr", &self.inner.addr)
            .finish_non_exhaustive()
    }
}

impl McpServerHandle {
    /// Make `ctx.turn_token` reachable at [`Self::url_for_turn`] until the guard drops.
    /// Registering the same token again replaces the earlier context.
    pub fn register_turn(&self, ctx: CommandContext) -> TurnGuard {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let token = ctx.turn_token.clone();
        self.inner
            .turns
            .lock()
            .unwrap()
            .insert(token.clone(), (id, ctx));
        TurnGuard {
            inner: self.inner.clone(),
            token,
            id,
        }
    }

    pub fn context(&self, token: &str) -> Option<CommandContext> {
        self.inner.context(token)
    }

    pub fn url_for_turn(&self, token: &str) -> String {
        format!("http://127.0.0.1:{}/mcp/t/{token}", self.inner.addr.port())
    }

    pub fn hermes_url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp/hermes", self.inner.addr.port())
    }

    pub fn set_active_hermes_turn(&self, token: Option<String>) {
        *self.inner.active_hermes.lock().unwrap() = token;
    }

    /// Shared lock making Hermes turns exclusive: the Hermes route binds to a
    /// single active turn, so every provider using this server must hold a
    /// permit from this one semaphore for the life of its turn.
    pub fn hermes_turn_lock(&self) -> Arc<tokio::sync::Semaphore> {
        self.inner.hermes_lock.clone()
    }

    /// The turn token Hermes calls are currently bound to, if any.
    pub fn active_hermes_turn(&self) -> Option<String> {
        self.inner.active_hermes.lock().unwrap().clone()
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.inner.addr
    }

    /// Stop accepting requests and close all MCP sessions.
    pub fn shutdown(self) {
        self.inner.shutdown.cancel();
    }
}

/// Unregisters its turn on drop. In-flight calls keep their cloned context.
pub struct TurnGuard {
    inner: Arc<Inner>,
    token: String,
    id: u64,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        let mut turns = self.inner.turns.lock().unwrap();
        // Only remove our own registration, not a later one with the same token.
        if turns.get(&self.token).is_some_and(|(id, _)| *id == self.id) {
            turns.remove(&self.token);
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RunCommandArgs {
    /// The PowerShell command to run.
    pub command: String,
    /// Working directory; must be inside the conversation's folder.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Clone)]
struct RunCommandServer {
    inner: Arc<Inner>,
    tool_router: ToolRouter<Self>,
}

impl RunCommandServer {
    fn new(inner: Arc<Inner>) -> Self {
        Self {
            inner,
            tool_router: Self::tool_router(),
        }
    }

    fn bound_context(&self, parts: &http::request::Parts) -> Option<CommandContext> {
        match parts.extensions.get::<Binding>()? {
            Binding::Turn(token) => self.inner.context(token),
            Binding::Hermes => {
                let active = self.inner.active_hermes.lock().unwrap().clone()?;
                self.inner.context(&active)
            }
        }
    }
}

fn tool_error(text: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(text)])
}

#[tool_router]
impl RunCommandServer {
    #[tool(description = TOOL_DESCRIPTION)]
    async fn run_command(
        &self,
        Parameters(args): Parameters<RunCommandArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> CallToolResult {
        let Some(mut ctx) = self.bound_context(&parts) else {
            return tool_error(NO_ACTIVE_TURN_TEXT);
        };
        ctx.cwd = choose_cwd(&ctx.cwd, args.cwd.as_deref());
        let service = &self.inner.service;
        let outcome = service.execute(&ctx, &args.command).await;
        // Chat history is how other providers see this command later.
        // A failed write must not change what the model is told.
        let text = service
            .add_result_to_chat(&ctx, &args.command, &outcome)
            .unwrap_or_else(|_| service.text_for_model(&outcome));
        match outcome {
            CommandOutcome::Ran { .. } => CallToolResult::success(vec![ContentBlock::text(text)]),
            CommandOutcome::Denied => tool_error(&text),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RunCommandServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

/// Use `requested` only if it is an existing directory inside `base`;
/// otherwise `base`. Relative paths are resolved against `base`.
pub fn choose_cwd(base: &Path, requested: Option<&str>) -> PathBuf {
    let Some(requested) = requested.filter(|r| !r.trim().is_empty()) else {
        return base.to_path_buf();
    };
    let Ok(base_canon) = std::fs::canonicalize(base) else {
        return base.to_path_buf();
    };
    let Ok(want) = std::fs::canonicalize(base.join(requested)) else {
        return base.to_path_buf();
    };
    if want.is_dir() && is_within(&base_canon, &want) {
        strip_verbatim(want)
    } else {
        base.to_path_buf()
    }
}

fn same_component(a: Component<'_>, b: Component<'_>) -> bool {
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase()
            == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn is_within(base: &Path, p: &Path) -> bool {
    let mut pc = p.components();
    for b in base.components() {
        match pc.next() {
            Some(c) if same_component(b, c) => {}
            _ => return false,
        }
    }
    true
}

/// `\\?\C:\x` -> `C:\x`, `\\?\UNC\s\x` -> `\\s\x`, so PowerShell shows a normal path.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.as_bytes().get(1) == Some(&b':') {
            return PathBuf::from(rest);
        }
    }
    p
}
