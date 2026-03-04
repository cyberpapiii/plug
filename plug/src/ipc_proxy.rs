//! IPC proxy handler — bridges stdio MCP ↔ daemon IPC.
//!
//! `IpcProxyHandler` implements rmcp's `ServerHandler` trait but forwards
//! all tool calls through the daemon's shared Engine via Unix socket IPC.
//! This is what `plug connect` uses when a daemon is running.

use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use tokio::sync::Mutex;

use plug_core::ipc::{IpcRequest, IpcResponse};

use crate::daemon;

/// MCP server handler that proxies all requests through the daemon via IPC.
///
/// Holds a persistent IPC connection to the daemon. The connection is
/// established during `cmd_connect` and reused for all MCP traffic.
pub struct IpcProxyHandler {
    reader: Mutex<tokio::net::unix::OwnedReadHalf>,
    writer: Mutex<tokio::net::unix::OwnedWriteHalf>,
    session_id: String,
}

impl IpcProxyHandler {
    /// Create a new proxy handler from an established IPC connection.
    ///
    /// The `session_id` is obtained from a prior `Register` IPC call.
    pub fn new(
        reader: tokio::net::unix::OwnedReadHalf,
        writer: tokio::net::unix::OwnedWriteHalf,
        session_id: String,
    ) -> Self {
        Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            session_id,
        }
    }

    /// Send an IPC request and read the response.
    async fn ipc_round_trip(&self, request: &IpcRequest) -> Result<IpcResponse, McpError> {
        let payload =
            serde_json::to_vec(request).map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Serialize IPC access — one request at a time per connection
        let mut writer = self.writer.lock().await;
        daemon::write_frame_pub(&mut *writer, &payload)
            .await
            .map_err(|e| McpError::internal_error(format!("IPC write failed: {e}"), None))?;
        drop(writer);

        let mut reader = self.reader.lock().await;
        let frame = daemon::read_frame_pub(&mut *reader)
            .await
            .map_err(|e| McpError::internal_error(format!("IPC read failed: {e}"), None))?
            .ok_or_else(|| McpError::internal_error("daemon closed connection", None))?;

        serde_json::from_slice(&frame)
            .map_err(|e| McpError::internal_error(format!("invalid IPC response: {e}"), None))
    }

    /// Send a deregister request (best-effort, used during shutdown).
    #[allow(dead_code)]
    pub async fn deregister(&self) {
        let request = IpcRequest::Deregister {
            session_id: self.session_id.clone(),
        };
        let _ = self.ipc_round_trip(&request).await;
    }
}

#[allow(clippy::manual_async_fn)]
impl ServerHandler for IpcProxyHandler {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability {
            list_changed: Some(true),
        });
        capabilities.resources = Some(ResourcesCapability {
            list_changed: Some(false),
            subscribe: None,
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

            // Forward client info to daemon for client-type-aware tool filtering
            let update_req = IpcRequest::UpdateSession {
                session_id: self.session_id.clone(),
                client_info: client_name,
            };
            if let Err(e) = self.ipc_round_trip(&update_req).await {
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
            let request = IpcRequest::McpRequest {
                session_id: self.session_id.clone(),
                method: "tools/list".to_string(),
                params: None,
            };

            match self.ipc_round_trip(&request).await? {
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

            let ipc_request = IpcRequest::McpRequest {
                session_id: self.session_id.clone(),
                method: "tools/call".to_string(),
                params: Some(params),
            };

            match self.ipc_round_trip(&ipc_request).await? {
                IpcResponse::McpResponse { payload } => {
                    // The payload might be a successful CallToolResult or an MCP error
                    // Try CallToolResult first, then try as error
                    if let Ok(result) = serde_json::from_value::<CallToolResult>(payload.clone()) {
                        Ok(result)
                    } else if let Ok(err) = serde_json::from_value::<McpError>(payload) {
                        Err(err)
                    } else {
                        Err(McpError::internal_error("unexpected tool call response", None))
                    }
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
