use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::model::RequestParamsMeta;
use rmcp::service::{NotificationContext, Peer, PeerRequestOptions, RequestContext, RoleServer};
use tokio::sync::broadcast;

use crate::circuit::CircuitBreakerError;
use crate::client_detect::detect_client;
use crate::config::Config;
use crate::engine::{Engine, EngineEvent, next_call_id};
use crate::error::ProtocolError;
use crate::notifications::{NotificationTarget, ProtocolNotification};
use crate::server::ServerManager;
use crate::types::{ClientType, ServerHealth};

/// Atomically-swapped tool snapshot with pre-cached filtered views per client type.
///
/// Built once at `refresh_tools()` time so that `list_tools_for_client()` is O(1).
pub(crate) struct RouterSnapshot {
    /// Full sorted tool list (for clients with no limit).
    pub tools_all: Arc<Vec<Tool>>,
    /// Priority-sorted, truncated to 100 (Windsurf).
    pub tools_windsurf: Arc<Vec<Tool>>,
    /// Priority-sorted, truncated to 128 (VS Code Copilot).
    pub tools_copilot: Arc<Vec<Tool>>,
    /// Tool name → (server name, original tool name) routing table.
    pub routes: HashMap<String, (String, String)>,
}

/// Configuration for token efficiency and tool filtering.
#[derive(Clone, Debug)]
pub struct RouterConfig {
    pub prefix_delimiter: String,
    pub priority_tools: Vec<String>,
    pub disabled_tools: Vec<String>,
    pub tool_description_max_chars: Option<usize>,
    pub tool_search_threshold: usize,
    pub tool_filter_enabled: bool,
    /// Servers with enrichment enabled (annotation inference + title normalization).
    pub enrichment_servers: std::collections::HashSet<String>,
}

impl From<&Config> for RouterConfig {
    fn from(config: &Config) -> Self {
        Self {
            prefix_delimiter: config.prefix_delimiter.clone(),
            priority_tools: config.priority_tools.clone(),
            disabled_tools: config.disabled_tools.clone(),
            tool_description_max_chars: config.tool_description_max_chars,
            tool_search_threshold: config.tool_search_threshold,
            tool_filter_enabled: config.tool_filter_enabled,
            enrichment_servers: config
                .servers
                .iter()
                .filter(|(_, sc)| sc.enrichment)
                .map(|(name, _)| name.clone())
                .collect(),
        }
    }
}

/// Shared tool routing logic used by both stdio (ProxyHandler) and HTTP handlers.
pub struct ToolRouter {
    server_manager: Arc<ServerManager>,
    cache: Arc<ArcSwap<RouterSnapshot>>,
    config: RouterConfig,
    /// Optional event sender for tool call observability.
    event_tx: Option<broadcast::Sender<EngineEvent>>,
    protocol_notification_tx: broadcast::Sender<ProtocolNotification>,
    active_calls: DashMap<u64, ActiveCallRecord>,
    active_call_lookup: DashMap<DownstreamCallKey, u64>,
    upstream_request_lookup: DashMap<UpstreamRequestKey, u64>,
    upstream_progress_lookup: DashMap<UpstreamProgressKey, u64>,
    notification_refresh_in_progress: AtomicBool,
    notification_refresh_pending: AtomicBool,
    /// Weak reference to Engine for session recovery (reconnect on error).
    /// Set after Engine construction via `set_engine()`.
    engine: std::sync::RwLock<Option<Weak<Engine>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DownstreamTransport {
    Stdio,
    Http,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownstreamCallContext {
    pub transport: DownstreamTransport,
    pub client_id: Arc<str>,
    pub request_id: RequestId,
}

impl DownstreamCallContext {
    pub fn stdio(client_id: impl Into<Arc<str>>, request_id: RequestId) -> Self {
        Self {
            transport: DownstreamTransport::Stdio,
            client_id: client_id.into(),
            request_id,
        }
    }

    pub fn http(session_id: impl Into<Arc<str>>, request_id: RequestId) -> Self {
        Self {
            transport: DownstreamTransport::Http,
            client_id: session_id.into(),
            request_id,
        }
    }

    pub fn notification_target(&self) -> NotificationTarget {
        match self.transport {
            DownstreamTransport::Stdio => NotificationTarget::Stdio {
                client_id: Arc::clone(&self.client_id),
            },
            DownstreamTransport::Http => NotificationTarget::Http {
                session_id: Arc::clone(&self.client_id),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DownstreamCallKey {
    transport: DownstreamTransport,
    client_id: Arc<str>,
    request_id: RequestId,
}

impl From<&DownstreamCallContext> for DownstreamCallKey {
    fn from(value: &DownstreamCallContext) -> Self {
        Self {
            transport: value.transport,
            client_id: Arc::clone(&value.client_id),
            request_id: value.request_id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct UpstreamRequestKey {
    server_id: String,
    request_id: RequestId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct UpstreamProgressKey {
    server_id: String,
    progress_token: ProgressToken,
}

#[derive(Clone, Debug)]
struct ActiveCallRecord {
    downstream: DownstreamCallContext,
    upstream_server_id: String,
    upstream_request_id: Option<RequestId>,
    progress_token: Option<ProgressToken>,
}

impl ToolRouter {
    pub fn new(server_manager: Arc<ServerManager>, config: RouterConfig) -> Self {
        let (protocol_notification_tx, _) = broadcast::channel(128);
        Self {
            server_manager,
            cache: Arc::new(ArcSwap::from_pointee(RouterSnapshot {
                routes: HashMap::new(),
                tools_all: Arc::new(Vec::new()),
                tools_windsurf: Arc::new(Vec::new()),
                tools_copilot: Arc::new(Vec::new()),
            })),
            config,
            event_tx: None,
            protocol_notification_tx,
            active_calls: DashMap::new(),
            active_call_lookup: DashMap::new(),
            upstream_request_lookup: DashMap::new(),
            upstream_progress_lookup: DashMap::new(),
            notification_refresh_in_progress: AtomicBool::new(false),
            notification_refresh_pending: AtomicBool::new(false),
            engine: std::sync::RwLock::new(None),
        }
    }

    /// Set the event sender for tool call observability.
    pub fn with_event_tx(mut self, tx: broadcast::Sender<EngineEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<ProtocolNotification> {
        self.protocol_notification_tx.subscribe()
    }

    pub fn publish_protocol_notification(&self, notification: ProtocolNotification) {
        let _ = self.protocol_notification_tx.send(notification);
    }

    pub fn schedule_tool_list_changed_refresh(self: &Arc<Self>) {
        self.notification_refresh_pending.store(true, Ordering::SeqCst);

        if self
            .notification_refresh_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let router = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                router
                    .notification_refresh_pending
                    .store(false, Ordering::SeqCst);
                router.refresh_tools().await;
                router.publish_protocol_notification(ProtocolNotification::ToolListChanged);

                if router
                    .notification_refresh_pending
                    .swap(false, Ordering::SeqCst)
                {
                    continue;
                }

                router
                    .notification_refresh_in_progress
                    .store(false, Ordering::SeqCst);

                if router
                    .notification_refresh_pending
                    .swap(false, Ordering::SeqCst)
                    && router
                        .notification_refresh_in_progress
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    continue;
                }

                break;
            }
        });
    }

    #[cfg(test)]
    pub(crate) fn active_call_count(&self) -> usize {
        self.active_calls.len()
    }

    fn register_active_call(&self, call_id: u64, record: ActiveCallRecord) {
        self.active_call_lookup
            .insert(DownstreamCallKey::from(&record.downstream), call_id);
        if let Some(request_id) = record.upstream_request_id.clone() {
            self.upstream_request_lookup.insert(
                UpstreamRequestKey {
                    server_id: record.upstream_server_id.clone(),
                    request_id,
                },
                call_id,
            );
        }
        if let Some(progress_token) = record.progress_token.clone() {
            self.upstream_progress_lookup.insert(
                UpstreamProgressKey {
                    server_id: record.upstream_server_id.clone(),
                    progress_token,
                },
                call_id,
            );
        }
        self.active_calls.insert(call_id, record);
    }

    fn attach_upstream_request_id(
        &self,
        call_id: u64,
        server_id: &str,
        request_id: RequestId,
    ) {
        if let Some(mut entry) = self.active_calls.get_mut(&call_id) {
            entry.upstream_request_id = Some(request_id.clone());
        }
        self.upstream_request_lookup.insert(
            UpstreamRequestKey {
                server_id: server_id.to_string(),
                request_id,
            },
            call_id,
        );
    }

    fn remove_active_call(&self, call_id: u64) {
        if let Some((_, record)) = self.active_calls.remove(&call_id) {
            self.active_call_lookup
                .remove(&DownstreamCallKey::from(&record.downstream));
            if let Some(request_id) = record.upstream_request_id {
                self.upstream_request_lookup.remove(&UpstreamRequestKey {
                    server_id: record.upstream_server_id.clone(),
                    request_id,
                });
            }
            if let Some(progress_token) = record.progress_token {
                self.upstream_progress_lookup.remove(&UpstreamProgressKey {
                    server_id: record.upstream_server_id,
                    progress_token,
                });
            }
        }
    }

    pub(crate) fn forward_cancel_from_downstream(
        &self,
        context: &DownstreamCallContext,
        reason: Option<String>,
    ) {
        let Some(call_id) = self
            .active_call_lookup
            .get(&DownstreamCallKey::from(context))
            .map(|entry| *entry)
        else {
            tracing::debug!(
                transport = ?context.transport,
                request_id = ?context.request_id,
                "no active call found for downstream cancellation"
            );
            return;
        };

        let Some(record) = self.active_calls.get(&call_id).map(|entry| entry.clone()) else {
            return;
        };

        let Some(upstream) = self.server_manager.get_upstream(&record.upstream_server_id) else {
            tracing::warn!(
                server = %record.upstream_server_id,
                request_id = ?record.upstream_request_id,
                "upstream missing during cancellation forward"
            );
            return;
        };

        let Some(request_id) = record.upstream_request_id.clone() else {
            tracing::debug!(
                server = %record.upstream_server_id,
                "upstream request id not attached yet for downstream cancellation"
            );
            return;
        };

        let peer = upstream.client.peer().clone();
        tokio::spawn(async move {
            if let Err(error) = peer.notify_cancelled(CancelledNotificationParam {
                request_id,
                reason,
            }).await {
                tracing::warn!(error = %error, "failed to forward downstream cancellation upstream");
            }
        });
    }

    pub(crate) fn route_upstream_progress(
        &self,
        server_id: &str,
        params: ProgressNotificationParam,
    ) {
        let key = UpstreamProgressKey {
            server_id: server_id.to_string(),
            progress_token: params.progress_token.clone(),
        };
        let Some(call_id) = self.upstream_progress_lookup.get(&key).map(|entry| *entry) else {
            tracing::debug!(
                server = %server_id,
                progress_token = ?params.progress_token,
                "no active call found for upstream progress"
            );
            return;
        };

        let Some(record) = self.active_calls.get(&call_id).map(|entry| entry.clone()) else {
            return;
        };

        self.publish_protocol_notification(ProtocolNotification::Progress {
            target: record.downstream.notification_target(),
            params,
        });
    }

    pub(crate) fn route_upstream_cancelled(
        &self,
        server_id: &str,
        params: CancelledNotificationParam,
    ) {
        let key = UpstreamRequestKey {
            server_id: server_id.to_string(),
            request_id: params.request_id.clone(),
        };
        let Some(call_id) = self.upstream_request_lookup.get(&key).map(|entry| *entry) else {
            tracing::debug!(
                server = %server_id,
                request_id = ?params.request_id,
                "no active call found for upstream cancellation"
            );
            return;
        };

        let Some(record) = self.active_calls.get(&call_id).map(|entry| entry.clone()) else {
            return;
        };

        self.publish_protocol_notification(ProtocolNotification::Cancelled {
            target: record.downstream.notification_target(),
            params,
        });
    }

    /// Set the Engine reference for session recovery.
    pub fn set_engine(&self, engine: Weak<Engine>) {
        let mut guard = self
            .engine
            .write()
            .expect("engine RwLock poisoned — prior panic");
        *guard = Some(engine);
    }

    fn upgrade_engine(&self) -> Option<Arc<Engine>> {
        self.engine
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().and_then(|weak| weak.upgrade()))
    }

    async fn reconnect_server_now(&self, server_id: &str) -> Result<(), anyhow::Error> {
        let engine = self
            .upgrade_engine()
            .ok_or_else(|| anyhow::anyhow!("engine reference unavailable"))?;
        engine.reconnect_server(server_id).await
    }

    fn reconnect_server_in_background(&self, server_id: String) {
        let Some(engine) = self.upgrade_engine() else {
            tracing::warn!(
                server = %server_id,
                "no engine reference available for background reconnect"
            );
            return;
        };

        tokio::spawn(async move {
            if let Err(reconnect_err) = engine.reconnect_server(&server_id).await {
                tracing::warn!(
                    server = %server_id,
                    error = %reconnect_err,
                    "background reconnect after timeout failed"
                );
            } else {
                tracing::info!(
                    server = %server_id,
                    "background reconnect after timeout succeeded"
                );
            }
        });
    }

    /// Refresh the merged tool list and routing table from all upstream servers.
    ///
    /// Builds the full sorted list plus pre-cached filtered views for each
    /// known client tool limit (Windsurf: 100, Copilot: 128). All views are
    /// swapped atomically to prevent torn reads.
    pub async fn refresh_tools(&self) {
        let upstream_tools = self.server_manager.get_tools().await;

        // ── Pass 1: classify, sanitize, and try keyword stripping ──
        // Each entry: (server_name, tool, prefix, stripped_name, full_name, matched_keyword)
        struct Classified {
            server_name: String,
            tool: Tool,
            prefix: String,
            stripped_name: String,
            full_name: String,
            has_strip_keywords: bool,
        }

        let mut classified: Vec<Classified> = Vec::new();

        for (server_name, tool) in upstream_tools {
            let mut exposed_name = tool.name.to_string();

            // 1. Apply manual renames if any
            if let Some(upstream) = self.server_manager.get_upstream(&server_name) {
                if let Some(new_name) = upstream.config.tool_renames.get(&exposed_name) {
                    exposed_name = new_name.clone();
                }
            }

            // 2. Sanitize to snake_case (hyphens, camelCase, dots -> snake_case)
            let sanitized = crate::tool_naming::sanitize_tool_name(&exposed_name);

            // 3. Determine prefix and tool name via rules or server name
            let tool_group_rules: Option<Vec<crate::config::ToolGroupRule>> = self
                .server_manager
                .get_upstream(&server_name)
                .and_then(|u| {
                    if !u.config.tool_groups.is_empty() {
                        Some(u.config.tool_groups.clone())
                    } else if server_name == "workspace" {
                        Some(crate::tool_naming::default_workspace_rules())
                    } else {
                        None
                    }
                });

            let (prefix, full_name, stripped_name, has_strip_keywords) =
                if let Some(ref rules) = tool_group_rules {
                    match crate::tool_naming::classify_with_rules(&sanitized, rules) {
                        Some(result) => {
                            let stripped = crate::tool_naming::strip_keywords(
                                &result.name,
                                &result.strip_keywords,
                            );
                            let has_strip = !result.strip_keywords.is_empty();
                            (result.prefix, result.name, stripped, has_strip)
                        }
                        None => {
                            let prefix = crate::tool_naming::format_server_prefix(&server_name);
                            (prefix, sanitized.clone(), sanitized.clone(), false)
                        }
                    }
                } else {
                    let prefix = crate::tool_naming::format_server_prefix(&server_name);
                    let mut name = sanitized.clone();

                    // Dedup: strip server_name prefix/suffix if redundant
                    if name.starts_with(&server_name) && name.len() > server_name.len() {
                        let rest = &name[server_name.len()..];
                        if rest.starts_with('_') || rest.starts_with('-') {
                            name = rest[1..].to_string();
                        }
                    }
                    if name.ends_with(&server_name) && name.len() > server_name.len() {
                        let prefix_len = name.len() - server_name.len();
                        let rest = &name[..prefix_len];
                        if rest.ends_with('_') || rest.ends_with('-') {
                            name = rest[..rest.len() - 1].to_string();
                        }
                    }

                    (prefix, name.clone(), name, false)
                };

            classified.push(Classified {
                server_name,
                tool,
                prefix,
                stripped_name,
                full_name,
                has_strip_keywords,
            });
        }

        // ── Pass 2: detect collisions in stripped wire names ──
        // Count how many tools map to each (prefix, stripped_name) pair.
        let mut wire_name_counts: HashMap<String, usize> = HashMap::new();
        for c in &classified {
            let wire = crate::tool_naming::build_wire_name(
                &c.prefix,
                &c.stripped_name,
                &self.config.prefix_delimiter,
            );
            *wire_name_counts.entry(wire).or_insert(0) += 1;
        }

        // ── Pass 3: build final tools with collision-safe names ──
        let mut routes = HashMap::new();
        let mut tools = Vec::new();

        for c in classified {
            let stripped_wire = crate::tool_naming::build_wire_name(
                &c.prefix,
                &c.stripped_name,
                &self.config.prefix_delimiter,
            );

            // Use stripped name unless it would collide, then fall back to full name
            let use_stripped = wire_name_counts.get(&stripped_wire).copied().unwrap_or(1) == 1;

            let final_name = if use_stripped {
                c.stripped_name.clone()
            } else {
                c.full_name.clone()
            };

            let prefixed_name = crate::tool_naming::build_wire_name(
                &c.prefix,
                &final_name,
                &self.config.prefix_delimiter,
            );

            if is_disabled_tool(&self.config.disabled_tools, &prefixed_name) {
                continue;
            }

            routes.insert(
                prefixed_name.clone(),
                (c.server_name.clone(), c.tool.name.to_string()),
            );

            let mut prefixed_tool = c.tool.clone();

            // Enrich BEFORE setting wire name (so get_* patterns match)
            if self.config.enrichment_servers.contains(&c.server_name) {
                prefixed_tool.name = Cow::Owned(final_name.clone());
                crate::enrichment::enrich_tool(&mut prefixed_tool);
            }

            // Title always uses stripped name (display-only, no collision risk)
            let title_name = if c.has_strip_keywords {
                crate::tool_naming::generate_title(&c.prefix, &c.stripped_name)
            } else {
                crate::tool_naming::generate_title(&c.prefix, &final_name)
            };

            // Set wire name and title
            prefixed_tool.name = Cow::Owned(prefixed_name);
            prefixed_tool.title = Some(title_name);

            // Strip optional fields for token efficiency
            strip_optional_fields(&mut prefixed_tool, self.config.tool_description_max_chars);

            tools.push(prefixed_tool);
        }

        // Sort: priority tools first, then alphabetical
        let priority = &self.config.priority_tools;
        tools.sort_unstable_by(|a, b| priority_sort(a, b, priority));

        // Add search_tools meta-tool if tool count exceeds threshold
        if tools.len() >= self.config.tool_search_threshold {
            let meta_tool = build_search_tools_meta_tool();
            routes.insert(
                meta_tool.name.to_string(),
                ("__plug_internal__".to_string(), meta_tool.name.to_string()),
            );
            // Insert at position 0 so it's always visible
            tools.insert(0, meta_tool);
        }

        tracing::info!(
            tool_count = tools.len(),
            server_count = routes
                .values()
                .map(|(s, _)| s)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            "refreshed tool cache"
        );

        // Build pre-cached filtered views
        let tools_windsurf = Arc::new(tools.iter().take(100).cloned().collect());
        let tools_copilot = Arc::new(tools.iter().take(128).cloned().collect());
        let tools_all = Arc::new(tools);

        let tool_count = tools_all.len();

        self.cache.store(Arc::new(RouterSnapshot {
            routes,
            tools_all,
            tools_windsurf,
            tools_copilot,
        }));

        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(EngineEvent::ToolCacheRefreshed { tool_count });
        }
    }

    /// Get the current list of tools (zero-copy via Arc). Returns all tools.
    pub fn list_tools(&self) -> Arc<Vec<Tool>> {
        Arc::clone(&self.cache.load().tools_all)
    }

    /// List all tools with their source server IDs.
    pub fn list_all_tools(&self) -> Vec<(String, Tool)> {
        let snapshot = self.cache.load();
        let mut result = Vec::new();
        for tool in snapshot.tools_all.iter() {
            let server_id = snapshot
                .routes
                .get(tool.name.as_ref())
                .map(|(s, _)| s.clone())
                .unwrap_or_else(|| "unknown".to_string());

            // Return tool with wire name intact (CLI handles display)
            result.push((server_id, tool.clone()));
        }
        result
    }

    /// Total number of tools in the unfiltered cache.
    pub fn tool_count(&self) -> usize {
        self.cache.load().tools_all.len()
    }

    /// Get tools filtered for a specific client type. O(1) — single Arc::clone.
    pub fn list_tools_for_client(&self, client_type: ClientType) -> Arc<Vec<Tool>> {
        if !self.config.tool_filter_enabled {
            return self.list_tools();
        }
        let snapshot = self.cache.load();
        match client_type {
            ClientType::Windsurf => Arc::clone(&snapshot.tools_windsurf),
            ClientType::VSCodeCopilot => Arc::clone(&snapshot.tools_copilot),
            _ => Arc::clone(&snapshot.tools_all),
        }
    }

    /// Call a tool by its prefixed name, routing to the correct upstream server.
    ///
    /// Applies: health gate → circuit breaker → semaphore → timeout.
    /// On session/transport errors, attempts one automatic reconnect + retry.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult, McpError> {
        self.call_tool_with_context(tool_name, arguments, None, None).await
    }

    pub async fn call_tool_with_context(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        progress_token: Option<ProgressToken>,
        downstream: Option<DownstreamCallContext>,
    ) -> Result<CallToolResult, McpError> {
        self.call_tool_inner(tool_name, arguments, progress_token, downstream, false)
            .await
    }

    /// Inner tool call implementation with retry support.
    /// `is_retry` prevents infinite recursion — max 1 reconnect per call.
    /// Uses `Box::pin` for the recursive call to avoid infinitely-sized future.
    fn call_tool_inner<'a>(
        &'a self,
        tool_name: &'a str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        progress_token: Option<ProgressToken>,
        downstream: Option<DownstreamCallContext>,
        is_retry: bool,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CallToolResult, McpError>> + Send + 'a>,
    > {
        Box::pin(async move {
            // Intercept search_tools meta-tool (case-insensitive for LLM casing drift)
            if tool_name.eq_ignore_ascii_case("plug__search_tools") {
                return self.handle_search_tools(arguments.clone());
            }

            // Look up the server and original name for this exposed tool name
            let cache = self.cache.load();
            let (server_id, original_name) = cache
                .routes
                .get(tool_name)
                .or_else(|| {
                    // Case-insensitive fallback for LLM casing drift
                    // (e.g. "slack__search_messages" → "Slack__search_messages")
                    cache
                        .routes
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(tool_name))
                        .map(|(_, v)| v)
                })
                .ok_or_else(|| {
                    McpError::from(ProtocolError::ToolNotFound {
                        tool_name: tool_name.to_string(),
                    })
                })?;

            let server_id = server_id.clone();
            let original_name = original_name.to_string();
            drop(cache);

            // Health gate — reject calls to Failed servers
            let health_ok = self
                .server_manager
                .health
                .get(&server_id)
                .map(|h| h.health != ServerHealth::Failed)
                .unwrap_or(true);
            if !health_ok {
                return Err(McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                }));
            }

            // Circuit breaker gate
            if let Some(cb) = self.server_manager.circuit_breakers.get(&server_id) {
                cb.call_allowed().map_err(|_: CircuitBreakerError| {
                    McpError::from(ProtocolError::ServerUnavailable {
                        server_id: server_id.clone(),
                    })
                })?;
            }

            let semaphore_timeout = self
                .server_manager
                .get_upstream(&server_id)
                .map(|upstream| Duration::from_secs(upstream.config.call_timeout_secs))
                .unwrap_or(Duration::from_secs(30));

            // Acquire concurrency semaphore
            let permit = if let Some(sem) = self.server_manager.semaphores.get(&server_id) {
                Some(
                    tokio::time::timeout(semaphore_timeout, sem.clone().acquire_owned())
                    .await
                    .map_err(|_| {
                        McpError::from(ProtocolError::ServerBusy {
                            server_id: server_id.clone(),
                        })
                    })?
                    .map_err(|_| {
                        McpError::from(ProtocolError::ServerUnavailable {
                            server_id: server_id.clone(),
                        })
                    })?,
                )
            } else {
                None
            };

            // Get the upstream server
            let upstream = self
                .server_manager
                .get_upstream(&server_id)
                .ok_or_else(|| {
                    McpError::from(ProtocolError::ServerUnavailable {
                        server_id: server_id.clone(),
                    })
                })?;

            let timeout_duration = Duration::from_secs(upstream.config.call_timeout_secs);
            let transport_type = upstream.config.transport.clone();
            let peer = upstream.client.peer().clone();
            drop(upstream); // Release Arc early

            // Build the upstream call with the original (unprefixed) tool name
            let mut upstream_params = CallToolRequestParams::new(original_name.clone());
            if let Some(ref args) = arguments {
                upstream_params = upstream_params.with_arguments(args.clone());
            }
            if let Some(token) = progress_token.clone() {
                upstream_params.set_progress_token(token);
            }
            let downstream_progress_token = upstream_params.progress_token();

            let request = ClientRequest::CallToolRequest(CallToolRequest::new(upstream_params));
            let options = PeerRequestOptions {
                timeout: Some(timeout_duration),
                meta: downstream_progress_token.clone().map(Meta::with_progress_token),
            };

            let call_id = next_call_id();
            let server_id_arc = Arc::<str>::from(server_id.as_str());
            let tool_name_arc = Arc::<str>::from(original_name.as_str());
            if let Some(call_context) = downstream.clone() {
                self.register_active_call(
                    call_id,
                    ActiveCallRecord {
                        downstream: call_context,
                        upstream_server_id: server_id.clone(),
                        upstream_request_id: None,
                        progress_token: downstream_progress_token.clone(),
                    },
                );
            }
            let request_handle = peer
                .send_cancellable_request(request, options)
                .await
                .map_err(|error| {
                    self.remove_active_call(call_id);
                    match error {
                        rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                        other => McpError::internal_error(other.to_string(), None),
                    }
                })?;

            self.attach_upstream_request_id(call_id, &server_id, request_handle.id.clone());
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(EngineEvent::ToolCallStarted {
                    call_id,
                    server_id: Arc::clone(&server_id_arc),
                    tool_name: Arc::clone(&tool_name_arc),
                });
            }

            let call_start = std::time::Instant::now();

            // Execute with timeout via rmcp RequestHandle so we retain upstream request ID.
            let result = request_handle.await_response().await;

            // Drop semaphore permit
            drop(permit);

            let duration_ms = call_start.elapsed().as_millis() as u64;

            // Record circuit breaker outcome
            let cb = self.server_manager.circuit_breakers.get(&server_id);

            match result {
                Ok(ServerResult::CallToolResult(response)) => {
                    if let Some(cb) = &cb {
                        cb.on_success();
                    }
                    self.remove_active_call(call_id);
                    if let Some(ref tx) = self.event_tx {
                        let _ = tx.send(EngineEvent::ToolCallCompleted {
                            call_id,
                            server_id: Arc::clone(&server_id_arc),
                            tool_name: Arc::clone(&tool_name_arc),
                            duration_ms,
                            success: true,
                        });
                    }
                    Ok(response)
                }
                Err(e) if is_session_error(&e) && !is_retry => {
                    // Session/transport error on first attempt — try to reconnect and retry
                    tracing::warn!(
                        server = %server_id,
                        tool = %original_name,
                        error = %e,
                        "session error detected, attempting reconnect"
                    );
                    self.remove_active_call(call_id);
                    if let Some(ref tx) = self.event_tx {
                        let _ = tx.send(EngineEvent::ToolCallCompleted {
                            call_id,
                            server_id: Arc::clone(&server_id_arc),
                            tool_name: Arc::clone(&tool_name_arc),
                            duration_ms,
                            success: false,
                        });
                    }

                    match self.reconnect_server_now(&server_id).await {
                        Ok(()) => {
                            tracing::info!(server = %server_id, "reconnected, retrying tool call");
                        }
                        Err(reconnect_err) => {
                            tracing::error!(
                                server = %server_id,
                                error = %reconnect_err,
                                "reconnect failed, returning original error"
                            );
                            if let Some(cb) = &cb {
                                cb.on_failure();
                            }
                            return Err(McpError::internal_error(e.to_string(), None));
                        }
                    }

                    // Retry the tool call exactly once
                    self.call_tool_inner(tool_name, arguments, progress_token, downstream, true)
                        .await
                }
                Err(rmcp::service::ServiceError::Timeout { timeout }) => {
                    tracing::error!(
                        server = %server_id,
                        tool = %original_name,
                        timeout_secs = timeout.as_secs(),
                        "upstream tool call timed out"
                    );
                    self.remove_active_call(call_id);
                    if let Some(ref tx) = self.event_tx {
                        let _ = tx.send(EngineEvent::ToolCallCompleted {
                            call_id,
                            server_id: Arc::clone(&server_id_arc),
                            tool_name: Arc::clone(&tool_name_arc),
                            duration_ms,
                            success: false,
                        });
                    }

                    if matches!(transport_type, crate::config::TransportType::Stdio) {
                        self.reconnect_server_in_background(server_id.clone());
                    }

                    Err(McpError::from(ProtocolError::Timeout { duration: timeout }))
                }
                Err(e) => {
                    tracing::error!(
                        server = %server_id,
                        tool = %original_name,
                        error = %e,
                        "upstream tool call failed"
                    );
                    if let Some(cb) = &cb {
                        cb.on_failure();
                    }
                    self.remove_active_call(call_id);
                    if let Some(ref tx) = self.event_tx {
                        let _ = tx.send(EngineEvent::ToolCallCompleted {
                            call_id,
                            server_id: Arc::clone(&server_id_arc),
                            tool_name: Arc::clone(&tool_name_arc),
                            duration_ms,
                            success: false,
                        });
                    }
                    match e {
                        rmcp::service::ServiceError::McpError(mcp_err) => Err(mcp_err),
                        other => Err(McpError::internal_error(other.to_string(), None)),
                    }
                }
                Ok(other) => {
                    self.remove_active_call(call_id);
                    Err(McpError::internal_error(
                        format!("unexpected response type from upstream tool call: {other:?}"),
                        None,
                    ))
                }
            }
        })
    }

    /// Handle the `plug__search_tools` meta-tool call.
    fn handle_search_tools(
        &self,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult, McpError> {
        let query = arguments
            .as_ref()
            .and_then(|args| args.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        if query.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "Please provide a search query.",
            )]));
        }

        let snapshot = self.cache.load();
        let matches: Vec<&Tool> = snapshot
            .tools_all
            .iter()
            .filter(|tool| {
                let name = tool.name.to_lowercase();
                let desc = tool.description.as_deref().unwrap_or("").to_lowercase();
                name.contains(&query) || desc.contains(&query)
            })
            .take(10)
            .collect();

        if matches.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No tools found matching '{query}'."
            ))]));
        }

        let mut result_text = format!("Found {} tool(s) matching '{query}':\n\n", matches.len());
        for tool in &matches {
            let server = snapshot
                .routes
                .get(tool.name.as_ref())
                .map(|(s, _)| s.as_str())
                .unwrap_or("unknown");
            result_text.push_str(&format!("- **{}** (server: {})\n", tool.name, server));
            if let Some(ref desc) = tool.description {
                result_text.push_str(&format!("  {}\n", desc));
            }
            result_text.push('\n');
        }

        Ok(CallToolResult::success(vec![Content::text(result_text)]))
    }

    /// Get a reference to the underlying ServerManager.
    pub fn server_manager(&self) -> &Arc<ServerManager> {
        &self.server_manager
    }
}

// ---------------------------------------------------------------------------
// Session error classification
// ---------------------------------------------------------------------------

/// Classify whether a ServiceError indicates a session/transport failure
/// that should trigger automatic reconnection.
///
/// rmcp v1.0.0 error mapping:
/// - HTTP 404 "Session not found" → `ServiceError::TransportSend(DynamicTransportError)`
///   with formatted string containing "404" and "session not found"
/// - Connection refused/reset → `ServiceError::TransportSend` or `TransportClosed`
/// - JSON-RPC application errors → `ServiceError::McpError` (do NOT reconnect)
/// - Timeouts → `ServiceError::Timeout` (do NOT reconnect)
fn is_session_error(e: &rmcp::service::ServiceError) -> bool {
    use rmcp::service::ServiceError;
    match e {
        // Transport send failures (HTTP 404, connection refused, etc.)
        ServiceError::TransportSend(dyn_err) => {
            let msg = dyn_err.to_string().to_lowercase();
            msg.contains("404")
                || msg.contains("session not found")
                || msg.contains("connection refused")
                || msg.contains("connection reset")
                || msg.contains("broken pipe")
        }
        // Transport closed = connection dropped (server crashed/restarted)
        ServiceError::TransportClosed => true,
        // All other variants: do NOT reconnect
        // McpError = application-level error (tool errors, invalid params)
        // Timeout = slow tool, not a server failure
        // Cancelled = task cancelled
        // UnexpectedResponse = wrong response type
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Strip optional fields from a tool for token efficiency.
///
/// Removes `outputSchema`. Keeps `title` (human-friendly display name) and
/// `annotations` (readOnlyHint, etc. — useful for client parallelization).
/// `inputSchema` is REQUIRED per MCP spec (ADR-003) — never stripped.
fn strip_optional_fields(tool: &mut Tool, max_desc_chars: Option<usize>) {
    // Keep title (human-friendly display name) and annotations (readOnlyHint etc.)
    tool.output_schema = None;
    // Note: tool.icons doesn't exist on rmcp Tool; skip if not present

    if let Some(ref desc) = tool.description {
        let sanitized = sanitize_description(desc);
        let final_desc = if let Some(max) = max_desc_chars {
            sanitized.chars().take(max).collect()
        } else {
            sanitized
        };
        tool.description = Some(Cow::Owned(final_desc));
    }
}

fn sanitize_description(desc: &str) -> String {
    desc.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\r' | '\t'))
        .collect()
}

/// Sort comparator: priority tools first (by priority_tools index), then alphabetical.
fn priority_sort(a: &Tool, b: &Tool, priority_tools: &[String]) -> std::cmp::Ordering {
    let a_priority = priority_tools
        .iter()
        .position(|p| a.name.contains(p.as_str()));
    let b_priority = priority_tools
        .iter()
        .position(|p| b.name.contains(p.as_str()));

    match (a_priority, b_priority) {
        (Some(a_idx), Some(b_idx)) => a_idx.cmp(&b_idx),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.as_ref().cmp(b.name.as_ref()),
    }
}

fn is_disabled_tool(patterns: &[String], tool_name: &str) -> bool {
    let tool_name = tool_name.to_ascii_lowercase();
    patterns
        .iter()
        .any(|pattern| wildcard_match(&pattern.to_ascii_lowercase(), &tool_name))
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }

    let mut remainder = text;
    let mut first = true;

    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if first && !pattern.starts_with('*') {
            if !remainder.starts_with(part) {
                return false;
            }
            remainder = &remainder[part.len()..];
            first = false;
            continue;
        }

        if index == parts.len() - 1 && !pattern.ends_with('*') {
            return remainder.ends_with(part);
        }

        if let Some(found) = remainder.find(part) {
            remainder = &remainder[found + part.len()..];
            first = false;
        } else {
            return false;
        }
    }

    true
}

/// Build the search_tools meta-tool definition.
fn build_search_tools_meta_tool() -> Tool {
    Tool::new(
        Cow::Borrowed("plug__search_tools"),
        Cow::Borrowed(
            "Search for tools by name or description. Returns matching tools with full schemas.",
        ),
        Arc::new(
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for tool name or description"
                    }
                },
                "required": ["query"]
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
    )
}

/// MCP proxy handler that aggregates tools from multiple upstream servers
/// and routes tool calls to the correct upstream. Used for stdio transport.
pub struct ProxyHandler {
    router: Arc<ToolRouter>,
    client_type: std::sync::RwLock<ClientType>,
    client_id: Arc<str>,
    notification_task_started: AtomicBool,
}

impl ProxyHandler {
    pub fn new(server_manager: Arc<ServerManager>, config: RouterConfig) -> Self {
        Self {
            router: Arc::new(ToolRouter::new(server_manager, config)),
            client_type: std::sync::RwLock::new(ClientType::Unknown),
            client_id: Arc::from(uuid::Uuid::new_v4().to_string()),
            notification_task_started: AtomicBool::new(false),
        }
    }

    /// Create a ProxyHandler from an existing shared ToolRouter.
    pub fn from_router(router: Arc<ToolRouter>) -> Self {
        Self {
            router,
            client_type: std::sync::RwLock::new(ClientType::Unknown),
            client_id: Arc::from(uuid::Uuid::new_v4().to_string()),
            notification_task_started: AtomicBool::new(false),
        }
    }

    /// Refresh the merged tool list and routing table from all upstream servers.
    pub async fn refresh_tools(&self) {
        self.router.refresh_tools().await;
    }

    /// Get a reference to the underlying ToolRouter.
    pub fn router(&self) -> &Arc<ToolRouter> {
        &self.router
    }

    #[cfg(test)]
    pub(crate) fn client_id(&self) -> Arc<str> {
        Arc::clone(&self.client_id)
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

            // Store client type for list_tools filtering
            match self.client_type.write() {
                Ok(mut ct) => *ct = client_type,
                Err(e) => tracing::warn!("client_type lock poisoned: {e}"),
            }

            context.peer.set_peer_info(request);

            if self
                .notification_task_started
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                let peer: Peer<RoleServer> = context.peer.clone();
                let client_id = Arc::clone(&self.client_id);
                let mut rx = self.router.subscribe_notifications();
                tokio::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(ProtocolNotification::ToolListChanged) => {
                                if let Err(error) = peer.notify_tool_list_changed().await {
                                    tracing::debug!(
                                        error = %error,
                                        "stopping stdio notification fan-out after peer send failure"
                                    );
                                    break;
                                }
                            }
                            Ok(ProtocolNotification::Progress { target, params }) => {
                                if matches!(
                                    target,
                                    NotificationTarget::Stdio { client_id: target_id }
                                        if target_id == client_id
                                ) && peer.notify_progress(params).await.is_err()
                                {
                                    break;
                                }
                            }
                            Ok(ProtocolNotification::Cancelled { target, params }) => {
                                if matches!(
                                    target,
                                    NotificationTarget::Stdio { client_id: target_id }
                                        if target_id == client_id
                                ) && peer.notify_cancelled(params).await.is_err()
                                {
                                    break;
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "stdio notification fan-out lagged");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
            }

            Ok(self.get_info())
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            let ct = self
                .client_type
                .read()
                .map(|ct| *ct)
                .unwrap_or(ClientType::Unknown);
            let tools = self.router.list_tools_for_client(ct);
            Ok(ListToolsResult::with_all_items((*tools).clone()))
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            let progress_token = request.progress_token();
            self.router
                .call_tool_with_context(
                    request.name.as_ref(),
                    request.arguments,
                    progress_token,
                    Some(DownstreamCallContext::stdio(
                        Arc::clone(&self.client_id),
                        context.id.clone(),
                    )),
                )
                .await
        }
    }

    fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        async move {
            self.router.forward_cancel_from_downstream(
                &DownstreamCallContext::stdio(
                    Arc::clone(&self.client_id),
                    notification.request_id.clone(),
                ),
                notification.reason,
            );
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

    #[test]
    fn get_info_returns_correct_server_info() {
        let sm = Arc::new(ServerManager::new());
        let handler = ProxyHandler::new(sm, test_router_config());
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
        let handler = ProxyHandler::new(sm, test_router_config());
        handler.refresh_tools().await;

        let tools = handler.router().list_tools();
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn tool_router_list_tools_returns_arc() {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, test_router_config());
        router.refresh_tools().await;

        let tools1 = router.list_tools();
        let tools2 = router.list_tools();
        // Both should point to the same allocation (Arc)
        assert!(Arc::ptr_eq(&tools1, &tools2));
    }

    #[test]
    fn priority_sort_orders_correctly() {
        let priority = vec!["important".to_string(), "medium".to_string()];

        let a = Tool::new(
            Cow::Borrowed("server__important_tool"),
            Cow::Borrowed("desc"),
            Arc::new(serde_json::Map::new()),
        );
        let b = Tool::new(
            Cow::Borrowed("server__other_tool"),
            Cow::Borrowed("desc"),
            Arc::new(serde_json::Map::new()),
        );
        let c = Tool::new(
            Cow::Borrowed("server__medium_tool"),
            Cow::Borrowed("desc"),
            Arc::new(serde_json::Map::new()),
        );

        // Priority tool should come before non-priority
        assert_eq!(priority_sort(&a, &b, &priority), std::cmp::Ordering::Less);
        // Non-priority after priority
        assert_eq!(
            priority_sort(&b, &a, &priority),
            std::cmp::Ordering::Greater
        );
        // Higher priority before lower priority
        assert_eq!(priority_sort(&a, &c, &priority), std::cmp::Ordering::Less);
        // Same priority: alphabetical
        assert_eq!(priority_sort(&b, &b, &priority), std::cmp::Ordering::Equal);
    }

    #[test]
    fn disabled_tool_patterns_support_exact_and_wildcard_matches() {
        assert!(is_disabled_tool(
            &["slack__search_messages".into()],
            "Slack__search_messages"
        ));
        assert!(is_disabled_tool(
            &["slack__*".into()],
            "Slack__search_messages"
        ));
        assert!(is_disabled_tool(
            &["*search*".into()],
            "Slack__search_messages"
        ));
        assert!(!is_disabled_tool(
            &["gmail__*".into()],
            "Slack__search_messages"
        ));
    }

    #[test]
    fn strip_optional_fields_removes_fields() {
        let mut tool = Tool::new(
            Cow::Borrowed("test_tool"),
            Cow::Borrowed("A long description that should be truncated if configured"),
            Arc::new(serde_json::Map::new()),
        );
        tool.title = Some("Title".to_string());
        tool.annotations = Some(ToolAnnotations::default());

        strip_optional_fields(&mut tool, Some(10));

        assert!(tool.title.is_some()); // title is now preserved
        assert!(tool.annotations.is_some()); // annotations are now preserved
        assert!(tool.output_schema.is_none());
        // Description should be truncated to 10 chars
        assert_eq!(tool.description.as_deref(), Some("A long des"));
        // inputSchema should be preserved
        // inputSchema preserved — it's an Arc<Map> (always present)
        assert!(!tool.input_schema.is_empty() || tool.input_schema.is_empty());
    }

    #[test]
    fn strip_optional_fields_removes_control_characters_from_description() {
        let mut tool = Tool::new(
            Cow::Borrowed("test_tool"),
            Cow::Borrowed("ok\u{0000}still-ok\tline\nnext"),
            Arc::new(serde_json::Map::new()),
        );

        strip_optional_fields(&mut tool, None);

        assert_eq!(tool.description.as_deref(), Some("okstill-ok\tline\nnext"));
    }

    #[test]
    fn strip_optional_fields_sanitizes_before_truncating() {
        let mut tool = Tool::new(
            Cow::Borrowed("test_tool"),
            Cow::Borrowed("ab\u{0000}cdef"),
            Arc::new(serde_json::Map::new()),
        );

        strip_optional_fields(&mut tool, Some(4));

        assert_eq!(tool.description.as_deref(), Some("abcd"));
    }

    #[test]
    fn list_tools_for_client_returns_correct_counts() {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, test_router_config());

        // Manually set up a snapshot with 150 tools
        let tools: Vec<Tool> = (0..150)
            .map(|i| {
                Tool::new(
                    Cow::Owned(format!("tool_{i:03}")),
                    Cow::Owned(format!("Tool {i}")),
                    Arc::new(serde_json::Map::new()),
                )
            })
            .collect();

        let tools_windsurf = Arc::new(tools.iter().take(100).cloned().collect::<Vec<_>>());
        let tools_copilot = Arc::new(tools.iter().take(128).cloned().collect::<Vec<_>>());
        let tools_all = Arc::new(tools);

        router.cache.store(Arc::new(RouterSnapshot {
            routes: HashMap::new(),
            tools_all,
            tools_windsurf,
            tools_copilot,
        }));

        assert_eq!(
            router.list_tools_for_client(ClientType::Windsurf).len(),
            100
        );
        assert_eq!(
            router
                .list_tools_for_client(ClientType::VSCodeCopilot)
                .len(),
            128
        );
        assert_eq!(
            router.list_tools_for_client(ClientType::ClaudeCode).len(),
            150
        );
        assert_eq!(router.list_tools_for_client(ClientType::Cursor).len(), 150);
    }

    #[test]
    fn search_tools_returns_matches() {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, test_router_config());

        // Set up a snapshot with named tools
        let tools = vec![
            Tool::new(
                Cow::Borrowed("git__commit"),
                Cow::Borrowed("Create a git commit"),
                Arc::new(serde_json::Map::new()),
            ),
            Tool::new(
                Cow::Borrowed("git__push"),
                Cow::Borrowed("Push changes to remote"),
                Arc::new(serde_json::Map::new()),
            ),
            Tool::new(
                Cow::Borrowed("slack__send"),
                Cow::Borrowed("Send a message on Slack"),
                Arc::new(serde_json::Map::new()),
            ),
        ];

        let mut routes = HashMap::new();
        routes.insert(
            "git__commit".to_string(),
            ("git".to_string(), "commit".to_string()),
        );
        routes.insert(
            "git__push".to_string(),
            ("git".to_string(), "push".to_string()),
        );
        routes.insert(
            "slack__send".to_string(),
            ("slack".to_string(), "send".to_string()),
        );

        router.cache.store(Arc::new(RouterSnapshot {
            routes,
            tools_all: Arc::new(tools),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
        }));

        // Search by name
        let mut args = serde_json::Map::new();
        args.insert("query".to_string(), serde_json::json!("git"));
        let result = router.handle_search_tools(Some(args)).unwrap();
        let text = format!("{result:?}");
        assert!(text.contains("git__commit"));
        assert!(text.contains("git__push"));

        // Search by description
        let mut args = serde_json::Map::new();
        args.insert("query".to_string(), serde_json::json!("slack"));
        let result = router.handle_search_tools(Some(args)).unwrap();
        let text = format!("{result:?}");
        assert!(text.contains("slack__send"));

        // No matches
        let mut args = serde_json::Map::new();
        args.insert("query".to_string(), serde_json::json!("nonexistent"));
        let result = router.handle_search_tools(Some(args)).unwrap();
        let text = format!("{result:?}");
        assert!(text.contains("No tools found"));
    }

    // -----------------------------------------------------------------------
    // Session error classification tests
    // -----------------------------------------------------------------------

    #[test]
    fn is_session_error_transport_closed() {
        use rmcp::service::ServiceError;
        assert!(is_session_error(&ServiceError::TransportClosed));
    }

    #[test]
    fn is_session_error_mcp_error_not_session() {
        use rmcp::service::ServiceError;
        // Application-level MCP error should NOT trigger reconnect
        let mcp_err = McpError::internal_error("tool failed".to_string(), None);
        assert!(!is_session_error(&ServiceError::McpError(mcp_err)));
    }

    #[test]
    fn is_session_error_timeout_not_session() {
        use rmcp::service::ServiceError;
        // Timeouts should NOT trigger reconnect
        assert!(!is_session_error(&ServiceError::Timeout {
            timeout: Duration::from_secs(30),
        }));
    }

    #[test]
    fn is_session_error_cancelled_not_session() {
        use rmcp::service::ServiceError;
        assert!(!is_session_error(&ServiceError::Cancelled {
            reason: Some("test".to_string()),
        }));
    }

    #[test]
    fn is_session_error_unexpected_response_not_session() {
        use rmcp::service::ServiceError;
        assert!(!is_session_error(&ServiceError::UnexpectedResponse));
    }

    #[test]
    fn case_insensitive_route_lookup() {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, test_router_config());

        let mut routes = HashMap::new();
        routes.insert(
            "Slack__search_messages".to_string(),
            (
                "slack".to_string(),
                "conversations_search_messages".to_string(),
            ),
        );

        router.cache.store(Arc::new(RouterSnapshot {
            routes,
            tools_all: Arc::new(Vec::new()),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
        }));

        let snapshot = router.cache.load();
        // Exact match works
        assert!(snapshot.routes.contains_key("Slack__search_messages"));
        // Case-insensitive fallback works
        let lower = "slack__search_messages";
        let found = snapshot.routes.get(lower).or_else(|| {
            snapshot
                .routes
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(lower))
                .map(|(_, v)| v)
        });
        assert!(found.is_some());
        assert_eq!(found.unwrap().0, "slack");
        assert_eq!(found.unwrap().1, "conversations_search_messages");
    }

    #[tokio::test(start_paused = true)]
    async fn call_tool_times_out_waiting_for_semaphore() {
        let server_manager = Arc::new(ServerManager::new());
        let router = ToolRouter::new(server_manager.clone(), test_router_config());

        server_manager.semaphores.insert(
            "busy-server".to_string(),
            Arc::new(tokio::sync::Semaphore::new(0)),
        );

        let mut routes = HashMap::new();
        routes.insert(
            "Busy__tool".to_string(),
            ("busy-server".to_string(), "tool".to_string()),
        );
        router.cache.store(Arc::new(RouterSnapshot {
            routes,
            tools_all: Arc::new(Vec::new()),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
        }));

        let call = router.call_tool("Busy__tool", None);
        tokio::pin!(call);

        tokio::time::advance(Duration::from_secs(31)).await;

        let err = call.await.unwrap_err();
        assert!(
            err.message.contains("server overloaded"),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn route_upstream_progress_publishes_targeted_notification() {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, test_router_config());
        let mut rx = router.subscribe_notifications();
        let progress_token = ProgressToken(NumberOrString::String(Arc::from("progress-1")));

        router.register_active_call(
            42,
            ActiveCallRecord {
                downstream: DownstreamCallContext::stdio(
                    Arc::from("client-1"),
                    RequestId::from(NumberOrString::Number(1)),
                ),
                upstream_server_id: "upstream".to_string(),
                upstream_request_id: None,
                progress_token: Some(progress_token.clone()),
            },
        );

        router.route_upstream_progress(
            "upstream",
            ProgressNotificationParam::new(progress_token.clone(), 0.5)
                .with_message("halfway"),
        );

        let notification = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("notification arrives")
            .expect("notification channel open");

        match notification {
            ProtocolNotification::Progress { target, params } => {
                assert_eq!(
                    target,
                    NotificationTarget::Stdio {
                        client_id: Arc::from("client-1"),
                    }
                );
                assert_eq!(params.progress_token, progress_token);
                assert_eq!(params.message.as_deref(), Some("halfway"));
            }
            other => panic!("unexpected notification: {other:?}"),
        }
    }
}
