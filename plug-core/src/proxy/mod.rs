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

/// Atomically-swapped tool cache: tool list + routing table together.
/// Stored in a single ArcSwap to prevent torn reads between list_tools and call_tool.
pub(crate) struct ToolCache {
    pub routes: HashMap<String, String>,
    pub tools: Arc<Vec<Tool>>,
}

/// Shared tool routing logic used by both stdio (ProxyHandler) and HTTP handlers.
pub struct ToolRouter {
    server_manager: Arc<ServerManager>,
    /// Combined tool cache (routes + definitions) swapped atomically
    cache: Arc<ArcSwap<ToolCache>>,
    /// Tool name prefix delimiter
    prefix_delimiter: String,
}

impl ToolRouter {
    pub fn new(server_manager: Arc<ServerManager>, prefix_delimiter: String) -> Self {
        Self {
            server_manager,
            cache: Arc::new(ArcSwap::from_pointee(ToolCache {
                routes: HashMap::new(),
                tools: Arc::new(Vec::new()),
            })),
            prefix_delimiter,
        }
    }

    /// Refresh the merged tool list and routing table from all upstream servers.
    /// Both are swapped atomically to prevent torn reads.
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

        // Atomic swap of both routes and tools together
        self.cache.store(Arc::new(ToolCache {
            routes,
            tools: Arc::new(tools),
        }));
    }

    /// Get the current list of tools (zero-copy via Arc).
    pub fn list_tools(&self) -> Arc<Vec<Tool>> {
        Arc::clone(&self.cache.load().tools)
    }

    /// Call a tool by its prefixed name, routing to the correct upstream server.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult, McpError> {
        // Look up the server for this prefixed tool name
        let cache = self.cache.load();
        let server_id = cache.routes.get(tool_name).ok_or_else(|| {
            McpError::from(ProtocolError::ToolNotFound {
                tool_name: tool_name.to_string(),
            })
        })?;

        // Strip the prefix to get the original tool name
        let original_name = tool_name
            .strip_prefix(server_id.as_str())
            .and_then(|s| s.strip_prefix(&self.prefix_delimiter))
            .unwrap_or(tool_name);

        let server_id = server_id.clone();
        let original_name = original_name.to_string();
        drop(cache);

        // Get the upstream server — wait-free via ArcSwap
        let upstream = self.server_manager.get_upstream(&server_id).ok_or_else(|| {
            McpError::from(ProtocolError::ServerUnavailable {
                server_id: server_id.clone(),
            })
        })?;

        let peer = upstream.client.peer().clone();
        drop(upstream); // Release Arc early

        // Build the upstream call with the original (unprefixed) tool name
        let mut upstream_params = CallToolRequestParams::new(original_name.clone());
        if let Some(args) = arguments {
            upstream_params = upstream_params.with_arguments(args);
        }

        let result: CallToolResult = peer
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

    /// Get a reference to the underlying ServerManager.
    pub fn server_manager(&self) -> &Arc<ServerManager> {
        &self.server_manager
    }
}

/// MCP proxy handler that aggregates tools from multiple upstream servers
/// and routes tool calls to the correct upstream. Used for stdio transport.
pub struct ProxyHandler {
    router: Arc<ToolRouter>,
}

impl ProxyHandler {
    pub fn new(server_manager: Arc<ServerManager>, prefix_delimiter: String) -> Self {
        Self {
            router: Arc::new(ToolRouter::new(server_manager, prefix_delimiter)),
        }
    }

    /// Create a ProxyHandler from an existing shared ToolRouter.
    pub fn from_router(router: Arc<ToolRouter>) -> Self {
        Self { router }
    }

    /// Refresh the merged tool list and routing table from all upstream servers.
    pub async fn refresh_tools(&self) {
        self.router.refresh_tools().await;
    }

    /// Get a reference to the underlying ToolRouter.
    pub fn router(&self) -> &Arc<ToolRouter> {
        &self.router
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
            let tools = self.router.list_tools();
            Ok(ListToolsResult::with_all_items((*tools).clone()))
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            self.router
                .call_tool(request.name.as_ref(), request.arguments)
                .await
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

        let tools = handler.router().list_tools();
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn tool_router_list_tools_returns_arc() {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, "__".to_string());
        router.refresh_tools().await;

        let tools1 = router.list_tools();
        let tools2 = router.list_tools();
        // Both should point to the same allocation (Arc)
        assert!(Arc::ptr_eq(&tools1, &tools2));
    }
}
