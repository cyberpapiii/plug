//! Daemon mode — headless Engine with Unix socket IPC.
//!
//! Provides `plug serve --daemon` functionality: starts the Engine without TUI,
//! listens on a Unix socket for CLI queries, and logs to file.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use dashmap::DashMap;
use fs2::FileExt as _;
use rmcp::ErrorData as McpError;
use rmcp::model::{
    ClientCapabilities, CreateElicitationRequestParams, CreateElicitationResult,
    CreateMessageRequestParams, CreateMessageResult, PaginatedRequestParams, RequestId,
};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

use plug_core::engine::Engine;
use plug_core::ipc::{self, IpcClientRequest, IpcClientResponse, IpcRequest, IpcResponse};
use plug_core::proxy::DownstreamBridge;
use plug_core::session::SessionStore;

/// Maximum concurrently registered proxy client sessions.
///
/// Short-lived admin/query sockets are not counted toward this limit so runtime
/// inspection remains available even when many long-lived `plug connect`
/// clients are active.
const MAX_REGISTERED_PROXY_CLIENTS: usize = 32;

/// Idle timeout for short-lived IPC connections (status queries, admin commands).
/// Proxy connections (those that have called Register) are exempt from this timeout.
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

// ──────────────────────── Client Registry ─────────────────────────────────────

/// Tracks proxy client sessions connected to the daemon.
///
/// Uses a `watch` channel to broadcast client count changes for the grace
/// period shutdown logic (avoids missed-wakeup races that `Notify` has).
pub struct ClientRegistry {
    sessions: DashMap<String, ClientSession>,
    client_sessions: DashMap<String, String>,
    /// Sends current client count on every change.
    count_tx: tokio::sync::watch::Sender<usize>,
}

/// Metadata for a connected proxy client.
struct ClientSession {
    client_id: String,
    client_info: Option<String>,
    connected_at: Instant,
    capabilities: ClientCapabilities,
}

struct RegistrationResult {
    session_id: String,
    replaced_session_id: Option<String>,
}

impl ClientRegistry {
    fn new() -> (Self, tokio::sync::watch::Receiver<usize>) {
        let (count_tx, count_rx) = tokio::sync::watch::channel(0usize);
        (
            Self {
                sessions: DashMap::new(),
                client_sessions: DashMap::new(),
                count_tx,
            },
            count_rx,
        )
    }

    /// Register a new client, returning the assigned session ID.
    ///
    /// Enforces a cap on concurrently registered proxy sessions while still
    /// allowing an existing client ID to replace its prior session.
    fn try_register(
        &self,
        client_id: String,
        client_info: Option<String>,
        max_clients: usize,
    ) -> Result<RegistrationResult, ()> {
        let replacing_existing_client = self.client_sessions.contains_key(&client_id);
        if !replacing_existing_client && self.sessions.len() >= max_clients {
            return Err(());
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        let replaced_session_id = self
            .client_sessions
            .insert(client_id.clone(), session_id.clone());
        if let Some(ref replaced) = replaced_session_id {
            self.sessions.remove(replaced);
        }
        tracing::info!(
            client_id = %client_id,
            session_id = %session_id,
            client_info = ?client_info,
            "client registered"
        );
        self.sessions.insert(
            session_id.clone(),
            ClientSession {
                client_id,
                client_info,
                connected_at: Instant::now(),
                capabilities: ClientCapabilities::default(),
            },
        );
        self.count_tx.send_modify(|c| *c = self.sessions.len());
        Ok(RegistrationResult {
            session_id,
            replaced_session_id,
        })
    }

    /// Deregister a client session.
    fn deregister(&self, session_id: &str) {
        if let Some((_, session)) = self.sessions.remove(session_id) {
            if self
                .client_sessions
                .get(&session.client_id)
                .is_some_and(|entry| entry.value() == session_id)
            {
                self.client_sessions.remove(&session.client_id);
            }
            let duration = session.connected_at.elapsed();
            tracing::info!(
                client_id = %session.client_id,
                session_id = %session_id,
                duration_secs = duration.as_secs(),
                "client deregistered"
            );
            self.count_tx.send_modify(|c| *c = self.sessions.len());
        }
    }

    /// Update client_info for an existing session.
    fn update_client_info(&self, session_id: &str, client_info: String) -> bool {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.client_info = Some(client_info);
            true
        } else {
            false
        }
    }

    /// Get the client_info string for a session (for client type detection).
    fn client_info(&self, session_id: &str) -> Option<String> {
        self.sessions
            .get(session_id)
            .and_then(|s| s.client_info.clone())
    }

    /// Number of currently connected clients.
    fn count(&self) -> usize {
        self.sessions.len()
    }

    /// Update the MCP client capabilities for a session.
    fn update_capabilities(&self, session_id: &str, capabilities: ClientCapabilities) -> bool {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.capabilities = capabilities;
            true
        } else {
            false
        }
    }

    /// Get the MCP client capabilities for a session.
    fn capabilities(&self, session_id: &str) -> Option<ClientCapabilities> {
        self.sessions
            .get(session_id)
            .map(|s| s.capabilities.clone())
    }

    fn session_exists(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    /// Snapshot all live sessions for CLI inspection.
    fn list(&self) -> Vec<plug_core::ipc::IpcClientInfo> {
        let mut clients = self
            .sessions
            .iter()
            .map(|entry| plug_core::ipc::IpcClientInfo {
                client_id: entry.client_id.clone(),
                session_id: entry.key().clone(),
                client_info: entry.client_info.clone(),
                connected_secs: entry.connected_at.elapsed().as_secs(),
            })
            .collect::<Vec<_>>();
        clients.sort_by(|a, b| {
            a.client_info
                .cmp(&b.client_info)
                .then(a.session_id.cmp(&b.session_id))
        });
        clients
    }

    /// Snapshot all live sessions in the newer transport-aware shape.
    fn list_live_sessions(&self) -> Vec<plug_core::ipc::IpcLiveSessionInfo> {
        let mut sessions = self
            .sessions
            .iter()
            .map(|entry| plug_core::ipc::IpcLiveSessionInfo {
                transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
                client_id: Some(entry.client_id.clone()),
                session_id: entry.key().clone(),
                client_type: entry
                    .client_info
                    .as_deref()
                    .map(plug_core::client_detect::detect_client)
                    .unwrap_or(plug_core::types::ClientType::Unknown),
                client_info: entry.client_info.clone(),
                connected_secs: entry.connected_at.elapsed().as_secs(),
                last_activity_secs: None,
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|a, b| {
            a.client_info
                .cmp(&b.client_info)
                .then(a.session_id.cmp(&b.session_id))
        });
        sessions
    }
}

// ──────────────────────────────── Path helpers ────────────────────────────────

/// Return the daemon runtime directory (for socket + PID file + auth token).
///
/// - macOS: `~/Library/Application Support/plug/`
/// - Linux: `$XDG_RUNTIME_DIR/plug/` (fallback: `~/.local/state/plug/`)
pub fn runtime_dir() -> PathBuf {
    #[cfg(test)]
    if let Some((runtime, _)) = test_runtime_paths()
        .lock()
        .expect("test runtime path mutex poisoned")
        .clone()
    {
        return runtime.join("plug");
    }

    #[cfg(target_os = "macos")]
    {
        dirs_path("Library/Application Support/plug")
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(dir).join("plug")
        } else {
            dirs_path(".local/state/plug")
        }
    }
}

/// Return the daemon log directory.
///
/// - macOS: `~/Library/Logs/plug/`
/// - Linux: `$XDG_STATE_HOME/plug/logs/` (fallback: `~/.local/state/plug/logs/`)
pub fn log_dir() -> PathBuf {
    #[cfg(test)]
    if let Some((_, state)) = test_runtime_paths()
        .lock()
        .expect("test runtime path mutex poisoned")
        .clone()
    {
        return state.join("plug/logs");
    }

    #[cfg(target_os = "macos")]
    {
        dirs_path("Library/Logs/plug")
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
            PathBuf::from(dir).join("plug/logs")
        } else {
            dirs_path(".local/state/plug/logs")
        }
    }
}

/// Expand `~/subpath` to an absolute path.
fn dirs_path(subpath: &str) -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().join(subpath))
        .unwrap_or_else(|| PathBuf::from(".").join(subpath))
}

#[cfg(test)]
fn test_runtime_paths() -> &'static std::sync::Mutex<Option<(PathBuf, PathBuf)>> {
    static TEST_RUNTIME_PATHS: std::sync::OnceLock<std::sync::Mutex<Option<(PathBuf, PathBuf)>>> =
        std::sync::OnceLock::new();
    TEST_RUNTIME_PATHS.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
pub(crate) fn set_test_runtime_paths(runtime: PathBuf, state: PathBuf) {
    *test_runtime_paths()
        .lock()
        .expect("test runtime path mutex poisoned") = Some((runtime, state));
}

#[cfg(test)]
pub(crate) fn clear_test_runtime_paths() {
    *test_runtime_paths()
        .lock()
        .expect("test runtime path mutex poisoned") = None;
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("plug.sock")
}

pub fn pid_path() -> PathBuf {
    runtime_dir().join("plug.pid")
}

pub fn token_path() -> PathBuf {
    runtime_dir().join("plug.token")
}

// ──────────────────────────────── Auth token ─────────────────────────────────

use plug_core::auth::{generate_auth_token, verify_auth_token};

// ──────────────────────────── Directory setup ────────────────────────────────

/// Create a directory with 0700 permissions, creating parents as needed.
/// On unix, uses DirBuilder to set mode atomically at creation time.
fn ensure_dir(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .with_context(|| format!("failed to create directory: {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
            .with_context(|| format!("failed to create directory: {}", path.display()))?;
    }
    Ok(())
}

/// Set file permissions to 0600 (owner read/write only).
#[cfg(unix)]
fn set_file_permissions_0600(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    Ok(())
}

// ──────────────────────────── PID file locking ───────────────────────────────

/// Acquire an exclusive lock on the PID file, returning the locked file handle.
///
/// The file handle MUST be held for the daemon's lifetime — dropping it releases the lock.
fn acquire_pid_lock(pid_path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .read(true)
        .open(pid_path)
        .with_context(|| format!("failed to open PID file: {}", pid_path.display()))?;

    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("another plug daemon is already running (PID file locked)"))?;

    // Write our PID
    use std::io::Write;
    let mut f = &file;
    write!(f, "{}", std::process::id())?;
    f.flush()?;

    #[cfg(unix)]
    set_file_permissions_0600(pid_path)?;

    Ok(file)
}

// ─────────────────────────── Daemon entry point ──────────────────────────────

/// Start the daemon: Engine + Unix socket IPC listener + file logging.
///
/// Returns the tracing-appender guard that MUST be held for the daemon's lifetime
/// (dropping it flushes and closes the log file).
pub async fn run_daemon(
    engine: Arc<Engine>,
    config_path: PathBuf,
    grace_period_secs: u64,
    http_sessions: Option<Arc<dyn SessionStore>>,
) -> anyhow::Result<()> {
    let rt_dir = runtime_dir();
    let log_directory = log_dir();

    // Create directories with secure permissions
    ensure_dir(&rt_dir)?;
    ensure_dir(&log_directory)?;

    // Generate auth token and write to file with restricted permissions from creation
    let auth_token = generate_auth_token();
    let token_file = token_path();
    plug_core::auth::write_token_file(&token_file, &auth_token)
        .with_context(|| format!("failed to write auth token: {}", token_file.display()))?;

    // Acquire PID file lock BEFORE socket operations to prevent TOCTOU races.
    // Two concurrent auto_start_daemon calls: the loser fails here, retries connecting.
    let pid_file_path = pid_path();
    let _pid_lock = acquire_pid_lock(&pid_file_path)?;

    // Clean up stale socket if it exists (safe now — we hold the PID lock)
    let sock_path = socket_path();
    if std::fs::symlink_metadata(&sock_path).is_ok() {
        // Try connecting to check if another daemon is alive
        if tokio::net::UnixStream::connect(&sock_path).await.is_ok() {
            anyhow::bail!(
                "another plug daemon is already running on {}",
                sock_path.display()
            );
        }
        // Stale socket — remove it
        std::fs::remove_file(&sock_path).ok();
    }

    // Bind Unix socket
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("failed to bind Unix socket: {}", sock_path.display()))?;

    #[cfg(unix)]
    set_file_permissions_0600(&sock_path)?;

    tracing::info!(
        socket = %sock_path.display(),
        pid_file = %pid_file_path.display(),
        "daemon started"
    );

    let cancel = engine.cancel_token().clone();
    let auth_token: Arc<str> = Arc::from(auth_token.as_str());
    let (client_registry, count_rx) = ClientRegistry::new();
    let client_registry = Arc::new(client_registry);

    // Grace period: when the last proxy client disconnects, start a countdown.
    // If no new client reconnects and no daemon-owned HTTP sessions remain when it
    // fires, shut down the daemon.
    // A grace_period_secs of 0 disables auto-shutdown (explicit shutdown only),
    // which is the default behavior.
    let grace_cancel = CancellationToken::new();

    if grace_period_secs > 0 {
        let grace_token = grace_cancel.clone();
        let daemon_cancel = cancel.clone();
        let mut count_rx = count_rx;
        let http_sessions = http_sessions.clone();
        tokio::spawn(async move {
            loop {
                // Wait for a change in client count
                if count_rx.changed().await.is_err() {
                    return; // Sender dropped (registry gone)
                }

                let count = *count_rx.borrow();
                if count == 0 {
                    tracing::info!(
                        grace_secs = grace_period_secs,
                        "last client disconnected, starting grace period"
                    );

                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(grace_period_secs)) => {
                            // Grace period expired — recheck IPC and daemon-owned HTTP sessions.
                            let http_session_count = http_sessions
                                .as_ref()
                                .map(|sessions| sessions.session_count())
                                .unwrap_or(0);
                            if *count_rx.borrow() == 0 && http_session_count == 0 {
                                tracing::info!("grace period expired with no clients, shutting down");
                                daemon_cancel.cancel();
                                return;
                            } else if *count_rx.borrow() == 0 && http_session_count > 0 {
                                tracing::info!(
                                    http_session_count,
                                    "grace period expired but HTTP sessions are still active; keeping daemon alive"
                                );
                            }
                        }
                        result = count_rx.changed() => {
                            if result.is_err() {
                                return;
                            }
                            if *count_rx.borrow() > 0 {
                                tracing::info!("client reconnected, grace period cancelled");
                            }
                        }
                        _ = grace_token.cancelled() => return,
                    }
                }
            }
        });
    }

    // Accept IPC connections until shutdown
    loop {
        tokio::select! {
            biased;

            _ = cancel.cancelled() => break,

            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let engine_cancel = engine.cancel_token().clone();
                        let server_manager = engine.server_manager().clone();
                        let snapshot = engine.snapshot();
                        let auth = auth_token.clone();
                        let engine_clone = engine.clone();
                        let registry = client_registry.clone();
                        let config_path = config_path.clone();
                        let http_sessions = http_sessions.clone();

                        tokio::spawn(async move {
                            let started_at = Instant::now()
                                .checked_sub(snapshot.uptime)
                                .unwrap_or_else(Instant::now);
                            let ctx = ConnectionContext {
                                cancel: engine_cancel,
                                auth_token: auth,
                                server_manager,
                                engine: engine_clone,
                                config_path: config_path.clone(),
                                started_at,
                                client_registry: registry,
                                http_sessions,
                                session_id: None,
                                reverse_request_rx: None,
                            };
                            if let Err(e) = handle_ipc_connection(stream, ctx).await {
                                tracing::debug!(error = %e, "IPC connection ended");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to accept IPC connection");
                    }
                }
            }
        }
    }

    // Stop grace period task
    grace_cancel.cancel();

    // Cleanup
    tracing::info!("daemon shutting down, cleaning up");
    std::fs::remove_file(&sock_path).ok();
    std::fs::remove_file(&pid_file_path).ok();
    std::fs::remove_file(token_path()).ok();

    Ok(())
}

// ──────────────────────── Daemon Bridge ────────────────────────────────────────

/// Downstream bridge for IPC proxy clients.
///
/// Forwards reverse requests (elicitation, sampling) from upstream MCP servers
/// to the IPC proxy client that initiated the tool call. The proxy side listens
/// on the `reverse_request_rx` end of the channel.
struct DaemonBridge {
    session_id: Arc<str>,
    reverse_request_tx: tokio::sync::mpsc::Sender<(
        IpcClientRequest,
        tokio::sync::oneshot::Sender<IpcClientResponse>,
    )>,
    client_registry: Arc<ClientRegistry>,
}

impl DaemonBridge {
    /// Send a reverse request over the IPC channel and await the response.
    async fn send_and_await(
        &self,
        request: IpcClientRequest,
    ) -> Result<IpcClientResponse, McpError> {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.reverse_request_tx
            .send((request, resp_tx))
            .await
            .map_err(|_| {
                McpError::internal_error(
                    format!("IPC connection lost for session {}", self.session_id),
                    None,
                )
            })?;
        resp_rx.await.map_err(|_| {
            McpError::internal_error(
                format!(
                    "IPC response channel closed for session {}",
                    self.session_id
                ),
                None,
            )
        })
    }
}

impl DownstreamBridge for DaemonBridge {
    fn create_elicitation(
        &self,
        request: CreateElicitationRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<CreateElicitationResult, McpError>> + Send + '_>> {
        let caps = self.client_registry.capabilities(&self.session_id);
        if caps.as_ref().and_then(|c| c.elicitation.as_ref()).is_none() {
            return Box::pin(async {
                Err(McpError::internal_error(
                    format!("client {} does not support elicitation", self.session_id),
                    None,
                ))
            });
        }
        Box::pin(async move {
            match self
                .send_and_await(IpcClientRequest::CreateElicitation { params: request })
                .await?
            {
                IpcClientResponse::CreateElicitation { result } => Ok(result),
                IpcClientResponse::Error { message } => {
                    Err(McpError::internal_error(message, None))
                }
                other => Err(McpError::internal_error(
                    format!("unexpected IPC response: {other:?}"),
                    None,
                )),
            }
        })
    }

    fn create_message(
        &self,
        request: CreateMessageRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<CreateMessageResult, McpError>> + Send + '_>> {
        let caps = self.client_registry.capabilities(&self.session_id);
        if caps.as_ref().and_then(|c| c.sampling.as_ref()).is_none() {
            return Box::pin(async {
                Err(McpError::internal_error(
                    format!("client {} does not support sampling", self.session_id),
                    None,
                ))
            });
        }
        Box::pin(async move {
            match self
                .send_and_await(IpcClientRequest::CreateMessage { params: request })
                .await?
            {
                IpcClientResponse::CreateMessage { result } => Ok(result),
                IpcClientResponse::Error { message } => {
                    Err(McpError::internal_error(message, None))
                }
                other => Err(McpError::internal_error(
                    format!("unexpected IPC response: {other:?}"),
                    None,
                )),
            }
        })
    }
}

// ──────────────────────── Connection Context ──────────────────────────────────

/// Per-connection context — everything needed to handle IPC requests without
/// holding a reference to Engine.
struct ConnectionContext {
    cancel: CancellationToken,
    auth_token: Arc<str>,
    server_manager: Arc<plug_core::server::ServerManager>,
    engine: Arc<Engine>,
    config_path: PathBuf,
    started_at: Instant,
    client_registry: Arc<ClientRegistry>,
    http_sessions: Option<Arc<dyn SessionStore>>,
    /// Session ID assigned during Register (for auto-deregister on disconnect).
    session_id: Option<String>,
    /// Receiver for reverse requests from the daemon bridge. Created during
    /// Register, consumed by the IPC loop to forward reverse requests to the
    /// proxy client. See `DaemonBridge` for the sender side.
    reverse_request_rx: Option<
        tokio::sync::mpsc::Receiver<(
            IpcClientRequest,
            tokio::sync::oneshot::Sender<IpcClientResponse>,
        )>,
    >,
}

/// Handle a single IPC connection: read requests, dispatch, send responses.
///
/// Auto-deregisters any session created via `Register` when the connection closes
/// (clean EOF, error, or idle timeout).
async fn handle_ipc_connection(
    stream: tokio::net::UnixStream,
    mut ctx: ConnectionContext,
) -> anyhow::Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    let result = handle_ipc_loop(&mut reader, &mut writer, &mut ctx).await;

    // Auto-deregister on disconnect (clean or crash)
    if let Some(ref session_id) = ctx.session_id {
        ctx.client_registry.deregister(session_id);
        let target = plug_core::notifications::NotificationTarget::Stdio {
            client_id: std::sync::Arc::from(session_id.as_str()),
        };
        ctx.engine
            .tool_router()
            .unregister_downstream_bridge(&target);
        if ctx.engine.tool_router().clear_roots_for_target(&target) {
            ctx.engine
                .tool_router()
                .forward_roots_list_changed_to_upstreams()
                .await;
        }
        ctx.engine.tool_router().remove_client_log_level(session_id);
    }

    result
}

/// Inner loop for IPC connection handling.
async fn handle_ipc_loop(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    ctx: &mut ConnectionContext,
) -> anyhow::Result<()> {
    use plug_core::notifications::ProtocolNotification;

    // Logging subscription — activated after Register so the daemon can push
    // logging notifications to this IPC client in real time.
    let mut log_rx: Option<tokio::sync::broadcast::Receiver<ProtocolNotification>> = None;
    // Protocol notification subscription — activated after Register so the daemon
    // can push list_changed, progress, and cancelled notifications to this IPC client.
    let mut ctrl_rx: Option<tokio::sync::broadcast::Receiver<ProtocolNotification>> = None;

    loop {
        // Proxy connections (those that have Registered) are long-lived and should
        // not be subject to the idle timeout. Short-lived admin/query connections use
        // the timeout to reclaim resources.
        let frame = if let Some(ref mut rx) = log_rx {
            // Registered with notifications — multiplex request reads and push
            // notifications. Notifications are sent immediately when idle. During
            // a request dispatch, they queue in the broadcast channel and get
            // drained after the response.
            'select: loop {
                tokio::select! {
                    biased;
                    _ = ctx.cancel.cancelled() => return Ok(()),
                    recv = rx.recv() => {
                        send_ipc_logging_notification(writer, recv).await?;
                    }
                    recv = async {
                        if let Some(ref mut crx) = ctrl_rx {
                            crx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        send_ipc_control_notification(writer, recv, ctx.session_id.as_deref()).await?;
                    }
                    result = ipc::read_frame(reader) => break 'select result,
                }
            }
        } else if ctx.session_id.is_some() {
            tokio::select! {
                _ = ctx.cancel.cancelled() => break,
                result = ipc::read_frame(reader) => result,
            }
        } else {
            tokio::select! {
                _ = ctx.cancel.cancelled() => break,
                result = tokio::time::timeout(CONNECTION_IDLE_TIMEOUT, ipc::read_frame(reader)) => {
                    match result {
                        Ok(result) => result,
                        Err(_) => {
                            tracing::debug!("IPC connection idle timeout");
                            break;
                        }
                    }
                }
            }
        };

        let frame = match frame {
            Ok(Some(data)) => data,
            Ok(None) => break, // clean EOF
            Err(e) => {
                tracing::debug!(error = %e, "IPC frame read error");
                let resp = IpcResponse::Error {
                    code: "FRAME_ERROR".to_string(),
                    message: "malformed frame".to_string(),
                };
                ipc::send_response(writer, &resp).await.ok();
                break;
            }
        };

        // Parse request
        let request: IpcRequest = match serde_json::from_slice(&frame) {
            Ok(req) => req,
            Err(e) => {
                tracing::debug!(error = %e, "IPC parse error");
                let resp = protocol_parse_error_response(&frame).unwrap_or(IpcResponse::Error {
                    code: "PARSE_ERROR".to_string(),
                    message: "invalid request format".to_string(),
                });
                ipc::send_response(writer, &resp).await.ok();
                break;
            }
        };

        // Auth check for admin commands
        if ipc::requires_auth(&request) {
            match ipc::extract_auth_token(&request) {
                Some(provided) => {
                    if !verify_auth_token(provided, &ctx.auth_token) {
                        let resp = IpcResponse::Error {
                            code: "AUTH_FAILED".to_string(),
                            message: "invalid auth token".to_string(),
                        };
                        ipc::send_response(writer, &resp).await?;
                        continue;
                    }
                }
                None => {
                    let resp = IpcResponse::Error {
                        code: "AUTH_REQUIRED".to_string(),
                        message: "auth_token required for this command".to_string(),
                    };
                    ipc::send_response(writer, &resp).await?;
                    continue;
                }
            }
        }

        // Dispatch request. During a tools/call, the upstream MCP server may
        // issue reverse requests (elicitation, sampling) via the DaemonBridge
        // channel. We service those concurrently with the dispatch future.
        //
        // We temporarily take `reverse_request_rx` out of `ctx` to avoid
        // borrow conflicts — `dispatch_request` borrows `ctx` mutably.
        let mut reverse_rx = ctx.reverse_request_rx.take();
        // Capture session_id before dispatch borrows ctx mutably.
        let dispatch_session_id = ctx.session_id.clone();

        let response = {
            use std::pin::pin;

            let dispatch_fut = pin!(dispatch_request(&request, ctx));
            let mut dispatch_fut = dispatch_fut;
            let mut done = false;
            let mut result = None;

            while !done {
                tokio::select! {
                    biased;
                    resp = &mut dispatch_fut, if !done => {
                        result = Some(resp);
                        done = true;
                    }
                    // Service reverse requests from DaemonBridge while tool call is in-flight
                    reverse = async {
                        if let Some(ref mut rx) = reverse_rx {
                            rx.recv().await
                        } else {
                            // No bridge registered — pend forever
                            std::future::pending().await
                        }
                    } => {
                        if let Some((reverse_req, resp_tx)) = reverse {
                            handle_reverse_request(reader, writer, reverse_req, resp_tx).await?;
                        }
                    }
                    // Also forward logging notifications while waiting
                    recv = async {
                        if let Some(ref mut rx) = log_rx {
                            rx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        send_ipc_logging_notification(writer, recv).await?;
                    }
                    // Forward control notifications (list_changed, progress, cancelled)
                    recv = async {
                        if let Some(ref mut crx) = ctrl_rx {
                            crx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        send_ipc_control_notification(writer, recv, dispatch_session_id.as_deref()).await?;
                    }
                }
            }
            result.unwrap()
        };

        // Restore reverse_request_rx (dispatch_request may have replaced it
        // during a Register call, in which case ctx already has the new one).
        if ctx.reverse_request_rx.is_none() {
            ctx.reverse_request_rx = reverse_rx;
        }

        ipc::send_response(writer, &response).await?;

        // Shutdown request — send OK then trigger cancel
        if matches!(request, IpcRequest::Shutdown { .. }) {
            ctx.cancel.cancel();
            break;
        }

        // After registration, subscribe to notification channels for push delivery
        if ctx.session_id.is_some() && log_rx.is_none() {
            log_rx = Some(ctx.engine.tool_router().subscribe_logging());
            ctrl_rx = Some(ctx.engine.tool_router().subscribe_notifications());
        }

        // Drain any notifications that queued during request dispatch
        if let Some(ref mut rx) = log_rx {
            use tokio::sync::broadcast::error::TryRecvError;
            loop {
                match rx.try_recv() {
                    Ok(notif) => {
                        send_ipc_logging_notification(writer, Ok(notif)).await?;
                    }
                    Err(TryRecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "IPC logging notification lagged");
                        let synthetic = IpcResponse::LoggingNotification {
                            params: serde_json::json!({
                                "level": "warning",
                                "logger": "plug",
                                "data": format!("skipped {skipped} log messages")
                            }),
                        };
                        ipc::send_response(writer, &synthetic).await.ok();
                    }
                    Err(TryRecvError::Closed) | Err(TryRecvError::Empty) => break,
                }
            }
        }
        if let Some(ref mut crx) = ctrl_rx {
            use tokio::sync::broadcast::error::TryRecvError;
            loop {
                match crx.try_recv() {
                    Ok(notif) => {
                        send_ipc_control_notification(writer, Ok(notif), ctx.session_id.as_deref())
                            .await?;
                    }
                    Err(TryRecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "IPC control notification lagged");
                        let notif = IpcResponse::LoggingNotification {
                            params: serde_json::to_value(
                                plug_core::notifications::ProtocolNotification::control_lagged_logging_params(
                                    skipped as u64,
                                    "ipc",
                                ),
                            )
                            .unwrap_or_default(),
                        };
                        ipc::send_response(writer, &notif).await.ok();
                    }
                    Err(TryRecvError::Closed) | Err(TryRecvError::Empty) => break,
                }
            }
        }
    }

    Ok(())
}

/// Send a logging notification to the IPC client, handling broadcast errors.
async fn send_ipc_logging_notification(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    recv: Result<
        plug_core::notifications::ProtocolNotification,
        tokio::sync::broadcast::error::RecvError,
    >,
) -> anyhow::Result<()> {
    use plug_core::notifications::ProtocolNotification;
    use tokio::sync::broadcast::error::RecvError;

    match recv {
        Ok(ProtocolNotification::LoggingMessage { params }) => {
            let notif = IpcResponse::LoggingNotification {
                params: serde_json::to_value(params).unwrap_or_default(),
            };
            ipc::send_response(writer, &notif).await.ok();
        }
        Ok(_) => {} // non-logging notification on wrong channel
        Err(RecvError::Lagged(skipped)) => {
            tracing::warn!(skipped, "IPC logging notification lagged");
            let synthetic = IpcResponse::LoggingNotification {
                params: serde_json::json!({
                    "level": "warning",
                    "logger": "plug",
                    "data": format!("skipped {skipped} log messages")
                }),
            };
            ipc::send_response(writer, &synthetic).await.ok();
        }
        Err(RecvError::Closed) => {}
    }
    Ok(())
}

/// Send a protocol (control) notification to the IPC client, handling broadcast errors.
///
/// Broadcast notifications (list_changed) are sent to all registered IPC clients.
/// Targeted notifications (progress, cancelled) are only sent if the target matches
/// this connection's session ID.
async fn send_ipc_control_notification(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    recv: Result<
        plug_core::notifications::ProtocolNotification,
        tokio::sync::broadcast::error::RecvError,
    >,
    session_id: Option<&str>,
) -> anyhow::Result<()> {
    use plug_core::notifications::{NotificationTarget, ProtocolNotification};
    use tokio::sync::broadcast::error::RecvError;

    match recv {
        Ok(ProtocolNotification::ToolListChanged) => {
            ipc::send_response(writer, &IpcResponse::ToolListChangedNotification)
                .await
                .ok();
        }
        Ok(ProtocolNotification::ResourceListChanged) => {
            ipc::send_response(writer, &IpcResponse::ResourceListChangedNotification)
                .await
                .ok();
        }
        Ok(ProtocolNotification::PromptListChanged) => {
            ipc::send_response(writer, &IpcResponse::PromptListChangedNotification)
                .await
                .ok();
        }
        Ok(ProtocolNotification::Progress { target, params }) => {
            // Only forward if this notification targets our session
            if matches!(
                target,
                NotificationTarget::Stdio { client_id: ref target_id }
                    if session_id.is_some_and(|sid| target_id.as_ref() == sid)
            ) {
                let notif = IpcResponse::ProgressNotification {
                    params: serde_json::to_value(params).unwrap_or_default(),
                };
                ipc::send_response(writer, &notif).await.ok();
            }
        }
        Ok(ProtocolNotification::Cancelled { target, params }) => {
            if matches!(
                target,
                NotificationTarget::Stdio { client_id: ref target_id }
                    if session_id.is_some_and(|sid| target_id.as_ref() == sid)
            ) {
                let notif = IpcResponse::CancelledNotification {
                    params: serde_json::to_value(params).unwrap_or_default(),
                };
                ipc::send_response(writer, &notif).await.ok();
            }
        }
        Ok(ProtocolNotification::AuthStateChanged {
            server_id,
            new_state,
        }) => {
            let notif = IpcResponse::AuthStateChanged {
                server_id: server_id.to_string(),
                state: new_state,
            };
            ipc::send_response(writer, &notif).await.ok();
        }
        Ok(notification @ ProtocolNotification::TokenRefreshExchanged { .. }) => {
            if let Some(params) = notification.as_logging_message_params() {
                let notif = IpcResponse::LoggingNotification {
                    params: serde_json::to_value(params).unwrap_or_default(),
                };
                ipc::send_response(writer, &notif).await.ok();
            }
        }
        Ok(
            ProtocolNotification::LoggingMessage { .. }
            | ProtocolNotification::ResourceUpdated { .. },
        ) => {
            // Logging is handled by the dedicated logging channel.
            // ResourceUpdated is not delivered over IPC (subscribe not supported).
        }
        Err(RecvError::Lagged(skipped)) => {
            tracing::warn!(skipped, "IPC control notification lagged");
            let notif = IpcResponse::LoggingNotification {
                params: serde_json::to_value(
                    plug_core::notifications::ProtocolNotification::control_lagged_logging_params(
                        skipped as u64,
                        "ipc",
                    ),
                )
                .unwrap_or_default(),
            };
            ipc::send_response(writer, &notif).await.ok();
        }
        Err(RecvError::Closed) => {}
    }
    Ok(())
}

/// Handle a reverse request from the `DaemonBridge` during an active tool call.
///
/// Writes an `IpcClientRequest` to the IPC socket as a `DaemonToProxyMessage::ReverseRequest`,
/// reads the `IpcClientResponse` back from the proxy, and sends it via the oneshot channel.
///
/// The proxy client's read loop must be prepared to receive `DaemonToProxyMessage::ReverseRequest`
/// frames interleaved with normal `IpcResponse` frames during a `tools/call`.
async fn handle_reverse_request(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    request: IpcClientRequest,
    response_tx: tokio::sync::oneshot::Sender<IpcClientResponse>,
) -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static REVERSE_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    let id = REVERSE_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let msg = ipc::DaemonToProxyMessage::ReverseRequest {
        id,
        request: Box::new(request),
    };

    tracing::debug!(
        reverse_request_id = id,
        "sending reverse request to IPC proxy"
    );
    ipc::send_daemon_message(writer, &msg).await?;

    // Read the proxy's response. The proxy sends an IpcClientResponse frame
    // after handling the reverse request.
    let response = match ipc::read_frame(reader).await? {
        Some(frame) => match serde_json::from_slice::<IpcClientResponse>(&frame) {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!(error = %e, "invalid IPC reverse-request response");
                IpcClientResponse::Error {
                    message: format!("invalid reverse-request response: {e}"),
                }
            }
        },
        None => {
            // Connection closed during reverse request
            IpcClientResponse::Error {
                message: "IPC connection closed during reverse request".to_string(),
            }
        }
    };

    // Send response back to DaemonBridge
    let _ = response_tx.send(response);
    Ok(())
}

/// Dispatch a single IPC request to the appropriate Engine query.
async fn dispatch_request(request: &IpcRequest, ctx: &mut ConnectionContext) -> IpcResponse {
    fn downstream_http_live_sessions(
        sessions: &dyn SessionStore,
    ) -> Vec<plug_core::ipc::IpcLiveSessionInfo> {
        let mut live_sessions = sessions
            .session_snapshots()
            .into_iter()
            .map(|snapshot| plug_core::ipc::IpcLiveSessionInfo {
                transport: match snapshot.transport {
                    plug_core::session::DownstreamTransport::Http => {
                        plug_core::ipc::LiveSessionTransport::Http
                    }
                    plug_core::session::DownstreamTransport::Sse => {
                        plug_core::ipc::LiveSessionTransport::Sse
                    }
                },
                client_id: None,
                session_id: snapshot.session_id,
                client_type: snapshot.client_type,
                client_info: None,
                connected_secs: snapshot.connected_seconds,
                last_activity_secs: Some(snapshot.idle_seconds),
            })
            .collect::<Vec<_>>();
        live_sessions.sort_by(|a, b| {
            a.client_type
                .to_string()
                .cmp(&b.client_type.to_string())
                .then(a.session_id.cmp(&b.session_id))
        });
        live_sessions
    }

    match request {
        IpcRequest::Status => {
            let servers = ctx.server_manager.server_statuses();
            IpcResponse::Status {
                servers,
                clients: ctx.client_registry.count(),
                uptime_secs: ctx.started_at.elapsed().as_secs(),
            }
        }
        IpcRequest::RestartServer { server_id, .. } => {
            match ctx.engine.restart_server(server_id).await {
                Ok(()) => IpcResponse::Ok,
                Err(e) => IpcResponse::Error {
                    code: "RESTART_FAILED".to_string(),
                    message: e.to_string(),
                },
            }
        }
        IpcRequest::Reload { .. } => {
            // Load fresh config from disk and apply via Engine
            match plug_core::config::load_config(Some(&ctx.config_path)) {
                Ok(new_config) => match ctx.engine.reload_config(new_config).await {
                    Ok(report) => {
                        tracing::info!(
                            added = report.added.len(),
                            removed = report.removed.len(),
                            changed = report.changed.len(),
                            "config reloaded via IPC"
                        );
                        IpcResponse::Ok
                    }
                    Err(e) => IpcResponse::Error {
                        code: "RELOAD_FAILED".to_string(),
                        message: e.to_string(),
                    },
                },
                Err(e) => IpcResponse::Error {
                    code: "CONFIG_LOAD_FAILED".to_string(),
                    message: e.to_string(),
                },
            }
        }
        IpcRequest::Shutdown { .. } => IpcResponse::Ok,

        IpcRequest::Register {
            protocol_version,
            client_id,
            client_info,
        } => {
            if *protocol_version != plug_core::ipc::IPC_PROTOCOL_VERSION {
                return IpcResponse::Error {
                    code: "PROTOCOL_VERSION_UNSUPPORTED".to_string(),
                    message: format!(
                        "daemon supports IPC protocol v{}, got v{}",
                        plug_core::ipc::IPC_PROTOCOL_VERSION,
                        protocol_version
                    ),
                };
            }
            // Enforce one registration per connection — deregister previous if exists
            if let Some(ref old_session) = ctx.session_id {
                ctx.client_registry.deregister(old_session);
            }
            let registration = match ctx
                .client_registry
                .try_register(
                    client_id.clone(),
                    client_info.clone(),
                    MAX_REGISTERED_PROXY_CLIENTS,
                ) {
                Ok(registration) => registration,
                Err(()) => {
                    tracing::warn!(
                        client_id = %client_id,
                        max_registered_proxy_clients = MAX_REGISTERED_PROXY_CLIENTS,
                        "proxy client registration rejected: max registered sessions reached"
                    );
                    return IpcResponse::Error {
                        code: "MAX_CONNECTIONS_REACHED".to_string(),
                        message: format!(
                            "maximum registered proxy sessions reached ({MAX_REGISTERED_PROXY_CLIENTS})"
                        ),
                    };
                }
            };
            if let Some(ref replaced_session_id) = registration.replaced_session_id {
                tracing::info!(
                    client_id = %client_id,
                    replaced_session_id = %replaced_session_id,
                    new_session_id = %registration.session_id,
                    "client transport session replaced"
                );
            }
            let session_id = registration.session_id;
            ctx.session_id = Some(session_id.clone());

            // Create reverse-request channel and register the daemon bridge
            // so upstream servers can forward elicitation/sampling requests
            // to this IPC proxy client.
            let (reverse_tx, reverse_rx) = tokio::sync::mpsc::channel(8);
            let bridge = Arc::new(DaemonBridge {
                session_id: Arc::from(session_id.as_str()),
                reverse_request_tx: reverse_tx,
                client_registry: Arc::clone(&ctx.client_registry),
            });
            ctx.engine.tool_router().register_downstream_bridge(
                plug_core::notifications::NotificationTarget::Stdio {
                    client_id: Arc::from(session_id.as_str()),
                },
                bridge,
            );
            ctx.reverse_request_rx = Some(reverse_rx);

            IpcResponse::Registered {
                protocol_version: plug_core::ipc::IPC_PROTOCOL_VERSION,
                client_id: client_id.clone(),
                session_id,
            }
        }

        IpcRequest::Deregister { session_id } => {
            // Enforce session ownership — only deregister your own session
            if ctx.session_id.as_deref() != Some(session_id.as_str()) {
                return IpcResponse::Error {
                    code: "SESSION_MISMATCH".to_string(),
                    message: "session_id does not match this connection".to_string(),
                };
            }
            if !ctx.client_registry.session_exists(session_id) {
                return IpcResponse::Error {
                    code: "SESSION_REPLACED".to_string(),
                    message: "session is no longer active for this client".to_string(),
                };
            }
            ctx.client_registry.deregister(session_id);
            let target = plug_core::notifications::NotificationTarget::Stdio {
                client_id: std::sync::Arc::from(session_id.as_str()),
            };
            ctx.engine
                .tool_router()
                .unregister_downstream_bridge(&target);
            if ctx.engine.tool_router().clear_roots_for_target(&target) {
                ctx.engine
                    .tool_router()
                    .forward_roots_list_changed_to_upstreams()
                    .await;
            }
            ctx.engine.tool_router().remove_client_log_level(session_id);
            ctx.session_id = None;
            ctx.reverse_request_rx = None;
            IpcResponse::Ok
        }

        IpcRequest::UpdateSession {
            session_id,
            client_info,
        } => {
            // Enforce session ownership
            if ctx.session_id.as_deref() != Some(session_id.as_str()) {
                return IpcResponse::Error {
                    code: "SESSION_MISMATCH".to_string(),
                    message: "session_id does not match this connection".to_string(),
                };
            }
            if !ctx.client_registry.session_exists(session_id) {
                return IpcResponse::Error {
                    code: "SESSION_REPLACED".to_string(),
                    message: "session is no longer active for this client".to_string(),
                };
            }
            if ctx
                .client_registry
                .update_client_info(session_id, client_info.clone())
            {
                tracing::info!(
                    session_id = %session_id,
                    client_info = %client_info,
                    "session updated with client info"
                );
                IpcResponse::Ok
            } else {
                IpcResponse::Error {
                    code: "UNKNOWN_SESSION".to_string(),
                    message: "session not found".to_string(),
                }
            }
        }

        IpcRequest::Ping { session_id } => {
            if ctx.session_id.as_deref() != Some(session_id.as_str()) {
                return IpcResponse::Error {
                    code: "SESSION_MISMATCH".to_string(),
                    message: "session_id does not match this connection".to_string(),
                };
            }
            if !ctx.client_registry.session_exists(session_id) {
                return IpcResponse::Error {
                    code: "SESSION_REPLACED".to_string(),
                    message: "session is no longer active for this client".to_string(),
                };
            }
            IpcResponse::Pong
        }

        IpcRequest::ListTools => {
            let tool_router = ctx.engine.tool_router();
            let tools = tool_router.list_all_tools();
            let ipc_tools = tools
                .into_iter()
                .map(|(server_id, tool)| plug_core::ipc::IpcToolInfo {
                    name: tool.name.to_string(),
                    server_id,
                    description: tool.description.map(|d| d.to_string()),
                    title: tool.title.clone(),
                })
                .collect();
            IpcResponse::Tools { tools: ipc_tools }
        }
        IpcRequest::ListClients => IpcResponse::Clients {
            clients: ctx.client_registry.list(),
        },
        IpcRequest::ListLiveSessions => IpcResponse::LiveSessions {
            sessions: {
                let mut sessions = ctx.client_registry.list_live_sessions();
                if let Some(http_sessions) = ctx.http_sessions.as_ref() {
                    sessions.extend(downstream_http_live_sessions(http_sessions.as_ref()));
                }
                sessions.sort_by(|a, b| {
                    let transport_order =
                        |transport: plug_core::ipc::LiveSessionTransport| match transport {
                            plug_core::ipc::LiveSessionTransport::DaemonProxy => 0,
                            plug_core::ipc::LiveSessionTransport::Http => 1,
                            plug_core::ipc::LiveSessionTransport::Sse => 2,
                        };
                    transport_order(a.transport)
                        .cmp(&transport_order(b.transport))
                        .then(a.client_type.to_string().cmp(&b.client_type.to_string()))
                        .then(a.session_id.cmp(&b.session_id))
                });
                sessions
            },
            scope: if ctx.http_sessions.is_some() {
                plug_core::ipc::LiveSessionInventoryScope::TransportComplete
            } else {
                plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly
            },
        },
        IpcRequest::Capabilities { session_id } => {
            if ctx.session_id.as_deref() != Some(session_id.as_str()) {
                return IpcResponse::Error {
                    code: "SESSION_MISMATCH".to_string(),
                    message: "session_id does not match this connection".to_string(),
                };
            }
            if !ctx.client_registry.session_exists(session_id) {
                return IpcResponse::Error {
                    code: "SESSION_REPLACED".to_string(),
                    message: "session is no longer active for this client".to_string(),
                };
            }
            let mut caps = ctx.engine.tool_router().synthesized_capabilities();
            // IPC clients cannot subscribe to resources (no long-lived push
            // channel for targeted ResourceUpdated delivery), so mask that.
            if let Some(ref mut resources) = caps.resources {
                resources.subscribe = None;
            }
            match serde_json::to_value(caps) {
                Ok(capabilities) => IpcResponse::Capabilities { capabilities },
                Err(error) => IpcResponse::Error {
                    code: "SERIALIZE_ERROR".to_string(),
                    message: error.to_string(),
                },
            }
        }

        IpcRequest::UpdateRoots { session_id, roots } => {
            // Enforce session ownership
            if ctx.session_id.as_deref() != Some(session_id.as_str()) {
                return IpcResponse::Error {
                    code: "SESSION_MISMATCH".to_string(),
                    message: "session_id does not match this connection".to_string(),
                };
            }
            if !ctx.client_registry.session_exists(session_id) {
                return IpcResponse::Error {
                    code: "SESSION_REPLACED".to_string(),
                    message: "session is no longer active for this client".to_string(),
                };
            }
            match serde_json::from_value::<Vec<rmcp::model::Root>>(roots.clone()) {
                Ok(parsed_roots) => {
                    let target = plug_core::notifications::NotificationTarget::Stdio {
                        client_id: std::sync::Arc::from(session_id.as_str()),
                    };
                    if ctx
                        .engine
                        .tool_router()
                        .set_roots_for_target(target, parsed_roots)
                    {
                        ctx.engine
                            .tool_router()
                            .forward_roots_list_changed_to_upstreams()
                            .await;
                    }
                    IpcResponse::Ok
                }
                Err(error) => IpcResponse::Error {
                    code: "INVALID_ROOTS".to_string(),
                    message: error.to_string(),
                },
            }
        }

        IpcRequest::UpdateCapabilities {
            session_id,
            capabilities,
        } => {
            // Enforce session ownership
            if ctx.session_id.as_deref() != Some(session_id.as_str()) {
                return IpcResponse::Error {
                    code: "SESSION_MISMATCH".to_string(),
                    message: "session_id does not match this connection".to_string(),
                };
            }
            if !ctx.client_registry.session_exists(session_id) {
                return IpcResponse::Error {
                    code: "SESSION_REPLACED".to_string(),
                    message: "session is no longer active for this client".to_string(),
                };
            }
            if ctx
                .client_registry
                .update_capabilities(session_id, *capabilities.clone())
            {
                tracing::info!(
                    session_id = %session_id,
                    "session capabilities updated"
                );
                IpcResponse::Ok
            } else {
                IpcResponse::Error {
                    code: "UNKNOWN_SESSION".to_string(),
                    message: "session not found".to_string(),
                }
            }
        }

        IpcRequest::McpRequest {
            session_id,
            method,
            params,
        } => {
            // Enforce session ownership
            if ctx.session_id.as_deref() != Some(session_id.as_str()) {
                return IpcResponse::Error {
                    code: "SESSION_MISMATCH".to_string(),
                    message: "session_id does not match this connection".to_string(),
                };
            }
            if !ctx.client_registry.session_exists(session_id) {
                return IpcResponse::Error {
                    code: "SESSION_REPLACED".to_string(),
                    message: "session is no longer active for this client".to_string(),
                };
            }
            dispatch_mcp_request(ctx, session_id, method, params.as_ref()).await
        }

        IpcRequest::AuthStatus => dispatch_auth_status(ctx).await,

        IpcRequest::InjectToken {
            server_name,
            access_token,
            refresh_token,
            expires_in,
            ..
        } => dispatch_inject_token(ctx, server_name, access_token, refresh_token, expires_in).await,
    }
}

/// Handle `AuthStatus` — return per-server OAuth state from config + credential stores.
async fn dispatch_auth_status(ctx: &ConnectionContext) -> IpcResponse {
    use plug_core::oauth;

    let config = plug_core::config::load_config(Some(&ctx.config_path));
    let config = match config {
        Ok(cfg) => cfg,
        Err(e) => {
            return IpcResponse::Error {
                code: "CONFIG_LOAD_FAILED".to_string(),
                message: e.to_string(),
            };
        }
    };

    // Get runtime health from server manager
    let statuses = ctx.server_manager.server_statuses();
    let status_map: std::collections::HashMap<&str, &plug_core::types::ServerStatus> =
        statuses.iter().map(|s| (s.server_id.as_str(), s)).collect();

    let mut oauth_servers: Vec<_> = config
        .servers
        .iter()
        .filter(|(_, sc)| sc.auth.as_deref() == Some("oauth"))
        .collect();
    oauth_servers.sort_by_key(|(name, _)| (*name).clone());

    let mut servers = Vec::new();
    for (name, sc) in &oauth_servers {
        let store = oauth::get_or_create_store(name);
        let snapshot = store.credential_snapshot();
        let has_creds = snapshot.credentials.is_some();

        let health = status_map
            .get(name.as_str())
            .map(|s| s.health)
            .unwrap_or_else(|| {
                if has_creds {
                    plug_core::types::ServerHealth::Degraded
                } else {
                    plug_core::types::ServerHealth::AuthRequired
                }
            });

        servers.push(plug_core::ipc::IpcAuthServerInfo {
            name: (*name).clone(),
            url: sc.url.clone(),
            authenticated: has_creds,
            health,
            scopes: sc.oauth_scopes.clone(),
            token_expires_in_secs: snapshot.token_expires_in_secs,
            warnings: snapshot.warnings,
        });
    }

    IpcResponse::AuthStatus { servers }
}

/// Handle `InjectToken` — save credentials and trigger server reconnect.
async fn dispatch_inject_token(
    ctx: &ConnectionContext,
    server_name: &str,
    access_token: &str,
    refresh_token: &Option<String>,
    expires_in: &Option<u64>,
) -> IpcResponse {
    use oauth2::{AccessToken, RefreshToken, basic::BasicTokenType};
    use plug_core::oauth;
    use rmcp::transport::auth::{CredentialStore, StoredCredentials, VendorExtraTokenFields};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Verify server exists and is OAuth-configured
    let config = match plug_core::config::load_config(Some(&ctx.config_path)) {
        Ok(cfg) => cfg,
        Err(e) => {
            return IpcResponse::Error {
                code: "CONFIG_LOAD_FAILED".to_string(),
                message: e.to_string(),
            };
        }
    };
    match config.servers.get(server_name) {
        Some(sc) if sc.auth.as_deref() == Some("oauth") => {}
        Some(_) => {
            return IpcResponse::Error {
                code: "NOT_OAUTH_SERVER".to_string(),
                message: format!("server '{server_name}' is not configured for OAuth"),
            };
        }
        None => {
            return IpcResponse::Error {
                code: "UNKNOWN_SERVER".to_string(),
                message: format!("server '{server_name}' not found in config"),
            };
        }
    }

    // Build and save credentials
    let store = oauth::get_or_create_store(server_name);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut token = oauth2::StandardTokenResponse::<VendorExtraTokenFields, BasicTokenType>::new(
        AccessToken::new(access_token.to_string()),
        BasicTokenType::Bearer,
        VendorExtraTokenFields::default(),
    );

    if let Some(rt) = refresh_token {
        token.set_refresh_token(Some(RefreshToken::new(rt.clone())));
    }
    if let Some(secs) = expires_in {
        token.set_expires_in(Some(&std::time::Duration::from_secs(*secs)));
    }

    let stored = StoredCredentials {
        client_id: "injected".to_string(),
        token_response: Some(token),
        granted_scopes: vec![],
        token_received_at: Some(now),
    };

    if let Err(e) = store.save(stored).await {
        return IpcResponse::Error {
            code: "CREDENTIAL_SAVE_FAILED".to_string(),
            message: e.to_string(),
        };
    }

    // Trigger server reconnect to pick up new credentials
    match ctx.engine.restart_server(server_name).await {
        Ok(()) => {
            tracing::info!(server = %server_name, "credentials injected and server restarted via IPC");
            // Notify IPC clients of the auth state change (→ Healthy)
            ctx.engine.tool_router().publish_protocol_notification(
                plug_core::notifications::ProtocolNotification::AuthStateChanged {
                    server_id: std::sync::Arc::from(server_name),
                    new_state: plug_core::types::ServerHealth::Healthy,
                },
            );
            IpcResponse::Ok
        }
        Err(e) => {
            tracing::warn!(server = %server_name, error = %e, "credentials injected but server restart failed");
            IpcResponse::Error {
                code: "RESTART_FAILED".to_string(),
                message: format!(
                    "credentials saved but server restart failed: {e}. \
                     The server may recover on next health check."
                ),
            }
        }
    }
}

fn protocol_parse_error_response(frame: &[u8]) -> Option<IpcResponse> {
    let value: serde_json::Value = serde_json::from_slice(frame).ok()?;
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("Register")
            if value.get("protocol_version").is_none() || value.get("client_id").is_none() =>
        {
            Some(IpcResponse::Error {
                code: "PROTOCOL_VERSION_UNSUPPORTED".to_string(),
                message: format!(
                    "daemon requires IPC protocol v{} registration fields",
                    plug_core::ipc::IPC_PROTOCOL_VERSION
                ),
            })
        }
        _ => None,
    }
}

/// Dispatch an MCP JSON-RPC request through the daemon's shared ToolRouter.
async fn dispatch_mcp_request(
    ctx: &ConnectionContext,
    session_id: &str,
    method: &str,
    params: Option<&serde_json::Value>,
) -> IpcResponse {
    let tool_router = ctx.engine.tool_router();

    match method {
        "tools/list" => {
            // Determine client type from session's client_info
            let client_type = ctx
                .client_registry
                .client_info(session_id)
                .map(|info| plug_core::client_detect::detect_client(&info))
                .unwrap_or(plug_core::types::ClientType::Unknown);

            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_tools_page_for_client(client_type, request);
            match serde_json::to_value(result) {
                Ok(payload) => IpcResponse::McpResponse { payload },
                Err(e) => IpcResponse::Error {
                    code: "SERIALIZE_ERROR".to_string(),
                    message: e.to_string(),
                },
            }
        }

        "resources/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_resources_page(request);
            match serde_json::to_value(result) {
                Ok(payload) => IpcResponse::McpResponse { payload },
                Err(e) => IpcResponse::Error {
                    code: "SERIALIZE_ERROR".to_string(),
                    message: e.to_string(),
                },
            }
        }

        "resources/templates/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_resource_templates_page(request);
            match serde_json::to_value(result) {
                Ok(payload) => IpcResponse::McpResponse { payload },
                Err(e) => IpcResponse::Error {
                    code: "SERIALIZE_ERROR".to_string(),
                    message: e.to_string(),
                },
            }
        }

        "resources/read" => {
            let uri = match params.and_then(|p| p.get("uri")).and_then(|v| v.as_str()) {
                Some(uri) => uri,
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "resources/read requires 'uri' in params".to_string(),
                    };
                }
            };

            match tool_router.read_resource(uri).await {
                Ok(result) => match serde_json::to_value(result) {
                    Ok(payload) => IpcResponse::McpResponse { payload },
                    Err(e) => IpcResponse::Error {
                        code: "SERIALIZE_ERROR".to_string(),
                        message: e.to_string(),
                    },
                },
                Err(mcp_err) => match serde_json::to_value(&mcp_err) {
                    Ok(payload) => IpcResponse::McpResponse { payload },
                    Err(e) => IpcResponse::Error {
                        code: "SERIALIZE_ERROR".to_string(),
                        message: e.to_string(),
                    },
                },
            }
        }

        "prompts/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_prompts_page(request);
            match serde_json::to_value(result) {
                Ok(payload) => IpcResponse::McpResponse { payload },
                Err(e) => IpcResponse::Error {
                    code: "SERIALIZE_ERROR".to_string(),
                    message: e.to_string(),
                },
            }
        }

        "prompts/get" => {
            let name = match params.and_then(|p| p.get("name")).and_then(|v| v.as_str()) {
                Some(name) if !name.is_empty() => name,
                _ => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "prompts/get requires non-empty 'name'".to_string(),
                    };
                }
            };
            let arguments = params
                .and_then(|p| p.get("arguments"))
                .and_then(|v| v.as_object())
                .cloned();

            match tool_router.get_prompt(name, arguments).await {
                Ok(result) => match serde_json::to_value(result) {
                    Ok(payload) => IpcResponse::McpResponse { payload },
                    Err(e) => IpcResponse::Error {
                        code: "SERIALIZE_ERROR".to_string(),
                        message: e.to_string(),
                    },
                },
                Err(mcp_err) => match serde_json::to_value(&mcp_err) {
                    Ok(payload) => IpcResponse::McpResponse { payload },
                    Err(e) => IpcResponse::Error {
                        code: "SERIALIZE_ERROR".to_string(),
                        message: e.to_string(),
                    },
                },
            }
        }

        "completion/complete" => {
            let params: rmcp::model::CompleteRequestParams = match params
                .map(|p| serde_json::from_value::<rmcp::model::CompleteRequestParams>(p.clone()))
            {
                Some(Ok(p)) => p,
                Some(Err(e)) => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: format!("completion/complete: {e}"),
                    };
                }
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "completion/complete requires params".to_string(),
                    };
                }
            };

            match tool_router.complete_request(params).await {
                Ok(result) => match serde_json::to_value(result) {
                    Ok(payload) => IpcResponse::McpResponse { payload },
                    Err(e) => IpcResponse::Error {
                        code: "SERIALIZE_ERROR".to_string(),
                        message: e.to_string(),
                    },
                },
                Err(mcp_err) => match serde_json::to_value(&mcp_err) {
                    Ok(payload) => IpcResponse::McpResponse { payload },
                    Err(e) => IpcResponse::Error {
                        code: "SERIALIZE_ERROR".to_string(),
                        message: e.to_string(),
                    },
                },
            }
        }

        "logging/setLevel" => {
            let level = match params.and_then(|p| p.get("level")).and_then(|v| v.as_str()) {
                Some(level_str) => {
                    match serde_json::from_value::<rmcp::model::LoggingLevel>(serde_json::json!(
                        level_str
                    )) {
                        Ok(level) => level,
                        Err(_) => {
                            return IpcResponse::Error {
                                code: "INVALID_PARAMS".to_string(),
                                message: format!("invalid logging level: {level_str}"),
                            };
                        }
                    }
                }
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "logging/setLevel requires 'level' in params".to_string(),
                    };
                }
            };

            tracing::info!(
                session_id = %session_id,
                level = ?level,
                "IPC client set log level"
            );
            tool_router.set_client_log_level(session_id, level);
            tool_router.forward_set_level_to_upstreams().await;
            match serde_json::to_value(serde_json::json!({})) {
                Ok(payload) => IpcResponse::McpResponse { payload },
                Err(e) => IpcResponse::Error {
                    code: "SERIALIZE_ERROR".to_string(),
                    message: e.to_string(),
                },
            }
        }

        "tools/call" => {
            // Extract tool name and arguments from params
            let (name, arguments) = match params {
                Some(p) => {
                    let name = p
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = p.get("arguments").and_then(|v| v.as_object()).cloned();
                    (name, arguments)
                }
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "tools/call requires 'name' in params".to_string(),
                    };
                }
            };

            if name.is_empty() {
                return IpcResponse::Error {
                    code: "INVALID_PARAMS".to_string(),
                    message: "tools/call requires non-empty 'name'".to_string(),
                };
            }

            // Build downstream context so the ToolRouter can route reverse
            // requests (elicitation, sampling) back to this IPC client.
            let downstream_ctx = {
                use plug_core::proxy::DownstreamCallContext;
                use rmcp::model::NumberOrString;
                // Use a synthetic request ID — the IPC protocol doesn't carry JSON-RPC IDs,
                // but the context needs one for active call tracking.
                let request_id = RequestId::from(NumberOrString::String(Arc::from(
                    format!("ipc-{session_id}-{}", uuid::Uuid::new_v4()).as_str(),
                )));
                Some(DownstreamCallContext::stdio(
                    Arc::from(session_id),
                    request_id,
                ))
            };

            match tool_router
                .call_tool_with_context(&name, arguments, None, downstream_ctx)
                .await
            {
                Ok(result) => match serde_json::to_value(result) {
                    Ok(payload) => IpcResponse::McpResponse { payload },
                    Err(e) => IpcResponse::Error {
                        code: "SERIALIZE_ERROR".to_string(),
                        message: e.to_string(),
                    },
                },
                Err(mcp_err) => match serde_json::to_value(&mcp_err) {
                    Ok(payload) => IpcResponse::McpResponse { payload },
                    Err(e) => IpcResponse::Error {
                        code: "SERIALIZE_ERROR".to_string(),
                        message: e.to_string(),
                    },
                },
            }
        }

        "resources/subscribe" | "resources/unsubscribe" => IpcResponse::Error {
            code: "UNSUPPORTED_METHOD".to_string(),
            message: format!(
                "'{method}' not supported via IPC proxy (no push channel for notifications)"
            ),
        },

        _ => IpcResponse::Error {
            code: "UNSUPPORTED_METHOD".to_string(),
            message: format!("MCP method '{method}' not supported via IPC proxy"),
        },
    }
}

// ──────────────────────── Unix signal handling ───────────────────────────────

/// Wait for SIGTERM or SIGINT (for daemon mode).
///
/// Systemd sends SIGTERM for graceful shutdown, not SIGINT.
#[cfg(unix)]
pub async fn shutdown_signal(cancel: CancellationToken) {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");
    tokio::select! {
        _ = sigterm.recv() => {
            tracing::info!("received SIGTERM");
        }
        _ = sigint.recv() => {
            tracing::info!("received SIGINT");
        }
        _ = cancel.cancelled() => {}
    }
    cancel.cancel();
}

/// Listen for SIGHUP and trigger config reload.
///
/// Runs in a loop — each SIGHUP triggers a reload. Exits when cancellation
/// token is triggered.
#[cfg(unix)]
#[allow(dead_code)]
pub async fn sighup_reload(engine: Arc<Engine>, cancel: CancellationToken) {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sighup = signal(SignalKind::hangup()).expect("failed to install SIGHUP handler");
    loop {
        tokio::select! {
            _ = sighup.recv() => {
                tracing::info!("received SIGHUP — reloading config");
                let config_path = plug_core::config::default_config_path();
                match plug_core::config::load_config(Some(&config_path)) {
                    Ok(new_config) => match engine.reload_config(new_config).await {
                        Ok(report) => {
                            tracing::info!(
                                added = report.added.len(),
                                removed = report.removed.len(),
                                changed = report.changed.len(),
                                "config reloaded via SIGHUP"
                            );
                        }
                        Err(e) => tracing::error!(error = %e, "config reload failed"),
                    },
                    Err(e) => tracing::error!(error = %e, "failed to load config for reload"),
                }
            }
            _ = cancel.cancelled() => break,
        }
    }
}

/// No-op SIGHUP handler for non-Unix platforms.
#[cfg(not(unix))]
pub async fn sighup_reload(_engine: Arc<Engine>, cancel: CancellationToken) {
    cancel.cancelled().await;
}

/// Fallback for non-Unix platforms.
#[cfg(not(unix))]
pub async fn shutdown_signal(cancel: CancellationToken) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received Ctrl+C");
        }
        _ = cancel.cancelled() => {}
    }
    cancel.cancel();
}

// ──────────────────────── File logging setup ─────────────────────────────────

/// Set up file logging with daily rotation for daemon mode.
///
/// Returns the non-blocking guard that MUST be held for the daemon's lifetime.
/// Must be called BEFORE any other tracing subscriber is initialized.
pub fn setup_file_logging(
    log_directory: &std::path::Path,
) -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    ensure_dir(log_directory)?;

    let file_appender = tracing_appender::rolling::daily(log_directory, "plug.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = tracing_subscriber::EnvFilter::try_from_env("PLUG_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .json()
        .init();

    Ok(guard)
}

// ──────────────────────── Client helpers ──────────────────────────────────────

/// Connect to a running daemon's Unix socket.
///
/// Returns None if no daemon is running (socket doesn't exist or connection refused).
pub async fn connect_to_daemon() -> Option<tokio::net::UnixStream> {
    let sock = socket_path();
    if !sock.exists() {
        return None;
    }
    tokio::net::UnixStream::connect(&sock).await.ok()
}

/// Send a request to the daemon and read the response.
pub async fn ipc_request(request: &IpcRequest) -> anyhow::Result<IpcResponse> {
    let stream = connect_to_daemon()
        .await
        .ok_or_else(|| anyhow::anyhow!("no plug daemon running"))?;

    let (mut reader, mut writer) = stream.into_split();

    // Send request
    let payload = serde_json::to_vec(request)?;
    ipc::write_frame(&mut writer, &payload).await?;

    // Read response
    let frame = ipc::read_frame(&mut reader)
        .await?
        .ok_or_else(|| anyhow::anyhow!("daemon closed connection"))?;

    let response: IpcResponse = serde_json::from_slice(&frame)?;
    Ok(response)
}

/// Read the auth token from the daemon's token file.
pub fn read_auth_token() -> anyhow::Result<String> {
    let path = token_path();
    std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read auth token from {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oauth2::{AccessToken, RefreshToken, basic::BasicTokenType};
    use rmcp::transport::auth::{CredentialStore, StoredCredentials, VendorExtraTokenFields};
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::AsyncWriteExt;

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("plug-daemon-{name}-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    async fn clear_store(server_name: &str) {
        let store = plug_core::oauth::get_or_create_store(server_name);
        store.clear().await.unwrap();
    }

    fn cleanup_temp_config(config_path: &std::path::Path) {
        if let Some(dir) = config_path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn write_oauth_config(path: &std::path::Path, servers: &[&str]) {
        let mut config = plug_core::config::Config::default();
        for name in servers {
            config.servers.insert(
                (*name).to_string(),
                plug_core::config::ServerConfig {
                    command: None,
                    args: Vec::new(),
                    env: HashMap::new(),
                    enabled: true,
                    transport: plug_core::config::TransportType::Http,
                    url: Some("https://example.com/mcp".to_string()),
                    auth_token: None,
                    auth: Some("oauth".to_string()),
                    oauth_client_id: Some("test-client".to_string()),
                    oauth_scopes: Some(vec!["read".to_string()]),
                    timeout_secs: 30,
                    call_timeout_secs: 30,
                    max_concurrent: 4,
                    health_check_interval_secs: 60,
                    circuit_breaker_enabled: false,
                    enrichment: false,
                    tool_renames: HashMap::new(),
                    tool_groups: Vec::new(),
                },
            );
        }
        std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
    }

    fn seeded_credentials() -> StoredCredentials {
        let mut token =
            oauth2::StandardTokenResponse::<VendorExtraTokenFields, BasicTokenType>::new(
                AccessToken::new("access-token".to_string()),
                BasicTokenType::Bearer,
                VendorExtraTokenFields::default(),
            );
        token.set_refresh_token(Some(RefreshToken::new("refresh-token".to_string())));
        token.set_expires_in(Some(&Duration::from_secs(3600)));

        StoredCredentials {
            client_id: "test-client".to_string(),
            token_response: Some(token),
            granted_scopes: vec!["read".to_string()],
            token_received_at: Some(0),
        }
    }

    fn auth_status_test_context(config_path: std::path::PathBuf) -> ConnectionContext {
        let config = plug_core::config::load_config(Some(&config_path)).unwrap();
        let engine = Arc::new(Engine::new(config));
        let (client_registry, _count_rx) = ClientRegistry::new();
        ConnectionContext {
            cancel: CancellationToken::new(),
            auth_token: Arc::from("test-token"),
            server_manager: Arc::clone(engine.server_manager()),
            engine,
            config_path,
            started_at: Instant::now(),
            client_registry: Arc::new(client_registry),
            http_sessions: None,
            session_id: None,
            reverse_request_rx: None,
        }
    }

    #[tokio::test]
    async fn list_live_sessions_includes_http_sessions_when_daemon_owns_http() {
        let temp = std::env::temp_dir().join(format!(
            "plug-daemon-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temp).expect("create temp dir");
        let config_path = temp.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let mut ctx = auth_status_test_context(config_path);
        let session_store = plug_core::session::StatefulSessionStore::new(1800, 100);
        let session_id = session_store.create_session().expect("session");
        session_store
            .set_client_type(&session_id, plug_core::types::ClientType::ClaudeDesktop)
            .expect("set client type");
        ctx.http_sessions = Some(Arc::new(session_store));

        let response = dispatch_request(&IpcRequest::ListLiveSessions, &mut ctx).await;
        let IpcResponse::LiveSessions { sessions, scope } = response else {
            panic!("expected live sessions response");
        };

        assert_eq!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete
        );
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].transport,
            plug_core::ipc::LiveSessionTransport::Http
        );
        assert_eq!(
            sessions[0].client_type,
            plug_core::types::ClientType::ClaudeDesktop
        );
        std::fs::remove_dir_all(temp).expect("cleanup temp dir");
    }

    #[test]
    fn socket_path_is_in_runtime_dir() {
        let sock = socket_path();
        let rt = runtime_dir();
        assert!(sock.starts_with(&rt));
        assert!(sock.to_string_lossy().ends_with("plug.sock"));
    }

    #[test]
    fn pid_path_is_in_runtime_dir() {
        let pid = pid_path();
        let rt = runtime_dir();
        assert!(pid.starts_with(&rt));
        assert!(pid.to_string_lossy().ends_with("plug.pid"));
    }

    #[test]
    fn token_path_is_in_runtime_dir() {
        let tok = token_path();
        let rt = runtime_dir();
        assert!(tok.starts_with(&rt));
        assert!(tok.to_string_lossy().ends_with("plug.token"));
    }

    #[tokio::test]
    async fn auth_status_without_credentials_reports_auth_required() {
        let config_path = temp_config_path("auth-status-missing");
        let server_name = format!("oauth-missing-{}", std::process::id());
        write_oauth_config(&config_path, &[server_name.as_str()]);

        clear_store(&server_name).await;

        let ctx = auth_status_test_context(config_path);
        let response = dispatch_auth_status(&ctx).await;
        let IpcResponse::AuthStatus { servers } = response else {
            panic!("expected auth status response");
        };

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, server_name);
        assert!(!servers[0].authenticated);
        assert_eq!(
            servers[0].health,
            plug_core::types::ServerHealth::AuthRequired
        );
        assert!(servers[0].token_expires_in_secs.is_none());
        assert!(servers[0].warnings.is_empty());

        clear_store(&server_name).await;
        cleanup_temp_config(&ctx.config_path);
    }

    #[tokio::test]
    async fn auth_status_with_credentials_and_no_runtime_reports_degraded() {
        let config_path = temp_config_path("auth-status-degraded");
        let server_name = format!("oauth-degraded-{}", std::process::id());
        write_oauth_config(&config_path, &[server_name.as_str()]);

        let store = plug_core::oauth::get_or_create_store(&server_name);
        clear_store(&server_name).await;
        store.save(seeded_credentials()).await.unwrap();

        let ctx = auth_status_test_context(config_path);
        let response = dispatch_auth_status(&ctx).await;
        let IpcResponse::AuthStatus { servers } = response else {
            panic!("expected auth status response");
        };

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, server_name);
        assert!(servers[0].authenticated);
        assert_eq!(servers[0].health, plug_core::types::ServerHealth::Degraded);
        assert!(servers[0].token_expires_in_secs.is_some());
        assert_eq!(servers[0].warnings, store.backing_store_warnings());

        clear_store(&server_name).await;
        cleanup_temp_config(&ctx.config_path);
    }

    #[tokio::test]
    async fn auth_status_prefers_runtime_auth_required_over_cached_credentials() {
        let config_path = temp_config_path("auth-status-runtime");
        let server_name = format!("oauth-runtime-{}", std::process::id());
        write_oauth_config(&config_path, &[server_name.as_str()]);

        let store = plug_core::oauth::get_or_create_store(&server_name);
        clear_store(&server_name).await;
        store.save(seeded_credentials()).await.unwrap();

        let ctx = auth_status_test_context(config_path);
        ctx.server_manager.mark_auth_required(&server_name);

        let response = dispatch_auth_status(&ctx).await;
        let IpcResponse::AuthStatus { servers } = response else {
            panic!("expected auth status response");
        };

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, server_name);
        assert!(servers[0].authenticated);
        assert_eq!(
            servers[0].health,
            plug_core::types::ServerHealth::AuthRequired
        );
        assert_eq!(servers[0].warnings, store.backing_store_warnings());

        clear_store(&server_name).await;
        cleanup_temp_config(&ctx.config_path);
    }

    #[tokio::test]
    async fn length_prefixed_frame_round_trip() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (_r1, mut w1) = client.into_split();
        let (mut r2, _w2) = server.into_split();

        let payload = b"hello world";

        let write_task = tokio::spawn(async move {
            ipc::write_frame(&mut w1, payload).await.unwrap();
        });

        let read_task = tokio::spawn(async move {
            let data = ipc::read_frame(&mut r2).await.unwrap().unwrap();
            assert_eq!(data, payload);
        });

        write_task.await.unwrap();
        read_task.await.unwrap();
    }

    #[tokio::test]
    async fn frame_too_large_rejected() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (_r1, mut w1) = client.into_split();
        let (mut r2, _w2) = server.into_split();

        // Write a length prefix that exceeds MAX_FRAME_SIZE
        use plug_core::ipc::MAX_FRAME_SIZE;
        w1.write_u32(MAX_FRAME_SIZE + 1).await.unwrap();

        let result = ipc::read_frame(&mut r2).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("frame too large"));
    }

    #[tokio::test]
    async fn ipc_request_response_over_socket() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (mut r_client, mut w_client) = client.into_split();
        let (mut r_server, mut w_server) = server.into_split();

        // Client sends Status request
        let request = IpcRequest::Status;
        let payload = serde_json::to_vec(&request).unwrap();

        let client_task = tokio::spawn(async move {
            ipc::write_frame(&mut w_client, &payload).await.unwrap();

            // Read response
            let frame = ipc::read_frame(&mut r_client).await.unwrap().unwrap();
            let resp: IpcResponse = serde_json::from_slice(&frame).unwrap();
            resp
        });

        // Server reads request and sends response
        let server_task = tokio::spawn(async move {
            let frame = ipc::read_frame(&mut r_server).await.unwrap().unwrap();
            let req: IpcRequest = serde_json::from_slice(&frame).unwrap();
            assert!(matches!(req, IpcRequest::Status));

            let response = IpcResponse::Status {
                servers: vec![],
                clients: 2,
                uptime_secs: 100,
            };
            ipc::send_response(&mut w_server, &response).await.unwrap();
        });

        server_task.await.unwrap();
        let resp = client_task.await.unwrap();

        match resp {
            IpcResponse::Status {
                servers,
                clients,
                uptime_secs,
            } => {
                assert!(servers.is_empty());
                assert_eq!(clients, 2);
                assert_eq!(uptime_secs, 100);
            }
            _ => panic!("expected Status response"),
        }
    }

    #[test]
    fn register_replaces_existing_session_for_same_client_id() {
        let (registry, _count_rx) = ClientRegistry::new();

        let first = registry
            .try_register(
                "client-123".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("first registration");
        let second = registry
            .try_register(
                "client-123".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("second registration");

        assert!(first.replaced_session_id.is_none());
        assert_eq!(
            second.replaced_session_id.as_deref(),
            Some(first.session_id.as_str())
        );
        assert!(!registry.session_exists(&first.session_id));
        assert!(registry.session_exists(&second.session_id));
        assert_eq!(registry.count(), 1);
    }

    #[test]
    fn deregistering_replaced_session_does_not_remove_active_replacement() {
        let (registry, _count_rx) = ClientRegistry::new();

        let first = registry
            .try_register(
                "client-123".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("first registration");
        let second = registry
            .try_register(
                "client-123".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("second registration");

        registry.deregister(&first.session_id);

        assert!(registry.session_exists(&second.session_id));
        assert_eq!(registry.count(), 1);
    }

    #[test]
    fn registration_rejects_new_client_once_cap_is_reached() {
        let (registry, _count_rx) = ClientRegistry::new();

        for i in 0..MAX_REGISTERED_PROXY_CLIENTS {
            registry
                .try_register(
                    format!("client-{i}"),
                    Some("claude-code".to_string()),
                    MAX_REGISTERED_PROXY_CLIENTS,
                )
                .expect("registration within cap");
        }

        let overflow = registry.try_register(
            "overflow-client".to_string(),
            Some("claude-code".to_string()),
            MAX_REGISTERED_PROXY_CLIENTS,
        );

        assert!(overflow.is_err(), "new client should be rejected at cap");
        assert_eq!(registry.count(), MAX_REGISTERED_PROXY_CLIENTS);
    }

    #[test]
    fn registration_allows_existing_client_to_replace_session_at_cap() {
        let (registry, _count_rx) = ClientRegistry::new();

        let original = registry
            .try_register(
                "stable-client".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("initial registration");

        for i in 1..MAX_REGISTERED_PROXY_CLIENTS {
            registry
                .try_register(
                    format!("other-client-{i}"),
                    Some("claude-code".to_string()),
                    MAX_REGISTERED_PROXY_CLIENTS,
                )
                .expect("fill cap");
        }

        let replacement = registry
            .try_register(
                "stable-client".to_string(),
                Some("claude-code".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("replacement at cap");

        assert_eq!(
            replacement.replaced_session_id.as_deref(),
            Some(original.session_id.as_str())
        );
        assert_eq!(registry.count(), MAX_REGISTERED_PROXY_CLIENTS);
        assert!(!registry.session_exists(&original.session_id));
        assert!(registry.session_exists(&replacement.session_id));
    }

    #[test]
    fn parse_error_for_legacy_register_maps_to_protocol_error() {
        let frame = serde_json::to_vec(&serde_json::json!({
            "type": "Register",
            "client_info": "claude-code"
        }))
        .unwrap();

        let response = protocol_parse_error_response(&frame).unwrap();
        assert!(matches!(
            response,
            IpcResponse::Error { ref code, .. } if code == "PROTOCOL_VERSION_UNSUPPORTED"
        ));
    }

    #[test]
    fn ping_request_round_trips_in_json() {
        let request = IpcRequest::Ping {
            session_id: "session-123".to_string(),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"type\":\"Ping\""));
        assert!(json.contains("\"session_id\":\"session-123\""));
    }

    #[tokio::test]
    async fn daemon_bridge_rejects_elicitation_without_capability() {
        use plug_core::proxy::DownstreamBridge;
        use rmcp::model::{ClientCapabilities, CreateElicitationRequestParams};

        let (registry, _count_rx) = ClientRegistry::new();
        let registry = Arc::new(registry);
        let reg_result = registry
            .try_register(
                "test-client".to_string(),
                Some("test".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("registration");
        // Capabilities default to None for elicitation/sampling

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let bridge = DaemonBridge {
            session_id: Arc::from(reg_result.session_id.as_str()),
            reverse_request_tx: tx,
            client_registry: Arc::clone(&registry),
        };

        // Build a minimal elicitation request via JSON deserialization
        let request: CreateElicitationRequestParams = serde_json::from_value(serde_json::json!({
            "message": "test",
            "requestedSchema": {
                "type": "object",
                "properties": {}
            }
        }))
        .unwrap();

        let result = bridge.create_elicitation(request).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("does not support elicitation"),
            "expected capability error, got: {}",
            err.message
        );

        // Now update capabilities to include elicitation and verify it passes the gate
        let caps_with_elicitation: ClientCapabilities =
            serde_json::from_value(serde_json::json!({ "elicitation": {} })).unwrap();
        registry.update_capabilities(&reg_result.session_id, caps_with_elicitation);

        let request2: CreateElicitationRequestParams = serde_json::from_value(serde_json::json!({
            "message": "test",
            "requestedSchema": {
                "type": "object",
                "properties": {}
            }
        }))
        .unwrap();

        // Spawn a task to consume from the channel and drop the oneshot sender,
        // which will cause the bridge to get a "channel closed" error — but
        // critically, NOT a capability-gate error.
        let drain = tokio::spawn(async move {
            if let Some((_request, _resp_tx)) = rx.recv().await {
                // Drop resp_tx so the bridge's oneshot recv returns Err
            }
        });

        let result2 = bridge.create_elicitation(request2).await;
        drain.await.unwrap();
        // The request passed the capability gate and entered the channel.
        // The oneshot was dropped, so we get a "channel closed" error.
        assert!(result2.is_err());
        let err2 = result2.unwrap_err();
        assert!(
            !err2.message.contains("does not support elicitation"),
            "should have passed capability gate, got: {}",
            err2.message
        );
    }

    #[tokio::test]
    async fn daemon_bridge_rejects_sampling_without_capability() {
        use plug_core::proxy::DownstreamBridge;
        use rmcp::model::ClientCapabilities;

        let (registry, _count_rx) = ClientRegistry::new();
        let registry = Arc::new(registry);
        let reg_result = registry
            .try_register(
                "test-client".to_string(),
                Some("test".to_string()),
                MAX_REGISTERED_PROXY_CLIENTS,
            )
            .expect("registration");

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let bridge = DaemonBridge {
            session_id: Arc::from(reg_result.session_id.as_str()),
            reverse_request_tx: tx,
            client_registry: Arc::clone(&registry),
        };

        // Build a minimal sampling request via JSON
        let request: rmcp::model::CreateMessageRequestParams =
            serde_json::from_value(serde_json::json!({
                "messages": [{"role": "user", "content": {"type": "text", "text": "hello"}}],
                "maxTokens": 100
            }))
            .unwrap();

        let result = bridge.create_message(request).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("does not support sampling"),
            "expected capability error, got: {}",
            err.message
        );

        // Now update capabilities to include sampling
        let caps_with_sampling: ClientCapabilities =
            serde_json::from_value(serde_json::json!({ "sampling": {} })).unwrap();
        registry.update_capabilities(&reg_result.session_id, caps_with_sampling);

        let request2: rmcp::model::CreateMessageRequestParams =
            serde_json::from_value(serde_json::json!({
                "messages": [{"role": "user", "content": {"type": "text", "text": "hello"}}],
                "maxTokens": 100
            }))
            .unwrap();

        // Spawn a task to consume from the channel and drop the oneshot sender
        let drain = tokio::spawn(async move {
            if let Some((_request, _resp_tx)) = rx.recv().await {
                // Drop resp_tx so the bridge's oneshot recv returns Err
            }
        });

        let result2 = bridge.create_message(request2).await;
        drain.await.unwrap();
        assert!(result2.is_err());
        let err2 = result2.unwrap_err();
        assert!(
            !err2.message.contains("does not support sampling"),
            "should have passed capability gate, got: {}",
            err2.message
        );
    }

    // ── IPC control notification forwarding tests ────────────────────────

    /// Helper: send a control notification through the daemon helper and read
    /// the IpcResponse that was written to the socket.
    async fn send_and_read_control_notification(
        notification: plug_core::notifications::ProtocolNotification,
        session_id: Option<&str>,
    ) -> Option<IpcResponse> {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (mut r_client, _w_client) = client.into_split();
        let (_r_server, mut w_server) = server.into_split();

        send_ipc_control_notification(&mut w_server, Ok(notification), session_id)
            .await
            .expect("send should not fail");

        // Drop the writer so the reader gets EOF instead of blocking
        drop(w_server);

        // Try to read a frame — None means nothing was written (filtered out)
        match ipc::read_frame(&mut r_client).await {
            Ok(Some(frame)) => Some(serde_json::from_slice(&frame).unwrap()),
            Ok(None) => None,
            Err(e) => panic!("unexpected read error: {e}"),
        }
    }

    #[tokio::test]
    async fn control_notification_broadcasts_tool_list_changed() {
        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ToolListChanged,
            Some("sess-1"),
        )
        .await;

        assert!(
            matches!(resp, Some(IpcResponse::ToolListChangedNotification)),
            "expected ToolListChangedNotification, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_broadcasts_resource_list_changed() {
        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ResourceListChanged,
            Some("sess-1"),
        )
        .await;

        assert!(
            matches!(resp, Some(IpcResponse::ResourceListChangedNotification)),
            "expected ResourceListChangedNotification, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_broadcasts_prompt_list_changed() {
        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::PromptListChanged,
            Some("sess-1"),
        )
        .await;

        assert!(
            matches!(resp, Some(IpcResponse::PromptListChangedNotification)),
            "expected PromptListChangedNotification, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_forwards_progress_for_matching_session() {
        use rmcp::model::{NumberOrString, ProgressNotificationParam, ProgressToken};

        let progress_token = ProgressToken(NumberOrString::String(Arc::from("tok-1")));
        let params = ProgressNotificationParam {
            progress_token: progress_token.clone(),
            progress: 50.0,
            total: Some(100.0),
            message: Some("halfway".to_string()),
        };

        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::Progress {
                target: plug_core::notifications::NotificationTarget::Stdio {
                    client_id: Arc::from("sess-42"),
                },
                params,
            },
            Some("sess-42"), // matches target
        )
        .await;

        match resp {
            Some(IpcResponse::ProgressNotification { params }) => {
                // Verify the serialized params contain the progress data
                assert_eq!(params["progress"], 50.0);
                assert_eq!(params["total"], 100.0);
                assert_eq!(params["message"], "halfway");
            }
            other => panic!("expected ProgressNotification, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn control_notification_filters_progress_for_different_session() {
        use rmcp::model::{NumberOrString, ProgressNotificationParam, ProgressToken};

        let progress_token = ProgressToken(NumberOrString::String(Arc::from("tok-1")));
        let params = ProgressNotificationParam {
            progress_token,
            progress: 50.0,
            total: Some(100.0),
            message: None,
        };

        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::Progress {
                target: plug_core::notifications::NotificationTarget::Stdio {
                    client_id: Arc::from("sess-OTHER"),
                },
                params,
            },
            Some("sess-42"), // does NOT match target
        )
        .await;

        assert!(
            resp.is_none(),
            "progress for a different session should be filtered out, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_forwards_cancelled_for_matching_session() {
        use rmcp::model::{CancelledNotificationParam, NumberOrString, RequestId};

        let params = CancelledNotificationParam {
            request_id: RequestId::from(NumberOrString::Number(99)),
            reason: Some("user cancelled".to_string()),
        };

        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::Cancelled {
                target: plug_core::notifications::NotificationTarget::Stdio {
                    client_id: Arc::from("sess-7"),
                },
                params,
            },
            Some("sess-7"), // matches target
        )
        .await;

        match resp {
            Some(IpcResponse::CancelledNotification { params }) => {
                assert_eq!(params["reason"], "user cancelled");
            }
            other => panic!("expected CancelledNotification, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn control_notification_filters_cancelled_for_different_session() {
        use rmcp::model::{CancelledNotificationParam, NumberOrString, RequestId};

        let params = CancelledNotificationParam {
            request_id: RequestId::from(NumberOrString::Number(99)),
            reason: None,
        };

        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::Cancelled {
                target: plug_core::notifications::NotificationTarget::Stdio {
                    client_id: Arc::from("sess-OTHER"),
                },
                params,
            },
            Some("sess-7"), // does NOT match target
        )
        .await;

        assert!(
            resp.is_none(),
            "cancelled for a different session should be filtered out, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_ignores_logging_on_control_channel() {
        use rmcp::model::LoggingMessageNotificationParam;

        let params = LoggingMessageNotificationParam::new(
            rmcp::model::LoggingLevel::Info,
            serde_json::json!("test log"),
        );
        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::LoggingMessage { params },
            Some("sess-1"),
        )
        .await;

        assert!(
            resp.is_none(),
            "logging messages should not be forwarded on the control channel, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_forwards_token_refresh_exchanged_as_logging() {
        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::TokenRefreshExchanged {
                server_id: Arc::from("github"),
            },
            Some("sess-1"),
        )
        .await;

        match resp {
            Some(IpcResponse::LoggingNotification { params }) => {
                assert_eq!(params["logger"], "plug.auth");
                assert_eq!(params["data"]["event"], "token_refresh_exchanged");
                assert_eq!(params["data"]["server_id"], "github");
            }
            other => panic!("expected LoggingNotification, got: {other:?}"),
        }
    }
}
