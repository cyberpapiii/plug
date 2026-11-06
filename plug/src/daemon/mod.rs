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
use rmcp::ErrorData as McpError;
use rmcp::model::{
    CreateMessageRequestParams, CreateMessageResult, ElicitRequestParams, ElicitResult,
};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

use plug_core::engine::Engine;
use plug_core::http::server::{ClientRevocation, HttpState};
use plug_core::ipc::{self, IpcClientRequest, IpcClientResponse, IpcRequest, IpcResponse};
use plug_core::proxy::{ConnectorAfter, EndedSession};
use plug_core::proxy::{DownstreamBridge, ToolRouter};

mod framing;
use framing::FrameReader;

#[cfg(test)]
pub(crate) use plug_core::paths::test::{
    clear as clear_test_runtime_paths, lock as runtime_paths_test_lock,
    set as set_test_runtime_paths,
};
use plug_core::paths::{log_dir, pid_path, socket_path, token_path};

/// Create a directory, and any missing parents, readable by this user only.
fn ensure_dir(path: &std::path::Path) -> anyhow::Result<()> {
    plug_core::fs_perm::ensure_dir_0700(path)
        .with_context(|| format!("failed to create directory: {}", path.display()))
}

fn set_file_permissions_0600(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

mod registry;
use registry::ClientRegistry;

mod auth_status;
use auth_status::{auth_status_from_statuses, dispatch_auth_status};

/// Read the on-disk config for an operator request, or the error response to
/// send back.
///
/// This is the editable form on purpose: it skips the `PLUG_` environment
/// merge and env-reference expansion that `config::load_config` performs, so an
/// operator surface shows and round-trips what the file actually says rather
/// than baking expanded values into it on the next write.
// `IpcResponse` is a wide enum, so a `Result` carrying one trips
// `result_large_err`. Boxing it would push an allocation onto a path that
// exists to report a config read failure. `plug_core::operator`'s own loader
// makes the same call for the same reason.
#[allow(clippy::result_large_err)]
fn load_editable_config(path: &std::path::Path) -> Result<plug_core::config::Config, IpcResponse> {
    plug_core::config::load_editable_config(path).map_err(|error| IpcResponse::Error {
        code: "CONFIG_READ_FAILED".to_string(),
        message: error.to_string(),
    })
}

/// A config parse error without the quoted source lines. The TOML parser
/// echoes the offending line, and in config.toml that line can hold a token;
/// the snapshot is polled constantly and the app may show this text.
fn config_error_summary(message: &str) -> String {
    message
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            !line
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start()
                .starts_with('|')
        })
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_os = "macos")]
mod menu_bar;
mod notify;
use notify::send_ipc_control_notification;

mod mcp_dispatch;
use mcp_dispatch::dispatch_mcp_request;

/// Idle timeout for short-lived IPC connections (status queries, admin commands).
/// Proxy connections (those that have called Register) are exempt from this timeout.
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long reading an OpenAPI document for the app may take.
const API_DESCRIBE_TIMEOUT: Duration = Duration::from_secs(10);

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

// ──────────────────────────── PID file locking ───────────────────────────────

/// Acquire an exclusive lock on the PID file, returning the locked file handle.
///
/// The file handle MUST be held for the daemon's lifetime — dropping it releases the lock.
/// How long to keep retrying a contended PID-file lock before concluding that
/// another daemon owns it.
///
/// `ipc_client::daemon_starting` probes the same lock by taking it and letting it go
/// again, and every status surface calls that probe. A real daemon holds the
/// lock for its whole life, so retrying cannot mistake one for a probe; without
/// the retry, a `plug status` that happened to land in the same microsecond as
/// a cold start would fail that start outright.
const LOCK_CONTENTION_WINDOW: std::time::Duration = std::time::Duration::from_millis(100);

/// Take the singleton lock, tolerating the momentary contention a probe causes.
///
/// `WouldBlock` past the window means another daemon holds it; any other error
/// is a real I/O failure and is surfaced as such.
fn lock_with_retry(
    file: &std::fs::File,
    pid_path: &std::path::Path,
    window: std::time::Duration,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + window;
    loop {
        // Fully qualified: std's inherent `File::try_lock` (Rust 1.89+) would
        // otherwise shadow the fs4 trait method depending on toolchain version.
        match fs4::FileExt::try_lock(file) {
            Ok(()) => return Ok(()),
            Err(fs4::TryLockError::Error(e)) => {
                return Err(e)
                    .with_context(|| format!("failed to lock PID file: {}", pid_path.display()));
            }
            Err(fs4::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    return Err(anyhow::anyhow!(
                        "another plug daemon is already running (PID file locked)"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}

fn acquire_pid_lock(pid_path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(pid_path)
        .with_context(|| format!("failed to open PID file: {}", pid_path.display()))?;

    lock_with_retry(&file, pid_path, LOCK_CONTENTION_WINDOW)?;

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

pub(crate) struct DaemonRuntimeLock {
    file: std::fs::File,
    pid_file_path: PathBuf,
}

/// Claim singleton daemon ownership before any upstream initialization.
///
/// `plug connect` clients can all notice a missing socket at once. The winner
/// must own this lock before loading OAuth credentials or spawning upstreams;
/// every loser exits without touching either.
pub(crate) fn acquire_runtime_lock() -> anyhow::Result<DaemonRuntimeLock> {
    ensure_dir(&plug_core::paths::dir())?;
    let pid_file_path = pid_path();
    let file = acquire_pid_lock(&pid_file_path)?;
    Ok(DaemonRuntimeLock {
        file,
        pid_file_path,
    })
}

// ─────────────────────────── Daemon entry point ──────────────────────────────

/// `run_daemon_with_lock` after taking the runtime lock, with HTTP sessions
/// but no downstream OAuth.
#[cfg(test)]
pub async fn run_daemon(engine: Arc<Engine>, config_path: PathBuf) -> anyhow::Result<()> {
    let runtime_lock = acquire_runtime_lock()?;
    let http = Arc::new(HttpState::new(
        Arc::clone(engine.tool_router()),
        Arc::new(plug_core::session::SessionStore::new(1800, 100)),
        engine.cancel_token().clone(),
    ));
    run_daemon_with_lock(engine, config_path, http, runtime_lock).await
}

/// Serve local connectors on the daemon socket. `http` is the downstream
/// HTTP state the same process serves, whose sessions the daemon lists and
/// whose clients it revokes.
pub(crate) async fn run_daemon_with_lock(
    engine: Arc<Engine>,
    config_path: PathBuf,
    http: Arc<HttpState>,
    runtime_lock: DaemonRuntimeLock,
) -> anyhow::Result<()> {
    let rt_dir = plug_core::paths::dir();
    let log_directory = log_dir();

    // Create directories with secure permissions
    ensure_dir(&rt_dir)?;
    ensure_dir(&log_directory)?;

    let DaemonRuntimeLock {
        file: _pid_lock,
        pid_file_path,
    } = runtime_lock;

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
    let client_registry = Arc::new(ClientRegistry::new());

    #[cfg(target_os = "macos")]
    menu_bar::spawn(cancel.clone());

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
                        let http = Arc::clone(&http);

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
                                http,
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
                        // Out of file handles is the usual cause, and it does
                        // not clear by asking again at once.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }

    tracing::info!("daemon shutting down, cleaning up");
    std::fs::remove_file(&sock_path).ok();
    std::fs::remove_file(&pid_file_path).ok();
    std::fs::remove_file(token_path()).ok();

    Ok(())
}

// ──────────────────────── Daemon Bridge ────────────────────────────────────────

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
        request: ElicitRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<ElicitResult, McpError>> + Send + '_>> {
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
    http: Arc<HttpState>,
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

impl ConnectionContext {
    /// A copy for one concurrently dispatched request. It sees the session as
    /// it stood when the request arrived and owns no reverse-request receiver;
    /// the connection loop keeps that.
    fn fork(&self) -> Self {
        Self {
            cancel: self.cancel.clone(),
            auth_token: Arc::clone(&self.auth_token),
            server_manager: Arc::clone(&self.server_manager),
            engine: Arc::clone(&self.engine),
            config_path: self.config_path.clone(),
            started_at: self.started_at,
            client_registry: Arc::clone(&self.client_registry),
            http: Arc::clone(&self.http),
            session_id: self.session_id.clone(),
            reverse_request_rx: None,
        }
    }
}

/// Requests tagged with `ipc_id` on a registered connection, dispatching on
/// their own tasks. Each task returns the response for the loop to write.
#[derive(Default)]
struct InFlight {
    tasks: tokio::task::JoinSet<IpcResponse>,
    ids: std::collections::HashMap<tokio::task::Id, u64>,
}

/// Reverse requests written to the proxy and awaiting its reply, by id. The
/// proxy tags each reply with the id as `reverse_id`.
type PendingReverse =
    std::collections::HashMap<u64, tokio::sync::oneshot::Sender<IpcClientResponse>>;

/// Handle a single IPC connection: read requests, dispatch, send responses.
///
/// Auto-deregisters any session created via `Register` when the connection closes
/// (clean EOF, error, or idle timeout).
async fn handle_ipc_connection(
    stream: tokio::net::UnixStream,
    mut ctx: ConnectionContext,
) -> anyhow::Result<()> {
    // Plug.app stops connectors with SIGSTOP while it swaps the daemon, so
    // a reply that finishes during the shutdown drain has to fit in the
    // kernel buffer; the macOS default of 8 KB is smaller than most replies.
    if let Err(error) = socket2::SockRef::from(&stream).set_send_buffer_size(IPC_SEND_BUFFER) {
        tracing::debug!(%error, "could not enlarge the IPC send buffer");
    }
    let (reader, mut writer) = stream.into_split();
    let mut reader = FrameReader::new(reader);
    let mut in_flight = InFlight::default();

    let result = handle_ipc_loop(&mut reader, &mut writer, &mut ctx, &mut in_flight).await;

    // Requests still dispatching have nowhere to send a reply. Let them
    // finish before the cleanup below, which is the order serial dispatch
    // gave. Dropping the reverse-request receiver first makes any elicitation
    // or sampling they start fail at once instead of waiting on a proxy that
    // is gone. On daemon shutdown they are aborted instead.
    ctx.reverse_request_rx = None;
    loop {
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                in_flight.tasks.abort_all();
                while in_flight.tasks.join_next().await.is_some() {}
                break;
            }
            joined = in_flight.tasks.join_next() => {
                if joined.is_none() {
                    break;
                }
            }
        }
    }

    if let Some(session_id) = ctx.session_id.take() {
        end_ipc_session(&ctx, &session_id).await;
    }

    result
}

/// Take `session_id` out of the registry and release what the router holds
/// for it. A session a newer registration already replaced was released
/// then; what is released again here is only what it picked up since.
async fn end_ipc_session(ctx: &ConnectionContext, session_id: &str) {
    let last_of_client = ctx.client_registry.deregister(session_id);
    let connector = match last_of_client.as_deref() {
        Some(client_id) => ConnectorAfter::Gone { client_id },
        None => ConnectorAfter::StillRegistered,
    };
    ctx.engine
        .tool_router()
        .end_downstream_session(EndedSession::Ipc {
            session_id,
            connector,
        })
        .await;
}

/// Key a live session's lazy working set the way its transport stores it:
/// bridge sessions under `ipc:`, HTTP and SSE sessions under `http:`.
fn live_session_lazy_key(session: &plug_core::ipc::IpcLiveSessionInfo) -> String {
    let transport = match session.transport {
        plug_core::ipc::LiveSessionTransport::DaemonProxy => {
            plug_core::downstream_identity::DownstreamTransport::Ipc
        }
        plug_core::ipc::LiveSessionTransport::Http | plug_core::ipc::LiveSessionTransport::Sse => {
            plug_core::downstream_identity::DownstreamTransport::Http
        }
    };
    transport.lazy_session_key(&session.session_id)
}

/// Resolves when the modern downstream gate may have changed. Never resolves
/// once the sender is gone, so a closed channel cannot spin a select loop.
async fn modern_gate_changed(gate_rx: &mut tokio::sync::watch::Receiver<bool>) {
    if gate_rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Push `ModernDownstreamGateChanged` when the gate differs from the value
/// this connection last reported.
async fn push_modern_gate_change(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    gate_rx: &mut tokio::sync::watch::Receiver<bool>,
    last_modern_gate: &mut bool,
) -> anyhow::Result<()> {
    let enabled = *gate_rx.borrow_and_update();
    if enabled != *last_modern_gate {
        ipc::send_response(
            writer,
            &IpcResponse::ModernDownstreamGateChanged { enabled },
        )
        .await?;
        *last_modern_gate = enabled;
    }
    Ok(())
}

/// Inner loop for IPC connection handling.
///
/// Untagged requests run one at a time, as they always have: the loop
/// dispatches each inline and writes its reply before taking the next. On a
/// registered connection, a request tagged with `ipc_id` dispatches on its own
/// task (see `InFlight`) and the loop keeps reading, so one slow call does not
/// hold up the proxy's other requests. Every frame is still written by this
/// loop, so a chunked reply goes out whole and never interleaves.
async fn handle_ipc_loop(
    reader: &mut FrameReader,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    ctx: &mut ConnectionContext,
    in_flight: &mut InFlight,
) -> anyhow::Result<()> {
    use plug_core::notifications::ProtocolNotification;

    // Logging subscription — activated after Register so the daemon can push
    // logging notifications to this IPC client in real time.
    let mut log_rx: Option<tokio::sync::broadcast::Receiver<ProtocolNotification>> = None;
    // Protocol notification subscription — activated after Register so the daemon
    // can push list_changed, progress, and cancelled notifications to this IPC client.
    let mut ctrl_rx: Option<tokio::sync::broadcast::Receiver<ProtocolNotification>> = None;
    // Modern downstream gate changes are pushed as they happen. The router
    // outlives this connection, so the sender never closes under it.
    let mut gate_rx = ctx.engine.tool_router().watch_modern_downstream();
    let mut last_modern_gate = *gate_rx.borrow_and_update();
    let mut pending_reverse = PendingReverse::new();
    // Frames read while an inline request was dispatching, in arrival order.
    let mut deferred: std::collections::VecDeque<anyhow::Result<Option<Vec<u8>>>> =
        std::collections::VecDeque::new();
    let permits = Arc::new(tokio::sync::Semaphore::new(
        ipc::MAX_CONCURRENT_REQUESTS_PER_CONNECTION,
    ));

    loop {
        // After registration, subscribe to notification channels for push
        // delivery and move frame reads onto the cancellation-safe reader.
        if ctx.session_id.is_some() && log_rx.is_none() {
            log_rx = Some(ctx.engine.tool_router().subscribe_logging());
            ctrl_rx = Some(ctx.engine.tool_router().subscribe_notifications());
            reader.ensure_multiplexed();
        }

        // Proxy connections (those that have Registered) are long-lived and should
        // not be subject to the idle timeout. Short-lived admin/query connections use
        // the timeout to reclaim resources.
        let frame = if let Some(deferred_frame) = deferred.pop_front() {
            deferred_frame
        } else if let Some(ref mut rx) = log_rx {
            // Registered with notifications — multiplex request reads, push
            // notifications, and replies from requests dispatching on tasks.
            'select: loop {
                tokio::select! {
                    biased;
                    _ = ctx.cancel.cancelled() => {
                        flush_finished_replies(writer, in_flight).await;
                        return Ok(());
                    }
                    Some(joined) = in_flight.tasks.join_next_with_id(), if !in_flight.tasks.is_empty() => {
                        write_in_flight_reply(writer, &mut in_flight.ids, joined).await?;
                    }
                    recv = rx.recv() => {
                        if may_tell(ctx.listener(), &recv) {
                            send_ipc_logging_notification(writer, recv).await?;
                        }
                    }
                    recv = async {
                        if let Some(ref mut crx) = ctrl_rx {
                            crx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        if may_tell(ctx.listener(), &recv) {
                            send_ipc_control_notification(writer, recv, ctx.session_id.as_deref()).await?;
                        }
                    }
                    _ = modern_gate_changed(&mut gate_rx) => {
                        push_modern_gate_change(writer, &mut gate_rx, &mut last_modern_gate).await?;
                    }
                    reverse = async {
                        if let Some(ref mut rx) = ctx.reverse_request_rx {
                            rx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        if let Some((reverse_req, resp_tx)) = reverse {
                            send_reverse_request(writer, &mut pending_reverse, reverse_req, resp_tx).await?;
                        } else {
                            ctx.reverse_request_rx = None;
                        }
                    }
                    result = reader.next() => break 'select result,
                }
            }
        } else {
            tokio::select! {
                _ = ctx.cancel.cancelled() => break,
                result = tokio::time::timeout(CONNECTION_IDLE_TIMEOUT, reader.next()) => {
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

        let ids = ipc::FrameIds::peek(&frame);
        if let Some(reverse_id) = ids.reverse_id {
            deliver_reverse_reply(&mut pending_reverse, reverse_id, &frame);
            continue;
        }

        // Parse request
        let request: IpcRequest = match serde_json::from_slice(&frame) {
            Ok(req) => req,
            Err(e) => {
                tracing::debug!(error = %e, "IPC parse error");
                let resp = protocol_parse_error_response(&frame).unwrap_or_else(|| {
                    IpcResponse::error(ipc::IpcErrorCode::ParseError, "invalid request format")
                });
                ipc::send_chunked_response_with_id(writer, ids.ipc_id, &resp)
                    .await
                    .ok();
                break;
            }
        };

        // Auth check for admin commands
        if let Some(provided) = request.auth_token()
            && !verify_auth_token(provided, &ctx.auth_token)
        {
            let resp = IpcResponse::Error {
                code: "AUTH_FAILED".to_string(),
                message: "invalid auth token".to_string(),
            };
            ipc::send_chunked_response_with_id(writer, ids.ipc_id, &resp).await?;
            continue;
        }

        if let Some(ipc_id) = ids.ipc_id
            && ctx.session_id.is_some()
            && !request.runs_inline()
        {
            let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                let busy = IpcResponse::error(
                    ipc::IpcErrorCode::Busy,
                    format!(
                        "this connection already has {} requests running; retry",
                        ipc::MAX_CONCURRENT_REQUESTS_PER_CONNECTION
                    ),
                );
                ipc::send_chunked_response_with_id(writer, Some(ipc_id), &busy).await?;
                continue;
            };
            let mut task_ctx = ctx.fork();
            let handle = in_flight.tasks.spawn(async move {
                let _permit = permit;
                dispatch_request(&request, &mut task_ctx).await
            });
            in_flight.ids.insert(handle.id(), ipc_id);
            continue;
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
        // Capture whether this request is a Deregister before dispatch — a
        // SUCCESSFUL Deregister drops the bridge channel's sender, closing
        // it, so the taken-out `reverse_rx` above must not be restored below.
        // The restore site additionally gates on the response being Ok: a
        // Deregister that fails validation (SESSION_MISMATCH /
        // SESSION_REPLACED) early-returns without touching the bridge, and
        // the still-live channel must be restored as usual.
        let request_was_deregister = is_deregister_request(&request);
        // Keep reading while this request dispatches, once reads are
        // cancellation-safe: replies to reverse requests are routed at once,
        // everything else waits in `deferred` for its turn. Without this an
        // inline request that triggers elicitation could never read its reply.
        let mut read_ahead = log_rx.is_some();

        let response = {
            use std::pin::pin;

            let (engine, registry) = (Arc::clone(&ctx.engine), Arc::clone(&ctx.client_registry));
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
                            send_reverse_request(writer, &mut pending_reverse, reverse_req, resp_tx).await?;
                        } else {
                            // Channel closed (e.g. bridge deregistered) — stop polling it.
                            reverse_rx = None;
                        }
                    }
                    Some(joined) = in_flight.tasks.join_next_with_id(), if !in_flight.tasks.is_empty() => {
                        write_in_flight_reply(writer, &mut in_flight.ids, joined).await?;
                    }
                    // Also forward logging notifications while waiting
                    recv = async {
                        if let Some(ref mut rx) = log_rx {
                            rx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        if may_tell((&engine, &registry, dispatch_session_id.as_deref()), &recv) {
                            send_ipc_logging_notification(writer, recv).await?;
                        }
                    }
                    // Forward control notifications (list_changed, progress, cancelled)
                    recv = async {
                        if let Some(ref mut crx) = ctrl_rx {
                            crx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        if may_tell((&engine, &registry, dispatch_session_id.as_deref()), &recv) {
                            send_ipc_control_notification(writer, recv, dispatch_session_id.as_deref()).await?;
                        }
                    }
                    _ = modern_gate_changed(&mut gate_rx) => {
                        push_modern_gate_change(writer, &mut gate_rx, &mut last_modern_gate).await?;
                    }
                    next = reader.next(), if read_ahead => {
                        match next {
                            Ok(Some(frame)) => match ipc::FrameIds::peek(&frame).reverse_id {
                                Some(reverse_id) => {
                                    deliver_reverse_reply(&mut pending_reverse, reverse_id, &frame);
                                }
                                None => deferred.push_back(Ok(Some(frame))),
                            },
                            end => {
                                read_ahead = false;
                                deferred.push_back(end);
                            }
                        }
                    }
                }
            }
            result.unwrap()
        };

        // Restore reverse_request_rx (dispatch_request may have replaced it
        // during a Register call, in which case ctx already has the new one).
        // Skip the restore only after a SUCCESSFUL Deregister — its handler
        // intentionally cleared ctx.reverse_request_rx and dropped the
        // bridge's sender, so the channel we took out above is now closed;
        // restoring it would resurrect a dead channel that busy-spins
        // Ready(None) on selects. A FAILED Deregister (SESSION_MISMATCH /
        // SESSION_REPLACED) early-returns without touching the bridge, so
        // the taken-out channel is still live and must be restored, or every
        // future reverse request for this still-active session would fail.
        let deregister_succeeded = request_was_deregister && matches!(response, IpcResponse::Ok);
        if ctx.reverse_request_rx.is_none() && !deregister_succeeded {
            ctx.reverse_request_rx = reverse_rx;
        }

        ipc::send_chunked_response_with_id(writer, ids.ipc_id, &response).await?;

        // Shutdown request — send OK then trigger cancel
        if matches!(request, IpcRequest::Shutdown { .. }) {
            ctx.cancel.cancel();
            break;
        }

        // Drain any notifications that queued during request dispatch
        if let Some(ref mut rx) = log_rx {
            use tokio::sync::broadcast::error::TryRecvError;
            loop {
                match rx.try_recv() {
                    Ok(notif) => {
                        let recv = Ok(notif);
                        if may_tell(ctx.listener(), &recv) {
                            send_ipc_logging_notification(writer, recv).await?;
                        }
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
                        let recv = Ok(notif);
                        if may_tell(ctx.listener(), &recv) {
                            send_ipc_control_notification(writer, recv, ctx.session_id.as_deref())
                                .await?;
                        }
                    }
                    Err(TryRecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "IPC control notification lagged");
                        let notif = IpcResponse::LoggingNotification {
                            params: serde_json::to_value(
                                plug_core::notifications::ProtocolNotification::control_lagged_logging_params(
                                    skipped,
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

/// Send buffer for an IPC connection. Memory is only committed as data
/// queues, and macOS caps socket buffers at 8 MB.
const IPC_SEND_BUFFER: usize = 4 * 1024 * 1024;

/// Write the reply of a request that dispatched on its own task, tagged with
/// the `ipc_id` it arrived with. A task that panicked answers with an error,
/// so the proxy's waiter never hangs on it.
/// How long a cancelled connection waits for request tasks to finish and
/// their replies to be written.
const REPLY_FLUSH_WINDOW: Duration = Duration::from_millis(250);

/// Shutdown cancels only after running tool calls drain, so a request task can
/// hold a finished reply, or be a moment from one, when the connection is
/// cancelled. Write those replies before the connection closes. Tasks still
/// running after the window are aborted by the caller.
async fn flush_finished_replies(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    in_flight: &mut InFlight,
) {
    let _ = tokio::time::timeout(REPLY_FLUSH_WINDOW, async {
        while let Some(joined) = in_flight.tasks.join_next_with_id().await {
            write_in_flight_reply(writer, &mut in_flight.ids, joined).await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
}

async fn write_in_flight_reply(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    ids: &mut std::collections::HashMap<tokio::task::Id, u64>,
    joined: Result<(tokio::task::Id, IpcResponse), tokio::task::JoinError>,
) -> anyhow::Result<()> {
    let (task_id, response) = match joined {
        Ok((task_id, response)) => (task_id, response),
        Err(error) => {
            tracing::error!(error = %error, "IPC request task failed");
            (
                error.id(),
                IpcResponse::Error {
                    code: "INTERNAL_ERROR".to_string(),
                    message: "request handler failed".to_string(),
                },
            )
        }
    };
    let Some(ipc_id) = ids.remove(&task_id) else {
        return Ok(());
    };
    ipc::send_chunked_response_with_id(writer, Some(ipc_id), &response).await
}

/// True when `request` is an `IpcRequest::Deregister`. On SUCCESS, the
/// Deregister handler in `dispatch_request` clears `ctx.reverse_request_rx`
/// and drops the bridge channel's sender (closing it), so `handle_ipc_loop`'s
/// post-dispatch restore step must skip putting the (now-closed) taken-out
/// channel back — otherwise the next request's select loop busy-spins on
/// `Ready(None)`. This predicate alone is NOT sufficient to skip the restore:
/// a Deregister that fails validation early-returns without touching the
/// bridge, leaving the taken-out channel live, so the call site additionally
/// gates the skip on the response being `IpcResponse::Ok`.
fn is_deregister_request(request: &IpcRequest) -> bool {
    matches!(request, IpcRequest::Deregister { .. })
}

/// Whether this connection's client may be sent what `recv` holds. See
/// [`plug_core::proxy::ToolRouter::client_may_be_told`].
fn may_tell(
    (engine, registry, session_id): (&Engine, &ClientRegistry, Option<&str>),
    recv: &Result<
        plug_core::notifications::ProtocolNotification,
        tokio::sync::broadcast::error::RecvError,
    >,
) -> bool {
    let Ok(notification) = recv else {
        return true;
    };
    let key = session_id.and_then(|session_id| registry.client_key(session_id));
    engine
        .tool_router()
        .client_may_be_told(key.as_deref(), notification)
}

impl ConnectionContext {
    /// Who [`may_tell`] is asked about on this connection.
    fn listener(&self) -> (&Engine, &ClientRegistry, Option<&str>) {
        (
            &self.engine,
            &self.client_registry,
            self.session_id.as_deref(),
        )
    }
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

/// Forward a reverse request from the `DaemonBridge` to the proxy.
///
/// Writes it as a `DaemonToProxyMessage::ReverseRequest` and parks the
/// bridge's reply channel under the request id. The proxy answers with an
/// `IpcClientResponse` carrying that id as `reverse_id`, which the connection
/// loop hands to `deliver_reverse_reply`; other requests keep flowing while
/// the proxy works on it.
async fn send_reverse_request(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    pending: &mut PendingReverse,
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

    // A caller that gave up has no use for its reply.
    pending.retain(|_, waiter| !waiter.is_closed());
    pending.insert(id, response_tx);
    tracing::debug!(
        reverse_request_id = id,
        "sending reverse request to IPC proxy"
    );
    ipc::send_daemon_message(writer, &msg).await
}

/// Hand the proxy's reply for reverse request `reverse_id` to its waiter.
fn deliver_reverse_reply(pending: &mut PendingReverse, reverse_id: u64, frame: &[u8]) {
    let response = match serde_json::from_slice::<IpcClientResponse>(frame) {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(error = %e, "invalid IPC reverse-request response");
            IpcClientResponse::Error {
                message: format!("invalid reverse-request response: {e}"),
            }
        }
    };
    match pending.remove(&reverse_id) {
        Some(tx) => {
            let _ = tx.send(response);
        }
        None => tracing::debug!(
            reverse_request_id = reverse_id,
            "late reverse-request reply"
        ),
    }
}

async fn dispatch_operator_mutation(
    ctx: &ConnectionContext,
    mutation: plug_core::operator::OperatorMutation,
) -> IpcResponse {
    dispatch_operator_mutation_keeping(ctx, mutation, None).await
}

async fn dispatch_operator_mutation_keeping(
    ctx: &ConnectionContext,
    mutation: plug_core::operator::OperatorMutation,
    secret_store: Option<&str>,
) -> IpcResponse {
    match ctx
        .engine
        .apply_operator_mutation_keeping(&ctx.config_path, mutation, secret_store)
        .await
    {
        Ok((result, reload)) => IpcResponse::OperatorMutation { result, reload },
        Err(error) => IpcResponse::Error {
            code: "OPERATOR_MUTATION_FAILED".to_string(),
            message: error.to_string(),
        },
    }
}

fn current_daemon_executable() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::canonicalize(path).ok())
}

/// Dispatch a single IPC request to the appropriate Engine query.
/// Drop upstream branding icons from an operator snapshot's server statuses.
///
/// An icon is a base64 data URI, and a server that advertises a large one makes
/// its status entry dwarf every other server in the snapshot combined. The
/// snapshot is polled every couple of seconds to show health, tool counts and
/// live sessions; branding is not part of that and no operator client reads it
/// here. Tool listings still carry icons, which is where a client renders them,
/// and `plug servers --output json` reads the unmodified statuses from the
/// separate `Status` request.
fn strip_upstream_icons(statuses: &mut [plug_core::types::ServerStatus]) {
    for status in statuses {
        if let Some(upstream) = status.upstream.as_mut() {
            upstream.icons = None;
        }
    }
}

/// A value that changes whenever `ListTools` would answer differently.
///
/// The router's own revision covers the merged upstream catalog. It does not
/// cover `disabled_tools`, which the operator tool list reads straight from the
/// live configuration and which the CLI can edit without any catalog refresh,
/// so hash that in as well.
fn tool_catalog_revision(ctx: &ConnectionContext) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ctx.engine
        .tool_router()
        .catalog_revision()
        .hash(&mut hasher);
    ctx.engine.config().disabled_tools.hash(&mut hasher);
    hasher.finish()
}

async fn dispatch_request(request: &IpcRequest, ctx: &mut ConnectionContext) -> IpcResponse {
    if let Some(session_id) = request.connection_session_id()
        && let Some(refusal) = reject_unowned_session(ctx, session_id)
    {
        return refusal;
    }
    match request {
        IpcRequest::Status => {
            let servers = ctx.server_manager.server_statuses();
            IpcResponse::Status {
                servers,
                clients: ctx.client_registry.count(),
                uptime_secs: ctx.started_at.elapsed().as_secs(),
                runtime_version: env!("CARGO_PKG_VERSION").to_string(),
                resource_subscriptions: ctx.engine.tool_router().active_subscription_count(),
            }
        }
        IpcRequest::OperatorHandshake { .. } => IpcResponse::OperatorHandshake {
            handshake: plug_core::ipc::OperatorHandshake {
                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                daemon_executable: current_daemon_executable(),
                ipc_min: plug_core::ipc::OPERATOR_IPC_MIN,
                ipc_max: plug_core::ipc::OPERATOR_IPC_MAX,
                ownership: crate::service::ipc_ownership(),
                capabilities: vec![
                    plug_core::ipc::OperatorCapability::ServerMutation,
                    plug_core::ipc::OperatorCapability::ClientMutation,
                    plug_core::ipc::OperatorCapability::AuthMutation,
                    plug_core::ipc::OperatorCapability::ConfigMutation,
                    plug_core::ipc::OperatorCapability::ActivityStream,
                    plug_core::ipc::OperatorCapability::ToolMutation,
                    plug_core::ipc::OperatorCapability::ServerConfigRead,
                ],
            },
        },
        IpcRequest::ActivitySnapshot {
            after_sequence,
            limit,
            failures_only,
            ..
        } => IpcResponse::ActivitySnapshot {
            events: ctx.engine.tool_router().activity_snapshot(
                &plug_core::activity::ActivityFilter {
                    after_sequence: *after_sequence,
                    failures_only: *failures_only,
                    limit: *limit,
                },
            ),
        },
        IpcRequest::OperatorSnapshot { .. } => {
            // The configured servers come from the file, so the app can see a
            // server the daemon has not loaded. A file that does not parse,
            // often one saved mid-edit, must not blank the app while the
            // daemon keeps serving its last good config: report the error and
            // list the running config instead.
            let (config, config_error) =
                match plug_core::config::load_editable_config(&ctx.config_path) {
                    Ok(config) => (std::sync::Arc::new(config), None),
                    Err(error) => (
                        std::sync::Arc::clone(&ctx.engine.config()),
                        Some(config_error_summary(&error.to_string())),
                    ),
                };
            let mut configured_servers = config
                .servers
                .iter()
                .map(|(name, server)| {
                    plug_core::operator::OperatorServerSummary::from_config(name.clone(), server)
                })
                .collect::<Vec<_>>();
            configured_servers.sort_by(|a, b| a.name.cmp(&b.name));
            let live_sessions = plug_core::session::live_sessions(
                ctx.client_registry.list_live_sessions(),
                &ctx.http.sessions,
            );
            let client_visibility = live_sessions
                .iter()
                .map(|session| plug_core::ipc::OperatorClientVisibility {
                    session_id: session.session_id.clone(),
                    client_type: session.client_type,
                    visible_tool_count: ctx
                        .engine
                        .tool_router()
                        .list_tools_for_client_session(
                            session.client_type,
                            Some(live_session_lazy_key(session).as_str()),
                            session.access_key.as_deref(),
                        )
                        .len(),
                    client_key: session.client_key.clone(),
                })
                .collect();
            let client_names = config
                .clients
                .iter()
                .filter_map(|(key, settings)| {
                    Some(plug_core::ipc::ClientName {
                        key: key.clone(),
                        name: settings.name.clone()?,
                    })
                })
                .collect();
            let client_places = config
                .clients
                .iter()
                .filter_map(|(key, settings)| {
                    Some(plug_core::ipc::ClientPlace {
                        key: key.clone(),
                        place: settings.place.clone()?,
                    })
                })
                .collect();
            let client_blocks = config
                .clients
                .iter()
                .filter(|(_, settings)| {
                    !settings.blocked_servers.is_empty()
                        || !settings.blocked_tools.is_empty()
                        || settings.has_allow_list()
                })
                .map(|(key, settings)| plug_core::ipc::ClientBlocks {
                    key: key.clone(),
                    servers: settings.blocked_servers.clone(),
                    only_allowed: settings.has_allow_list(),
                    partly_allowed_servers: {
                        let mut servers: Vec<String> = settings
                            .allowed_tools
                            .iter()
                            .map(|allowed| allowed.server.clone())
                            .collect();
                        servers.sort();
                        servers.dedup();
                        servers
                    },
                    allowed_servers: settings.allowed_servers.clone(),
                    allowed_tools: settings
                        .allowed_tools
                        .iter()
                        .map(|allowed| {
                            ctx.engine
                                .tool_router()
                                .listed_name(&allowed.server, &allowed.tool)
                                // The form `plug clients only --remove`
                                // takes for a tool Plug no longer lists.
                                .unwrap_or_else(|| format!("{}/{}", allowed.server, allowed.tool))
                        })
                        .collect(),
                    // By the name each tool is listed under now, which is
                    // how the app and the command line know a tool.
                    tools: settings
                        .blocked_tools
                        .iter()
                        .map(|block| match block {
                            plug_core::config::ToolBlock::Of { server, tool } => ctx
                                .engine
                                .tool_router()
                                .listed_name(server, tool)
                                .unwrap_or_else(|| block.to_string()),
                            plug_core::config::ToolBlock::Named(name) => name.clone(),
                        })
                        .collect(),
                })
                .collect();
            let events = ctx.engine.event_statuses(&config).await;
            let mut server_statuses = ctx.server_manager.server_statuses();
            strip_upstream_icons(&mut server_statuses);
            let upstream_auth = auth_status_from_statuses(ctx, &server_statuses).await;
            let downstream_clients = match ctx.http.downstream_oauth() {
                Some(manager) => manager.list_clients().await,
                None => Vec::new(),
            };
            IpcResponse::OperatorSnapshot {
                snapshot: Box::new(plug_core::ipc::OperatorSnapshot {
                    runtime_version: env!("CARGO_PKG_VERSION").to_string(),
                    uptime_secs: ctx.started_at.elapsed().as_secs(),
                    tool_catalog_revision: tool_catalog_revision(ctx),
                    ownership: crate::service::ipc_ownership(),
                    configured_servers,
                    servers: server_statuses,
                    live_sessions,
                    client_visibility,
                    upstream_auth,
                    downstream_clients,
                    client_names,
                    client_places,
                    client_blocks,
                    events,
                    config_error,
                    client_address: Some(config.http.mcp_url()),
                }),
            }
        }
        IpcRequest::GetServerConfig { name, .. } => {
            let config = match load_editable_config(&ctx.config_path) {
                Ok(config) => config,
                Err(response) => return response,
            };
            match config.servers.get(name) {
                // Credentials never cross the socket. The client saves the
                // placeholders back and `apply_operator_mutation` restores the
                // stored values, so an edit round-trips without them.
                Some(server) => IpcResponse::ServerConfig {
                    name: name.clone(),
                    server: Box::new(server.redacted()),
                },
                None => IpcResponse::Error {
                    code: "UNKNOWN_SERVER".to_string(),
                    message: format!("unknown server `{name}`"),
                },
            }
        }
        IpcRequest::RevokeDownstreamClient { client_id, .. } => {
            match ctx.http.revoke_downstream_client(client_id).await {
                ClientRevocation::Revoked => IpcResponse::DownstreamClientRevoked {
                    client_id: client_id.clone(),
                },
                ClientRevocation::UnknownClient => IpcResponse::Error {
                    code: "UNKNOWN_DOWNSTREAM_CLIENT".to_string(),
                    message: format!("registered client `{client_id}` was not found"),
                },
                ClientRevocation::OauthDisabled => IpcResponse::Error {
                    code: "DOWNSTREAM_OAUTH_DISABLED".to_string(),
                    message: "downstream OAuth is not enabled".to_string(),
                },
                ClientRevocation::SaveFailed(error) => IpcResponse::Error {
                    code: "DOWNSTREAM_CLIENT_REVOKE_FAILED".to_string(),
                    message: error.to_string(),
                },
            }
        }
        IpcRequest::ValidateServer { name, server, .. } => {
            match ctx.engine.validate_server_draft(name, server) {
                Ok(server) => IpcResponse::ServerValidated { server },
                Err(error) => IpcResponse::Error {
                    code: "INVALID_SERVER".to_string(),
                    message: error.to_string(),
                },
            }
        }
        IpcRequest::DescribeApi { spec, .. } => {
            match plug_core::openapi::describe(spec, API_DESCRIBE_TIMEOUT).await {
                Ok(api) => IpcResponse::ApiDescribed { api },
                Err(error) => IpcResponse::Error {
                    code: "API_UNREADABLE".to_string(),
                    message: error.to_string(),
                },
            }
        }
        IpcRequest::AddServer {
            name,
            server,
            secret_store,
            ..
        } => {
            dispatch_operator_mutation_keeping(
                ctx,
                plug_core::operator::OperatorMutation::AddServer {
                    name: name.clone(),
                    server: *server.clone(),
                },
                secret_store.as_deref(),
            )
            .await
        }
        IpcRequest::UpdateServer {
            name,
            server,
            secret_store,
            ..
        } => {
            dispatch_operator_mutation_keeping(
                ctx,
                plug_core::operator::OperatorMutation::UpdateServer {
                    name: name.clone(),
                    server: *server.clone(),
                },
                secret_store.as_deref(),
            )
            .await
        }
        IpcRequest::RemoveServer { name, .. } => {
            dispatch_operator_mutation(
                ctx,
                plug_core::operator::OperatorMutation::RemoveServer { name: name.clone() },
            )
            .await
        }
        IpcRequest::AddAccount {
            server, account, ..
        } => {
            dispatch_operator_mutation(
                ctx,
                plug_core::operator::OperatorMutation::AddAccount {
                    server: server.clone(),
                    account: account.clone(),
                },
            )
            .await
        }
        IpcRequest::SetServerEnabled { name, enabled, .. } => {
            dispatch_operator_mutation(
                ctx,
                plug_core::operator::OperatorMutation::SetServerEnabled {
                    name: name.clone(),
                    enabled: *enabled,
                },
            )
            .await
        }
        IpcRequest::SetToolEnabled { tool, enabled, .. } => {
            dispatch_operator_mutation(
                ctx,
                plug_core::operator::OperatorMutation::SetToolEnabled {
                    tool: tool.clone(),
                    enabled: *enabled,
                },
            )
            .await
        }
        IpcRequest::RenameClient { key, name, .. } => {
            dispatch_operator_mutation(
                ctx,
                plug_core::operator::OperatorMutation::RenameClient {
                    key: key.clone(),
                    name: name.clone(),
                },
            )
            .await
        }
        IpcRequest::MoveClientSettings { from, to, .. } => {
            dispatch_operator_mutation(
                ctx,
                plug_core::operator::OperatorMutation::MoveClientSettings {
                    from: from.clone(),
                    to: to.clone(),
                },
            )
            .await
        }
        IpcRequest::SetClientPlace { key, place, .. } => {
            dispatch_operator_mutation(
                ctx,
                plug_core::operator::OperatorMutation::SetClientPlace {
                    key: key.clone(),
                    place: place.clone(),
                },
            )
            .await
        }
        IpcRequest::SetClientBlock {
            key,
            kind,
            target,
            blocked,
            ..
        } => {
            dispatch_operator_mutation(
                ctx,
                plug_core::operator::OperatorMutation::SetClientBlock {
                    key: key.clone(),
                    kind: *kind,
                    target: target.clone(),
                    blocked: *blocked,
                },
            )
            .await
        }
        IpcRequest::AddWatch { watch, .. } => {
            if let Err(error) = ctx.engine.check_watch(watch) {
                return IpcResponse::Error {
                    code: "OPERATOR_MUTATION_FAILED".to_string(),
                    message: error.to_string(),
                };
            }
            dispatch_operator_mutation(
                ctx,
                plug_core::operator::OperatorMutation::AddWatch {
                    watch: watch.clone(),
                },
            )
            .await
        }
        IpcRequest::RemoveWatch { event, .. } => {
            dispatch_operator_mutation(
                ctx,
                plug_core::operator::OperatorMutation::RemoveWatch {
                    event: event.clone(),
                },
            )
            .await
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
                Ok(new_config) => {
                    let report = ctx.engine.reload_config(new_config).await;
                    tracing::info!(
                        added = report.added.len(),
                        removed = report.removed.len(),
                        changed = report.changed.len(),
                        "config reloaded via IPC"
                    );
                    IpcResponse::Reloaded { report }
                }
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
            adapter_version,
            host,
            link_target,
        } => {
            if *protocol_version != plug_core::ipc::IPC_PROTOCOL_VERSION {
                return IpcResponse::error(
                    ipc::IpcErrorCode::ProtocolVersionUnsupported,
                    format!(
                        "daemon supports IPC protocol v{}, got v{}",
                        plug_core::ipc::IPC_PROTOCOL_VERSION,
                        protocol_version
                    ),
                );
            }
            let previous_session_id = ctx.session_id.take();
            let registration = ctx.client_registry.register_hosted(
                client_id.clone(),
                client_info.clone(),
                adapter_version.clone(),
                host.clone(),
            );
            ctx.client_registry
                .set_link_target(&registration.session_id, link_target.as_deref());
            if let Some(ref replaced_session_id) = registration.replaced_session_id {
                tracing::info!(
                    client_id = %client_id,
                    replaced_session_id = %replaced_session_id,
                    new_session_id = %registration.session_id,
                    "client transport session replaced"
                );
                ctx.engine
                    .tool_router()
                    .end_downstream_session(EndedSession::Ipc {
                        session_id: replaced_session_id,
                        // The new registration holds the same client id.
                        connector: ConnectorAfter::StillRegistered,
                    })
                    .await;
            }
            // One registration per connection: one this connection made
            // earlier for another client id ends now.
            if let Some(previous) = previous_session_id
                && registration.replaced_session_id.as_ref() != Some(&previous)
            {
                end_ipc_session(ctx, &previous).await;
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
                modern_downstream_enabled: ctx.engine.tool_router().modern_downstream_enabled(),
                cancellation_capability: plug_core::ipc::IpcCancellationCapability::new(
                    registration.cancellation_capability,
                ),
                request_ceiling_ms: plug_core::ipc::proxy_request_ceiling(&ctx.engine.config())
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            }
        }

        IpcRequest::Deregister { session_id } => {
            end_ipc_session(ctx, session_id).await;
            ctx.session_id = None;
            ctx.reverse_request_rx = None;
            IpcResponse::Ok
        }

        IpcRequest::UpdateSession {
            session_id,
            client_info,
        } => {
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

        IpcRequest::Ping { .. } => IpcResponse::Pong,

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
                    let own = tool_router.own_tool(tool.name.as_ref());
                    plug_core::ipc::IpcToolInfo {
                        own_name: own.as_ref().map(|(name, _)| name.clone()),
                        read_only: own.is_some_and(|(_, read_only)| read_only),
                        source: config
                            .servers
                            .get(&server_id)
                            .map(plug_core::ipc::IpcServerSourceInfo::from_config),
                        trust: plug_core::ipc::IpcTrustInfo::for_server(
                            &server_id,
                            config.servers.get(&server_id),
                        ),
                        risk: plug_core::ipc::IpcToolRiskInfo::from_annotations(
                            risk.upstream_declared.as_ref(),
                            risk.plug_inferred.as_ref(),
                            risk.effective.as_ref(),
                        ),
                        disabled: plug_core::tool_patterns::is_disabled_tool(
                            &config.disabled_tools,
                            tool.name.as_ref(),
                        ),
                        disabled_by_pattern: plug_core::tool_patterns::disabling_wildcard(
                            &config.disabled_tools,
                            tool.name.as_ref(),
                        ),
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
        IpcRequest::ListLiveSessions => IpcResponse::LiveSessions {
            sessions: plug_core::session::live_sessions(
                ctx.client_registry.list_live_sessions(),
                &ctx.http.sessions,
            ),
            scope: plug_core::ipc::LiveSessionInventoryScope::TransportComplete,
        },
        IpcRequest::Capabilities { session_id } => {
            // Capabilities are negotiated once per session and never
            // revisited, so answering from a catalog that is still filling
            // would tell a client the daemon has no tools and leave it with no
            // way to learn otherwise. This is the one answer that must not be
            // partial.
            ctx.engine.wait_until_ready().await;
            let caps = plug_core::dispatch::server_capabilities(
                ctx.engine.tool_router(),
                &mcp_dispatch::ipc_context(ctx, session_id, None),
            );
            match serde_json::to_value(caps) {
                Ok(capabilities) => IpcResponse::Capabilities { capabilities },
                Err(error) => IpcResponse::Error {
                    code: "SERIALIZE_ERROR".to_string(),
                    message: error.to_string(),
                },
            }
        }

        IpcRequest::ModernDownstreamGate { .. } => IpcResponse::ModernDownstreamGate {
            enabled: ctx.engine.tool_router().modern_downstream_enabled(),
        },

        IpcRequest::UpdateRoots { session_id, roots } => {
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

        IpcRequest::RestoreResourceSubscriptions { session_id, uris } => {
            let target = plug_core::notifications::NotificationTarget::Ipc {
                client_id: Arc::from(session_id.as_str()),
            };
            let tool_router = Arc::clone(ctx.engine.tool_router());
            let client_key = ctx.client_registry.client_key(session_id);
            let uris = uris.clone();
            const MAX_RESTORE_URIS: usize = 256;
            const MAX_RESTORE_IN_FLIGHT: usize = 8;
            if uris.len() > MAX_RESTORE_URIS {
                return IpcResponse::Error {
                    code: "RESTORE_SUBSCRIPTIONS_TOO_LARGE".to_string(),
                    message: format!(
                        "refusing to restore {} subscriptions (max {MAX_RESTORE_URIS})",
                        uris.len()
                    ),
                };
            }
            let mut join_set = tokio::task::JoinSet::new();
            let mut failures = Vec::new();
            for uri in uris {
                while join_set.len() >= MAX_RESTORE_IN_FLIGHT {
                    match join_set.join_next().await {
                        Some(Ok(Err(failure))) => failures.push(failure),
                        Some(Err(join_error)) => {
                            failures.push(format!("subscribe task failed: {join_error}"))
                        }
                        Some(Ok(Ok(()))) | None => {}
                    }
                }
                let tool_router = Arc::clone(&tool_router);
                let target = target.clone();
                let client_key = client_key.clone();
                join_set.spawn(async move {
                    tool_router
                        .subscribe_resource(&uri, target, client_key.as_deref())
                        .await
                        .map_err(|error| format!("{uri}: {error}"))
                });
            }
            while let Some(joined) = join_set.join_next().await {
                match joined {
                    Ok(Err(failure)) => failures.push(failure),
                    Err(join_error) => {
                        failures.push(format!("subscribe task failed: {join_error}"))
                    }
                    Ok(Ok(())) => {}
                }
            }
            if failures.is_empty() {
                IpcResponse::Ok
            } else {
                let preview: Vec<_> = failures.into_iter().take(8).collect();
                IpcResponse::Error {
                    code: "RESTORE_SUBSCRIPTIONS_PARTIAL".to_string(),
                    message: preview.join("; "),
                }
            }
        }

        IpcRequest::McpRequest {
            session_id,
            method,
            params,
        } => dispatch_mcp_request(ctx, session_id, method, params.as_ref(), None).await,

        IpcRequest::McpRequestWithContext {
            session_id,
            method,
            params,
            context,
        } => dispatch_mcp_request(ctx, session_id, method, params.as_ref(), Some(context)).await,

        IpcRequest::CancelMcpRequest {
            session_id,
            client_id,
            cancellation_capability,
            request_id,
            reason,
        } => {
            // Cancellation intentionally arrives on a short-lived auxiliary
            // socket: the primary socket may be blocked waiting for the call
            // being cancelled. Authorize it against the active registry
            // binding instead of this connection's session field.
            if let Some(response) = reject_invalid_cancellation_identity(
                &ctx.client_registry,
                session_id,
                client_id,
                cancellation_capability,
            ) {
                return response;
            }

            let client_type = ctx
                .client_registry
                .client_info(session_id)
                .as_deref()
                .map(plug_core::client_detect::detect_client)
                .unwrap_or(plug_core::types::ClientType::Unknown);
            let downstream = plug_core::proxy::DownstreamCallContext::ipc(
                session_id.as_str(),
                request_id.clone(),
                client_type,
                Some(client_id),
            )
            .with_client_key(ctx.client_registry.client_key(session_id));
            ctx.engine
                .tool_router()
                .cancel_downstream_request(&downstream, reason.clone());
            IpcResponse::Ok
        }

        IpcRequest::AuthStatus => dispatch_auth_status(ctx).await,
    }
}

/// Reject a request naming a session this connection does not own
/// (`SESSION_MISMATCH`) or one a newer registration has replaced
/// (`SESSION_REPLACED`).
fn reject_unowned_session(ctx: &ConnectionContext, session_id: &str) -> Option<IpcResponse> {
    if ctx.session_id.as_deref() != Some(session_id) {
        return Some(IpcResponse::error(
            ipc::IpcErrorCode::SessionMismatch,
            "session_id does not match this connection",
        ));
    }
    if !ctx.client_registry.session_exists(session_id) {
        return Some(IpcResponse::error(
            ipc::IpcErrorCode::SessionReplaced,
            "session is no longer active for this client",
        ));
    }
    None
}

fn reject_invalid_cancellation_identity(
    registry: &ClientRegistry,
    session_id: &str,
    client_id: &str,
    cancellation_capability: &plug_core::ipc::IpcCancellationCapability,
) -> Option<IpcResponse> {
    if !registry.session_exists(session_id) {
        return Some(IpcResponse::error(
            ipc::IpcErrorCode::SessionReplaced,
            "session is no longer active for this client",
        ));
    }
    if registry.client_id(session_id).as_deref() != Some(client_id) {
        return Some(IpcResponse::Error {
            code: "CLIENT_MISMATCH".to_string(),
            message: "session_id is not owned by client_id".to_string(),
        });
    }
    if !registry
        .cancellation_capability_matches(session_id, cancellation_capability.expose_secret())
    {
        return Some(IpcResponse::Error {
            code: "INVALID_CANCELLATION_CAPABILITY".to_string(),
            message: "cancellation capability is invalid".to_string(),
        });
    }
    None
}

fn protocol_parse_error_response(frame: &[u8]) -> Option<IpcResponse> {
    let value: serde_json::Value = serde_json::from_slice(frame).ok()?;
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("Register")
            if value.get("protocol_version").is_none() || value.get("client_id").is_none() =>
        {
            Some(IpcResponse::error(
                ipc::IpcErrorCode::ProtocolVersionUnsupported,
                format!(
                    "daemon requires IPC protocol v{} registration fields",
                    plug_core::ipc::IPC_PROTOCOL_VERSION
                ),
            ))
        }
        _ => None,
    }
}

// ──────────────────────── Unix signal handling ───────────────────────────────

/// How long a signalled daemon lets running tool calls finish before it tears
/// down. The launchd plists set `ExitTimeOut` to twenty seconds, which leaves
/// room for upstream shutdown after the drain.
const CALL_DRAIN_TIMEOUT: Duration = Duration::from_secs(8);

/// Wait for SIGTERM or SIGINT (for daemon mode), then let running tool calls
/// finish before cancelling. Clients and transports stay up while the calls
/// drain, so an update or a stop does not fail a call an agent is waiting on.
///
/// Systemd sends SIGTERM for graceful shutdown, not SIGINT.
#[cfg(unix)]
pub async fn shutdown_signal(cancel: CancellationToken, router: Arc<ToolRouter>) {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");
    tokio::select! {
        _ = sigterm.recv() => {
            tracing::info!("received SIGTERM");
            drain_calls(&router).await;
        }
        _ = sigint.recv() => {
            tracing::info!("received SIGINT");
            drain_calls(&router).await;
        }
        _ = cancel.cancelled() => {}
    }
    cancel.cancel();
}

async fn drain_calls(router: &ToolRouter) {
    let started = Instant::now();
    let remaining = router.drain_calls(CALL_DRAIN_TIMEOUT).await;
    let waited_ms = started.elapsed().as_millis() as u64;
    if remaining == 0 {
        tracing::info!(waited_ms, "tool calls drained");
    } else {
        tracing::warn!(waited_ms, remaining, "tool calls still running at shutdown");
    }
}

/// Fallback for non-Unix platforms.
#[cfg(not(unix))]
pub async fn shutdown_signal(cancel: CancellationToken, _router: Arc<ToolRouter>) {
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
    filter: tracing_subscriber::EnvFilter,
) -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    ensure_dir(log_directory)?;

    let file_appender = ReopeningLog::open(log_directory, LOG_FILE_CHECK_EVERY)?;
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .json()
        .init();

    Ok(guard)
}

/// Daily log files kept, today's included. At about a megabyte a day this is
/// two weeks of history for `scripts/perf.sh` and post-mortems.
const LOG_FILES_KEPT: usize = 14;

/// `plug.log.YYYY-MM-DD`, one file a day, pruning past [`LOG_FILES_KEPT`] when
/// the daemon starts and at each rollover.
fn daily_log_appender(
    log_directory: &std::path::Path,
) -> anyhow::Result<tracing_appender::rolling::RollingFileAppender> {
    Ok(tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(plug_core::paths::LOG_FILE_PREFIX)
        .max_log_files(LOG_FILES_KEPT)
        .build(log_directory)?)
}

/// How often the log writer checks that its file is still on disk.
const LOG_FILE_CHECK_EVERY: Duration = Duration::from_secs(30);

/// The daily log, reopened when its file is deleted underneath it.
///
/// A disk cleaner that empties the log folder leaves the daemon writing to a
/// file that no longer has a name, so nothing it logs can be read until the
/// next midnight rollover. This notices within [`LOG_FILE_CHECK_EVERY`] and
/// starts a new file.
struct ReopeningLog {
    directory: std::path::PathBuf,
    appender: tracing_appender::rolling::RollingFileAppender,
    check_every: Duration,
    checked: std::time::Instant,
}

impl ReopeningLog {
    fn open(directory: &std::path::Path, check_every: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            directory: directory.to_path_buf(),
            appender: daily_log_appender(directory)?,
            check_every,
            checked: std::time::Instant::now(),
        })
    }

    fn has_a_file(&self) -> bool {
        std::fs::read_dir(&self.directory).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(plug_core::paths::LOG_FILE_PREFIX)
            })
        })
    }

    fn reopen_if_deleted(&mut self) {
        if self.checked.elapsed() < self.check_every {
            return;
        }
        self.checked = std::time::Instant::now();
        if self.has_a_file() {
            return;
        }
        if ensure_dir(&self.directory).is_ok()
            && let Ok(appender) = daily_log_appender(&self.directory)
        {
            self.appender = appender;
        }
    }
}

impl std::io::Write for ReopeningLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.reopen_if_deleted();
        self.appender.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.appender.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plug_core::test_helpers::mock_server;

    #[test]
    fn the_log_starts_a_new_file_when_its_folder_is_emptied() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("plug-log-reopen-{}", uuid::Uuid::new_v4()));
        let mut log = ReopeningLog::open(&dir, Duration::ZERO).expect("open the log");
        log.write_all(b"before\n").expect("write");
        log.flush().expect("flush");

        std::fs::remove_dir_all(&dir).expect("empty the log folder");
        log.write_all(b"after\n").expect("write");
        log.flush().expect("flush");

        let file = std::fs::read_dir(&dir)
            .expect("the folder is back")
            .flatten()
            .next()
            .expect("a new log file");
        let text = std::fs::read_to_string(file.path()).expect("read the new file");
        assert_eq!(text, "after\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One server advertising a large icon can outweigh every other server in
    /// the snapshot, and the snapshot is polled every couple of seconds.
    #[test]
    fn operator_server_statuses_carry_no_upstream_icons() {
        let icon = rmcp::model::Icon::new(format!("data:image/png;base64,{}", "A".repeat(8192)))
            .with_mime_type("image/png");
        let mut statuses = vec![plug_core::types::ServerStatus {
            server_id: "branded".to_string(),
            health: plug_core::types::ServerHealth::Healthy,
            tool_count: 1,
            auth_status: "none".to_string(),
            upstream: Some(plug_core::types::UpstreamServerMetadata {
                name: "branded".to_string(),
                version: "1.0.0".to_string(),
                title: Some("Branded".to_string()),
                description: None,
                website_url: None,
                icons: Some(vec![icon]),
                selected_protocol_version: None,
            }),
            metrics: None,
            availability: plug_core::types::Availability::Healthy,
            selected_protocol_era: None,
            selected_protocol_version: None,
            error: None,
            last_seen: None,
        }];

        strip_upstream_icons(&mut statuses);

        let upstream = statuses[0].upstream.as_ref().expect("metadata is kept");
        assert!(upstream.icons.is_none(), "icons must not ride the snapshot");
        assert_eq!(upstream.title.as_deref(), Some("Branded"));
        assert!(
            serde_json::to_string(&statuses).expect("serialize").len() < 512,
            "the entry must no longer be dominated by branding"
        );
    }

    use oauth2::{AccessToken, RefreshToken, basic::BasicTokenType};
    use rmcp::transport::auth::{CredentialStore, StoredCredentials, VendorExtraTokenFields};
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::AsyncWriteExt;

    pub(super) fn temp_config_path(name: &str) -> std::path::PathBuf {
        crate::install_test_credential_environment();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("plug-daemon-{name}-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    pub(super) async fn clear_store(server_name: &str) {
        let store = plug_core::oauth::get_or_create_store(server_name);
        store.clear().await.unwrap();
    }

    pub(super) fn cleanup_temp_config(config_path: &std::path::Path) {
        if let Some(dir) = config_path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Daily logs used to be kept forever. Pruning must keep the file names
    /// `scripts/perf.sh` globs and leave other files in the directory alone.
    #[test]
    fn daily_log_appender_prunes_old_logs_and_keeps_names() {
        let dir = std::env::temp_dir().join(format!(
            "plug-log-retention-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let old: Vec<String> = (1..=20)
            .map(|day| format!("plug.log.2000-01-{day:02}"))
            .collect();
        for name in &old {
            std::fs::write(dir.join(name), "").unwrap();
        }
        std::fs::write(dir.join("installation-reconciliation.log"), "").unwrap();

        drop(daily_log_appender(&dir).unwrap());

        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        let logs: Vec<&String> = names
            .iter()
            .filter(|name| name.starts_with("plug.log."))
            .collect();
        assert_eq!(logs.len(), LOG_FILES_KEPT, "{names:?}");
        let today: Vec<&&String> = logs.iter().filter(|name| !old.contains(name)).collect();
        assert_eq!(today.len(), 1, "{names:?}");
        let date = today[0].strip_prefix("plug.log.").unwrap();
        assert!(
            date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-',
            "today's log must be plug.log.YYYY-MM-DD, got {}",
            today[0]
        );
        assert!(names.contains(&"installation-reconciliation.log".to_string()));
    }

    #[test]
    fn reported_daemon_executable_is_canonical() {
        let expected = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(current_daemon_executable(), Some(expected));
    }

    #[test]
    fn cancellation_identity_cannot_cross_client_boundary() {
        let registry = ClientRegistry::new();
        let first = registry.register("first-client".to_string(), Some("client".to_string()), None);
        let second = registry.register(
            "second-client".to_string(),
            Some("client".to_string()),
            None,
        );

        assert!(
            reject_invalid_cancellation_identity(
                &registry,
                &first.session_id,
                "first-client",
                &plug_core::ipc::IpcCancellationCapability::new(
                    first.cancellation_capability.clone(),
                ),
            )
            .is_none()
        );
        let error = reject_invalid_cancellation_identity(
            &registry,
            &first.session_id,
            "second-client",
            &plug_core::ipc::IpcCancellationCapability::new(second.cancellation_capability.clone()),
        )
        .expect("second client must not cancel first client's request");
        assert!(matches!(
            error,
            IpcResponse::Error { ref code, .. } if code == "CLIENT_MISMATCH"
        ));
        assert!(
            reject_invalid_cancellation_identity(
                &registry,
                &second.session_id,
                "second-client",
                &plug_core::ipc::IpcCancellationCapability::new(second.cancellation_capability,),
            )
            .is_none()
        );

        let forged = reject_invalid_cancellation_identity(
            &registry,
            &first.session_id,
            "first-client",
            &plug_core::ipc::IpcCancellationCapability::new("forged".to_string()),
        )
        .expect("enumerated ids without capability must fail");
        assert!(matches!(
            forged,
            IpcResponse::Error { ref code, .. } if code == "INVALID_CANCELLATION_CAPABILITY"
        ));
    }

    #[test]
    fn daemon_tests_do_not_use_the_real_plug_token_directory() {
        let config_path = temp_config_path("credential-isolation");
        let production = plug_core::paths::dir().join("tokens");
        let actual = plug_core::oauth::tokens_dir();

        assert_ne!(actual, production);
        assert!(actual.starts_with(std::env::temp_dir()));
        cleanup_temp_config(&config_path);
    }

    pub(super) fn write_oauth_config(path: &std::path::Path, servers: &[&str]) {
        write_oauth_config_with_enabled(path, servers, true);
    }

    pub(super) fn write_oauth_config_with_enabled(
        path: &std::path::Path,
        servers: &[&str],
        enabled: bool,
    ) {
        let mut config = plug_core::config::Config::default();
        for name in servers {
            config.servers.insert(
                (*name).to_string(),
                plug_core::config::ServerConfig {
                    enabled,
                    transport: plug_core::config::TransportType::Http,
                    url: Some("https://example.com/mcp".to_string()),
                    auth: Some("oauth".to_string()),
                    oauth_client_id: Some("test-client".to_string()),
                    oauth_scopes: Some(vec!["read".to_string()]),
                    call_timeout_secs: 30,
                    max_concurrent: 4,
                    circuit_breaker_enabled: false,
                    ..Default::default()
                },
            );
        }
        std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
    }

    pub(super) fn seeded_credentials() -> StoredCredentials {
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

    pub(super) fn auth_status_test_context(config_path: std::path::PathBuf) -> ConnectionContext {
        let config = plug_core::config::load_config(Some(&config_path)).unwrap();
        let engine = Arc::new(Engine::new(config));
        let client_registry = ClientRegistry::new();
        test_connection_context(
            &engine,
            CancellationToken::new(),
            client_registry,
            config_path,
        )
    }

    /// A connection that has not registered yet, holding the token `test-token`.
    fn test_connection_context(
        engine: &Arc<Engine>,
        cancel: CancellationToken,
        client_registry: ClientRegistry,
        config_path: impl Into<std::path::PathBuf>,
    ) -> ConnectionContext {
        ConnectionContext {
            cancel,
            auth_token: Arc::from("test-token"),
            server_manager: Arc::clone(engine.server_manager()),
            engine: Arc::clone(engine),
            config_path: config_path.into(),
            started_at: Instant::now(),
            client_registry: Arc::new(client_registry),
            http: Arc::new(HttpState::new(
                Arc::clone(engine.tool_router()),
                Arc::new(plug_core::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )),
            session_id: None,
            reverse_request_rx: None,
        }
    }

    /// The app polls this snapshot every few seconds. A config.toml saved
    /// mid-edit used to fail the whole snapshot and blank the app while the
    /// daemon kept serving its last good config.
    #[tokio::test]
    async fn operator_snapshot_survives_an_unparsable_config_file() {
        let config_path = temp_config_path("snapshot-bad-config");
        let server_name = format!("oauth-snapshot-{}", std::process::id());
        write_oauth_config(&config_path, &[server_name.as_str()]);
        clear_store(&server_name).await;
        let mut ctx = auth_status_test_context(config_path);
        std::fs::write(
            &ctx.config_path,
            "[servers.broken]\nauth_token = \"sk-secret-value\n",
        )
        .unwrap();

        let response = dispatch_request(
            &IpcRequest::OperatorSnapshot {
                auth_token: "test-token".to_string(),
            },
            &mut ctx,
        )
        .await;
        let IpcResponse::OperatorSnapshot { snapshot } = response else {
            panic!("expected a snapshot, got {response:?}");
        };

        let config_error = snapshot.config_error.as_deref().expect("config_error");
        assert!(config_error.contains("line 2"), "{config_error}");
        assert!(
            !config_error.contains("sk-secret-value"),
            "the error must not quote the config: {config_error}"
        );
        let configured: Vec<_> = snapshot
            .configured_servers
            .iter()
            .map(|server| server.name.as_str())
            .collect();
        assert_eq!(configured, vec![server_name.as_str()]);
        assert_eq!(snapshot.upstream_auth.len(), 1);
        assert_eq!(snapshot.upstream_auth[0].name, server_name);

        clear_store(&server_name).await;
        cleanup_temp_config(&ctx.config_path);
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
        let session_id = ctx.http.sessions.create_session().expect("session");
        ctx.http
            .sessions
            .set_client_type(&session_id, plug_core::types::ClientType::ClaudeDesktop)
            .expect("set client type");

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

    /// The app and `plug auth clients revoke` revoke over the socket. That
    /// used to end the client's sessions but leave the tasks it owned.
    #[tokio::test]
    async fn revoking_a_client_over_ipc_ends_its_sessions_and_its_tasks() {
        let mut config = plug_core::config::Config::default();
        config
            .servers
            .insert("mock".to_string(), mock_server("echo"));
        let engine = Arc::new(Engine::new(config));
        engine.start().await.expect("engine start");
        let oauth_path =
            std::env::temp_dir().join(format!("plug-daemon-revoke-{}.json", uuid::Uuid::new_v4()));
        let manager = plug_core::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            plug_core::downstream_oauth::DownstreamOauthConfig {
                public_base_url: "https://plug.example.com".to_string(),
                oauth_scopes: vec!["tools:read".to_string()],
            },
            oauth_path.clone(),
        )
        .expect("OAuth manager");
        let client = manager
            .register_client(
                plug_core::downstream_oauth::ClientRegistrationRequest {
                    redirect_uris: vec!["https://client.example/callback".to_string()],
                    client_name: Some("revoked client".to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: Some(vec!["authorization_code".to_string()]),
                    response_types: Some(vec!["code".to_string()]),
                    scope: None,
                },
                "daemon-revoke-test",
            )
            .await
            .expect("register client");
        let mut ctx = test_connection_context(
            &engine,
            CancellationToken::new(),
            ClientRegistry::new(),
            "/tmp/plug-daemon-revoke-config.toml",
        );
        ctx.http = Arc::new(
            HttpState::new(
                Arc::clone(engine.tool_router()),
                Arc::new(plug_core::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_auth(plug_core::http::server::HttpAuth::Oauth(manager.clone())),
        );
        let session_id = ctx.http.sessions.create_session().expect("session");
        ctx.http
            .sessions
            .set_grant(&session_id, client.client_id.clone())
            .expect("set grant");
        let resource = manager.resource();
        let owner = plug_core::downstream_identity::HttpCredential::Grant {
            issuer: manager.base_url(),
            client_id: &client.client_id,
            resource: &resource,
        }
        .task_owner(None);
        let router = Arc::clone(engine.tool_router());
        router
            .enqueue_tool_task("Mock__echo", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue a task for the client");

        let response = dispatch_request(
            &IpcRequest::RevokeDownstreamClient {
                auth_token: "test-token".to_string(),
                client_id: client.client_id.clone(),
            },
            &mut ctx,
        )
        .await;

        assert!(
            matches!(response, IpcResponse::DownstreamClientRevoked { .. }),
            "{response:?}"
        );
        assert!(ctx.http.sessions.validate(&session_id).is_err());
        assert_eq!(router.task_count_for_owner(&owner).await, 0);
        engine.shutdown().await;
        let _ = std::fs::remove_file(oauth_path);
    }

    #[test]
    fn pid_lock_survives_the_momentary_hold_a_status_probe_takes() {
        // `ipc_client::daemon_starting` takes the lock and lets it go again, and every
        // status surface calls it. A start that collided with one used to fail
        // outright. The window is passed explicitly so the assertion is about
        // retrying at all, not about how a loaded machine schedules threads.
        let temp = std::env::temp_dir().join(format!(
            "plug-daemon-pid-lock-probe-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temp).expect("create temp dir");
        let pid_file = temp.join("plug.pid");

        let held = acquire_pid_lock(&pid_file).expect("probe holds the lock");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            drop(held);
        });

        let claiming = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&pid_file)
            .expect("open pid file");
        let claimed = lock_with_retry(&claiming, &pid_file, std::time::Duration::from_secs(10));
        releaser.join().expect("probe thread");
        assert!(
            claimed.is_ok(),
            "a start must outwait a probe-length hold: {:?}",
            claimed.err()
        );

        drop(claiming);
        std::fs::remove_dir_all(temp).expect("cleanup temp dir");
    }

    #[test]
    fn pid_lock_still_refuses_a_second_daemon_that_keeps_holding_it() {
        let temp = std::env::temp_dir().join(format!(
            "plug-daemon-pid-lock-held-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temp).expect("create temp dir");
        let pid_file = temp.join("plug.pid");

        let _held = acquire_pid_lock(&pid_file).expect("first pid lock");
        let error = acquire_pid_lock(&pid_file).expect_err("second pid lock must fail");
        assert!(
            error.to_string().contains("already running"),
            "unexpected error: {error}"
        );

        std::fs::remove_dir_all(temp).expect("cleanup temp dir");
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
    async fn losing_daemon_start_does_not_boot_upstream_servers() {
        let _guard = runtime_paths_test_lock().lock().await;
        let runtime_root = std::env::temp_dir().join(format!(
            "plug-daemon-early-lock-runtime-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let state_root = std::env::temp_dir().join(format!(
            "plug-daemon-early-lock-state-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        std::fs::create_dir_all(plug_core::paths::dir()).expect("create daemon runtime dir");
        let winning_lock = acquire_pid_lock(&pid_path()).expect("winning daemon lock");

        let marker = state_root.join("upstream-started");
        let config_path = state_root.join("config.toml");
        let mut config = plug_core::config::Config::default();
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("reserve HTTP port");
        config.http.port = probe.local_addr().expect("HTTP address").port();
        drop(probe);
        config.servers.insert(
            "marker".to_string(),
            plug_core::config::ServerConfig {
                command: Some("/bin/sh".to_string()),
                args: vec!["-c".to_string(), "touch \"$MARKER\"".to_string()],
                env: HashMap::from([("MARKER".to_string(), marker.display().to_string())]),
                timeout_secs: 1,
                call_timeout_secs: 1,
                circuit_breaker_enabled: false,
                ..Default::default()
            },
        );
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let result = crate::runtime::cmd_daemon(Some(&config_path)).await;
        let error = result.expect_err("losing daemon should exit immediately");
        assert!(
            error.to_string().contains("PID file locked"),
            "startup should reach the singleton lock: {error:#}"
        );
        assert!(
            !marker.exists(),
            "a daemon that loses the singleton lock must not start upstream servers"
        );

        drop(winning_lock);
        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    /// `run_daemon` must bind and serve without a started engine, which is what
    /// lets `cmd_daemon` run `Engine::start` alongside it. `Engine::start` waits
    /// on third-party server processes, so a socket bound after it makes a
    /// healthy cold start look like an absent daemon for as long as the slowest
    /// upstream takes.
    #[tokio::test]
    async fn ipc_socket_is_bound_while_upstream_startup_is_still_running() {
        let _guard = runtime_paths_test_lock().lock().await;
        // The bound socket lives under this root, and a Unix socket path has to
        // fit in SUN_LEN, so keep the directory names short.
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let runtime_root = std::env::temp_dir().join(format!("plug-eb-{}", &suffix[..8]));
        let state_root = std::env::temp_dir().join(format!("plug-ebs-{}", &suffix[..8]));
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        // `/bin/sleep` never completes an MCP handshake, so this start stays in
        // flight until the upstream timeout expires.
        let mut server = mock_server("noop");
        server.command = Some("/bin/sleep".to_string());
        server.args = vec!["30".to_string()];
        server.timeout_secs = 20;
        let mut config = plug_core::config::Config::default();
        config.servers.insert("slow".to_string(), server);

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = Arc::new(Engine::new(config));

        let startup = {
            let engine = engine.clone();
            tokio::spawn(async move { engine.start().await })
        };
        let daemon = {
            let engine = engine.clone();
            let config_path = config_path.clone();
            tokio::spawn(async move { run_daemon(engine, config_path).await })
        };

        let sock_path = socket_path();
        let mut connected = false;
        for _ in 0..50 {
            if tokio::net::UnixStream::connect(&sock_path).await.is_ok() {
                connected = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        if !connected && daemon.is_finished() {
            panic!("daemon exited early: {:?}", daemon.await);
        }
        assert!(connected, "socket never accepted a connection");
        assert!(
            !startup.is_finished(),
            "upstream startup finished first, so this run proves nothing"
        );

        engine.cancel_token().cancel();
        startup.abort();
        daemon.abort();
        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).ok();
        std::fs::remove_dir_all(state_root).ok();
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

        let rt_dir = plug_core::paths::dir();
        std::fs::create_dir_all(&rt_dir).expect("create daemon runtime dir");
        let token_file = token_path();
        std::fs::write(&token_file, "stable-token").expect("write seed token");
        let pid_file = pid_path();
        let winning_lock = acquire_pid_lock(&pid_file).expect("winning pid lock");

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = Arc::new(Engine::new(plug_core::config::Config::default()));

        let result = run_daemon(engine, config_path).await;
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

        let rt_dir = plug_core::paths::dir();
        std::fs::create_dir_all(&rt_dir).expect("create daemon runtime dir");
        let token_file = token_path();
        std::fs::write(&token_file, "stable-token").expect("write seed token");
        let sock_path = socket_path();
        std::fs::create_dir_all(&sock_path).expect("create blocking socket directory");

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = Arc::new(Engine::new(plug_core::config::Config::default()));

        let result = run_daemon(engine, config_path).await;
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

    #[test]
    fn is_deregister_request_detects_only_deregister() {
        // Regression guard for the restore-skip predicate in
        // `handle_ipc_loop`: only a Deregister request should suppress
        // restoring `reverse_request_rx`, since only Deregister's handler
        // closes the channel out from under the dispatch loop.
        assert!(is_deregister_request(&IpcRequest::Deregister {
            session_id: "session-123".to_string(),
        }));

        assert!(!is_deregister_request(&IpcRequest::Status));
        assert!(!is_deregister_request(&IpcRequest::Ping {
            session_id: "session-123".to_string(),
        }));
        assert!(!is_deregister_request(&IpcRequest::Register {
            protocol_version: plug_core::ipc::IPC_PROTOCOL_VERSION,
            client_id: "client-123".to_string(),
            client_info: None,
            adapter_version: None,
            host: None,
            link_target: None,
        }));
    }

    #[tokio::test]
    async fn daemon_bridge_rejects_elicitation_without_capability() {
        use plug_core::proxy::DownstreamBridge;
        use rmcp::model::{ClientCapabilities, ElicitRequestParams};

        let registry = ClientRegistry::new();
        let registry = Arc::new(registry);
        let reg_result =
            registry.register("test-client".to_string(), Some("test".to_string()), None);
        // Capabilities default to None for elicitation/sampling

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let bridge = DaemonBridge {
            session_id: Arc::from(reg_result.session_id.as_str()),
            reverse_request_tx: tx,
            client_registry: Arc::clone(&registry),
        };

        // Build a minimal elicitation request via JSON deserialization
        let request: ElicitRequestParams = serde_json::from_value(serde_json::json!({
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

        let request2: ElicitRequestParams = serde_json::from_value(serde_json::json!({
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

        let registry = ClientRegistry::new();
        let registry = Arc::new(registry);
        let reg_result =
            registry.register("test-client".to_string(), Some("test".to_string()), None);

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

    /// `mock_server` with extra mock-server flags appended (e.g. to enable
    /// the resources / prompts / completions capability fixtures).
    fn ipc_harness_mock_config_with(
        tools: &str,
        extra_args: &[&str],
    ) -> plug_core::config::ServerConfig {
        let mut server = mock_server(tools);
        server.args.extend(extra_args.iter().map(|a| a.to_string()));
        server
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
            Self::start_with_config(mock_server(tools)).await
        }

        /// Full-capability variant for the parity matrix: same upstream config as
        /// the HTTP parity driver so every method family is comparable.
        async fn start_full() -> Self {
            Self::start_with_config(parity_mock_config()).await
        }

        async fn start_with_config(mock: plug_core::config::ServerConfig) -> Self {
            Self::start_with_servers(vec![("mock", mock)]).await
        }

        async fn start_with_servers(servers: Vec<(&str, plug_core::config::ServerConfig)>) -> Self {
            Self::start_registered(servers, plug_core::ipc::IPC_PROTOCOL_VERSION).await
        }

        /// Register at `protocol_version`, as a proxy of that version would.
        async fn start_registered(
            servers: Vec<(&str, plug_core::config::ServerConfig)>,
            protocol_version: u16,
        ) -> Self {
            let mut config = plug_core::config::Config::default();
            for (name, server) in servers {
                config.servers.insert(name.to_string(), server);
            }
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
            let client_registry = ClientRegistry::new();
            let ctx = test_connection_context(
                &engine,
                cancel.clone(),
                client_registry,
                "/tmp/plug-ipc-test-config.toml",
            );
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
                    protocol_version,
                    client_id: uuid::Uuid::new_v4().to_string(),
                    client_info: Some("plug-test".to_string()),
                    adapter_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                    host: None,
                    link_target: None,
                },
            )
            .await;
            let session_id = match read_ipc_response(&mut stream).await {
                IpcResponse::Registered {
                    session_id,
                    protocol_version: registered,
                    ..
                } => {
                    assert_eq!(registered, protocol_version, "daemon echoes the version");
                    session_id
                }
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
            if resp.is_push() {
                continue;
            }
            return resp;
        }
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

    /// After a `Deregister`, the bridge channel's sender is dropped (closed)
    /// and `ctx.reverse_request_rx` is cleared. Sending another request on
    /// the same connection must not resurrect that closed channel — the
    /// direct regression guard for the restore-skip predicate is
    /// `is_deregister_request_detects_only_deregister`; this test documents
    /// the end-to-end sequence still completing normally afterward, matching
    /// the plan's minimum bar (pre-fix this also passes functionally, since
    /// a busy-spun `Ready(None)` arm still lets the dispatch future resolve —
    /// it just burns CPU doing so).
    #[tokio::test]
    async fn deregister_then_status_request_completes_normally() {
        let mut harness = IpcTestHarness::start("echo").await;

        write_ipc(
            &mut harness.stream,
            &IpcRequest::Deregister {
                session_id: harness.session_id.clone(),
            },
        )
        .await;
        let deregister_resp = read_ipc_response(&mut harness.stream).await;
        assert!(
            matches!(deregister_resp, IpcResponse::Ok),
            "expected Ok, got {deregister_resp:?}"
        );

        write_ipc(&mut harness.stream, &IpcRequest::Status).await;
        let status_resp = read_ipc_response(&mut harness.stream).await;

        harness.shutdown().await;

        assert!(
            matches!(status_resp, IpcResponse::Status { .. }),
            "expected Status, got {status_resp:?}"
        );
    }

    /// Bug 4 regression: an elicitation/sampling request arriving on
    /// `ctx.reverse_request_rx` while a registered connection is idle (no
    /// request currently being dispatched) must reach the client without the
    /// client sending another frame first — pre-fix, the registered-idle
    /// `select!` had no arm for `reverse_request_rx`, so the request would
    /// hang until the next client-initiated request happened to service it.
    ///
    /// This injects directly into a hand-built reverse-request channel
    /// (mirroring `daemon_bridge_rejects_elicitation_without_capability`)
    /// rather than driving it through a real task-wrapped tool call: the
    /// background-task dispatch path (`execute_tool_task` in
    /// `plug-core/src/proxy/tasks.rs`) doesn't register into `active_calls`,
    /// so upstream-triggered reverse-request routing for it is a separate
    /// concern from the idle-select servicing arm this test targets.
    #[tokio::test]
    async fn idle_registered_connection_services_reverse_request_without_new_client_frame() {
        let config = plug_core::config::Config::default();
        let engine = Arc::new(Engine::new(config));
        engine.start().await.expect("engine start");

        let socket_path = std::path::PathBuf::from(format!(
            "/tmp/plug-ipc-idle-reverse-{}.sock",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ));
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind temp socket");
        let cancel = CancellationToken::new();

        let client_registry = ClientRegistry::new();
        let session_id = "idle-reverse-session".to_string();

        // Pre-seed `session_id` and a hand-built reverse-request channel
        // directly on the initial context, bypassing a real `Register` call
        // (whose handler would create its own channel and discard ours).
        let (reverse_tx, reverse_rx) = tokio::sync::mpsc::channel(8);
        let mut ctx = test_connection_context(
            &engine,
            cancel.clone(),
            client_registry,
            "/tmp/plug-ipc-idle-reverse-config.toml",
        );
        ctx.session_id = Some(session_id.clone());
        ctx.reverse_request_rx = Some(reverse_rx);

        let server_task = tokio::spawn(async move {
            if let Ok((stream, _addr)) = listener.accept().await {
                let _ = handle_ipc_connection(stream, ctx).await;
            }
        });

        let mut stream = tokio::net::UnixStream::connect(&socket_path)
            .await
            .expect("client connect");

        // One throwaway request: `log_rx`/`ctrl_rx` only get subscribed right
        // after a request completes while `ctx.session_id` is already
        // `Some`, which is what routes the connection into the labeled
        // select loop this bug lives in for subsequent idling.
        write_ipc(&mut stream, &IpcRequest::Status).await;
        let status = read_ipc_response(&mut stream).await;
        assert!(
            matches!(status, IpcResponse::Status { .. }),
            "expected Status, got {status:?}"
        );

        // Inject a reverse request directly into the connection's channel.
        // No further client frame is sent before reading the pushed frame
        // below — that's the crux of the regression.
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let request = plug_core::ipc::IpcClientRequest::CreateElicitation {
            params: serde_json::from_value(serde_json::json!({
                "message": "idle test",
                "requestedSchema": { "type": "object", "properties": {} }
            }))
            .unwrap(),
        };
        reverse_tx
            .send((request, resp_tx))
            .await
            .expect("send reverse request into idle connection");

        let frame = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            plug_core::ipc::read_frame(&mut stream),
        )
        .await
        .expect("timed out waiting for the idle-time reverse request")
        .expect("read frame")
        .expect("unexpected EOF waiting for reverse request");

        let msg: plug_core::ipc::DaemonToProxyMessage =
            serde_json::from_slice(&frame).expect("decode DaemonToProxyMessage");
        let reverse_id = match msg {
            plug_core::ipc::DaemonToProxyMessage::ReverseRequest { id, request } => {
                assert!(
                    matches!(
                        *request,
                        plug_core::ipc::IpcClientRequest::CreateElicitation { .. }
                    ),
                    "expected CreateElicitation, got {request:?}"
                );
                id
            }
            other => panic!("expected ReverseRequest envelope, got {other:?}"),
        };

        // The reply carries the request's id and reaches the bridge waiter.
        let reply = IpcClientResponse::Error {
            message: "test reply".to_string(),
        };
        plug_core::ipc::write_frame(
            &mut stream,
            &plug_core::ipc::encode_reverse_response(reverse_id, &reply).unwrap(),
        )
        .await
        .expect("write reverse-request reply");
        let delivered = tokio::time::timeout(std::time::Duration::from_secs(10), resp_rx)
            .await
            .expect("reverse reply routed in time")
            .expect("reverse reply delivered");
        assert!(
            matches!(delivered, IpcClientResponse::Error { ref message } if message == "test reply"),
            "got {delivered:?}"
        );

        cancel.cancel();
        drop(stream);
        let _ = server_task.await;
        engine.shutdown().await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// The operator snapshot counts visible tools from the same working-set
    /// key each transport writes, not the raw session id.
    #[test]
    fn live_session_lazy_key_matches_the_transport_key() {
        let session = |transport| plug_core::ipc::IpcLiveSessionInfo {
            transport,
            client_id: None,
            session_id: "sess-1".to_string(),
            client_type: plug_core::types::ClientType::Unknown,
            client_info: None,
            adapter_version: None,
            host: None,
            grant: None,
            link_target: None,
            client_key: None,
            access_key: None,
            connected_secs: 0,
            last_activity_secs: None,
        };
        let ipc =
            plug_core::downstream_identity::DownstreamTransport::Ipc.lazy_session_key("sess-1");
        let http =
            plug_core::downstream_identity::DownstreamTransport::Http.lazy_session_key("sess-1");
        assert_eq!(
            live_session_lazy_key(&session(plug_core::ipc::LiveSessionTransport::DaemonProxy)),
            ipc
        );
        assert_eq!(
            live_session_lazy_key(&session(plug_core::ipc::LiveSessionTransport::Http)),
            http
        );
        assert_eq!(
            live_session_lazy_key(&session(plug_core::ipc::LiveSessionTransport::Sse)),
            http
        );
    }

    /// A registered connection hears about a modern downstream gate change
    /// without sending another request.
    #[tokio::test]
    async fn registered_connection_is_pushed_modern_gate_changes() {
        let engine = Arc::new(Engine::new(plug_core::config::Config::default()));
        engine.start().await.expect("engine start");

        let socket_path = std::path::PathBuf::from(format!(
            "/tmp/plug-ipc-gate-push-{}.sock",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ));
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind temp socket");
        let cancel = CancellationToken::new();
        let client_registry = ClientRegistry::new();
        let mut ctx = test_connection_context(
            &engine,
            cancel.clone(),
            client_registry,
            "/tmp/plug-ipc-gate-push-config.toml",
        );
        ctx.session_id = Some("gate-push-session".to_string());
        let server_task = tokio::spawn(async move {
            if let Ok((stream, _addr)) = listener.accept().await {
                let _ = handle_ipc_connection(stream, ctx).await;
            }
        });

        let mut stream = tokio::net::UnixStream::connect(&socket_path)
            .await
            .expect("client connect");
        // Enter the registered-idle loop.
        write_ipc(&mut stream, &IpcRequest::Status).await;
        let status = read_ipc_response(&mut stream).await;
        assert!(matches!(status, IpcResponse::Status { .. }), "{status:?}");

        for expected in [true, false] {
            engine.tool_router().set_modern_downstream_enabled(expected);
            let pushed = loop {
                let frame = tokio::time::timeout(
                    Duration::from_secs(5),
                    plug_core::ipc::read_frame(&mut stream),
                )
                .await
                .expect("timed out waiting for the gate push")
                .expect("read frame")
                .expect("unexpected EOF");
                let response: IpcResponse = serde_json::from_slice(&frame).expect("decode frame");
                if let IpcResponse::ModernDownstreamGateChanged { enabled } = response {
                    break enabled;
                }
            };
            assert_eq!(pushed, expected);
        }

        cancel.cancel();
        drop(stream);
        let _ = server_task.await;
        engine.shutdown().await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// A Deregister that FAILS validation (here: SESSION_MISMATCH) must not
    /// kill the connection's live reverse-request channel. The Deregister
    /// handler only clears `ctx.reverse_request_rx` and drops the bridge's
    /// sender on its SUCCESS path — the validation early-returns leave the
    /// channel untouched. If the dispatch loop's restore-skip predicate keyed
    /// on the request TYPE alone (rather than request type AND `Ok` response),
    /// the still-live receiver taken out for the dispatch would be dropped at
    /// end of iteration, closing the bridge sender and permanently breaking
    /// elicitation/sampling for a session that is still active.
    #[tokio::test]
    async fn failed_deregister_does_not_kill_reverse_channel() {
        let config = plug_core::config::Config::default();
        let engine = Arc::new(Engine::new(config));
        engine.start().await.expect("engine start");

        let socket_path = std::path::PathBuf::from(format!(
            "/tmp/plug-ipc-failed-dereg-{}.sock",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ));
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind temp socket");
        let cancel = CancellationToken::new();

        let client_registry = ClientRegistry::new();
        let session_id = "failed-dereg-session".to_string();

        // Same hand-built harness as
        // `idle_registered_connection_services_reverse_request_without_new_client_frame`:
        // pre-seed `session_id` and a hand-built reverse-request channel.
        let (reverse_tx, reverse_rx) = tokio::sync::mpsc::channel(8);
        let mut ctx = test_connection_context(
            &engine,
            cancel.clone(),
            client_registry,
            "/tmp/plug-ipc-failed-dereg-config.toml",
        );
        ctx.session_id = Some(session_id.clone());
        ctx.reverse_request_rx = Some(reverse_rx);

        let server_task = tokio::spawn(async move {
            if let Ok((stream, _addr)) = listener.accept().await {
                let _ = handle_ipc_connection(stream, ctx).await;
            }
        });

        let mut stream = tokio::net::UnixStream::connect(&socket_path)
            .await
            .expect("client connect");

        // Enter the registered-idle loop (subscribes log/ctrl channels).
        write_ipc(&mut stream, &IpcRequest::Status).await;
        let status = read_ipc_response(&mut stream).await;
        assert!(
            matches!(status, IpcResponse::Status { .. }),
            "expected Status, got {status:?}"
        );

        // Deregister with the WRONG session id — the handler must reject it
        // with SESSION_MISMATCH and leave the connection fully registered.
        write_ipc(
            &mut stream,
            &IpcRequest::Deregister {
                session_id: "not-the-session".to_string(),
            },
        )
        .await;
        let deregister_resp = read_ipc_response(&mut stream).await;
        match &deregister_resp {
            IpcResponse::Error { code, .. } => {
                assert_eq!(code, "SESSION_MISMATCH", "unexpected error code");
            }
            other => panic!("expected SESSION_MISMATCH error, got {other:?}"),
        }

        // The reverse channel must still be alive and serviced: inject a
        // reverse request with no further client frame and expect it pushed.
        let (resp_tx, _resp_rx) = tokio::sync::oneshot::channel();
        let request = plug_core::ipc::IpcClientRequest::CreateElicitation {
            params: serde_json::from_value(serde_json::json!({
                "message": "post-failed-deregister test",
                "requestedSchema": { "type": "object", "properties": {} }
            }))
            .unwrap(),
        };
        reverse_tx
            .send((request, resp_tx))
            .await
            .expect("reverse channel receiver was dropped — failed Deregister killed it");

        let frame = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            plug_core::ipc::read_frame(&mut stream),
        )
        .await
        .expect("timed out waiting for the reverse request after failed Deregister")
        .expect("read frame")
        .expect("unexpected EOF waiting for reverse request");

        let msg: plug_core::ipc::DaemonToProxyMessage =
            serde_json::from_slice(&frame).expect("decode DaemonToProxyMessage");
        let reverse_id = match msg {
            plug_core::ipc::DaemonToProxyMessage::ReverseRequest { id, request } => {
                assert!(
                    matches!(
                        *request,
                        plug_core::ipc::IpcClientRequest::CreateElicitation { .. }
                    ),
                    "expected CreateElicitation, got {request:?}"
                );
                id
            }
            other => panic!("expected ReverseRequest envelope, got {other:?}"),
        };

        // Answer the reverse request so everything winds down cleanly.
        let reply = IpcClientResponse::Error {
            message: "test reply".to_string(),
        };
        plug_core::ipc::write_frame(
            &mut stream,
            &plug_core::ipc::encode_reverse_response(reverse_id, &reply).unwrap(),
        )
        .await
        .expect("write reverse-request reply");

        cancel.cancel();
        drop(stream);
        let _ = server_task.await;
        engine.shutdown().await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// Regression test for the frame-reader cancellation-safety fix.
    ///
    /// Writes a request frame in two pieces (with a deliberate gap between
    /// them) while flooding the daemon's logging broadcast channel, so the
    /// registered-client `select!` races several ready notification arms
    /// against a not-yet-complete frame read. Before the fix, `ipc::read_frame`
    /// was polled directly inside that `select!`; when a notification arm won
    /// the race while `read_frame` was parked mid-frame (e.g. having consumed
    /// only part of the 4-byte length prefix), the future was dropped and the
    /// already-consumed bytes were lost — the next read started mid-frame,
    /// desyncing the length-prefixed protocol and killing the connection with
    /// a spurious frame error. After the fix, frame reads go through a
    /// dedicated reader task feeding an `mpsc` channel, whose `recv()` is
    /// cancellation-safe, so this should round-trip cleanly.
    ///
    /// Confirmed to reproduce pre-fix: reverting the `FrameReader` change and
    /// running this test fails with `frame too large: N bytes (max
    /// 4194304)` — the exact desync symptom this fix prevents.
    #[tokio::test]
    async fn ipc_registered_client_survives_notification_flood_during_fragmented_frame() {
        let mut harness = IpcTestHarness::start("echo").await;

        let request = IpcRequest::McpRequest {
            session_id: harness.session_id.clone(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "Mock__echo",
                "arguments": { "input": "fragmented-hello" }
            })),
        };
        let payload = serde_json::to_vec(&request).expect("serialize ipc request");
        let len = u32::try_from(payload.len()).expect("payload fits in u32");
        let len_bytes = len.to_be_bytes();

        harness
            .stream
            .write_all(&len_bytes[..2])
            .await
            .expect("write partial length prefix");
        harness.stream.flush().await.expect("flush partial write");

        // Give the daemon's connection task a chance to actually poll
        // `read_frame` and consume the 2 bytes already on the wire (parking
        // mid length-prefix, pending the remaining 2). Without this delay,
        // the notifications below could win the very first `select!` poll
        // before `read_frame` ever touches the socket, which would not
        // exercise the bug (nothing consumed yet, so nothing to lose).
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Flood the logging broadcast channel while the frame is mid-flight.
        // Each send makes the registered client's notification arm ready,
        // racing it against the still-incomplete frame read in the daemon's
        // `select!`. Uses `Error` level: the engine's default effective log
        // level is `Warning`, so anything below that is filtered out before
        // ever reaching the broadcast channel (see
        // `ToolRouter::route_upstream_logging_message`) and would not
        // actually race the frame read at all.
        for i in 0..5 {
            harness.engine.tool_router().route_upstream_logging_message(
                "test-server",
                rmcp::model::LoggingMessageNotificationParam::new(
                    rmcp::model::LoggingLevel::Error,
                    serde_json::json!(format!("flood-{i}")),
                ),
            );
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        harness
            .stream
            .write_all(&len_bytes[2..])
            .await
            .expect("write remaining length prefix");
        harness
            .stream
            .write_all(&payload)
            .await
            .expect("write frame body");
        harness.stream.flush().await.expect("flush frame body");

        let resp = read_ipc_response(&mut harness.stream).await;
        harness.shutdown().await;

        let IpcResponse::McpResponse { payload } = resp else {
            panic!(
                "expected McpResponse after fragmented frame + notification flood, got: {resp:?}"
            );
        };
        let text = payload["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("Called") && text.contains("fragmented-hello"),
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

    // ── Cross-transport parity matrix ────────────────────────────────────────
    //
    // Drives identical scenarios through the real HTTP and IPC downstream
    // transports and asserts identical decoded results and error codes, so a
    // change to one transport's adapter that the other does not share fails here.
    //
    // Each transport drives its own engine + mock upstream so the only variable is
    // the downstream transport. Envelopes (HTTP 200-with-error-body, IPC
    // McpResponse frame) are normalized to a transport-agnostic outcome first.

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum ParityOutcome {
        Success { text: String, is_error: bool },
        Error { code: i64 },
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
    /// family has real routed content to compare across transports. Both
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
        let state = Arc::new(plug_core::http::server::HttpState::new(
            engine.tool_router().clone(),
            Arc::new(plug_core::session::SessionStore::new(1800, 100)),
            CancellationToken::new(),
        ));
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
        // The same full-capability upstream as `parity_mock_engine`, so the only
        // variable in a parity row is the transport.
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

    /// Drive `method`+`params` through both transports and assert the normalized
    /// outcomes are identical, returning the agreed outcome for any further
    /// method-specific assertions. The core parity gate for a method.
    async fn assert_parity(method: &str, params: serde_json::Value) -> MethodOutcome {
        let http = parity_http_call(method, params.clone()).await;
        let ipc = parity_ipc_call(method, params.clone()).await;
        assert_eq!(http, ipc, "{method}: http vs ipc divergence");
        http
    }

    #[tokio::test]
    async fn parity_tools_call_success_matches_across_transports() {
        let args = serde_json::json!({ "input": "parity" });
        let http = parity_http("Mock__echo", args.clone()).await;
        let ipc = parity_ipc("Mock__echo", args.clone()).await;

        assert_eq!(http, ipc, "http vs ipc success divergence");
        // And it must actually be the echo success, not a coincidental match.
        match &http {
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
        let http = parity_http("Mock__missing", args.clone()).await;
        let ipc = parity_ipc("Mock__missing", args.clone()).await;

        // ToolNotFound -> METHOD_NOT_FOUND (-32601) on both.
        assert_eq!(http, ParityOutcome::Error { code: -32601 }, "http");
        assert_eq!(ipc, ParityOutcome::Error { code: -32601 }, "ipc");
    }

    /// An empty tool name is just another unknown tool.
    #[tokio::test]
    async fn parity_tools_call_empty_name_matches_across_transports() {
        let args = serde_json::json!({});
        let http = parity_http("", args.clone()).await;
        let ipc = parity_ipc("", args.clone()).await;

        assert_eq!(http, ParityOutcome::Error { code: -32601 }, "http");
        assert_eq!(ipc, ParityOutcome::Error { code: -32601 }, "ipc");
    }

    /// A legacy task-augmented call to a tool creates a plug-side passthrough
    /// task on both transports.
    #[tokio::test]
    async fn parity_task_augmented_call_creates_a_task_on_http_and_ipc() {
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

    // ── Method-family parity rows ────────────────────────────────────────────
    //
    // Each row drives one method through HTTP and IPC against the shared
    // full-capability mock and asserts the decoded results agree, plus a light
    // content check so a coincidental error on both can't pass as success.
    // Routed names use the "Mock" server prefix (server "mock" -> "Mock__name").
    // The fixtures fit in one page (PAGE_SIZE = 500), so list results carry no
    // cursor; first-page parity still asserts nextCursor agreement.

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

    /// A downstream client's `ping` is answered by RMCP inside `plug connect`
    /// and reaches the daemon only from a raw IPC speaker or the `plug/legacy/`
    /// escape hatch; it still answers like HTTP.
    #[tokio::test]
    async fn parity_ping_matches_across_transports() {
        match assert_parity("ping", serde_json::json!({})).await {
            MethodOutcome::Result(json) => {
                assert!(
                    json.as_object().is_some_and(|obj| obj.is_empty()),
                    "ping must return an empty result, got {json}"
                );
            }
            other => panic!("expected empty ping result, got {other:?}"),
        }
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
        // so both transports must surface the identical error code.
        let params = serde_json::json!({ "uri": "file:///does/not/exist" });
        // assert_parity already proves the transports agree on the code;
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

    // ── Subscribe/unsubscribe lifecycle parity ───────────────────────────────
    //
    // Each call is a fresh session, so the unsubscribe row also exercises the
    // idempotent (not-currently-subscribed) path.

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

    /// Send `request` tagged with `ipc_id`, as a v4 proxy does.
    async fn write_tagged_ipc(stream: &mut tokio::net::UnixStream, ipc_id: u64, req: &IpcRequest) {
        let payload = plug_core::ipc::encode_tagged(Some(ipc_id), req).expect("encode request");
        plug_core::ipc::write_frame(stream, &payload)
            .await
            .expect("write frame");
    }

    /// Read the next reply that carries an `ipc_id`, reassembling a chunked
    /// one. Fails if another frame lands between the chunks of a response.
    async fn read_tagged_reply(stream: &mut tokio::net::UnixStream) -> (u64, IpcResponse) {
        let mut decoder = plug_core::ipc::DaemonFrameDecoder::default();
        loop {
            let frame = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                plug_core::ipc::read_frame(stream),
            )
            .await
            .expect("timed out waiting for a tagged reply")
            .expect("read frame")
            .expect("unexpected EOF before reply");
            match decoder.decode(&frame).expect("decode frame") {
                None => {}
                Some(plug_core::ipc::DaemonFrame::Reply { response, .. }) if response.is_push() => {
                }
                Some(plug_core::ipc::DaemonFrame::Reply { ipc_id, response }) => {
                    let ipc_id = ipc_id.expect("a tagged request gets a tagged reply");
                    return (ipc_id, response);
                }
                Some(other) => panic!("unexpected frame {other:?}"),
            }
        }
    }

    fn slow_and_fast_servers() -> Vec<(&'static str, plug_core::config::ServerConfig)> {
        vec![
            (
                "slow",
                ipc_harness_mock_config_with("echo", &["--delay-ms", "1500"]),
            ),
            ("fast", mock_server("echo,chunked_text")),
        ]
    }

    fn tool_call_request(session_id: &str, name: &str) -> IpcRequest {
        IpcRequest::McpRequest {
            session_id: session_id.to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({ "name": name, "arguments": { "input": name } })),
        }
    }

    /// Shutdown waits for running tool calls, then cancels. A call that
    /// finished in that wait must still get its reply to the proxy before the
    /// connection closes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_call_that_finishes_during_the_shutdown_drain_still_gets_its_reply() {
        let mut harness = IpcTestHarness::start_with_servers(slow_and_fast_servers()).await;
        let session_id = harness.session_id.clone();
        write_tagged_ipc(
            &mut harness.stream,
            1,
            &tool_call_request(&session_id, "Slow__echo"),
        )
        .await;
        // Let the call reach the upstream, then shut down as the daemon does.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let remaining = harness
            .engine
            .tool_router()
            .drain_calls(Duration::from_secs(5))
            .await;
        assert_eq!(remaining, 0);
        harness.cancel.cancel();

        let (ipc_id, response) = read_tagged_reply(&mut harness.stream).await;
        assert_eq!(ipc_id, 1);
        assert!(
            matches!(response, IpcResponse::McpResponse { .. }),
            "{response:?}"
        );
        harness.shutdown().await;
    }

    /// Plug.app stops every connector with SIGSTOP while it swaps the
    /// daemon, so a proxy reads nothing during the drain. A reply larger
    /// than the 8 KB macOS default socket buffer must still be written in
    /// full before the connection closes, for the proxy to read on resume.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_drained_reply_reaches_a_proxy_that_is_not_reading() {
        let mut harness = IpcTestHarness::start_with_servers(slow_and_fast_servers()).await;
        let session_id = harness.session_id.clone();
        let input = "x".repeat(256 * 1024);
        let request = IpcRequest::McpRequest {
            session_id: session_id.clone(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "Slow__echo",
                "arguments": { "input": input },
            })),
        };
        write_tagged_ipc(&mut harness.stream, 1, &request).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let remaining = harness
            .engine
            .tool_router()
            .drain_calls(Duration::from_secs(5))
            .await;
        assert_eq!(remaining, 0);
        harness.cancel.cancel();
        // The proxy stays stopped well past the daemon's flush window.
        tokio::time::sleep(REPLY_FLUSH_WINDOW * 4).await;

        let (ipc_id, response) = read_tagged_reply(&mut harness.stream).await;
        assert_eq!(ipc_id, 1);
        let IpcResponse::McpResponse { payload } = response else {
            panic!("{response:?}");
        };
        let text = payload["content"][0]["text"].as_str().expect("text");
        assert!(
            text.contains(&input),
            "reply truncated to {} bytes",
            text.len()
        );
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn tagged_requests_on_one_connection_run_concurrently() {
        let mut harness = IpcTestHarness::start_with_servers(slow_and_fast_servers()).await;
        let session_id = harness.session_id.clone();

        // The slow call goes first; the big (chunked) and small calls behind
        // it must not wait for it.
        write_tagged_ipc(
            &mut harness.stream,
            1,
            &tool_call_request(&session_id, "Slow__echo"),
        )
        .await;
        write_tagged_ipc(
            &mut harness.stream,
            2,
            &tool_call_request(&session_id, "Fast__chunked_text"),
        )
        .await;
        write_tagged_ipc(
            &mut harness.stream,
            3,
            &tool_call_request(&session_id, "Fast__echo"),
        )
        .await;

        let mut order = Vec::new();
        for _ in 0..3 {
            let (ipc_id, response) = read_tagged_reply(&mut harness.stream).await;
            let IpcResponse::McpResponse { payload } = response else {
                panic!("request {ipc_id} failed: {response:?}");
            };
            let text = payload["content"][0]["text"]
                .as_str()
                .expect("text content")
                .to_string();
            match ipc_id {
                1 => assert!(text.contains("\"Slow__echo\""), "{text}"),
                2 => assert_eq!(text.len(), 6 * 1024 * 1024),
                3 => assert!(text.contains("\"Fast__echo\""), "{text}"),
                other => panic!("unknown ipc_id {other}"),
            }
            order.push(ipc_id);
        }
        assert_eq!(
            order.last(),
            Some(&1),
            "the slow call finishes last: {order:?}"
        );

        // An untagged request on the same connection still gets a plain reply.
        let reply = harness
            .call_tool("Fast__echo", serde_json::json!({ "input": "untagged" }))
            .await;
        assert!(
            matches!(reply, IpcResponse::McpResponse { .. }),
            "{reply:?}"
        );

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn client_disconnect_with_requests_in_flight_ends_the_connection() {
        let mut harness = IpcTestHarness::start_with_servers(slow_and_fast_servers()).await;
        let session_id = harness.session_id.clone();

        for (ipc_id, tool) in [(1, "Slow__echo"), (2, "Slow__echo"), (3, "Fast__echo")] {
            write_tagged_ipc(
                &mut harness.stream,
                ipc_id,
                &tool_call_request(&session_id, tool),
            )
            .await;
        }
        tokio::io::AsyncWriteExt::shutdown(&mut harness.stream)
            .await
            .expect("close client side");

        let server_task = harness.server_task.take().expect("server task");
        tokio::time::timeout(std::time::Duration::from_secs(10), server_task)
            .await
            .expect("the connection handler hung on its in-flight requests")
            .expect("server task join");

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn register_rejects_versions_outside_the_supported_range() {
        for version in [
            plug_core::ipc::IPC_PROTOCOL_VERSION - 1,
            plug_core::ipc::IPC_PROTOCOL_VERSION + 1,
        ] {
            let client_registry = ClientRegistry::new();
            let engine = Arc::new(Engine::new(plug_core::config::Config::default()));
            let mut ctx = test_connection_context(
                &engine,
                CancellationToken::new(),
                client_registry,
                "/tmp/plug-ipc-test-config.toml",
            );
            let response = dispatch_request(
                &IpcRequest::Register {
                    protocol_version: version,
                    client_id: "client".to_string(),
                    client_info: None,
                    adapter_version: None,
                    host: None,
                    link_target: None,
                },
                &mut ctx,
            )
            .await;
            assert!(
                matches!(response, IpcResponse::Error { ref code, .. } if code == "PROTOCOL_VERSION_UNSUPPORTED"),
                "v{version}: {response:?}"
            );
        }
    }

    /// With every request slot taken by long calls, one more call is refused
    /// at once and a heartbeat still answers. Shutdown then aborts the calls
    /// instead of waiting them out.
    #[tokio::test]
    async fn full_connection_refuses_more_calls_but_still_answers_pings() {
        let mut harness = IpcTestHarness::start_with_servers(vec![(
            "slow",
            ipc_harness_mock_config_with("echo", &["--delay-ms", "3000"]),
        )])
        .await;
        let session_id = harness.session_id.clone();

        let limit = ipc::MAX_CONCURRENT_REQUESTS_PER_CONNECTION as u64;
        for ipc_id in 1..=limit + 1 {
            write_tagged_ipc(
                &mut harness.stream,
                ipc_id,
                &tool_call_request(&session_id, "Slow__echo"),
            )
            .await;
        }
        write_tagged_ipc(
            &mut harness.stream,
            limit + 2,
            &IpcRequest::Ping {
                session_id: session_id.clone(),
            },
        )
        .await;

        let (ipc_id, busy) = read_tagged_reply(&mut harness.stream).await;
        assert_eq!(ipc_id, limit + 1);
        assert!(
            matches!(busy, IpcResponse::Error { ref code, .. } if code == "BUSY"),
            "{busy:?}"
        );
        let (ipc_id, pong) = read_tagged_reply(&mut harness.stream).await;
        assert_eq!(ipc_id, limit + 2);
        assert!(matches!(pong, IpcResponse::Pong), "{pong:?}");

        // The calls are still running; shutdown must not wait for them.
        harness.cancel.cancel();
        let server_task = harness.server_task.take().expect("server task");
        tokio::time::timeout(std::time::Duration::from_secs(2), server_task)
            .await
            .expect("shutdown waited on in-flight calls")
            .expect("server task join");
        harness.shutdown().await;
    }
}
