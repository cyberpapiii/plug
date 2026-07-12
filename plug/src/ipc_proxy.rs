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

const DAEMON_PING_INTERVAL: Duration = Duration::from_secs(1);
const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

struct SharedConnection {
    conn: Mutex<crate::runtime::DaemonProxySession>,
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
    /// `refresh_session_locked`/`refresh_session` (the reconnect path),
    /// which already hold `conn` when they lock `replay` for the replay
    /// round trips. Every other mutation site (`initialize`, `subscribe`,
    /// `unsubscribe`, `set_level`) locks `replay` alone, strictly after its
    /// own `session_round_trip` call has already released `conn` — never
    /// while `conn` is held. Do not acquire `replay` and then try to
    /// acquire `conn`; that ordering is never used and would risk deadlock
    /// against the reconnect path above.
    replay: Mutex<ReplayState>,
}

/// Client-negotiated session state that the daemon cannot recover on its
/// own after a restart, tracked here so the proxy can replay it onto the
/// fresh session (see plans/007-ipc-reconnect-state-replay-claude-fable.md).
///
/// IMPORTANT: any FUTURE IPC message that represents durable per-session
/// negotiated client state (in the same family as `UpdateCapabilities`,
/// `resources/subscribe`/`unsubscribe`, and `logging/setLevel`) must be
/// added here and replayed in `replay_session_state_locked`, or it will be
/// silently lost on daemon reconnect exactly like the bug this plan fixes.
#[derive(Default)]
struct ReplayState {
    client_capabilities: Option<ClientCapabilities>,
    subscriptions: HashSet<String>,
    log_level: Option<LoggingLevel>,
}

/// MCP server handler that proxies all requests through the daemon via IPC.
///
/// Holds a persistent IPC connection to the daemon. The connection is
/// established during `cmd_connect` and reused for all MCP traffic.
///
/// A single mutex guards the entire round-trip (write + read) to prevent
/// concurrent requests from reading each other's responses.
pub struct IpcProxyHandler {
    shared: Arc<SharedConnection>,
    heartbeat: JoinHandle<()>,
}

#[derive(Clone, Copy)]
enum RetryPolicy {
    SafeToRetry,
    UnsafeToRetry,
}

struct TransportFailure {
    message: String,
    reconnectable: bool,
}

impl IpcProxyHandler {
    /// Create a new proxy handler from an established IPC connection.
    pub fn new(session: crate::runtime::DaemonProxySession, config_path: Option<PathBuf>) -> Self {
        let pending_daemon_notifications =
            std::sync::Mutex::new(session.pending_notifications.clone());
        let shared = Arc::new(SharedConnection {
            capabilities: std::sync::RwLock::new(session.capabilities.clone()),
            conn: Mutex::new(session),
            config_path,
            peer: std::sync::OnceLock::new(),
            roots_supported: std::sync::atomic::AtomicBool::new(false),
            pending_daemon_notifications,
            replay: Mutex::new(ReplayState::default()),
        });
        let heartbeat = tokio::spawn(Self::heartbeat_loop(shared.clone()));
        Self { shared, heartbeat }
    }

    /// Send an IPC request and read the response.
    ///
    /// Holds the connection lock for the entire round-trip to ensure
    /// request-response pairing under concurrent MCP calls.
    async fn session_round_trip<F>(
        &self,
        retry_policy: RetryPolicy,
        build_request: F,
    ) -> Result<IpcResponse, McpError>
    where
        F: Fn(&str) -> IpcRequest,
    {
        let mut conn = self.shared.conn.lock().await;
        let peer = self.shared.peer.get();
        let request = build_request(&conn.session_id);
        let payload = serde_json::to_vec(&request)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        match Self::try_round_trip_locked(&mut conn, &payload, peer).await {
            Ok(response) => Ok(response),
            Err(failure) if failure.reconnectable => {
                tracing::warn!(error = %failure.message, "daemon IPC connection lost; reconnecting");
                self.reconnect_locked(&mut conn).await?;
                match retry_policy {
                    RetryPolicy::SafeToRetry => {
                        let rebound = build_request(&conn.session_id);
                        let retry_payload = serde_json::to_vec(&rebound)
                            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                        Self::try_round_trip_locked(&mut conn, &retry_payload, peer)
                            .await
                            .map_err(|e| {
                                McpError::internal_error(
                                    format!("IPC retry failed after reconnect: {}", e.message),
                                    None,
                                )
                            })
                    }
                    RetryPolicy::UnsafeToRetry => Err(McpError::internal_error(
                        "REQUEST_RETRY_UNSAFE: daemon connection recovered; retry the tool call",
                        None,
                    )),
                }
            }
            Err(failure) => Err(McpError::internal_error(failure.message, None)),
        }
    }

    async fn try_round_trip_locked(
        conn: &mut crate::runtime::DaemonProxySession,
        payload: &[u8],
        peer: Option<&Peer<RoleServer>>,
    ) -> Result<IpcResponse, TransportFailure> {
        ipc::write_frame(&mut conn.writer, payload)
            .await
            .map_err(|e| Self::transport_failure("IPC write failed", e))?;

        // Read frames in a loop — the daemon may interleave push notifications
        // (logging) and reverse requests (elicitation, sampling) with the actual
        // response. Forward notifications to the downstream peer and handle
        // reverse requests inline, then keep reading until we get the final
        // response frame.
        let mut chunked_response = Vec::new();
        let mut expected_chunks: Option<u32> = None;
        loop {
            let frame = ipc::read_frame(&mut conn.reader)
                .await
                .map_err(|e| Self::transport_failure("IPC read failed", e))?
                .ok_or_else(|| TransportFailure {
                    message: "daemon closed connection".to_string(),
                    reconnectable: true,
                })?;

            // Discriminate frame type by tag key: DaemonToProxyMessage uses
            // `"envelope"`, plain IpcResponse uses `"type"`. Check for the
            // envelope key first to avoid double-parsing on the hot path
            // (normal frames like pings never contain `"envelope"`).
            if frame.windows(10).any(|w| w == b"\"envelope\"") {
                // Parse as DaemonToProxyMessage (envelope-wrapped)
                let daemon_msg: DaemonToProxyMessage =
                    serde_json::from_slice(&frame).map_err(|e| TransportFailure {
                        message: format!("invalid envelope message: {e}"),
                        reconnectable: false,
                    })?;
                match daemon_msg {
                    DaemonToProxyMessage::Response { inner } => match inner {
                        IpcResponse::LoggingNotification { params } => {
                            if let Some(peer) = peer
                                && let Ok(notif_params) = serde_json::from_value::<
                                    LoggingMessageNotificationParam,
                                >(params)
                            {
                                let _ = peer.notify_logging_message(notif_params).await;
                            }
                            continue;
                        }
                        resp @ (IpcResponse::ToolListChangedNotification
                        | IpcResponse::ResourceListChangedNotification
                        | IpcResponse::ResourceUpdatedNotification { .. }
                        | IpcResponse::PromptListChangedNotification
                        | IpcResponse::ProgressNotification { .. }
                        | IpcResponse::CancelledNotification { .. }
                        | IpcResponse::AuthStateChanged { .. }) => {
                            forward_control_notification(peer, resp).await;
                            continue;
                        }
                        other => return Ok(other),
                    },
                    DaemonToProxyMessage::ResponseChunk {
                        chunk_index,
                        chunk_count,
                        payload_b64,
                    } => {
                        if chunk_count == 0 {
                            return Err(TransportFailure {
                                message: "invalid response chunk count 0".to_string(),
                                reconnectable: false,
                            });
                        }
                        if chunk_index == 0 {
                            chunked_response.clear();
                            expected_chunks = Some(chunk_count);
                        } else if expected_chunks != Some(chunk_count) {
                            return Err(TransportFailure {
                                message: "response chunk count changed mid-stream".to_string(),
                                reconnectable: false,
                            });
                        }

                        let decoded = base64::engine::general_purpose::STANDARD
                            .decode(payload_b64)
                            .map_err(|e| TransportFailure {
                                message: format!("invalid chunk payload: {e}"),
                                reconnectable: false,
                            })?;
                        chunked_response.extend_from_slice(&decoded);

                        if chunk_index + 1 == chunk_count {
                            let response: IpcResponse = serde_json::from_slice(&chunked_response)
                                .map_err(|e| TransportFailure {
                                message: format!("invalid chunked IPC response: {e}"),
                                reconnectable: false,
                            })?;
                            chunked_response.clear();
                            return Ok(response);
                        }

                        continue;
                    }
                    DaemonToProxyMessage::ReverseRequest { id, request } => {
                        // Handle reverse request from daemon (elicitation / sampling)
                        let response =
                            Self::handle_daemon_reverse_request(peer, id, *request).await;

                        // Send the response back to the daemon
                        let resp_payload =
                            serde_json::to_vec(&response).map_err(|e| TransportFailure {
                                message: format!("failed to serialize reverse response: {e}"),
                                reconnectable: false,
                            })?;
                        ipc::write_frame(&mut conn.writer, &resp_payload)
                            .await
                            .map_err(|e| {
                                Self::transport_failure("IPC reverse response write failed", e)
                            })?;
                        continue; // keep reading for the actual tool call response
                    }
                }
            }

            // Plain IpcResponse (no envelope key)
            let response: IpcResponse =
                serde_json::from_slice(&frame).map_err(|e| TransportFailure {
                    message: format!("invalid IPC response: {e}"),
                    reconnectable: false,
                })?;

            match response {
                IpcResponse::LoggingNotification { params } => {
                    if let Some(peer) = peer
                        && let Ok(notif_params) =
                            serde_json::from_value::<LoggingMessageNotificationParam>(params)
                    {
                        let _ = peer.notify_logging_message(notif_params).await;
                    }
                    continue; // keep reading for the actual response
                }
                resp @ (IpcResponse::ToolListChangedNotification
                | IpcResponse::ResourceListChangedNotification
                | IpcResponse::ResourceUpdatedNotification { .. }
                | IpcResponse::PromptListChangedNotification
                | IpcResponse::ProgressNotification { .. }
                | IpcResponse::CancelledNotification { .. }
                | IpcResponse::AuthStateChanged { .. }) => {
                    forward_control_notification(peer, resp).await;
                    continue;
                }
                other => return Ok(other),
            }
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

    async fn reconnect_locked(
        &self,
        conn: &mut crate::runtime::DaemonProxySession,
    ) -> Result<(), McpError> {
        self.refresh_session_locked(self.shared.config_path.as_ref(), conn)
            .await
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
        let mut conn = shared.conn.lock().await;
        let peer = shared.peer.get();
        let payload = serde_json::to_vec(&IpcRequest::Ping {
            session_id: conn.session_id.clone(),
        })
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        match Self::try_round_trip_locked(&mut conn, &payload, peer).await {
            Ok(IpcResponse::Pong) => Ok(()),
            Ok(IpcResponse::Error { code, message }) => {
                if matches!(code.as_str(), "SESSION_REPLACED" | "SESSION_MISMATCH") {
                    tracing::warn!(code = %code, message = %message, "daemon heartbeat detected stale session; reconnecting");
                    Self::refresh_session(shared, &mut conn).await?;
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
                Self::refresh_session(shared, &mut conn).await?;
                Ok(())
            }
            Err(failure) => Err(McpError::internal_error(failure.message, None)),
        }
    }

    async fn refresh_session_locked(
        &self,
        config_path: Option<&PathBuf>,
        conn: &mut crate::runtime::DaemonProxySession,
    ) -> Result<(), McpError> {
        let session = crate::runtime::establish_daemon_proxy_session(
            config_path,
            conn.client_id.clone(),
            conn.client_info.clone(),
        )
        .await
        .map_err(|e| McpError::internal_error(format!("daemon reconnect failed: {e}"), None))?;
        if let Ok(mut caps) = self.shared.capabilities.write() {
            *caps = session.capabilities.clone();
        }
        *conn = session;
        Self::replay_session_state_locked(&self.shared, conn).await;
        Ok(())
    }

    async fn refresh_session(
        shared: &Arc<SharedConnection>,
        conn: &mut crate::runtime::DaemonProxySession,
    ) -> Result<(), McpError> {
        let session = crate::runtime::establish_daemon_proxy_session(
            shared.config_path.as_ref(),
            conn.client_id.clone(),
            conn.client_info.clone(),
        )
        .await
        .map_err(|e| McpError::internal_error(format!("daemon reconnect failed: {e}"), None))?;
        if let Ok(mut caps) = shared.capabilities.write() {
            *caps = session.capabilities.clone();
        }
        *conn = session;
        Self::replay_session_state_locked(shared, conn).await;
        Ok(())
    }

    /// Best-effort replay of client-negotiated session state that the fresh
    /// daemon session lost in the restart (see `ReplayState`). Called from
    /// `refresh_session_locked`/`refresh_session` immediately after the new
    /// session has been installed into `conn`.
    ///
    /// CRITICAL: the caller already holds the `shared.conn` lock (`conn` is
    /// a reborrow of that guard), so every round trip here MUST use the
    /// locked path (`try_round_trip_locked` via `send_replay_request`), never
    /// `session_round_trip` — that would try to re-acquire `shared.conn` and
    /// deadlock. `replay` is locked strictly after `conn` here, matching the
    /// ordering documented on `SharedConnection::replay`.
    ///
    /// A replay failure (transport or daemon-rejected) is logged and does
    /// NOT fail the reconnect, and does NOT trigger a recursive reconnect —
    /// a degraded session beats no session, and beats today's silently
    /// degraded session because it's now logged.
    async fn replay_session_state_locked(
        shared: &Arc<SharedConnection>,
        conn: &mut crate::runtime::DaemonProxySession,
    ) {
        let replay = shared.replay.lock().await;
        let peer = shared.peer.get();

        if let Some(caps) = replay.client_capabilities.clone() {
            let request = IpcRequest::UpdateCapabilities {
                session_id: conn.session_id.clone(),
                capabilities: Box::new(caps),
            };
            if let Err(e) = Self::send_replay_request(conn, peer, &request).await {
                tracing::warn!(error = %e, "reconnect: failed to replay client capabilities");
            }
        }

        for uri in &replay.subscriptions {
            let params = serde_json::to_value(SubscribeRequestParams::new(uri.clone())).ok();
            let request = IpcRequest::McpRequest {
                session_id: conn.session_id.clone(),
                method: "resources/subscribe".to_string(),
                params,
            };
            if let Err(e) = Self::send_replay_request(conn, peer, &request).await {
                tracing::warn!(%uri, error = %e, "reconnect: failed to replay subscription");
            }
        }

        if let Some(level) = replay.log_level {
            let params = serde_json::json!({ "level": level });
            let request = IpcRequest::McpRequest {
                session_id: conn.session_id.clone(),
                method: "logging/setLevel".to_string(),
                params: Some(params),
            };
            if let Err(e) = Self::send_replay_request(conn, peer, &request).await {
                tracing::warn!(error = %e, "reconnect: failed to replay log level");
            }
        }
    }

    /// Send a single replay request over the already-locked `conn` and
    /// classify the result as success/failure, without ever calling
    /// `reconnect_locked`/`session_round_trip` (see
    /// `replay_session_state_locked`).
    async fn send_replay_request(
        conn: &mut crate::runtime::DaemonProxySession,
        peer: Option<&Peer<RoleServer>>,
        request: &IpcRequest,
    ) -> Result<(), String> {
        let payload = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        match Self::try_round_trip_locked(conn, &payload, peer).await {
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
            .with_protocol_version(
                serde_json::from_value(serde_json::Value::String(
                    LATEST_PROTOCOL_VERSION.to_string(),
                ))
                .expect("latest protocol version must parse"),
            )
    }

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, McpError>> + Send + '_ {
        async move {
            let client_name = request.client_info.name.to_string();
            tracing::info!(
                client = %client_name,
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
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            let params = request
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "tools/list".to_string(),
                        params: params.clone(),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(format!("failed to parse tools/list: {e}"), None)
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
    }

    fn enqueue_task(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CreateTaskResult, McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::UnsafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "tools/call".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(
                            format!("unexpected task enqueue response: {e}"),
                            None,
                        )
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
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            // Serialize the full request so `_meta` (including
            // `progressToken`) survives to the daemon — matching
            // `enqueue_task`. A hand-built `{name, arguments}` object drops
            // the progress token and silently disables progress on the
            // default `plug connect` path.
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::UnsafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "tools/call".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    // Check if this is an error response before attempting CallToolResult parse
                    if payload.get("code").is_some()
                        && let Ok(err) = serde_json::from_value::<McpError>(payload.clone())
                    {
                        return Err(err);
                    }
                    serde_json::from_value::<CallToolResult>(payload).map_err(|e| {
                        McpError::internal_error(
                            format!("unexpected tool call response: {e}"),
                            None,
                        )
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
    }

    fn list_tasks(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListTasksResult, McpError>> + Send + '_ {
        async move {
            let params = request
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "tasks/list".to_string(),
                        params: params.clone(),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(format!("failed to parse tasks/list: {e}"), None)
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
    }

    fn get_task_info(
        &self,
        request: GetTaskInfoParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetTaskResult, McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "tasks/get".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(format!("failed to parse tasks/get: {e}"), None)
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
    }

    fn get_task_result(
        &self,
        request: GetTaskResultParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetTaskPayloadResult, McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "tasks/result".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => Ok(GetTaskPayloadResult::new(payload)),
                IpcResponse::Error { code, message } => {
                    Err(McpError::internal_error(format!("{code}: {message}"), None))
                }
                other => Err(McpError::internal_error(
                    format!("unexpected IPC response: {other:?}"),
                    None,
                )),
            }
        }
    }

    fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CancelTaskResult, McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "tasks/cancel".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(format!("failed to parse tasks/cancel: {e}"), None)
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
    }

    fn set_level(
        &self,
        request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        async move {
            let params = serde_json::json!({ "level": request.level });
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "logging/setLevel".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { .. } => {
                    // Record for replay after a future daemon reconnect —
                    // see `ReplayState`. `conn` is not held here.
                    self.shared.replay.lock().await.log_level = Some(request.level);
                    Ok(())
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
    }

    fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        async move {
            let params = request
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "resources/list".to_string(),
                        params: params.clone(),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(
                            format!("failed to parse resources/list: {e}"),
                            None,
                        )
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
    }

    fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, McpError>> + Send + '_ {
        async move {
            let params = request
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "resources/templates/list".to_string(),
                        params: params.clone(),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(
                            format!("failed to parse resources/templates/list: {e}"),
                            None,
                        )
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
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResult, McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "resources/read".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(
                            format!("failed to parse resources/read: {e}"),
                            None,
                        )
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
    }

    fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "resources/subscribe".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    if payload.get("code").is_some()
                        && let Ok(err) = serde_json::from_value::<McpError>(payload.clone())
                    {
                        return Err(err);
                    }
                    // Record for replay after a future daemon reconnect —
                    // see `ReplayState`. `conn` is not held here. Only a
                    // successful subscribe is replayed.
                    self.shared
                        .replay
                        .lock()
                        .await
                        .subscriptions
                        .insert(request.uri.clone());
                    Ok(())
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
    }

    fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "resources/unsubscribe".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    if payload.get("code").is_some()
                        && let Ok(err) = serde_json::from_value::<McpError>(payload.clone())
                    {
                        return Err(err);
                    }
                    // Remove from the replay set on success — a failed
                    // unsubscribe must not stop the subscription from being
                    // replayed after a future reconnect.
                    self.shared
                        .replay
                        .lock()
                        .await
                        .subscriptions
                        .remove(&request.uri);
                    Ok(())
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
    }

    fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        async move {
            let params = request
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "prompts/list".to_string(),
                        params: params.clone(),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(format!("failed to parse prompts/list: {e}"), None)
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
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResult, McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "prompts/get".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(format!("failed to parse prompts/get: {e}"), None)
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
    }

    fn complete(
        &self,
        request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CompleteResult, McpError>> + Send + '_ {
        async move {
            let params = serde_json::to_value(&request)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "completion/complete".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(
                            format!("failed to parse completion/complete: {e}"),
                            None,
                        )
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
            let mut conn = shared.conn.lock().await;
            let request = IpcRequest::UpdateRoots {
                session_id: conn.session_id.clone(),
                roots: roots_json,
            };
            let payload = match serde_json::to_vec(&request) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(error = %e, "failed to serialize UpdateRoots");
                    return;
                }
            };
            if let Err(e) = ipc::write_frame(&mut conn.writer, &payload).await {
                tracing::debug!(error = %e, "failed to send UpdateRoots to daemon");
                return;
            }
            // Read response while forwarding any interleaved daemon push traffic.
            loop {
                match ipc::read_frame(&mut conn.reader).await {
                    Ok(Some(frame)) => match serde_json::from_slice::<IpcResponse>(&frame) {
                        Ok(IpcResponse::LoggingNotification { params }) => {
                            if let Ok(notif_params) =
                                serde_json::from_value::<LoggingMessageNotificationParam>(params)
                            {
                                let _ = peer.notify_logging_message(notif_params).await;
                            }
                            continue;
                        }
                        Ok(
                            resp @ (IpcResponse::ToolListChangedNotification
                            | IpcResponse::ResourceListChangedNotification
                            | IpcResponse::ResourceUpdatedNotification { .. }
                            | IpcResponse::PromptListChangedNotification
                            | IpcResponse::ProgressNotification { .. }
                            | IpcResponse::CancelledNotification { .. }
                            | IpcResponse::AuthStateChanged { .. }),
                        ) => {
                            forward_control_notification(Some(peer), resp).await;
                            continue;
                        }
                        Ok(IpcResponse::Ok) => break,
                        Ok(IpcResponse::Error { code, message }) => {
                            tracing::debug!(
                                code = %code,
                                message = %message,
                                "daemon rejected UpdateRoots"
                            );
                            break;
                        }
                        Ok(_) => break,
                        Err(e) => {
                            tracing::debug!(error = %e, "invalid UpdateRoots response");
                            break;
                        }
                    },
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!(error = %e, "failed to read UpdateRoots response");
                        break;
                    }
                }
            }
        }
        Err(error) => {
            tracing::debug!(error = %error, "failed to fetch roots from downstream peer");
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
    use rmcp::ServiceExt as _;
    use rmcp::handler::client::ClientHandler;
    use rmcp::model::{
        CallToolRequest, CallToolRequestParams, ClientRequest, GetTaskInfoParams,
        GetTaskInfoRequest, GetTaskResultParams, GetTaskResultRequest, ServerResult, TaskStatus,
    };
    use tokio::io::AsyncWriteExt as _;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::task::JoinHandle;

    // Shared with the daemon and runtime test modules: every test that touches the
    // global runtime-paths slot must serialize on the SAME lock so the suite is
    // safe under parallel threads (see daemon::runtime_paths_test_lock).
    fn daemon_test_lock() -> &'static tokio::sync::Mutex<()> {
        crate::daemon::runtime_paths_test_lock()
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
            ClientInfo::default().with_protocol_version(
                serde_json::from_value(serde_json::Value::String(
                    LATEST_PROTOCOL_VERSION.to_string(),
                ))
                .expect("latest protocol version must parse"),
            )
        }
    }

    #[derive(Clone)]
    struct ResourceNotifyClient {
        notify: std::sync::Arc<tokio::sync::Notify>,
        uri: std::sync::Arc<tokio::sync::Mutex<Option<String>>>,
    }

    impl ClientHandler for ResourceNotifyClient {
        fn get_info(&self) -> ClientInfo {
            ClientInfo::default().with_protocol_version(
                serde_json::from_value(serde_json::Value::String(
                    LATEST_PROTOCOL_VERSION.to_string(),
                ))
                .expect("latest protocol version must parse"),
            )
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
            crate::runtime::wait_for_daemon_ready(None),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "daemon ready timeout (socket path: {}, task_finished: {})",
                crate::daemon::socket_path().display(),
                handle.is_finished()
            )
        })
        .expect("daemon ready");
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
            server_info.capabilities.tasks.is_some(),
            "IPC proxy should advertise tasks capability when routed tools exist"
        );

        let task_request = CallToolRequestParams::new("Mock__echo")
            .with_arguments(
                serde_json::json!({"input": "task-mode"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .with_task(serde_json::Map::new());

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

        let final_status = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let response = client
                    .peer()
                    .send_request(ClientRequest::GetTaskInfoRequest(GetTaskInfoRequest::new(
                        GetTaskInfoParams {
                            meta: None,
                            task_id: task_id.clone(),
                        },
                    )))
                    .await
                    .expect("task info response");
                match response {
                    ServerResult::GetTaskResult(result) => {
                        if result.task.status == TaskStatus::Completed {
                            break result.task;
                        }
                    }
                    other => panic!("unexpected task info response: {other:?}"),
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("task completion timeout");
        assert_eq!(final_status.status, TaskStatus::Completed);

        let payload_response = client
            .peer()
            .send_request(ClientRequest::GetTaskResultRequest(
                GetTaskResultRequest::new(GetTaskResultParams {
                    meta: None,
                    task_id: task_id.clone(),
                }),
            ))
            .await
            .expect("task result response");
        match payload_response {
            ServerResult::GetTaskPayloadResult(payload) => {
                assert!(payload.0.to_string().contains("task-mode"));
            }
            ServerResult::CustomResult(payload) => {
                assert!(payload.0.to_string().contains("task-mode"));
            }
            ServerResult::CallToolResult(result) => {
                assert!(format!("{result:?}").contains("task-mode"));
            }
            other => panic!("unexpected task payload response: {other:?}"),
        }

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
            .with_task(serde_json::Map::new());

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
                    .send_request(ClientRequest::GetTaskInfoRequest(GetTaskInfoRequest::new(
                        GetTaskInfoParams {
                            meta: None,
                            task_id: task_id.clone(),
                        },
                    )))
                    .await
                    .expect("replacement task info response");
                match response {
                    ServerResult::GetTaskResult(result) => {
                        if result.task.status == TaskStatus::Completed {
                            break result.task;
                        }
                    }
                    other => panic!("unexpected replacement task info response: {other:?}"),
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("task completion timeout after session replacement");
        assert_eq!(final_status.task_id, task_id);
        assert_eq!(final_status.status, TaskStatus::Completed);

        let payload_response = client_b
            .peer()
            .send_request(ClientRequest::GetTaskResultRequest(
                GetTaskResultRequest::new(GetTaskResultParams {
                    meta: None,
                    task_id: task_id.clone(),
                }),
            ))
            .await
            .expect("replacement task result response");
        match payload_response {
            ServerResult::GetTaskPayloadResult(payload) => {
                assert!(payload.0.to_string().contains("continuity"));
            }
            ServerResult::CustomResult(payload) => {
                assert!(payload.0.to_string().contains("continuity"));
            }
            ServerResult::CallToolResult(result) => {
                assert!(format!("{result:?}").contains("continuity"));
            }
            other => panic!("unexpected replacement task payload response: {other:?}"),
        }

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
        let text = first.raw.as_text().expect("text content");
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
            .find_map(|content| content.raw.as_resource_link())
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
            CallToolRequestParams::new("Mock__artifact_text").with_task(serde_json::Map::new());

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
                    .send_request(ClientRequest::GetTaskResultRequest(
                        GetTaskResultRequest::new(GetTaskResultParams {
                            meta: None,
                            task_id: task_id.clone(),
                        }),
                    ))
                    .await
                    .expect("task result response");
                match response {
                    ServerResult::GetTaskPayloadResult(payload) => break payload,
                    ServerResult::CustomResult(payload) => {
                        if payload.0.get("code").is_some() && payload.0.get("message").is_some() {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        } else {
                            break GetTaskPayloadResult::new(payload.0);
                        }
                    }
                    ServerResult::CallToolResult(result) => {
                        break GetTaskPayloadResult::new(serde_json::to_value(result).unwrap());
                    }
                    _ => tokio::time::sleep(Duration::from_millis(50)).await,
                }
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
            .find_map(|content| content.raw.as_resource_link())
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

    // ── Fake-daemon harness for reconnect / retry-policy / frame-handling
    // characterization tests (plan 006) ────────────────────────────────────
    //
    // The harness above (`spawn_test_daemon`) runs a REAL `Engine` +
    // `run_daemon` in-process against an actual upstream mock MCP server —
    // the right tool for end-to-end behavior (tool listing, task lifecycle,
    // artifact spilling), but it gives no way to inspect exactly which wire
    // requests a reconnect sends (the daemon's `ClientRegistry` and its
    // capability tracking are private to `daemon.rs`, invisible from this
    // module) or to make the daemon emit a malformed frame, a precisely
    // timed interleaved notification, or an indefinite stall — none of
    // which the real `Engine` exposes a test hook for, and adding one is
    // out of scope for this plan. The tests below instead speak
    // `plug_core::ipc`'s wire protocol directly against a hand-rolled
    // `UnixListener` bound at the same (test-scoped) `daemon::socket_path()`
    // used everywhere else in this file, giving full control over
    // daemon-side responses while exercising the real client-side code
    // (`session_round_trip`, `try_round_trip_locked`, `reconnect_locked`,
    // and — where a downstream peer matters — the full `IpcProxyHandler` +
    // `.serve()` path) completely unmodified. `fake_daemon_handshake`
    // performs the Register+Capabilities pair that `establish_daemon_proxy_
    // session` sends on the initial connect AND on every reconnect;
    // `drive_fake_daemon_initialize` additionally answers the three extra
    // round trips (`UpdateSession`, `UpdateCapabilities`, a `Capabilities`
    // refresh) that `IpcProxyHandler::initialize` performs only the first
    // time a downstream MCP client connects — matching production, this is
    // also the point at which `shared.peer` becomes populated. Because
    // `establish_daemon_proxy_session` only touches `config_path` when it
    // has to auto-spawn a daemon (never true here, since our listener is
    // already bound), these tests pass `None` for it throughout. All tests
    // below abort the handler's background heartbeat task right after
    // construction, before it can independently race a scripted disconnect
    // against the test's own explicit round trip (`DAEMON_PING_INTERVAL` is
    // only ~1s, which is not a safe margin under this repo's noted CI
    // contention). Like the rest of this module they serialize on
    // `daemon_test_lock()` since they touch the global runtime-paths slot.

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ))
    }

    fn bind_fake_daemon_socket() -> UnixListener {
        let path = crate::daemon::socket_path();
        std::fs::create_dir_all(path.parent().expect("socket path has a parent"))
            .expect("create daemon socket directory");
        let _ = std::fs::remove_file(&path);
        UnixListener::bind(&path).expect("bind fake daemon socket")
    }

    /// Perform the Register + Capabilities handshake that
    /// `establish_daemon_proxy_session` sends on the initial connect AND on
    /// every reconnect, replying with `session_id` and default
    /// `ServerCapabilities`. Returns the split halves for further scripted
    /// interaction plus which request types were observed, so callers can
    /// pin exactly what reconnect does (and does not) replay.
    async fn fake_daemon_handshake(
        stream: UnixStream,
        session_id: &str,
    ) -> (OwnedReadHalf, OwnedWriteHalf, Vec<String>) {
        let (mut reader, mut writer) = stream.into_split();
        let mut seen = Vec::new();

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
                capabilities: serde_json::to_value(ServerCapabilities::default())
                    .expect("serialize default capabilities"),
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
    /// only redo Register+Capabilities (see
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

    #[derive(Clone)]
    struct LoggingCaptureClient {
        notify: Arc<tokio::sync::Notify>,
        messages: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    impl ClientHandler for LoggingCaptureClient {
        fn get_info(&self) -> ClientInfo {
            ClientInfo::default().with_protocol_version(
                serde_json::from_value(serde_json::Value::String(
                    LATEST_PROTOCOL_VERSION.to_string(),
                ))
                .expect("latest protocol version must parse"),
            )
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
            // Register+Capabilities...
            let (stream2, _) = listener.accept().await.expect("accept 2");
            let (mut reader2, mut writer2, seen2) =
                fake_daemon_handshake(stream2, "fake-session-2").await;
            assert_eq!(
                seen2,
                vec!["Register".to_string(), "Capabilities".to_string()],
                "reconnect handshake itself is still exactly Register+Capabilities; \
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
                IpcRequest::McpRequest { method, params, .. } => {
                    assert_eq!(method, "resources/subscribe");
                    let params = params.expect("subscribe replay must carry params");
                    assert_eq!(params["uri"], serde_json::json!("test://resource"));
                }
                other => panic!("expected replayed resources/subscribe, got {other:?}"),
            }
            ipc::send_response(
                &mut writer2,
                &IpcResponse::McpResponse {
                    payload: serde_json::json!({}),
                },
            )
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
                matches!(req, IpcRequest::McpRequest { ref method, .. } if method == "resources/subscribe"),
                "expected replayed resources/subscribe, got {req:?}"
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
                matches!(req, IpcRequest::McpRequest { ref method, .. } if method == "resources/subscribe"),
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
                matches!(req, IpcRequest::McpRequest { ref method, .. } if method == "resources/unsubscribe"),
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
                IpcRequest::McpRequest { method, .. } => assert_eq!(
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
                matches!(req, IpcRequest::McpRequest { ref method, .. } if method == "tools/call"),
                "expected tools/call, got {req:?}"
            );

            // Interleave a push notification BEFORE the actual response,
            // exactly as the real daemon does when an upstream server logs
            // mid-call (plug/src/daemon.rs sends LoggingNotification via
            // plain ipc::send_response, never enveloped — see the "Plain
            // IpcResponse" branch of try_round_trip_locked).
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
                serde_json::to_value(CallToolResult::success(vec![Content::text("ok")]))
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
                matches!(req, IpcRequest::McpRequest { ref method, .. } if method == "tools/call"),
                "expected tools/call, got {req:?}"
            );

            // > MAX_FRAME_SIZE (4 MiB) so plug_core::ipc::send_chunked_response
            // — the SAME helper the real daemon uses — must split it into
            // multiple ResponseChunk envelopes for try_round_trip_locked to
            // reassemble.
            let big_text = "x".repeat(6 * 1024 * 1024);
            let call_result =
                serde_json::to_value(CallToolResult::success(vec![Content::text(big_text)]))
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
            .and_then(|c| c.raw.as_text())
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
                matches!(req, IpcRequest::McpRequest { ref method, .. } if method == "tools/call"),
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
            // try_round_trip_locked's parse-error arms, which always set
            // `reconnectable: false`.
            writer1
                .write_u32(64)
                .await
                .expect("write bogus length prefix");
            writer1.flush().await.expect("flush bogus length prefix");
            drop(reader1);
            drop(writer1);

            // The reconnect after the transport failure only re-sends
            // Register + Capabilities (see
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
                matches!(req2, IpcRequest::McpRequest { ref method, .. } if method == "tools/call"),
                "expected retried tools/call, got {req2:?}"
            );
            let call_result =
                serde_json::to_value(CallToolResult::success(vec![Content::text("recovered")]))
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
                .and_then(|c| c.raw.as_text())
                .map(|t| t.text.as_str()),
            Some("recovered")
        );

        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn silent_daemon_stall_currently_hangs() {
        let _guard = daemon_test_lock().lock().await;
        let temp = unique_temp_dir("stall");
        set_test_runtime_paths(temp.join("r"), temp.join("s"));

        let listener = bind_fake_daemon_socket();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let daemon_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut reader, writer) = drive_fake_daemon_initialize(stream, "fake-session-1").await;

            let frame = ipc::read_frame(&mut reader)
                .await
                .expect("read tools/call frame")
                .expect("connection open");
            let req: IpcRequest = serde_json::from_slice(&frame).expect("parse tools/call request");
            assert!(
                matches!(req, IpcRequest::McpRequest { ref method, .. } if method == "tools/call"),
                "expected tools/call, got {req:?}"
            );

            // Accept the request and never respond — e.g. a wedged upstream
            // server the daemon is still waiting on. Hold the connection
            // open (silent) until the test releases us.
            let _ = release_rx.await;
            drop(reader);
            drop(writer);
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

        // CHARACTERIZATION: no read watchdog today — plan 009 will turn
        // this into a reconnectable failure. Keep the timeout short (real
        // time, not paused: the fake daemon does genuine socket I/O, which
        // paused/mocked time cannot advance around safely).
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            client.call_tool(CallToolRequestParams::new("whatever")),
        )
        .await;
        assert!(
            outcome.is_err(),
            "CHARACTERIZATION: no read watchdog today — a silently stalled daemon currently \
             hangs the round trip indefinitely instead of surfacing a reconnectable failure"
        );

        let _ = release_tx.send(());
        daemon_task.await.expect("daemon task join");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }
}
