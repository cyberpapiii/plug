//! IPC proxy handler — bridges stdio MCP ↔ daemon IPC.
//!
//! `IpcProxyHandler` implements rmcp's `ServerHandler` trait but forwards
//! all tool calls through the daemon's shared Engine via Unix socket IPC.
//! This is what `plug connect` uses when a daemon is running.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
                            if let Some(peer) = peer {
                                if let Ok(notif_params) = serde_json::from_value::<
                                    LoggingMessageNotificationParam,
                                >(params)
                                {
                                    let _ = peer.notify_logging_message(notif_params).await;
                                }
                            }
                            continue;
                        }
                        resp @ (IpcResponse::ToolListChangedNotification
                        | IpcResponse::ResourceListChangedNotification
                        | IpcResponse::PromptListChangedNotification
                        | IpcResponse::ProgressNotification { .. }
                        | IpcResponse::CancelledNotification { .. }
                        | IpcResponse::AuthStateChanged { .. }) => {
                            forward_control_notification(peer, resp).await;
                            continue;
                        }
                        other => return Ok(other),
                    },
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
                    if let Some(peer) = peer {
                        if let Ok(notif_params) =
                            serde_json::from_value::<LoggingMessageNotificationParam>(params)
                        {
                            let _ = peer.notify_logging_message(notif_params).await;
                        }
                    }
                    continue; // keep reading for the actual response
                }
                resp @ (IpcResponse::ToolListChangedNotification
                | IpcResponse::ResourceListChangedNotification
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
        Ok(())
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
        InitializeResult::new(capabilities).with_server_info(
            Implementation::new("plug", env!("CARGO_PKG_VERSION"))
                .with_title("Plug")
                .with_description("MCP multiplexer")
                .with_website_url("https://github.com/plug-mcp/plug")
                .with_icons(vec![
                    Icon::new(
                        "https://raw.githubusercontent.com/plug-mcp/plug/main/docs/assets/plug-icon.svg",
                    )
                    .with_mime_type("image/svg+xml")
                    .with_sizes(vec!["any".to_string()]),
                ]),
        )
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
            if let Err(e) = self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::UpdateCapabilities {
                        session_id: session_id.to_string(),
                        capabilities: Box::new(capabilities.clone()),
                    }
                })
                .await
            {
                tracing::warn!(error = %e, "failed to update session capabilities");
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
            let params = serde_json::json!({
                "name": request.name,
                "arguments": request.arguments,
            });
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
                    if payload.get("code").is_some() {
                        if let Ok(err) = serde_json::from_value::<McpError>(payload.clone()) {
                            return Err(err);
                        }
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
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(format!("failed to parse tasks/result: {e}"), None)
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
                IpcResponse::McpResponse { .. } => Ok(()),
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
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use crate::daemon::{clear_test_runtime_paths, run_daemon, set_test_runtime_paths};
    use plug_core::config::{Config, ServerConfig, TransportType};
    use plug_core::engine::Engine;
    use rmcp::ServiceExt as _;
    use rmcp::handler::client::ClientHandler;
    use rmcp::model::{
        CallToolRequest, CallToolRequestParams, ClientRequest, GetTaskInfoParams,
        GetTaskInfoRequest, GetTaskResultParams, GetTaskResultRequest, ServerResult, TaskStatus,
    };
    use tokio::task::JoinHandle;

    fn daemon_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    fn ensure_mock_server_built() -> PathBuf {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| {
            let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("plug crate should live under workspace root");
            let status = std::process::Command::new("cargo")
                .current_dir(workspace_root)
                .args([
                    "build",
                    "--quiet",
                    "-p",
                    "plug-test-harness",
                    "--bin",
                    "mock-mcp-server",
                ])
                .status()
                .expect("build mock-mcp-server");
            assert!(status.success(), "mock-mcp-server build failed");
            plug_test_harness::mock_server_path()
        })
        .clone()
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

    fn mock_server_config() -> ServerConfig {
        let mock_server = ensure_mock_server_built();
        ServerConfig {
            command: Some(mock_server.display().to_string()),
            args: vec!["--tools".to_string(), "echo".to_string()],
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
        }
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
            crate::runtime::wait_for_daemon_ready(),
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

        engine.shutdown().await;
        daemon
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }
}
