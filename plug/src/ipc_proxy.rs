//! IPC proxy handler — bridges stdio MCP ↔ daemon IPC.
//!
//! `IpcProxyHandler` implements rmcp's `ServerHandler` trait but forwards
//! all tool calls through the daemon's shared Engine via Unix socket IPC.
//! This is what `plug connect` uses when a daemon is running.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{NotificationContext, Peer, RequestContext, RoleServer};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use plug_core::ipc::{
    self, DaemonToProxyMessage, IpcClientRequest, IpcClientResponse, IpcRequest, IpcResponse,
};
use plug_core::legacy_tasks::CreateTaskResult as LegacyCreateTaskResult;

const DAEMON_PING_INTERVAL: Duration = Duration::from_secs(1);
/// Max silence (no frames of ANY kind) on a locked read before the daemon is
/// declared wedged and the connection is torn down for reconnect. Frames
/// (notifications, chunks, reverse requests) reset the clock, so slow tool
/// calls that emit progress are unaffected. See plans/009.
const READ_WATCHDOG: Duration = Duration::from_secs(120);
/// How long requests already sent wait for their replies after a write to
/// the daemon fails, before the connection is closed under them.
const WRITE_FAILURE_GRACE: Duration = Duration::from_secs(1);
/// Every `DaemonToProxyMessage` frame starts with its serde tag.
const ENVELOPE_FRAME_PREFIX: &[u8] = b"{\"envelope\":";

/// The version `initialize` answers with: the requested one when Plug
/// supports it, otherwise Plug's default, which is what RMCP negotiates.
fn selected_protocol_for_log(
    requested: &ProtocolVersion,
    supported: &[ProtocolVersion],
) -> ProtocolVersion {
    if supported.contains(requested) {
        requested.clone()
    } else {
        plug_core::protocol::supported_protocol_version()
    }
}

/// Test-only override for `READ_WATCHDOG` so the suite can exercise watchdog
/// expiry without a real 120s wait. Zero means "no override, use the real
/// constant." This must never grow into a runtime/config knob — see
/// plans/009 maintenance notes. Tests install it via
/// `tests::ReadWatchdogTestOverride`, which all run under `daemon_test_lock`
/// so the single global value is never contended across tests.
#[cfg(test)]
static READ_WATCHDOG_TEST_OVERRIDE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Effective read-watchdog duration: the real constant, unless a test has
/// installed a shorter override.
fn read_watchdog() -> Duration {
    #[cfg(test)]
    {
        let override_ms = READ_WATCHDOG_TEST_OVERRIDE_MS.load(std::sync::atomic::Ordering::SeqCst);
        if override_ms != 0 {
            return Duration::from_millis(override_ms);
        }
    }
    READ_WATCHDOG
}

struct SharedConnection {
    conn: Mutex<ProxyConnection>,
    /// Handed to each new `DaemonMux` so its reader can reach the peer and the
    /// gate without keeping this struct alive.
    self_ref: std::sync::Weak<SharedConnection>,
    /// Stable cancellation routing identity, duplicated outside `conn` so a
    /// cancellation never waits behind a reconnect.
    cancellation_identity: std::sync::RwLock<CancellationIdentity>,
    config_path: Option<PathBuf>,
    capabilities: std::sync::RwLock<ServerCapabilities>,
    /// Downstream peer — set during initialize, used to forward logging
    /// notifications pushed by the daemon over IPC.
    peer: std::sync::OnceLock<Peer<RoleServer>>,
    /// Whether the downstream client advertises roots capability.
    roots_supported: std::sync::atomic::AtomicBool,
    /// Notifications received during daemon session establishment before the
    /// downstream peer exists. Flushed after initialize.
    pending_daemon_notifications: std::sync::Mutex<Vec<IpcResponse>>,
    /// Client-negotiated session state the daemon does not persist across a
    /// restart (see `ReplayState`). Replayed onto the fresh daemon session
    /// in `replay_session_state_locked` after every successful reconnect.
    ///
    /// Lock ordering: `conn` is always acquired before `replay` whenever
    /// both are needed. The only sites that need both are
    /// `refresh_session` (the reconnect path),
    /// which already hold `conn` when they lock `replay` for the replay
    /// round trips. Every other mutation site (`initialize`, `subscribe`,
    /// `unsubscribe`, `set_level`) locks `replay` alone, strictly after its
    /// own `session_round_trip` call has already released `conn` — never
    /// while `conn` is held. Do not acquire `replay` and then try to
    /// acquire `conn`; that ordering is never used and would risk deadlock
    /// against the reconnect path above.
    replay: Mutex<ReplayState>,
    modern_downstream_enabled: std::sync::atomic::AtomicBool,
}

/// The registered daemon session every request currently goes through.
///
/// `conn` is held only to read or replace this: requests clone `mux` and
/// release the lock before they write, so they run concurrently. A reconnect
/// holds it for the whole re-registration and replay, so no request reaches
/// the new session before its state is restored.
struct ProxyConnection {
    client_id: String,
    client_info: Option<String>,
    session_id: String,
    mux: Arc<DaemonMux>,
}

impl ProxyConnection {
    fn start(
        session: crate::runtime::DaemonProxySession,
        config_path: Option<&PathBuf>,
        shared: std::sync::Weak<SharedConnection>,
    ) -> Self {
        Self {
            client_id: session.client_id,
            client_info: session.client_info,
            session_id: session.session_id,
            mux: DaemonMux::start(
                session.reader,
                session.writer,
                session.ipc_protocol_version >= 4,
                request_ceiling(config_path),
                shared,
            ),
        }
    }
}

// ──────────────────────── Multiplexed daemon connection ─────────────────────────

#[cfg(test)]
static REQUEST_CEILING_TEST_OVERRIDE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Longest one request waits for its reply before the proxy gives up on it.
///
/// The daemon bounds every upstream call by that server's
/// `call_timeout_secs`, elicitation or sampling inside the call included, so
/// twice the longest configured timeout plus a minute only trips on a request
/// the daemon has lost. Never below the default call timeout, and read at
/// each connect, so a config the proxy cannot load still gets a bound.
fn request_ceiling(config_path: Option<&PathBuf>) -> Duration {
    #[cfg(test)]
    {
        let override_ms =
            REQUEST_CEILING_TEST_OVERRIDE_MS.load(std::sync::atomic::Ordering::SeqCst);
        if override_ms != 0 {
            return Duration::from_millis(override_ms);
        }
    }
    const DEFAULT_CALL_TIMEOUT_SECS: u64 = 300;
    let longest = plug_core::config::load_config(config_path)
        .ok()
        .and_then(|config| {
            config
                .servers
                .values()
                .map(|server| server.call_timeout_secs.max(server.timeout_secs))
                .max()
        })
        .unwrap_or(DEFAULT_CALL_TIMEOUT_SECS)
        .max(DEFAULT_CALL_TIMEOUT_SECS);
    Duration::from_secs(longest.saturating_mul(2).saturating_add(60))
}

type ReplyWaiter = tokio::sync::oneshot::Sender<Result<IpcResponse, TransportFailure>>;

#[derive(Default)]
struct MuxState {
    /// Waiters by `ipc_id`. Ordered so an untagged reply can go to the oldest.
    pending: std::collections::BTreeMap<u64, ReplyWaiter>,
    /// Set once the connection is unusable. Every later request fails fast
    /// with a reconnectable error, which sends its caller to `refresh_session`.
    closed: Option<String>,
}

/// One daemon connection shared by all of this proxy's in-flight requests.
///
/// Each request is written with a fresh `ipc_id` by a single writer task, so
/// frames never interleave and a caller that gives up mid-request cannot
/// leave half a frame on the wire. A reader task routes each reply to the
/// waiter registered under its id, forwards push notifications, and answers
/// reverse requests on tasks of their own. A reply without an id goes to the
/// oldest waiter, which is how the one-at-a-time protocol paired them.
///
/// A session registered at IPC v3 (an older daemon) gets no ids and one
/// request slot, so each request waits for the one before it, as before v4.
struct DaemonMux {
    outbound: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    /// False when the daemon predates tagged requests.
    tagged: bool,
    /// Requests allowed on the wire at once; the daemon's own limit at v4.
    slots: tokio::sync::Semaphore,
    /// Longest one request waits for its reply; see `request_ceiling`.
    ceiling: Duration,
    state: std::sync::Mutex<MuxState>,
    next_id: std::sync::atomic::AtomicU64,
    /// Last time the daemon sent a frame, or a request started on an idle
    /// connection; the read watchdog measures silence from here.
    last_activity: std::sync::Mutex<tokio::time::Instant>,
    tasks: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for DaemonMux {
    fn drop(&mut self) {
        if let Ok(tasks) = self.tasks.get_mut() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

impl DaemonMux {
    fn start(
        reader: tokio::net::unix::OwnedReadHalf,
        writer: tokio::net::unix::OwnedWriteHalf,
        tagged: bool,
        ceiling: Duration,
        shared: std::sync::Weak<SharedConnection>,
    ) -> Arc<Self> {
        let (outbound, outbound_rx) = tokio::sync::mpsc::unbounded_channel();
        let slots = if tagged {
            crate::daemon::MAX_CONCURRENT_REQUESTS_PER_CONNECTION
        } else {
            1
        };
        let mux = Arc::new(Self {
            outbound,
            tagged,
            slots: tokio::sync::Semaphore::new(slots),
            ceiling,
            state: std::sync::Mutex::new(MuxState::default()),
            next_id: std::sync::atomic::AtomicU64::new(1),
            last_activity: std::sync::Mutex::new(tokio::time::Instant::now()),
            tasks: std::sync::Mutex::new(Vec::new()),
        });
        // The tasks hold the mux weakly: dropping the last handle aborts them
        // and closes the socket.
        let writer_task = tokio::spawn(Self::write_loop(writer, outbound_rx, Arc::downgrade(&mux)));
        let reader_task = tokio::spawn(Self::read_loop(
            reader,
            Arc::downgrade(&mux),
            tagged,
            shared,
        ));
        if let Ok(mut tasks) = mux.tasks.lock() {
            tasks.push(writer_task);
            tasks.push(reader_task);
        }
        mux
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, MuxState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn last_activity(&self) -> tokio::time::Instant {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = tokio::time::Instant::now();
    }

    /// Fail every waiter with `failure` and refuse new requests. Idempotent;
    /// the first failure is the one later requests see.
    fn close(&self, failure: TransportFailure) {
        let pending = {
            let mut state = self.lock_state();
            state.closed.get_or_insert_with(|| failure.message.clone());
            std::mem::take(&mut state.pending)
        };
        for waiter in pending.into_values() {
            let _ = waiter.send(Err(failure.clone()));
        }
        if let Ok(tasks) = self.tasks.lock() {
            for task in tasks.iter() {
                task.abort();
            }
        }
    }

    /// The socket refused a write: refuse new requests, but leave waiters to
    /// the reader. A daemon that shuts down writes its last replies before it
    /// closes, and a connector paused across a daemon swap resumes with those
    /// replies unread and a heartbeat due at once. Failing the waiters here
    /// would lose replies that are already in the socket buffer.
    fn writes_failed(&self, failure: &TransportFailure) {
        self.lock_state()
            .closed
            .get_or_insert_with(|| failure.message.clone());
    }

    /// Fail the requests currently waiting without closing the connection:
    /// a frame arrived that cannot be matched to any of them.
    fn fail_pending(&self, failure: TransportFailure) {
        let pending = std::mem::take(&mut self.lock_state().pending);
        for waiter in pending.into_values() {
            let _ = waiter.send(Err(failure.clone()));
        }
    }

    fn deliver(&self, ipc_id: Option<u64>, response: IpcResponse) {
        let waiter = {
            let mut state = self.lock_state();
            match ipc_id {
                Some(id) => state.pending.remove(&id),
                None => state.pending.pop_first().map(|(_, waiter)| waiter),
            }
        };
        match waiter {
            Some(waiter) => {
                let _ = waiter.send(Ok(response));
            }
            None => tracing::debug!(?ipc_id, "daemon reply has no waiting request"),
        }
    }

    /// Send `request` and wait for its reply.
    ///
    /// Two limits apply while it waits:
    /// - The read watchdog fails the whole connection, reconnectably, once the
    ///   daemon has sent nothing at all for `read_watchdog()`. Any frame resets
    ///   it, including replies to other requests and heartbeat pongs.
    /// - `ceiling` fails this request alone once it has waited that long, so a
    ///   request the daemon lost cannot hang its caller while the heartbeat
    ///   keeps the connection alive.
    async fn round_trip(&self, request: &IpcRequest) -> Result<IpcResponse, TransportFailure> {
        // A v3 daemon has one slot, taken by every request. A v4 daemon runs
        // up to its per-connection limit, and control requests it answers
        // inline skip the queue so the heartbeat is never stuck behind calls.
        let bypass = self.tagged
            && matches!(
                request,
                IpcRequest::Ping { .. } | IpcRequest::ModernDownstreamGate { .. }
            );
        let _slot = if bypass {
            None
        } else {
            // The semaphore is never closed.
            self.slots.acquire().await.ok()
        };
        // Allocated after the slot, so on a v3 daemon ids follow wire order.
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let ipc_id = self.tagged.then_some(id);
        let payload = ipc::encode_tagged(ipc_id, request).map_err(|e| TransportFailure {
            message: format!("failed to encode IPC request: {e}"),
            reconnectable: false,
        })?;
        if payload.len() > ipc::MAX_FRAME_SIZE as usize {
            return Err(TransportFailure {
                message: format!(
                    "IPC write failed: payload too large: {} bytes (max {})",
                    payload.len(),
                    ipc::MAX_FRAME_SIZE
                ),
                reconnectable: false,
            });
        }

        let (waiter, mut reply) = tokio::sync::oneshot::channel();
        {
            let mut state = self.lock_state();
            if let Some(reason) = &state.closed {
                return Err(TransportFailure {
                    message: reason.clone(),
                    reconnectable: true,
                });
            }
            if state.pending.is_empty() {
                self.touch();
            }
            state.pending.insert(id, waiter);
        }
        let started = tokio::time::Instant::now();
        if self.outbound.send(payload).is_err() {
            self.close(TransportFailure {
                message: "IPC write failed: connection writer stopped".to_string(),
                reconnectable: true,
            });
        }

        let watchdog = read_watchdog();
        let ceiling = started + self.ceiling;
        loop {
            let deadline = (self.last_activity().max(started) + watchdog).min(ceiling);
            match tokio::time::timeout_at(deadline, &mut reply).await {
                Ok(Ok(result)) => return result,
                Ok(Err(_)) => {
                    return Err(TransportFailure {
                        message: "daemon connection closed".to_string(),
                        reconnectable: true,
                    });
                }
                Err(_elapsed) => {
                    let now = tokio::time::Instant::now();
                    if now >= ceiling {
                        self.lock_state().pending.remove(&id);
                        tracing::warn!(
                            secs = self.ceiling.as_secs(),
                            "daemon never answered an IPC request; giving up on it"
                        );
                        return Err(TransportFailure {
                            message: format!(
                                "daemon did not answer within {}s",
                                self.ceiling.as_secs()
                            ),
                            reconnectable: false,
                        });
                    }
                    if now < self.last_activity().max(started) + watchdog {
                        continue;
                    }
                    tracing::warn!(
                        secs = watchdog.as_secs(),
                        "daemon read watchdog expired; forcing reconnect"
                    );
                    let failure = TransportFailure {
                        message: format!(
                            "daemon read watchdog expired after {}s",
                            watchdog.as_secs()
                        ),
                        reconnectable: true,
                    };
                    self.close(failure.clone());
                    return Err(failure);
                }
            }
        }
    }

    /// Send a frame that expects no reply (a reverse-request answer).
    fn send_frame(&self, payload: Vec<u8>) {
        if self.outbound.send(payload).is_err() {
            tracing::debug!("reverse-request reply dropped: connection writer stopped");
        }
    }

    async fn write_loop(
        mut writer: tokio::net::unix::OwnedWriteHalf,
        mut outbound: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
        mux: std::sync::Weak<DaemonMux>,
    ) {
        while let Some(payload) = outbound.recv().await {
            if let Err(error) = ipc::write_frame(&mut writer, &payload).await {
                let failure = IpcProxyHandler::transport_failure("IPC write failed", error);
                if let Some(mux) = mux.upgrade() {
                    mux.writes_failed(&failure);
                }
                // The reader normally sees end of file right after the last
                // reply. Close anyway if it does not.
                tokio::time::sleep(WRITE_FAILURE_GRACE).await;
                if let Some(mux) = mux.upgrade() {
                    mux.close(failure);
                }
                return;
            }
        }
    }

    async fn read_loop(
        mut reader: tokio::net::unix::OwnedReadHalf,
        mux: std::sync::Weak<DaemonMux>,
        tagged: bool,
        shared: std::sync::Weak<SharedConnection>,
    ) {
        let mut chunks = ChunkAssembler::default();
        let failure = loop {
            let frame = match ipc::read_frame(&mut reader).await {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    break TransportFailure {
                        message: "daemon closed connection".to_string(),
                        reconnectable: true,
                    };
                }
                Err(error) => break IpcProxyHandler::transport_failure("IPC read failed", error),
            };
            let Some(mux_ref) = mux.upgrade() else {
                return;
            };
            mux_ref.touch();
            match decode_daemon_frame(&frame, &mut chunks) {
                Ok(None) => {}
                Ok(Some(DaemonFrame::Reply { ipc_id, response })) => {
                    handle_daemon_reply(&mux_ref, &shared, ipc_id, response).await;
                }
                Ok(Some(DaemonFrame::Reverse { id, request })) => {
                    let mux = mux.clone();
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        let peer = shared
                            .upgrade()
                            .and_then(|shared| shared.peer.get().cloned());
                        let response = IpcProxyHandler::handle_daemon_reverse_request(
                            peer.as_ref(),
                            id,
                            request,
                        )
                        .await;
                        let encoded = if tagged {
                            ipc::encode_reverse_response(id, &response)
                        } else {
                            serde_json::to_vec(&response)
                        };
                        match encoded {
                            Ok(payload) => {
                                if let Some(mux) = mux.upgrade() {
                                    mux.send_frame(payload);
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "failed to serialize reverse response");
                            }
                        }
                    });
                }
                Err(failure) => mux_ref.fail_pending(failure),
            }
        };
        if let Some(mux) = mux.upgrade() {
            mux.close(failure);
        }
    }
}

/// Reassembles one chunked response. The daemon writes all chunks of a
/// response back to back, so there is only ever one in progress.
#[derive(Default)]
struct ChunkAssembler {
    buffer: Vec<u8>,
    expected: Option<u32>,
}

enum DaemonFrame {
    Reply {
        ipc_id: Option<u64>,
        response: IpcResponse,
    },
    Reverse {
        id: u64,
        request: IpcClientRequest,
    },
}

/// Decode one frame from the daemon. `Ok(None)` means a chunk was consumed
/// and the response is not complete yet. An error concerns this frame only.
fn decode_daemon_frame(
    frame: &[u8],
    chunks: &mut ChunkAssembler,
) -> Result<Option<DaemonFrame>, TransportFailure> {
    // Envelope frames are serialized by `ipc::send_daemon_message` with
    // `serde_json::to_vec`, which writes an internally tagged enum's tag
    // first. Matching the prefix keeps plain `IpcResponse` frames (the hot
    // path) to one parse, and a payload that merely contains an `"envelope"`
    // key somewhere is never mistaken for one.
    if !frame.starts_with(ENVELOPE_FRAME_PREFIX) {
        return decode_reply(frame, "invalid IPC response").map(Some);
    }
    let daemon_msg: DaemonToProxyMessage =
        serde_json::from_slice(frame).map_err(|e| TransportFailure {
            message: format!("invalid envelope message: {e}"),
            reconnectable: false,
        })?;
    match daemon_msg {
        // Never sent by the current daemon; decoded for tolerance.
        DaemonToProxyMessage::Response { inner } => Ok(Some(DaemonFrame::Reply {
            ipc_id: None,
            response: inner,
        })),
        DaemonToProxyMessage::ResponseChunk {
            chunk_index,
            chunk_count,
            payload_b64,
        } => {
            let invalid = |message: String| TransportFailure {
                message,
                reconnectable: false,
            };
            if chunk_count == 0 {
                *chunks = ChunkAssembler::default();
                return Err(invalid("invalid response chunk count 0".to_string()));
            }
            if chunk_index == 0 {
                chunks.buffer.clear();
                chunks.expected = Some(chunk_count);
            } else if chunks.expected != Some(chunk_count) {
                *chunks = ChunkAssembler::default();
                return Err(invalid(
                    "response chunk count changed mid-stream".to_string(),
                ));
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(payload_b64)
                .map_err(|e| {
                    *chunks = ChunkAssembler::default();
                    invalid(format!("invalid chunk payload: {e}"))
                })?;
            chunks.buffer.extend_from_slice(&decoded);
            if chunk_index + 1 != chunk_count {
                return Ok(None);
            }
            let whole = std::mem::take(chunks);
            decode_reply(&whole.buffer, "invalid chunked IPC response").map(Some)
        }
        DaemonToProxyMessage::ReverseRequest { id, request } => Ok(Some(DaemonFrame::Reverse {
            id,
            request: *request,
        })),
    }
}

fn decode_reply(bytes: &[u8], context: &str) -> Result<DaemonFrame, TransportFailure> {
    let response = serde_json::from_slice(bytes).map_err(|e| TransportFailure {
        message: format!("{context}: {e}"),
        reconnectable: false,
    })?;
    Ok(DaemonFrame::Reply {
        ipc_id: ipc::FrameIds::peek(bytes).ipc_id,
        response,
    })
}

/// Apply a push notification, or hand a reply to its waiter.
async fn handle_daemon_reply(
    mux: &DaemonMux,
    shared: &std::sync::Weak<SharedConnection>,
    ipc_id: Option<u64>,
    response: IpcResponse,
) {
    match response {
        IpcResponse::LoggingNotification { params } => {
            if let Some(shared) = shared.upgrade()
                && let Some(peer) = shared.peer.get()
                && let Ok(notif_params) =
                    serde_json::from_value::<LoggingMessageNotificationParam>(params)
            {
                let _ = peer.notify_logging_message(notif_params).await;
            }
        }
        resp @ (IpcResponse::ToolListChangedNotification
        | IpcResponse::ResourceListChangedNotification
        | IpcResponse::ResourceUpdatedNotification { .. }
        | IpcResponse::PromptListChangedNotification
        | IpcResponse::ProgressNotification { .. }
        | IpcResponse::CancelledNotification { .. }
        | IpcResponse::AuthStateChanged { .. }) => {
            let shared = shared.upgrade();
            forward_control_notification(shared.as_ref().and_then(|s| s.peer.get()), resp).await;
        }
        IpcResponse::ModernDownstreamGateChanged { enabled } => {
            if let Some(shared) = shared.upgrade() {
                shared
                    .modern_downstream_enabled
                    .store(enabled, std::sync::atomic::Ordering::Release);
            }
        }
        other => mux.deliver(ipc_id, other),
    }
}

/// What a failed reconnect can actually do about itself.
#[derive(Debug, PartialEq, Eq)]
enum ReconnectRecovery {
    /// The daemon was upgraded under a client still running the old image.
    /// Exiting hands the problem to the host, which spawns a fresh
    /// `plug connect` from the installed binary.
    RespawnClient,
    /// Nothing a restart fixes. Surface it to the caller.
    ReportError,
}

fn reconnect_recovery(error: &anyhow::Error) -> ReconnectRecovery {
    match error.downcast_ref::<crate::runtime::DaemonVersionMismatch>() {
        Some(mismatch) if mismatch.respawn_resolves => ReconnectRecovery::RespawnClient,
        _ => ReconnectRecovery::ReportError,
    }
}

fn reconnect_error(error: anyhow::Error) -> McpError {
    if reconnect_recovery(&error) == ReconnectRecovery::RespawnClient {
        tracing::error!(
            %error,
            "the daemon was upgraded under this client; exiting so the host respawns `plug connect` from the installed binary"
        );
        // A reconnect only happens after a handshake that matched, so the
        // versions parted because the file on disk changed, not because this
        // client was installed against the wrong one: the respawn lands on the
        // new binary. Without this the process stays up and answers every call
        // with the same mismatch forever, which no client recovers from.
        //
        // Exit zero deliberately. A supervisor that reads a non-zero exit as
        // "stop retrying" would wedge the client permanently, which is the
        // state this exists to end. Closing stdout is the EOF the host waits
        // for.
        std::process::exit(0);
    }
    McpError::internal_error(format!("daemon reconnect failed: {error}"), None)
}

#[derive(Clone)]
struct CancellationIdentity {
    session_id: String,
    client_id: String,
    cancellation_capability: ipc::IpcCancellationCapability,
}

/// Client-negotiated session state replayed onto a fresh daemon session after
/// reconnect (capabilities, subscriptions, log level).
#[derive(Default)]
struct ReplayState {
    client_capabilities: Option<ClientCapabilities>,
    subscriptions: HashSet<String>,
    log_level: Option<LoggingLevel>,
}

/// MCP server handler that proxies all requests through the daemon via IPC.
///
/// Holds a persistent IPC connection to the daemon. The connection is
/// established during `cmd_connect` and reused for all MCP traffic. Requests
/// share it concurrently; see `DaemonMux`.
pub struct IpcProxyHandler {
    shared: Arc<SharedConnection>,
    heartbeat: JoinHandle<()>,
}

#[derive(Clone, Copy)]
enum RetryPolicy {
    SafeToRetry,
    UnsafeToRetry,
}

#[derive(Clone, Debug)]
struct TransportFailure {
    message: String,
    reconnectable: bool,
}

impl IpcProxyHandler {
    /// Create a new proxy handler from an established IPC connection.
    pub fn new(session: crate::runtime::DaemonProxySession, config_path: Option<PathBuf>) -> Self {
        let pending_daemon_notifications =
            std::sync::Mutex::new(session.pending_notifications.clone());
        let modern_downstream_enabled = session.modern_downstream_enabled;
        let cancellation_identity = CancellationIdentity {
            session_id: session.session_id.clone(),
            client_id: session.client_id.clone(),
            cancellation_capability: session.cancellation_capability.clone(),
        };
        let shared = Arc::new_cyclic(|self_ref| SharedConnection {
            capabilities: std::sync::RwLock::new(session.capabilities.clone()),
            conn: Mutex::new(ProxyConnection::start(
                session,
                config_path.as_ref(),
                self_ref.clone(),
            )),
            self_ref: self_ref.clone(),
            cancellation_identity: std::sync::RwLock::new(cancellation_identity),
            config_path,
            peer: std::sync::OnceLock::new(),
            roots_supported: std::sync::atomic::AtomicBool::new(false),
            pending_daemon_notifications,
            replay: Mutex::new(ReplayState::default()),
            modern_downstream_enabled: std::sync::atomic::AtomicBool::new(
                modern_downstream_enabled,
            ),
        });
        let heartbeat = tokio::spawn(Self::heartbeat_loop(shared.clone()));
        Self { shared, heartbeat }
    }

    pub(crate) fn modern_gate_reader(&self) -> Arc<dyn Fn() -> bool + Send + Sync> {
        let shared = Arc::clone(&self.shared);
        Arc::new(move || {
            shared
                .modern_downstream_enabled
                .load(std::sync::atomic::Ordering::Acquire)
        })
    }

    fn request_context(context: &RequestContext<RoleServer>) -> ipc::IpcMcpRequestContext {
        let protocol_version = context
            .protocol_version()
            .unwrap_or_else(plug_core::protocol::supported_protocol_version)
            .to_string();
        let client = context.client_info();
        ipc::IpcMcpRequestContext {
            request_id: context.id.clone(),
            protocol_version,
            client_name: client.as_ref().map(|client| client.name.to_string()),
            client_version: client.map(|client| client.version.to_string()),
        }
    }

    async fn mcp_round_trip(
        &self,
        retry_policy: RetryPolicy,
        method: &str,
        params: Option<serde_json::Value>,
        context: &RequestContext<RoleServer>,
    ) -> Result<IpcResponse, McpError> {
        let method = method.to_string();
        let request_context = Self::request_context(context);
        self.session_round_trip(retry_policy, |session_id| {
            IpcRequest::McpRequestWithContext {
                session_id: session_id.to_string(),
                method: method.clone(),
                params: params.clone(),
                context: request_context.clone(),
            }
        })
        .await
    }

    async fn refresh_modern_gate(&self) -> Result<bool, McpError> {
        let response = self
            .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                IpcRequest::ModernDownstreamGate {
                    session_id: session_id.to_string(),
                }
            })
            .await?;
        let IpcResponse::ModernDownstreamGate { enabled } = response else {
            return Err(McpError::internal_error(
                format!("unexpected modern gate response: {response:?}"),
                None,
            ));
        };
        self.shared
            .modern_downstream_enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
        Ok(enabled)
    }

    async fn send_cancellation_out_of_band(
        shared: &SharedConnection,
        request_id: RequestId,
        reason: Option<String>,
    ) -> Result<(), McpError> {
        let identity = shared
            .cancellation_identity
            .read()
            .map_err(|_| McpError::internal_error("cancellation identity lock poisoned", None))?
            .clone();
        let mut stream = crate::daemon::connect_to_daemon()
            .await
            .ok_or_else(|| McpError::internal_error("daemon is unavailable", None))?;
        let request = IpcRequest::CancelMcpRequest {
            session_id: identity.session_id,
            client_id: identity.client_id,
            cancellation_capability: identity.cancellation_capability,
            request_id,
            reason,
        };
        let payload = serde_json::to_vec(&request)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        ipc::write_frame(&mut stream, &payload)
            .await
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        let frame = ipc::read_frame(&mut stream)
            .await
            .map_err(|error| McpError::internal_error(error.to_string(), None))?
            .ok_or_else(|| McpError::internal_error("daemon closed cancellation socket", None))?;
        match serde_json::from_slice::<IpcResponse>(&frame)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?
        {
            IpcResponse::Ok => Ok(()),
            IpcResponse::Error { code, message } => {
                Err(McpError::internal_error(format!("{code}: {message}"), None))
            }
            other => Err(McpError::internal_error(
                format!("unexpected cancellation response: {other:?}"),
                None,
            )),
        }
    }

    /// Send an IPC request and read the response.
    ///
    /// Requests run concurrently over the shared connection; replies are
    /// paired by `ipc_id` (see `DaemonMux`).
    async fn session_round_trip<F>(
        &self,
        retry_policy: RetryPolicy,
        build_request: F,
    ) -> Result<IpcResponse, McpError>
    where
        F: Fn(&str) -> IpcRequest,
    {
        Self::shared_round_trip(&self.shared, retry_policy, build_request).await
    }

    /// The current session id and connection. Waits out a reconnect in
    /// progress, so a request never reaches a session before its replay.
    async fn current_connection(shared: &SharedConnection) -> (String, Arc<DaemonMux>) {
        let conn = shared.conn.lock().await;
        (conn.session_id.clone(), Arc::clone(&conn.mux))
    }

    /// Reconnect after `failed` broke, unless a concurrent request already
    /// did. Returns the connection to retry on.
    async fn reconnect_after(
        shared: &SharedConnection,
        failed: &Arc<DaemonMux>,
    ) -> Result<(String, Arc<DaemonMux>), McpError> {
        let mut conn = shared.conn.lock().await;
        if Arc::ptr_eq(&conn.mux, failed) {
            Self::refresh_session(shared, &mut conn).await?;
        }
        Ok((conn.session_id.clone(), Arc::clone(&conn.mux)))
    }

    /// `session_round_trip` for callers that hold only the shared connection,
    /// such as the spawned roots refresh.
    async fn shared_round_trip<F>(
        shared: &SharedConnection,
        retry_policy: RetryPolicy,
        build_request: F,
    ) -> Result<IpcResponse, McpError>
    where
        F: Fn(&str) -> IpcRequest,
    {
        let (session_id, mux) = Self::current_connection(shared).await;
        match mux.round_trip(&build_request(&session_id)).await {
            Ok(response) => Ok(response),
            Err(failure) if failure.reconnectable => {
                tracing::warn!(error = %failure.message, "daemon IPC connection lost; reconnecting");
                let (session_id, mux) = Self::reconnect_after(shared, &mux).await?;
                match retry_policy {
                    RetryPolicy::SafeToRetry => mux
                        .round_trip(&build_request(&session_id))
                        .await
                        .map_err(|e| {
                            McpError::internal_error(
                                format!("IPC retry failed after reconnect: {}", e.message),
                                None,
                            )
                        }),
                    RetryPolicy::UnsafeToRetry => Err(McpError::internal_error(
                        "REQUEST_RETRY_UNSAFE: daemon connection recovered; retry the tool call",
                        None,
                    )),
                }
            }
            Err(failure) => Err(McpError::internal_error(failure.message, None)),
        }
    }

    /// Handle a reverse request from the daemon during an active tool call.
    ///
    /// Calls the downstream peer's `create_elicitation()` or `create_message()`
    /// and returns the response as an `IpcClientResponse`.
    async fn handle_daemon_reverse_request(
        peer: Option<&Peer<RoleServer>>,
        id: u64,
        request: IpcClientRequest,
    ) -> IpcClientResponse {
        let Some(peer) = peer else {
            tracing::warn!(
                reverse_request_id = id,
                "received reverse request but no downstream peer is available"
            );
            return IpcClientResponse::Error {
                message: "no downstream peer available for reverse request".to_string(),
            };
        };

        tracing::debug!(reverse_request_id = id, "handling daemon reverse request");

        match request {
            IpcClientRequest::CreateElicitation { params } => {
                match peer.create_elicitation(params).await {
                    Ok(result) => IpcClientResponse::CreateElicitation { result },
                    Err(e) => IpcClientResponse::Error {
                        message: format!("elicitation failed: {e}"),
                    },
                }
            }
            IpcClientRequest::CreateMessage { params } => match peer.create_message(params).await {
                Ok(result) => IpcClientResponse::CreateMessage { result },
                Err(e) => IpcClientResponse::Error {
                    message: format!("sampling failed: {e}"),
                },
            },
        }
    }

    async fn heartbeat_loop(shared: Arc<SharedConnection>) {
        let mut tick = tokio::time::interval(DAEMON_PING_INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        tick.tick().await;

        loop {
            tick.tick().await;
            if let Err(error) = Self::ping_once(&shared).await {
                tracing::debug!(error = %error, "daemon heartbeat ping failed");
            }
        }
    }

    async fn ping_once(shared: &Arc<SharedConnection>) -> Result<(), McpError> {
        let (session_id, mux) = Self::current_connection(shared).await;
        match mux.round_trip(&IpcRequest::Ping { session_id }).await {
            Ok(IpcResponse::Pong) => Ok(()),
            Ok(IpcResponse::Error { code, message }) => {
                if matches!(code.as_str(), "SESSION_REPLACED" | "SESSION_MISMATCH") {
                    tracing::warn!(code = %code, message = %message, "daemon heartbeat detected stale session; reconnecting");
                    Self::reconnect_after(shared, &mux).await?;
                    return Ok(());
                }
                Err(McpError::internal_error(format!("{code}: {message}"), None))
            }
            Ok(other) => Err(McpError::internal_error(
                format!("unexpected IPC ping response: {other:?}"),
                None,
            )),
            Err(failure) if failure.reconnectable => {
                tracing::warn!(error = %failure.message, "daemon heartbeat lost connection; reconnecting");
                Self::reconnect_after(shared, &mux).await?;
                Ok(())
            }
            Err(failure) => Err(McpError::internal_error(failure.message, None)),
        }
    }

    async fn refresh_session(
        shared: &SharedConnection,
        conn: &mut ProxyConnection,
    ) -> Result<(), McpError> {
        let session = crate::runtime::establish_daemon_proxy_session(
            shared.config_path.as_ref(),
            conn.client_id.clone(),
            conn.client_info.clone(),
        )
        .await
        .map_err(reconnect_error)?;
        if let Ok(mut caps) = shared.capabilities.write() {
            *caps = session.capabilities.clone();
        }
        shared.modern_downstream_enabled.store(
            session.modern_downstream_enabled,
            std::sync::atomic::Ordering::Release,
        );
        if let Ok(mut identity) = shared.cancellation_identity.write() {
            *identity = CancellationIdentity {
                session_id: session.session_id.clone(),
                client_id: session.client_id.clone(),
                cancellation_capability: session.cancellation_capability.clone(),
            };
        }
        let replaced = std::mem::replace(
            conn,
            ProxyConnection::start(
                session,
                shared.config_path.as_ref(),
                shared.self_ref.clone(),
            ),
        );
        // Requests still waiting on the old connection fail over to the new
        // one through their own retry policy.
        replaced.mux.close(TransportFailure {
            message: "daemon session replaced by reconnect".to_string(),
            reconnectable: true,
        });
        Self::replay_session_state_locked(shared, conn).await;
        Ok(())
    }

    /// Replay `ReplayState` onto a fresh session. Caller already holds
    /// `shared.conn`, so no other request reaches the session first.
    /// Replay failures are logged and do not fail reconnect.
    async fn replay_session_state_locked(shared: &SharedConnection, conn: &ProxyConnection) {
        let replay = shared.replay.lock().await;

        if let Some(caps) = replay.client_capabilities.clone() {
            let request = IpcRequest::UpdateCapabilities {
                session_id: conn.session_id.clone(),
                capabilities: Box::new(caps),
            };
            if let Err(e) = Self::send_replay_request(&conn.mux, &request).await {
                tracing::warn!(error = %e, "reconnect: failed to replay client capabilities");
            }
        }

        if !replay.subscriptions.is_empty() {
            let request = IpcRequest::RestoreResourceSubscriptions {
                session_id: conn.session_id.clone(),
                uris: replay.subscriptions.iter().cloned().collect(),
            };
            if let Err(e) = Self::send_replay_request(&conn.mux, &request).await {
                tracing::warn!(error = %e, "reconnect: failed to replay subscriptions");
            }
        }

        if let Some(level) = replay.log_level {
            let params = serde_json::json!({ "level": level });
            let request = IpcRequest::McpRequest {
                session_id: conn.session_id.clone(),
                method: "logging/setLevel".to_string(),
                params: Some(params),
            };
            if let Err(e) = Self::send_replay_request(&conn.mux, &request).await {
                tracing::warn!(error = %e, "reconnect: failed to replay log level");
            }
        }
    }

    /// Send a single replay request on the fresh connection and classify the
    /// result as success/failure, without ever calling
    /// `refresh_session`/`session_round_trip` (see
    /// `replay_session_state_locked`).
    async fn send_replay_request(mux: &DaemonMux, request: &IpcRequest) -> Result<(), String> {
        match mux.round_trip(request).await {
            Ok(IpcResponse::Ok) => Ok(()),
            Ok(IpcResponse::McpResponse { payload }) => {
                if payload.get("code").is_some()
                    && let Ok(err) = serde_json::from_value::<McpError>(payload.clone())
                {
                    return Err(err.message.to_string());
                }
                Ok(())
            }
            Ok(IpcResponse::Error { code, message }) => Err(format!("{code}: {message}")),
            Ok(other) => Err(format!("unexpected IPC response: {other:?}")),
            Err(failure) => Err(failure.message),
        }
    }

    fn transport_failure(context: &str, error: anyhow::Error) -> TransportFailure {
        let reconnectable = error.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::UnexpectedEof
            )
        });

        TransportFailure {
            message: format!("{context}: {error}"),
            reconnectable,
        }
    }
}

/// Forward a control notification (list_changed, progress, cancelled) to the downstream peer.
async fn forward_control_notification(peer: Option<&Peer<RoleServer>>, response: IpcResponse) {
    let Some(peer) = peer else {
        return;
    };
    match response {
        IpcResponse::ToolListChangedNotification => {
            let _ = peer.notify_tool_list_changed().await;
        }
        IpcResponse::ResourceListChangedNotification => {
            let _ = peer.notify_resource_list_changed().await;
        }
        IpcResponse::ResourceUpdatedNotification { params } => {
            if let Ok(notif_params) =
                serde_json::from_value::<ResourceUpdatedNotificationParam>(params)
            {
                let _ = peer.notify_resource_updated(notif_params).await;
            }
        }
        IpcResponse::PromptListChangedNotification => {
            let _ = peer.notify_prompt_list_changed().await;
        }
        IpcResponse::ProgressNotification { params } => {
            if let Ok(notif_params) = serde_json::from_value::<ProgressNotificationParam>(params) {
                let _ = peer.notify_progress(notif_params).await;
            }
        }
        IpcResponse::CancelledNotification { params } => {
            if let Ok(notif_params) = serde_json::from_value::<CancelledNotificationParam>(params) {
                let _ = peer.notify_cancelled(notif_params).await;
            }
        }
        IpcResponse::AuthStateChanged {
            ref server_id,
            ref state,
        } => {
            // AuthStateChanged is a plug-internal notification with no MCP wire
            // equivalent. Log it for observability but there's nothing to forward
            // to the downstream MCP peer.
            tracing::info!(server = %server_id, state = ?state, "auth state changed (IPC push)");
        }
        _ => {} // not a control notification
    }
}

async fn flush_pending_daemon_notifications(shared: &SharedConnection) {
    let pending = {
        let mut guard = shared
            .pending_daemon_notifications
            .lock()
            .expect("pending daemon notifications mutex poisoned");
        std::mem::take(&mut *guard)
    };

    let peer = shared.peer.get();
    for response in pending {
        match response {
            IpcResponse::ModernDownstreamGateChanged { enabled } => {
                shared
                    .modern_downstream_enabled
                    .store(enabled, std::sync::atomic::Ordering::Release);
            }
            IpcResponse::LoggingNotification { params } => {
                if let Some(peer) = peer
                    && let Ok(notif_params) =
                        serde_json::from_value::<LoggingMessageNotificationParam>(params)
                {
                    let _ = peer.notify_logging_message(notif_params).await;
                }
            }
            other => forward_control_notification(peer, other).await,
        }
    }
}

impl Drop for IpcProxyHandler {
    fn drop(&mut self) {
        self.heartbeat.abort();
    }
}

#[allow(clippy::manual_async_fn)]
impl ServerHandler for IpcProxyHandler {
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Owned(plug_core::protocol::supported_downstream_protocol_versions(
            self.shared
                .modern_downstream_enabled
                .load(std::sync::atomic::Ordering::Acquire),
        ))
    }
    fn get_info(&self) -> ServerInfo {
        let capabilities = self
            .shared
            .capabilities
            .read()
            .map(|caps| caps.clone())
            .unwrap_or_default();
        InitializeResult::new(capabilities)
            .with_server_info(plug_core::branding::plug_implementation(env!(
                "CARGO_PKG_VERSION"
            )))
            .with_protocol_version(plug_core::protocol::supported_protocol_version())
    }

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, McpError>> + Send + '_ {
        async move {
            // Same deliberate leniency as the stdio handler: `plug connect`
            // speaks RMCP's initialize lifecycle and has no era header to
            // classify, so a `2026-07-28` initialize is the only way a
            // modern-aware local client can connect. The HTTP adapter, which
            // does classify eras, still answers `initialize` with
            // METHOD_NOT_FOUND in the modern era.
            if request.protocol_version == ProtocolVersion::V_2026_07_28
                && !self.refresh_modern_gate().await?
            {
                return Err(McpError::unsupported_protocol_version(
                    ProtocolVersion::V_2026_07_28,
                    &self.supported_protocol_versions(),
                ));
            }

            let client_name = request.client_info.name.to_string();
            tracing::info!(
                client = %client_name,
                requested_protocol = %request.protocol_version,
                selected_protocol = %selected_protocol_for_log(
                    &request.protocol_version,
                    &self.supported_protocol_versions(),
                ),
                "client connected via IPC proxy"
            );
            self.shared.conn.lock().await.client_info = Some(client_name.clone());

            // Forward client info to daemon for client-type-aware tool filtering
            if let Err(e) = self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::UpdateSession {
                        session_id: session_id.to_string(),
                        client_info: client_name.clone(),
                    }
                })
                .await
            {
                tracing::warn!(error = %e, "failed to update session client info");
            }

            // Track roots capability before consuming request
            self.shared.roots_supported.store(
                request.capabilities.roots.is_some(),
                std::sync::atomic::Ordering::SeqCst,
            );

            // Forward client capabilities to daemon for reverse-request gating
            let capabilities = request.capabilities.clone();
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::UpdateCapabilities {
                        session_id: session_id.to_string(),
                        capabilities: Box::new(capabilities.clone()),
                    }
                })
                .await
            {
                Ok(IpcResponse::Ok) => {
                    // Record for replay after a future daemon reconnect — see
                    // `ReplayState`. `conn` is not held here (the round trip
                    // above already released it), so this locks `replay`
                    // alone.
                    self.shared.replay.lock().await.client_capabilities =
                        Some(capabilities.clone());
                }
                Ok(other) => {
                    tracing::warn!(response = ?other, "unexpected IPC response updating session capabilities");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to update session capabilities");
                }
            }

            // Refresh daemon-derived server capabilities at handshake time so
            // downstream initialize sees the current routed surface, including
            // late-bound capabilities like tasks once tools are available.
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::Capabilities {
                        session_id: session_id.to_string(),
                    }
                })
                .await
            {
                Ok(IpcResponse::Capabilities { capabilities }) => {
                    if let Ok(parsed) = serde_json::from_value::<ServerCapabilities>(capabilities)
                        && let Ok(mut caps) = self.shared.capabilities.write()
                    {
                        *caps = parsed;
                    }
                }
                Ok(other) => {
                    tracing::warn!(response = ?other, "unexpected IPC capabilities response");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to refresh daemon capabilities");
                }
            }

            // Store peer for logging notification forwarding. The daemon
            // pushes LoggingNotification frames after registration; the
            // heartbeat and request round-trips forward them to this peer.
            let _ = self.shared.peer.set(context.peer.clone());
            flush_pending_daemon_notifications(&self.shared).await;

            context.peer.set_peer_info(request);
            Ok(self.get_info())
        }
    }

    fn discover(
        &self,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<DiscoverResult, McpError>> + Send + '_ {
        async move {
            if !self.refresh_modern_gate().await? {
                return Err(McpError::unsupported_protocol_version(
                    ProtocolVersion::V_2026_07_28,
                    &self.supported_protocol_versions(),
                ));
            }
            let client_info = context.client_info().ok_or_else(|| {
                McpError::invalid_params("discover requires client implementation metadata", None)
            })?;
            let capabilities = context.client_capabilities().ok_or_else(|| {
                McpError::invalid_params("discover requires client capability metadata", None)
            })?;
            let initialize = InitializeRequestParams::new(capabilities, client_info)
                .with_protocol_version(ProtocolVersion::V_2026_07_28);
            let mut info = self.initialize(initialize, context).await?;
            plug_core::protocol::suppress_unimplemented_modern_capabilities(&mut info.capabilities);
            Ok(DiscoverResult::from_server_info(
                self.supported_protocol_versions().into_owned(),
                info,
            ))
        }
    }

    fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        async move {
            let Some(request_id) = notification.request_id else {
                return;
            };
            let result =
                Self::send_cancellation_out_of_band(&self.shared, request_id, notification.reason)
                    .await;
            if let Err(error) = result {
                tracing::warn!(%error, "failed to forward downstream cancellation to daemon");
            }
        }
    }

    fn on_initialized(
        &self,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let shared = Arc::clone(&self.shared);
        async move {
            if !shared
                .roots_supported
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return;
            }
            if let Some(peer) = shared.peer.get().cloned() {
                let shared = shared.clone();
                tokio::spawn(async move {
                    refresh_roots_via_daemon(&shared, &peer).await;
                });
            }
        }
    }

    fn on_roots_list_changed(
        &self,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let shared = Arc::clone(&self.shared);
        async move {
            if !shared
                .roots_supported
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return;
            }
            if let Some(peer) = shared.peer.get().cloned() {
                let shared = shared.clone();
                tokio::spawn(async move {
                    refresh_roots_via_daemon(&shared, &peer).await;
                });
            }
        }
    }

    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            let params = request
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let response = self
                .mcp_round_trip(RetryPolicy::SafeToRetry, "tools/list", params, &context)
                .await?;
            decode_mcp(response, "tools/list")
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
        async move {
            let legacy_task_requested = request.meta.as_ref().is_some_and(|meta| {
                meta.contains_key(plug_core::protocol::LEGACY_TASK_REQUEST_KEY)
            }) || context
                .meta
                .contains_key(plug_core::protocol::LEGACY_TASK_REQUEST_KEY);

            // Serialize the full request so `_meta` (including
            // `progressToken`) survives to the daemon. A hand-built
            // `{name, arguments}` object drops the progress token and
            // silently disables progress on the default `plug connect` path.
            let mut params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            if !context.meta.is_empty()
                && let Some(params) = params.as_object_mut()
            {
                params.insert(
                    "_meta".to_string(),
                    serde_json::to_value(&context.meta)
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?,
                );
            }
            let response = self
                .mcp_round_trip(
                    RetryPolicy::UnsafeToRetry,
                    "tools/call",
                    Some(params),
                    &context,
                )
                .await?;
            let payload: serde_json::Value = decode_mcp(response, "tools/call")?;
            if legacy_task_requested {
                let task: LegacyCreateTaskResult =
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(format!("unexpected task response: {e}"), None)
                    })?;
                Ok(CallToolResponse::Task(rmcp::model::CreateTaskResult::new(
                    (&task.task).into(),
                )))
            } else if payload.get("resultType").and_then(|v| v.as_str()) == Some("input_required") {
                serde_json::from_value::<rmcp::model::InputRequiredResult>(payload)
                    .map(Into::into)
                    .map_err(|e| {
                        McpError::internal_error(
                            format!("unexpected input-required response: {e}"),
                            None,
                        )
                    })
            } else {
                serde_json::from_value::<CallToolResult>(payload)
                    .map(Into::into)
                    .map_err(|e| {
                        McpError::internal_error(
                            format!("unexpected tool call response: {e}"),
                            None,
                        )
                    })
            }
        }
    }

    fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CustomResult, McpError>> + Send + '_ {
        async move {
            let method = request
                .method
                .strip_prefix("plug/legacy/")
                .ok_or_else(|| {
                    McpError::new(ErrorCode::METHOD_NOT_FOUND, request.method.clone(), None)
                })?
                .to_string();
            let params = request.params.clone();
            let response = self
                .mcp_round_trip(RetryPolicy::SafeToRetry, &method, params, &context)
                .await?;
            decode_mcp(response, &method).map(CustomResult::new)
        }
    }

    fn set_level(
        &self,
        request: SetLevelRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        async move {
            let params = serde_json::json!({ "level": request.level });
            let response = self
                .mcp_round_trip(
                    RetryPolicy::SafeToRetry,
                    "logging/setLevel",
                    Some(params),
                    &context,
                )
                .await?;
            decode_mcp::<serde_json::Value>(response, "logging/setLevel")?;
            // Record for replay after a future daemon reconnect — see
            // `ReplayState`. `conn` is not held here.
            self.shared.replay.lock().await.log_level = Some(request.level);
            Ok(())
        }
    }

    fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        async move {
            let params = request
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let response = self
                .mcp_round_trip(RetryPolicy::SafeToRetry, "resources/list", params, &context)
                .await?;
            decode_mcp(response, "resources/list")
        }
    }

    fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, McpError>> + Send + '_ {
        async move {
            let params = request
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let response = self
                .mcp_round_trip(
                    RetryPolicy::SafeToRetry,
                    "resources/templates/list",
                    params,
                    &context,
                )
                .await?;
            decode_mcp(response, "resources/templates/list")
        }
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let response = self
                .mcp_round_trip(
                    RetryPolicy::SafeToRetry,
                    "resources/read",
                    Some(params),
                    &context,
                )
                .await?;
            decode_mcp::<ReadResourceResult>(response, "resources/read").map(Into::into)
        }
    }

    fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let response = self
                .mcp_round_trip(
                    RetryPolicy::SafeToRetry,
                    "resources/subscribe",
                    Some(params),
                    &context,
                )
                .await?;
            decode_mcp::<serde_json::Value>(response, "resources/subscribe")?;
            // Record for replay after a future daemon reconnect — see
            // `ReplayState`. `conn` is not held here. Only a successful
            // subscribe is replayed.
            self.shared
                .replay
                .lock()
                .await
                .subscriptions
                .insert(request.uri.clone());
            Ok(())
        }
    }

    fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let response = self
                .mcp_round_trip(
                    RetryPolicy::SafeToRetry,
                    "resources/unsubscribe",
                    Some(params),
                    &context,
                )
                .await?;
            decode_mcp::<serde_json::Value>(response, "resources/unsubscribe")?;
            // Remove from the replay set on success — a failed unsubscribe
            // must not stop the subscription from being replayed after a
            // future reconnect.
            self.shared
                .replay
                .lock()
                .await
                .subscriptions
                .remove(&request.uri);
            Ok(())
        }
    }

    fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        async move {
            let params = request
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let response = self
                .mcp_round_trip(RetryPolicy::SafeToRetry, "prompts/list", params, &context)
                .await?;
            decode_mcp(response, "prompts/list")
        }
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResponse, McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let response = self
                .mcp_round_trip(
                    RetryPolicy::SafeToRetry,
                    "prompts/get",
                    Some(params),
                    &context,
                )
                .await?;
            decode_mcp::<GetPromptResult>(response, "prompts/get").map(Into::into)
        }
    }

    fn complete(
        &self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CompleteResult, McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let response = self
                .mcp_round_trip(
                    RetryPolicy::SafeToRetry,
                    "completion/complete",
                    Some(params),
                    &context,
                )
                .await?;
            decode_mcp(response, "completion/complete")
        }
    }
}

/// Decode a daemon reply to an MCP request. Upstream errors ride back as an
/// `McpResponse` whose payload is a serialized `McpError`; surface those as
/// the error they are instead of failing to parse them as a result.
fn decode_mcp<T: serde::de::DeserializeOwned>(
    response: IpcResponse,
    method: &str,
) -> Result<T, McpError> {
    match response {
        IpcResponse::McpResponse { payload } => {
            if payload.get("code").is_some()
                && let Ok(err) = serde_json::from_value::<McpError>(payload.clone())
            {
                return Err(err);
            }
            serde_json::from_value(payload).map_err(|e| {
                McpError::internal_error(format!("failed to parse {method}: {e}"), None)
            })
        }
        IpcResponse::Error { code, message } => {
            Err(McpError::internal_error(format!("{code}: {message}"), None))
        }
        other => Err(McpError::internal_error(
            format!("unexpected IPC response: {other:?}"),
            None,
        )),
    }
}

/// Fetch roots from the downstream peer and push them to the daemon
/// via `IpcRequest::UpdateRoots`.
async fn refresh_roots_via_daemon(shared: &SharedConnection, peer: &Peer<RoleServer>) {
    let roots_result =
        match tokio::time::timeout(std::time::Duration::from_secs(10), peer.list_roots()).await {
            Ok(result) => result,
            Err(_) => {
                tracing::debug!("downstream roots request timed out");
                return;
            }
        };
    match roots_result {
        Ok(result) => {
            let roots_json = match serde_json::to_value(&result.roots) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(error = %e, "failed to serialize roots for IPC");
                    return;
                }
            };
            push_roots_to_daemon(shared, roots_json).await;
        }
        Err(error) => {
            tracing::debug!(error = %error, "failed to fetch roots from downstream peer");
        }
    }
}

/// Send `IpcRequest::UpdateRoots` through the shared round trip, so the reply
/// is read under the watchdog with push frames handled like any other call.
async fn push_roots_to_daemon(shared: &SharedConnection, roots_json: serde_json::Value) {
    let result =
        IpcProxyHandler::shared_round_trip(shared, RetryPolicy::SafeToRetry, |session_id| {
            IpcRequest::UpdateRoots {
                session_id: session_id.to_string(),
                roots: roots_json.clone(),
            }
        })
        .await;
    match result {
        Ok(IpcResponse::Ok) => {}
        Ok(IpcResponse::Error { code, message }) => {
            tracing::debug!(code = %code, message = %message, "daemon rejected UpdateRoots");
        }
        Ok(other) => {
            tracing::debug!(response = ?other, "unexpected UpdateRoots response");
        }
        Err(error) => {
            tracing::debug!(error = %error, "failed to send UpdateRoots to daemon");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use crate::daemon::{clear_test_runtime_paths, run_daemon, set_test_runtime_paths};
    use plug_core::config::{Config, ServerConfig, TransportType};
    use plug_core::engine::Engine;
    use plug_core::legacy_tasks::TaskMetadata;
    use rmcp::handler::client::ClientHandler;
    use rmcp::model::{
        CallToolRequest, CallToolRequestParams, ClientRequest, GetTaskParams, GetTaskRequest,
        RequestMetaObject as Meta, ServerResult, TaskStatus,
    };
    use rmcp::{ClientLifecycleMode, ClientServiceExt as _, ServiceExt as _};
    use tokio::io::AsyncWriteExt as _;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::task::JoinHandle;

    #[test]
    fn selected_protocol_log_reports_the_negotiated_version() {
        let legacy_only = plug_core::protocol::supported_downstream_protocol_versions(false);
        let with_modern = plug_core::protocol::supported_downstream_protocol_versions(true);
        assert_eq!(
            selected_protocol_for_log(&ProtocolVersion::V_2025_06_18, &legacy_only).as_str(),
            plug_core::protocol::SUPPORTED_PROTOCOL_VERSION,
            "an older client gets Plug's version, not the one it asked for"
        );
        assert_eq!(
            selected_protocol_for_log(&ProtocolVersion::V_2025_11_25, &legacy_only).as_str(),
            plug_core::protocol::SUPPORTED_PROTOCOL_VERSION
        );
        assert_eq!(
            selected_protocol_for_log(&ProtocolVersion::V_2026_07_28, &with_modern).as_str(),
            plug_core::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION
        );
    }

    // Shared with the daemon and runtime test modules: every test that touches the
    // global runtime-paths slot must serialize on the SAME lock so the suite is
    // safe under parallel threads (see daemon::runtime_paths_test_lock).
    fn daemon_test_lock() -> &'static tokio::sync::Mutex<()> {
        crate::daemon::runtime_paths_test_lock()
    }

    /// RAII guard that installs a short `READ_WATCHDOG` override for the
    /// life of a test (see `READ_WATCHDOG_TEST_OVERRIDE_MS`), restoring the
    /// "no override" state on drop — including on panic/unwind, so a failed
    /// assertion never leaks a short watchdog into a later test. The
    /// override is a single process-global value, so every test that
    /// installs one must hold `daemon_test_lock()` for its entire body
    /// (all fake-daemon tests in this module already do), and this guard
    /// must be declared AFTER the `daemon_test_lock()` guard so it drops
    /// (and resets the override) BEFORE the lock is released.
    struct ReadWatchdogTestOverride;

    impl ReadWatchdogTestOverride {
        fn install(duration: Duration) -> Self {
            READ_WATCHDOG_TEST_OVERRIDE_MS.store(
                duration.as_millis() as u64,
                std::sync::atomic::Ordering::SeqCst,
            );
            Self
        }
    }

    impl Drop for ReadWatchdogTestOverride {
        fn drop(&mut self) {
            READ_WATCHDOG_TEST_OVERRIDE_MS.store(0, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn artifact_base_dir() -> PathBuf {
        directories::ProjectDirs::from("", "", "plug")
            .map(|dirs| dirs.cache_dir().join("artifacts"))
            .unwrap_or_else(|| std::env::temp_dir().join("plug-artifacts"))
    }

    fn cleanup_artifact_uri(uri: &str) {
        let Some(rest) = uri.strip_prefix("plug://artifact/") else {
            return;
        };
        let Some((id, _)) = rest.split_once('/') else {
            return;
        };
        let _ = std::fs::remove_dir_all(artifact_base_dir().join(id));
    }

    fn ensure_mock_server_built() -> PathBuf {
        plug_test_harness::mock_server_bin()
    }

    #[derive(Clone)]
    struct TestClient;

    impl ClientHandler for TestClient {
        fn get_info(&self) -> ClientInfo {
            ClientInfo::new(
                ClientCapabilities::builder().enable_tasks().build(),
                Implementation::default(),
            )
            .with_protocol_version(plug_core::protocol::supported_protocol_version())
        }
    }

    trait LegacyTaskRequestExt {
        fn with_task(self, task: TaskMetadata) -> Self;
    }

    impl LegacyTaskRequestExt for CallToolRequestParams {
        fn with_task(mut self, task: TaskMetadata) -> Self {
            self.meta.get_or_insert_with(Default::default).insert(
                plug_core::protocol::LEGACY_TASK_REQUEST_KEY.to_string(),
                serde_json::to_value(task).expect("legacy task metadata serializes"),
            );
            self
        }
    }

    #[derive(Clone)]
    struct ResourceNotifyClient {
        notify: std::sync::Arc<tokio::sync::Notify>,
        uri: std::sync::Arc<tokio::sync::Mutex<Option<String>>>,
    }

    impl ClientHandler for ResourceNotifyClient {
        fn get_info(&self) -> ClientInfo {
            ClientInfo::default()
                .with_protocol_version(plug_core::protocol::supported_protocol_version())
        }

        async fn on_resource_updated(
            &self,
            params: ResourceUpdatedNotificationParam,
            _context: NotificationContext<rmcp::RoleClient>,
        ) {
            *self.uri.lock().await = Some(params.uri);
            self.notify.notify_one();
        }
    }

    fn mock_server_config_with_tools(tools: &str) -> ServerConfig {
        let mock_server = ensure_mock_server_built();
        ServerConfig {
            command: Some(mock_server.display().to_string()),
            args: vec!["--tools".to_string(), tools.to_string()],
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            protocol_mode: Default::default(),
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

    fn mock_server_config() -> ServerConfig {
        mock_server_config_with_tools("echo")
    }

    fn mock_server_config_with_resources() -> ServerConfig {
        let mut config = mock_server_config_with_tools("echo");
        config.args.push("--resources".to_string());
        config
    }

    async fn spawn_test_daemon(
        config: Config,
        config_path: std::path::PathBuf,
    ) -> (Arc<Engine>, JoinHandle<anyhow::Result<()>>) {
        let engine = Arc::new(Engine::new(config));
        engine.start().await.expect("engine start");
        let engine_for_task = Arc::clone(&engine);
        let handle =
            tokio::spawn(async move { run_daemon(engine_for_task, config_path, 0, None).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        if handle.is_finished() {
            let result = handle.await.expect("daemon task join");
            panic!("daemon exited before readiness: {result:?}");
        }
        tokio::time::timeout(
            Duration::from_secs(5),
            crate::runtime::wait_for_daemon_socket(),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "daemon ready timeout (socket path: {}, task_finished: {})",
                crate::daemon::socket_path().display(),
                handle.is_finished()
            )
        });
        (engine, handle)
    }

    // Wire-contract guard for the daemon-IPC progress regression: `call_tool`
    // must serialize the full `CallToolRequestParams` so `_meta.progressToken`
    // survives to the daemon. The previous hand-built `{name, arguments}`
    // object dropped it, silently disabling progress on the default
    // `plug connect` path. (End-to-end progress delivery over the daemon is
    // a separate harness gap — the mock server emits no progress.)
    #[test]
    fn ipc_tools_call_params_preserve_progress_token() {
        let mut request = CallToolRequestParams::new("Mock__echo");
        request.meta = Some(Meta::with_progress_token(ProgressToken(
            NumberOrString::Number(42),
        )));

        let params = serde_json::to_value(&request).expect("serialize call params");

        assert_eq!(
            params
                .get("_meta")
                .and_then(|m| m.get("progressToken"))
                .and_then(|t| t.as_i64()),
            Some(42),
            "serialized IPC tools/call params must carry _meta.progressToken"
        );
    }

    #[tokio::test]
    async fn daemon_backed_proxy_recovers_after_daemon_restart() {
        let _guard = daemon_test_lock().lock().await;

        let temp = std::env::temp_dir().join(format!(
            "pdc-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = temp.join("plug.toml");
        let mut config = Config::default();
        config
            .servers
            .insert("mock".to_string(), mock_server_config());
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let (engine_a, daemon_a) = spawn_test_daemon(config.clone(), config_path.clone()).await;

        let session = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            "client-continuity".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, Some(config_path.clone()));
        let shared = Arc::clone(&proxy.shared);
        let initial_session_id = {
            let conn = shared.conn.lock().await;
            conn.session_id.clone()
        };

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        let initial_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let _initial_tools = loop {
            let tools =
                tokio::time::timeout(Duration::from_secs(5), client.peer().list_all_tools())
                    .await
                    .expect("initial tools timeout")
                    .expect("initial tools");
            if tools.iter().any(|tool| tool.name == "Mock__echo") {
                break tools;
            }
            assert!(
                tokio::time::Instant::now() < initial_deadline,
                "expected daemon-backed proxy to expose Mock__echo"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let initial_result = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool(
                CallToolRequestParams::new("Mock__echo").with_arguments(
                    serde_json::json!({"input": "before"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            ),
        )
        .await
        .expect("initial tool call timeout")
        .expect("initial tool call");
        assert!(format!("{initial_result:?}").contains("before"));

        engine_a.shutdown().await;
        daemon_a
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        let (engine_b, daemon_b) = spawn_test_daemon(config, config_path.clone()).await;

        let engine_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            match engine_b
                .tool_router()
                .call_tool(
                    "Mock__echo",
                    Some(
                        serde_json::json!({"input": "engine-ready"})
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                )
                .await
            {
                Ok(_) => break,
                Err(error)
                    if error.message.contains("server unavailable")
                        && tokio::time::Instant::now() < engine_deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => panic!("restarted daemon never became ready: {error:?}"),
            }
        }

        let repaired_tools =
            tokio::time::timeout(Duration::from_secs(5), client.peer().list_all_tools())
                .await
                .expect("tools after reconnect timeout")
                .expect("tools after reconnect");
        assert!(
            repaired_tools.iter().any(|tool| tool.name == "Mock__echo"),
            "expected repaired proxy to expose Mock__echo"
        );
        let repaired_session_id = { shared.conn.lock().await.session_id.clone() };
        assert_ne!(
            repaired_session_id, initial_session_id,
            "reconnect should replace the daemon session"
        );

        engine_b.shutdown().await;
        daemon_b
            .await
            .expect("restarted daemon task join")
            .expect("restarted daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn daemon_backed_proxy_supports_task_wrapped_tool_calls() {
        let _guard = daemon_test_lock().lock().await;

        let temp = std::env::temp_dir().join(format!(
            "pdt-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = temp.join("plug.toml");
        let mut config = Config::default();
        config
            .servers
            .insert("mock".to_string(), mock_server_config());
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let (engine, daemon) = spawn_test_daemon(config, config_path.clone()).await;

        let session = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            "client-tasks".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, Some(config_path.clone()));

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");
        let server_info = client
            .peer()
            .peer_info()
            .expect("server initialize info available");
        assert!(
            plug_core::protocol::legacy_tasks_capability(&server_info.capabilities),
            "IPC proxy should advertise tasks capability when routed tools exist"
        );

        let task_request = CallToolRequestParams::new("Mock__echo")
            .with_arguments(
                serde_json::json!({"input": "task-mode"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .with_task(TaskMetadata::new());

        let create_response = tokio::time::timeout(
            Duration::from_secs(5),
            client
                .peer()
                .send_request(ClientRequest::CallToolRequest(CallToolRequest::new(
                    task_request,
                ))),
        )
        .await
        .expect("task create timeout")
        .expect("task create response");

        let task_id = match create_response {
            ServerResult::CreateTaskResult(result) => {
                assert_eq!(result.task.status, TaskStatus::Working);
                result.task.task_id
            }
            other => panic!("unexpected create task response: {other:?}"),
        };

        // This connection negotiated the legacy protocol. RMCP 3's SEP-2663
        // typed task method must remain unavailable here; the outer stdio
        // adapter maps legacy SEP-1686 `tasks/get` onto Plug's private custom
        // method before this handler sees it.
        let modern_error = client
            .peer()
            .send_request(ClientRequest::GetTaskRequest(GetTaskRequest::new(
                GetTaskParams::new(task_id.clone()),
            )))
            .await
            .expect_err("modern tasks/get must not be accepted on a legacy connection");
        let rmcp::service::ServiceError::McpError(modern_error) = modern_error else {
            panic!("expected MCP method-not-found, got {modern_error:?}");
        };
        assert_eq!(modern_error.code, ErrorCode::METHOD_NOT_FOUND);

        let final_status = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let response = client
                    .peer()
                    .send_request(ClientRequest::CustomRequest(CustomRequest::new(
                        "plug/legacy/tasks/get",
                        Some(serde_json::json!({"taskId": task_id})),
                    )))
                    .await
                    .expect("task info response");
                let result = plug_core::legacy_tasks::parse_get_result(response)
                    .expect("legacy task info response shape");
                if result.task.status == plug_core::legacy_tasks::TaskStatus::Completed {
                    break result.task;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("task completion timeout");
        assert_eq!(
            final_status.status,
            plug_core::legacy_tasks::TaskStatus::Completed
        );

        let result = client
            .peer()
            .send_request(ClientRequest::CustomRequest(CustomRequest::new(
                "plug/legacy/tasks/result",
                Some(serde_json::json!({"taskId": task_id})),
            )))
            .await
            .expect("task result response");
        let result = match result {
            ServerResult::CustomResult(result) => result.0,
            // The payload is itself a CallToolResult, so RMCP's response
            // classifier may recover the typed variant for this custom legacy
            // method. Both representations are the same legacy wire value.
            ServerResult::CallToolResult(result) => {
                serde_json::to_value(result).expect("task result serializes")
            }
            other => panic!("unexpected task result response: {other:?}"),
        };
        assert!(result.to_string().contains("task-mode"));

        engine.shutdown().await;
        daemon
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn daemon_backed_proxy_forwards_resource_subscribe_updates() {
        let _guard = daemon_test_lock().lock().await;

        let temp = std::env::temp_dir().join(format!(
            "pdr-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = temp.join("plug.toml");
        let mut config = Config::default();
        config
            .servers
            .insert("mock".to_string(), mock_server_config_with_resources());
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let (engine, daemon) = spawn_test_daemon(config, config_path.clone()).await;

        let session = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            "client-resources".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, Some(config_path.clone()));

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        let updated_uri = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let client = ResourceNotifyClient {
            notify: notify.clone(),
            uri: updated_uri.clone(),
        }
        .serve(client_transport)
        .await
        .expect("connect downstream client");

        let server_info = client
            .peer()
            .peer_info()
            .expect("server initialize info available");
        assert_eq!(
            server_info
                .capabilities
                .resources
                .as_ref()
                .and_then(|resources| resources.subscribe),
            Some(true),
            "daemon-backed proxy should advertise resource subscribe when upstream supports it"
        );

        let resource_uri = "file:///tmp/mock-resource.txt";
        tokio::time::timeout(
            Duration::from_secs(5),
            client
                .peer()
                .subscribe(SubscribeRequestParams::new(resource_uri)),
        )
        .await
        .expect("resource subscribe timeout")
        .expect("resource subscribe");

        tokio::time::timeout(Duration::from_secs(5), notify.notified())
            .await
            .expect("resource updated notification timeout");
        assert_eq!(
            updated_uri.lock().await.as_deref(),
            Some(resource_uri),
            "resource update should be forwarded through daemon IPC to the stdio client"
        );

        tokio::time::timeout(
            Duration::from_secs(5),
            client
                .peer()
                .unsubscribe(UnsubscribeRequestParams::new(resource_uri)),
        )
        .await
        .expect("resource unsubscribe timeout")
        .expect("resource unsubscribe");

        engine.shutdown().await;
        daemon
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn daemon_backed_proxy_tasks_survive_session_replacement_for_same_client() {
        let _guard = daemon_test_lock().lock().await;

        let temp = std::env::temp_dir().join(format!(
            "pdu-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = temp.join("plug.toml");
        let mut config = Config::default();
        config
            .servers
            .insert("mock".to_string(), mock_server_config());
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let (engine, daemon) = spawn_test_daemon(config, config_path.clone()).await;

        let client_id = "client-task-continuity".to_string();
        let session_a = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            client_id.clone(),
            None,
        )
        .await
        .expect("establish first daemon proxy session");
        let proxy_a = IpcProxyHandler::new(session_a, Some(config_path.clone()));

        let (server_transport_a, client_transport_a) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy_a
                .serve(server_transport_a)
                .await
                .expect("start first IPC proxy server");
            let _ = server.waiting().await;
        });

        let client_a = TestClient
            .serve(client_transport_a)
            .await
            .expect("connect first downstream client");

        let task_request = CallToolRequestParams::new("Mock__echo")
            .with_arguments(
                serde_json::json!({"input": "continuity"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .with_task(TaskMetadata::new());

        let create_response = client_a
            .peer()
            .send_request(ClientRequest::CallToolRequest(CallToolRequest::new(
                task_request,
            )))
            .await
            .expect("task create response");
        let task_id = match create_response {
            ServerResult::CreateTaskResult(result) => result.task.task_id,
            other => panic!("unexpected create task response: {other:?}"),
        };

        let session_b =
            crate::runtime::establish_daemon_proxy_session(Some(&config_path), client_id, None)
                .await
                .expect("establish replacement daemon proxy session");
        let proxy_b = IpcProxyHandler::new(session_b, Some(config_path.clone()));

        let (server_transport_b, client_transport_b) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy_b
                .serve(server_transport_b)
                .await
                .expect("start replacement IPC proxy server");
            let _ = server.waiting().await;
        });

        let client_b = TestClient
            .serve(client_transport_b)
            .await
            .expect("connect replacement downstream client");

        let final_status = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let response = client_b
                    .peer()
                    .send_request(ClientRequest::CustomRequest(CustomRequest::new(
                        "plug/legacy/tasks/get",
                        Some(serde_json::json!({"taskId": task_id})),
                    )))
                    .await
                    .expect("replacement task info response");
                let result = plug_core::legacy_tasks::parse_get_result(response)
                    .expect("legacy replacement task info response shape");
                if result.task.status == plug_core::legacy_tasks::TaskStatus::Completed {
                    break result.task;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("task completion timeout after session replacement");
        assert_eq!(final_status.task_id, task_id);
        assert_eq!(
            final_status.status,
            plug_core::legacy_tasks::TaskStatus::Completed
        );

        let payload = client_b
            .peer()
            .send_request(ClientRequest::CustomRequest(CustomRequest::new(
                "plug/legacy/tasks/result",
                Some(serde_json::json!({"taskId": task_id})),
            )))
            .await
            .expect("replacement task payload response");
        assert!(
            plug_core::legacy_tasks::parse_payload_result(payload)
                .expect("legacy replacement task payload shape")
                .0
                .to_string()
                .contains("continuity")
        );

        engine.shutdown().await;
        daemon
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn daemon_backed_proxy_advertises_latest_protocol_version() {
        let _guard = daemon_test_lock().lock().await;

        let temp = std::env::temp_dir().join(format!(
            "pdc-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = temp.join("plug.toml");
        let mut config = Config::default();
        config
            .servers
            .insert("mock".to_string(), mock_server_config());
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let (engine, daemon) = spawn_test_daemon(config, config_path.clone()).await;

        let session = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            "client-protocol-version".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, Some(config_path));

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        let server_info = client
            .peer()
            .peer_info()
            .expect("server initialize info available");
        assert_eq!(server_info.protocol_version.as_str(), "2025-11-25");
        let icons = server_info
            .server_info
            .as_ref()
            .expect("server implementation advertised")
            .icons
            .as_ref()
            .expect("plug icons advertised");
        assert_ipc_plug_icons_sequence(icons);

        engine.shutdown().await;
        daemon
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    fn assert_ipc_plug_icons_sequence(icons: &[Icon]) {
        let expected_sizes = ["16x16", "32x32", "64x64", "128x128", "256x256", "512x512"];
        assert_eq!(icons.len(), expected_sizes.len() + 1);

        for (icon, expected_size) in icons.iter().zip(expected_sizes) {
            assert!(icon.src.starts_with("data:image/png;base64,"));
            assert_eq!(icon.mime_type.as_deref(), Some("image/png"));
            assert_eq!(
                icon.sizes
                    .as_ref()
                    .and_then(|sizes| sizes.first())
                    .map(String::as_str),
                Some(expected_size)
            );
        }

        let svg = icons.last().expect("svg fallback icon");
        assert!(svg.src.starts_with("data:image/svg+xml;base64,"));
        assert_eq!(svg.mime_type.as_deref(), Some("image/svg+xml"));
        assert_eq!(svg.sizes.as_deref(), Some(&["any".to_string()][..]));
    }

    #[tokio::test]
    async fn daemon_backed_proxy_reassembles_chunked_tool_response() {
        let _guard = daemon_test_lock().lock().await;

        let temp = std::env::temp_dir().join(format!(
            "pdchunk-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = temp.join("plug.toml");
        let mut config = Config::default();
        config.servers.insert(
            "mock".to_string(),
            mock_server_config_with_tools("chunked_text"),
        );
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let (engine, daemon) = spawn_test_daemon(config, config_path.clone()).await;

        let session = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            "client-chunked".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, Some(config_path.clone()));

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            client.call_tool(CallToolRequestParams::new("Mock__chunked_text")),
        )
        .await
        .expect("chunked tool call timeout")
        .expect("chunked tool call");

        let first = result.content.first().expect("content");
        let text = first.as_text().expect("text content");
        assert_eq!(text.text.len(), 6 * 1024 * 1024);
        assert_eq!(result.is_error, Some(false));

        engine.shutdown().await;
        daemon
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn daemon_backed_proxy_spills_large_tool_result_to_artifact_link() {
        let _guard = daemon_test_lock().lock().await;

        let temp = std::env::temp_dir().join(format!(
            "pdartifact-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = temp.join("plug.toml");
        let mut config = Config::default();
        config.servers.insert(
            "mock".to_string(),
            mock_server_config_with_tools("artifact_text"),
        );
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let (engine, daemon) = spawn_test_daemon(config, config_path.clone()).await;

        let session = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            "client-artifact".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, Some(config_path.clone()));

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            client.call_tool(CallToolRequestParams::new("Mock__artifact_text")),
        )
        .await
        .expect("artifact tool call timeout")
        .expect("artifact tool call");

        let resource = result
            .content
            .iter()
            .find_map(ContentBlock::as_resource_link)
            .expect("artifact resource_link content");
        assert!(resource.uri.starts_with("plug://artifact/"));
        assert_eq!(result.is_error, Some(false));

        engine.shutdown().await;
        daemon
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn daemon_backed_proxy_task_result_spills_to_artifact_link() {
        let _guard = daemon_test_lock().lock().await;

        let temp = std::env::temp_dir().join(format!(
            "pdtartifact-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = temp.join("plug.toml");
        let mut config = Config::default();
        config.servers.insert(
            "mock".to_string(),
            mock_server_config_with_tools("artifact_text"),
        );
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let (engine, daemon) = spawn_test_daemon(config, config_path.clone()).await;

        let session = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            "client-task-artifact".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, Some(config_path.clone()));

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        let task_request =
            CallToolRequestParams::new("Mock__artifact_text").with_task(TaskMetadata::new());

        let create_response = client
            .peer()
            .send_request(ClientRequest::CallToolRequest(CallToolRequest::new(
                task_request,
            )))
            .await
            .expect("task create response");
        let task_id = match create_response {
            ServerResult::CreateTaskResult(result) => result.task.task_id,
            other => panic!("unexpected create task response: {other:?}"),
        };

        let payload_response = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let response = client
                    .peer()
                    .send_request(ClientRequest::CustomRequest(CustomRequest::new(
                        "plug/legacy/tasks/get",
                        Some(serde_json::json!({"taskId": task_id})),
                    )))
                    .await
                    .expect("task result response");
                let result = plug_core::legacy_tasks::parse_get_result(response)
                    .expect("legacy task info response shape");
                if result.task.status == plug_core::legacy_tasks::TaskStatus::Completed {
                    let response = client
                        .peer()
                        .send_request(ClientRequest::CustomRequest(CustomRequest::new(
                            "plug/legacy/tasks/result",
                            Some(serde_json::json!({"taskId": task_id})),
                        )))
                        .await
                        .expect("task payload response");
                    break plug_core::legacy_tasks::parse_payload_result(response)
                        .expect("legacy task payload response shape");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("task result timeout");

        let payload_text = payload_response.0.to_string();
        assert_ne!(payload_response.0["isError"], true);
        assert!(payload_text.contains("resource_link"), "{payload_text}");
        assert!(payload_text.contains("plug://artifact/"), "{payload_text}");

        engine.shutdown().await;
        daemon
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn daemon_backed_proxy_artifact_manifest_survives_daemon_restart() {
        let _guard = daemon_test_lock().lock().await;

        let temp = std::env::temp_dir().join(format!(
            "pdrehydrate-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = temp.join("plug.toml");
        let mut config = Config::default();
        config.servers.insert(
            "mock".to_string(),
            mock_server_config_with_tools("artifact_text"),
        );
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let (engine_a, daemon_a) = spawn_test_daemon(config.clone(), config_path.clone()).await;

        let session_a = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            "client-rehydrate-a".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy_a = IpcProxyHandler::new(session_a, Some(config_path.clone()));

        let (server_transport_a, client_transport_a) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy_a
                .serve(server_transport_a)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client_a = TestClient
            .serve(client_transport_a)
            .await
            .expect("connect downstream client");

        let result = client_a
            .call_tool(CallToolRequestParams::new("Mock__artifact_text"))
            .await
            .expect("artifact tool call");
        let manifest_uri = result
            .content
            .iter()
            .find_map(ContentBlock::as_resource_link)
            .expect("artifact resource_link")
            .uri
            .clone();

        engine_a.shutdown().await;
        daemon_a
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        let (engine_b, daemon_b) = spawn_test_daemon(config, config_path.clone()).await;

        let session_b = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            "client-rehydrate-b".to_string(),
            None,
        )
        .await
        .expect("establish replacement daemon proxy session");
        let proxy_b = IpcProxyHandler::new(session_b, Some(config_path.clone()));

        let (server_transport_b, client_transport_b) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy_b
                .serve(server_transport_b)
                .await
                .expect("start replacement IPC proxy server");
            let _ = server.waiting().await;
        });

        let client_b = TestClient
            .serve(client_transport_b)
            .await
            .expect("connect replacement downstream client");

        let manifest = client_b
            .peer()
            .read_resource(ReadResourceRequestParams::new(manifest_uri.clone()))
            .await
            .expect("read rehydrated manifest");
        assert_eq!(manifest.contents.len(), 1);

        cleanup_artifact_uri(&manifest_uri);
        engine_b.shutdown().await;
        daemon_b
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    // Fake-daemon harness: hand-rolled UnixListener speaking plug_core::ipc
    // so reconnect/frame tests control wire responses without Engine hooks.
    // Tests abort the handler heartbeat after construction (avoids racing
    // DAEMON_PING_INTERVAL) and take `daemon_test_lock()` for socket paths.

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ))
    }

    fn is_mcp_request(request: &IpcRequest, expected_method: &str) -> bool {
        match request {
            IpcRequest::McpRequest { method, .. }
            | IpcRequest::McpRequestWithContext { method, .. } => method == expected_method,
            _ => false,
        }
    }

    fn bind_fake_daemon_socket() -> UnixListener {
        let path = crate::daemon::socket_path();
        std::fs::create_dir_all(path.parent().expect("socket path has a parent"))
            .expect("create daemon socket directory");
        let _ = std::fs::remove_file(&path);
        UnixListener::bind(&path).expect("bind fake daemon socket")
    }

    /// Answer the `OperatorHandshake` every session setup opens with.
    async fn answer_operator_handshake(reader: &mut OwnedReadHalf, writer: &mut OwnedWriteHalf) {
        let frame = ipc::read_frame(reader)
            .await
            .expect("read operator handshake frame")
            .expect("connection closed before operator handshake");
        let req: IpcRequest =
            serde_json::from_slice(&frame).expect("parse operator handshake request");
        match req {
            IpcRequest::OperatorHandshake {
                client_version,
                ipc_min,
                ipc_max,
            } => {
                assert_eq!(client_version, env!("CARGO_PKG_VERSION"));
                assert!(ipc_min <= ipc::OPERATOR_IPC_MAX);
                assert!(ipc_max >= ipc::OPERATOR_IPC_MIN);
                ipc::send_response(
                    writer,
                    &IpcResponse::OperatorHandshake {
                        handshake: ipc::OperatorHandshake {
                            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                            daemon_executable: Some(
                                std::env::current_exe().expect("test executable path"),
                            ),
                            ipc_min: ipc::OPERATOR_IPC_MIN,
                            ipc_max: ipc::OPERATOR_IPC_MAX,
                            ownership: ipc::DaemonOwnershipMode::Unmanaged,
                            capabilities: Vec::new(),
                        },
                    },
                )
                .await
                .expect("send OperatorHandshake");
            }
            other => panic!("expected OperatorHandshake, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_setup_rejects_a_registration_for_another_client() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("register-mismatch");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, mut writer) = stream.into_split();
            answer_operator_handshake(&mut reader, &mut writer).await;
            ipc::read_frame(&mut reader)
                .await
                .expect("read register frame")
                .expect("connection open");
            ipc::send_response(
                &mut writer,
                &IpcResponse::Registered {
                    protocol_version: ipc::IPC_PROTOCOL_VERSION,
                    client_id: "someone-else".to_string(),
                    session_id: "stolen-session".to_string(),
                    modern_downstream_enabled: false,
                    cancellation_capability: ipc::IpcCancellationCapability::new(
                        "capability".to_string(),
                    ),
                },
            )
            .await
            .expect("send Registered");
        });

        let error = match crate::runtime::establish_daemon_proxy_session(
            None,
            "client-register-mismatch".to_string(),
            None,
        )
        .await
        {
            Ok(_) => panic!("a registration for another client must fail setup"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("registration mismatch"),
            "unexpected error: {error}"
        );

        daemon_task.await.expect("daemon task join");
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// Perform the OperatorHandshake + Register + Capabilities handshake that
    /// `establish_daemon_proxy_session` sends on the initial connect AND on
    /// every reconnect, replying with `session_id` and default
    /// `ServerCapabilities`. Returns the split halves for further scripted
    /// interaction plus which request types were observed, so callers can
    /// pin exactly what reconnect does (and does not) replay.
    async fn fake_daemon_handshake(
        stream: UnixStream,
        session_id: &str,
    ) -> (OwnedReadHalf, OwnedWriteHalf, Vec<String>) {
        fake_daemon_handshake_with_gate(stream, session_id, false).await
    }

    async fn fake_daemon_handshake_with_gate(
        stream: UnixStream,
        session_id: &str,
        modern_downstream_enabled: bool,
    ) -> (OwnedReadHalf, OwnedWriteHalf, Vec<String>) {
        fake_daemon_handshake_with_gate_and_capabilities(
            stream,
            session_id,
            modern_downstream_enabled,
            ServerCapabilities::default(),
        )
        .await
    }

    async fn fake_daemon_handshake_with_gate_and_capabilities(
        stream: UnixStream,
        session_id: &str,
        modern_downstream_enabled: bool,
        capabilities: ServerCapabilities,
    ) -> (OwnedReadHalf, OwnedWriteHalf, Vec<String>) {
        let (mut reader, mut writer) = stream.into_split();
        let mut seen = Vec::new();
        answer_operator_handshake(&mut reader, &mut writer).await;
        seen.push("OperatorHandshake".to_string());

        let frame = ipc::read_frame(&mut reader)
            .await
            .expect("read register frame")
            .expect("connection closed before register");
        let req: IpcRequest = serde_json::from_slice(&frame).expect("parse register request");
        let (protocol_version, client_id) = match req {
            IpcRequest::Register {
                protocol_version,
                client_id,
                ..
            } => {
                seen.push("Register".to_string());
                (protocol_version, client_id)
            }
            other => panic!("expected Register, got {other:?}"),
        };
        ipc::send_response(
            &mut writer,
            &IpcResponse::Registered {
                protocol_version,
                client_id,
                session_id: session_id.to_string(),
                modern_downstream_enabled,
                cancellation_capability: ipc::IpcCancellationCapability::new(
                    "fake-cancellation-capability".to_string(),
                ),
            },
        )
        .await
        .expect("send Registered");

        let frame = ipc::read_frame(&mut reader)
            .await
            .expect("read capabilities frame")
            .expect("connection closed before capabilities");
        let req: IpcRequest = serde_json::from_slice(&frame).expect("parse capabilities request");
        match req {
            IpcRequest::Capabilities { .. } => seen.push("Capabilities".to_string()),
            other => panic!("expected Capabilities, got {other:?}"),
        }
        ipc::send_response(
            &mut writer,
            &IpcResponse::Capabilities {
                capabilities: serde_json::to_value(capabilities)
                    .expect("serialize daemon capabilities"),
            },
        )
        .await
        .expect("send Capabilities");

        (reader, writer, seen)
    }

    /// Extend `fake_daemon_handshake` with the three extra round trips
    /// `IpcProxyHandler::initialize` performs the FIRST time a downstream
    /// MCP client connects (`UpdateSession`, `UpdateCapabilities`, a
    /// `Capabilities` refresh) — NOT repeated on later reconnects, which
    /// only redo OperatorHandshake+Register+Capabilities (see
    /// `reconnect_reregisters_with_register_and_capabilities_only` below).
    /// Returns once the daemon side has answered the final `Capabilities`
    /// refresh, matching the point at which production populates
    /// `shared.peer`.
    async fn drive_fake_daemon_initialize(
        stream: UnixStream,
        session_id: &str,
    ) -> (OwnedReadHalf, OwnedWriteHalf) {
        let (mut reader, mut writer, _handshake_seen) =
            fake_daemon_handshake(stream, session_id).await;

        loop {
            let frame = ipc::read_frame(&mut reader)
                .await
                .expect("read initialize request")
                .expect("connection closed during initialize");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse initialize request");
            match req {
                IpcRequest::UpdateSession { .. } => {
                    ipc::send_response(&mut writer, &IpcResponse::Ok)
                        .await
                        .expect("send UpdateSession ack");
                }
                IpcRequest::UpdateCapabilities { .. } => {
                    ipc::send_response(&mut writer, &IpcResponse::Ok)
                        .await
                        .expect("send UpdateCapabilities ack");
                }
                IpcRequest::Capabilities { .. } => {
                    ipc::send_response(
                        &mut writer,
                        &IpcResponse::Capabilities {
                            capabilities: serde_json::to_value(ServerCapabilities::default())
                                .expect("serialize default capabilities"),
                        },
                    )
                    .await
                    .expect("send Capabilities refresh");
                    return (reader, writer);
                }
                other => panic!("unexpected request during fake daemon initialize: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn modern_daemon_proxy_starts_with_discover_without_initialize() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("modern-discover");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let mut delegated_capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_resources_subscribe()
            .enable_tasks()
            .build();
        delegated_capabilities.experimental = Some(Default::default());
        let daemon_capabilities = delegated_capabilities.clone();
        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept proxy");
            let (mut reader, mut writer, _) = fake_daemon_handshake_with_gate_and_capabilities(
                stream,
                "modern-session",
                true,
                daemon_capabilities.clone(),
            )
            .await;

            let expected = ["UpdateSession", "UpdateCapabilities", "Capabilities"];
            let mut lifecycle_index = 0;
            while lifecycle_index < expected.len() {
                let frame = ipc::read_frame(&mut reader)
                    .await
                    .expect("read lifecycle IPC frame")
                    .expect("proxy remains connected");
                let request: IpcRequest =
                    serde_json::from_slice(&frame).expect("parse lifecycle IPC frame");
                let observed = match request {
                    IpcRequest::UpdateSession { .. } => "UpdateSession",
                    IpcRequest::UpdateCapabilities { .. } => "UpdateCapabilities",
                    IpcRequest::Capabilities { .. } => "Capabilities",
                    IpcRequest::Ping { .. } => {
                        ipc::send_response(&mut writer, &IpcResponse::Pong)
                            .await
                            .expect("send heartbeat response");
                        continue;
                    }
                    IpcRequest::ModernDownstreamGate { .. } => {
                        ipc::send_response(
                            &mut writer,
                            &IpcResponse::ModernDownstreamGate { enabled: true },
                        )
                        .await
                        .expect("send authoritative gate response");
                        continue;
                    }
                    IpcRequest::McpRequest { method, .. }
                    | IpcRequest::McpRequestWithContext { method, .. } => {
                        panic!("discover lifecycle must not forward {method} over IPC")
                    }
                    other => panic!("unexpected lifecycle IPC request: {other:?}"),
                };
                assert_eq!(observed, expected[lifecycle_index]);
                let response = if observed == "Capabilities" {
                    IpcResponse::Capabilities {
                        capabilities: serde_json::to_value(&daemon_capabilities)
                            .expect("serialize capabilities"),
                    }
                } else {
                    IpcResponse::Ok
                };
                ipc::send_response(&mut writer, &response)
                    .await
                    .expect("send lifecycle response");
                lifecycle_index += 1;
            }
        });

        let session =
            crate::runtime::establish_daemon_proxy_session(None, "modern-client".to_string(), None)
                .await
                .expect("establish modern daemon session");
        let proxy = IpcProxyHandler::new(session, None);
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move { proxy.serve(server_transport).await });
        let client = tokio::time::timeout(
            Duration::from_secs(5),
            ().serve_with_lifecycle(
                client_transport,
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            ),
        )
        .await
        .expect("discover lifecycle completes")
        .expect("discover-first client connects");
        let server = tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("proxy startup completes")
            .expect("proxy startup task")
            .expect("serve proxy");
        let discovered = client.peer_info().expect("server discovery result");
        assert_eq!(discovered.protocol_version, ProtocolVersion::V_2026_07_28);
        assert!(discovered.capabilities.tools.is_some());
        assert!(!discovered.capabilities.supports_tasks());
        assert!(discovered.capabilities.experimental.is_none());
        assert!(discovered.capabilities.extensions.is_none());
        assert_eq!(
            discovered
                .capabilities
                .resources
                .as_ref()
                .and_then(|resources| resources.subscribe),
            None
        );

        tokio::time::timeout(Duration::from_secs(5), client.cancel())
            .await
            .expect("client cancellation completes")
            .expect("stop client");
        tokio::time::timeout(Duration::from_secs(5), server.cancel())
            .await
            .expect("server cancellation completes")
            .expect("stop server");
        tokio::time::timeout(Duration::from_secs(5), daemon_task)
            .await
            .expect("fake daemon completes")
            .expect("daemon task");
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn cancellation_uses_auxiliary_socket_while_primary_request_is_locked() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("oob-cancel");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let request_id =
            RequestId::from(rmcp::model::NumberOrString::String(Arc::from("cancel-me")));
        let expected_request_id = request_id.clone();
        let daemon_task = tokio::spawn(async move {
            let (primary, _) = listener.accept().await.expect("accept primary socket");
            let (_primary_reader, _primary_writer, _) =
                fake_daemon_handshake(primary, "primary-session").await;

            let (mut auxiliary, _) = listener.accept().await.expect("accept cancellation socket");
            let frame = ipc::read_frame(&mut auxiliary)
                .await
                .expect("read cancellation frame")
                .expect("cancellation socket remains open");
            let request: IpcRequest =
                serde_json::from_slice(&frame).expect("parse cancellation request");
            assert!(matches!(
                request,
                IpcRequest::CancelMcpRequest {
                    session_id,
                    client_id,
                    cancellation_capability,
                    request_id,
                    reason,
                } if session_id == "primary-session"
                    && client_id == "stable-client"
                    && cancellation_capability.expose_secret()
                        == "fake-cancellation-capability"
                    && request_id == expected_request_id
                    && reason.as_deref() == Some("user cancelled")
            ));
            ipc::send_response(&mut auxiliary, &IpcResponse::Ok)
                .await
                .expect("ack cancellation");
        });

        let session =
            crate::runtime::establish_daemon_proxy_session(None, "stable-client".to_string(), None)
                .await
                .expect("establish primary session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let _primary_lock = proxy.shared.conn.lock().await;
        tokio::time::timeout(
            Duration::from_secs(5),
            IpcProxyHandler::send_cancellation_out_of_band(
                &proxy.shared,
                request_id,
                Some("user cancelled".to_string()),
            ),
        )
        .await
        .expect("cancellation must not wait for primary connection lock")
        .expect("daemon accepts cancellation");

        daemon_task.await.expect("daemon task");
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn daemon_gate_push_disables_modern_on_existing_proxy() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("gate-push");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept proxy");
            let (mut reader, mut writer, _) =
                fake_daemon_handshake_with_gate(stream, "gate-session", true).await;
            let frame = ipc::read_frame(&mut reader)
                .await
                .expect("read ping")
                .expect("connection remains open");
            let request: IpcRequest = serde_json::from_slice(&frame).expect("parse ping");
            assert!(matches!(request, IpcRequest::Ping { .. }));
            ipc::send_response(
                &mut writer,
                &IpcResponse::ModernDownstreamGateChanged { enabled: false },
            )
            .await
            .expect("push gate disable");
            ipc::send_response(&mut writer, &IpcResponse::Pong)
                .await
                .expect("send pong");
        });

        let session =
            crate::runtime::establish_daemon_proxy_session(None, "gate-client".to_string(), None)
                .await
                .expect("establish gated session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();
        assert!(
            proxy
                .supported_protocol_versions()
                .contains(&ProtocolVersion::V_2026_07_28)
        );
        let response = proxy
            .session_round_trip(RetryPolicy::SafeToRetry, |session_id| IpcRequest::Ping {
                session_id: session_id.to_string(),
            })
            .await
            .expect("gate push followed by pong");
        assert!(matches!(response, IpcResponse::Pong));
        assert!(
            !proxy
                .supported_protocol_versions()
                .contains(&ProtocolVersion::V_2026_07_28),
            "existing proxy must immediately stop advertising modern after daemon disable"
        );

        daemon_task.await.expect("daemon task");
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn reconnect_publishes_disabled_gate_before_new_session() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("reconnect-gate");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (first, _) = listener.accept().await.expect("accept first session");
            let (_first_reader, _first_writer, _) =
                fake_daemon_handshake_with_gate(first, "enabled-session", true).await;
            let (second, _) = listener.accept().await.expect("accept replacement session");
            let (_second_reader, _second_writer, _) =
                fake_daemon_handshake_with_gate(second, "disabled-session", false).await;
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "reconnect-gate-client".to_string(),
            None,
        )
        .await
        .expect("establish enabled session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();
        assert!(
            proxy
                .supported_protocol_versions()
                .contains(&ProtocolVersion::V_2026_07_28)
        );

        {
            let mut connection = proxy.shared.conn.lock().await;
            IpcProxyHandler::refresh_session(&proxy.shared, &mut connection)
                .await
                .expect("reconnect with disabled gate");
        }
        assert_eq!(
            proxy.supported_protocol_versions().as_ref(),
            &[plug_core::protocol::supported_protocol_version()],
            "gate must be disabled as soon as the replacement session is published"
        );

        daemon_task.await.expect("daemon task");
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(temp);
    }

    #[derive(Clone)]
    struct LoggingCaptureClient {
        notify: Arc<tokio::sync::Notify>,
        messages: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    impl ClientHandler for LoggingCaptureClient {
        fn get_info(&self) -> ClientInfo {
            ClientInfo::default()
                .with_protocol_version(plug_core::protocol::supported_protocol_version())
        }

        async fn on_logging_message(
            &self,
            params: LoggingMessageNotificationParam,
            _context: NotificationContext<rmcp::RoleClient>,
        ) {
            self.messages.lock().await.push(params.data.to_string());
            self.notify.notify_one();
        }
    }

    #[tokio::test]
    async fn reconnect_replays_client_capabilities() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("reconnect-caps");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        // A non-default capabilities value so the "replayed == negotiated"
        // assertion below can't trivially pass by matching two empty structs.
        let negotiated_capabilities = ClientCapabilities::builder()
            .enable_roots()
            .enable_roots_list_changed()
            .build();

        let listener = bind_fake_daemon_socket();
        let expected_capabilities = negotiated_capabilities.clone();
        let daemon_task = tokio::spawn(async move {
            // Connection 1: handshake only, then drop — simulates the daemon
            // restarting mid-session so the NEXT round trip must reconnect.
            let (stream1, _) = listener.accept().await.expect("accept 1");
            let (reader1, writer1, _seen1) = fake_daemon_handshake(stream1, "fake-session-1").await;
            drop(reader1);
            drop(writer1);

            // Connection 2: the reconnect handshake itself is still exactly
            // OperatorHandshake+Register+Capabilities...
            let (stream2, _) = listener.accept().await.expect("accept 2");
            let (mut reader2, mut writer2, seen2) =
                fake_daemon_handshake(stream2, "fake-session-2").await;
            assert_eq!(
                seen2,
                vec![
                    "OperatorHandshake".to_string(),
                    "Register".to_string(),
                    "Capabilities".to_string(),
                ],
                "reconnect handshake itself is still exactly OperatorHandshake+Register+Capabilities; \
                 capability replay happens as a SEPARATE round trip right after it"
            );

            // ...followed by a replay of the client capabilities negotiated
            // before the restart (plan 007) — assert it matches exactly.
            let frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read replayed capabilities frame")
                .expect("connection open");
            let req: IpcRequest =
                serde_json::from_slice(&frame).expect("parse replayed capabilities request");
            match req {
                IpcRequest::UpdateCapabilities { capabilities, .. } => {
                    assert_eq!(
                        *capabilities, expected_capabilities,
                        "replayed capabilities must match what was negotiated before reconnect"
                    );
                }
                other => panic!("expected replayed UpdateCapabilities, got {other:?}"),
            }
            ipc::send_response(&mut writer2, &IpcResponse::Ok)
                .await
                .expect("send replayed UpdateCapabilities ack");

            let frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read ping")
                .expect("connection open");
            let _req: IpcRequest = serde_json::from_slice(&frame).expect("parse ping");
            ipc::send_response(&mut writer2, &IpcResponse::Pong)
                .await
                .expect("send pong");
        });

        let session =
            crate::runtime::establish_daemon_proxy_session(None, "client-caps".to_string(), None)
                .await
                .expect("establish daemon proxy session");
        let initial_session_id = session.session_id.clone();
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        // Seed the replay state as if a downstream client had already
        // negotiated these capabilities via initialize() before the
        // restart. The full initialize()-driven capture path (production
        // code recording `ReplayState::client_capabilities`) is covered
        // end-to-end by `malformed_frame_is_reconnectable_failure`; this
        // test isolates the replay-on-reconnect behavior itself.
        proxy.shared.replay.lock().await.client_capabilities = Some(negotiated_capabilities);

        let response = proxy
            .session_round_trip(RetryPolicy::SafeToRetry, |session_id| IpcRequest::Ping {
                session_id: session_id.to_string(),
            })
            .await
            .expect("round trip should succeed after reconnect");
        assert!(matches!(response, IpcResponse::Pong));

        let final_session_id = { proxy.shared.conn.lock().await.session_id.clone() };
        assert_ne!(
            final_session_id, initial_session_id,
            "reconnect should replace the daemon session id"
        );

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn reconnect_replays_subscriptions() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("reconnect-subs");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            // Connection 1: handshake only, then drop — simulates the daemon
            // restarting mid-session so the NEXT round trip must reconnect.
            let (stream1, _) = listener.accept().await.expect("accept 1");
            let (reader1, writer1, _seen1) = fake_daemon_handshake(stream1, "fake-session-1").await;
            drop(reader1);
            drop(writer1);

            // Connection 2: the reconnect handshake, followed by a replay of
            // the subscription that was active before the restart.
            let (stream2, _) = listener.accept().await.expect("accept 2");
            let (mut reader2, mut writer2, _seen2) =
                fake_daemon_handshake(stream2, "fake-session-2").await;

            let frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read replayed subscribe frame")
                .expect("connection open");
            let req: IpcRequest =
                serde_json::from_slice(&frame).expect("parse replayed subscribe request");
            match req {
                IpcRequest::RestoreResourceSubscriptions { uris, .. } => {
                    assert_eq!(uris, vec!["test://resource".to_string()]);
                }
                other => panic!("expected RestoreResourceSubscriptions, got {other:?}"),
            }
            ipc::send_response(&mut writer2, &IpcResponse::Ok)
                .await
                .expect("send replayed subscribe ack");

            let frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read ping")
                .expect("connection open");
            let _req: IpcRequest = serde_json::from_slice(&frame).expect("parse ping");
            ipc::send_response(&mut writer2, &IpcResponse::Pong)
                .await
                .expect("send pong");
        });

        let session =
            crate::runtime::establish_daemon_proxy_session(None, "client-subs".to_string(), None)
                .await
                .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        proxy
            .shared
            .replay
            .lock()
            .await
            .subscriptions
            .insert("test://resource".to_string());

        let response = proxy
            .session_round_trip(RetryPolicy::SafeToRetry, |session_id| IpcRequest::Ping {
                session_id: session_id.to_string(),
            })
            .await
            .expect("round trip should succeed after reconnect");
        assert!(matches!(response, IpcResponse::Pong));

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn reconnect_replay_failure_does_not_fail_session() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("reconnect-replay-fail");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            // Connection 1: handshake only, then drop — simulates the daemon
            // restarting mid-session so the NEXT round trip must reconnect.
            let (stream1, _) = listener.accept().await.expect("accept 1");
            let (reader1, writer1, _seen1) = fake_daemon_handshake(stream1, "fake-session-1").await;
            drop(reader1);
            drop(writer1);

            // Connection 2: the reconnect handshake, then REJECT the
            // replayed subscribe outright.
            let (stream2, _) = listener.accept().await.expect("accept 2");
            let (mut reader2, mut writer2, _seen2) =
                fake_daemon_handshake(stream2, "fake-session-2").await;

            let frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read replayed subscribe frame")
                .expect("connection open");
            let req: IpcRequest =
                serde_json::from_slice(&frame).expect("parse replayed subscribe request");
            assert!(
                matches!(
                    req,
                    IpcRequest::RestoreResourceSubscriptions { ref uris, .. }
                        if uris == &["test://rejected".to_string()]
                ),
                "expected RestoreResourceSubscriptions, got {req:?}"
            );
            ipc::send_response(
                &mut writer2,
                &IpcResponse::Error {
                    code: "SUBSCRIBE_REPLAY_REJECTED".to_string(),
                    message: "simulated replay rejection".to_string(),
                },
            )
            .await
            .expect("send replay rejection");

            // The reconnect must still complete and the ORIGINAL request
            // must still be retried against the new session — a rejected
            // replay is warn-and-continue, not a reconnect failure.
            let frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read retried tools/list")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse retried tools/list");
            match req {
                IpcRequest::McpRequest { method, .. } => assert_eq!(method, "tools/list"),
                other => panic!("expected retried tools/list, got {other:?}"),
            }
            ipc::send_response(
                &mut writer2,
                &IpcResponse::McpResponse {
                    payload: serde_json::json!({ "tools": [] }),
                },
            )
            .await
            .expect("send tools/list response");
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-replay-fail".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        proxy
            .shared
            .replay
            .lock()
            .await
            .subscriptions
            .insert("test://rejected".to_string());

        let response = proxy
            .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                IpcRequest::McpRequest {
                    session_id: session_id.to_string(),
                    method: "tools/list".to_string(),
                    params: None,
                }
            })
            .await
            .expect("reconnect must succeed even though the replayed subscribe was rejected");
        assert!(matches!(response, IpcResponse::McpResponse { .. }));

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn unsubscribe_removes_from_replay_set() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("unsub-replay");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            // Connection 1: full initialize handshake — the downstream
            // client subscribes then unsubscribes on this connection.
            let (stream1, _) = listener.accept().await.expect("accept 1");
            let (mut reader1, mut writer1) =
                drive_fake_daemon_initialize(stream1, "fake-session-1").await;

            let frame = ipc::read_frame(&mut reader1)
                .await
                .expect("read subscribe frame")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse subscribe request");
            assert!(
                is_mcp_request(&req, "resources/subscribe"),
                "expected resources/subscribe, got {req:?}"
            );
            ipc::send_response(
                &mut writer1,
                &IpcResponse::McpResponse {
                    payload: serde_json::json!({}),
                },
            )
            .await
            .expect("send subscribe ack");

            let frame = ipc::read_frame(&mut reader1)
                .await
                .expect("read unsubscribe frame")
                .expect("connection open");
            let req: IpcRequest =
                serde_json::from_slice(&frame).expect("parse unsubscribe request");
            assert!(
                is_mcp_request(&req, "resources/unsubscribe"),
                "expected resources/unsubscribe, got {req:?}"
            );
            ipc::send_response(
                &mut writer1,
                &IpcResponse::McpResponse {
                    payload: serde_json::json!({}),
                },
            )
            .await
            .expect("send unsubscribe ack");

            // Simulate the daemon restarting mid-session.
            drop(reader1);
            drop(writer1);

            // Connection 2: the reconnect. Client capabilities negotiated
            // during initialize() are replayed (see
            // reconnect_replays_client_capabilities) — but the unsubscribed
            // URI must NOT be re-subscribed. The very next frame after the
            // capability replay must be the retried tools/list, not a
            // resources/subscribe.
            let (stream2, _) = listener.accept().await.expect("accept 2");
            let (mut reader2, mut writer2, _seen2) =
                fake_daemon_handshake(stream2, "fake-session-2").await;

            let frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read replayed capabilities frame")
                .expect("connection open");
            let req: IpcRequest =
                serde_json::from_slice(&frame).expect("parse replayed capabilities request");
            assert!(
                matches!(req, IpcRequest::UpdateCapabilities { .. }),
                "expected replayed UpdateCapabilities, got {req:?}"
            );
            ipc::send_response(&mut writer2, &IpcResponse::Ok)
                .await
                .expect("send replayed UpdateCapabilities ack");

            let frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read retried tools/list")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse retried tools/list");
            match req {
                IpcRequest::McpRequest { method, .. }
                | IpcRequest::McpRequestWithContext { method, .. } => assert_eq!(
                    method, "tools/list",
                    "unsubscribed URI must NOT be re-subscribed on reconnect"
                ),
                other => panic!(
                    "unsubscribed URI must NOT be re-subscribed on reconnect; expected retried \
                     tools/list, got {other:?}"
                ),
            }
            ipc::send_response(
                &mut writer2,
                &IpcResponse::McpResponse {
                    payload: serde_json::json!({ "tools": [] }),
                },
            )
            .await
            .expect("send tools/list response");
        });

        let session =
            crate::runtime::establish_daemon_proxy_session(None, "client-unsub".to_string(), None)
                .await
                .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        let resource_uri = "test://unsub-resource";
        tokio::time::timeout(
            Duration::from_secs(5),
            client
                .peer()
                .subscribe(SubscribeRequestParams::new(resource_uri)),
        )
        .await
        .expect("subscribe timeout")
        .expect("subscribe");

        tokio::time::timeout(
            Duration::from_secs(5),
            client
                .peer()
                .unsubscribe(UnsubscribeRequestParams::new(resource_uri)),
        )
        .await
        .expect("unsubscribe timeout")
        .expect("unsubscribe");

        // Trigger the reconnect (connection 1 was dropped by the daemon
        // task above) and drive it to completion.
        let _tools = tokio::time::timeout(Duration::from_secs(5), client.peer().list_all_tools())
            .await
            .expect("list_all_tools timeout")
            .expect("list_all_tools should succeed after reconnect");

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn retry_policy_safe_rebuilds_against_new_session() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("retry-safe");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream1, _) = listener.accept().await.expect("accept 1");
            let (reader1, writer1, _seen1) = fake_daemon_handshake(stream1, "fake-session-1").await;
            drop(reader1);
            drop(writer1); // force the first Ping attempt to fail (reconnectable)

            let (stream2, _) = listener.accept().await.expect("accept 2");
            let (mut reader2, mut writer2, _seen2) =
                fake_daemon_handshake(stream2, "fake-session-2").await;

            let frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read retried ping")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse retried ping");
            match req {
                IpcRequest::Ping { session_id } => assert_eq!(
                    session_id, "fake-session-2",
                    "SafeToRetry must rebuild the retried request against the NEW session id"
                ),
                other => panic!("expected retried Ping, got {other:?}"),
            }
            ipc::send_response(&mut writer2, &IpcResponse::Pong)
                .await
                .expect("send pong");
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-retry-safe".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let response = proxy
            .session_round_trip(RetryPolicy::SafeToRetry, |session_id| IpcRequest::Ping {
                session_id: session_id.to_string(),
            })
            .await
            .expect("safe retry should succeed after reconnect");
        assert!(matches!(response, IpcResponse::Pong));

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn retry_policy_unsafe_surfaces_retry_error() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("retry-unsafe");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream1, _) = listener.accept().await.expect("accept 1");
            let (reader1, writer1, _seen1) = fake_daemon_handshake(stream1, "fake-session-1").await;
            drop(reader1);
            drop(writer1);

            // Reconnect handshake happens even though the original request
            // is never retried under UnsafeToRetry.
            let (stream2, _) = listener.accept().await.expect("accept 2");
            let (reader2, writer2, _seen2) = fake_daemon_handshake(stream2, "fake-session-2").await;
            drop(reader2);
            drop(writer2);
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-retry-unsafe".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let initial_session_id = { proxy.shared.conn.lock().await.session_id.clone() };

        let error = proxy
            .session_round_trip(RetryPolicy::UnsafeToRetry, |session_id| IpcRequest::Ping {
                session_id: session_id.to_string(),
            })
            .await
            .expect_err("UnsafeToRetry must surface an error, not silently retry");
        assert!(
            error.message.contains("REQUEST_RETRY_UNSAFE"),
            "unexpected error message: {}",
            error.message
        );

        let final_session_id = { proxy.shared.conn.lock().await.session_id.clone() };
        assert_ne!(
            final_session_id, initial_session_id,
            "reconnect should still happen under UnsafeToRetry — only the retried send is skipped"
        );

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn notifications_interleaved_before_response_are_forwarded() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("interleave");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, mut writer) =
                drive_fake_daemon_initialize(stream, "fake-session-1").await;

            let frame = ipc::read_frame(&mut reader)
                .await
                .expect("read tools/call frame")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse tools/call request");
            assert!(
                is_mcp_request(&req, "tools/call"),
                "expected tools/call, got {req:?}"
            );

            // Interleave a push notification BEFORE the actual response,
            // exactly as the real daemon does when an upstream server logs
            // mid-call (plug/src/daemon.rs sends LoggingNotification via
            // plain ipc::send_response, never enveloped — see the "Plain
            // IpcResponse" branch of decode_daemon_frame).
            let notif_params = serde_json::to_value(LoggingMessageNotificationParam::new(
                LoggingLevel::Info,
                serde_json::json!("hello from daemon"),
            ))
            .expect("serialize logging params");
            ipc::send_response(
                &mut writer,
                &IpcResponse::LoggingNotification {
                    params: notif_params,
                },
            )
            .await
            .expect("send interleaved notification");

            let call_result =
                serde_json::to_value(CallToolResult::success(vec![ContentBlock::text("ok")]))
                    .expect("serialize call result");
            ipc::send_response(
                &mut writer,
                &IpcResponse::McpResponse {
                    payload: call_result,
                },
            )
            .await
            .expect("send tools/call response");
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-interleave".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let notify = Arc::new(tokio::sync::Notify::new());
        let messages = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let client = LoggingCaptureClient {
            notify: notify.clone(),
            messages: messages.clone(),
        }
        .serve(client_transport)
        .await
        .expect("connect downstream client");

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool(CallToolRequestParams::new("whatever")),
        )
        .await
        .expect("call timeout")
        .expect("call should succeed despite the interleaved notification");
        assert!(!result.content.is_empty());

        tokio::time::timeout(Duration::from_secs(5), notify.notified())
            .await
            .expect("expected the interleaved notification to reach the downstream peer");
        let captured = messages.lock().await.clone();
        assert_eq!(captured.len(), 1);
        assert!(captured[0].contains("hello from daemon"));

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn chunked_response_reassembly() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("chunked");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, mut writer) =
                drive_fake_daemon_initialize(stream, "fake-session-1").await;

            let frame = ipc::read_frame(&mut reader)
                .await
                .expect("read tools/call frame")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse tools/call request");
            assert!(
                is_mcp_request(&req, "tools/call"),
                "expected tools/call, got {req:?}"
            );

            // > MAX_FRAME_SIZE (4 MiB) so plug_core::ipc::send_chunked_response
            // — the SAME helper the real daemon uses — must split it into
            // multiple ResponseChunk envelopes for decode_daemon_frame to
            // reassemble.
            let big_text = "x".repeat(6 * 1024 * 1024);
            let call_result =
                serde_json::to_value(CallToolResult::success(vec![ContentBlock::text(big_text)]))
                    .expect("serialize call result");
            ipc::send_chunked_response(
                &mut writer,
                &IpcResponse::McpResponse {
                    payload: call_result,
                },
            )
            .await
            .expect("send chunked response");
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-chunked".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            client.call_tool(CallToolRequestParams::new("whatever")),
        )
        .await
        .expect("chunked call timeout")
        .expect("chunked call should succeed");
        let text = result
            .content
            .first()
            .and_then(ContentBlock::as_text)
            .expect("text content");
        assert_eq!(text.text.len(), 6 * 1024 * 1024);

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn malformed_frame_is_reconnectable_failure() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("malformed");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream1, _) = listener.accept().await.expect("accept 1");
            let (mut reader1, mut writer1) =
                drive_fake_daemon_initialize(stream1, "fake-session-1").await;

            let frame = ipc::read_frame(&mut reader1)
                .await
                .expect("read tools/call frame")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse tools/call request");
            assert!(
                is_mcp_request(&req, "tools/call"),
                "expected tools/call, got {req:?}"
            );

            // Write a frame length prefix promising a body, then close
            // before sending it — read_frame's read_exact hits
            // UnexpectedEof mid-body, which transport_failure() classifies
            // reconnectable=true. NOTE: this is the ONE "malformed frame"
            // flavor the current code treats as reconnectable. A
            // syntactically-complete frame containing garbage JSON, or a
            // length prefix over MAX_FRAME_SIZE, are BOTH classified
            // reconnectable=false today (a parse error / anyhow::bail, not
            // a std::io::Error) and do NOT auto-recover — see
            // decode_daemon_frame's parse-error arms, which always set
            // `reconnectable: false`.
            writer1
                .write_u32(64)
                .await
                .expect("write bogus length prefix");
            writer1.flush().await.expect("flush bogus length prefix");
            drop(reader1);
            drop(writer1);

            // The reconnect after the transport failure only re-sends
            // OperatorHandshake + Register + Capabilities (see
            // reconnect_replays_client_capabilities) — not the full
            // initialize() sequence, which only runs once per downstream
            // client lifetime. It IS followed by a replay of the client
            // capabilities negotiated during connection 1's initialize
            // (plan 007) — consume and ack that before the retried
            // tools/call.
            let (stream2, _) = listener.accept().await.expect("accept 2");
            let (mut reader2, mut writer2, _seen2) =
                fake_daemon_handshake(stream2, "fake-session-2").await;

            let replay_frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read replayed capabilities frame")
                .expect("connection open");
            let replay_req: IpcRequest =
                serde_json::from_slice(&replay_frame).expect("parse replayed capabilities request");
            assert!(
                matches!(replay_req, IpcRequest::UpdateCapabilities { .. }),
                "expected replayed UpdateCapabilities before the retried tools/call, got {replay_req:?}"
            );
            ipc::send_response(&mut writer2, &IpcResponse::Ok)
                .await
                .expect("send replayed UpdateCapabilities ack");

            let frame2 = ipc::read_frame(&mut reader2)
                .await
                .expect("read second tools/call frame")
                .expect("connection open");
            let req2: IpcRequest =
                serde_json::from_slice(&frame2).expect("parse second tools/call request");
            assert!(
                is_mcp_request(&req2, "tools/call"),
                "expected retried tools/call, got {req2:?}"
            );
            let call_result =
                serde_json::to_value(CallToolResult::success(vec![ContentBlock::text(
                    "recovered",
                )]))
                .expect("serialize call result");
            ipc::send_response(
                &mut writer2,
                &IpcResponse::McpResponse {
                    payload: call_result,
                },
            )
            .await
            .expect("send second tools/call response");
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-malformed".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        // call_tool() uses RetryPolicy::UnsafeToRetry: the malformed-frame
        // failure triggers a reconnect, but the ORIGINAL request is not
        // resent — the call surfaces REQUEST_RETRY_UNSAFE instead. Confirms
        // "not a hang, not a panic": this returns promptly with an error.
        let first_attempt = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool(CallToolRequestParams::new("whatever")),
        )
        .await
        .expect("first call timed out — would indicate a hang, not a surfaced error");
        assert!(
            first_attempt.is_err(),
            "expected the first call to surface an error after the malformed frame, not silently succeed"
        );

        // The proxy has now reconnected (session 2); the next call succeeds.
        let second_attempt = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool(CallToolRequestParams::new("whatever")),
        )
        .await
        .expect("second call timeout")
        .expect("second call should succeed on the reconnected session");
        assert_eq!(
            second_attempt
                .content
                .first()
                .and_then(ContentBlock::as_text)
                .map(|t| t.text.as_str()),
            Some("recovered")
        );

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// A connector paused across a daemon swap resumes with the old daemon's
    /// last reply unread and a heartbeat due at once. The heartbeat's write
    /// fails first; the reply already in the socket must still reach its
    /// caller.
    #[tokio::test]
    async fn a_failed_write_still_delivers_replies_already_sent() {
        let (client, daemon) = UnixStream::pair().expect("socket pair");
        let (reader, writer) = client.into_split();
        let mux = DaemonMux::start(
            reader,
            writer,
            true,
            Duration::from_secs(10),
            std::sync::Weak::new(),
        );
        let call = tokio::spawn({
            let mux = Arc::clone(&mux);
            async move { mux.round_trip(&IpcRequest::Status).await }
        });

        let (mut daemon_reader, mut daemon_writer) = daemon.into_split();
        let frame = ipc::read_frame(&mut daemon_reader)
            .await
            .expect("read request")
            .expect("connection open");
        let ipc_id = ipc::FrameIds::peek(&frame).ipc_id;
        mux.writes_failed(&TransportFailure {
            message: "IPC write failed: broken pipe".to_string(),
            reconnectable: true,
        });
        ipc::send_chunked_response_with_id(&mut daemon_writer, ipc_id, &IpcResponse::Pong)
            .await
            .expect("send reply");
        drop((daemon_reader, daemon_writer));

        let reply = tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .expect("call finished")
            .expect("call task");
        assert!(matches!(reply, Ok(IpcResponse::Pong)), "{reply:?}");
        let later = mux.round_trip(&IpcRequest::Status).await;
        assert!(
            matches!(
                later,
                Err(TransportFailure {
                    reconnectable: true,
                    ..
                })
            ),
            "a request after the failed write must reconnect"
        );
    }

    #[tokio::test]
    async fn silent_daemon_stall_triggers_reconnect() {
        let _guard = daemon_test_lock().lock().await;
        // Real Unix socket reads don't complete under tokio::time::pause, so
        // (per plans/009) use the test-only override instead of paused time.
        let _watchdog = ReadWatchdogTestOverride::install(Duration::from_millis(150));
        let temp = unique_temp_dir("stall");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream1, _) = listener.accept().await.expect("accept 1");
            let (mut reader1, _writer1) =
                drive_fake_daemon_initialize(stream1, "fake-session-1").await;

            let frame = ipc::read_frame(&mut reader1)
                .await
                .expect("read tools/call frame")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse tools/call request");
            assert!(
                is_mcp_request(&req, "tools/call"),
                "expected tools/call, got {req:?}"
            );

            // Accept the request and never respond — e.g. a wedged upstream
            // server the daemon is still waiting on. Deliberately do NOT
            // close this connection (that would just retrigger the
            // pre-existing "daemon closed connection" EOF path exercised by
            // other tests, not the read watchdog this test targets): leave
            // reader1/_writer1 open and silent for the rest of this task, so
            // the client can only recover via watchdog expiry.
            let (stream2, _) = listener.accept().await.expect("accept 2");
            let (mut reader2, mut writer2, _seen2) =
                fake_daemon_handshake(stream2, "fake-session-2").await;

            // The reconnect after watchdog expiry only re-sends Register +
            // Capabilities (see reconnect_replays_client_capabilities) — not
            // the full initialize() sequence, which only runs once per
            // downstream client lifetime. It IS followed by a replay of the
            // client capabilities negotiated during connection 1's
            // initialize (plan 007) — consume and ack that before the
            // retried tools/call.
            let replay_frame = ipc::read_frame(&mut reader2)
                .await
                .expect("read replayed capabilities frame")
                .expect("connection open");
            let replay_req: IpcRequest =
                serde_json::from_slice(&replay_frame).expect("parse replayed capabilities request");
            assert!(
                matches!(replay_req, IpcRequest::UpdateCapabilities { .. }),
                "expected replayed UpdateCapabilities before the retried tools/call, got {replay_req:?}"
            );
            ipc::send_response(&mut writer2, &IpcResponse::Ok)
                .await
                .expect("send replayed UpdateCapabilities ack");

            let frame2 = ipc::read_frame(&mut reader2)
                .await
                .expect("read second tools/call frame")
                .expect("connection open");
            let req2: IpcRequest =
                serde_json::from_slice(&frame2).expect("parse second tools/call request");
            assert!(
                is_mcp_request(&req2, "tools/call"),
                "expected retried tools/call, got {req2:?}"
            );
            let call_result =
                serde_json::to_value(CallToolResult::success(vec![ContentBlock::text(
                    "recovered",
                )]))
                .expect("serialize call result");
            ipc::send_response(
                &mut writer2,
                &IpcResponse::McpResponse {
                    payload: call_result,
                },
            )
            .await
            .expect("send second tools/call response");

            drop(reader1);
        });

        let session =
            crate::runtime::establish_daemon_proxy_session(None, "client-stall".to_string(), None)
                .await
                .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        // call_tool() uses RetryPolicy::UnsafeToRetry: the watchdog-expiry
        // failure triggers a reconnect, but the ORIGINAL request is not
        // resent — the call surfaces REQUEST_RETRY_UNSAFE instead. Confirms
        // "not a hang, not a panic": this returns promptly (well within 5s,
        // via the 150ms test-shortened watchdog) with an error, whereas
        // before plan 009 the same scenario hung until this outer timeout.
        let first_attempt = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool(CallToolRequestParams::new("whatever")),
        )
        .await
        .expect(
            "first call timed out — the read watchdog should force a reconnect well within 5s, \
             not hang indefinitely",
        );
        assert!(
            first_attempt.is_err(),
            "expected the first call to surface an error after the watchdog-triggered reconnect, \
             not silently succeed"
        );

        // The proxy has now reconnected (session 2); the next call succeeds.
        let second_attempt = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool(CallToolRequestParams::new("whatever")),
        )
        .await
        .expect("second call timeout")
        .expect("second call should succeed on the reconnected session");
        assert_eq!(
            second_attempt
                .content
                .first()
                .and_then(ContentBlock::as_text)
                .map(|t| t.text.as_str()),
            Some("recovered")
        );

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn watchdog_resets_on_interleaved_frames() {
        let _guard = daemon_test_lock().lock().await;
        let _watchdog = ReadWatchdogTestOverride::install(Duration::from_millis(150));
        let temp = unique_temp_dir("watchdog-reset");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, mut writer) =
                drive_fake_daemon_initialize(stream, "fake-session-1").await;

            let frame = ipc::read_frame(&mut reader)
                .await
                .expect("read tools/call frame")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse tools/call request");
            assert!(
                is_mcp_request(&req, "tools/call"),
                "expected tools/call, got {req:?}"
            );

            // Send a notification every 40ms — comfortably below the 150ms
            // test watchdog — for 6 rounds (240ms total), THEN the actual
            // response. Total elapsed (240ms) exceeds the watchdog, but no
            // single silent gap does, because each arriving frame resets the
            // per-read clock. If the watchdog were per-request-total instead
            // of per-frame-inactivity, this round trip would incorrectly
            // fail.
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(40)).await;
                ipc::send_response(&mut writer, &IpcResponse::ToolListChangedNotification)
                    .await
                    .expect("send interleaved keep-alive notification");
            }

            let call_result =
                serde_json::to_value(CallToolResult::success(vec![ContentBlock::text(
                    "survived",
                )]))
                .expect("serialize call result");
            ipc::send_response(
                &mut writer,
                &IpcResponse::McpResponse {
                    payload: call_result,
                },
            )
            .await
            .expect("send tools/call response");
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-watchdog-reset".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        // If the watchdog fired incorrectly, this would surface
        // REQUEST_RETRY_UNSAFE (tools/call is UnsafeToRetry) instead of the
        // real "survived" response — so a successful match here proves the
        // per-frame reset, not just "did not hang".
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool(CallToolRequestParams::new("whatever")),
        )
        .await
        .expect("call timeout")
        .expect(
            "call should succeed: total elapsed exceeds the watchdog only via interleaved \
             frames, each of which resets the clock",
        );
        assert_eq!(
            result
                .content
                .first()
                .and_then(ContentBlock::as_text)
                .map(|t| t.text.as_str()),
            Some("survived")
        );

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// Start a proxy against a fake daemon that answers one `expected_method`
    /// request with `payload`, and connect a downstream client to it. Callers
    /// hold `daemon_test_lock` and own the temp dir.
    async fn proxy_with_one_scripted_reply(
        client_id: &str,
        expected_method: &'static str,
        payload: serde_json::Value,
    ) -> (
        rmcp::service::RunningService<rmcp::RoleClient, TestClient>,
        JoinHandle<()>,
    ) {
        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, mut writer) =
                drive_fake_daemon_initialize(stream, "fake-session-1").await;
            let frame = ipc::read_frame(&mut reader)
                .await
                .expect("read request frame")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse request");
            assert!(
                is_mcp_request(&req, expected_method),
                "expected {expected_method}, got {req:?}"
            );
            ipc::send_response(&mut writer, &IpcResponse::McpResponse { payload })
                .await
                .expect("send scripted reply");
        });

        let session =
            crate::runtime::establish_daemon_proxy_session(None, client_id.to_string(), None)
                .await
                .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });
        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");
        (client, daemon_task)
    }

    #[tokio::test]
    async fn tool_result_with_an_envelope_key_is_not_mistaken_for_an_envelope() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("envelope-key");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let payload = serde_json::to_value(CallToolResult::structured(
            serde_json::json!({ "envelope": "sealed" }),
        ))
        .expect("serialize call result");
        let (client, daemon_task) =
            proxy_with_one_scripted_reply("client-envelope-key", "tools/call", payload).await;

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool(CallToolRequestParams::new("whatever")),
        )
        .await
        .expect("call timeout")
        .expect("a result carrying an `envelope` key must reach the client intact");
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({ "envelope": "sealed" }))
        );

        daemon_task.await.expect("daemon task join");
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn upstream_error_for_resources_read_reaches_the_client_as_that_error() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("read-error");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let payload = serde_json::to_value(McpError::resource_not_found(
            "no such resource: file:///missing",
            None,
        ))
        .expect("serialize error");
        let (client, daemon_task) =
            proxy_with_one_scripted_reply("client-read-error", "resources/read", payload).await;

        let error = tokio::time::timeout(
            Duration::from_secs(5),
            client.read_resource(ReadResourceRequestParams::new("file:///missing")),
        )
        .await
        .expect("read timeout")
        .expect_err("the upstream error must surface as an error");
        let rmcp::ServiceError::McpError(error) = error else {
            panic!("expected an MCP error, got {error:?}");
        };
        assert_eq!(error.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert!(error.message.contains("no such resource"), "{error:?}");

        daemon_task.await.expect("daemon task join");
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn update_roots_consumes_its_reply_past_a_gate_push() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("roots-gate");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, mut writer, _seen) =
                fake_daemon_handshake(stream, "fake-session-1").await;

            let frame = ipc::read_frame(&mut reader)
                .await
                .expect("read UpdateRoots")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse UpdateRoots");
            assert!(
                matches!(req, IpcRequest::UpdateRoots { .. }),
                "expected UpdateRoots, got {req:?}"
            );
            // The daemon may push a gate change while a request is in flight.
            ipc::send_response(
                &mut writer,
                &IpcResponse::ModernDownstreamGateChanged { enabled: true },
            )
            .await
            .expect("send gate push");
            ipc::send_response(&mut writer, &IpcResponse::Ok)
                .await
                .expect("send UpdateRoots ack");

            let frame = ipc::read_frame(&mut reader)
                .await
                .expect("read Ping")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse Ping");
            assert!(matches!(req, IpcRequest::Ping { .. }), "got {req:?}");
            ipc::send_response(&mut writer, &IpcResponse::Pong)
                .await
                .expect("send pong");
        });

        let session =
            crate::runtime::establish_daemon_proxy_session(None, "client-roots".to_string(), None)
                .await
                .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        push_roots_to_daemon(&proxy.shared, serde_json::json!([])).await;
        assert!(
            proxy
                .shared
                .modern_downstream_enabled
                .load(std::sync::atomic::Ordering::Acquire),
            "the gate push must be applied, not treated as the UpdateRoots reply"
        );
        let response = proxy
            .session_round_trip(RetryPolicy::SafeToRetry, |session_id| IpcRequest::Ping {
                session_id: session_id.to_string(),
            })
            .await
            .expect("ping after UpdateRoots");
        assert!(
            matches!(response, IpcResponse::Pong),
            "the next call must read its own reply, got {response:?}"
        );

        daemon_task.await.expect("daemon task join");
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn a_reconnect_exits_only_when_a_respawn_would_land_on_the_new_binary() {
        let upgraded = anyhow::Error::new(crate::runtime::DaemonVersionMismatch {
            daemon_version: "9.9.9".to_string(),
            client_version: env!("CARGO_PKG_VERSION"),
            respawn_resolves: true,
        });
        assert_eq!(
            reconnect_recovery(&upgraded),
            ReconnectRecovery::RespawnClient
        );

        // A client built against a different install gains nothing from a
        // restart, so it must report instead of exiting into a loop.
        let skewed = anyhow::Error::new(crate::runtime::DaemonVersionMismatch {
            daemon_version: "9.9.9".to_string(),
            client_version: env!("CARGO_PKG_VERSION"),
            respawn_resolves: false,
        });
        assert_eq!(reconnect_recovery(&skewed), ReconnectRecovery::ReportError);

        let unrelated = anyhow::anyhow!("socket closed");
        assert_eq!(
            reconnect_recovery(&unrelated),
            ReconnectRecovery::ReportError
        );
    }

    fn tool_call_name(request: &IpcRequest) -> String {
        match request {
            IpcRequest::McpRequestWithContext {
                params: Some(params),
                ..
            } => params["name"].as_str().unwrap_or_default().to_string(),
            other => panic!("expected a tools/call request, got {other:?}"),
        }
    }

    fn text_result(text: &str) -> IpcResponse {
        IpcResponse::McpResponse {
            payload: serde_json::to_value(CallToolResult::success(vec![ContentBlock::text(
                text.to_string(),
            )]))
            .expect("serialize call result"),
        }
    }

    async fn send_tagged(writer: &mut OwnedWriteHalf, ipc_id: u64, response: &IpcResponse) {
        let payload = ipc::encode_tagged(Some(ipc_id), response).expect("encode tagged reply");
        ipc::write_frame(writer, &payload)
            .await
            .expect("send tagged reply");
    }

    /// Read `count` tools/call requests and return their ipc ids by tool name.
    async fn read_tagged_calls(reader: &mut OwnedReadHalf, count: usize) -> HashMap<String, u64> {
        let mut ids = HashMap::new();
        while ids.len() < count {
            let frame = ipc::read_frame(reader)
                .await
                .expect("read request")
                .expect("connection open");
            let request: IpcRequest = serde_json::from_slice(&frame).expect("parse request");
            let ipc_id = ipc::FrameIds::peek(&frame)
                .ipc_id
                .expect("every proxy request carries an ipc_id");
            ids.insert(tool_call_name(&request), ipc_id);
        }
        ids
    }

    fn call_text(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(ContentBlock::as_text)
            .expect("text content")
            .text
            .clone()
    }

    fn spawn_tool_call(
        proxy: &Arc<IpcProxyHandler>,
        name: &'static str,
    ) -> JoinHandle<Result<IpcResponse, McpError>> {
        let proxy = Arc::clone(proxy);
        tokio::spawn(async move {
            proxy
                .session_round_trip(RetryPolicy::UnsafeToRetry, |session_id| {
                    IpcRequest::McpRequestWithContext {
                        session_id: session_id.to_string(),
                        method: "tools/call".to_string(),
                        params: Some(serde_json::json!({ "name": name })),
                        context: ipc::IpcMcpRequestContext {
                            request_id: RequestId::Number(1),
                            protocol_version: plug_core::protocol::supported_protocol_version()
                                .to_string(),
                            client_name: None,
                            client_version: None,
                        },
                    }
                })
                .await
        })
    }

    #[tokio::test]
    async fn concurrent_calls_complete_out_of_order_and_notifications_still_arrive() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("concurrent-calls");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let (release_slow, slow_released) = tokio::sync::oneshot::channel::<()>();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, mut writer) =
                drive_fake_daemon_initialize(stream, "fake-session-1").await;

            // Both calls are in flight at once; the slow one arrived first.
            let ids = read_tagged_calls(&mut reader, 2).await;
            send_tagged(&mut writer, ids["fast"], &text_result("fast")).await;
            let notification = serde_json::to_value(LoggingMessageNotificationParam::new(
                LoggingLevel::Info,
                serde_json::json!("between replies"),
            ))
            .expect("serialize logging params");
            ipc::send_response(
                &mut writer,
                &IpcResponse::LoggingNotification {
                    params: notification,
                },
            )
            .await
            .expect("send notification");
            slow_released.await.expect("release slow reply");
            send_tagged(&mut writer, ids["slow"], &text_result("slow")).await;
            // Keep the connection open until the proxy is done with it.
            let _ = ipc::read_frame(&mut reader).await;
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-concurrent".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, None);
        proxy.heartbeat.abort();

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let notify = Arc::new(tokio::sync::Notify::new());
        let messages = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let client = LoggingCaptureClient {
            notify: notify.clone(),
            messages: messages.clone(),
        }
        .serve(client_transport)
        .await
        .expect("connect downstream client");

        let slow_peer = client.peer().clone();
        let slow = tokio::spawn(async move {
            slow_peer
                .call_tool(CallToolRequestParams::new("slow"))
                .await
        });
        // Make sure the slow call is written first.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let fast = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool(CallToolRequestParams::new("fast")),
        )
        .await
        .expect("the fast call must not wait behind the slow one")
        .expect("fast call succeeds");
        assert_eq!(call_text(&fast), "fast");
        assert!(!slow.is_finished(), "the slow call has no reply yet");

        tokio::time::timeout(Duration::from_secs(5), notify.notified())
            .await
            .expect("a notification between replies reaches the downstream peer");
        assert!(messages.lock().await[0].contains("between replies"));

        release_slow.send(()).expect("daemon waiting");
        let slow = tokio::time::timeout(Duration::from_secs(5), slow)
            .await
            .expect("slow call timeout")
            .expect("slow task join")
            .expect("slow call succeeds");
        assert_eq!(call_text(&slow), "slow");

        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), daemon_task).await;
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn chunked_reply_and_small_reply_route_to_their_own_requests() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("concurrent-chunked");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, mut writer, _seen) =
                fake_daemon_handshake(stream, "fake-session-1").await;

            let ids = read_tagged_calls(&mut reader, 2).await;
            // The big reply goes out first, over MAX_FRAME_SIZE, so it is
            // chunked; the small reply follows it.
            let big = "x".repeat(6 * 1024 * 1024);
            ipc::send_chunked_response_with_id(&mut writer, Some(ids["big"]), &text_result(&big))
                .await
                .expect("send chunked reply");
            send_tagged(&mut writer, ids["small"], &text_result("small")).await;
            let _ = ipc::read_frame(&mut reader).await;
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-concurrent-chunked".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = Arc::new(IpcProxyHandler::new(session, None));
        proxy.heartbeat.abort();

        let small = spawn_tool_call(&proxy, "small");
        let big = spawn_tool_call(&proxy, "big");

        let text = |response: IpcResponse| match response {
            IpcResponse::McpResponse { payload } => {
                call_text(&serde_json::from_value::<CallToolResult>(payload).expect("call result"))
            }
            other => panic!("unexpected response {other:?}"),
        };
        let small = tokio::time::timeout(Duration::from_secs(10), small)
            .await
            .expect("small timeout")
            .expect("join")
            .expect("small call");
        let big = tokio::time::timeout(Duration::from_secs(10), big)
            .await
            .expect("big timeout")
            .expect("join")
            .expect("big call");
        assert_eq!(text(small), "small");
        assert_eq!(text(big).len(), 6 * 1024 * 1024);

        drop(proxy);
        let _ = tokio::time::timeout(Duration::from_secs(5), daemon_task).await;
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn connection_drop_resolves_every_in_flight_request_with_one_reconnect() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("concurrent-drop");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept 1");
            let (mut reader, writer, _seen) = fake_daemon_handshake(stream, "fake-session-1").await;
            // Three requests in flight, then the connection dies.
            read_tagged_calls(&mut reader, 3).await;
            drop(reader);
            drop(writer);

            let (stream, _) = listener.accept().await.expect("accept 2");
            let (mut reader, _writer, _seen) =
                fake_daemon_handshake(stream, "fake-session-2").await;
            // One reconnect serves all three waiters: no third connection.
            assert!(
                tokio::time::timeout(Duration::from_millis(500), listener.accept())
                    .await
                    .is_err(),
                "each failed request reconnected on its own"
            );
            let _ = ipc::read_frame(&mut reader).await;
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-concurrent-drop".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = Arc::new(IpcProxyHandler::new(session, None));
        proxy.heartbeat.abort();

        let calls = [
            spawn_tool_call(&proxy, "a"),
            spawn_tool_call(&proxy, "b"),
            spawn_tool_call(&proxy, "c"),
        ];
        for call in calls {
            let error = tokio::time::timeout(Duration::from_secs(10), call)
                .await
                .expect("an in-flight request hung after the connection dropped")
                .expect("join")
                .expect_err("the connection died under the request");
            assert!(
                error.message.contains("REQUEST_RETRY_UNSAFE"),
                "unexpected error: {}",
                error.message
            );
        }
        assert_eq!(proxy.shared.conn.lock().await.session_id, "fake-session-2");

        drop(proxy);
        tokio::time::timeout(Duration::from_secs(5), daemon_task)
            .await
            .expect("daemon task timeout")
            .expect("daemon task join");
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// A daemon that predates v4 rejects a v4 registration. The proxy falls
    /// back to v3 and then sends untagged requests one at a time.
    #[tokio::test]
    async fn proxy_falls_back_to_serial_requests_on_a_v3_daemon() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("v3-daemon");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, mut writer) = stream.into_split();
            answer_operator_handshake(&mut reader, &mut writer).await;

            async fn read_request(reader: &mut OwnedReadHalf) -> IpcRequest {
                let frame = ipc::read_frame(reader)
                    .await
                    .expect("read request")
                    .expect("connection open");
                assert_eq!(
                    ipc::FrameIds::peek(&frame),
                    ipc::FrameIds::default(),
                    "a v3 session never tags requests"
                );
                serde_json::from_slice::<IpcRequest>(&frame).expect("parse request")
            }

            let mut versions = Vec::new();
            loop {
                let IpcRequest::Register {
                    protocol_version,
                    client_id,
                    ..
                } = read_request(&mut reader).await
                else {
                    panic!("expected Register");
                };
                versions.push(protocol_version);
                if protocol_version != 3 {
                    ipc::send_response(
                        &mut writer,
                        &IpcResponse::Error {
                            code: "PROTOCOL_VERSION_UNSUPPORTED".to_string(),
                            message: "daemon supports IPC protocol v3".to_string(),
                        },
                    )
                    .await
                    .expect("reject v4");
                    continue;
                }
                ipc::send_response(
                    &mut writer,
                    &IpcResponse::Registered {
                        protocol_version,
                        client_id,
                        session_id: "v3-session".to_string(),
                        modern_downstream_enabled: false,
                        cancellation_capability: ipc::IpcCancellationCapability::new(
                            "fake-cancellation-capability".to_string(),
                        ),
                    },
                )
                .await
                .expect("send Registered");
                break;
            }
            assert_eq!(versions, vec![ipc::IPC_PROTOCOL_VERSION, 3]);

            assert!(matches!(
                read_request(&mut reader).await,
                IpcRequest::Capabilities { .. }
            ));
            ipc::send_response(
                &mut writer,
                &IpcResponse::Capabilities {
                    capabilities: serde_json::to_value(ServerCapabilities::default())
                        .expect("serialize capabilities"),
                },
            )
            .await
            .expect("send Capabilities");

            // Two calls are pending, but only one is on the wire until its
            // reply is written.
            for _ in 0..2 {
                let name = tool_call_name(&read_request(&mut reader).await);
                assert!(
                    tokio::time::timeout(Duration::from_millis(200), ipc::read_frame(&mut reader))
                        .await
                        .is_err(),
                    "a second request was written before the first reply"
                );
                ipc::send_response(&mut writer, &text_result(&name))
                    .await
                    .expect("send reply");
            }
            let _ = ipc::read_frame(&mut reader).await;
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-v3-daemon".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        assert_eq!(session.ipc_protocol_version, 3);
        let proxy = Arc::new(IpcProxyHandler::new(session, None));
        proxy.heartbeat.abort();

        let first = spawn_tool_call(&proxy, "first");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let second = spawn_tool_call(&proxy, "second");
        for (call, expected) in [(first, "first"), (second, "second")] {
            let response = tokio::time::timeout(Duration::from_secs(10), call)
                .await
                .expect("call timeout")
                .expect("join")
                .expect("call succeeds");
            let IpcResponse::McpResponse { payload } = response else {
                panic!("unexpected response {response:?}");
            };
            let result: CallToolResult = serde_json::from_value(payload).expect("call result");
            assert_eq!(call_text(&result), expected);
        }

        drop(proxy);
        tokio::time::timeout(Duration::from_secs(5), daemon_task)
            .await
            .expect("daemon task timeout")
            .expect("daemon task join");
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// Sets the per-request ceiling for one test, cleared on drop.
    struct RequestCeilingTestOverride;

    impl RequestCeilingTestOverride {
        fn install(duration: Duration) -> Self {
            REQUEST_CEILING_TEST_OVERRIDE_MS.store(
                duration.as_millis() as u64,
                std::sync::atomic::Ordering::SeqCst,
            );
            Self
        }
    }

    impl Drop for RequestCeilingTestOverride {
        fn drop(&mut self) {
            REQUEST_CEILING_TEST_OVERRIDE_MS.store(0, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// The daemon keeps answering heartbeats but never answers one call. The
    /// call fails at the ceiling and the connection stays up.
    #[tokio::test]
    async fn a_request_the_daemon_never_answers_fails_at_the_ceiling() {
        let _guard = daemon_test_lock().lock().await;
        let _ceiling = RequestCeilingTestOverride::install(Duration::from_millis(500));
        let temp = unique_temp_dir("request-ceiling");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, mut writer, _seen) =
                fake_daemon_handshake(stream, "fake-session-1").await;
            // Answer every Ping; swallow everything else.
            while let Ok(Some(frame)) = ipc::read_frame(&mut reader).await {
                let request: IpcRequest = serde_json::from_slice(&frame).expect("parse request");
                if matches!(request, IpcRequest::Ping { .. }) {
                    let ipc_id = ipc::FrameIds::peek(&frame).ipc_id.expect("tagged");
                    send_tagged(&mut writer, ipc_id, &IpcResponse::Pong).await;
                }
            }
        });

        let session = crate::runtime::establish_daemon_proxy_session(
            None,
            "client-request-ceiling".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        // The heartbeat stays on: its pongs are what used to keep the
        // watchdog from ever firing on the lost call.
        let proxy = Arc::new(IpcProxyHandler::new(session, None));

        let error = tokio::time::timeout(Duration::from_secs(5), spawn_tool_call(&proxy, "lost"))
            .await
            .expect("the lost call hung past its ceiling")
            .expect("join")
            .expect_err("the daemon never answered");
        assert!(
            error.message.contains("did not answer"),
            "unexpected error: {}",
            error.message
        );

        // Only that request failed; the connection still works.
        let pong = proxy
            .session_round_trip(RetryPolicy::UnsafeToRetry, |session_id| IpcRequest::Ping {
                session_id: session_id.to_string(),
            })
            .await
            .expect("ping after the lost call");
        assert!(matches!(pong, IpcResponse::Pong), "{pong:?}");
        assert_eq!(proxy.shared.conn.lock().await.session_id, "fake-session-1");

        drop(proxy);
        let _ = tokio::time::timeout(Duration::from_secs(5), daemon_task).await;
        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }
}
