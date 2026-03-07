use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::RequestParamsMeta;
use rmcp::model::*;
use rmcp::service::{NotificationContext, Peer, PeerRequestOptions, RequestContext, RoleServer};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

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
    /// Meta-tool-only list exposed when meta-tool mode is enabled.
    pub meta_tools_all: Arc<Vec<Tool>>,
    /// Priority-sorted, truncated to 100 (Windsurf).
    pub tools_windsurf: Arc<Vec<Tool>>,
    /// Priority-sorted, truncated to 128 (VS Code Copilot).
    pub tools_copilot: Arc<Vec<Tool>>,
    /// Tool name → (server name, original tool name) routing table.
    pub routes: HashMap<String, (String, String)>,
    /// Routed resources for downstream list responses.
    pub resources_all: Arc<Vec<Resource>>,
    /// Routed resource templates for downstream list responses.
    pub resource_templates_all: Arc<Vec<ResourceTemplate>>,
    /// Routed prompts for downstream list responses.
    pub prompts_all: Arc<Vec<Prompt>>,
    /// Canonical resource URI → upstream server.
    pub resource_routes: HashMap<String, String>,
    /// Routed prompt name → (server name, original prompt name).
    pub prompt_routes: HashMap<String, (String, String)>,
    /// Fingerprints for routed tool definitions to detect material drift.
    pub tool_definition_fingerprints: HashMap<String, u64>,
}

/// Configuration for token efficiency and tool filtering.
#[derive(Clone, Debug)]
pub struct RouterConfig {
    pub prefix_delimiter: String,
    pub priority_tools: Vec<String>,
    pub disabled_tools: Vec<String>,
    pub tool_description_max_chars: Option<usize>,
    pub tool_search_threshold: usize,
    pub meta_tool_mode: bool,
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
            meta_tool_mode: config.meta_tool_mode,
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
    /// Resource subscription registry: upstream URI → set of downstream subscribers.
    resource_subscriptions: DashMap<String, HashSet<NotificationTarget>>,
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
                meta_tools_all: Arc::new(build_meta_tools()),
                tools_windsurf: Arc::new(Vec::new()),
                tools_copilot: Arc::new(Vec::new()),
                resources_all: Arc::new(Vec::new()),
                resource_templates_all: Arc::new(Vec::new()),
                prompts_all: Arc::new(Vec::new()),
                resource_routes: HashMap::new(),
                prompt_routes: HashMap::new(),
                tool_definition_fingerprints: HashMap::new(),
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
            resource_subscriptions: DashMap::new(),
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
        self.notification_refresh_pending
            .store(true, Ordering::SeqCst);

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

    #[cfg(test)]
    pub(crate) fn replace_snapshot(&self, snapshot: RouterSnapshot) {
        self.cache.store(Arc::new(snapshot));
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

    fn attach_upstream_request_id(&self, call_id: u64, server_id: &str, request_id: RequestId) {
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
            if let Err(error) = peer
                .notify_cancelled(CancelledNotificationParam { request_id, reason })
                .await
            {
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

    /// Subscribe a downstream client to resource updates for a given URI.
    ///
    /// On the first subscriber for a URI, forwards the subscribe request to the
    /// upstream server. Returns an error if the upstream does not support subscriptions
    /// or the resource URI is unknown.
    pub async fn subscribe_resource(
        &self,
        uri: &str,
        target: NotificationTarget,
    ) -> Result<(), McpError> {
        let snapshot = self.cache.load();
        let server_id = snapshot.resource_routes.get(uri).cloned().ok_or_else(|| {
            McpError::from(ProtocolError::InvalidRequest {
                detail: format!("resource not found: {uri}"),
            })
        })?;
        drop(snapshot);

        // Check upstream supports subscriptions
        let upstream = self
            .server_manager
            .get_upstream(&server_id)
            .ok_or_else(|| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?;
        let supports_subscribe = upstream
            .capabilities
            .resources
            .as_ref()
            .and_then(|r| r.subscribe)
            .unwrap_or(false);
        if !supports_subscribe {
            return Err(McpError::invalid_request(
                format!("server {server_id} does not support resource subscriptions"),
                None,
            ));
        }

        let mut entry = self
            .resource_subscriptions
            .entry(uri.to_string())
            .or_default();
        let is_first = entry.is_empty();
        entry.insert(target.clone());
        drop(entry);

        if is_first {
            if let Err(error) = upstream
                .client
                .peer()
                .subscribe(SubscribeRequestParams::new(uri))
                .await
            {
                // Roll back the local subscription on upstream failure
                if let Some(mut entry) = self.resource_subscriptions.get_mut(uri) {
                    entry.remove(&target);
                    if entry.is_empty() {
                        drop(entry);
                        self.resource_subscriptions.remove(uri);
                    }
                }
                return Err(match error {
                    rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                    other => McpError::internal_error(other.to_string(), None),
                });
            }
        }

        Ok(())
    }

    /// Unsubscribe a downstream client from resource updates.
    ///
    /// When the last subscriber is removed, forwards the unsubscribe to upstream.
    pub async fn unsubscribe_resource(
        &self,
        uri: &str,
        target: &NotificationTarget,
    ) -> Result<(), McpError> {
        let snapshot = self.cache.load();
        let server_id = snapshot.resource_routes.get(uri).cloned().ok_or_else(|| {
            McpError::from(ProtocolError::InvalidRequest {
                detail: format!("resource not found: {uri}"),
            })
        })?;
        drop(snapshot);

        let should_unsubscribe_upstream = {
            let mut entry = match self.resource_subscriptions.get_mut(uri) {
                Some(e) => e,
                None => return Ok(()),
            };
            entry.remove(target);
            entry.is_empty()
        };

        if should_unsubscribe_upstream {
            self.resource_subscriptions.remove(uri);

            if let Some(upstream) = self.server_manager.get_upstream(&server_id) {
                let _ = upstream
                    .client
                    .peer()
                    .unsubscribe(
                        serde_json::from_value::<UnsubscribeRequestParams>(
                            serde_json::json!({ "uri": uri }),
                        )
                        .expect("UnsubscribeRequestParams from known-good JSON"),
                    )
                    .await;
            }
        }

        Ok(())
    }

    /// Remove all subscriptions for a given downstream target (cleanup on disconnect).
    ///
    /// Iterates all subscription entries and removes the target. When a URI
    /// transitions from 1 → 0 subscribers, forwards `unsubscribe` upstream.
    pub async fn cleanup_subscriptions_for_target(&self, target: &NotificationTarget) {
        let mut uris_to_unsubscribe: Vec<(String, String)> = Vec::new();

        // Collect URIs where this target is subscribed
        self.resource_subscriptions.retain(|uri, subscribers| {
            subscribers.remove(target);
            if subscribers.is_empty() {
                // Need to unsubscribe upstream — resolve server_id from cache
                let snapshot = self.cache.load();
                if let Some(server_id) = snapshot.resource_routes.get(uri).cloned() {
                    uris_to_unsubscribe.push((uri.clone(), server_id));
                }
                false // remove the empty entry
            } else {
                true // keep entries that still have subscribers
            }
        });

        // Send upstream unsubscribe for each URI that lost its last subscriber
        for (uri, server_id) in uris_to_unsubscribe {
            if let Some(upstream) = self.server_manager.get_upstream(&server_id) {
                if let Err(error) = upstream
                    .client
                    .peer()
                    .unsubscribe(
                        serde_json::from_value::<UnsubscribeRequestParams>(
                            serde_json::json!({ "uri": uri }),
                        )
                        .expect("UnsubscribeRequestParams from known-good JSON"),
                    )
                    .await
                {
                    tracing::warn!(
                        uri = %uri,
                        error = %error,
                        "failed to unsubscribe upstream during target cleanup"
                    );
                }
            }
        }
    }

    /// Route an upstream resource-updated notification to subscribed downstream clients.
    pub(crate) fn route_upstream_resource_updated(&self, params: ResourceUpdatedNotificationParam) {
        let subscribers = match self.resource_subscriptions.get(&params.uri) {
            Some(entry) => entry.clone(),
            None => return,
        };

        for target in subscribers {
            self.publish_protocol_notification(ProtocolNotification::ResourceUpdated {
                target,
                params: params.clone(),
            });
        }
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
        let upstream_resources = self.server_manager.get_resources().await;
        let upstream_resource_templates = self.server_manager.get_resource_templates().await;
        let upstream_prompts = self.server_manager.get_prompts().await;

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

        let tool_definition_fingerprints = tools
            .iter()
            .map(|tool| (tool.name.to_string(), fingerprint_tool_definition(tool)))
            .collect::<HashMap<_, _>>();

        let previous_snapshot = self.cache.load();
        let drifted_tools = detect_tool_definition_drift(
            &previous_snapshot.tool_definition_fingerprints,
            &tool_definition_fingerprints,
        );

        if !drifted_tools.is_empty() {
            tracing::warn!(
                tools = ?drifted_tools,
                "detected material tool definition drift during refresh"
            );
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(EngineEvent::ToolDefinitionDriftDetected {
                    tool_names: drifted_tools
                        .iter()
                        .cloned()
                        .map(Arc::<str>::from)
                        .collect(),
                });
            }
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

        let mut resource_routes = HashMap::new();
        let mut resources_vec = Vec::new();
        for (server_name, mut resource) in upstream_resources {
            if let Some(existing_server) = resource_routes.get(&resource.uri) {
                if existing_server != &server_name {
                    tracing::warn!(
                        uri = %resource.uri,
                        first_server = %existing_server,
                        ignored_server = %server_name,
                        "resource URI collision detected; keeping first route"
                    );
                }
            }
            resource_routes
                .entry(resource.uri.clone())
                .or_insert_with(|| server_name.clone());

            let prefix = crate::tool_naming::format_server_prefix(&server_name);
            let original_name = resource.name.clone();
            let routed_name = crate::tool_naming::build_wire_name(
                &prefix,
                &original_name,
                &self.config.prefix_delimiter,
            );
            if resource.title.is_none() {
                resource.title = Some(crate::tool_naming::generate_title(&prefix, &original_name));
            }
            resource.name = routed_name;
            resources_vec.push(resource);
        }
        resources_vec.sort_by(|a, b| a.name.cmp(&b.name));
        let resources_all = Arc::new(resources_vec);

        let mut resource_templates_vec = Vec::new();
        for (server_name, mut template) in upstream_resource_templates {
            let prefix = crate::tool_naming::format_server_prefix(&server_name);
            let original_name = template.name.clone();
            let routed_name = crate::tool_naming::build_wire_name(
                &prefix,
                &original_name,
                &self.config.prefix_delimiter,
            );
            if template.title.is_none() {
                template.title = Some(crate::tool_naming::generate_title(&prefix, &original_name));
            }
            template.name = routed_name;
            resource_templates_vec.push(template);
        }
        resource_templates_vec.sort_by(|a, b| a.name.cmp(&b.name));
        let resource_templates_all = Arc::new(resource_templates_vec);

        let mut prompt_routes = HashMap::new();
        let mut prompts_vec = Vec::new();
        for (server_name, mut prompt) in upstream_prompts {
            let prefix = crate::tool_naming::format_server_prefix(&server_name);
            let original_name = prompt.name.clone();
            let routed_name = crate::tool_naming::build_wire_name(
                &prefix,
                &original_name,
                &self.config.prefix_delimiter,
            );
            prompt_routes
                .entry(routed_name.clone())
                .or_insert_with(|| (server_name.clone(), original_name.clone()));
            if prompt.title.is_none() {
                prompt.title = Some(crate::tool_naming::generate_title(&prefix, &original_name));
            }
            prompt.name = routed_name;
            prompts_vec.push(prompt);
        }
        prompts_vec.sort_by(|a, b| a.name.cmp(&b.name));
        let prompts_all = Arc::new(prompts_vec);

        let tool_count = tools_all.len();

        self.cache.store(Arc::new(RouterSnapshot {
            routes,
            tools_all,
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_windsurf,
            tools_copilot,
            resources_all,
            resource_templates_all,
            prompts_all,
            resource_routes,
            prompt_routes,
            tool_definition_fingerprints,
        }));

        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(EngineEvent::ToolCacheRefreshed { tool_count });
        }
    }

    /// Get the current list of tools (zero-copy via Arc). Returns all tools.
    pub fn list_tools(&self) -> Arc<Vec<Tool>> {
        let snapshot = self.cache.load();
        if self.config.meta_tool_mode {
            Arc::clone(&snapshot.meta_tools_all)
        } else {
            Arc::clone(&snapshot.tools_all)
        }
    }

    pub fn list_tools_page_for_client(
        &self,
        client_type: ClientType,
        request: Option<PaginatedRequestParams>,
    ) -> ListToolsResult {
        let tools = self.list_tools_for_client(client_type);
        paginated_result((*tools).clone(), request, |tools, next_cursor| {
            ListToolsResult {
                meta: None,
                next_cursor,
                tools,
            }
        })
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

    pub fn synthesized_capabilities(&self) -> ServerCapabilities {
        let upstream_caps = self.server_manager.healthy_capabilities();
        let mut capabilities = ServerCapabilities::default();

        if self.config.meta_tool_mode
            || !self.list_tools().is_empty()
            || upstream_caps.iter().any(|caps| caps.tools.is_some())
        {
            capabilities.tools = Some(ToolsCapability {
                list_changed: Some(true),
            });
        }
        if upstream_caps.iter().any(|caps| caps.resources.is_some()) {
            let any_subscribe = upstream_caps.iter().any(|caps| {
                caps.resources
                    .as_ref()
                    .and_then(|r| r.subscribe)
                    .unwrap_or(false)
            });
            capabilities.resources = Some(ResourcesCapability {
                subscribe: if any_subscribe { Some(true) } else { None },
                list_changed: Some(false),
            });
        }
        if upstream_caps.iter().any(|caps| caps.prompts.is_some()) {
            capabilities.prompts = Some(PromptsCapability {
                list_changed: Some(false),
            });
        }
        if upstream_caps.iter().any(|caps| caps.completions.is_some()) {
            capabilities.completions = Some(serde_json::Map::new());
        }

        capabilities
    }

    /// Get tools filtered for a specific client type. O(1) — single Arc::clone.
    pub fn list_tools_for_client(&self, client_type: ClientType) -> Arc<Vec<Tool>> {
        if self.config.meta_tool_mode {
            return Arc::clone(&self.cache.load().meta_tools_all);
        }
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

    pub fn list_resources(&self) -> Arc<Vec<Resource>> {
        Arc::clone(&self.cache.load().resources_all)
    }

    pub fn list_resources_page(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> ListResourcesResult {
        paginated_result(
            (*self.list_resources()).clone(),
            request,
            |resources, next_cursor| ListResourcesResult {
                meta: None,
                next_cursor,
                resources,
            },
        )
    }

    pub fn list_resource_templates(&self) -> Arc<Vec<ResourceTemplate>> {
        Arc::clone(&self.cache.load().resource_templates_all)
    }

    pub fn list_resource_templates_page(
        &self,
        request: Option<PaginatedRequestParams>,
    ) -> ListResourceTemplatesResult {
        paginated_result(
            (*self.list_resource_templates()).clone(),
            request,
            |resource_templates, next_cursor| ListResourceTemplatesResult {
                meta: None,
                next_cursor,
                resource_templates,
            },
        )
    }

    pub fn list_prompts(&self) -> Arc<Vec<Prompt>> {
        Arc::clone(&self.cache.load().prompts_all)
    }

    pub fn list_prompts_page(&self, request: Option<PaginatedRequestParams>) -> ListPromptsResult {
        paginated_result(
            (*self.list_prompts()).clone(),
            request,
            |prompts, next_cursor| ListPromptsResult {
                meta: None,
                next_cursor,
                prompts,
            },
        )
    }

    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, McpError> {
        let snapshot = self.cache.load();
        let server_id = snapshot.resource_routes.get(uri).cloned().ok_or_else(|| {
            McpError::from(ProtocolError::InvalidRequest {
                detail: format!("resource not found: {uri}"),
            })
        })?;
        drop(snapshot);

        let upstream = self
            .server_manager
            .get_upstream(&server_id)
            .ok_or_else(|| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?;

        upstream
            .client
            .peer()
            .read_resource(ReadResourceRequestParams::new(uri))
            .await
            .map_err(|error| match error {
                rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                other => McpError::internal_error(other.to_string(), None),
            })
    }

    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<GetPromptResult, McpError> {
        let snapshot = self.cache.load();
        let (server_id, prompt_name) =
            snapshot.prompt_routes.get(name).cloned().ok_or_else(|| {
                McpError::from(ProtocolError::InvalidRequest {
                    detail: format!("prompt not found: {name}"),
                })
            })?;
        drop(snapshot);

        let upstream = self
            .server_manager
            .get_upstream(&server_id)
            .ok_or_else(|| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?;

        let mut request = GetPromptRequestParams::new(prompt_name);
        if let Some(arguments) = arguments {
            request = request.with_arguments(arguments);
        }

        upstream
            .client
            .peer()
            .get_prompt(request)
            .await
            .map_err(|error| match error {
                rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                other => McpError::internal_error(other.to_string(), None),
            })
    }

    /// Forward a `completion/complete` request to the correct upstream server
    /// based on the reference type (prompt name or resource URI).
    pub async fn complete_request(
        &self,
        params: CompleteRequestParams,
    ) -> Result<CompleteResult, McpError> {
        let server_id = match &params.r#ref {
            Reference::Prompt(prompt_ref) => {
                let snapshot = self.cache.load();
                let (sid, _) = snapshot
                    .prompt_routes
                    .get(&prompt_ref.name)
                    .cloned()
                    .ok_or_else(|| {
                        McpError::from(ProtocolError::InvalidRequest {
                            detail: format!("prompt not found: {}", prompt_ref.name),
                        })
                    })?;
                sid
            }
            Reference::Resource(resource_ref) => {
                let snapshot = self.cache.load();
                snapshot
                    .resource_routes
                    .get(&resource_ref.uri)
                    .cloned()
                    .ok_or_else(|| {
                        McpError::from(ProtocolError::InvalidRequest {
                            detail: format!("resource not found: {}", resource_ref.uri),
                        })
                    })?
            }
        };

        let upstream = self
            .server_manager
            .get_upstream(&server_id)
            .ok_or_else(|| {
                McpError::from(ProtocolError::ServerUnavailable {
                    server_id: server_id.clone(),
                })
            })?;

        upstream
            .client
            .peer()
            .complete(params)
            .await
            .map_err(|error| match error {
                rmcp::service::ServiceError::McpError(mcp_err) => mcp_err,
                other => McpError::internal_error(other.to_string(), None),
            })
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
        self.call_tool_with_context(tool_name, arguments, None, None)
            .await
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
            // Intercept plug meta-tools (case-insensitive for LLM casing drift).
            if tool_name.eq_ignore_ascii_case("plug__list_servers") {
                return Ok(self.handle_list_servers());
            }
            if tool_name.eq_ignore_ascii_case("plug__list_tools") {
                return self.handle_list_tools(arguments.clone());
            }
            if tool_name.eq_ignore_ascii_case("plug__search_tools") {
                return self.handle_search_tools(arguments.clone());
            }
            if tool_name.eq_ignore_ascii_case("plug__invoke_tool") {
                return self
                    .handle_invoke_tool(arguments.clone(), progress_token, downstream, is_retry)
                    .await;
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
                meta: downstream_progress_token
                    .clone()
                    .map(Meta::with_progress_token),
            };

            let call_id = next_call_id();
            struct ActiveCallGuard<'a> {
                router: &'a ToolRouter,
                call_id: u64,
                armed: bool,
            }
            impl<'a> ActiveCallGuard<'a> {
                fn disarm(&mut self) {
                    self.armed = false;
                }
            }
            impl Drop for ActiveCallGuard<'_> {
                fn drop(&mut self) {
                    if self.armed {
                        self.router.remove_active_call(self.call_id);
                    }
                }
            }
            let server_id_arc = Arc::<str>::from(server_id.as_str());
            let tool_name_arc = Arc::<str>::from(original_name.as_str());
            let mut active_call_guard = None;
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
                active_call_guard = Some(ActiveCallGuard {
                    router: self,
                    call_id,
                    armed: true,
                });
            }
            let request_handle = peer
                .send_cancellable_request(request, options)
                .await
                .map_err(|error| {
                    if let Some(ref mut guard) = active_call_guard {
                        guard.disarm();
                    }
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
                    if let Some(ref mut guard) = active_call_guard {
                        guard.disarm();
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
                    if let Some(ref mut guard) = active_call_guard {
                        guard.disarm();
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
                    if let Some(ref mut guard) = active_call_guard {
                        guard.disarm();
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
                    if let Some(ref mut guard) = active_call_guard {
                        guard.disarm();
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
                    if let Some(ref mut guard) = active_call_guard {
                        guard.disarm();
                    }
                    self.remove_active_call(call_id);
                    Err(McpError::internal_error(
                        format!("unexpected response type from upstream tool call: {other:?}"),
                        None,
                    ))
                }
            }
        })
    }

    fn handle_list_servers(&self) -> CallToolResult {
        let snapshot = self.cache.load();
        let mut tool_counts: HashMap<&str, usize> = HashMap::new();
        for (server_id, _) in snapshot.routes.values() {
            if server_id != "__plug_internal__" {
                *tool_counts.entry(server_id.as_str()).or_insert(0) += 1;
            }
        }

        let statuses = self.server_manager.server_statuses();
        if statuses.is_empty() {
            return CallToolResult::success(vec![Content::text("No upstream servers configured.")]);
        }

        let mut lines = vec![format!("Servers ({})", statuses.len())];
        for status in statuses {
            let tool_count = tool_counts
                .get(status.server_id.as_str())
                .copied()
                .unwrap_or(0);
            lines.push(format!(
                "- {} (health: {:?}, tools: {})",
                status.server_id, status.health, tool_count
            ));
        }

        CallToolResult::success(vec![Content::text(lines.join("\n"))])
    }

    fn handle_list_tools(
        &self,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult, McpError> {
        let server_filter = arguments
            .as_ref()
            .and_then(|args| args.get("server_id"))
            .and_then(|value| value.as_str())
            .map(|value| value.to_lowercase());
        let query = arguments
            .as_ref()
            .and_then(|args| args.get("query"))
            .and_then(|value| value.as_str())
            .map(|value| value.to_lowercase());
        let limit = arguments
            .as_ref()
            .and_then(|args| args.get("limit"))
            .and_then(|value| value.as_u64())
            .map(|value| value.min(100) as usize)
            .unwrap_or(25);

        let snapshot = self.cache.load();
        let mut matches = Vec::new();
        for tool in snapshot.tools_all.iter() {
            let Some((server_id, _)) = snapshot.routes.get(tool.name.as_ref()) else {
                continue;
            };
            if server_id == "__plug_internal__" {
                continue;
            }
            if let Some(filter) = server_filter.as_ref() {
                if server_id.to_lowercase() != *filter {
                    continue;
                }
            }
            if let Some(query) = query.as_ref() {
                let name = tool.name.to_lowercase();
                let desc = tool.description.as_deref().unwrap_or("").to_lowercase();
                if !name.contains(query) && !desc.contains(query) {
                    continue;
                }
            }
            matches.push((server_id.clone(), tool.clone()));
            if matches.len() >= limit {
                break;
            }
        }

        if matches.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No tools matched the requested filters.",
            )]));
        }

        let mut lines = vec![format!("Tools ({})", matches.len())];
        for (server_id, tool) in matches {
            lines.push(format!("- {} (server: {})", tool.name, server_id));
            if let Some(desc) = tool.description.as_deref() {
                lines.push(format!("  {}", desc));
            }
        }

        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
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

        let mut args = arguments.unwrap_or_default();
        args.insert("query".to_string(), serde_json::Value::String(query));
        args.insert("limit".to_string(), serde_json::Value::Number(10.into()));
        self.handle_list_tools(Some(args))
    }

    fn handle_invoke_tool<'a>(
        &'a self,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        progress_token: Option<ProgressToken>,
        downstream: Option<DownstreamCallContext>,
        is_retry: bool,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CallToolResult, McpError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let args = arguments.ok_or_else(|| {
                McpError::from(ProtocolError::InvalidRequest {
                    detail: "plug__invoke_tool requires arguments".to_string(),
                })
            })?;
            let target = args
                .get("tool_name")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    McpError::from(ProtocolError::InvalidRequest {
                        detail: "plug__invoke_tool requires a string tool_name".to_string(),
                    })
                })?;
            if target.eq_ignore_ascii_case("plug__invoke_tool") {
                return Err(McpError::from(ProtocolError::InvalidRequest {
                    detail: "plug__invoke_tool cannot invoke itself".to_string(),
                }));
            }

            let forwarded_arguments = args
                .get("arguments")
                .and_then(|value| value.as_object())
                .cloned();

            self.call_tool_inner(
                target,
                forwarded_arguments,
                progress_token,
                downstream,
                is_retry,
            )
            .await
        })
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

/// Sanitize and optionally truncate tool descriptions for token efficiency.
///
/// Preserves `outputSchema` (structured output), `title` (human-friendly
/// display name), and `annotations` (readOnlyHint, etc.).
/// `inputSchema` is REQUIRED per MCP spec (ADR-003) — never stripped.
fn strip_optional_fields(tool: &mut Tool, max_desc_chars: Option<usize>) {
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

fn paginated_result<T: Clone, R>(
    items: Vec<T>,
    request: Option<PaginatedRequestParams>,
    build: impl FnOnce(Vec<T>, Option<String>) -> R,
) -> R {
    const PAGE_SIZE: usize = 100;

    let start = request
        .as_ref()
        .and_then(|params| params.cursor.as_ref())
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .filter(|idx| *idx < items.len())
        .unwrap_or(0);
    let end = usize::min(start + PAGE_SIZE, items.len());
    let next_cursor = (end < items.len()).then(|| end.to_string());

    build(items[start..end].to_vec(), next_cursor)
}

fn detect_tool_definition_drift(
    previous: &HashMap<String, u64>,
    current: &HashMap<String, u64>,
) -> Vec<String> {
    let mut drifted = current
        .iter()
        .filter_map(|(tool_name, fingerprint)| {
            previous
                .get(tool_name)
                .filter(|previous_fingerprint| *previous_fingerprint != fingerprint)
                .map(|_| tool_name.clone())
        })
        .collect::<Vec<_>>();
    drifted.sort();
    drifted
}

fn fingerprint_tool_definition(tool: &Tool) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool.name.hash(&mut hasher);
    tool.description.as_deref().unwrap_or("").hash(&mut hasher);
    tool.title.as_deref().unwrap_or("").hash(&mut hasher);
    serde_json::to_string(&tool.input_schema)
        .expect("tool input schema serializes")
        .hash(&mut hasher);
    serde_json::to_string(&tool.annotations)
        .expect("tool annotations serialize")
        .hash(&mut hasher);
    hasher.finish()
}

fn build_meta_tools() -> Vec<Tool> {
    vec![
        build_list_servers_meta_tool(),
        build_list_tools_meta_tool(),
        build_search_tools_meta_tool(),
        build_invoke_tool_meta_tool(),
    ]
}

fn build_list_servers_meta_tool() -> Tool {
    Tool::new(
        Cow::Borrowed("plug__list_servers"),
        Cow::Borrowed("List upstream server IDs, health, and current routed tool counts."),
        Arc::new(serde_json::Map::new()),
    )
}

fn build_list_tools_meta_tool() -> Tool {
    Tool::new(
        Cow::Borrowed("plug__list_tools"),
        Cow::Borrowed(
            "List routed tools hidden behind meta-tool mode, optionally filtered by server or query.",
        ),
        Arc::new(
            serde_json::json!({
                "type": "object",
                "properties": {
                    "server_id": {
                        "type": "string",
                        "description": "Optional upstream server ID filter"
                    },
                    "query": {
                        "type": "string",
                        "description": "Optional substring filter on tool name or description"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Maximum tools to return (default: 25)"
                    }
                }
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
    )
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

fn build_invoke_tool_meta_tool() -> Tool {
    Tool::new(
        Cow::Borrowed("plug__invoke_tool"),
        Cow::Borrowed(
            "Invoke a specific routed tool by prefixed name and return the raw upstream result.",
        ),
        Arc::new(
            serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_name": {
                        "type": "string",
                        "description": "Exact prefixed tool name to invoke"
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Arguments object to forward to the target tool"
                    }
                },
                "required": ["tool_name"]
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
    /// Cancelled on drop to signal the notification fan-out task to exit.
    shutdown: CancellationToken,
}

impl Drop for ProxyHandler {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl ProxyHandler {
    pub fn new(server_manager: Arc<ServerManager>, config: RouterConfig) -> Self {
        Self {
            router: Arc::new(ToolRouter::new(server_manager, config)),
            client_type: std::sync::RwLock::new(ClientType::Unknown),
            client_id: Arc::from(uuid::Uuid::new_v4().to_string()),
            notification_task_started: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
        }
    }

    /// Create a ProxyHandler from an existing shared ToolRouter.
    pub fn from_router(router: Arc<ToolRouter>) -> Self {
        Self {
            router,
            client_type: std::sync::RwLock::new(ClientType::Unknown),
            client_id: Arc::from(uuid::Uuid::new_v4().to_string()),
            notification_task_started: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
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
        InitializeResult::new(self.router.synthesized_capabilities())
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
                let router = Arc::clone(&self.router);
                let mut rx = self.router.subscribe_notifications();
                let shutdown = self.shutdown.clone();
                tokio::spawn(async move {
                    loop {
                        let msg = tokio::select! {
                            biased;
                            _ = shutdown.cancelled() => break,
                            msg = rx.recv() => msg,
                        };
                        match msg {
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
                            Ok(ProtocolNotification::ResourceUpdated { target, params }) => {
                                if matches!(
                                    target,
                                    NotificationTarget::Stdio { client_id: target_id }
                                        if target_id == client_id
                                ) && peer.notify_resource_updated(params).await.is_err()
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
                    // Clean up resource subscriptions for this disconnected client
                    let target = NotificationTarget::Stdio {
                        client_id: Arc::clone(&client_id),
                    };
                    router.cleanup_subscriptions_for_target(&target).await;
                });
            }

            Ok(self.get_info())
        }
    }

    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            let ct = self
                .client_type
                .read()
                .map(|ct| *ct)
                .unwrap_or(ClientType::Unknown);
            Ok(self.router.list_tools_page_for_client(ct, request))
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
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        async move { Ok(self.router.list_resources_page(request)) }
    }

    fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, McpError>> + Send + '_ {
        async move { Ok(self.router.list_resource_templates_page(request)) }
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResult, McpError>> + Send + '_ {
        async move { self.router.read_resource(&request.uri).await }
    }

    fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        async move { Ok(self.router.list_prompts_page(request)) }
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResult, McpError>> + Send + '_ {
        async move {
            self.router
                .get_prompt(&request.name, request.arguments)
                .await
        }
    }

    fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        let target = NotificationTarget::Stdio {
            client_id: Arc::clone(&self.client_id),
        };
        async move { self.router.subscribe_resource(&request.uri, target).await }
    }

    fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        let target = NotificationTarget::Stdio {
            client_id: Arc::clone(&self.client_id),
        };
        async move {
            self.router
                .unsubscribe_resource(&request.uri, &target)
                .await
        }
    }

    fn complete(
        &self,
        request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CompleteResult, McpError>> + Send + '_ {
        async move { self.router.complete_request(request).await }
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
            meta_tool_mode: false,
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
        assert!(info.capabilities.tools.is_none());
        assert!(info.capabilities.resources.is_none());
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
    fn strip_optional_fields_preserves_schema_and_truncates_description() {
        let mut tool = Tool::new(
            Cow::Borrowed("test_tool"),
            Cow::Borrowed("A long description that should be truncated if configured"),
            Arc::new(serde_json::Map::new()),
        );
        tool.title = Some("Title".to_string());
        tool.annotations = Some(ToolAnnotations::default());
        tool.output_schema = Some(Arc::new(serde_json::Map::new()));

        strip_optional_fields(&mut tool, Some(10));

        assert!(tool.title.is_some());
        assert!(tool.annotations.is_some());
        assert!(
            tool.output_schema.is_some(),
            "outputSchema must be preserved"
        );
        assert_eq!(tool.description.as_deref(), Some("A long des"));
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
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_windsurf,
            tools_copilot,
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
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
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
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
        assert!(text.contains("No tools matched"));
    }

    #[test]
    fn meta_tool_mode_lists_only_meta_tools() {
        let sm = Arc::new(ServerManager::new());
        let mut config = test_router_config();
        config.meta_tool_mode = true;
        let router = ToolRouter::new(sm, config);

        let tools = vec![Tool::new(
            Cow::Borrowed("git__commit"),
            Cow::Borrowed("Create a git commit"),
            Arc::new(serde_json::Map::new()),
        )];

        let mut routes = HashMap::new();
        routes.insert(
            "git__commit".to_string(),
            ("git".to_string(), "commit".to_string()),
        );

        router.cache.store(Arc::new(RouterSnapshot {
            routes,
            tools_all: Arc::new(tools),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
        }));

        let visible_tools = router.list_tools_for_client(ClientType::ClaudeCode);
        let names = visible_tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "plug__list_servers",
                "plug__list_tools",
                "plug__search_tools",
                "plug__invoke_tool",
            ]
        );

        let full_tools = router.list_all_tools();
        assert_eq!(full_tools.len(), 1);
        assert_eq!(full_tools[0].1.name.as_ref(), "git__commit");
    }

    #[test]
    fn detect_tool_definition_drift_reports_changed_tools_only() {
        let previous = HashMap::from([
            ("git__commit".to_string(), 1_u64),
            ("git__push".to_string(), 2_u64),
        ]);
        let current = HashMap::from([
            ("git__commit".to_string(), 3_u64),
            ("git__push".to_string(), 2_u64),
            ("git__status".to_string(), 4_u64),
        ]);

        assert_eq!(
            detect_tool_definition_drift(&previous, &current),
            vec!["git__commit".to_string()]
        );
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
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
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
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
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

    #[test]
    fn list_tools_page_for_client_uses_cursor_pagination() {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, test_router_config());
        let tools: Vec<Tool> = (0..150)
            .map(|index| {
                Tool::new(
                    Cow::Owned(format!("tool_{index}")),
                    Cow::Borrowed("desc"),
                    Arc::new(serde_json::Map::new()),
                )
            })
            .collect();
        router.cache.store(Arc::new(RouterSnapshot {
            routes: HashMap::new(),
            tools_windsurf: Arc::new(tools.iter().take(100).cloned().collect()),
            tools_copilot: Arc::new(tools.iter().take(128).cloned().collect()),
            tools_all: Arc::new(tools),
            meta_tools_all: Arc::new(build_meta_tools()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
        }));

        let first =
            router.list_tools_page_for_client(ClientType::Unknown, Some(Default::default()));
        assert_eq!(first.tools.len(), 100);
        assert_eq!(first.next_cursor.as_deref(), Some("100"));

        let mut second_request = PaginatedRequestParams::default();
        second_request.cursor = first.next_cursor;
        let second = router.list_tools_page_for_client(ClientType::Unknown, Some(second_request));
        assert_eq!(second.tools.len(), 50);
        assert!(second.next_cursor.is_none());
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
            ProgressNotificationParam::new(progress_token.clone(), 0.5).with_message("halfway"),
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

    #[test]
    fn synthesized_capabilities_advertises_subscribe_when_upstream_supports_it() {
        let sm = Arc::new(ServerManager::new());
        let config = test_router_config();
        let router = ToolRouter::new(sm, config);

        // No upstreams → no resources capability at all
        let caps = router.synthesized_capabilities();
        assert!(caps.resources.is_none());
    }

    #[test]
    fn resource_subscription_registry_lifecycle() {
        let sm = Arc::new(ServerManager::new());
        let config = test_router_config();
        let router = ToolRouter::new(sm, config);

        let target = NotificationTarget::Stdio {
            client_id: Arc::from("test-client"),
        };

        // Registry starts empty
        assert!(router.resource_subscriptions.is_empty());

        // Insert directly (bypassing upstream check for unit test)
        router
            .resource_subscriptions
            .entry("file:///test".to_string())
            .or_default()
            .insert(target.clone());
        assert_eq!(router.resource_subscriptions.len(), 1);

        // Route notification should publish to subscriber
        let mut rx = router.subscribe_notifications();
        router
            .route_upstream_resource_updated(ResourceUpdatedNotificationParam::new("file:///test"));

        match rx.try_recv() {
            Ok(ProtocolNotification::ResourceUpdated {
                target: t, params, ..
            }) => {
                assert_eq!(t, target);
                assert_eq!(params.uri, "file:///test");
            }
            other => panic!("expected ResourceUpdated, got: {other:?}"),
        }

        // Route notification for unsubscribed URI → no notification
        router.route_upstream_resource_updated(ResourceUpdatedNotificationParam::new(
            "file:///other",
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn synthesized_capabilities_no_completions_without_upstream() {
        let sm = Arc::new(ServerManager::new());
        let router = ToolRouter::new(sm, test_router_config());
        let caps = router.synthesized_capabilities();
        assert!(caps.completions.is_none());
    }

    #[test]
    fn complete_request_params_serde_roundtrip() {
        let params = CompleteRequestParams::new(
            Reference::for_prompt("test-prompt"),
            ArgumentInfo {
                name: "arg1".to_string(),
                value: "partial".to_string(),
            },
        );

        let json = serde_json::to_value(&params).unwrap();
        let deserialized: CompleteRequestParams = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.argument.name, "arg1");
        assert_eq!(deserialized.argument.value, "partial");
        match &deserialized.r#ref {
            Reference::Prompt(p) => assert_eq!(p.name, "test-prompt"),
            other => panic!("expected Prompt reference, got {other:?}"),
        }
    }
}
