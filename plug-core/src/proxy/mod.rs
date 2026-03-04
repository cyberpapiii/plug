use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};

use crate::client_detect::detect_client;
use crate::error::ProtocolError;
use crate::server::ServerManager;

/// MCP proxy handler that aggregates tools from multiple upstream servers
/// and routes tool calls to the correct upstream.
pub struct ProxyHandler {
    server_manager: Arc<ServerManager>,
    /// Routing cache: prefixed_tool_name -> server_id
    tool_routes: Arc<ArcSwap<HashMap<String, String>>>,
    /// Cached merged tool list
    merged_tools: Arc<ArcSwap<Vec<Tool>>>,
    /// Tool name prefix delimiter
    prefix_delimiter: String,
}

impl ProxyHandler {
    pub fn new(server_manager: Arc<ServerManager>, prefix_delimiter: String) -> Self {
        Self {
            server_manager,
            tool_routes: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            merged_tools: Arc::new(ArcSwap::from_pointee(Vec::new())),
            prefix_delimiter,
        }
    }

    /// Refresh the merged tool list and routing table from all upstream servers.
    pub async fn refresh_tools(&self) {
        let upstream_tools = self.server_manager.get_tools().await;

        let mut routes = HashMap::new();
        let mut tools = Vec::new();

        for (server_name, tool) in upstream_tools {
            let prefixed_name =
                format!("{}{}{}", server_name, self.prefix_delimiter, tool.name);

            routes.insert(prefixed_name.clone(), server_name.clone());

            let mut prefixed_tool = tool.clone();
            prefixed_tool.name = Cow::Owned(prefixed_name);
            tools.push(prefixed_tool);
        }

        tracing::info!(
            tool_count = tools.len(),
            server_count = routes.values().collect::<std::collections::HashSet<_>>().len(),
            "refreshed tool cache"
        );

        self.merged_tools.store(Arc::new(tools));
        self.tool_routes.store(Arc::new(routes));
    }
}

#[allow(clippy::manual_async_fn)]
impl ServerHandler for ProxyHandler {
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
            let client_type = detect_client(&request.client_info.name);
            tracing::info!(
                client = %request.client_info.name,
                detected = %client_type,
                "client connected"
            );

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
            let tools = self.merged_tools.load();
            Ok(ListToolsResult::with_all_items(tools.as_ref().clone()))
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            let tool_name = request.name.as_ref();

            // Look up the server for this prefixed tool name
            let routes = self.tool_routes.load();
            let server_id = routes.get(tool_name).ok_or_else(|| {
                McpError::from(ProtocolError::ToolNotFound {
                    tool_name: tool_name.to_string(),
                })
            })?;

            // Strip the prefix to get the original tool name
            let original_name = tool_name
                .strip_prefix(server_id.as_str())
                .and_then(|s| s.strip_prefix(&self.prefix_delimiter))
                .unwrap_or(tool_name);

            // Get the upstream server's client
            let guard = self.server_manager.get_server(server_id).await.ok_or_else(|| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?;

            let upstream = &guard[server_id];

            // Build the upstream call with the original (unprefixed) tool name
            let mut upstream_params = CallToolRequestParams::new(original_name.to_string());
            if let Some(args) = request.arguments {
                upstream_params = upstream_params.with_arguments(args);
            }

            let result: CallToolResult = upstream
                .client
                .peer()
                .call_tool(upstream_params)
                .await
                .map_err(|e| {
                    tracing::error!(
                        server = %server_id,
                        tool = %original_name,
                        error = %e,
                        "upstream tool call failed"
                    );
                    match e {
                        rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                        other => McpError::internal_error(other.to_string(), None),
                    }
                })?;

            Ok(result)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_info_returns_correct_server_info() {
        let sm = Arc::new(ServerManager::new());
        let handler = ProxyHandler::new(sm, "__".to_string());
        let info = handler.get_info();

        assert_eq!(info.server_info.name, "plug");
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        assert!(info.capabilities.tools.is_some());
        assert_eq!(
            info.capabilities.tools.as_ref().unwrap().list_changed,
            Some(true)
        );
        assert!(info.capabilities.resources.is_some());
    }

    #[tokio::test]
    async fn refresh_tools_with_no_servers() {
        let sm = Arc::new(ServerManager::new());
        let handler = ProxyHandler::new(sm, "__".to_string());
        handler.refresh_tools().await;

        let tools = handler.merged_tools.load();
        assert!(tools.is_empty());

        let routes = handler.tool_routes.load();
        assert!(routes.is_empty());
    }
}
