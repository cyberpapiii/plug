//! The client end of the daemon socket. Every CLI command, the service checks
//! and `plug connect` session setup reach the daemon through here; the daemon
//! module holds only the server end.

use std::path::PathBuf;
use std::time::Duration;

use plug_core::ipc::{
    self, DaemonFrame, DaemonFrameDecoder, IpcRequest, IpcResponse, OperatorRequest,
};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// How long an operator request may wait for the daemon to answer. Generous,
/// because a request can land while the daemon is refreshing credentials, but
/// bounded, because an unbounded wait reports nothing at all.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(crate) enum IpcClientError {
    /// Nothing accepts connections on the daemon socket.
    NotRunning,
    /// The request needs the daemon token and its file could not be read.
    Token {
        path: PathBuf,
        error: std::io::Error,
    },
    /// The daemon took the request and sent no complete answer in time.
    Timeout(Duration),
    /// The daemon closed the connection before it answered.
    Closed,
    /// A frame could not be written, read or decoded.
    Transport(String),
}

impl std::fmt::Display for IpcClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning => f.write_str("no plug daemon running"),
            Self::Token { path, .. } => {
                write!(f, "failed to read auth token from {}", path.display())
            }
            Self::Timeout(timeout) => write!(
                f,
                "daemon did not answer within {}s; see {}",
                timeout.as_secs(),
                plug_core::paths::log_dir().display()
            ),
            Self::Closed => f.write_str("daemon closed connection"),
            Self::Transport(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for IpcClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Token { error, .. } => Some(error),
            _ => None,
        }
    }
}

fn transport(error: impl std::fmt::Display) -> IpcClientError {
    IpcClientError::Transport(error.to_string())
}

/// A connection to the daemon, or `None` when nothing is listening.
pub(crate) async fn connect() -> Option<UnixStream> {
    UnixStream::connect(plug_core::paths::socket_path())
        .await
        .ok()
}

pub(crate) async fn daemon_running() -> bool {
    connect().await.is_some()
}

/// Whether a daemon start is in flight: another process holds the runtime
/// lock the daemon takes before anything else. The daemon binds its socket
/// only after that, so for a moment a starting daemon is neither reachable
/// nor stopped, and reporting that gap as stopped invites a repair that
/// fights the start it is describing. Never creates or rewrites the lock file.
pub(crate) fn daemon_starting() -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(plug_core::paths::pid_path())
    else {
        return false;
    };

    match fs4::FileExt::try_lock(&file) {
        Err(fs4::TryLockError::WouldBlock) => true,
        Ok(()) => {
            let _ = fs4::FileExt::unlock(&file);
            false
        }
        Err(fs4::TryLockError::Error(_)) => false,
    }
}

fn read_auth_token() -> Result<String, IpcClientError> {
    let path = plug_core::paths::token_path();
    std::fs::read_to_string(&path).map_err(|error| IpcClientError::Token { path, error })
}

/// One connection to the daemon carrying one request at a time.
pub(crate) struct Connection {
    reader: OwnedReadHalf,
    writer: OwnedWriteHalf,
    decoder: DaemonFrameDecoder,
}

impl Connection {
    pub(crate) async fn open() -> Result<Self, IpcClientError> {
        Ok(Self::from_stream(
            connect().await.ok_or(IpcClientError::NotRunning)?,
        ))
    }

    pub(crate) fn from_stream(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader,
            writer,
            decoder: DaemonFrameDecoder::default(),
        }
    }

    pub(crate) async fn send(&mut self, request: IpcRequest) -> Result<(), IpcClientError> {
        let payload = serde_json::to_vec(&request).map_err(transport)?;
        ipc::write_frame(&mut self.writer, &payload)
            .await
            .map_err(transport)
    }

    /// Read until the reply arrives, handing pushes that come first to
    /// `on_push`. Only a registered connection is sent pushes.
    pub(crate) async fn recv(
        &mut self,
        on_push: &mut impl FnMut(IpcResponse),
    ) -> Result<IpcResponse, IpcClientError> {
        loop {
            let frame = ipc::read_frame(&mut self.reader)
                .await
                .map_err(transport)?
                .ok_or(IpcClientError::Closed)?;
            match self.decoder.decode(&frame).map_err(transport)? {
                None => {}
                Some(DaemonFrame::Reply { response, .. }) if response.is_push() => {
                    on_push(response);
                }
                Some(DaemonFrame::Reply { response, .. }) => return Ok(response),
                Some(DaemonFrame::Reverse { id, .. }) => {
                    return Err(IpcClientError::Transport(format!(
                        "daemon sent reverse request {id} to a connection that cannot answer it"
                    )));
                }
            }
        }
    }

    pub(crate) fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        (self.reader, self.writer)
    }
}

/// Send one request on a connection of its own and read the reply.
pub(crate) async fn request(request: IpcRequest) -> Result<IpcResponse, IpcClientError> {
    request_with_timeout(request, REQUEST_TIMEOUT).await
}

/// Send one request that needs the daemon token, read from its file.
pub(crate) async fn operator(request: OperatorRequest) -> Result<IpcResponse, IpcClientError> {
    self::request(IpcRequest::Operator {
        auth_token: read_auth_token()?.into(),
        request,
    })
    .await
}

pub(crate) async fn request_with_timeout(
    request: IpcRequest,
    timeout: Duration,
) -> Result<IpcResponse, IpcClientError> {
    let mut connection = Connection::open().await?;
    connection.send(request).await?;
    // A daemon that accepted the connection but cannot answer, because its
    // engine is stuck on an upstream or a Keychain read, would otherwise hang
    // the caller forever with nothing to report.
    tokio::time::timeout(timeout, connection.recv(&mut |_| {}))
        .await
        .map_err(|_| IpcClientError::Timeout(timeout))?
}

/// Every live downstream session, as the daemon reports them; none when it
/// does not answer.
pub(crate) async fn live_sessions() -> Vec<plug_core::ipc::IpcLiveSessionInfo> {
    match request(IpcRequest::ListLiveSessions).await {
        Ok(IpcResponse::LiveSessions { sessions }) => sessions,
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonQueryAvailability {
    Unreachable,
    IpcUnavailable,
    Available,
}

impl DaemonQueryAvailability {
    pub(crate) fn runtime_available(self) -> bool {
        matches!(self, Self::Available)
    }

    pub(crate) fn daemon_reachable(self) -> bool {
        !matches!(self, Self::Unreachable)
    }

    pub(crate) fn status_source(self) -> &'static str {
        match self {
            Self::Available => "live_daemon",
            Self::IpcUnavailable => "ipc_unavailable",
            Self::Unreachable => "runtime_unavailable",
        }
    }
}

/// Ask the daemon something a view can do without: how available it was,
/// and the answer `decode` made of its reply.
pub(crate) async fn query<T>(
    request: IpcRequest,
    decode: impl FnOnce(IpcResponse) -> Option<T>,
) -> (DaemonQueryAvailability, Option<T>) {
    match self::request(request).await {
        Ok(response) => match decode(response) {
            Some(value) => (DaemonQueryAvailability::Available, Some(value)),
            None => (DaemonQueryAvailability::IpcUnavailable, None),
        },
        Err(IpcClientError::NotRunning) => (DaemonQueryAvailability::Unreachable, None),
        Err(_) => (DaemonQueryAvailability::IpcUnavailable, None),
    }
}

/// Poll until an in-process daemon binds its socket. Callers bound the wait
/// with their own timeout.
#[cfg(test)]
pub(crate) async fn wait_for_daemon() -> UnixStream {
    loop {
        if let Some(stream) = connect().await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Short and directly under /tmp: a Unix socket path has to fit in
    /// SUN_LEN, and the macOS per-user temp directory alone nearly fills it.
    fn temp_root(name: &str) -> PathBuf {
        PathBuf::from(format!(
            "/tmp/plug-{name}{:x}",
            uuid::Uuid::new_v4().as_fields().0
        ))
    }

    /// A reply bigger than one frame arrives as chunks; a one-shot request
    /// reads all of them.
    #[tokio::test]
    async fn a_reply_over_one_frame_is_reassembled() {
        let root = temp_root("chunked");
        let _paths = plug_core::paths::test::isolate(&root).await;
        let socket = plug_core::paths::socket_path();
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let text = "t".repeat(ipc::MAX_FRAME_SIZE as usize * 2);
        let reply = IpcResponse::McpResponse {
            payload: serde_json::json!({ "text": text }),
        };
        let daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            ipc::read_frame(&mut reader).await.unwrap().unwrap();
            ipc::send_chunked_response(&mut writer, &reply)
                .await
                .unwrap();
        });

        let response = request(IpcRequest::Status).await.expect("reply");
        let IpcResponse::McpResponse { payload } = response else {
            panic!("unexpected reply: {response:?}");
        };
        assert_eq!(payload["text"].as_str().map(str::len), Some(text.len()));
        daemon.await.unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An operator request gets the token from the token file without the
    /// caller reading it.
    #[tokio::test]
    async fn the_token_is_attached_to_operator_requests() {
        let root = temp_root("token");
        let _paths = plug_core::paths::test::isolate(&root).await;
        let socket = plug_core::paths::socket_path();
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        std::fs::write(plug_core::paths::token_path(), "the-token").unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            let frame = ipc::read_frame(&mut reader).await.unwrap().unwrap();
            let request: IpcRequest = serde_json::from_slice(&frame).unwrap();
            ipc::send_response(&mut writer, &IpcResponse::Ok)
                .await
                .unwrap();
            request
        });

        let response = operator(OperatorRequest::Reload).await.expect("reply");
        assert!(matches!(response, IpcResponse::Ok));
        let sent = daemon.await.unwrap();
        assert_eq!(sent.auth_token(), Some("the-token"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A daemon whose engine is stuck still accepts connections, so an
    /// unbounded read turns `plug status`, `plug doctor` and every app checkup
    /// into a spinner with nothing to report. The wait has to end.
    #[tokio::test]
    async fn a_request_gives_up_on_a_daemon_that_never_answers() {
        let root = temp_root("silent");
        let _paths = plug_core::paths::test::isolate(&root).await;
        let socket = plug_core::paths::socket_path();
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // Accept and hold the connection open without ever replying.
        let silent = tokio::spawn(async move {
            let accepted = listener.accept().await;
            std::future::pending::<()>().await;
            drop(accepted);
        });

        let error = request_with_timeout(IpcRequest::Status, Duration::from_millis(200))
            .await
            .expect_err("a silent daemon must not be waited on forever");

        silent.abort();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            matches!(error, IpcClientError::Timeout(_)),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn no_daemon_reads_as_unreachable() {
        let root = temp_root("absent");
        let _paths = plug_core::paths::test::isolate(&root).await;
        let (availability, value) = query(IpcRequest::Status, Some).await;
        assert_eq!(availability, DaemonQueryAvailability::Unreachable);
        assert!(value.is_none());
    }
}
