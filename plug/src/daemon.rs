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
    SubscribeRequestParams, UnsubscribeRequestParams,
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

    /// Get the stable client_id for a session.
    fn client_id(&self, session_id: &str) -> Option<String> {
        self.sessions.get(session_id).map(|s| s.client_id.clone())
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

/// Single process-wide lock serializing every test that reads or writes the
/// global `test_runtime_paths` slot (daemon, ipc_proxy, and runtime tests).
///
/// The runtime/state paths are a process global, so a test must hold this lock
/// for the whole window it has them set — otherwise a concurrently-running test
/// (under parallel threads) could clobber the slot or resolve `socket_path()`
/// to a foreign temp dir. Using ONE shared lock across all three test modules
/// (rather than a per-module lock) is what makes dropping `--test-threads=1`
/// safe: the ~15 path-touching tests serialize among themselves while the rest
/// of the suite runs in parallel.
///
/// Acquire from a `#[tokio::test]` with `.lock().await`, and from a plain
/// `#[test]` with `.blocking_lock()`. Never call `.blocking_lock()` from inside a
/// tokio runtime — it panics. The two forms interoperate correctly: a sync
/// `blocking_lock` holder and an async `.lock().await` waiter mutually exclude.
#[cfg(test)]
pub(crate) fn runtime_paths_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
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

fn restore_token_file(
    token_file: &std::path::Path,
    previous_token: Option<&str>,
) -> anyhow::Result<()> {
    match previous_token {
        Some(token) => plug_core::auth::write_token_file(token_file, token)
            .with_context(|| format!("failed to restore auth token: {}", token_file.display())),
        None => match std::fs::remove_file(token_file) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("failed to remove auth token: {}", token_file.display())),
        },
    }
}

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
        .write(true)
        .read(true)
        .open(pid_path)
        .with_context(|| format!("failed to open PID file: {}", pid_path.display()))?;

    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("another plug daemon is already running (PID file locked)"))?;

    // Write our PID
    use std::io::Write;
    let mut f = &file;
    f.set_len(0)?;
    write!(f, "{}", std::process::id())?;
    f.flush()?;

    #[cfg(unix)]
    set_file_permissions_0600(pid_path)?;

    Ok(file)
}

// ───────────────────────── Grace-period auto-shutdown ─────────────────────────

/// Spawn the grace-period auto-shutdown task.
///
/// Watches `count_rx` for IPC client-count changes. When the count drops to
/// zero, starts a `grace_period_secs` countdown. If the countdown fires while
/// IPC count is still zero but daemon-owned HTTP sessions are active, the
/// daemon is kept alive — but instead of falling back to the plain IPC watch
/// (which would never wake on an HTTP-session-count change, stranding the
/// daemon alive indefinitely), the task enters a bounded re-check loop that
/// polls both counts on `recheck_interval` until either an IPC client
/// reconnects (control returns to the outer watch loop) or HTTP sessions
/// drain to zero (the daemon is cancelled). The re-check loop does not
/// restart a fresh grace period — the grace period already expired once;
/// HTTP sessions were the only thing holding the daemon up, and the
/// session-store's own timeout already governs their lifetime.
fn spawn_grace_period_task(
    grace_period_secs: u64,
    mut count_rx: tokio::sync::watch::Receiver<usize>,
    http_sessions: Option<Arc<dyn SessionStore>>,
    daemon_cancel: CancellationToken,
    grace_token: CancellationToken,
) {
    tokio::spawn(async move {
        // Bounded: never busier than 1s, never lazier than 30s; scales down
        // for short test-sized grace periods.
        let recheck_interval = Duration::from_secs(grace_period_secs.clamp(1, 30));

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

                            // Re-check loop: HTTP sessions are the only thing keeping the
                            // daemon alive. Poll until they drain (shut down) or an IPC
                            // client reconnects (resume the outer watch loop).
                            'recheck: loop {
                                tokio::select! {
                                    _ = tokio::time::sleep(recheck_interval) => {
                                        if *count_rx.borrow() > 0 {
                                            tracing::info!("client reconnected, grace period cancelled");
                                            break 'recheck;
                                        }
                                        let http_session_count = http_sessions
                                            .as_ref()
                                            .map(|sessions| sessions.session_count())
                                            .unwrap_or(0);
                                        if http_session_count == 0 {
                                            tracing::info!("grace period expired with no clients, shutting down");
                                            daemon_cancel.cancel();
                                            return;
                                        }
                                        // Still held alive by HTTP sessions; keep polling.
                                    }
                                    result = count_rx.changed() => {
                                        if result.is_err() {
                                            return;
                                        }
                                        if *count_rx.borrow() > 0 {
                                            tracing::info!("client reconnected, grace period cancelled");
                                            break 'recheck;
                                        }
                                        // Still 0; keep polling the re-check loop.
                                    }
                                    _ = grace_token.cancelled() => return,
                                }
                            }
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

    // Acquire PID file lock BEFORE socket operations to prevent TOCTOU races.
    // Two concurrent auto_start_daemon calls: the loser fails here, retries connecting.
    let pid_file_path = pid_path();
    let _pid_lock = acquire_pid_lock(&pid_file_path)?;

    // Generate auth token only after we own the daemon runtime lock so a
    // losing concurrent startup cannot overwrite the live daemon's control token.
    let auth_token = generate_auth_token();
    let token_file = token_path();
    let previous_token = std::fs::read_to_string(&token_file).ok();
    plug_core::auth::write_token_file(&token_file, &auth_token)
        .with_context(|| format!("failed to write auth token: {}", token_file.display()))?;

    let startup_result = async {
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

        Ok::<_, anyhow::Error>((sock_path, listener))
    }
    .await;

    let (sock_path, listener) = match startup_result {
        Ok(started) => started,
        Err(error) => {
            restore_token_file(&token_file, previous_token.as_deref())?;
            return Err(error);
        }
    };

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
        spawn_grace_period_task(
            grace_period_secs,
            count_rx,
            http_sessions.clone(),
            cancel.clone(),
            grace_cancel.clone(),
        );
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
/// IPC adapter for the shared `tools/call` dispatcher.
///
/// IPC has a first-class downstream identity: `DownstreamTransport::Ipc`, the
/// `ipc:{session_id}` lazy session-key namespace, and `NotificationTarget::Ipc`
/// (the KTD3 split — it no longer masquerades as stdio). The task owner is
/// pre-resolved by the shim so the transport-specific `UNKNOWN_SESSION` error
/// frame is preserved for a task-augmented call whose session vanished.
struct IpcDownstreamContext {
    session_id: Arc<str>,
    request_id: RequestId,
    client_type: plug_core::types::ClientType,
    owner: Option<plug_core::tasks::TaskOwner>,
}

impl plug_core::dispatch::DownstreamContext for IpcDownstreamContext {
    fn downstream_call_context(&self) -> plug_core::proxy::DownstreamCallContext {
        plug_core::proxy::DownstreamCallContext::ipc_for_client(
            Arc::clone(&self.session_id),
            self.request_id.clone(),
            self.client_type,
        )
    }

    fn task_owner(&self) -> Result<plug_core::tasks::TaskOwner, McpError> {
        self.owner.clone().ok_or_else(|| {
            McpError::internal_error("ipc task owner was not resolved".to_string(), None)
        })
    }
}

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
        let removed_client_id = ctx.client_registry.client_id(session_id);
        ctx.client_registry.deregister(session_id);
        let target = plug_core::notifications::NotificationTarget::Ipc {
            client_id: std::sync::Arc::from(session_id.as_str()),
        };
        ctx.engine
            .tool_router()
            .cleanup_subscriptions_for_target(&target)
            .await;
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
        let lazy_session_key = plug_core::proxy::ToolRouter::lazy_session_key(
            plug_core::proxy::DownstreamTransport::Ipc,
            session_id,
        );
        ctx.engine
            .tool_router()
            .clear_lazy_session(&lazy_session_key);
        if let Some(client_id) = removed_client_id {
            if !ctx.client_registry.client_sessions.contains_key(&client_id) {
                let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
                ctx.engine
                    .tool_router()
                    .cleanup_tasks_for_owner(&owner)
                    .await;
            }
        }
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

        ipc::send_chunked_response(writer, &response).await?;

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
        Ok(ProtocolNotification::ToolListChangedFor { target }) => {
            if matches!(
                target,
                NotificationTarget::Ipc { client_id: ref target_id }
                    if session_id.is_some_and(|sid| target_id.as_ref() == sid)
            ) {
                ipc::send_response(writer, &IpcResponse::ToolListChangedNotification)
                    .await
                    .ok();
            }
        }
        Ok(ProtocolNotification::ResourceListChanged) => {
            ipc::send_response(writer, &IpcResponse::ResourceListChangedNotification)
                .await
                .ok();
        }
        Ok(ProtocolNotification::ResourceUpdated { target, params }) => {
            if matches!(
                target,
                NotificationTarget::Ipc { client_id: ref target_id }
                    if session_id.is_some_and(|sid| target_id.as_ref() == sid)
            ) {
                let notif = IpcResponse::ResourceUpdatedNotification {
                    params: serde_json::to_value(params).unwrap_or_default(),
                };
                ipc::send_response(writer, &notif).await.ok();
            }
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
                NotificationTarget::Ipc { client_id: ref target_id }
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
                NotificationTarget::Ipc { client_id: ref target_id }
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
        Ok(ProtocolNotification::LoggingMessage { .. }) => {
            // Logging is handled by the dedicated logging channel.
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
                    message: format!("{e:#}"),
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
                        IpcResponse::Reloaded { report }
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
            let registration = match ctx.client_registry.try_register(
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
                let target = plug_core::notifications::NotificationTarget::Ipc {
                    client_id: Arc::from(replaced_session_id.as_str()),
                };
                ctx.engine
                    .tool_router()
                    .cleanup_subscriptions_for_target(&target)
                    .await;
                ctx.engine
                    .tool_router()
                    .unregister_downstream_bridge(&target);
                if ctx.engine.tool_router().clear_roots_for_target(&target) {
                    ctx.engine
                        .tool_router()
                        .forward_roots_list_changed_to_upstreams()
                        .await;
                }
                ctx.engine
                    .tool_router()
                    .remove_client_log_level(replaced_session_id);
                let lazy_session_key = plug_core::proxy::ToolRouter::lazy_session_key(
                    plug_core::proxy::DownstreamTransport::Ipc,
                    replaced_session_id,
                );
                ctx.engine
                    .tool_router()
                    .clear_lazy_session(&lazy_session_key);
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
                plug_core::notifications::NotificationTarget::Ipc {
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
            let removed_client_id = ctx.client_registry.client_id(session_id);
            ctx.client_registry.deregister(session_id);
            let target = plug_core::notifications::NotificationTarget::Ipc {
                client_id: std::sync::Arc::from(session_id.as_str()),
            };
            ctx.engine
                .tool_router()
                .cleanup_subscriptions_for_target(&target)
                .await;
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
            let lazy_session_key = plug_core::proxy::ToolRouter::lazy_session_key(
                plug_core::proxy::DownstreamTransport::Ipc,
                session_id,
            );
            ctx.engine
                .tool_router()
                .clear_lazy_session(&lazy_session_key);
            if let Some(client_id) = removed_client_id {
                if !ctx.client_registry.client_sessions.contains_key(&client_id) {
                    let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
                    ctx.engine
                        .tool_router()
                        .cleanup_tasks_for_owner(&owner)
                        .await;
                }
            }
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
            let tools = tool_router.list_all_tools_with_risk();
            let config = ctx.engine.config();
            let ipc_tools = tools
                .into_iter()
                .map(|(server_id, tool, risk)| {
                    let upstream = tool_router
                        .server_manager()
                        .get_upstream_metadata(&server_id);
                    plug_core::ipc::IpcToolInfo {
                        source: config
                            .servers
                            .get(&server_id)
                            .map(plug_core::ipc::IpcServerSourceInfo::from_config),
                        trust: plug_core::ipc::IpcTrustInfo::for_server(
                            &server_id,
                            config.servers.get(&server_id),
                        ),
                        risk,
                        name: tool.name.to_string(),
                        server_id,
                        description: tool.description.map(|d| d.to_string()),
                        title: tool.title.clone(),
                        icons: tool.icons.clone(),
                        upstream,
                    }
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
            let client_type = ctx
                .client_registry
                .client_info(session_id)
                .map(|info| plug_core::client_detect::detect_client(&info))
                .unwrap_or(plug_core::types::ClientType::Unknown);
            let caps = ctx
                .engine
                .tool_router()
                .synthesized_capabilities_for_client(client_type);
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
                    let target = plug_core::notifications::NotificationTarget::Ipc {
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
        let snapshot = store.fallback_auth_snapshot();
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

    let snapshot = store.credential_snapshot();
    let existing_client_id = snapshot
        .credentials
        .as_ref()
        .map(|creds| creds.client_id.as_str());
    let (client_id, _) = oauth::injected_client_identity(
        config
            .servers
            .get(server_name)
            .is_some_and(|sc| sc.auth.as_deref() == Some("oauth")),
        config
            .servers
            .get(server_name)
            .and_then(|sc| sc.oauth_client_id.as_deref()),
        existing_client_id,
        refresh_token.is_some(),
    );

    let stored = StoredCredentials::new(client_id, Some(token), vec![], Some(now));

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
                    "credentials saved but server restart failed: {e:#}. \
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

/// Encode a serializable value as an IPC `McpResponse` payload, falling back to
/// a `SERIALIZE_ERROR` frame if serialization fails. The single encode primitive
/// for IPC method results — replaces the per-arm `match serde_json::to_value`
/// ladder so every arm shares one fallback path.
fn ipc_ok<T: serde::Serialize>(value: T) -> IpcResponse {
    match serde_json::to_value(value) {
        Ok(payload) => IpcResponse::McpResponse { payload },
        Err(e) => IpcResponse::Error {
            code: "SERIALIZE_ERROR".to_string(),
            message: e.to_string(),
        },
    }
}

/// Encode a `Result<T, McpError>` from the shared router as an IPC response:
/// success serializes to an `McpResponse` payload; an `McpError` serializes into
/// an `McpResponse`-with-error payload (the IPC convention — errors ride the same
/// channel, distinguished by a `code` field). Both paths share the
/// `SERIALIZE_ERROR` fallback via [`ipc_ok`].
fn ipc_from_mcp_result<T: serde::Serialize>(result: Result<T, McpError>) -> IpcResponse {
    match result {
        Ok(value) => ipc_ok(value),
        Err(err) => ipc_ok(err),
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
            let lazy_session_key = plug_core::proxy::ToolRouter::lazy_session_key(
                plug_core::proxy::DownstreamTransport::Ipc,
                session_id,
            );
            let result = tool_router.list_tools_page_for_client_session(
                client_type,
                Some(&lazy_session_key),
                request,
            );
            ipc_ok(result)
        }

        "resources/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_resources_page(request);
            ipc_ok(result)
        }

        "resources/templates/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_resource_templates_page(request);
            ipc_ok(result)
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

            ipc_from_mcp_result(tool_router.read_resource(uri).await)
        }

        "prompts/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let result = tool_router.list_prompts_page(request);
            ipc_ok(result)
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

            ipc_from_mcp_result(tool_router.get_prompt(name, arguments).await)
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

            ipc_from_mcp_result(tool_router.complete_request(params).await)
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
            ipc_ok(serde_json::json!({}))
        }

        "tools/call" => {
            let call_params = match params
                .map(|p| serde_json::from_value::<rmcp::model::CallToolRequestParams>(p.clone()))
            {
                Some(Ok(p)) => p,
                Some(Err(e)) => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: format!("tools/call: {e}"),
                    };
                }
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "tools/call requires params".to_string(),
                    };
                }
            };

            // An empty / unknown tool name is left to the shared dispatcher so all
            // three transports return the identical router error (ToolNotFound ->
            // METHOD_NOT_FOUND) rather than IPC short-circuiting with its own frame.

            // Build downstream context so the ToolRouter can route reverse
            // requests (elicitation, sampling) back to this IPC client.
            let client_type = ctx
                .client_registry
                .client_info(session_id)
                .map(|info| plug_core::client_detect::detect_client(&info))
                .unwrap_or(plug_core::types::ClientType::Unknown);

            // Pre-resolve the task owner so the transport-specific UNKNOWN_SESSION
            // error frame is preserved for a task-augmented call whose session
            // vanished (the dispatcher only sees an opaque McpError otherwise).
            let owner = if call_params.task.is_some() {
                let Some(client_id) = ctx.client_registry.client_id(session_id) else {
                    return IpcResponse::Error {
                        code: "UNKNOWN_SESSION".to_string(),
                        message: "session not found".to_string(),
                    };
                };
                Some(plug_core::proxy::ToolRouter::task_owner_for_ipc_client(
                    &client_id,
                ))
            } else {
                None
            };

            // Synthetic request ID — the IPC protocol doesn't carry JSON-RPC IDs,
            // but the context needs one for active call tracking.
            let request_id = RequestId::from(rmcp::model::NumberOrString::String(Arc::from(
                format!("ipc-{session_id}-{}", uuid::Uuid::new_v4()).as_str(),
            )));
            let downstream_ctx = IpcDownstreamContext {
                session_id: Arc::from(session_id),
                request_id,
                client_type,
                owner,
            };

            match plug_core::dispatch::dispatch_tools_call(
                tool_router,
                &downstream_ctx,
                call_params,
            )
            .await
            {
                Ok(plug_core::dispatch::ToolCallOutcome::Called(result)) => ipc_ok(result),
                Ok(plug_core::dispatch::ToolCallOutcome::TaskCreated(result)) => ipc_ok(result),
                Err(mcp_err) => ipc_ok(mcp_err),
            }
        }

        "tasks/list" => {
            let request = params
                .and_then(|p| serde_json::from_value::<PaginatedRequestParams>(p.clone()).ok());
            let Some(client_id) = ctx.client_registry.client_id(session_id) else {
                return IpcResponse::Error {
                    code: "UNKNOWN_SESSION".to_string(),
                    message: "session not found".to_string(),
                };
            };
            let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
            ipc_from_mcp_result(tool_router.list_tasks_for_owner(&owner, request).await)
        }

        "tasks/get" => {
            let task_id = match params
                .and_then(|p| p.get("taskId"))
                .and_then(|v| v.as_str())
                .filter(|task_id| !task_id.is_empty())
            {
                Some(task_id) => task_id,
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "tasks/get requires non-empty 'taskId'".to_string(),
                    };
                }
            };
            let Some(client_id) = ctx.client_registry.client_id(session_id) else {
                return IpcResponse::Error {
                    code: "UNKNOWN_SESSION".to_string(),
                    message: "session not found".to_string(),
                };
            };
            let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
            ipc_from_mcp_result(tool_router.get_task_info_for_owner(&owner, task_id).await)
        }

        "tasks/result" => {
            let task_id = match params
                .and_then(|p| p.get("taskId"))
                .and_then(|v| v.as_str())
                .filter(|task_id| !task_id.is_empty())
            {
                Some(task_id) => task_id,
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "tasks/result requires non-empty 'taskId'".to_string(),
                    };
                }
            };
            let Some(client_id) = ctx.client_registry.client_id(session_id) else {
                return IpcResponse::Error {
                    code: "UNKNOWN_SESSION".to_string(),
                    message: "session not found".to_string(),
                };
            };
            let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
            ipc_from_mcp_result(tool_router.get_task_result_for_owner(&owner, task_id).await)
        }

        "tasks/cancel" => {
            let task_id = match params
                .and_then(|p| p.get("taskId"))
                .and_then(|v| v.as_str())
                .filter(|task_id| !task_id.is_empty())
            {
                Some(task_id) => task_id,
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "tasks/cancel requires non-empty 'taskId'".to_string(),
                    };
                }
            };
            let Some(client_id) = ctx.client_registry.client_id(session_id) else {
                return IpcResponse::Error {
                    code: "UNKNOWN_SESSION".to_string(),
                    message: "session not found".to_string(),
                };
            };
            let owner = plug_core::proxy::ToolRouter::task_owner_for_ipc_client(&client_id);
            ipc_from_mcp_result(tool_router.cancel_task_for_owner(&owner, task_id).await)
        }

        "resources/subscribe" => {
            let request =
                match params.map(|p| serde_json::from_value::<SubscribeRequestParams>(p.clone())) {
                    Some(Ok(request)) => request,
                    Some(Err(e)) => {
                        return IpcResponse::Error {
                            code: "INVALID_PARAMS".to_string(),
                            message: format!("resources/subscribe: {e}"),
                        };
                    }
                    None => {
                        return IpcResponse::Error {
                            code: "INVALID_PARAMS".to_string(),
                            message: "resources/subscribe requires params".to_string(),
                        };
                    }
                };
            let target = plug_core::notifications::NotificationTarget::Ipc {
                client_id: Arc::from(session_id),
            };
            // Empty success encodes as `{}` (not `null`) to match stdio/HTTP.
            ipc_from_mcp_result(
                tool_router
                    .subscribe_resource(&request.uri, target)
                    .await
                    .map(|()| serde_json::json!({})),
            )
        }

        "resources/unsubscribe" => {
            let request = match params
                .map(|p| serde_json::from_value::<UnsubscribeRequestParams>(p.clone()))
            {
                Some(Ok(request)) => request,
                Some(Err(e)) => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: format!("resources/unsubscribe: {e}"),
                    };
                }
                None => {
                    return IpcResponse::Error {
                        code: "INVALID_PARAMS".to_string(),
                        message: "resources/unsubscribe requires params".to_string(),
                    };
                }
            };
            let target = plug_core::notifications::NotificationTarget::Ipc {
                client_id: Arc::from(session_id),
            };
            // Empty success encodes as `{}` (not `null`) to match stdio/HTTP.
            ipc_from_mcp_result(
                tool_router
                    .unsubscribe_resource(&request.uri, &target)
                    .await
                    .map(|()| serde_json::json!({})),
            )
        }

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

                    sandbox: None,
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

        StoredCredentials::new(
            "test-client".to_string(),
            Some(token),
            vec!["read".to_string()],
            Some(0),
        )
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

    #[tokio::test]
    async fn grace_period_shuts_down_when_no_http_sessions() {
        let (registry, count_rx) = ClientRegistry::new();
        let registration = registry
            .try_register("client-1".to_string(), None, MAX_REGISTERED_PROXY_CLIENTS)
            .expect("register");

        let daemon_cancel = CancellationToken::new();
        let grace_cancel = CancellationToken::new();

        spawn_grace_period_task(
            1,
            count_rx,
            None,
            daemon_cancel.clone(),
            grace_cancel.clone(),
        );

        registry.deregister(&registration.session_id);

        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            daemon_cancel.is_cancelled(),
            "daemon should shut down once grace expires with no IPC clients and no HTTP sessions"
        );

        grace_cancel.cancel();
    }

    #[tokio::test]
    async fn grace_period_rearms_when_http_sessions_drain_to_zero() {
        let (registry, count_rx) = ClientRegistry::new();
        let registration = registry
            .try_register("client-1".to_string(), None, MAX_REGISTERED_PROXY_CLIENTS)
            .expect("register");

        let session_store = plug_core::session::StatefulSessionStore::new(1800, 100);
        let http_session_id = session_store.create_session().expect("session");
        let http_sessions: Arc<dyn SessionStore> = Arc::new(session_store);

        let daemon_cancel = CancellationToken::new();
        let grace_cancel = CancellationToken::new();

        spawn_grace_period_task(
            1,
            count_rx,
            Some(http_sessions.clone()),
            daemon_cancel.clone(),
            grace_cancel.clone(),
        );

        // Last IPC client disconnects -> grace countdown starts.
        registry.deregister(&registration.session_id);

        // Grace period (1s) expires while the HTTP session is still active: the
        // daemon must NOT fall back to an IPC-only wait and must stay alive.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !daemon_cancel.is_cancelled(),
            "daemon should stay alive while HTTP sessions are active"
        );

        // Drain the HTTP session; the bounded re-check loop (1s poll) should
        // notice on its next tick and shut the daemon down.
        http_sessions.remove(&http_session_id);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            daemon_cancel.is_cancelled(),
            "daemon should shut down within one re-check interval of HTTP sessions draining to zero"
        );

        grace_cancel.cancel();
    }

    #[tokio::test]
    async fn grace_period_ipc_reconnect_during_held_alive_resumes_watching() {
        let (registry, count_rx) = ClientRegistry::new();
        let registration = registry
            .try_register("client-1".to_string(), None, MAX_REGISTERED_PROXY_CLIENTS)
            .expect("register");

        let session_store = plug_core::session::StatefulSessionStore::new(1800, 100);
        let _http_session_id = session_store.create_session().expect("session");
        let http_sessions: Arc<dyn SessionStore> = Arc::new(session_store);

        let daemon_cancel = CancellationToken::new();
        let grace_cancel = CancellationToken::new();

        spawn_grace_period_task(
            1,
            count_rx,
            Some(http_sessions),
            daemon_cancel.clone(),
            grace_cancel.clone(),
        );

        registry.deregister(&registration.session_id);

        // Grace period expires with the HTTP session still active -> the daemon
        // is held alive and enters the bounded re-check loop.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!daemon_cancel.is_cancelled());

        // An IPC client reconnects while the daemon is held alive by the HTTP session.
        let reconnect = registry
            .try_register("client-2".to_string(), None, MAX_REGISTERED_PROXY_CLIENTS)
            .expect("reconnect");

        // Give the re-check loop's `count_rx.changed()` branch time to observe it.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !daemon_cancel.is_cancelled(),
            "daemon must not shut down once an IPC client reconnects"
        );

        // Disconnecting again should start a *fresh* grace period (the task
        // returned to the outer watch loop, not a stale re-check that fires
        // immediately).
        registry.deregister(&reconnect.session_id);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !daemon_cancel.is_cancelled(),
            "a fresh grace period should not have expired yet"
        );

        grace_cancel.cancel();
    }

    #[test]
    fn socket_path_is_in_runtime_dir() {
        // Hold the shared lock so a concurrent setter can't change the global
        // runtime paths between the two reads below.
        let _guard = runtime_paths_test_lock().blocking_lock();
        let rt = runtime_dir();
        let sock = socket_path();
        assert!(sock.starts_with(&rt));
        assert!(sock.to_string_lossy().ends_with("plug.sock"));
    }

    #[test]
    fn pid_path_is_in_runtime_dir() {
        let _guard = runtime_paths_test_lock().blocking_lock();
        let rt = runtime_dir();
        let pid = pid_path();
        assert!(pid.starts_with(&rt));
        assert!(pid.to_string_lossy().ends_with("plug.pid"));
    }

    #[test]
    fn token_path_is_in_runtime_dir() {
        let _guard = runtime_paths_test_lock().blocking_lock();
        let rt = runtime_dir();
        let tok = token_path();
        assert!(tok.starts_with(&rt));
        assert!(tok.to_string_lossy().ends_with("plug.token"));
    }

    #[test]
    fn acquire_pid_lock_does_not_truncate_existing_file_on_failed_relock() {
        let temp = std::env::temp_dir().join(format!(
            "plug-daemon-pid-lock-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temp).expect("create temp dir");
        let pid_file = temp.join("plug.pid");

        let first_lock = acquire_pid_lock(&pid_file).expect("first pid lock");
        let initial = std::fs::read_to_string(&pid_file).expect("read pid after first lock");
        assert!(
            !initial.trim().is_empty(),
            "first lock should write the current pid"
        );

        let second = acquire_pid_lock(&pid_file);
        assert!(second.is_err(), "second pid lock should fail");

        let after_failed_relock =
            std::fs::read_to_string(&pid_file).expect("read pid after failed relock");
        assert_eq!(
            after_failed_relock, initial,
            "failed relock must not blank or rewrite the existing pid file"
        );

        drop(first_lock);
        std::fs::remove_dir_all(temp).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn run_daemon_losing_start_does_not_rotate_existing_token() {
        // Hold the shared lock for the whole window the global paths are set, so
        // this test is safe to run alongside other global-paths tests in parallel.
        let _guard = runtime_paths_test_lock().lock().await;
        let runtime_root = std::env::temp_dir().join(format!(
            "plug-daemon-runtime-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let state_root = std::env::temp_dir().join(format!(
            "plug-daemon-state-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let rt_dir = runtime_dir();
        std::fs::create_dir_all(&rt_dir).expect("create daemon runtime dir");
        let token_file = token_path();
        std::fs::write(&token_file, "stable-token").expect("write seed token");
        let pid_file = pid_path();
        let winning_lock = acquire_pid_lock(&pid_file).expect("winning pid lock");

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = Arc::new(Engine::new(plug_core::config::Config::default()));

        let result = run_daemon(engine, config_path, 0, None).await;
        assert!(result.is_err(), "losing concurrent start should fail");

        let token_after =
            std::fs::read_to_string(&token_file).expect("read token after failed start");
        assert_eq!(token_after, "stable-token");

        drop(winning_lock);
        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    #[tokio::test]
    async fn run_daemon_restores_existing_token_when_startup_fails_after_token_write() {
        let _guard = runtime_paths_test_lock().lock().await;
        let runtime_root = std::env::temp_dir().join(format!(
            "plug-daemon-runtime-post-token-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let state_root = std::env::temp_dir().join(format!(
            "plug-daemon-state-post-token-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let rt_dir = runtime_dir();
        std::fs::create_dir_all(&rt_dir).expect("create daemon runtime dir");
        let token_file = token_path();
        std::fs::write(&token_file, "stable-token").expect("write seed token");
        let sock_path = socket_path();
        std::fs::create_dir_all(&sock_path).expect("create blocking socket directory");

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = Arc::new(Engine::new(plug_core::config::Config::default()));

        let result = run_daemon(engine, config_path, 0, None).await;
        assert!(
            result.is_err(),
            "startup with a blocking socket directory should fail"
        );

        let token_after =
            std::fs::read_to_string(&token_file).expect("read token after failed startup");
        assert_eq!(token_after, "stable-token");

        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
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
        assert!(servers[0].warnings.is_empty());

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
        assert!(servers[0].warnings.is_empty());

        clear_store(&server_name).await;
        cleanup_temp_config(&ctx.config_path);
    }

    #[tokio::test]
    async fn inject_token_reuses_existing_persisted_client_id() {
        let config_path = temp_config_path("inject-token-client-id");
        let server_name = format!("oauth-inject-{}", std::process::id());
        write_oauth_config(&config_path, &[server_name.as_str()]);
        let mut config = plug_core::config::load_config(Some(&config_path)).unwrap();
        config
            .servers
            .get_mut(&server_name)
            .expect("server config")
            .oauth_client_id = None;
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

        let store = plug_core::oauth::get_or_create_store(&server_name);
        clear_store(&server_name).await;
        let mut existing = seeded_credentials();
        existing.client_id = "dynamic-client-123".to_string();
        store.save(existing).await.unwrap();

        let ctx = auth_status_test_context(config_path.clone());
        let response = dispatch_inject_token(
            &ctx,
            &server_name,
            "new-access-token",
            &Some("new-refresh-token".to_string()),
            &Some(3600),
        )
        .await;

        match response {
            IpcResponse::Ok | IpcResponse::Error { .. } => {}
            other => panic!("unexpected inject response: {other:?}"),
        }

        let stored = store
            .load()
            .await
            .expect("load injected credentials")
            .expect("stored credentials");
        assert_eq!(stored.client_id, "dynamic-client-123");

        clear_store(&server_name).await;
        cleanup_temp_config(&config_path);
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
    async fn control_notification_routes_targeted_tool_list_changed() {
        let matching = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ToolListChangedFor {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: std::sync::Arc::from("sess-1"),
                },
            },
            Some("sess-1"),
        )
        .await;
        assert!(
            matches!(matching, Some(IpcResponse::ToolListChangedNotification)),
            "expected targeted ToolListChangedNotification, got: {matching:?}"
        );

        let non_matching = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ToolListChangedFor {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: std::sync::Arc::from("sess-2"),
                },
            },
            Some("sess-1"),
        )
        .await;
        assert!(
            non_matching.is_none(),
            "expected non-matching targeted notification to be filtered, got: {non_matching:?}"
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
    async fn control_notification_forwards_resource_updated_for_matching_session() {
        let matching = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ResourceUpdated {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: Arc::from("sess-42"),
                },
                params: rmcp::model::ResourceUpdatedNotificationParam::new(
                    "file:///tmp/mock-resource.txt",
                ),
            },
            Some("sess-42"),
        )
        .await;

        match matching {
            Some(IpcResponse::ResourceUpdatedNotification { params }) => {
                assert_eq!(params["uri"], "file:///tmp/mock-resource.txt");
            }
            other => panic!("expected ResourceUpdatedNotification, got: {other:?}"),
        }

        let non_matching = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ResourceUpdated {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: Arc::from("sess-other"),
                },
                params: rmcp::model::ResourceUpdatedNotificationParam::new(
                    "file:///tmp/mock-resource.txt",
                ),
            },
            Some("sess-42"),
        )
        .await;
        assert!(
            non_matching.is_none(),
            "expected non-matching resource update to be filtered, got: {non_matching:?}"
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
                target: plug_core::notifications::NotificationTarget::Ipc {
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
                target: plug_core::notifications::NotificationTarget::Ipc {
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
                target: plug_core::notifications::NotificationTarget::Ipc {
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
                target: plug_core::notifications::NotificationTarget::Ipc {
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

    // ── IPC end-to-end test harness (U5) ─────────────────────────────────────
    //
    // Boots a daemon-backed engine with a stdio mock upstream over a TEMPORARY
    // Unix socket and drives real `tools/call` round-trips through the actual
    // `handle_ipc_connection` server loop. This is the only end-to-end IPC test
    // path in the workspace — daemon internals are private to this binary crate,
    // so it (and the cross-transport parity matrix that builds on it) must live
    // here rather than in plug-core's integration tests.
    //
    // Each harness binds a unique temp socket and touches no global runtime path,
    // so it is safe under the parallel test suite without the runtime-paths lock.

    /// Build a stdio mock-upstream `ServerConfig` exposing `tools` (comma-separated).
    fn ipc_harness_mock_config(tools: &str) -> plug_core::config::ServerConfig {
        ipc_harness_mock_config_with(tools, &[])
    }

    /// Mock upstream config with extra mock-server flags appended (e.g. to enable
    /// the resources / prompts / completions capability fixtures).
    fn ipc_harness_mock_config_with(
        tools: &str,
        extra_args: &[&str],
    ) -> plug_core::config::ServerConfig {
        let mut args = vec!["--tools".to_string(), tools.to_string()];
        args.extend(extra_args.iter().map(|a| a.to_string()));
        plug_core::config::ServerConfig {
            command: Some(
                plug_test_harness::mock_server_bin()
                    .to_string_lossy()
                    .into_owned(),
            ),
            args,
            env: HashMap::new(),
            enabled: true,
            transport: plug_core::config::TransportType::Stdio,
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 10,
            call_timeout_secs: 5,
            max_concurrent: 4,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
            sandbox: None,
        }
    }

    /// A connected, registered IPC client wired to a real daemon connection loop
    /// over a temporary socket, with a mock upstream named `mock`.
    struct IpcTestHarness {
        engine: Arc<Engine>,
        cancel: CancellationToken,
        server_task: Option<tokio::task::JoinHandle<()>>,
        stream: tokio::net::UnixStream,
        session_id: String,
        socket_path: std::path::PathBuf,
    }

    impl IpcTestHarness {
        async fn start(tools: &str) -> Self {
            Self::start_with_config(ipc_harness_mock_config(tools)).await
        }

        /// Full-capability variant for the parity matrix: same upstream config as
        /// the stdio/HTTP parity drivers so every method family is comparable.
        async fn start_full() -> Self {
            Self::start_with_config(parity_mock_config()).await
        }

        async fn start_with_config(mock: plug_core::config::ServerConfig) -> Self {
            let mut config = plug_core::config::Config::default();
            config.servers.insert("mock".to_string(), mock);
            let engine = Arc::new(Engine::new(config));
            engine.start().await.expect("engine start");

            // Short unique socket path in /tmp — a Unix socket path must fit in
            // SUN_LEN (~104 bytes), and the platform temp dir is already long. No
            // global runtime path is touched, so this is safe under parallel tests.
            let socket_path = std::path::PathBuf::from(format!(
                "/tmp/plug-ipc-{}.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..12]
            ));
            let _ = std::fs::remove_file(&socket_path);

            let listener = UnixListener::bind(&socket_path).expect("bind temp socket");
            let cancel = CancellationToken::new();

            // Server side: accept exactly one connection and run the real handler.
            let (client_registry, _count_rx) = ClientRegistry::new();
            let ctx = ConnectionContext {
                cancel: cancel.clone(),
                auth_token: Arc::from("test-token"),
                server_manager: Arc::clone(engine.server_manager()),
                engine: Arc::clone(&engine),
                config_path: std::path::PathBuf::from("/tmp/plug-ipc-test-config.toml"),
                started_at: Instant::now(),
                client_registry: Arc::new(client_registry),
                http_sessions: None,
                session_id: None,
                reverse_request_rx: None,
            };
            let server_task = tokio::spawn(async move {
                if let Ok((stream, _addr)) = listener.accept().await {
                    let _ = handle_ipc_connection(stream, ctx).await;
                }
            });

            // Client side: connect and register to obtain a session id.
            let mut stream = tokio::net::UnixStream::connect(&socket_path)
                .await
                .expect("client connect");
            write_ipc(
                &mut stream,
                &IpcRequest::Register {
                    protocol_version: plug_core::ipc::IPC_PROTOCOL_VERSION,
                    client_id: uuid::Uuid::new_v4().to_string(),
                    client_info: Some("plug-test".to_string()),
                },
            )
            .await;
            let session_id = match read_ipc_response(&mut stream).await {
                IpcResponse::Registered { session_id, .. } => session_id,
                other => panic!("expected Registered, got {other:?}"),
            };

            Self {
                engine,
                cancel,
                server_task: Some(server_task),
                stream,
                session_id,
                socket_path,
            }
        }

        /// Issue an arbitrary MCP request over IPC and return the raw decoded
        /// `IpcResponse`. The method-generic entry point used by the parity matrix.
        async fn call(&mut self, method: &str, params: serde_json::Value) -> IpcResponse {
            write_ipc(
                &mut self.stream,
                &IpcRequest::McpRequest {
                    session_id: self.session_id.clone(),
                    method: method.to_string(),
                    params: Some(params),
                },
            )
            .await;
            read_ipc_response(&mut self.stream).await
        }

        /// Issue a `tools/call` and return the raw decoded `IpcResponse`.
        async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> IpcResponse {
            self.call_tool_params(serde_json::json!({ "name": name, "arguments": arguments }))
                .await
        }

        /// Issue a `tools/call` with caller-provided params (so a `task` field can
        /// be included) and return the raw decoded `IpcResponse`.
        async fn call_tool_params(&mut self, params: serde_json::Value) -> IpcResponse {
            write_ipc(
                &mut self.stream,
                &IpcRequest::McpRequest {
                    session_id: self.session_id.clone(),
                    method: "tools/call".to_string(),
                    params: Some(params),
                },
            )
            .await;
            read_ipc_response(&mut self.stream).await
        }

        /// Graceful async teardown: cancel the connection loop, join the server
        /// task, shut the engine down (killing the mock subprocess), and remove the
        /// socket. Idempotent with `Drop` so a test that asserts before calling this
        /// still cleans up on panic (via `Drop`), just without the async engine
        /// shutdown.
        async fn shutdown(&mut self) {
            self.cancel.cancel();
            if let Some(task) = self.server_task.take() {
                let _ = task.await;
            }
            self.engine.shutdown().await;
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    impl Drop for IpcTestHarness {
        fn drop(&mut self) {
            // Best-effort synchronous cleanup so a panicking test (e.g. a parity
            // assertion that fires before `shutdown().await`) does not orphan the
            // server task or leak the temp socket. The engine's async shutdown can
            // only run in `shutdown()`; here we cancel + abort to release the task's
            // engine handle and remove the socket file.
            self.cancel.cancel();
            if let Some(task) = self.server_task.take() {
                task.abort();
            }
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    async fn write_ipc(stream: &mut tokio::net::UnixStream, req: &IpcRequest) {
        let payload = serde_json::to_vec(req).expect("serialize ipc request");
        plug_core::ipc::write_frame(stream, &payload)
            .await
            .expect("write frame");
    }

    /// Read the next non-push `IpcResponse`, skipping any interleaved
    /// notification frames the daemon may push after registration. Bounded by a
    /// timeout so a stalled or push-only stream fails the test fast instead of
    /// hanging indefinitely.
    async fn read_ipc_response(stream: &mut tokio::net::UnixStream) -> IpcResponse {
        let deadline = std::time::Duration::from_secs(10);
        loop {
            let frame = tokio::time::timeout(deadline, plug_core::ipc::read_frame(stream))
                .await
                .expect("timed out waiting for an IPC response frame")
                .expect("read frame")
                .expect("unexpected EOF before response");
            let resp: IpcResponse =
                serde_json::from_slice(&frame).expect("decode ipc response frame");
            if is_ipc_push_notification(&resp) {
                continue;
            }
            return resp;
        }
    }

    fn is_ipc_push_notification(resp: &IpcResponse) -> bool {
        matches!(
            resp,
            IpcResponse::LoggingNotification { .. }
                | IpcResponse::ToolListChangedNotification
                | IpcResponse::ResourceListChangedNotification
                | IpcResponse::PromptListChangedNotification
                | IpcResponse::ResourceUpdatedNotification { .. }
                | IpcResponse::ProgressNotification { .. }
                | IpcResponse::CancelledNotification { .. }
        )
    }

    #[tokio::test]
    async fn ipc_tools_call_echo_round_trips() {
        let mut harness = IpcTestHarness::start("echo").await;
        let resp = harness
            .call_tool("Mock__echo", serde_json::json!({ "input": "ipc-hello" }))
            .await;
        // Tear down before asserting so a failed assertion can't orphan the engine
        // or mock subprocess (Drop is the backstop; this is the clean path).
        harness.shutdown().await;
        let IpcResponse::McpResponse { payload } = resp else {
            panic!("expected McpResponse, got {resp:?}");
        };
        let text = payload["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("Called") && text.contains("ipc-hello"),
            "unexpected echo payload: {payload}"
        );
    }

    #[tokio::test]
    async fn ipc_tools_call_unknown_tool_returns_method_not_found() {
        let mut harness = IpcTestHarness::start("echo").await;
        let resp = harness
            .call_tool("Mock__does_not_exist", serde_json::json!({}))
            .await;
        harness.shutdown().await;
        // IPC encodes McpError as McpResponse-with-error payload.
        let IpcResponse::McpResponse { payload } = resp else {
            panic!("expected McpResponse error payload, got {resp:?}");
        };
        // ToolNotFound -> METHOD_NOT_FOUND (-32601).
        assert_eq!(
            payload["code"].as_i64(),
            Some(-32601),
            "unexpected error payload: {payload}"
        );
    }

    #[test]
    fn ipc_ok_encodes_value_as_mcp_response() {
        let resp = ipc_ok(serde_json::json!({ "a": 1 }));
        let IpcResponse::McpResponse { payload } = resp else {
            panic!("expected McpResponse, got {resp:?}");
        };
        assert_eq!(payload, serde_json::json!({ "a": 1 }));

        // A value that fails serialization (a map with non-string tuple keys)
        // takes the SERIALIZE_ERROR fallback frame rather than an McpResponse.
        let mut unserializable = std::collections::BTreeMap::new();
        unserializable.insert((1_i32, 2_i32), 3_i32);
        match ipc_ok(unserializable) {
            IpcResponse::Error { code, .. } => assert_eq!(code, "SERIALIZE_ERROR"),
            other => panic!("expected SERIALIZE_ERROR frame, got {other:?}"),
        }
    }

    #[test]
    fn ipc_from_mcp_result_encodes_ok_and_err() {
        // Ok -> McpResponse with the serialized value.
        let ok = ipc_from_mcp_result::<serde_json::Value>(Ok(serde_json::json!({ "ok": true })));
        let IpcResponse::McpResponse { payload } = ok else {
            panic!("expected McpResponse for Ok, got {ok:?}");
        };
        assert_eq!(payload, serde_json::json!({ "ok": true }));

        // Err -> McpResponse carrying the serialized McpError (code + message),
        // the IPC convention where errors ride the same channel.
        let err = ipc_from_mcp_result::<serde_json::Value>(Err(McpError::invalid_params(
            "boom".to_string(),
            None,
        )));
        let IpcResponse::McpResponse { payload } = err else {
            panic!("expected McpResponse for Err, got {err:?}");
        };
        assert_eq!(payload["code"].as_i64(), Some(-32602));
        assert_eq!(payload["message"].as_str(), Some("boom"));
    }

    // ── Cross-transport tools/call parity matrix (U6) ────────────────────────
    //
    // Drives identical tools/call scenarios through the REAL stdio, HTTP, and IPC
    // downstream transports and asserts identical decoded results and identical
    // error codes. This is the CI gate that freezes cross-transport parity for the
    // tools/call method family: a fix or regression that lands on one transport's
    // shim but not the others fails the assert_eq here.
    //
    // Each transport drives its own engine + mock upstream so the only variable is
    // the downstream transport. Envelopes (HTTP 200-with-error-body, IPC McpResponse
    // frame, rmcp ServiceError) are normalized to a transport-agnostic ParityOutcome
    // before comparison.

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum ParityOutcome {
        Success { text: String, is_error: bool },
        Error { code: i64 },
    }

    /// Minimal rmcp client handler for driving the stdio transport.
    struct ParityClient;

    impl rmcp::handler::client::ClientHandler for ParityClient {
        fn get_info(&self) -> rmcp::model::ClientInfo {
            rmcp::model::ClientInfo::default()
        }
    }

    /// A `tools/call` result JSON is either a `CallToolResult` ({content, isError})
    /// or a serialized `McpError` ({code, message}). Normalize either to a
    /// ParityOutcome.
    fn parity_from_result_json(payload: &serde_json::Value) -> ParityOutcome {
        if let Some(code) = payload.get("code").and_then(|c| c.as_i64()) {
            return ParityOutcome::Error { code };
        }
        ParityOutcome::Success {
            text: payload["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            is_error: payload["isError"].as_bool().unwrap_or(false),
        }
    }

    /// Full-capability mock upstream for the cross-transport parity matrix: tools,
    /// resources, resource templates, prompts, and completion, so every method
    /// family has real routed content to compare across transports. All three
    /// transport drivers build their engine from this identical config so the
    /// only variable in a parity row is the downstream transport.
    fn parity_mock_config() -> plug_core::config::ServerConfig {
        ipc_harness_mock_config_with(
            "echo",
            &[
                "--resources",
                "--resource-templates",
                "--prompts",
                "--completions",
            ],
        )
    }

    fn parity_mock_engine() -> Arc<Engine> {
        let mut config = plug_core::config::Config::default();
        config
            .servers
            .insert("mock".to_string(), parity_mock_config());
        Arc::new(Engine::new(config))
    }

    /// stdio: drive tools/call through a real ProxyHandler served over a duplex,
    /// with an rmcp client issuing the call.
    async fn parity_stdio(tool: &str, arguments: serde_json::Value) -> ParityOutcome {
        use rmcp::ServiceExt;

        let engine = parity_mock_engine();
        engine.start().await.expect("engine start");
        let proxy = plug_core::proxy::ProxyHandler::from_router(engine.tool_router().clone());
        let (server_transport, client_transport) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            if let Ok(running) = proxy.serve(server_transport).await {
                let _ = running.waiting().await;
            }
        });
        let client = ParityClient
            .serve(client_transport)
            .await
            .expect("stdio client serve");

        let mut params = rmcp::model::CallToolRequestParams::new(tool.to_string());
        if let Some(obj) = arguments.as_object() {
            params = params.with_arguments(obj.clone());
        }
        let outcome = match client.call_tool(params).await {
            Ok(result) => parity_from_result_json(&serde_json::to_value(&result).unwrap()),
            Err(rmcp::service::ServiceError::McpError(m)) => ParityOutcome::Error {
                code: m.code.0 as i64,
            },
            Err(other) => panic!("unexpected stdio service error: {other:?}"),
        };

        let _ = client.cancel().await;
        server.abort();
        engine.shutdown().await;
        outcome
    }

    /// HTTP: drive a `tools/call` through the real axum router and return the raw
    /// JSON-RPC response. Thin wrapper over `http_method_response` preserved for
    /// the task-augmentation tests that pass a `task` field in `params`.
    async fn http_tools_call_response(params: serde_json::Value) -> serde_json::Value {
        http_method_response("tools/call", params).await
    }

    /// HTTP: drive an arbitrary MCP method through the real axum router (tower
    /// oneshot, no real port) with the given `params` value and return the raw
    /// JSON-RPC response. The method-generic driver used by the parity matrix.
    async fn http_method_response(method: &str, params: serde_json::Value) -> serde_json::Value {
        use tower::ServiceExt as _;

        let engine = parity_mock_engine();
        engine.start().await.expect("engine start");
        let state = Arc::new(plug_core::http::server::HttpState {
            router: engine.tool_router().clone(),
            sessions: Arc::new(plug_core::http::session::SessionManager::new(1800, 100))
                as Arc<dyn SessionStore>,
            cancel: CancellationToken::new(),
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: std::sync::atomic::AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
            client_capabilities: dashmap::DashMap::new(),
        });
        let app = plug_core::http::server::build_router(state);

        let init_body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "parity", "version": "1.0" }
            }
        });
        let init_req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                serde_json::to_vec(&init_body).unwrap(),
            ))
            .unwrap();
        let init_resp = app.clone().oneshot(init_req).await.unwrap();
        let session_id = init_resp
            .headers()
            .get("Mcp-Session-Id")
            .expect("session id header")
            .to_str()
            .unwrap()
            .to_string();

        let call_body = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": method,
            "params": params
        });
        let call_req = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("Mcp-Session-Id", &session_id)
            .header("MCP-Protocol-Version", "2025-11-25")
            .body(axum::body::Body::from(
                serde_json::to_vec(&call_body).unwrap(),
            ))
            .unwrap();
        let call_resp = app.clone().oneshot(call_req).await.unwrap();
        let bytes = axum::body::to_bytes(call_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        engine.shutdown().await;
        json
    }

    /// HTTP: drive a plain (non-task) tools/call and normalize to a ParityOutcome.
    async fn parity_http(tool: &str, arguments: serde_json::Value) -> ParityOutcome {
        let json =
            http_tools_call_response(serde_json::json!({ "name": tool, "arguments": arguments }))
                .await;
        if let Some(err) = json.get("error") {
            ParityOutcome::Error {
                code: err["code"].as_i64().unwrap_or(0),
            }
        } else {
            parity_from_result_json(&json["result"])
        }
    }

    /// IPC: drive tools/call through the real daemon socket loop.
    async fn parity_ipc(tool: &str, arguments: serde_json::Value) -> ParityOutcome {
        // Use the same full-capability upstream config as the stdio/HTTP parity
        // drivers (parity_mock_engine) so all three transports share an identical
        // upstream and the only variable in a parity row is the transport.
        let mut harness = IpcTestHarness::start_full().await;
        let resp = harness.call_tool(tool, arguments).await;
        harness.shutdown().await;
        match resp {
            IpcResponse::McpResponse { payload } => parity_from_result_json(&payload),
            other => panic!("unexpected IPC response: {other:?}"),
        }
    }

    // ── Method-generic parity drivers ────────────────────────────────────────
    //
    // The `tools/call` drivers above stay as the characterization guard. These
    // generic drivers extend the matrix to every other method family: each takes
    // a JSON-RPC `method` + `params`, drives it through the real transport against
    // the shared full-capability mock upstream, and normalizes to a transport-
    // agnostic `MethodOutcome` (a canonicalized result JSON, or an error code).

    /// Normalized cross-transport outcome for any MCP method: either a
    /// canonicalized result object or an error code. Result JSON is key-sorted so
    /// structural equality is independent of each transport's serialization order.
    #[derive(Debug, Clone, PartialEq)]
    enum MethodOutcome {
        Result(serde_json::Value),
        Error { code: i64 },
    }

    /// Recursively sort object keys so two structurally-equal results with
    /// different key order compare equal.
    fn canonicalize_json(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let mut sorted = serde_json::Map::new();
                for k in keys {
                    sorted.insert(k.clone(), canonicalize_json(&map[k]));
                }
                serde_json::Value::Object(sorted)
            }
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(arr.iter().map(canonicalize_json).collect())
            }
            other => other.clone(),
        }
    }

    /// Normalize a bare result payload (IPC carries success and a serialized
    /// `McpError` over the same channel) to a `MethodOutcome`. A top-level integer
    /// `code` marks an error; none of the routed result types carry one.
    fn method_outcome_from_payload(payload: &serde_json::Value) -> MethodOutcome {
        // A serialized McpError carries BOTH a top-level integer `code` and a
        // string `message`. Requiring both (not `code` alone) keeps a future
        // result type that happens to expose a top-level integer `code` from
        // being misclassified as an error.
        let code = payload.get("code").and_then(|c| c.as_i64());
        let has_message = payload.get("message").and_then(|m| m.as_str()).is_some();
        if let (Some(code), true) = (code, has_message) {
            return MethodOutcome::Error { code };
        }
        MethodOutcome::Result(canonicalize_json(payload))
    }

    /// Build an optional paginated request from a `{ "cursor": "..." }` params
    /// object, matching the rmcp typed list-method signatures.
    fn parity_paginated(params: &serde_json::Value) -> Option<rmcp::model::PaginatedRequestParams> {
        params.get("cursor").and_then(|c| c.as_str()).map(|cursor| {
            rmcp::model::PaginatedRequestParams::default().with_cursor(Some(cursor.to_string()))
        })
    }

    /// stdio: drive an arbitrary method through a real `ProxyHandler` served over
    /// a duplex, with an rmcp client issuing the typed request. The rmcp client is
    /// typed, so each method maps to its typed call; the Ok result is serialized
    /// to the same JSON the server emitted, and an `McpError` maps to its code.
    async fn parity_stdio_call(method: &str, params: serde_json::Value) -> MethodOutcome {
        use rmcp::ServiceExt;

        let engine = parity_mock_engine();
        engine.start().await.expect("engine start");
        let proxy = plug_core::proxy::ProxyHandler::from_router(engine.tool_router().clone());
        let (server_transport, client_transport) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            if let Ok(running) = proxy.serve(server_transport).await {
                let _ = running.waiting().await;
            }
        });
        let client = ParityClient
            .serve(client_transport)
            .await
            .expect("stdio client serve");

        macro_rules! outcome {
            ($call:expr) => {
                match $call.await {
                    Ok(result) => MethodOutcome::Result(canonicalize_json(
                        &serde_json::to_value(&result).unwrap(),
                    )),
                    Err(rmcp::service::ServiceError::McpError(m)) => MethodOutcome::Error {
                        code: m.code.0 as i64,
                    },
                    Err(other) => panic!("unexpected stdio service error: {other:?}"),
                }
            };
        }

        // subscribe/unsubscribe return EmptyResult on the wire; the typed rmcp
        // client discards it to `()`, so normalize Ok to the canonical empty-ok
        // ({}) that HTTP (EmptyResult) and IPC (json!({})) also produce.
        macro_rules! outcome_unit {
            ($call:expr) => {
                match $call.await {
                    Ok(()) => MethodOutcome::Result(serde_json::json!({})),
                    Err(rmcp::service::ServiceError::McpError(m)) => MethodOutcome::Error {
                        code: m.code.0 as i64,
                    },
                    Err(other) => panic!("unexpected stdio service error: {other:?}"),
                }
            };
        }

        let outcome = match method {
            "tools/list" => outcome!(client.list_tools(parity_paginated(&params))),
            "resources/list" => outcome!(client.list_resources(parity_paginated(&params))),
            "resources/templates/list" => {
                outcome!(client.list_resource_templates(parity_paginated(&params)))
            }
            "resources/read" => {
                outcome!(client.read_resource(serde_json::from_value(params).unwrap()))
            }
            "prompts/list" => outcome!(client.list_prompts(parity_paginated(&params))),
            "prompts/get" => outcome!(client.get_prompt(serde_json::from_value(params).unwrap())),
            "completion/complete" => {
                outcome!(client.complete(serde_json::from_value(params).unwrap()))
            }
            "resources/subscribe" => {
                outcome_unit!(client.subscribe(serde_json::from_value(params).unwrap()))
            }
            "resources/unsubscribe" => {
                outcome_unit!(client.unsubscribe(serde_json::from_value(params).unwrap()))
            }
            other => panic!("unsupported parity method for stdio: {other}"),
        };

        let _ = client.cancel().await;
        server.abort();
        engine.shutdown().await;
        outcome
    }

    /// HTTP: drive an arbitrary method through the real axum router and normalize.
    async fn parity_http_call(method: &str, params: serde_json::Value) -> MethodOutcome {
        let json = http_method_response(method, params).await;
        if let Some(err) = json.get("error") {
            MethodOutcome::Error {
                code: err["code"].as_i64().unwrap_or(0),
            }
        } else {
            MethodOutcome::Result(canonicalize_json(&json["result"]))
        }
    }

    /// IPC: drive an arbitrary method through the real daemon socket loop.
    async fn parity_ipc_call(method: &str, params: serde_json::Value) -> MethodOutcome {
        let mut harness = IpcTestHarness::start_full().await;
        let resp = harness.call(method, params).await;
        harness.shutdown().await;
        match resp {
            IpcResponse::McpResponse { payload } => method_outcome_from_payload(&payload),
            other => panic!("unexpected IPC response: {other:?}"),
        }
    }

    /// Drive `method`+`params` through all three transports and assert the three
    /// normalized outcomes are identical, returning the agreed outcome for any
    /// further method-specific assertions. The core parity gate for a method.
    async fn assert_parity(method: &str, params: serde_json::Value) -> MethodOutcome {
        let stdio = parity_stdio_call(method, params.clone()).await;
        let http = parity_http_call(method, params.clone()).await;
        let ipc = parity_ipc_call(method, params.clone()).await;
        assert_eq!(stdio, http, "{method}: stdio vs http divergence");
        assert_eq!(http, ipc, "{method}: http vs ipc divergence");
        stdio
    }

    #[tokio::test]
    async fn parity_tools_call_success_matches_across_transports() {
        let args = serde_json::json!({ "input": "parity" });
        let stdio = parity_stdio("Mock__echo", args.clone()).await;
        let http = parity_http("Mock__echo", args.clone()).await;
        let ipc = parity_ipc("Mock__echo", args.clone()).await;

        // All three must produce the identical decoded CallToolResult.
        assert_eq!(stdio, http, "stdio vs http success divergence");
        assert_eq!(http, ipc, "http vs ipc success divergence");
        // And it must actually be the echo success, not a coincidental match.
        match &stdio {
            ParityOutcome::Success { text, is_error } => {
                assert!(!is_error, "echo should not be an error result");
                assert!(
                    text.contains("Called echo with") && text.contains("parity"),
                    "unexpected echo text: {text}"
                );
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parity_tools_call_unknown_tool_matches_across_transports() {
        let args = serde_json::json!({});
        let stdio = parity_stdio("Mock__missing", args.clone()).await;
        let http = parity_http("Mock__missing", args.clone()).await;
        let ipc = parity_ipc("Mock__missing", args.clone()).await;

        // All three must surface the identical error code (ToolNotFound ->
        // METHOD_NOT_FOUND, -32601). A transport that diverged here would fail.
        assert_eq!(stdio, ParityOutcome::Error { code: -32601 }, "stdio");
        assert_eq!(http, ParityOutcome::Error { code: -32601 }, "http");
        assert_eq!(ipc, ParityOutcome::Error { code: -32601 }, "ipc");
    }

    /// An empty tool name now routes through the shared dispatcher on every
    /// transport (IPC's old INVALID_PARAMS pre-check was removed), so all three
    /// return the same router error as any other unknown tool.
    #[tokio::test]
    async fn parity_tools_call_empty_name_matches_across_transports() {
        let args = serde_json::json!({});
        let stdio = parity_stdio("", args.clone()).await;
        let http = parity_http("", args.clone()).await;
        let ipc = parity_ipc("", args.clone()).await;

        assert_eq!(stdio, ParityOutcome::Error { code: -32601 }, "stdio");
        assert_eq!(http, ParityOutcome::Error { code: -32601 }, "http");
        assert_eq!(ipc, ParityOutcome::Error { code: -32601 }, "ipc");
    }

    /// Documents and locks the intentional task-augmentation divergence between
    /// transports. The stdio path goes through rmcp's `ServerHandler`, which
    /// validates the tool's task capability and REJECTS a task-augmented call to a
    /// non-task tool with INVALID_PARAMS (-32602) before the dispatcher runs. The
    /// HTTP and IPC paths bypass that rmcp validation and create a plug-side
    /// passthrough task. This is pre-existing behavior the dispatcher preserves —
    /// the test pins it so a future change can't silently alter it on one transport.
    #[tokio::test]
    async fn parity_task_augmented_call_diverges_stdio_rejects_http_ipc_create_task() {
        // stdio: rmcp's ServerHandler rejects a task call to a non-task tool.
        let stdio_err = {
            use rmcp::ServiceExt;
            let engine = parity_mock_engine();
            engine.start().await.expect("engine start");
            let proxy = plug_core::proxy::ProxyHandler::from_router(engine.tool_router().clone());
            let (server_transport, client_transport) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                if let Ok(running) = proxy.serve(server_transport).await {
                    let _ = running.waiting().await;
                }
            });
            let client = ParityClient
                .serve(client_transport)
                .await
                .expect("stdio client serve");
            let mut params = rmcp::model::CallToolRequestParams::new("Mock__echo".to_string());
            params = params.with_arguments(serde_json::Map::new());
            params.task = Some(serde_json::Map::new());
            let err = client
                .call_tool(params)
                .await
                .expect_err("stdio should reject a task call to a non-task tool");
            let _ = client.cancel().await;
            server.abort();
            engine.shutdown().await;
            err
        };
        match stdio_err {
            rmcp::service::ServiceError::McpError(m) => assert_eq!(
                m.code.0, -32602,
                "stdio task rejection should be INVALID_PARAMS"
            ),
            other => panic!("unexpected stdio error: {other:?}"),
        }

        // HTTP: the direct path also creates a plug-side passthrough task.
        let http_json = http_tools_call_response(serde_json::json!({
            "name": "Mock__echo",
            "arguments": {},
            "task": {}
        }))
        .await;
        // A CreateTaskResult is carried under result.task with a taskId.
        assert!(
            http_json["result"].get("task").is_some(),
            "http task-augmented call should create a task, got {http_json}"
        );

        // IPC: the direct path creates a plug-side passthrough task.
        let mut harness = IpcTestHarness::start("echo").await;
        let ipc_resp = harness
            .call_tool_params(serde_json::json!({
                "name": "Mock__echo",
                "arguments": {},
                "task": {}
            }))
            .await;
        harness.shutdown().await;
        let IpcResponse::McpResponse { payload } = ipc_resp else {
            panic!("expected IPC McpResponse, got {ipc_resp:?}");
        };
        // CreateTaskResult carries a `task` object with a taskId.
        assert!(
            payload.get("task").is_some(),
            "ipc task-augmented call should create a task, got {payload}"
        );
    }

    // ── Method-family parity rows (U3) ───────────────────────────────────────
    //
    // Each row drives one method through stdio + HTTP + IPC against the shared
    // full-capability mock and asserts the three decoded results agree, plus a
    // light content check so a coincidental three-way error can't pass as success.
    // Routed names use the "Mock" server prefix (server "mock" -> "Mock__name").
    // The fixtures fit in one page (PAGE_SIZE = 500), so list results carry no
    // cursor; first-page parity still asserts nextCursor agreement across all three.

    /// Collect `name`-field values from a result array at `key`.
    fn result_names(json: &serde_json::Value, key: &str) -> Vec<String> {
        json[key]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn parity_tools_list_matches_across_transports() {
        match assert_parity("tools/list", serde_json::json!({})).await {
            MethodOutcome::Result(json) => {
                let names = result_names(&json, "tools");
                assert!(
                    names.iter().any(|n| n == "Mock__echo"),
                    "expected Mock__echo in tools/list, got {names:?}"
                );
                assert!(
                    json.get("nextCursor").is_none() || json["nextCursor"].is_null(),
                    "single-page fixture should carry no cursor: {json}"
                );
            }
            other => panic!("expected tools/list result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parity_resources_list_matches_across_transports() {
        match assert_parity("resources/list", serde_json::json!({})).await {
            MethodOutcome::Result(json) => {
                let names = result_names(&json, "resources");
                assert!(
                    names.iter().any(|n| n == "Mock__mock-resource.txt"),
                    "expected routed mock resource, got {names:?}"
                );
            }
            other => panic!("expected resources/list result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parity_resource_templates_list_matches_across_transports() {
        match assert_parity("resources/templates/list", serde_json::json!({})).await {
            MethodOutcome::Result(json) => {
                let names = result_names(&json, "resourceTemplates");
                assert!(
                    names.iter().any(|n| n == "Mock__mock_template"),
                    "expected routed mock template, got {names:?}"
                );
            }
            other => panic!("expected templates result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parity_resources_read_matches_across_transports() {
        let params = serde_json::json!({ "uri": "file:///tmp/mock-resource.txt" });
        match assert_parity("resources/read", params).await {
            MethodOutcome::Result(json) => {
                let text = json["contents"][0]["text"].as_str().unwrap_or_default();
                assert!(
                    text.contains("mock resource contents"),
                    "unexpected resource contents: {json}"
                );
            }
            other => panic!("expected resources/read result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parity_resources_read_unknown_uri_matches_across_transports() {
        // Unknown URI is rejected by the shared router before any upstream call,
        // so all three transports must surface the identical error code.
        let params = serde_json::json!({ "uri": "file:///does/not/exist" });
        // assert_parity already proves the three transports agree on the code;
        // pin the absolute value too (InvalidRequest -> -32600) so a uniform
        // drift to a different-but-equal code can't pass silently.
        match assert_parity("resources/read", params).await {
            MethodOutcome::Error { code } => assert_eq!(code, -32600, "unknown-uri error code"),
            other => panic!("expected resources/read error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parity_prompts_list_matches_across_transports() {
        match assert_parity("prompts/list", serde_json::json!({})).await {
            MethodOutcome::Result(json) => {
                let names = result_names(&json, "prompts");
                assert!(
                    names.iter().any(|n| n == "Mock__mock_prompt"),
                    "expected routed mock prompt, got {names:?}"
                );
            }
            other => panic!("expected prompts/list result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parity_prompts_get_matches_across_transports() {
        let params = serde_json::json!({ "name": "Mock__mock_prompt" });
        match assert_parity("prompts/get", params).await {
            MethodOutcome::Result(json) => {
                let text = json["messages"][0]["content"]["text"]
                    .as_str()
                    .unwrap_or_default();
                assert!(
                    text.contains("mock prompt body"),
                    "unexpected prompt body: {json}"
                );
            }
            other => panic!("expected prompts/get result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parity_prompts_get_unknown_matches_across_transports() {
        let params = serde_json::json!({ "name": "Mock__nonexistent" });
        // Pin the absolute code (InvalidRequest -> -32600) in addition to the
        // cross-transport agreement assert_parity enforces.
        match assert_parity("prompts/get", params).await {
            MethodOutcome::Error { code } => assert_eq!(code, -32600, "unknown-prompt error code"),
            other => panic!("expected prompts/get error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parity_completion_complete_matches_across_transports() {
        let params = serde_json::json!({
            "ref": { "type": "ref/prompt", "name": "Mock__mock_prompt" },
            "argument": { "name": "topic", "value": "mo" }
        });
        match assert_parity("completion/complete", params).await {
            MethodOutcome::Result(json) => {
                let values = json["completion"]["values"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                assert!(
                    values.iter().any(|v| v == "mock_completion"),
                    "unexpected completion values: {json}"
                );
            }
            other => panic!("expected completion result, got {other:?}"),
        }
    }

    // ── Subscribe/unsubscribe lifecycle parity (U4) ──────────────────────────
    //
    // The three transports encode an empty success differently (stdio's typed
    // client discards it to `()`, HTTP returns EmptyResult, IPC returns json!({})).
    // The drivers normalize all three to the canonical empty-ok ({}), so these
    // rows prove the divergent encodings are equivalent. Each call is a fresh
    // session, so the unsubscribe row also exercises the idempotent (not-currently-
    // subscribed) path.

    fn assert_empty_ok(outcome: MethodOutcome, label: &str) {
        match outcome {
            MethodOutcome::Result(json) => assert!(
                json.as_object().is_some_and(|o| o.is_empty()),
                "{label} should be empty-ok ({{}}), got {json}"
            ),
            other => panic!("{label} expected empty-ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parity_resources_subscribe_matches_across_transports() {
        let params = serde_json::json!({ "uri": "file:///tmp/mock-resource.txt" });
        let outcome = assert_parity("resources/subscribe", params).await;
        assert_empty_ok(outcome, "subscribe");
    }

    #[tokio::test]
    async fn parity_resources_unsubscribe_matches_across_transports() {
        let params = serde_json::json!({ "uri": "file:///tmp/mock-resource.txt" });
        let outcome = assert_parity("resources/unsubscribe", params).await;
        assert_empty_ok(outcome, "unsubscribe");
    }
}
