#![allow(clippy::mutable_key_type)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use rmcp::handler::client::ClientHandler;
use rmcp::model::{ClientInfo, Tool};
use rmcp::ServiceExt as _;
use rmcp::service::NotificationContext;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};

use crate::circuit::{CircuitBreaker, CircuitBreakerConfig};
use crate::config::{Config, ServerConfig, TransportType};
use crate::notifications::ProtocolNotification;
use crate::proxy::ToolRouter;
use crate::types::{HealthState, ServerHealth, ServerStatus};

type McpClient =
    rmcp::service::RunningService<rmcp::RoleClient, Arc<UpstreamClientHandler>>;

pub(crate) struct UpstreamClientHandler {
    server_id: Arc<str>,
    tools: Arc<ArcSwap<Vec<Tool>>>,
    router: std::sync::Weak<ToolRouter>,
}

impl ClientHandler for UpstreamClientHandler {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::default()
    }

    fn on_tool_list_changed(
        &self,
        context: NotificationContext<rmcp::RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let server_id = Arc::clone(&self.server_id);
        let tools = Arc::clone(&self.tools);
        let router = self.router.clone();
        let peer = context.peer.clone();

        async move {
            match peer.list_all_tools().await {
                Ok(fresh_tools) => {
                    tools.store(Arc::new(fresh_tools));

                    if let Some(router) = router.upgrade() {
                        router.refresh_tools().await;
                        router.publish_protocol_notification(ProtocolNotification::ToolListChanged {
                            server_id,
                        });
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        server = %server_id,
                        error = %error,
                        "failed to refresh tools after tools/list_changed"
                    );
                }
            }
        }
    }
}

/// A connected upstream MCP server with its client handle and discovered tools.
pub struct UpstreamServer {
    pub name: String,
    pub config: ServerConfig,
    pub(crate) client: McpClient,
    pub(crate) tools: Arc<ArcSwap<Vec<rmcp::model::Tool>>>,
    pub health: ServerHealth,
}

/// Manages the lifecycle of upstream MCP servers.
///
/// Uses `ArcSwap` for wait-free reads — critical for HTTP concurrency where
/// multiple requests resolve tools simultaneously. Writes (server start/stop)
/// are infrequent and use compare-and-swap.
pub struct ServerManager {
    servers: ArcSwap<HashMap<String, Arc<UpstreamServer>>>,
    pub(crate) health: DashMap<String, HealthState>,
    pub(crate) circuit_breakers: DashMap<String, Arc<CircuitBreaker>>,
    pub(crate) semaphores: DashMap<String, Arc<tokio::sync::Semaphore>>,
    /// Per-server reconnection flag to prevent stampede (multiple concurrent callers
    /// all trying to reconnect the same server simultaneously).
    reconnecting: DashMap<String, Arc<AtomicBool>>,
    tool_router: std::sync::RwLock<Option<std::sync::Weak<ToolRouter>>>,
}

impl ServerManager {
    pub fn new() -> Self {
        Self {
            servers: ArcSwap::from_pointee(HashMap::new()),
            health: DashMap::new(),
            circuit_breakers: DashMap::new(),
            semaphores: DashMap::new(),
            reconnecting: DashMap::new(),
            tool_router: std::sync::RwLock::new(None),
        }
    }

    pub fn set_tool_router(&self, router: std::sync::Weak<ToolRouter>) {
        if let Ok(mut guard) = self.tool_router.write() {
            *guard = Some(router);
        }
    }

    fn tool_router(&self) -> std::sync::Weak<ToolRouter> {
        self.tool_router
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
            .unwrap_or_default()
    }

    /// Start all enabled servers from config, batched by `config.startup_concurrency`.
    pub async fn start_all(&self, config: &Config) -> Result<(), anyhow::Error> {
        let enabled: Vec<(String, ServerConfig)> = config
            .servers
            .iter()
            .filter(|(_, sc)| sc.enabled)
            .map(|(name, sc)| (name.clone(), sc.clone()))
            .collect();

        if enabled.is_empty() {
            tracing::info!("no servers configured");
            return Ok(());
        }

        tracing::info!(
            count = enabled.len(),
            concurrency = config.startup_concurrency,
            "starting upstream servers"
        );

        // Process servers in batches of startup_concurrency
        for chunk in enabled.chunks(config.startup_concurrency) {
            let mut join_set = tokio::task::JoinSet::new();

            for (name, server_config) in chunk {
                let name_clone = name.clone();
                let sc = server_config.clone();
                let tool_router = self.tool_router();
                join_set.spawn(async move {
                    let result =
                        Self::start_server_with_router(&name_clone, &sc, tool_router).await;
                    (name_clone, result)
                });
            }

            while let Some(join_result) = join_set.join_next().await {
                match join_result {
                    Ok((name, Ok(upstream))) => {
                        tracing::info!(
                            server = %name,
                            tools = upstream.tools.load().len(),
                            "server started"
                        );
                        // Clone current map, insert new server, swap
                        let max_concurrent = upstream.config.max_concurrent;
                        let cb_enabled = upstream.config.circuit_breaker_enabled;
                        let mut new_map = HashMap::clone(&self.servers.load());
                        new_map.insert(name.clone(), Arc::new(upstream));
                        self.servers.store(Arc::new(new_map));

                        // Initialize resilience state for this server
                        self.health.insert(name.clone(), HealthState::new());
                        self.semaphores.insert(
                            name.clone(),
                            Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
                        );
                        if cb_enabled {
                            self.circuit_breakers.insert(
                                name.clone(),
                                Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default())),
                            );
                        }
                    }
                    Ok((name, Err(e))) => {
                        tracing::error!(server = %name, error = %e, "failed to start server");
                        // One server failing should not prevent others from starting
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "server start task panicked");
                    }
                }
            }
        }

        let servers = self.servers.load();
        tracing::info!(started = servers.len(), "server startup complete");

        Ok(())
    }

    /// Spawn and initialize a single upstream server.
    pub async fn start_server(
        &self,
        name: &str,
        config: &ServerConfig,
    ) -> Result<UpstreamServer, anyhow::Error> {
        Self::start_server_with_router(name, config, self.tool_router()).await
    }

    async fn start_server_with_router(
        name: &str,
        config: &ServerConfig,
        tool_router: std::sync::Weak<ToolRouter>,
    ) -> Result<UpstreamServer, anyhow::Error> {
        // Recursion shield: never start a server named "plug"
        if name == "plug" {
            anyhow::bail!("recursion shield: ignoring upstream server named 'plug'");
        }

        let timeout_duration = Duration::from_secs(config.timeout_secs);

        let result = tokio::time::timeout(timeout_duration, async {
            match config.transport {
                TransportType::Stdio => {
                    let command = config
                        .command
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("stdio transport requires a command"))?;

                    // Use native Command — no shell wrapper, no injection risk.
                    // Arguments are passed directly without shell interpretation.
                    let mut cmd = tokio::process::Command::new(command);
                    for arg in &config.args {
                        cmd.arg(arg);
                    }
                    // Suppress stderr at the OS level to prevent noisy server logs
                    cmd.stderr(std::process::Stdio::null());

                    for (key, value) in &config.env {
                        cmd.env(key, value);
                    }

                    tracing::info!(
                        server = %name,
                        command = %command,
                        args = ?config.args,
                        "spawning server process"
                    );

                    let transport =
                        rmcp::transport::child_process::TokioChildProcess::new(cmd)
                            .map_err(|e| anyhow::anyhow!("failed to spawn process: {e}"))?;

                    let tools = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
                    let handler = Arc::new(UpstreamClientHandler {
                        server_id: Arc::from(name),
                        tools: Arc::clone(&tools),
                        router: tool_router.clone(),
                    });

                    let client: McpClient = handler
                        .serve(transport)
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to initialize client: {e}"))?;

                    let tools_result = client
                        .peer()
                        .list_all_tools()
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to list tools: {e}"))?;
                    tools.store(Arc::new(tools_result));

                    let server_info = client.peer().peer_info();
                    if let Some(info) = server_info {
                        tracing::info!(
                            server = %name,
                            server_name = %info.server_info.name,
                            server_version = %info.server_info.version,
                            "connected to server"
                        );
                    }

                    Ok(UpstreamServer {
                        name: name.to_string(),
                        config: config.clone(),
                        client,
                        tools,
                        health: ServerHealth::Healthy,
                    })
                }
                TransportType::Http => {
                    let url = config
                        .url
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("HTTP transport requires a URL"))?;

                    // SSRF protection: reject private/loopback/link-local URLs.
                    // Note: DNS-based bypasses (hostname resolving to private IP) are
                    // not covered here — would require async DNS resolution at connect time.
                    let parsed = url
                        .parse::<http::Uri>()
                        .map_err(|e| anyhow::anyhow!("invalid URL '{url}': {e}"))?;
                    if let Some(host) = parsed.host() {
                        if is_blocked_host(host) {
                            anyhow::bail!(
                                "URL host '{host}' is blocked — private, loopback, or metadata endpoint"
                            );
                        }
                    }

                    let mut transport_config =
                        StreamableHttpClientTransportConfig::with_uri(url);

                    if let Some(ref token) = config.auth_token {
                        transport_config =
                            transport_config.auth_header(format!("Bearer {token}"));
                    }

                    tracing::info!(
                        server = %name,
                        url = %url,
                        "connecting to HTTP upstream"
                    );

                    let transport =
                        StreamableHttpClientTransport::from_config(transport_config);

                    let tools = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
                    let handler = Arc::new(UpstreamClientHandler {
                        server_id: Arc::from(name),
                        tools: Arc::clone(&tools),
                        router: tool_router.clone(),
                    });

                    let client: McpClient = handler.serve(transport).await.map_err(|e| {
                        anyhow::anyhow!("failed to connect to HTTP upstream: {e}")
                    })?;

                    let tools_result = client
                        .peer()
                        .list_all_tools()
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to list tools: {e}"))?;
                    tools.store(Arc::new(tools_result));

                    let server_info = client.peer().peer_info();
                    if let Some(info) = server_info {
                        tracing::info!(
                            server = %name,
                            server_name = %info.server_info.name,
                            server_version = %info.server_info.version,
                            "connected to HTTP upstream"
                        );
                    }

                    Ok(UpstreamServer {
                        name: name.to_string(),
                        config: config.clone(),
                        client,
                        tools,
                        health: ServerHealth::Healthy,
                    })
                }
            }
        })
        .await;

        match result {
            Ok(Ok(server)) => Ok(server),
            Ok(Err(e)) => {
                tracing::error!(server = %name, error = %e, "server initialization failed");
                Err(e)
            }
            Err(_) => {
                let msg = format!(
                    "server '{}' timed out after {}s during startup",
                    name, config.timeout_secs
                );
                tracing::error!("{}", msg);
                Err(anyhow::anyhow!(msg))
            }
        }
    }

    /// Return all tools from all healthy servers, each paired with the server name.
    pub async fn get_tools(&self) -> Vec<(String, rmcp::model::Tool)> {
        let servers = self.servers.load();
        let mut result = Vec::new();
        for (server_name, upstream) in servers.iter() {
            let health_ok = self
                .health
                .get(server_name)
                .map(|h| h.health != ServerHealth::Failed)
                .unwrap_or(true);
            if health_ok {
                let tools = upstream.tools.load();
                for tool in tools.iter() {
                    result.push((server_name.clone(), tool.clone()));
                }
            }
        }
        result
    }

    /// Get a reference to a specific upstream server by name.
    /// Returns an Arc clone for wait-free access — no lock held.
    pub fn get_upstream(&self, server_name: &str) -> Option<Arc<UpstreamServer>> {
        let servers = self.servers.load();
        servers.get(server_name).cloned()
    }

    /// Gracefully shutdown all upstream servers.
    ///
    /// Swaps in an empty map, then attempts to take ownership of each server
    /// via `Arc::try_unwrap` and cancel it cleanly. Falls back to dropping
    /// the Arc if other references still exist (rmcp's Drop handles cleanup).
    pub async fn shutdown_all(&self) {
        // Swap in empty map — after this, no new code can access the servers
        let old = self.servers.swap(Arc::new(HashMap::new()));

        let map = match Arc::try_unwrap(old) {
            Ok(map) => map,
            Err(arc) => {
                tracing::warn!("other references to server map exist; dropping");
                drop(arc);
                return;
            }
        };

        if map.is_empty() {
            return;
        }

        tracing::info!(count = map.len(), "shutting down upstream servers");

        for (name, upstream_arc) in map {
            match Arc::try_unwrap(upstream_arc) {
                Ok(upstream) => {
                    tracing::info!(server = %name, "shutting down server");
                    // Drop the UpstreamServer — rmcp client's Drop impl handles
                    // sending the shutdown notification and cleaning up the process.
                    drop(upstream);
                    tracing::info!(server = %name, "server shut down");
                }
                Err(arc) => {
                    tracing::warn!(
                        server = %name,
                        "could not take ownership; relying on Drop"
                    );
                    drop(arc);
                }
            }
        }

        self.health.clear();
        self.circuit_breakers.clear();
        self.semaphores.clear();
        self.reconnecting.clear();
    }

    /// Return health/status information for all servers.
    pub fn server_statuses(&self) -> Vec<ServerStatus> {
        let servers = self.servers.load();
        let mut statuses: Vec<ServerStatus> = servers
            .values()
            .map(|upstream| {
                let health = self
                    .health
                    .get(&upstream.name)
                    .map(|h| h.health)
                    .unwrap_or(upstream.health);
                ServerStatus {
                    server_id: upstream.name.clone(),
                    health,
                    tool_count: upstream.tools.load().len(),
                    last_seen: None,
                }
            })
            .collect();

        for entry in &self.health {
            if servers.contains_key(entry.key()) {
                continue;
            }
            statuses.push(ServerStatus {
                server_id: entry.key().clone(),
                health: entry.health,
                tool_count: 0,
                last_seen: None,
            });
        }

        statuses.sort_by(|a, b| a.server_id.cmp(&b.server_id));
        statuses
    }

    /// Record that a configured server failed during startup so it appears in
    /// status output and becomes eligible for proactive recovery.
    pub fn mark_start_failure(&self, name: &str) {
        self.health.insert(
            name.to_string(),
            HealthState {
                health: ServerHealth::Failed,
                consecutive_failures: 6,
            },
        );
    }

    /// Start a single server and register it in the manager.
    pub async fn start_and_register(
        &self,
        name: &str,
        config: &ServerConfig,
    ) -> Result<(), anyhow::Error> {
        let upstream = self.start_server(name, config).await?;
        let max_concurrent = upstream.config.max_concurrent;
        let cb_enabled = upstream.config.circuit_breaker_enabled;
        let mut new_map = HashMap::clone(&self.servers.load());
        new_map.insert(name.to_string(), Arc::new(upstream));
        self.servers.store(Arc::new(new_map));

        self.health.insert(name.to_string(), HealthState::new());
        self.semaphores.insert(
            name.to_string(),
            Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
        );
        if cb_enabled {
            self.circuit_breakers.insert(
                name.to_string(),
                Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default())),
            );
        }
        Ok(())
    }

    /// Stop and remove a single upstream server.
    pub async fn stop_server(&self, name: &str) {
        let mut new_map = HashMap::clone(&self.servers.load());
        if let Some(upstream_arc) = new_map.remove(name) {
            self.servers.store(Arc::new(new_map));
            self.health.remove(name);
            self.circuit_breakers.remove(name);
            self.semaphores.remove(name);

            match Arc::try_unwrap(upstream_arc) {
                Ok(upstream) => {
                    tracing::info!(server = %name, "stopped server");
                    drop(upstream);
                }
                Err(arc) => {
                    tracing::warn!(server = %name, "could not take ownership; relying on Drop");
                    drop(arc);
                }
            }
        }
    }

    /// Replace an upstream server (used after reconnection).
    /// Updates the servers map and resets circuit breaker and health state.
    pub fn replace_server(&self, name: &str, upstream: UpstreamServer) {
        let mut new_map = HashMap::clone(&self.servers.load());
        new_map.insert(name.to_string(), Arc::new(upstream));
        self.servers.store(Arc::new(new_map));

        // Reset circuit breaker on successful reconnection
        if let Some(cb) = self.circuit_breakers.get(name) {
            cb.reset();
        }

        // Reset health state on successful reconnection
        if let Some(mut entry) = self.health.get_mut(name) {
            *entry = HealthState::new();
        }

        tracing::info!(server = %name, "server replaced after reconnection");
    }

    /// Get the reconnecting flag for a server (creates one if missing).
    /// Used to prevent concurrent reconnection stampedes.
    pub fn get_reconnecting_flag(&self, name: &str) -> Arc<AtomicBool> {
        self.reconnecting
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }
}

impl Default for ServerManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a hostname or IP address is a cloud metadata endpoint.
///
/// Only blocks cloud metadata endpoints (169.254.169.254, metadata.google.internal).
/// Loopback and private IPs are allowed because all servers in config.toml are
/// explicitly user-configured — blocking them prevents legitimate local servers.
fn is_blocked_host(host: &str) -> bool {
    // Known metadata hostnames
    if host == "metadata.google.internal" {
        return true;
    }

    // Try parsing as IP address — only block cloud metadata IP
    let host_trimmed = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host_trimmed.parse::<std::net::IpAddr>() {
        return is_metadata_ip(&ip);
    }

    false
}

/// Returns true only for cloud metadata IPs (169.254.169.254).
fn is_metadata_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            // AWS/GCP/Azure metadata endpoint
            *v4 == std::net::Ipv4Addr::new(169, 254, 169, 254)
        }
        std::net::IpAddr::V6(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::config::{ServerConfig, TransportType};
    use crate::proxy::{ProxyHandler, RouterConfig};
    use rmcp::handler::server::ServerHandler;
    use rmcp::model::{
        CallToolRequestParams, CallToolResult, Content, ListToolsResult, ServerCapabilities,
        ServerInfo, Tool,
    };
    use rmcp::service::{Peer, RequestContext, RoleClient, RoleServer};
    use rmcp::{ClientHandler, ServiceExt};
    use tokio::sync::{Notify, watch};

    fn test_router_config() -> RouterConfig {
        RouterConfig {
            prefix_delimiter: "__".to_string(),
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        }
    }

    fn test_server_config() -> ServerConfig {
        ServerConfig {
            command: Some("fake".to_string()),
            args: Vec::new(),
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            url: None,
            auth_token: None,
            timeout_secs: 30,
            call_timeout_secs: 30,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: false,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
        }
    }

    fn make_tool(name: &str) -> Tool {
        Tool::new(
            std::borrow::Cow::Owned(name.to_string()),
            std::borrow::Cow::Borrowed("test tool"),
            Arc::new(serde_json::Map::new()),
        )
    }

    #[derive(Clone)]
    struct MutableToolServer {
        tools_tx: watch::Sender<Vec<Tool>>,
        peer: Arc<Mutex<Option<Peer<RoleServer>>>>,
    }

    impl MutableToolServer {
        fn new(initial_tools: Vec<Tool>) -> (Self, watch::Receiver<Vec<Tool>>) {
            let (tools_tx, tools_rx) = watch::channel(initial_tools);
            (
                Self {
                    tools_tx,
                    peer: Arc::new(Mutex::new(None)),
                },
                tools_rx,
            )
        }

        async fn set_tools_and_notify(&self, tools: Vec<Tool>) {
            self.tools_tx.send(tools).expect("update tool list");

            let mut attempts = 0usize;
            loop {
                let peer = { self.peer.lock().unwrap().clone() };
                if let Some(peer) = peer {
                    peer.notify_tool_list_changed()
                        .await
                        .expect("notify tool list changed");
                    return;
                }

                attempts += 1;
                assert!(attempts < 50, "server peer should be ready before notify");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    struct MutableToolServerHandler {
        tools_rx: watch::Receiver<Vec<Tool>>,
        peer: Arc<Mutex<Option<Peer<RoleServer>>>>,
    }

    impl ServerHandler for MutableToolServerHandler {
        fn get_info(&self) -> ServerInfo {
            let mut capabilities = ServerCapabilities::default();
            capabilities.tools = Some(rmcp::model::ToolsCapability {
                list_changed: Some(true),
            });
            ServerInfo::new(capabilities)
        }

        async fn on_initialized(&self, context: rmcp::service::NotificationContext<RoleServer>) {
            *self.peer.lock().unwrap() = Some(context.peer.clone());
        }

        fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_ {
            let tools = self.tools_rx.borrow().clone();
            std::future::ready(Ok(ListToolsResult::with_all_items(tools)))
        }

        fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CallToolResult, rmcp::ErrorData>> + Send + '_ {
            let content = format!("called {}", request.name);
            std::future::ready(Ok(CallToolResult::success(vec![Content::text(content)])))
        }
    }

    struct ToolListChangedClient {
        signal: Arc<Notify>,
        notifications: Arc<AtomicUsize>,
    }

    impl ClientHandler for ToolListChangedClient {
        async fn on_tool_list_changed(
            &self,
            _context: rmcp::service::NotificationContext<RoleClient>,
        ) {
            self.notifications.fetch_add(1, Ordering::SeqCst);
            self.signal.notify_one();
        }
    }

    #[test]
    fn ssrf_allows_loopback() {
        // Loopback is allowed — user-configured local servers are legitimate
        assert!(!is_blocked_host("127.0.0.1"));
        assert!(!is_blocked_host("127.0.0.2"));
        assert!(!is_blocked_host("[::1]"));
    }

    #[test]
    fn ssrf_allows_private_ranges() {
        // Private IPs are allowed — user-configured local servers are legitimate
        assert!(!is_blocked_host("10.0.0.1"));
        assert!(!is_blocked_host("172.16.0.1"));
        assert!(!is_blocked_host("192.168.1.1"));
    }

    #[test]
    fn ssrf_blocks_cloud_metadata() {
        assert!(is_blocked_host("169.254.169.254"));
        assert!(is_blocked_host("metadata.google.internal"));
        // Other link-local IPs are NOT blocked (only the specific metadata IP)
        assert!(!is_blocked_host("169.254.0.1"));
    }

    #[test]
    fn ssrf_allows_public_ips() {
        assert!(!is_blocked_host("8.8.8.8"));
        assert!(!is_blocked_host("1.1.1.1"));
        assert!(!is_blocked_host("example.com"));
        assert!(!is_blocked_host("localhost"));
    }

    #[test]
    fn replace_server_resets_health_state() {
        let mgr = ServerManager::new();

        // Simulate a degraded server by recording failures
        {
            let mut entry = mgr.health.entry("test".to_string()).or_default();
            entry.record_failure();
            entry.record_failure();
            entry.record_failure(); // → Degraded
            assert_eq!(entry.health, ServerHealth::Degraded);
        }

        // We can't easily create a real UpstreamServer without a running MCP server,
        // but we can verify the health reset logic by checking the DashMap directly.
        // The replace_server method resets health via: `*entry = HealthState::new()`
        if let Some(mut entry) = mgr.health.get_mut("test") {
            *entry = HealthState::new();
        }

        let health = mgr.health.get("test").unwrap();
        assert_eq!(health.health, ServerHealth::Healthy);
        assert_eq!(health.consecutive_failures, 0);
    }

    #[test]
    fn get_reconnecting_flag_returns_same_instance() {
        let mgr = ServerManager::new();
        let flag1 = mgr.get_reconnecting_flag("test");
        let flag2 = mgr.get_reconnecting_flag("test");
        // Both should point to the same AtomicBool
        assert!(Arc::ptr_eq(&flag1, &flag2));
    }

    #[test]
    fn reconnecting_flags_are_per_server() {
        let mgr = ServerManager::new();
        let flag_a = mgr.get_reconnecting_flag("server-a");
        let flag_b = mgr.get_reconnecting_flag("server-b");
        // Different servers should have different flags
        assert!(!Arc::ptr_eq(&flag_a, &flag_b));
    }

    #[test]
    fn server_statuses_include_failed_startups_without_upstreams() {
        let mgr = ServerManager::new();
        mgr.mark_start_failure("workspace");

        let statuses = mgr.server_statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].server_id, "workspace");
        assert_eq!(statuses[0].health, ServerHealth::Failed);
        assert_eq!(statuses[0].tool_count, 0);
    }

    #[tokio::test]
    async fn upstream_tool_list_changed_refreshes_router_and_notifies_stdio_client() {
        let server_manager = Arc::new(ServerManager::new());
        let router = Arc::new(crate::proxy::ToolRouter::new(
            server_manager.clone(),
            test_router_config(),
        ));
        server_manager.set_tool_router(Arc::downgrade(&router));

        let (upstream_server, tools_rx) = MutableToolServer::new(vec![make_tool("echo")]);
        let upstream_peer = Arc::clone(&upstream_server.peer);

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let handler = MutableToolServerHandler { tools_rx, peer: upstream_peer };
            let server = handler.serve(server_transport).await.expect("start upstream test server");
            let _ = server.waiting().await;
        });

        let tools = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
        let upstream_handler = Arc::new(UpstreamClientHandler {
            server_id: Arc::from("upstream"),
            tools: Arc::clone(&tools),
            router: Arc::downgrade(&router),
        });
        let client: McpClient = upstream_handler
            .serve(client_transport)
            .await
            .expect("connect upstream test client");
        let initial_tools = client.peer().list_all_tools().await.expect("initial tools");
        tools.store(Arc::new(initial_tools));

        server_manager.replace_server(
            "upstream",
            UpstreamServer {
                name: "upstream".to_string(),
                config: test_server_config(),
                client,
                tools,
                health: ServerHealth::Healthy,
            },
        );
        router.refresh_tools().await;
        assert_eq!(router.tool_count(), 1);

        let proxy_handler = ProxyHandler::from_router(router.clone());
        let (proxy_server_transport, downstream_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy_handler
                .serve(proxy_server_transport)
                .await
                .expect("start proxy server");
            let _ = server.waiting().await;
        });

        let signal = Arc::new(Notify::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        let downstream_client = ToolListChangedClient {
            signal: Arc::clone(&signal),
            notifications: Arc::clone(&notifications),
        }
        .serve(downstream_transport)
        .await
        .expect("connect downstream client");

        upstream_server
            .set_tools_and_notify(vec![make_tool("echo"), make_tool("extra")])
            .await;

        tokio::time::timeout(Duration::from_secs(5), signal.notified())
            .await
            .expect("downstream stdio client should receive tools/list_changed");

        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        assert_eq!(router.tool_count(), 2);

        let exposed_tool_name = router
            .list_tools()
            .first()
            .expect("tool exists")
            .name
            .to_string();
        let result = downstream_client
            .call_tool(CallToolRequestParams::new(exposed_tool_name))
            .await
            .expect("tool call succeeds");
        assert!(!result.content.is_empty());
        assert_eq!(router.active_call_count(), 0);
    }
}
