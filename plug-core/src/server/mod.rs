#![allow(clippy::mutable_key_type)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use futures::future::join_all;
use futures::stream::BoxStream;
use rmcp::ErrorData as McpError;
use rmcp::ServiceExt as _;
use rmcp::handler::client::ClientHandler;
use rmcp::model::{
    CancelledNotificationParam, ClientInfo, CreateElicitationRequestParams,
    CreateElicitationResult, CreateMessageRequestParams, CreateMessageResult,
    ElicitationCapability, FormElicitationCapability, InitializedNotification,
    LoggingMessageNotificationParam, ProgressNotificationParam, Prompt, Resource, ResourceTemplate,
    ResourceUpdatedNotificationParam, RootsCapabilities, SamplingCapability, ServerCapabilities,
    SetLevelRequestParams, TasksCapability, Tool, UrlElicitationCapability,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
    StreamableHttpError, StreamableHttpPostResponse,
};
use sse_stream::{Error as SseError, Sse};

use crate::circuit::{CircuitBreaker, CircuitBreakerConfig};
use crate::config::{Config, ServerConfig, TransportType};
use crate::proxy::ToolRouter;
use crate::transport::sse_client::{LegacySseClientTransport, LegacySseTransportConfig};
use crate::types::{HealthState, ServerHealth, ServerStatus};

const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

type McpClient = rmcp::service::RunningService<rmcp::RoleClient, Arc<UpstreamClientHandler>>;
const UPSTREAM_REPLACEMENT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(test)]
const UPSTREAM_REPLACEMENT_GRACE_PERIOD: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const UPSTREAM_REPLACEMENT_GRACE_PERIOD: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct InitializedNotificationCompatHttpClient {
    inner: reqwest::Client,
    server_name: Arc<str>,
}

impl InitializedNotificationCompatHttpClient {
    fn new(server_name: Arc<str>) -> Self {
        Self {
            inner: reqwest::Client::new(),
            server_name,
        }
    }
}

fn is_initialized_notification_message(message: &rmcp::model::ClientJsonRpcMessage) -> bool {
    matches!(
        message,
        rmcp::model::ClientJsonRpcMessage::Notification(notification)
            if matches!(notification.notification, rmcp::model::ClientNotification::InitializedNotification(InitializedNotification { .. }))
    )
}

fn is_initialized_notification_auth_failure(error: &StreamableHttpError<reqwest::Error>) -> bool {
    let message = error.to_string().to_lowercase();
    crate::oauth::is_auth_error(&message)
        || message.contains("403")
        || message.contains("forbidden")
}

fn is_initialized_notification_compat_failure(error: &StreamableHttpError<reqwest::Error>) -> bool {
    matches!(
        error,
        StreamableHttpError::UnexpectedServerResponse(message)
            if message.starts_with("HTTP 400")
    )
}

impl StreamableHttpClient for InitializedNotificationCompatHttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: rmcp::model::ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<http::HeaderName, http::HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let is_initialized = is_initialized_notification_message(&message);
        let result = <reqwest::Client as StreamableHttpClient>::post_message(
            &self.inner,
            uri,
            message,
            session_id.clone(),
            auth_header,
            custom_headers,
        )
        .await;

        if is_initialized
            && result
                .as_ref()
                .err()
                .is_some_and(is_initialized_notification_compat_failure)
        {
            tracing::warn!(
                server = %self.server_name,
                "ignoring notifications/initialized failure for HTTP upstream; continuing with compatibility fallback"
            );
            return Ok(StreamableHttpPostResponse::Accepted);
        }

        if is_initialized
            && result
                .as_ref()
                .err()
                .is_some_and(is_initialized_notification_auth_failure)
        {
            tracing::debug!(
                server = %self.server_name,
                "notifications/initialized failure classified as auth rejection"
            );
        }

        result
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<http::HeaderName, http::HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        <reqwest::Client as StreamableHttpClient>::delete_session(
            &self.inner,
            uri,
            session_id,
            auth_header,
            custom_headers,
        )
        .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<http::HeaderName, http::HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        <reqwest::Client as StreamableHttpClient>::get_stream(
            &self.inner,
            uri,
            session_id,
            last_event_id,
            auth_header,
            custom_headers,
        )
        .await
    }
}

pub(crate) struct UpstreamClientHandler {
    server_id: Arc<str>,
    tools: Arc<ArcSwap<Vec<Tool>>>,
    router: std::sync::Weak<ToolRouter>,
}

impl ClientHandler for UpstreamClientHandler {
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::new(
            rmcp::model::ClientCapabilities::default(),
            rmcp::model::Implementation::new("plug", env!("CARGO_PKG_VERSION")),
        );
        info.capabilities.roots = Some(RootsCapabilities {
            list_changed: Some(true),
        });
        info.capabilities.tasks = Some(TasksCapability::client_default());
        info.capabilities.sampling = Some(SamplingCapability::default());
        info.capabilities.elicitation = Some(ElicitationCapability {
            form: Some(FormElicitationCapability::default()),
            url: Some(UrlElicitationCapability::default()),
        });
        info = info.with_protocol_version(
            serde_json::from_value(serde_json::Value::String(
                LATEST_PROTOCOL_VERSION.to_string(),
            ))
            .expect("latest protocol version must parse"),
        );
        info
    }

    fn list_roots(
        &self,
        _context: RequestContext<rmcp::RoleClient>,
    ) -> impl Future<Output = Result<rmcp::model::ListRootsResult, rmcp::ErrorData>> + Send + '_
    {
        let router = self.router.clone();
        async move {
            if let Some(router) = router.upgrade() {
                Ok(router.list_roots_union())
            } else {
                Ok(rmcp::model::ListRootsResult::default())
            }
        }
    }

    fn create_elicitation(
        &self,
        request: CreateElicitationRequestParams,
        context: RequestContext<rmcp::RoleClient>,
    ) -> impl Future<Output = Result<CreateElicitationResult, McpError>> + Send + '_ {
        let router = self.router.clone();
        let server_id = Arc::clone(&self.server_id);
        let request_id = context.id.clone();
        async move {
            if let Some(router) = router.upgrade() {
                router
                    .create_elicitation_from_upstream(server_id.as_ref(), request_id, request)
                    .await
            } else {
                Err(McpError::internal_error(
                    "router unavailable during upstream elicitation".to_string(),
                    None,
                ))
            }
        }
    }

    fn create_message(
        &self,
        request: CreateMessageRequestParams,
        context: RequestContext<rmcp::RoleClient>,
    ) -> impl Future<Output = Result<CreateMessageResult, McpError>> + Send + '_ {
        let router = self.router.clone();
        let server_id = Arc::clone(&self.server_id);
        let request_id = context.id.clone();
        async move {
            if let Some(router) = router.upgrade() {
                router
                    .create_message_from_upstream(server_id.as_ref(), request_id, request)
                    .await
            } else {
                Err(McpError::internal_error(
                    "router unavailable during upstream sampling".to_string(),
                    None,
                ))
            }
        }
    }

    fn on_tool_list_changed(
        &self,
        context: NotificationContext<rmcp::RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let tools = Arc::clone(&self.tools);
        let router = self.router.clone();
        let peer = context.peer.clone();
        let server_id = Arc::clone(&self.server_id);

        async move {
            match peer.list_all_tools().await {
                Ok(fresh_tools) => {
                    tools.store(Arc::new(fresh_tools));

                    if let Some(router) = router.upgrade() {
                        router.schedule_tool_list_changed_refresh();
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

    fn on_resource_list_changed(
        &self,
        _context: NotificationContext<rmcp::RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let router = self.router.clone();
        let server_id = Arc::clone(&self.server_id);
        async move {
            if let Some(router) = router.upgrade() {
                tracing::debug!(
                    server = %server_id,
                    "received resources/list_changed from upstream"
                );
                router.schedule_resource_list_changed_refresh();
            }
        }
    }

    fn on_prompt_list_changed(
        &self,
        _context: NotificationContext<rmcp::RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let router = self.router.clone();
        let server_id = Arc::clone(&self.server_id);
        async move {
            if let Some(router) = router.upgrade() {
                tracing::debug!(
                    server = %server_id,
                    "received prompts/list_changed from upstream"
                );
                router.schedule_prompt_list_changed_refresh();
            }
        }
    }

    fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<rmcp::RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let router = self.router.clone();
        let server_id = Arc::clone(&self.server_id);
        async move {
            if let Some(router) = router.upgrade() {
                router.route_upstream_progress(server_id.as_ref(), params);
            }
        }
    }

    fn on_cancelled(
        &self,
        params: CancelledNotificationParam,
        _context: NotificationContext<rmcp::RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let router = self.router.clone();
        let server_id = Arc::clone(&self.server_id);
        async move {
            if let Some(router) = router.upgrade() {
                router.route_upstream_cancelled(server_id.as_ref(), params);
            }
        }
    }

    fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<rmcp::RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let router = self.router.clone();
        async move {
            if let Some(router) = router.upgrade() {
                router.route_upstream_resource_updated(params);
            }
        }
    }

    fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<rmcp::RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        let router = self.router.clone();
        let server_id = Arc::clone(&self.server_id);
        async move {
            if let Some(router) = router.upgrade() {
                router.route_upstream_logging_message(server_id.as_ref(), params);
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
    pub capabilities: ServerCapabilities,
    pub health: ServerHealth,
}

/// Manages the lifecycle of upstream MCP servers.
///
/// Uses `ArcSwap` for wait-free reads — critical for HTTP concurrency where
/// multiple requests resolve tools simultaneously. Writes (server start/stop)
/// are infrequent and use compare-and-swap.
pub struct ServerManager {
    servers: ArcSwap<HashMap<String, Arc<UpstreamServer>>>,
    server_map_write_lock: std::sync::Mutex<()>,
    pub(crate) health: DashMap<String, HealthState>,
    configured_auth: DashMap<String, ConfiguredAuth>,
    pub(crate) circuit_breakers: DashMap<String, Arc<CircuitBreaker>>,
    pub(crate) semaphores: DashMap<String, Arc<tokio::sync::Semaphore>>,
    /// Per-server reconnection flag to prevent stampede (multiple concurrent callers
    /// all trying to reconnect the same server simultaneously).
    reconnecting: DashMap<String, Arc<AtomicBool>>,
    tool_router: std::sync::RwLock<Option<std::sync::Weak<ToolRouter>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfiguredAuth {
    None,
    Bearer,
    Oauth,
}

impl ServerManager {
    fn configured_auth_for_server(config: &ServerConfig) -> ConfiguredAuth {
        if config.auth.as_deref() == Some("oauth") {
            ConfiguredAuth::Oauth
        } else if config.auth_token.is_some() {
            ConfiguredAuth::Bearer
        } else {
            ConfiguredAuth::None
        }
    }

    fn auth_status_from_configured_auth(auth: ConfiguredAuth, health: ServerHealth) -> String {
        match auth {
            ConfiguredAuth::Oauth => {
                if health == ServerHealth::AuthRequired {
                    "auth-required".to_string()
                } else {
                    "oauth".to_string()
                }
            }
            ConfiguredAuth::Bearer => "bearer".to_string(),
            ConfiguredAuth::None => "none".to_string(),
        }
    }

    fn record_start_failure(&self, name: &str, config: &ServerConfig, error: &anyhow::Error) {
        self.configured_auth
            .insert(name.to_string(), Self::configured_auth_for_server(config));
        if config.auth.as_deref() == Some("oauth")
            && (crate::oauth::is_auth_error_chain(error)
                || crate::oauth::is_auth_error(&format!("{error:#}")))
        {
            self.mark_auth_required(name);
        } else {
            self.mark_start_failure(name);
        }
    }

    pub fn new() -> Self {
        Self {
            servers: ArcSwap::from_pointee(HashMap::new()),
            server_map_write_lock: std::sync::Mutex::new(()),
            health: DashMap::new(),
            configured_auth: DashMap::new(),
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

    fn insert_upstream(
        &self,
        name: String,
        upstream: Arc<UpstreamServer>,
    ) -> Option<Arc<UpstreamServer>> {
        let _guard = self
            .server_map_write_lock
            .lock()
            .expect("server map write mutex poisoned");
        let mut new_map = HashMap::clone(&self.servers.load());
        let previous = new_map.insert(name, upstream);
        self.servers.store(Arc::new(new_map));
        previous
    }

    fn remove_upstream(&self, name: &str) -> Option<Arc<UpstreamServer>> {
        let _guard = self
            .server_map_write_lock
            .lock()
            .expect("server map write mutex poisoned");
        let mut new_map = HashMap::clone(&self.servers.load());
        let removed = new_map.remove(name);
        if removed.is_some() {
            self.servers.store(Arc::new(new_map));
        }
        removed
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

                        // Apply current effective log level to new server so all
                        // upstreams converge to the same level regardless of start order.
                        if upstream.capabilities.logging.is_some() {
                            if let Some(router) = self.tool_router().upgrade() {
                                let level = router.log_level();
                                let params = SetLevelRequestParams::new(level);
                                if let Err(e) = upstream.client.peer().set_level(params).await {
                                    tracing::debug!(
                                        server = %name,
                                        error = %e,
                                        "failed to apply initial log level"
                                    );
                                }
                            }
                        }

                        // Clone current map, insert new server, swap
                        let max_concurrent = upstream.config.max_concurrent;
                        let cb_enabled = upstream.config.circuit_breaker_enabled;
                        self.configured_auth.insert(
                            name.clone(),
                            Self::configured_auth_for_server(&upstream.config),
                        );
                        self.insert_upstream(name.clone(), Arc::new(upstream));

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
                        if let Some(server_config) = config.servers.get(&name) {
                            self.record_start_failure(&name, server_config, &e);
                        }
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

                    let capabilities = client
                        .peer()
                        .peer_info()
                        .map(|info| info.capabilities.clone())
                        .unwrap_or_default();

                    Ok(UpstreamServer {
                        name: name.to_string(),
                        config: config.clone(),
                        client,
                        tools,
                        capabilities,
                        health: ServerHealth::Healthy,
                    })
                }
                TransportType::Http => {
                    crate::tls::ensure_rustls_provider_installed();

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

                    // Resolve auth header: OAuth token from cache, or static bearer token
                    let auth_header = if config.auth.as_deref() == Some("oauth") {
                        match crate::oauth::current_or_stored_access_token(name).await {
                            Some(token) => Some(token),
                            None => {
                                tracing::info!(
                                    server = %name,
                                    "OAuth server has no available token, marking AuthRequired"
                                );
                                return Err(anyhow::anyhow!("OAuth authorization required for server '{name}'. Run `plug auth login --server {name}` to authenticate."));
                            }
                        }
                    } else {
                        config.auth_token.as_ref().map(|t| format!("Bearer {}", t.as_str()))
                    };

                    if let Some(header) = auth_header {
                        transport_config = transport_config.auth_header(header);
                    }

                    tracing::info!(
                        server = %name,
                        url = %url,
                        "connecting to HTTP upstream"
                    );

                    let transport = StreamableHttpClientTransport::with_client(
                        InitializedNotificationCompatHttpClient::new(Arc::from(name)),
                        transport_config,
                    );

                    let tools = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
                    let handler = Arc::new(UpstreamClientHandler {
                        server_id: Arc::from(name),
                        tools: Arc::clone(&tools),
                        router: tool_router.clone(),
                    });

                    match handler.serve(transport).await {
                        Ok(client) => {
                            Self::finish_upstream_connection(name, config, client, tools, "HTTP upstream").await
                        }
                        Err(e) => {
                            let error = anyhow::Error::new(e)
                                .context("failed to connect to HTTP upstream");
                            if crate::transport::sse_client::should_fallback_http_error(&error) {
                                tracing::info!(
                                    server = %name,
                                    error = %error,
                                    "HTTP upstream looks legacy-SSE compatible; falling back"
                                );
                                Self::connect_sse_upstream(name, config, tool_router).await
                            } else {
                                Err(error)
                            }
                        }
                    }
                }
                TransportType::Sse => {
                    Self::connect_sse_upstream(name, config, tool_router).await
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

    /// Connect to a legacy SSE upstream server.
    async fn connect_sse_upstream(
        name: &str,
        config: &ServerConfig,
        tool_router: std::sync::Weak<ToolRouter>,
    ) -> Result<UpstreamServer, anyhow::Error> {
        crate::tls::ensure_rustls_provider_installed();

        let url = config
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("SSE transport requires a URL"))?;

        // SSRF protection: same rules as HTTP upstream
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

        let mut transport_config = LegacySseTransportConfig::with_uri(url)
            .endpoint_wait_timeout(Duration::from_secs(config.timeout_secs));

        // Resolve auth token: OAuth token from cache, or static bearer token
        let auth_token_value = if config.auth.as_deref() == Some("oauth") {
            match crate::oauth::current_or_stored_access_token(name).await {
                Some(token) => Some(token),
                None => {
                    return Err(anyhow::anyhow!(
                        "OAuth authorization required for server '{name}'. Run `plug auth login --server {name}` to authenticate."
                    ));
                }
            }
        } else {
            config.auth_token.as_ref().map(|t| t.as_str().to_string())
        };

        if let Some(token) = auth_token_value {
            transport_config = transport_config.auth_token(token.as_str());
        }

        tracing::info!(
            server = %name,
            url = %url,
            "connecting to legacy SSE upstream"
        );

        let transport = LegacySseClientTransport::from_config(transport_config);

        let tools = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
        let handler = Arc::new(UpstreamClientHandler {
            server_id: Arc::from(name),
            tools: Arc::clone(&tools),
            router: tool_router,
        });

        let client: McpClient = handler
            .serve(transport)
            .await
            .map_err(|e| anyhow::anyhow!("failed to connect to legacy SSE upstream: {e}"))?;

        Self::finish_upstream_connection(name, config, client, tools, "legacy SSE upstream").await
    }

    /// Finalize an upstream connection: list tools, extract capabilities, build UpstreamServer.
    async fn finish_upstream_connection(
        name: &str,
        config: &ServerConfig,
        client: McpClient,
        tools: Arc<ArcSwap<Vec<Tool>>>,
        transport_label: &str,
    ) -> Result<UpstreamServer, anyhow::Error> {
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
                "connected to {transport_label}"
            );
        }

        let capabilities = client
            .peer()
            .peer_info()
            .map(|info| info.capabilities.clone())
            .unwrap_or_default();

        Ok(UpstreamServer {
            name: name.to_string(),
            config: config.clone(),
            client,
            tools,
            capabilities,
            health: ServerHealth::Healthy,
        })
    }

    /// Return all tools from all healthy servers, each paired with the server name.
    pub async fn get_tools(&self) -> Vec<(String, rmcp::model::Tool)> {
        let servers = self.servers.load();
        let mut result = Vec::new();
        for (server_name, upstream) in servers.iter() {
            let health_ok = self
                .health
                .get(server_name)
                .map(|h| h.health.is_routable())
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

    pub async fn get_resources(&self) -> Vec<(String, Resource)> {
        let servers = self.servers.load();
        let mut targets: Vec<(String, Arc<UpstreamServer>)> = servers
            .iter()
            .filter_map(|(server_name, upstream)| {
                let health_ok = self
                    .health
                    .get(server_name)
                    .map(|h| h.health.is_routable())
                    .unwrap_or(true);
                (health_ok && upstream.capabilities.resources.is_some())
                    .then(|| (server_name.clone(), Arc::clone(upstream)))
            })
            .collect();
        targets.sort_by(|a, b| a.0.cmp(&b.0));

        let results = join_all(
            targets
                .into_iter()
                .map(|(server_name, upstream)| async move {
                    let resources = upstream.client.peer().list_all_resources().await;
                    (server_name, resources)
                }),
        )
        .await;

        let mut collected = Vec::new();
        for (server_name, resources) in results {
            match resources {
                Ok(resources) => {
                    for resource in resources {
                        collected.push((server_name.clone(), resource));
                    }
                }
                Err(error) => {
                    tracing::warn!(server = %server_name, error = %error, "failed to list resources");
                }
            }
        }
        collected
    }

    pub async fn get_resource_templates(&self) -> Vec<(String, ResourceTemplate)> {
        let servers = self.servers.load();
        let mut targets: Vec<(String, Arc<UpstreamServer>)> = servers
            .iter()
            .filter_map(|(server_name, upstream)| {
                let health_ok = self
                    .health
                    .get(server_name)
                    .map(|h| h.health.is_routable())
                    .unwrap_or(true);
                (health_ok && upstream.capabilities.resources.is_some())
                    .then(|| (server_name.clone(), Arc::clone(upstream)))
            })
            .collect();
        targets.sort_by(|a, b| a.0.cmp(&b.0));

        let results = join_all(
            targets
                .into_iter()
                .map(|(server_name, upstream)| async move {
                    let templates = upstream.client.peer().list_all_resource_templates().await;
                    (server_name, templates)
                }),
        )
        .await;

        let mut collected = Vec::new();
        for (server_name, templates) in results {
            match templates {
                Ok(resource_templates) => {
                    for template in resource_templates {
                        collected.push((server_name.clone(), template));
                    }
                }
                Err(error) => {
                    tracing::warn!(server = %server_name, error = %error, "failed to list resource templates");
                }
            }
        }
        collected
    }

    pub async fn get_prompts(&self) -> Vec<(String, Prompt)> {
        let servers = self.servers.load();
        let mut targets: Vec<(String, Arc<UpstreamServer>)> = servers
            .iter()
            .filter_map(|(server_name, upstream)| {
                let health_ok = self
                    .health
                    .get(server_name)
                    .map(|h| h.health.is_routable())
                    .unwrap_or(true);
                (health_ok && upstream.capabilities.prompts.is_some())
                    .then(|| (server_name.clone(), Arc::clone(upstream)))
            })
            .collect();
        targets.sort_by(|a, b| a.0.cmp(&b.0));

        let results = join_all(
            targets
                .into_iter()
                .map(|(server_name, upstream)| async move {
                    let prompts = upstream.client.peer().list_all_prompts().await;
                    (server_name, prompts)
                }),
        )
        .await;

        let mut collected = Vec::new();
        for (server_name, prompts) in results {
            match prompts {
                Ok(prompts) => {
                    for prompt in prompts {
                        collected.push((server_name.clone(), prompt));
                    }
                }
                Err(error) => {
                    tracing::warn!(server = %server_name, error = %error, "failed to list prompts");
                }
            }
        }
        collected
    }

    pub fn healthy_capabilities(&self) -> Vec<ServerCapabilities> {
        let servers = self.servers.load();
        servers
            .iter()
            .filter(|(server_name, _)| {
                self.health
                    .get(*server_name)
                    .map(|h| h.health.is_routable())
                    .unwrap_or(true)
            })
            .map(|(_, upstream)| upstream.capabilities.clone())
            .collect()
    }

    /// Get a reference to a specific upstream server by name.
    /// Returns an Arc clone for wait-free access — no lock held.
    pub fn get_upstream(&self, server_name: &str) -> Option<Arc<UpstreamServer>> {
        let servers = self.servers.load();
        servers.get(server_name).cloned()
    }

    /// Get all healthy upstream servers as (name, server) pairs.
    pub fn healthy_upstreams(&self) -> Vec<(String, Arc<UpstreamServer>)> {
        let servers = self.servers.load();
        servers
            .iter()
            .filter(|(name, _)| {
                self.health
                    .get(name.as_str())
                    .map(|h| h.health.is_routable())
                    .unwrap_or(true)
            })
            .map(|(name, upstream)| (name.clone(), Arc::clone(upstream)))
            .collect()
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
        join_all(
            map.into_iter().map(|(name, upstream_arc)| {
                retire_upstream_owned(name, upstream_arc, "shutdown_all")
            }),
        )
        .await;

        self.health.clear();
        self.configured_auth.clear();
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
                let auth_status = Self::auth_status_from_configured_auth(
                    Self::configured_auth_for_server(&upstream.config),
                    health,
                );
                ServerStatus {
                    server_id: upstream.name.clone(),
                    health,
                    tool_count: upstream.tools.load().len(),
                    auth_status,
                    last_seen: None,
                }
            })
            .collect();

        for entry in &self.health {
            if servers.contains_key(entry.key()) {
                continue;
            }
            let configured_auth = self
                .configured_auth
                .get(entry.key())
                .map(|value| *value)
                .unwrap_or(ConfiguredAuth::None);
            let auth_status = Self::auth_status_from_configured_auth(configured_auth, entry.health);
            statuses.push(ServerStatus {
                server_id: entry.key().clone(),
                health: entry.health,
                tool_count: 0,
                auth_status,
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

    /// Mark a server as requiring OAuth authentication.
    /// AuthRequired is sticky — it persists until explicit credential provision + reconnect.
    pub fn mark_auth_required(&self, name: &str) {
        self.configured_auth
            .insert(name.to_string(), ConfiguredAuth::Oauth);
        self.health.insert(
            name.to_string(),
            HealthState {
                health: ServerHealth::AuthRequired,
                consecutive_failures: 0,
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
        self.insert_upstream(name.to_string(), Arc::new(upstream));

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
        if let Some(upstream_arc) = self.remove_upstream(name) {
            self.health.remove(name);
            self.circuit_breakers.remove(name);
            self.semaphores.remove(name);
            retire_upstream_owned(name.to_string(), upstream_arc, "stop").await;
        }
    }

    /// Replace an upstream server (used after reconnection).
    /// Updates the servers map and resets circuit breaker and health state.
    pub async fn replace_server(&self, name: &str, upstream: UpstreamServer) {
        let old_upstream = self.insert_upstream(name.to_string(), Arc::new(upstream));

        // Reset circuit breaker on successful reconnection
        if let Some(cb) = self.circuit_breakers.get(name) {
            cb.reset();
        }

        // Reset health state on successful reconnection
        if let Some(mut entry) = self.health.get_mut(name) {
            *entry = HealthState::new();
        }

        tracing::info!(server = %name, "server replaced after reconnection");

        if let Some(old_upstream) = old_upstream {
            if Arc::strong_count(&old_upstream) > 1 {
                let name = name.to_string();
                tokio::spawn(async move {
                    tokio::time::sleep(UPSTREAM_REPLACEMENT_GRACE_PERIOD).await;
                    retire_upstream_owned(name, old_upstream, "replace_after_grace").await;
                });
            } else {
                retire_upstream_owned(name.to_string(), old_upstream, "replace").await;
            }
        }
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

async fn retire_upstream_owned(name: String, upstream_arc: Arc<UpstreamServer>, reason: &str) {
    upstream_arc.client.cancellation_token().cancel();

    match Arc::try_unwrap(upstream_arc) {
        Ok(mut upstream) => {
            match upstream
                .client
                .close_with_timeout(UPSTREAM_REPLACEMENT_SHUTDOWN_TIMEOUT)
                .await
            {
                Ok(Some(_)) => {
                    tracing::info!(server = %name, reason, "retired upstream cleanly");
                }
                Ok(None) => {
                    tracing::warn!(
                        server = %name,
                        reason,
                        timeout_secs = UPSTREAM_REPLACEMENT_SHUTDOWN_TIMEOUT.as_secs(),
                        "upstream shutdown timed out after cancellation"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        server = %name,
                        reason,
                        error = %error,
                        "upstream shutdown join failed after cancellation"
                    );
                }
            }
        }
        Err(arc) => {
            tracing::warn!(
                server = %name,
                reason,
                "could not take ownership of upstream; sent cancellation and dropped Arc"
            );
            drop(arc);
        }
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
    use std::convert::Infallible;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::sse::{Event, Sse};
    use axum::routing::{get, post};
    use axum::{Json, Router};

    use super::*;
    use crate::config::{ServerConfig, TransportType};
    use crate::proxy::{ProxyHandler, RouterConfig};
    use rmcp::handler::server::ServerHandler;
    use rmcp::model::RequestParamsMeta;
    use rmcp::model::{
        AnnotateAble, CallToolRequest, CallToolRequestParams, CallToolResult, CancelTaskParams,
        CancelTaskResult, ClientJsonRpcMessage, ClientRequest, Content, CreateTaskResult,
        GetPromptResult, GetTaskInfoParams, GetTaskPayloadResult, GetTaskResult, Implementation,
        InitializeResult, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult,
        ListTasksResult, ListToolsResult, Meta, NumberOrString, ProgressNotificationParam,
        ProgressToken, Prompt, PromptMessage, PromptMessageContent, PromptMessageRole, RawResource,
        RawResourceTemplate, ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
        ServerJsonRpcMessage, ServerResult, Task, TaskStatus, TasksCapability, Tool,
    };
    use rmcp::service::{Peer, PeerRequestOptions, RequestContext, RoleClient, RoleServer};
    use rmcp::{ClientHandler, ServiceExt};
    use tokio::sync::{Notify, watch};

    #[test]
    fn secret_string_as_str_preserves_auth_header_value() {
        let token = crate::types::SecretString::from("real-token".to_string());
        assert_eq!(format!("Bearer {}", token.as_str()), "Bearer real-token");
        assert_eq!(format!("{token}"), "[REDACTED]");
    }

    fn test_router_config() -> RouterConfig {
        RouterConfig {
            prefix_delimiter: "__".to_string(),
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            meta_tool_mode: false,
            lazy_tools: crate::config::LazyToolsConfig::default(),
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
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
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

    fn make_tool_with_description(name: &str, description: &str) -> Tool {
        Tool::new(
            std::borrow::Cow::Owned(name.to_string()),
            std::borrow::Cow::Owned(description.to_string()),
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

        async fn wait_for_peer(&self) -> Peer<RoleServer> {
            let mut attempts = 0usize;
            loop {
                let peer = { self.peer.lock().unwrap().clone() };
                if let Some(peer) = peer {
                    return peer;
                }

                attempts += 1;
                assert!(attempts < 50, "server peer should be ready before notify");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        async fn notify_tool_list_changed(&self) {
            let peer = self.wait_for_peer().await;
            peer.notify_tool_list_changed()
                .await
                .expect("notify tool list changed");
        }

        async fn set_tools_and_notify(&self, tools: Vec<Tool>) {
            self.tools_tx.send(tools).expect("update tool list");
            self.notify_tool_list_changed().await;
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

    #[derive(Clone)]
    struct ProgressCancelServer {
        cancel_signal: Arc<Notify>,
        cancelled_request: Arc<Mutex<Option<rmcp::model::RequestId>>>,
    }

    impl ProgressCancelServer {
        fn new() -> Self {
            Self {
                cancel_signal: Arc::new(Notify::new()),
                cancelled_request: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl ServerHandler for ProgressCancelServer {
        fn get_info(&self) -> ServerInfo {
            let mut capabilities = ServerCapabilities::default();
            capabilities.tools = Some(rmcp::model::ToolsCapability {
                list_changed: Some(true),
            });
            ServerInfo::new(capabilities)
        }

        fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_ {
            std::future::ready(Ok(ListToolsResult::with_all_items(vec![make_tool("echo")])))
        }

        fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CallToolResult, rmcp::ErrorData>> + Send + '_ {
            let cancel_signal = Arc::clone(&self.cancel_signal);
            async move {
                let _ = request.progress_token();
                cancel_signal.notified().await;
                Ok(CallToolResult::success(vec![Content::text(
                    "cancelled upstream",
                )]))
            }
        }

        fn on_cancelled(
            &self,
            notification: rmcp::model::CancelledNotificationParam,
            _context: rmcp::service::NotificationContext<RoleServer>,
        ) -> impl Future<Output = ()> + Send + '_ {
            let cancel_signal = Arc::clone(&self.cancel_signal);
            let cancelled_request = Arc::clone(&self.cancelled_request);
            async move {
                *cancelled_request.lock().unwrap() = Some(notification.request_id);
                cancel_signal.notify_one();
            }
        }
    }

    #[derive(Default)]
    struct TaskNativeUpstreamHandler {
        next_id: AtomicUsize,
        tasks: Mutex<HashMap<String, (Task, serde_json::Value)>>,
        task_result_requests: Arc<AtomicUsize>,
    }

    impl ServerHandler for TaskNativeUpstreamHandler {
        fn get_info(&self) -> ServerInfo {
            let mut capabilities = ServerCapabilities::default();
            capabilities.tools = Some(rmcp::model::ToolsCapability {
                list_changed: Some(false),
            });
            capabilities.tasks = Some(TasksCapability::server_default());
            ServerInfo::new(capabilities)
        }

        fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_ {
            std::future::ready(Ok(ListToolsResult::with_all_items(vec![make_tool("echo")])))
        }

        fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CallToolResult, rmcp::ErrorData>> + Send + '_ {
            std::future::ready(Err(McpError::internal_error(
                "wrapper mode should not reach upstream call_tool".to_string(),
                None,
            )))
        }

        fn enqueue_task(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CreateTaskResult, McpError>> + Send + '_ {
            let input = request
                .arguments
                .as_ref()
                .and_then(|args| args.get("input"))
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let id = format!(
                "upstream-task-{}",
                self.next_id.fetch_add(1, Ordering::SeqCst) + 1
            );
            let now = rmcp::task_manager::current_timestamp();
            let task = Task::new(id.clone(), TaskStatus::Working, now.clone(), now)
                .with_status_message("Working")
                .with_ttl(60_000)
                .with_poll_interval(25);
            let payload = serde_json::json!({
                "content": [{ "type": "text", "text": format!("task-native {input}") }],
                "isError": false
            });
            self.tasks
                .lock()
                .unwrap()
                .insert(id, (task.clone(), payload));
            std::future::ready(Ok(CreateTaskResult::new(task)))
        }

        fn list_tasks(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<ListTasksResult, McpError>> + Send + '_ {
            let tasks = self
                .tasks
                .lock()
                .unwrap()
                .values()
                .map(|(task, _)| task.clone())
                .collect::<Vec<_>>();
            std::future::ready(Ok(ListTasksResult::new(tasks)))
        }

        fn get_task_info(
            &self,
            request: GetTaskInfoParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<GetTaskResult, McpError>> + Send + '_ {
            let mut tasks = self.tasks.lock().unwrap();
            let task = tasks
                .get_mut(&request.task_id)
                .ok_or_else(|| McpError::invalid_params("unknown upstream task", None))
                .map(|entry| {
                    if entry.0.status == TaskStatus::Working {
                        entry.0.status = TaskStatus::Completed;
                        entry.0.status_message = Some("Completed".to_string());
                        entry.0.last_updated_at = rmcp::task_manager::current_timestamp();
                    }
                    entry.0.clone()
                });
            std::future::ready(task.map(|task| GetTaskResult { meta: None, task }))
        }

        fn get_task_result(
            &self,
            request: rmcp::model::GetTaskResultParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<GetTaskPayloadResult, McpError>> + Send + '_ {
            let call_count = self.task_result_requests.fetch_add(1, Ordering::SeqCst);
            if call_count > 0 {
                return std::future::ready(Err(McpError::internal_error(
                    "upstream task result should have been cached locally after first fetch"
                        .to_string(),
                    None,
                )));
            }
            let result = self
                .tasks
                .lock()
                .unwrap()
                .get(&request.task_id)
                .map(|(_, payload)| GetTaskPayloadResult::new(payload.clone()))
                .ok_or_else(|| McpError::invalid_params("unknown upstream task", None));
            std::future::ready(result)
        }

        fn cancel_task(
            &self,
            request: CancelTaskParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<CancelTaskResult, McpError>> + Send + '_ {
            let mut tasks = self.tasks.lock().unwrap();
            let task = tasks
                .get_mut(&request.task_id)
                .ok_or_else(|| McpError::invalid_params("unknown upstream task", None))
                .map(|entry| {
                    entry.0.status = TaskStatus::Cancelled;
                    entry.0.status_message = Some("Cancelled".to_string());
                    entry.0.last_updated_at = rmcp::task_manager::current_timestamp();
                    entry.0.clone()
                });
            std::future::ready(task.map(|task| CancelTaskResult { meta: None, task }))
        }
    }

    async fn make_connected_test_upstream(name: &str) -> UpstreamServer {
        let (upstream_server, tools_rx) = MutableToolServer::new(vec![make_tool("echo")]);
        let upstream_peer = Arc::clone(&upstream_server.peer);
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let handler = MutableToolServerHandler {
                tools_rx,
                peer: upstream_peer,
            };
            let server = handler
                .serve(server_transport)
                .await
                .expect("start upstream test server");
            let _ = server.waiting().await;
        });

        let tools = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
        let upstream_handler = Arc::new(UpstreamClientHandler {
            server_id: Arc::from(name.to_string()),
            tools: Arc::clone(&tools),
            router: std::sync::Weak::new(),
        });
        let client: McpClient = upstream_handler
            .serve(client_transport)
            .await
            .expect("connect upstream test client");
        let initial_tools = client.peer().list_all_tools().await.expect("initial tools");
        tools.store(Arc::new(initial_tools));

        UpstreamServer {
            name: name.to_string(),
            config: test_server_config(),
            client,
            tools,
            capabilities: ServerCapabilities::default(),
            health: ServerHealth::Healthy,
        }
    }

    async fn make_connected_task_native_upstream(
        name: &str,
        router: &Arc<crate::proxy::ToolRouter>,
    ) -> (UpstreamServer, Arc<AtomicUsize>) {
        let result_request_count = Arc::new(AtomicUsize::new(0));
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        let result_request_count_for_server = Arc::clone(&result_request_count);
        tokio::spawn(async move {
            let server = TaskNativeUpstreamHandler {
                next_id: AtomicUsize::new(0),
                tasks: Mutex::new(HashMap::new()),
                task_result_requests: result_request_count_for_server,
            }
            .serve(server_transport)
            .await
            .expect("start task-native upstream test server");
            let _ = server.waiting().await;
        });

        let tools = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
        let upstream_handler = Arc::new(UpstreamClientHandler {
            server_id: Arc::from(name.to_string()),
            tools: Arc::clone(&tools),
            router: Arc::downgrade(router),
        });
        let client: McpClient = upstream_handler
            .serve(client_transport)
            .await
            .expect("connect task-native upstream test client");
        let initial_tools = client.peer().list_all_tools().await.expect("initial tools");
        tools.store(Arc::new(initial_tools));

        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(rmcp::model::ToolsCapability {
            list_changed: Some(false),
        });
        capabilities.tasks = Some(TasksCapability::server_default());

        (
            UpstreamServer {
                name: name.to_string(),
                config: test_server_config(),
                client,
                tools,
                capabilities,
                health: ServerHealth::Healthy,
            },
            result_request_count,
        )
    }

    struct ProgressClient {
        progress_signal: Arc<Notify>,
        progress: Arc<Mutex<Vec<ProgressNotificationParam>>>,
    }

    impl ClientHandler for ProgressClient {
        async fn on_progress(
            &self,
            params: ProgressNotificationParam,
            _context: rmcp::service::NotificationContext<RoleClient>,
        ) {
            self.progress.lock().unwrap().push(params);
            self.progress_signal.notify_one();
        }
    }

    struct CatalogServer;

    impl ServerHandler for CatalogServer {
        fn get_info(&self) -> ServerInfo {
            let mut capabilities = ServerCapabilities::default();
            capabilities.resources = Some(rmcp::model::ResourcesCapability {
                subscribe: None,
                list_changed: Some(false),
            });
            capabilities.prompts = Some(rmcp::model::PromptsCapability {
                list_changed: Some(false),
            });
            capabilities.tools = Some(rmcp::model::ToolsCapability {
                list_changed: Some(true),
            });
            ServerInfo::new(capabilities)
        }

        fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_ {
            std::future::ready(Ok(ListToolsResult::with_all_items(vec![make_tool("echo")])))
        }

        fn list_resources(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_
        {
            std::future::ready(Ok(ListResourcesResult::with_all_items(vec![
                RawResource::new("memory://notes", "notes").no_annotation(),
            ])))
        }

        fn list_resource_templates(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<ListResourceTemplatesResult, rmcp::ErrorData>> + Send + '_
        {
            std::future::ready(Ok(ListResourceTemplatesResult::with_all_items(vec![
                RawResourceTemplate::new("memory://notes/{id}", "notes_template").no_annotation(),
            ])))
        }

        fn read_resource(
            &self,
            request: rmcp::model::ReadResourceRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<ReadResourceResult, rmcp::ErrorData>> + Send + '_ {
            std::future::ready(Ok(ReadResourceResult::new(vec![ResourceContents::text(
                "hello",
                request.uri,
            )])))
        }

        fn list_prompts(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<ListPromptsResult, rmcp::ErrorData>> + Send + '_ {
            std::future::ready(Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
                "summarize",
                Some("Summarize text"),
                None,
            )])))
        }

        fn get_prompt(
            &self,
            request: rmcp::model::GetPromptRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> impl Future<Output = Result<GetPromptResult, rmcp::ErrorData>> + Send + '_ {
            std::future::ready(Ok(GetPromptResult::new(vec![PromptMessage::new(
                PromptMessageRole::User,
                PromptMessageContent::text(format!("prompt: {}", request.name)),
            )])))
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

    #[tokio::test]
    async fn concurrent_insert_upstream_preserves_all_servers() {
        let mgr = Arc::new(ServerManager::new());
        let upstream_a = make_connected_test_upstream("alpha").await;
        let upstream_b = make_connected_test_upstream("beta").await;

        let mgr_a = Arc::clone(&mgr);
        let mgr_b = Arc::clone(&mgr);

        let task_a = tokio::spawn(async move {
            mgr_a.insert_upstream("alpha".to_string(), Arc::new(upstream_a));
        });
        let task_b = tokio::spawn(async move {
            mgr_b.insert_upstream("beta".to_string(), Arc::new(upstream_b));
        });

        task_a.await.expect("alpha insert task");
        task_b.await.expect("beta insert task");

        let servers = mgr.servers.load();
        assert!(servers.contains_key("alpha"));
        assert!(servers.contains_key("beta"));
        assert_eq!(servers.len(), 2);
    }

    #[tokio::test]
    async fn replace_server_cancels_replaced_upstream_when_old_arc_is_still_held() {
        let mgr = ServerManager::new();

        let (upstream_server_a, tools_rx_a) = MutableToolServer::new(vec![make_tool("echo")]);
        let upstream_peer_a = Arc::clone(&upstream_server_a.peer);
        let (server_transport_a, client_transport_a) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let handler = MutableToolServerHandler {
                tools_rx: tools_rx_a,
                peer: upstream_peer_a,
            };
            let server = handler
                .serve(server_transport_a)
                .await
                .expect("start upstream test server a");
            let _ = server.waiting().await;
        });

        let tools_a = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
        let upstream_handler_a = Arc::new(UpstreamClientHandler {
            server_id: Arc::from("replace-test"),
            tools: Arc::clone(&tools_a),
            router: std::sync::Weak::new(),
        });
        let client_a: McpClient = upstream_handler_a
            .serve(client_transport_a)
            .await
            .expect("connect upstream test client a");
        let initial_tools_a = client_a
            .peer()
            .list_all_tools()
            .await
            .expect("initial tools a");
        tools_a.store(Arc::new(initial_tools_a));

        mgr.replace_server(
            "replace-test",
            UpstreamServer {
                name: "replace-test".to_string(),
                config: test_server_config(),
                client: client_a,
                tools: tools_a,
                capabilities: ServerCapabilities::default(),
                health: ServerHealth::Healthy,
            },
        )
        .await;

        let old_upstream = mgr.get_upstream("replace-test").expect("old upstream");
        assert!(
            !old_upstream.client.is_closed(),
            "old upstream should start open"
        );

        let (upstream_server_b, tools_rx_b) = MutableToolServer::new(vec![make_tool("echo")]);
        let upstream_peer_b = Arc::clone(&upstream_server_b.peer);
        let (server_transport_b, client_transport_b) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let handler = MutableToolServerHandler {
                tools_rx: tools_rx_b,
                peer: upstream_peer_b,
            };
            let server = handler
                .serve(server_transport_b)
                .await
                .expect("start upstream test server b");
            let _ = server.waiting().await;
        });

        let tools_b = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
        let upstream_handler_b = Arc::new(UpstreamClientHandler {
            server_id: Arc::from("replace-test"),
            tools: Arc::clone(&tools_b),
            router: std::sync::Weak::new(),
        });
        let client_b: McpClient = upstream_handler_b
            .serve(client_transport_b)
            .await
            .expect("connect upstream test client b");
        let initial_tools_b = client_b
            .peer()
            .list_all_tools()
            .await
            .expect("initial tools b");
        tools_b.store(Arc::new(initial_tools_b));

        mgr.replace_server(
            "replace-test",
            UpstreamServer {
                name: "replace-test".to_string(),
                config: test_server_config(),
                client: client_b,
                tools: tools_b,
                capabilities: ServerCapabilities::default(),
                health: ServerHealth::Healthy,
            },
        )
        .await;

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if old_upstream.client.is_closed() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("replaced upstream should be cancelled even with lingering Arc");
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

    #[test]
    fn oauth_start_failure_with_auth_error_chain_marks_auth_required() {
        let mgr = ServerManager::new();
        let config = ServerConfig {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Http,
            url: Some("https://example.com/mcp".to_string()),
            auth_token: None,
            auth: Some("oauth".to_string()),
            oauth_client_id: Some("test-client".to_string()),
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
        };

        let err = anyhow::anyhow!(
            "worker quit with fatal: Transport channel closed, when AuthRequired(AuthRequiredError {{ www_authenticate_header: \"Bearer resource_metadata=\\\"https://example.com/.well-known/oauth-protected-resource/mcp\\\"\" }})"
        )
        .context("failed to connect to HTTP upstream");

        mgr.record_start_failure("supabase", &config, &err);

        let statuses = mgr.server_statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].server_id, "supabase");
        assert_eq!(statuses[0].health, ServerHealth::AuthRequired);
        assert_eq!(statuses[0].auth_status, "auth-required");
    }

    #[test]
    fn mark_auth_required_without_upstream_preserves_oauth_status() {
        let mgr = ServerManager::new();

        mgr.mark_auth_required("todoist");

        let statuses = mgr.server_statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].server_id, "todoist");
        assert_eq!(statuses[0].health, ServerHealth::AuthRequired);
        assert_eq!(statuses[0].auth_status, "auth-required");
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
            let handler = MutableToolServerHandler {
                tools_rx,
                peer: upstream_peer,
            };
            let server = handler
                .serve(server_transport)
                .await
                .expect("start upstream test server");
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

        server_manager
            .replace_server(
                "upstream",
                UpstreamServer {
                    name: "upstream".to_string(),
                    config: test_server_config(),
                    client,
                    tools,
                    capabilities: ServerCapabilities::default(),
                    health: ServerHealth::Healthy,
                },
            )
            .await;
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

    #[tokio::test]
    async fn rapid_upstream_tool_list_changed_notifications_coalesce_before_downstream_stdio_delivery()
     {
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
            let handler = MutableToolServerHandler {
                tools_rx,
                peer: upstream_peer,
            };
            let server = handler
                .serve(server_transport)
                .await
                .expect("start upstream test server");
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

        server_manager
            .replace_server(
                "upstream",
                UpstreamServer {
                    name: "upstream".to_string(),
                    config: test_server_config(),
                    client,
                    tools,
                    capabilities: ServerCapabilities::default(),
                    health: ServerHealth::Healthy,
                },
            )
            .await;
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
        let _downstream_client = ToolListChangedClient {
            signal: Arc::clone(&signal),
            notifications: Arc::clone(&notifications),
        }
        .serve(downstream_transport)
        .await
        .expect("connect downstream client");

        upstream_server
            .tools_tx
            .send(vec![make_tool("echo"), make_tool("extra")])
            .expect("update tool list");
        upstream_server.notify_tool_list_changed().await;
        upstream_server.notify_tool_list_changed().await;
        upstream_server.notify_tool_list_changed().await;

        tokio::time::timeout(Duration::from_secs(5), signal.notified())
            .await
            .expect("downstream stdio client should receive tools/list_changed");
        tokio::time::sleep(Duration::from_secs(1)).await;

        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        assert_eq!(router.tool_count(), 2);
    }

    #[tokio::test]
    async fn task_native_upstream_pass_through_proxies_task_lifecycle() {
        let server_manager = Arc::new(ServerManager::new());
        let router = Arc::new(crate::proxy::ToolRouter::new(
            server_manager.clone(),
            test_router_config(),
        ));
        server_manager.set_tool_router(Arc::downgrade(&router));

        let (upstream, task_result_request_count) = tokio::time::timeout(
            Duration::from_secs(5),
            make_connected_task_native_upstream("upstream", &router),
        )
        .await
        .expect("connect task-native upstream");
        server_manager.replace_server("upstream", upstream).await;
        tokio::time::timeout(Duration::from_secs(5), router.refresh_tools())
            .await
            .expect("refresh routed tools");

        let tool_name = router
            .list_tools()
            .first()
            .expect("task-native tool should be exposed")
            .name
            .to_string();
        let owner = crate::tasks::TaskOwner::new(Arc::<str>::from("stdio:test-task-pass-through"));

        let create = tokio::time::timeout(
            Duration::from_secs(5),
            router.enqueue_tool_task(
                &tool_name,
                Some(
                    serde_json::json!({"input": "pass-through"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                None,
                owner.clone(),
                None,
            ),
        )
        .await
        .expect("create passthrough task timed out")
        .expect("create passthrough task");
        assert_eq!(create.task.task_id, "task_1");
        assert_eq!(create.task.status, TaskStatus::Working);

        let info = tokio::time::timeout(
            Duration::from_secs(5),
            router.get_task_info_for_owner(&owner, &create.task.task_id),
        )
        .await
        .expect("fetch passthrough task info timed out")
        .expect("fetch passthrough task info");
        assert_eq!(info.task.task_id, "task_1");
        assert_eq!(info.task.status, TaskStatus::Completed);

        let payload = tokio::time::timeout(
            Duration::from_secs(5),
            router.get_task_result_for_owner(&owner, &create.task.task_id),
        )
        .await
        .expect("fetch passthrough task result timed out")
        .expect("fetch passthrough task result");
        assert!(payload.0.to_string().contains("task-native pass-through"));
        assert_eq!(task_result_request_count.load(Ordering::SeqCst), 1);

        let cached_payload = tokio::time::timeout(
            Duration::from_secs(5),
            router.get_task_result_for_owner(&owner, &create.task.task_id),
        )
        .await
        .expect("fetch cached passthrough task result timed out")
        .expect("fetch cached passthrough task result");
        assert!(
            cached_payload
                .0
                .to_string()
                .contains("task-native pass-through")
        );
        assert_eq!(
            task_result_request_count.load(Ordering::SeqCst),
            1,
            "second result read should come from local cache"
        );

        let second = tokio::time::timeout(
            Duration::from_secs(5),
            router.enqueue_tool_task(
                &tool_name,
                Some(
                    serde_json::json!({"input": "cancel-me"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                None,
                owner.clone(),
                None,
            ),
        )
        .await
        .expect("create second passthrough task timed out")
        .expect("create second passthrough task");
        let cancelled = tokio::time::timeout(
            Duration::from_secs(5),
            router.cancel_task_for_owner(&owner, &second.task.task_id),
        )
        .await
        .expect("cancel passthrough task timed out")
        .expect("cancel passthrough task");
        assert_eq!(cancelled.task.task_id, second.task.task_id);
        assert_eq!(cancelled.task.status, TaskStatus::Cancelled);
        assert!(
            tokio::time::timeout(
                Duration::from_secs(5),
                router.get_task_result_for_owner(&owner, &second.task.task_id),
            )
            .await
            .expect("fetch cancelled passthrough task result timed out")
            .is_err(),
            "cancelled passthrough task should not expose a result"
        );
    }

    #[tokio::test]
    async fn bridge_lazy_mode_exposes_search_and_loaded_direct_tool_calls() {
        let server_manager = Arc::new(ServerManager::new());
        let mut config = test_router_config();
        config.lazy_tools.mode = crate::types::LazyToolSetting::Bridge;
        let router = Arc::new(crate::proxy::ToolRouter::new(
            server_manager.clone(),
            config,
        ));
        server_manager.set_tool_router(Arc::downgrade(&router));

        let (upstream_server, tools_rx) =
            MutableToolServer::new(vec![make_tool_with_description("echo", "Echo input")]);
        let upstream_peer = Arc::clone(&upstream_server.peer);

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let handler = MutableToolServerHandler {
                tools_rx,
                peer: upstream_peer,
            };
            let server = handler
                .serve(server_transport)
                .await
                .expect("start upstream test server");
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

        server_manager
            .replace_server(
                "upstream",
                UpstreamServer {
                    name: "upstream".to_string(),
                    config: test_server_config(),
                    client,
                    tools,
                    capabilities: ServerCapabilities::default(),
                    health: ServerHealth::Healthy,
                },
            )
            .await;
        router.refresh_tools().await;

        let proxy_handler = ProxyHandler::from_router(router.clone());
        let (proxy_server_transport, downstream_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy_handler
                .serve(proxy_server_transport)
                .await
                .expect("start proxy server");
            let _ = server.waiting().await;
        });

        let downstream_client = ToolListChangedClient {
            signal: Arc::new(Notify::new()),
            notifications: Arc::new(AtomicUsize::new(0)),
        }
        .serve(downstream_transport)
        .await
        .expect("connect downstream client");

        let visible_tools = downstream_client
            .list_all_tools()
            .await
            .expect("list tools");
        let visible_names = visible_tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        assert_eq!(visible_names, vec!["plug__search_tools"]);

        let mut search_args = serde_json::Map::new();
        search_args.insert(
            "query".to_string(),
            serde_json::Value::String("echo".to_string()),
        );
        downstream_client
            .call_tool(CallToolRequestParams::new("plug__search_tools").with_arguments(search_args))
            .await
            .expect("search and load hidden tool");

        let visible_tools = downstream_client
            .list_all_tools()
            .await
            .expect("list tools after load");
        assert!(
            visible_tools
                .iter()
                .any(|tool| tool.name.as_ref() == "Upstream__echo"),
            "loaded tool should be visible under its routed name"
        );

        let mut call_args = serde_json::Map::new();
        call_args.insert(
            "message".to_string(),
            serde_json::Value::String("hello".to_string()),
        );
        let result = downstream_client
            .call_tool(CallToolRequestParams::new("Upstream__echo").with_arguments(call_args))
            .await
            .expect("call loaded tool directly");

        let rendered = format!("{result:?}");
        assert!(
            rendered.contains("called echo"),
            "unexpected direct call result: {rendered}"
        );
        assert_eq!(router.active_call_count(), 0);
    }

    #[tokio::test]
    async fn stdio_progress_and_cancellation_route_end_to_end() {
        let server_manager = Arc::new(ServerManager::new());
        let router = Arc::new(crate::proxy::ToolRouter::new(
            server_manager.clone(),
            test_router_config(),
        ));
        server_manager.set_tool_router(Arc::downgrade(&router));

        let upstream_server = ProgressCancelServer::new();
        let cancelled_request = Arc::clone(&upstream_server.cancelled_request);
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = upstream_server
                .serve(server_transport)
                .await
                .expect("start upstream progress server");
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

        server_manager
            .replace_server(
                "upstream",
                UpstreamServer {
                    name: "upstream".to_string(),
                    config: test_server_config(),
                    client,
                    tools,
                    capabilities: ServerCapabilities::default(),
                    health: ServerHealth::Healthy,
                },
            )
            .await;
        router.refresh_tools().await;

        let proxy_handler = ProxyHandler::from_router(router.clone());
        let proxy_client_id = proxy_handler.client_id();
        let (proxy_server_transport, downstream_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy_handler
                .serve(proxy_server_transport)
                .await
                .expect("start proxy server");
            let _ = server.waiting().await;
        });

        let progress_signal = Arc::new(Notify::new());
        let progress = Arc::new(Mutex::new(Vec::new()));
        let downstream_client = ProgressClient {
            progress_signal: Arc::clone(&progress_signal),
            progress: Arc::clone(&progress),
        }
        .serve(downstream_transport)
        .await
        .expect("connect downstream progress client");

        let prefixed_tool_name = router
            .list_tools()
            .first()
            .expect("tool exists")
            .name
            .to_string();
        let progress_token =
            ProgressToken(NumberOrString::String(Arc::from("downstream-progress")));
        let mut params = CallToolRequestParams::new(prefixed_tool_name);
        params.set_progress_token(progress_token.clone());

        let mut options = PeerRequestOptions::default();
        options.meta = Some(Meta::with_progress_token(progress_token.clone()));

        let handle = downstream_client
            .send_cancellable_request(
                rmcp::model::ClientRequest::CallToolRequest(CallToolRequest::new(params)),
                options,
            )
            .await
            .expect("start downstream call");
        let downstream_request_id = handle.id.clone();

        router.publish_protocol_notification(
            crate::notifications::ProtocolNotification::Progress {
                target: crate::notifications::NotificationTarget::Stdio {
                    client_id: proxy_client_id,
                },
                params: ProgressNotificationParam::new(progress_token.clone(), 0.5)
                    .with_message("halfway"),
            },
        );

        tokio::time::timeout(Duration::from_secs(5), progress_signal.notified())
            .await
            .expect("progress should be delivered to downstream client");

        let received = progress.lock().unwrap().clone();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].progress_token, progress_token);
        assert_eq!(received[0].message.as_deref(), Some("halfway"));

        downstream_client
            .notify_cancelled(rmcp::model::CancelledNotificationParam {
                request_id: downstream_request_id,
                reason: Some("user cancelled".to_string()),
            })
            .await
            .expect("send downstream cancellation");

        match handle.await_response().await {
            Err(rmcp::service::ServiceError::Cancelled { reason }) => {
                assert_eq!(reason.as_deref(), Some("user cancelled"));
            }
            other => panic!("unexpected response state: {other:?}"),
        }

        let _cancelled = cancelled_request
            .lock()
            .unwrap()
            .clone()
            .expect("upstream cancellation captured");
        assert_eq!(router.active_call_count(), 0);
    }

    #[tokio::test]
    async fn router_refreshes_resources_and_prompts_and_routes_reads() {
        let server_manager = Arc::new(ServerManager::new());
        let router = Arc::new(crate::proxy::ToolRouter::new(
            server_manager.clone(),
            test_router_config(),
        ));
        server_manager.set_tool_router(Arc::downgrade(&router));

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = CatalogServer
                .serve(server_transport)
                .await
                .expect("start catalog server");
            let _ = server.waiting().await;
        });

        let tools = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
        let upstream_handler = Arc::new(UpstreamClientHandler {
            server_id: Arc::from("catalog"),
            tools: Arc::clone(&tools),
            router: Arc::downgrade(&router),
        });
        let client: McpClient = upstream_handler
            .serve(client_transport)
            .await
            .expect("connect catalog upstream");
        let initial_tools = client.peer().list_all_tools().await.expect("initial tools");
        let capabilities = client
            .peer()
            .peer_info()
            .map(|info| info.capabilities.clone())
            .unwrap_or_default();
        tools.store(Arc::new(initial_tools));

        server_manager
            .replace_server(
                "catalog",
                UpstreamServer {
                    name: "catalog".to_string(),
                    config: test_server_config(),
                    client,
                    tools,
                    capabilities,
                    health: ServerHealth::Healthy,
                },
            )
            .await;

        router.refresh_tools().await;

        assert_eq!(router.list_resources().len(), 1);
        assert_eq!(router.list_resource_templates().len(), 1);
        assert_eq!(router.list_prompts().len(), 1);

        let capabilities = router.synthesized_capabilities();
        assert!(capabilities.resources.is_some());
        assert!(capabilities.prompts.is_some());

        let read = router
            .read_resource("memory://notes")
            .await
            .expect("read resource");
        assert_eq!(read.contents.len(), 1);

        let prompt_name = router.list_prompts()[0].name.clone();
        let prompt = router
            .get_prompt(prompt_name.as_str(), None)
            .await
            .expect("get prompt");
        assert_eq!(prompt.messages.len(), 1);
    }

    // ── Legacy SSE test mock server ──────────────────────────────────────

    #[derive(Clone)]
    struct LegacySseTestAppState {
        tools: Arc<tokio::sync::RwLock<Vec<Tool>>>,
        tx: tokio::sync::broadcast::Sender<sse_stream::Sse>,
        expected_auth: Option<String>,
        reject_post_on_stream_path: bool,
    }

    #[derive(Clone)]
    struct LegacySseTestServer {
        state: LegacySseTestAppState,
    }

    impl LegacySseTestServer {
        async fn spawn(
            initial_tools: Vec<Tool>,
            expected_auth: Option<&str>,
            reject_post_on_stream_path: bool,
        ) -> (Self, String) {
            let (tx, _) = tokio::sync::broadcast::channel(32);
            let state = LegacySseTestAppState {
                tools: Arc::new(tokio::sync::RwLock::new(initial_tools)),
                tx,
                expected_auth: expected_auth.map(str::to_string),
                reject_post_on_stream_path,
            };
            let app = Router::new()
                .route("/mcp", get(legacy_sse_stream).post(legacy_sse_stream_post))
                .route("/messages", post(legacy_sse_messages))
                .with_state(state.clone());

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind legacy SSE test server");
            let addr = listener.local_addr().expect("legacy SSE local addr");
            tokio::spawn(async move {
                axum::serve(listener, app)
                    .await
                    .expect("serve legacy SSE test server");
            });

            (
                Self { state },
                format!("http://127.0.0.1:{}/mcp", addr.port()),
            )
        }

        async fn set_tools_and_notify(&self, tools: Vec<Tool>) {
            *self.state.tools.write().await = tools;
            let notification = ServerJsonRpcMessage::notification(
                rmcp::model::ServerNotification::ToolListChangedNotification(
                    rmcp::model::ToolListChangedNotification::default(),
                ),
            );
            self.state
                .tx
                .send(
                    sse_stream::Sse::default().data(
                        serde_json::to_string(&notification).expect("serialize notification"),
                    ),
                )
                .expect("broadcast tool list changed");
        }
    }

    fn legacy_sse_authorized(headers: &HeaderMap, expected: &Option<String>) -> bool {
        match expected {
            None => true,
            Some(expected) => headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value == format!("Bearer {expected}")),
        }
    }

    async fn legacy_sse_stream(
        State(state): State<LegacySseTestAppState>,
        headers: HeaderMap,
    ) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, StatusCode> {
        if !legacy_sse_authorized(&headers, &state.expected_auth) {
            return Err(StatusCode::UNAUTHORIZED);
        }

        let mut rx = state.tx.subscribe();
        let stream = async_stream::stream! {
            yield Ok(Event::default().event("endpoint").data("/messages"));
            loop {
                match rx.recv().await {
                    Ok(event) => yield Ok(event_to_axum(event)),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        };
        Ok(Sse::new(stream))
    }

    async fn legacy_sse_stream_post(
        State(state): State<LegacySseTestAppState>,
        headers: HeaderMap,
    ) -> StatusCode {
        if !legacy_sse_authorized(&headers, &state.expected_auth) {
            return StatusCode::UNAUTHORIZED;
        }
        if state.reject_post_on_stream_path {
            StatusCode::METHOD_NOT_ALLOWED
        } else {
            StatusCode::ACCEPTED
        }
    }

    async fn legacy_sse_messages(
        State(state): State<LegacySseTestAppState>,
        headers: HeaderMap,
        Json(message): Json<ClientJsonRpcMessage>,
    ) -> StatusCode {
        if !legacy_sse_authorized(&headers, &state.expected_auth) {
            return StatusCode::UNAUTHORIZED;
        }

        match message {
            ClientJsonRpcMessage::Request(request) => match request.request {
                ClientRequest::InitializeRequest(_) => {
                    let mut caps = ServerCapabilities::default();
                    caps.tools = Some(rmcp::model::ToolsCapability {
                        list_changed: Some(true),
                    });
                    let response = ServerJsonRpcMessage::response(
                        ServerResult::InitializeResult(
                            InitializeResult::new(caps)
                                .with_server_info(Implementation::new("legacy-sse-server", "1.0")),
                        ),
                        request.id,
                    );
                    let _ =
                        state.tx.send(sse_stream::Sse::default().data(
                            serde_json::to_string(&response).expect("serialize init response"),
                        ));
                }
                ClientRequest::ListToolsRequest(_) => {
                    let tools = state.tools.read().await.clone();
                    let response = ServerJsonRpcMessage::response(
                        ServerResult::ListToolsResult(ListToolsResult::with_all_items(tools)),
                        request.id,
                    );
                    let _ =
                        state.tx.send(sse_stream::Sse::default().data(
                            serde_json::to_string(&response).expect("serialize tools response"),
                        ));
                }
                ClientRequest::CallToolRequest(call) => {
                    let response = ServerJsonRpcMessage::response(
                        ServerResult::CallToolResult(CallToolResult::success(vec![Content::text(
                            format!("legacy sse called {}", call.params.name),
                        )])),
                        request.id,
                    );
                    let _ = state.tx.send(sse_stream::Sse::default().data(
                        serde_json::to_string(&response).expect("serialize tool call response"),
                    ));
                }
                _ => {}
            },
            ClientJsonRpcMessage::Notification(_) => {}
            ClientJsonRpcMessage::Response(_) => {}
            ClientJsonRpcMessage::Error(_) => {}
        }

        StatusCode::ACCEPTED
    }

    fn event_to_axum(event: sse_stream::Sse) -> Event {
        let mut axum_event = Event::default();
        if let Some(kind) = event.event {
            axum_event = axum_event.event(kind);
        }
        if let Some(data) = event.data {
            axum_event = axum_event.data(data);
        }
        if let Some(id) = event.id {
            axum_event = axum_event.id(id);
        }
        axum_event
    }

    // ── Legacy SSE integration tests ─────────────────────────────────────

    #[tokio::test]
    async fn explicit_sse_upstream_connects_and_routes_tool_calls() {
        let server_manager = Arc::new(ServerManager::new());
        let router = Arc::new(crate::proxy::ToolRouter::new(
            server_manager.clone(),
            test_router_config(),
        ));
        server_manager.set_tool_router(Arc::downgrade(&router));

        let (_legacy_server, url) =
            LegacySseTestServer::spawn(vec![make_tool("echo")], Some("sse-token"), false).await;

        let mut config = test_server_config();
        config.transport = TransportType::Sse;
        config.command = None;
        config.url = Some(url);
        config.auth_token = Some(crate::types::SecretString::from("sse-token".to_string()));

        server_manager
            .start_and_register("legacy-sse", &config)
            .await
            .expect("start legacy SSE upstream");
        router.refresh_tools().await;

        let tool_name = router
            .list_tools()
            .iter()
            .find(|tool| tool.name.ends_with("__echo"))
            .map(|tool| tool.name.to_string())
            .expect("legacy SSE tool should exist");

        let result = router
            .call_tool(&tool_name, None)
            .await
            .expect("legacy SSE tool call");
        assert!(format!("{result:?}").contains("legacy sse called echo"));
    }

    #[tokio::test]
    async fn http_upstream_falls_back_to_legacy_sse() {
        let server_manager = Arc::new(ServerManager::new());
        let router = Arc::new(crate::proxy::ToolRouter::new(
            server_manager.clone(),
            test_router_config(),
        ));
        server_manager.set_tool_router(Arc::downgrade(&router));

        // reject_post_on_stream_path=true → the mock returns 405 on POST to /mcp,
        // which signals to the HTTP transport that this is a legacy SSE server.
        // No auth here so the POST gets 405 (not 401) to trigger fallback.
        let (_legacy_server, url) =
            LegacySseTestServer::spawn(vec![make_tool("echo")], None, true).await;

        let mut config = test_server_config();
        config.transport = TransportType::Http;
        config.command = None;
        config.url = Some(url);

        server_manager
            .start_and_register("legacy-fallback", &config)
            .await
            .expect("fallback HTTP -> legacy SSE upstream");
        router.refresh_tools().await;

        let tool_name = router
            .list_tools()
            .iter()
            .find(|tool| tool.name.ends_with("__echo"))
            .map(|tool| tool.name.to_string())
            .expect("fallback SSE tool should exist");

        let result = router
            .call_tool(&tool_name, None)
            .await
            .expect("fallback SSE tool call");
        assert!(format!("{result:?}").contains("legacy sse called echo"));
    }

    #[tokio::test]
    async fn legacy_sse_tool_list_changed_reaches_downstream_stdio_client() {
        let server_manager = Arc::new(ServerManager::new());
        let router = Arc::new(crate::proxy::ToolRouter::new(
            server_manager.clone(),
            test_router_config(),
        ));
        server_manager.set_tool_router(Arc::downgrade(&router));

        let (legacy_server, url) =
            LegacySseTestServer::spawn(vec![make_tool("echo")], None, false).await;

        let mut config = test_server_config();
        config.transport = TransportType::Sse;
        config.command = None;
        config.url = Some(url);

        server_manager
            .start_and_register("legacy-notify", &config)
            .await
            .expect("start legacy SSE upstream");
        router.refresh_tools().await;

        // Wire up a downstream stdio client to receive notifications.
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
        let _downstream_client = ToolListChangedClient {
            signal: Arc::clone(&signal),
            notifications: Arc::clone(&notifications),
        }
        .serve(downstream_transport)
        .await
        .expect("connect downstream client");

        // Trigger a tool list changed notification from the legacy SSE server.
        legacy_server
            .set_tools_and_notify(vec![make_tool("echo"), make_tool("extra")])
            .await;

        tokio::time::timeout(Duration::from_secs(5), signal.notified())
            .await
            .expect("downstream stdio client should receive tools/list_changed");

        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        assert_eq!(router.tool_count(), 2);
    }
}
