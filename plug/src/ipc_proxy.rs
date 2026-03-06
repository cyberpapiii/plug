//! IPC proxy handler — bridges stdio MCP ↔ daemon IPC.
//!
//! `IpcProxyHandler` implements rmcp's `ServerHandler` trait but forwards
//! all tool calls through the daemon's shared Engine via Unix socket IPC.
//! This is what `plug connect` uses when a daemon is running.

use std::path::PathBuf;

use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use tokio::sync::Mutex;

use plug_core::ipc::{self, IpcRequest, IpcResponse};

/// MCP server handler that proxies all requests through the daemon via IPC.
///
/// Holds a persistent IPC connection to the daemon. The connection is
/// established during `cmd_connect` and reused for all MCP traffic.
///
/// A single mutex guards the entire round-trip (write + read) to prevent
/// concurrent requests from reading each other's responses.
pub struct IpcProxyHandler {
    conn: Mutex<crate::runtime::DaemonProxySession>,
    config_path: Option<PathBuf>,
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
        Self {
            conn: Mutex::new(session),
            config_path,
        }
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
        let mut conn = self.conn.lock().await;
        let request = build_request(&conn.session_id);
        let payload = serde_json::to_vec(&request)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        match Self::try_round_trip_locked(&mut conn, &payload).await {
            Ok(response) => Ok(response),
            Err(failure) if failure.reconnectable => {
                tracing::warn!(error = %failure.message, "daemon IPC connection lost; reconnecting");
                self.reconnect_locked(&mut conn).await?;
                match retry_policy {
                    RetryPolicy::SafeToRetry => {
                        let rebound = build_request(&conn.session_id);
                        let retry_payload = serde_json::to_vec(&rebound)
                            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                        Self::try_round_trip_locked(&mut conn, &retry_payload)
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
    ) -> Result<IpcResponse, TransportFailure> {
        ipc::write_frame(&mut conn.writer, payload)
            .await
            .map_err(|e| Self::transport_failure("IPC write failed", e))?;

        let frame = ipc::read_frame(&mut conn.reader)
            .await
            .map_err(|e| Self::transport_failure("IPC read failed", e))?
            .ok_or_else(|| TransportFailure {
                message: "daemon closed connection".to_string(),
                reconnectable: true,
            })?;

        serde_json::from_slice(&frame).map_err(|e| TransportFailure {
            message: format!("invalid IPC response: {e}"),
            reconnectable: false,
        })
    }

    async fn reconnect_locked(
        &self,
        conn: &mut crate::runtime::DaemonProxySession,
    ) -> Result<(), McpError> {
        let session = crate::runtime::establish_daemon_proxy_session(
            self.config_path.as_ref(),
            conn.client_id.clone(),
            conn.client_info.clone(),
        )
        .await
        .map_err(|e| McpError::internal_error(format!("daemon reconnect failed: {e}"), None))?;
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

#[allow(clippy::manual_async_fn)]
impl ServerHandler for IpcProxyHandler {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability {
            list_changed: Some(true),
        });

        InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("plug", env!("CARGO_PKG_VERSION")))
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
            self.conn.lock().await.client_info = Some(client_name.clone());

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

            context.peer.set_peer_info(request);
            Ok(self.get_info())
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "tools/list".to_string(),
                        params: None,
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

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListResourcesResult::default()))
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListPromptsResult::default()))
    }
}
