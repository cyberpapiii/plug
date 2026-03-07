//! Daemon mode — headless Engine with Unix socket IPC.
//!
//! Provides `plug serve --daemon` functionality: starts the Engine without TUI,
//! listens on a Unix socket for CLI queries, and logs to file.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use dashmap::DashMap;
use fs2::FileExt as _;
use tokio::net::UnixListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use plug_core::engine::Engine;
use plug_core::ipc::{self, IpcRequest, IpcResponse};

/// Maximum concurrent IPC connections.
const MAX_IPC_CONNECTIONS: usize = 32;

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
    fn register(&self, client_id: String, client_info: Option<String>) -> RegistrationResult {
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
            },
        );
        self.count_tx.send_modify(|c| *c = self.sessions.len());
        RegistrationResult {
            session_id,
            replaced_session_id,
        }
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
) -> anyhow::Result<()> {
    let rt_dir = runtime_dir();
    let log_directory = log_dir();

    // Create directories with secure permissions
    ensure_dir(&rt_dir)?;
    ensure_dir(&log_directory)?;

    // Generate auth token and write to file with restricted permissions from creation
    let auth_token = generate_auth_token();
    let token_file = token_path();
    {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&token_file)
                .and_then(|mut f| {
                    use std::io::Write;
                    f.write_all(auth_token.as_bytes())
                })
                .with_context(|| format!("failed to write auth token: {}", token_file.display()))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&token_file, &auth_token)
                .with_context(|| format!("failed to write auth token: {}", token_file.display()))?;
        }
    }

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
    let semaphore = Arc::new(Semaphore::new(MAX_IPC_CONNECTIONS));
    let auth_token: Arc<str> = Arc::from(auth_token.as_str());
    let (client_registry, count_rx) = ClientRegistry::new();
    let client_registry = Arc::new(client_registry);

    // Grace period: when the last proxy client disconnects, start a countdown.
    // If no new client connects before it fires, shut down the daemon.
    // A grace_period_secs of 0 means disable auto-shutdown (explicit shutdown only).
    let grace_cancel = CancellationToken::new();

    if grace_period_secs > 0 {
        let grace_token = grace_cancel.clone();
        let daemon_cancel = cancel.clone();
        let mut count_rx = count_rx;
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
                            // Grace period expired — recheck count
                            if *count_rx.borrow() == 0 {
                                tracing::info!("grace period expired with no clients, shutting down");
                                daemon_cancel.cancel();
                                return;
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
                        let permit = match semaphore.clone().try_acquire_owned() {
                            Ok(permit) => permit,
                            Err(_) => {
                                tracing::warn!("IPC connection rejected: max connections reached");
                                continue;
                            }
                        };

                        let engine_cancel = engine.cancel_token().clone();
                        let server_manager = engine.server_manager().clone();
                        let snapshot = engine.snapshot();
                        let auth = auth_token.clone();
                        let engine_clone = engine.clone();
                        let registry = client_registry.clone();
                        let config_path = config_path.clone();

                        tokio::spawn(async move {
                            let _permit = permit; // held for connection lifetime
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
                                session_id: None,
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
    /// Session ID assigned during Register (for auto-deregister on disconnect).
    session_id: Option<String>,
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

    loop {
        // Proxy connections (those that have Registered) are long-lived and should
        // not be subject to the idle timeout. Short-lived admin/query connections use
        // the timeout to reclaim resources.
        let frame = if let Some(ref mut rx) = log_rx {
            // Registered with logging — multiplex request reads and notifications.
            // Notifications are sent immediately when idle. During a request dispatch,
            // they queue in the broadcast channel and get drained after the response.
            'select: loop {
                tokio::select! {
                    biased;
                    _ = ctx.cancel.cancelled() => return Ok(()),
                    recv = rx.recv() => {
                        send_ipc_logging_notification(writer, recv).await?;
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

        // Dispatch request
        let response = dispatch_request(&request, ctx).await;

        ipc::send_response(writer, &response).await?;

        // Shutdown request — send OK then trigger cancel
        if matches!(request, IpcRequest::Shutdown { .. }) {
            ctx.cancel.cancel();
            break;
        }

        // After registration, subscribe to logging channel for push notifications
        if ctx.session_id.is_some() && log_rx.is_none() {
            log_rx = Some(ctx.engine.tool_router().subscribe_logging());
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
    }

    Ok(())
}

/// Send a logging notification to the IPC client, handling broadcast errors.
async fn send_ipc_logging_notification(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    recv: Result<plug_core::notifications::ProtocolNotification, tokio::sync::broadcast::error::RecvError>,
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

/// Dispatch a single IPC request to the appropriate Engine query.
async fn dispatch_request(request: &IpcRequest, ctx: &mut ConnectionContext) -> IpcResponse {
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
            let registration = ctx
                .client_registry
                .register(client_id.clone(), client_info.clone());
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
            ctx.engine.tool_router().remove_client_log_level(session_id);
            ctx.session_id = None;
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
            // Mask resource subscriptions for IPC clients — the daemon IPC
            // transport has no push channel for targeted notifications.
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

            let tools = tool_router.list_tools_for_client(client_type);
            let result = rmcp::model::ListToolsResult::with_all_items((*tools).clone());
            match serde_json::to_value(result) {
                Ok(payload) => IpcResponse::McpResponse { payload },
                Err(e) => IpcResponse::Error {
                    code: "SERIALIZE_ERROR".to_string(),
                    message: e.to_string(),
                },
            }
        }

        "resources/list" => {
            let result = tool_router.list_resources_page(None);
            match serde_json::to_value(result) {
                Ok(payload) => IpcResponse::McpResponse { payload },
                Err(e) => IpcResponse::Error {
                    code: "SERIALIZE_ERROR".to_string(),
                    message: e.to_string(),
                },
            }
        }

        "resources/templates/list" => {
            let result = tool_router.list_resource_templates_page(None);
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
            let result = tool_router.list_prompts_page(None);
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
            let level = match params
                .and_then(|p| p.get("level"))
                .and_then(|v| v.as_str())
            {
                Some(level_str) => {
                    match serde_json::from_value::<rmcp::model::LoggingLevel>(
                        serde_json::json!(level_str),
                    ) {
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

            match tool_router.call_tool(&name, arguments).await {
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
    use tokio::io::AsyncWriteExt;

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

        let first = registry.register("client-123".to_string(), Some("claude-code".to_string()));
        let second = registry.register("client-123".to_string(), Some("claude-code".to_string()));

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

        let first = registry.register("client-123".to_string(), Some("claude-code".to_string()));
        let second = registry.register("client-123".to_string(), Some("claude-code".to_string()));

        registry.deregister(&first.session_id);

        assert!(registry.session_exists(&second.session_id));
        assert_eq!(registry.count(), 1);
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
}
