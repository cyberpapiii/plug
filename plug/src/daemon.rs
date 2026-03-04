//! Daemon mode — headless Engine with Unix socket IPC.
//!
//! Provides `plug serve --daemon` functionality: starts the Engine without TUI,
//! listens on a Unix socket for CLI queries, and logs to file.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use fs2::FileExt as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use plug_core::engine::Engine;
use plug_core::ipc::{self, IpcRequest, IpcResponse, MAX_FRAME_SIZE};

/// Maximum concurrent IPC connections.
const MAX_IPC_CONNECTIONS: usize = 32;

/// Idle timeout for IPC connections (no complete message received).
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

// ──────────────────────────────── Path helpers ────────────────────────────────

/// Return the daemon runtime directory (for socket + PID file + auth token).
///
/// - macOS: `~/Library/Application Support/plug/`
/// - Linux: `$XDG_RUNTIME_DIR/plug/` (fallback: `~/.local/state/plug/`)
pub fn runtime_dir() -> PathBuf {
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

/// Generate a 256-bit (32-byte) cryptographic random auth token, hex-encoded.
fn generate_auth_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Constant-time comparison of auth tokens to prevent timing side-channel.
fn verify_auth_token(provided: &str, expected: &str) -> bool {
    use subtle::ConstantTimeEq;
    let a = provided.as_bytes();
    let b = expected.as_bytes();
    // Lengths must match first (not timing-sensitive since token length is public)
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
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

// ──────────────────────── Length-prefixed framing ─────────────────────────────

/// Read a length-prefixed JSON frame from a Unix socket.
///
/// Wire format: 4-byte big-endian u32 length + JSON payload.
/// Returns None on clean EOF (connection closed).
async fn read_frame(
    stream: &mut tokio::net::unix::OwnedReadHalf,
) -> anyhow::Result<Option<Vec<u8>>> {
    // Read length prefix
    let len = match stream.read_u32().await {
        Ok(len) => len,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    // Enforce max frame size before allocating
    if len > MAX_FRAME_SIZE {
        anyhow::bail!("frame too large: {len} bytes (max {MAX_FRAME_SIZE})");
    }

    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Write a length-prefixed JSON frame to a Unix socket.
async fn write_frame(
    stream: &mut tokio::net::unix::OwnedWriteHalf,
    payload: &[u8],
) -> anyhow::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| anyhow::anyhow!("payload too large: {} bytes", payload.len()))?;
    stream.write_u32(len).await?;
    stream.write_all(payload).await?;
    stream.flush().await?;
    Ok(())
}

/// Send an IpcResponse as a length-prefixed JSON frame.
async fn send_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &IpcResponse,
) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(response)?;
    write_frame(writer, &payload).await
}

// ─────────────────────────── Daemon entry point ──────────────────────────────

/// Start the daemon: Engine + Unix socket IPC listener + file logging.
///
/// Returns the tracing-appender guard that MUST be held for the daemon's lifetime
/// (dropping it flushes and closes the log file).
pub async fn run_daemon(engine: &Engine) -> anyhow::Result<()> {
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
                .with_context(|| {
                    format!("failed to write auth token: {}", token_file.display())
                })?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&token_file, &auth_token).with_context(|| {
                format!("failed to write auth token: {}", token_file.display())
            })?;
        }
    }

    // Clean up stale socket if it exists
    let sock_path = socket_path();
    if std::fs::symlink_metadata(&sock_path).is_ok() {
        // Try connecting to check if another daemon is alive
        if tokio::net::UnixStream::connect(&sock_path).await.is_ok() {
            anyhow::bail!("another plug daemon is already running on {}", sock_path.display());
        }
        // Stale socket — remove it
        std::fs::remove_file(&sock_path).ok();
    }

    // Bind Unix socket BEFORE writing PID file
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("failed to bind Unix socket: {}", sock_path.display()))?;

    #[cfg(unix)]
    set_file_permissions_0600(&sock_path)?;

    // Acquire PID file lock AFTER socket bind
    let pid_file_path = pid_path();
    let _pid_lock = acquire_pid_lock(&pid_file_path)?;

    tracing::info!(
        socket = %sock_path.display(),
        pid_file = %pid_file_path.display(),
        "daemon started"
    );

    let cancel = engine.cancel_token().clone();
    let semaphore = Arc::new(Semaphore::new(MAX_IPC_CONNECTIONS));
    let auth_token: Arc<str> = Arc::from(auth_token.as_str());

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

                        tokio::spawn(async move {
                            let _permit = permit; // held for connection lifetime
                            let started_at = Instant::now()
                                .checked_sub(snapshot.uptime)
                                .unwrap_or_else(Instant::now);
                            let ctx = ConnectionContext {
                                cancel: engine_cancel,
                                auth_token: auth,
                                server_manager,
                                started_at,
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
    started_at: Instant,
}

/// Handle a single IPC connection: read requests, dispatch, send responses.
async fn handle_ipc_connection(
    stream: tokio::net::UnixStream,
    ctx: ConnectionContext,
) -> anyhow::Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    loop {
        // Read with idle timeout
        let frame = tokio::time::timeout(CONNECTION_IDLE_TIMEOUT, read_frame(&mut reader)).await;

        let frame = match frame {
            Ok(Ok(Some(data))) => data,
            Ok(Ok(None)) => break, // clean EOF
            Ok(Err(e)) => {
                let resp = IpcResponse::Error {
                    code: "FRAME_ERROR".to_string(),
                    message: e.to_string(),
                };
                send_response(&mut writer, &resp).await.ok();
                break;
            }
            Err(_) => {
                // Idle timeout
                tracing::debug!("IPC connection idle timeout");
                break;
            }
        };

        // Parse request
        let request: IpcRequest = match serde_json::from_slice(&frame) {
            Ok(req) => req,
            Err(e) => {
                let resp = IpcResponse::Error {
                    code: "PARSE_ERROR".to_string(),
                    message: format!("invalid JSON: {e}"),
                };
                send_response(&mut writer, &resp).await.ok();
                break;
            }
        };

        // Auth check for mutating commands
        if ipc::requires_auth(&request) {
            match ipc::extract_auth_token(&request) {
                Some(provided) => {
                    if !verify_auth_token(provided, &ctx.auth_token) {
                        let resp = IpcResponse::Error {
                            code: "AUTH_FAILED".to_string(),
                            message: "invalid auth token".to_string(),
                        };
                        send_response(&mut writer, &resp).await?;
                        continue;
                    }
                }
                None => {
                    let resp = IpcResponse::Error {
                        code: "AUTH_REQUIRED".to_string(),
                        message: "auth_token required for this command".to_string(),
                    };
                    send_response(&mut writer, &resp).await?;
                    continue;
                }
            }
        }

        // Dispatch request
        let response = dispatch_request(&request, &ctx);

        send_response(&mut writer, &response).await?;

        // Shutdown request — send OK then trigger cancel
        if matches!(request, IpcRequest::Shutdown { .. }) {
            ctx.cancel.cancel();
            break;
        }
    }

    Ok(())
}

/// Dispatch a single IPC request to the appropriate Engine query.
fn dispatch_request(request: &IpcRequest, ctx: &ConnectionContext) -> IpcResponse {
    match request {
        IpcRequest::Status => {
            let servers = ctx.server_manager.server_statuses();
            IpcResponse::Status {
                servers,
                clients: 0, // TODO: track client count in Engine
                uptime_secs: ctx.started_at.elapsed().as_secs(),
            }
        }
        IpcRequest::RestartServer { server_id, .. } => {
            let statuses = ctx.server_manager.server_statuses();
            if !statuses.iter().any(|s| s.server_id == *server_id) {
                return IpcResponse::Error {
                    code: "UNKNOWN_SERVER".to_string(),
                    message: format!("server '{server_id}' not found"),
                };
            }
            IpcResponse::Error {
                code: "NOT_IMPLEMENTED".to_string(),
                message: "server restart via IPC not yet supported".to_string(),
            }
        }
        IpcRequest::Shutdown { .. } => {
            IpcResponse::Ok
        }
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
/// Note: must be called BEFORE any other tracing subscriber is initialized.
/// Currently not wired into cmd_daemon because main() initializes stderr logging
/// before command dispatch. Will be integrated when main() is refactored to
/// defer tracing setup based on command.
#[allow(dead_code)]
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
    write_frame(&mut writer, &payload).await?;

    // Read response
    let frame = read_frame(&mut reader)
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

    #[test]
    fn auth_token_generation_is_64_hex_chars() {
        let token = generate_auth_token();
        assert_eq!(token.len(), 64); // 32 bytes → 64 hex chars
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn auth_token_uniqueness() {
        let t1 = generate_auth_token();
        let t2 = generate_auth_token();
        assert_ne!(t1, t2);
    }

    #[test]
    fn verify_auth_token_correct() {
        let token = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        assert!(verify_auth_token(token, token));
    }

    #[test]
    fn verify_auth_token_incorrect() {
        let token = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(!verify_auth_token(wrong, token));
    }

    #[test]
    fn verify_auth_token_different_lengths() {
        assert!(!verify_auth_token("short", "longertoken"));
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
    async fn length_prefixed_frame_round_trip() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (_r1, mut w1) = client.into_split();
        let (mut r2, _w2) = server.into_split();

        let payload = b"hello world";

        let write_task = tokio::spawn(async move {
            write_frame(&mut w1, payload).await.unwrap();
        });

        let read_task = tokio::spawn(async move {
            let data = read_frame(&mut r2).await.unwrap().unwrap();
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
        w1.write_u32(MAX_FRAME_SIZE + 1).await.unwrap();

        let result = read_frame(&mut r2).await;
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
            write_frame(&mut w_client, &payload).await.unwrap();

            // Read response
            let frame = read_frame(&mut r_client).await.unwrap().unwrap();
            let resp: IpcResponse = serde_json::from_slice(&frame).unwrap();
            resp
        });

        // Server reads request and sends response
        let server_task = tokio::spawn(async move {
            let frame = read_frame(&mut r_server).await.unwrap().unwrap();
            let req: IpcRequest = serde_json::from_slice(&frame).unwrap();
            assert!(matches!(req, IpcRequest::Status));

            let response = IpcResponse::Status {
                servers: vec![],
                clients: 2,
                uptime_secs: 100,
            };
            send_response(&mut w_server, &response).await.unwrap();
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
}
