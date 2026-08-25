use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::RequestParamsMeta;
use rmcp::model::*;
use rmcp::service::{NotificationContext, Peer, PeerRequestOptions, RequestContext, RoleServer};
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::artifacts::ArtifactStore;
use crate::branding;
use crate::circuit::CircuitBreakerError;
use crate::client_detect::detect_client;
use crate::config::{Config, LazyToolsConfig};
use crate::engine::{Engine, EngineEvent, next_call_id};
use crate::error::ProtocolError;
use crate::notifications::{NotificationTarget, ProtocolNotification};
use crate::server::ServerManager;
use crate::tasks::{TaskOwner, TaskStore, TaskUpstreamRef};
use crate::types::{
    ClientType, ExtensionEnvelope, LazyToolMode, LazyToolModeOrigin, PrincipalId,
    ResolvedLazyToolPolicy, ServerHealth,
};
use continuations::{
    MRTR_CHAIN_TTL, MRTR_MAX_BYTES, MRTR_MAX_ITEMS, MRTR_MAX_REQUEST_STATE_BYTES, MRTR_MAX_ROUNDS,
};

const LIST_CHANGED_REFRESH_DEBOUNCE: Duration = Duration::from_millis(750);
/// Backstop timeout for a single `refresh_tools` pass inside the notification
/// refresh task. Per-server listing calls are already bounded by
/// `call_timeout_secs` (see `ServerManager::get_resources`); this is a last
/// resort so an unforeseen hang can never wedge the refresh task and silently
/// disable all future `list_changed` delivery.
const LIST_CHANGED_REFRESH_MAX: Duration = Duration::from_secs(600);
const BRIDGE_WORKING_SET_MAX_TOOLS: usize = 20;
const BRIDGE_SEARCH_RESULT_MAX: usize = 10;
#[cfg(test)]
const LATEST_PROTOCOL_VERSION: &str = crate::protocol::SUPPORTED_PROTOCOL_VERSION;

fn plug_implementation() -> Implementation {
    branding::plug_implementation(env!("CARGO_PKG_VERSION"))
}

/// Atomically-swapped tool snapshot with pre-cached filtered views per client type.
///
/// Built once at `refresh_tools()` time so that `list_tools_for_client()` is O(1).
#[derive(Clone)]
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
    /// ASCII-lowercased route keys for O(1) case-insensitive tools/call fallback.
    pub routes_lower: HashMap<String, (String, String)>,
    /// Exact tool name → index into `tools_all`.
    pub tools_by_name: HashMap<String, usize>,
    /// ASCII-lowercased tool name → index into `tools_all`.
    pub tools_by_name_lower: HashMap<String, usize>,
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
    /// Tool name -> operator-only risk metadata preserving upstream-vs-Plug annotation provenance.
    pub tool_risk_inventory: HashMap<String, crate::ipc::IpcToolRiskInfo>,
}

impl RouterSnapshot {
    /// Populate O(1) lookup indexes after routes/tools are finalized.
    pub(crate) fn with_indexes(mut self) -> Self {
        self.routes_lower = self
            .routes
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        self.tools_by_name = self
            .tools_all
            .iter()
            .enumerate()
            .map(|(i, tool)| (tool.name.to_string(), i))
            .collect();
        self.tools_by_name_lower = self
            .tools_all
            .iter()
            .enumerate()
            .map(|(i, tool)| (tool.name.to_ascii_lowercase(), i))
            .collect();
        self
    }

    pub(crate) fn resolve_route(&self, tool_name: &str) -> Option<&(String, String)> {
        self.routes
            .get(tool_name)
            .or_else(|| self.routes_lower.get(&tool_name.to_ascii_lowercase()))
    }

    pub(crate) fn tool_by_name(&self, name: &str) -> Option<&Tool> {
        self.tools_by_name
            .get(name)
            .or_else(|| self.tools_by_name_lower.get(&name.to_ascii_lowercase()))
            .and_then(|&i| self.tools_all.get(i))
    }
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
    pub lazy_tools: LazyToolsConfig,
    pub tool_filter_enabled: bool,
    /// Servers with enrichment enabled (annotation inference + title normalization).
    pub enrichment_servers: std::collections::HashSet<String>,
}

impl From<&Config> for RouterConfig {
    fn from(config: &Config) -> Self {
        Self {
            prefix_delimiter: config.prefix_delimiter.clone(),
            priority_tools: config.priority_tools.clone(),
            disabled_tools: config
                .disabled_tools
                .iter()
                .map(|pattern| pattern.to_ascii_lowercase())
                .collect(),
            tool_description_max_chars: config.tool_description_max_chars,
            tool_search_threshold: config.tool_search_threshold,
            meta_tool_mode: config.meta_tool_mode,
            lazy_tools: config.lazy_tools.clone(),
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

impl RouterConfig {
    fn lazy_policy_for_client(&self, client_type: ClientType) -> ResolvedLazyToolPolicy {
        let target = client_type.target_slug().unwrap_or("unknown");
        crate::config::resolve_lazy_tool_policy_from_settings(
            self.meta_tool_mode,
            &self.lazy_tools,
            target,
        )
    }

    fn lazy_surface_for_client(&self, client_type: ClientType) -> LazyToolSurface {
        let policy = self.lazy_policy_for_client(client_type);
        if policy.origin == LazyToolModeOrigin::LegacyMetaToolMode {
            return LazyToolSurface::LegacyMeta;
        }
        match policy.mode {
            LazyToolMode::Standard => LazyToolSurface::Standard,
            LazyToolMode::Native => LazyToolSurface::Native,
            LazyToolMode::Bridge => LazyToolSurface::Bridge,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LazyToolSurface {
    Standard,
    Native,
    Bridge,
    LegacyMeta,
}

fn mrtr_pair_supported(
    downstream_era: crate::protocol::ProtocolEra,
    upstream_era: crate::protocol::ProtocolEra,
    has_continuation: bool,
    task_requested: bool,
) -> bool {
    let native_modern = downstream_era == crate::protocol::ProtocolEra::Modern
        && upstream_era == crate::protocol::ProtocolEra::Modern;
    if has_continuation && !native_modern {
        return false;
    }
    if task_requested && upstream_era == crate::protocol::ProtocolEra::Modern {
        return false;
    }
    !(downstream_era == crate::protocol::ProtocolEra::Legacy
        && upstream_era == crate::protocol::ProtocolEra::Modern)
}

/// Shared tool routing logic used by both stdio (ProxyHandler) and HTTP handlers.
pub struct ToolRouter {
    server_manager: Arc<ServerManager>,
    activity_store: crate::activity::ActivityStore,
    cache: Arc<ArcSwap<RouterSnapshot>>,
    /// Last published call-routing identity. Catalog metadata may change
    /// without invalidating parked rounds; route targets and upstream
    /// instances may not.
    published_tool_routes: std::sync::Mutex<HashMap<String, MaterialToolRoute>>,
    config: RouterConfig,
    /// Optional event sender for tool call observability.
    event_tx: Option<broadcast::Sender<EngineEvent>>,
    protocol_notification_tx: broadcast::Sender<ProtocolNotification>,
    /// Separate channel for logging notifications to prevent log volume
    /// from causing Lagged errors that drop Progress/Cancelled delivery.
    logging_tx: broadcast::Sender<ProtocolNotification>,
    /// Per-client requested log levels. Effective level = most permissive (lowest severity).
    client_log_levels: DashMap<Arc<str>, LoggingLevel>,
    /// The effective (most permissive) log level across all connected clients.
    effective_log_level: ArcSwap<LoggingLevel>,
    active_calls: DashMap<u64, ActiveCallRecord>,
    active_call_lookup: DashMap<DownstreamCallKey, u64>,
    upstream_request_lookup: DashMap<UpstreamRequestKey, u64>,
    upstream_progress_lookup: DashMap<UpstreamProgressKey, u64>,
    notification_refresh_in_progress: AtomicBool,
    notification_refresh_pending: AtomicBool,
    pending_tool_list_changed: AtomicBool,
    pending_resource_list_changed: AtomicBool,
    pending_prompt_list_changed: AtomicBool,
    /// Weak reference to Engine for session recovery (reconnect on error).
    /// Set after Engine construction via `set_engine()`.
    engine: std::sync::RwLock<Option<Weak<Engine>>>,
    artifact_store: ArtifactStore,
    /// Resource subscription registry: upstream URI → set of downstream subscribers.
    /// Owns the atomic subscribe/unsubscribe state machine — see
    /// `subscriptions::SubscriptionRegistry` for the invariants.
    resource_subscriptions: Arc<subscriptions::SubscriptionRegistry>,
    /// Cached downstream roots per client. Upstream servers see the union via `list_roots_union()`.
    client_roots: DashMap<NotificationTarget, Vec<Root>>,
    /// Per-client bridge for forwarding reverse requests (elicitation, sampling).
    downstream_bridges: DashMap<NotificationTarget, Arc<dyn DownstreamBridge>>,
    /// Session-scoped lazy loaded routed tool names, oldest first.
    lazy_working_sets: DashMap<String, VecDeque<String>>,
    /// Runtime-owned mutable task state. Intentionally not part of the immutable router snapshot.
    task_store: Mutex<TaskStore>,
    /// One process-wide admission ledger shared by every downstream transport.
    admission_quotas: crate::protocol::AdmissionQuotas,
    /// Process-wide, reloadable gate for the modern downstream lifecycle.
    /// It defaults off and is captured into each downstream call context.
    modern_downstream_enabled: AtomicBool,
    /// Serializes `refresh_tools`' decide-and-mutate phase (subscription
    /// classify → prune → snapshot publish → rebind) across concurrent
    /// refresh passes, so one pass cannot interleave reconciliation
    /// decisions made against a pre-publish snapshot with another pass's
    /// publish. Per-server listing/fetch work stays outside it.
    refresh_reconcile_lock: Mutex<()>,
    /// One-time indirection for upstream-owned modern requestState. The raw
    /// upstream token remains server-side and is bound to the route instance.
    continuation_registry: continuations::ContinuationRegistry<NativeContinuation>,
    #[cfg(test)]
    continuation_insert_gate: std::sync::Mutex<Option<Arc<ContinuationInsertGate>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DownstreamTransport {
    Stdio,
    Http,
    /// Daemon IPC client (`plug connect` over the Unix socket). Has its own
    /// lazy-session-key namespace (`ipc:`) and `NotificationTarget::Ipc` so it no
    /// longer masquerades as `Stdio`.
    Ipc,
}

#[derive(Clone, Debug)]
pub struct DownstreamCallContext {
    pub transport: DownstreamTransport,
    pub client_id: Arc<str>,
    pub request_id: RequestId,
    pub client_type: ClientType,
    pub trace_id: Arc<str>,
    pub protocol_era: crate::protocol::ProtocolEra,
    pub protocol_version: Arc<str>,
    /// Reloadable transport gate captured when this request context is built.
    /// Modern policy remains fail-closed unless the owning adapter sets it.
    pub modern_direction_enabled: bool,
    pub principal: Option<PrincipalId>,
    pub scopes: Arc<BTreeSet<String>>,
    pub local_trust: bool,
    pub client_metadata: Option<crate::protocol::ClientMetadata>,
    pub deadline: Option<std::time::Instant>,
    pub cancellation: CancellationToken,
    pub durable_owner: Option<TaskOwner>,
    /// OAuth client generation captured during bearer validation. Durable
    /// admission rechecks it after registering the TaskStore create guard.
    pub principal_lifecycle: Option<crate::downstream_oauth::PrincipalLifecycleLease>,
    /// Admitted peer metadata stays opaque to authorization and routing code.
    pub(crate) extension_envelope: Option<ExtensionEnvelope>,
}

impl DownstreamCallContext {
    pub fn stdio(client_id: impl Into<Arc<str>>, request_id: RequestId) -> Self {
        Self::stdio_for_client(client_id, request_id, ClientType::Unknown)
    }

    pub fn stdio_for_client(
        client_id: impl Into<Arc<str>>,
        request_id: RequestId,
        client_type: ClientType,
    ) -> Self {
        let client_id = client_id.into();
        Self {
            transport: DownstreamTransport::Stdio,
            client_id: Arc::clone(&client_id),
            request_id,
            client_type,
            trace_id: Arc::from(new_trace_id()),
            protocol_era: crate::protocol::ProtocolEra::Legacy,
            protocol_version: Arc::from(crate::protocol::SUPPORTED_PROTOCOL_VERSION),
            modern_direction_enabled: false,
            principal: Some(PrincipalId::stdio_process(stable_instance_uuid(
                "stdio", &client_id,
            ))),
            scopes: Arc::new(BTreeSet::new()),
            local_trust: true,
            client_metadata: None,
            deadline: None,
            cancellation: CancellationToken::new(),
            durable_owner: None,
            principal_lifecycle: None,
            extension_envelope: None,
        }
    }

    pub fn ipc_for_client(
        client_id: impl Into<Arc<str>>,
        request_id: RequestId,
        client_type: ClientType,
    ) -> Self {
        let client_id = client_id.into();
        Self {
            transport: DownstreamTransport::Ipc,
            client_id: Arc::clone(&client_id),
            request_id,
            client_type,
            trace_id: Arc::from(new_trace_id()),
            protocol_era: crate::protocol::ProtocolEra::Legacy,
            protocol_version: Arc::from(crate::protocol::SUPPORTED_PROTOCOL_VERSION),
            modern_direction_enabled: false,
            principal: Some(PrincipalId::daemon_ipc(stable_instance_uuid(
                "ipc", &client_id,
            ))),
            scopes: Arc::new(BTreeSet::new()),
            local_trust: true,
            client_metadata: None,
            deadline: None,
            cancellation: CancellationToken::new(),
            durable_owner: None,
            principal_lifecycle: None,
            extension_envelope: None,
        }
    }

    pub fn http(session_id: impl Into<Arc<str>>, request_id: RequestId) -> Self {
        Self::http_for_client(session_id, request_id, ClientType::Unknown)
    }

    /// HTTP is the only internet-reachable transport, so its contexts start
    /// untrusted and an adapter opts in — `with_authorization` for a bearer
    /// principal, `with_local_principal` for a loopback listener that requires
    /// no auth. Stdio and IPC are local by construction and stay trusted.
    /// An adapter that forgets to say which it is gets a context that can do
    /// nothing, rather than one that can do everything.
    pub fn http_for_client(
        session_id: impl Into<Arc<str>>,
        request_id: RequestId,
        client_type: ClientType,
    ) -> Self {
        let session_id = session_id.into();
        Self {
            transport: DownstreamTransport::Http,
            client_id: session_id,
            request_id,
            client_type,
            trace_id: Arc::from(new_trace_id()),
            protocol_era: crate::protocol::ProtocolEra::Legacy,
            protocol_version: Arc::from(crate::protocol::SUPPORTED_PROTOCOL_VERSION),
            modern_direction_enabled: false,
            principal: None,
            scopes: Arc::new(BTreeSet::new()),
            local_trust: false,
            client_metadata: None,
            deadline: None,
            cancellation: CancellationToken::new(),
            durable_owner: None,
            principal_lifecycle: None,
            extension_envelope: None,
        }
    }

    /// Untrusted by default, for the reasons on [`Self::http_for_client`].
    pub fn http_for_client_with_trace(
        session_id: impl Into<Arc<str>>,
        request_id: RequestId,
        client_type: ClientType,
        trace_id: impl Into<Arc<str>>,
    ) -> Self {
        let session_id = session_id.into();
        Self {
            transport: DownstreamTransport::Http,
            client_id: session_id,
            request_id,
            client_type,
            trace_id: trace_id.into(),
            protocol_era: crate::protocol::ProtocolEra::Legacy,
            protocol_version: Arc::from(crate::protocol::SUPPORTED_PROTOCOL_VERSION),
            modern_direction_enabled: false,
            principal: None,
            scopes: Arc::new(BTreeSet::new()),
            local_trust: false,
            client_metadata: None,
            deadline: None,
            cancellation: CancellationToken::new(),
            durable_owner: None,
            principal_lifecycle: None,
            extension_envelope: None,
        }
    }

    pub fn with_authorization(
        mut self,
        principal: PrincipalId,
        scopes: impl IntoIterator<Item = String>,
    ) -> Self {
        self.principal = Some(principal);
        self.scopes = Arc::new(scopes.into_iter().collect());
        self.local_trust = false;
        self
    }

    pub fn with_local_principal(mut self, principal: PrincipalId) -> Self {
        self.principal = Some(principal);
        self.local_trust = true;
        self
    }

    pub fn with_principal_lifecycle(
        mut self,
        lifecycle: crate::downstream_oauth::PrincipalLifecycleLease,
    ) -> Self {
        self.principal_lifecycle = Some(lifecycle);
        self
    }

    pub(crate) fn with_extension_envelope(mut self, envelope: ExtensionEnvelope) -> Self {
        self.extension_envelope = Some(envelope);
        self
    }

    pub fn with_client_metadata(
        mut self,
        name: impl Into<Arc<str>>,
        version: impl Into<Arc<str>>,
    ) -> Self {
        self.client_metadata = Some(crate::protocol::ClientMetadata {
            name: name.into(),
            version: version.into(),
        });
        self
    }

    pub fn with_protocol(
        mut self,
        era: crate::protocol::ProtocolEra,
        version: impl Into<Arc<str>>,
    ) -> Self {
        self.protocol_era = era;
        self.protocol_version = version.into();
        self
    }

    pub fn with_modern_direction_enabled(mut self, enabled: bool) -> Self {
        self.modern_direction_enabled = enabled;
        self
    }

    pub fn with_lifecycle(
        mut self,
        deadline: Option<std::time::Instant>,
        cancellation: CancellationToken,
        durable_owner: Option<TaskOwner>,
    ) -> Self {
        self.deadline = deadline;
        self.cancellation = cancellation;
        self.durable_owner = durable_owner;
        self
    }

    pub fn policy_decision(
        &self,
        family: crate::protocol::MethodFamily,
    ) -> crate::protocol::PolicyDecision {
        crate::protocol::decide_method(
            &crate::protocol::CapabilityPolicyInput {
                era: self.protocol_era,
                transport: self.transport,
                principal: self.principal.as_ref(),
                scopes: self.scopes.as_ref(),
                local_trust: self.local_trust,
                modern_direction_enabled: self.modern_direction_enabled,
                bridge_implemented: false,
            },
            family,
        )
    }

    pub fn authorize(&self, family: crate::protocol::MethodFamily) -> Result<(), rmcp::ErrorData> {
        match self.policy_decision(family) {
            crate::protocol::PolicyDecision::Allow => Ok(()),
            crate::protocol::PolicyDecision::Deny(outcome) => {
                Err(outcome.into_error(self.protocol_era))
            }
        }
    }

    fn authorize_continuation(&self) -> Result<(), rmcp::ErrorData> {
        match crate::protocol::decide_method(
            &crate::protocol::CapabilityPolicyInput {
                era: self.protocol_era,
                transport: self.transport,
                principal: self.principal.as_ref(),
                scopes: self.scopes.as_ref(),
                local_trust: self.local_trust,
                modern_direction_enabled: self.modern_direction_enabled,
                bridge_implemented: true,
            },
            crate::protocol::MethodFamily::Continuations,
        ) {
            crate::protocol::PolicyDecision::Allow => Ok(()),
            crate::protocol::PolicyDecision::Deny(outcome) => {
                Err(outcome.into_error(self.protocol_era))
            }
        }
    }

    fn ensure_principal_lifecycle_active(&self) -> Result<(), rmcp::ErrorData> {
        if self
            .principal_lifecycle
            .as_ref()
            .is_some_and(|lifecycle| !lifecycle.is_active())
        {
            Err(crate::protocol::ProtocolOutcome::AuthorizationRequired
                .into_error(self.protocol_era))
        } else {
            Ok(())
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
            DownstreamTransport::Ipc => NotificationTarget::Ipc {
                client_id: Arc::clone(&self.client_id),
            },
        }
    }
}

fn stable_instance_uuid(namespace: &str, value: &str) -> Uuid {
    let mut first = std::collections::hash_map::DefaultHasher::new();
    namespace.hash(&mut first);
    value.hash(&mut first);
    let mut second = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut second);
    namespace.hash(&mut second);
    0x706c_7567_u32.hash(&mut second);
    Uuid::from_u128(((first.finish() as u128) << 64) | second.finish() as u128)
}

static NEXT_TRACE_ID: AtomicU64 = AtomicU64::new(1);

/// Generate an OpenTelemetry/W3C-compatible 16-byte trace id as 32 lowercase hex chars.
pub fn new_trace_id() -> String {
    let id = NEXT_TRACE_ID.fetch_add(1, Ordering::Relaxed);
    format!("0000000000000000{id:016x}")
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
    downstream_progress_token: Option<ProgressToken>,
    upstream_progress_token: Option<ProgressToken>,
    pending_cancel_reason: Option<Option<String>>,
}

#[derive(Clone)]
struct ToolRoundInput {
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
    input_responses: Option<InputResponses>,
    request_state: Option<String>,
    expected_route: Option<RouteIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RouteIdentity {
    server_id: String,
    original_name: String,
    route_generation: u64,
    upstream_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MaterialToolRoute {
    server_id: String,
    original_name: String,
    upstream_generation: Option<u64>,
}

struct NativeContinuation {
    upstream_request_state: Option<String>,
    input_requests: InputRequests,
    route: RouteIdentity,
    upstream: Arc<crate::server::UpstreamServer>,
    round: usize,
    chain_expires_at: std::time::Instant,
}

#[cfg(test)]
pub(crate) struct ContinuationInsertGate {
    pub entered: Arc<std::sync::Barrier>,
    pub release: Arc<std::sync::Barrier>,
}

/// Abstraction for forwarding reverse requests (elicitation, sampling) to a downstream client.
///
/// Each transport implements this trait differently:
/// - stdio: calls `Peer<RoleServer>` methods directly
/// - HTTP: sends JSON-RPC request via SSE, awaits POST response via oneshot channel
/// - daemon IPC: sends `IpcClientRequest` to the proxy over Unix socket
pub trait DownstreamBridge: Send + Sync {
    fn create_elicitation(
        &self,
        request: ElicitRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<ElicitResult, McpError>> + Send + '_>>;

    fn create_message(
        &self,
        request: CreateMessageRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<CreateMessageResult, McpError>> + Send + '_>>;
}

impl ToolRouter {
    pub fn new(server_manager: Arc<ServerManager>, config: RouterConfig) -> Self {
        Self::new_with_quotas(
            server_manager,
            config,
            crate::protocol::AdmissionQuotas::new(Default::default()),
        )
    }

    fn new_with_quotas(
        server_manager: Arc<ServerManager>,
        config: RouterConfig,
        admission_quotas: crate::protocol::AdmissionQuotas,
    ) -> Self {
        let (protocol_notification_tx, _) = broadcast::channel(128);
        let (logging_tx, _) = broadcast::channel(512);
        let cache = Arc::new(ArcSwap::from_pointee(RouterSnapshot {
            routes: HashMap::new(),
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),
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
            tool_risk_inventory: HashMap::new(),
        }));
        let resource_subscriptions = subscriptions::SubscriptionRegistry::new();
        // Drains resolve an entry's recorded owning server to a live handle
        // at drain time — never trusting a handle resolved earlier from the
        // route cache, which can point at the wrong server on either side
        // of a refresh_tools snapshot publish.
        {
            let server_manager = Arc::clone(&server_manager);
            resource_subscriptions.set_owner_resolver(Arc::new(move |server_id: &str| {
                server_manager
                    .get_upstream(server_id)
                    .map(subscriptions::as_upstream_ops)
            }));
        }
        // Post-confirm heal: a subscribe transition can confirm against an
        // owner the published snapshot no longer routes the URI to (the
        // publish landed mid-transition, and that pass classified before
        // the entry existed). The registry invokes this hook from the
        // detached transition task, so the heal fires even when the
        // downstream caller's request future was cancelled. The hook body
        // only spawns: the rebind acquires the same per-URI transition lock
        // the confirming transition still holds, so it must never run
        // inline here.
        {
            let server_manager = Arc::clone(&server_manager);
            let cache = Arc::clone(&cache);
            // The registry stores this hook, so it must capture the registry
            // weakly. A strong capture forms registry -> hook -> registry and
            // retains the router's subscription/cache/server graph forever.
            let registry = Arc::downgrade(&resource_subscriptions);
            resource_subscriptions.set_post_confirm_hook(Arc::new(
                move |uri: String, confirmed_owner: String| {
                    let server_manager = Arc::clone(&server_manager);
                    let cache = Arc::clone(&cache);
                    let Some(registry) = registry.upgrade() else {
                        return;
                    };
                    tokio::spawn(async move {
                        let published_route = cache.load().resource_routes.get(&uri).cloned();
                        let Some(new_server_id) = published_route else {
                            // Route vanished entirely: the next refresh's
                            // prune plus the recorded owner drain it.
                            return;
                        };
                        if new_server_id == confirmed_owner {
                            return;
                        }
                        tracing::debug!(
                            uri = %uri,
                            old_server = %confirmed_owner,
                            new_server = %new_server_id,
                            "subscribe confirmed against stale owner; rebinding to current owner"
                        );
                        if let Err(error) = Self::rebind_subscription_route_with(
                            &server_manager,
                            &registry,
                            &uri,
                            &confirmed_owner,
                            &new_server_id,
                        )
                        .await
                        {
                            tracing::warn!(
                                uri = %uri,
                                error = %error,
                                "post-confirm subscription heal failed to rebind"
                            );
                        }
                    });
                },
            ));
        }
        let continuation_registry =
            continuations::ContinuationRegistry::new(admission_quotas.clone(), MRTR_CHAIN_TTL);
        Self {
            server_manager,
            activity_store: crate::activity::ActivityStore::default(),
            cache,
            published_tool_routes: std::sync::Mutex::new(HashMap::new()),
            config,
            event_tx: None,
            protocol_notification_tx,
            logging_tx,
            client_log_levels: DashMap::new(),
            effective_log_level: ArcSwap::from_pointee(LoggingLevel::Warning),
            active_calls: DashMap::new(),
            active_call_lookup: DashMap::new(),
            upstream_request_lookup: DashMap::new(),
            upstream_progress_lookup: DashMap::new(),
            notification_refresh_in_progress: AtomicBool::new(false),
            notification_refresh_pending: AtomicBool::new(false),
            pending_tool_list_changed: AtomicBool::new(false),
            pending_resource_list_changed: AtomicBool::new(false),
            pending_prompt_list_changed: AtomicBool::new(false),
            engine: std::sync::RwLock::new(None),
            artifact_store: ArtifactStore::new(),
            resource_subscriptions,
            client_roots: DashMap::new(),
            downstream_bridges: DashMap::new(),
            lazy_working_sets: DashMap::new(),
            task_store: Mutex::new(TaskStore::new()),
            admission_quotas,
            modern_downstream_enabled: AtomicBool::new(false),
            refresh_reconcile_lock: Mutex::new(()),
            continuation_registry,
            #[cfg(test)]
            continuation_insert_gate: std::sync::Mutex::new(None),
        }
    }

    pub fn record_activity(&self, event: crate::activity::ActivityEvent) {
        self.activity_store.record(event);
    }

    pub fn activity_snapshot(
        &self,
        filter: &crate::activity::ActivityFilter,
    ) -> Vec<crate::activity::ActivityEvent> {
        self.activity_store.snapshot(filter)
    }

    #[cfg(test)]
    pub(crate) fn new_with_admission_quotas_for_tests(
        server_manager: Arc<ServerManager>,
        config: RouterConfig,
        admission_quotas: crate::protocol::AdmissionQuotas,
    ) -> Self {
        Self::new_with_quotas(server_manager, config, admission_quotas)
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

    pub fn set_modern_downstream_enabled(&self, enabled: bool) {
        self.modern_downstream_enabled
            .store(enabled, Ordering::Release);
        if !enabled {
            self.continuation_registry.clear();
        }
    }

    pub fn modern_downstream_enabled(&self) -> bool {
        self.modern_downstream_enabled.load(Ordering::Acquire)
    }

    pub fn clear_continuations(&self) {
        self.continuation_registry.clear();
    }

    pub fn revoke_continuations_for_principal(&self, principal: &PrincipalId) {
        self.continuation_registry.revoke_principal(principal);
    }

    #[cfg(test)]
    pub(crate) fn set_continuation_insert_gate(&self, gate: Option<Arc<ContinuationInsertGate>>) {
        *self
            .continuation_insert_gate
            .lock()
            .expect("continuation insert gate mutex poisoned") = gate;
    }

    /// Fail-closed MRTR admission before a tools/call can reach an upstream.
    /// Mixed-era ownership transfer and task input are intentionally dormant
    /// until their parked-call coordinators exist; native modern rounds may
    /// pass through without Plug retaining continuation state.
    pub fn ensure_tool_round_supported(
        &self,
        tool_name: &str,
        downstream: &DownstreamCallContext,
        has_continuation: bool,
        task_requested: bool,
    ) -> Result<(), McpError> {
        if task_requested
            && downstream.protocol_era == crate::protocol::ProtocolEra::Legacy
            && has_continuation
        {
            return Err(crate::protocol::ProtocolOutcome::UnsupportedBridge
                .into_error(downstream.protocol_era));
        }
        if canonical_plug_meta_tool_name(tool_name).is_some() {
            if has_continuation {
                return Err(crate::protocol::ProtocolOutcome::UnsupportedBridge
                    .into_error(downstream.protocol_era));
            }
            return Ok(());
        }
        let cache = self.cache.load();
        let server_id = cache
            .resolve_route(tool_name)
            .map(|(server_id, _)| server_id.clone());
        drop(cache);
        let Some(server_id) = server_id else {
            // Legacy task wrappers intentionally create their durable record
            // before resolving a missing route, so teardown can still own a
            // create racing session deletion. Only a resolved modern route
            // needs the new pre-create pair rejection below.
            if task_requested && downstream.protocol_era == crate::protocol::ProtocolEra::Legacy {
                return Ok(());
            }
            return Err(McpError::from(ProtocolError::ToolNotFound {
                tool_name: tool_name.to_string(),
            }));
        };
        let Some(upstream_era) = self
            .server_manager
            .get_upstream(&server_id)
            .map(|upstream| upstream.protocol_era)
        else {
            // Preserve legacy task enqueue semantics: the task owns its later
            // upstream-unavailable result. Synchronous calls still fail in the
            // router before any upstream effect.
            if task_requested {
                return Ok(());
            }
            return Err(McpError::from(ProtocolError::ServerUnavailable {
                server_id,
            }));
        };

        if upstream_era == crate::protocol::ProtocolEra::Modern {
            // A modern upstream may require another round even when this first
            // request carries no continuation fields. Admit durable ownership
            // before the initial tool can produce side effects.
            downstream.authorize_continuation()?;
        }

        if !mrtr_pair_supported(
            downstream.protocol_era,
            upstream_era,
            has_continuation,
            task_requested,
        ) {
            return Err(crate::protocol::ProtocolOutcome::UnsupportedBridge
                .into_error(downstream.protocol_era));
        }
        Ok(())
    }

    fn tool_route_identity(&self, tool_name: &str) -> Result<(RouteIdentity, u64), McpError> {
        let (resolved, global_generation) = self
            .continuation_registry
            .capture_with_global_generation(|| -> Result<_, McpError> {
                let cache = self.cache.load_full();
                let (server_id, original_name) =
                    cache.resolve_route(tool_name).cloned().ok_or_else(|| {
                        McpError::from(ProtocolError::ToolNotFound {
                            tool_name: tool_name.to_string(),
                        })
                    })?;
                let upstream = self
                    .server_manager
                    .get_upstream(&server_id)
                    .ok_or_else(|| {
                        McpError::from(ProtocolError::ServerUnavailable {
                            server_id: server_id.clone(),
                        })
                    })?;
                Ok((server_id, original_name, upstream.generation()))
            });
        let (server_id, original_name, upstream_generation) = resolved?;
        Ok((
            RouteIdentity {
                server_id,
                original_name,
                route_generation: global_generation,
                upstream_generation,
            },
            global_generation,
        ))
    }

    fn publish_route_snapshot(&self, snapshot: Arc<RouterSnapshot>) {
        let needs_indexes = (snapshot.routes_lower.is_empty() && !snapshot.routes.is_empty())
            || (snapshot.tools_by_name.is_empty() && !snapshot.tools_all.is_empty());
        let snapshot = if needs_indexes {
            Arc::new((*snapshot).clone().with_indexes())
        } else {
            snapshot
        };
        let material_routes = snapshot
            .routes
            .iter()
            .map(|(name, (server_id, original_name))| {
                let upstream_generation = self
                    .server_manager
                    .get_upstream(server_id)
                    .map(|upstream| upstream.generation());
                (
                    name.clone(),
                    MaterialToolRoute {
                        server_id: server_id.clone(),
                        original_name: original_name.clone(),
                        upstream_generation,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let mut published = self
            .published_tool_routes
            .lock()
            .expect("published route mutex poisoned");
        let invalidate = *published != material_routes;
        self.continuation_registry
            .publish_with_invalidation(invalidate, || {
                *published = material_routes;
                self.cache.store(snapshot);
            });
    }

    /// Reserve one process-wide modern request slot before adapter code can
    /// trigger routing, durable state, or upstream effects. Anonymous local
    /// transports receive a stable process-scoped quota identity; durable
    /// method policy still independently requires an authenticated principal.
    pub fn admit_modern_request(
        &self,
        context: &DownstreamCallContext,
    ) -> Result<crate::protocol::QuotaLease, McpError> {
        let principal = context.principal.clone().unwrap_or_else(|| {
            PrincipalId::stdio_process(stable_instance_uuid(
                "modern-request",
                context.client_id.as_ref(),
            ))
        });
        self.admission_quotas
            .try_acquire(
                &principal,
                crate::protocol::QuotaResource::ConcurrentModernRequests,
                1,
            )
            .map_err(|outcome| outcome.into_error(context.protocol_era))
    }

    // ── Logging channel ──────────────────────────────────────────────────

    pub fn subscribe_logging(&self) -> broadcast::Receiver<ProtocolNotification> {
        self.logging_tx.subscribe()
    }

    pub fn prune_artifacts(&self) {
        self.artifact_store.prune();
    }

    /// Map LoggingLevel to numeric severity (Debug=0 .. Emergency=7).
    pub fn level_severity(level: LoggingLevel) -> u8 {
        match level {
            LoggingLevel::Debug => 0,
            LoggingLevel::Info => 1,
            LoggingLevel::Notice => 2,
            LoggingLevel::Warning => 3,
            LoggingLevel::Error => 4,
            LoggingLevel::Critical => 5,
            LoggingLevel::Alert => 6,
            LoggingLevel::Emergency => 7,
        }
    }

    /// Route a logging message from an upstream server to all downstream clients.
    /// Filters by current effective log level and prefixes logger with server_id.
    pub fn route_upstream_logging_message(
        &self,
        server_id: &str,
        mut params: LoggingMessageNotificationParam,
    ) {
        let effective = **self.effective_log_level.load();
        if Self::level_severity(params.level) < Self::level_severity(effective) {
            return;
        }

        let original_logger = params.logger.as_deref().unwrap_or("default");
        params.logger = Some(format!("{server_id}:{original_logger}"));

        let _ = self
            .logging_tx
            .send(ProtocolNotification::LoggingMessage { params });
    }

    /// Get the current effective log level.
    pub fn log_level(&self) -> LoggingLevel {
        **self.effective_log_level.load()
    }

    /// Set a client's requested log level and recalculate the effective level.
    pub fn set_client_log_level(&self, client_id: &str, level: LoggingLevel) {
        self.client_log_levels.insert(Arc::from(client_id), level);
        self.recalculate_effective_level();
    }

    /// Remove a client's log level (on disconnect) and recalculate.
    pub fn remove_client_log_level(&self, client_id: &str) {
        self.client_log_levels.remove(client_id);
        self.recalculate_effective_level();
    }

    pub fn clear_lazy_session(&self, session_key: &str) {
        self.lazy_working_sets.remove(session_key);
    }

    pub fn lazy_session_key(transport: DownstreamTransport, client_id: &str) -> String {
        match transport {
            DownstreamTransport::Stdio => format!("stdio:{client_id}"),
            DownstreamTransport::Http => format!("http:{client_id}"),
            DownstreamTransport::Ipc => format!("ipc:{client_id}"),
        }
    }

    /// Recalculate effective level as the most permissive (lowest severity) across all clients.
    /// Defaults to Warning when no clients have set a level.
    fn recalculate_effective_level(&self) {
        let effective = self
            .client_log_levels
            .iter()
            .map(|entry| *entry.value())
            .min_by_key(|level| Self::level_severity(*level))
            .unwrap_or(LoggingLevel::Warning);
        self.effective_log_level.store(Arc::new(effective));
    }

    /// Forward the current effective log level to all healthy upstream servers concurrently.
    pub async fn forward_set_level_to_upstreams(&self) {
        let level = self.log_level();
        let params = SetLevelRequestParams::new(level);
        let upstreams = self.server_manager.healthy_upstreams();
        let futures: Vec<_> = upstreams
            .into_iter()
            .filter(|(_, upstream)| upstream.capabilities.logging.is_some())
            .map(|(name, upstream)| {
                let params = params.clone();
                async move {
                    if let Err(error) = upstream.client.peer().set_level(params).await {
                        tracing::warn!(
                            server = %name,
                            error = %error,
                            "failed to forward setLevel to upstream"
                        );
                    }
                }
            })
            .collect();
        futures::future::join_all(futures).await;
    }

    // ── Roots cache ──────────────────────────────────────────────────────

    /// Update cached roots for a downstream client. Returns true if the roots changed.
    /// Uses `DashMap::entry()` for atomic check-and-set within a single shard lock.
    pub fn set_roots_for_target(&self, target: NotificationTarget, roots: Vec<Root>) -> bool {
        use dashmap::mapref::entry::Entry;
        match self.client_roots.entry(target) {
            Entry::Occupied(mut e) => {
                if *e.get() == roots {
                    return false;
                }
                e.insert(roots);
                true
            }
            Entry::Vacant(e) => {
                e.insert(roots);
                true
            }
        }
    }

    /// Remove cached roots for a disconnected client. Returns true if entry existed.
    pub fn clear_roots_for_target(&self, target: &NotificationTarget) -> bool {
        self.client_roots.remove(target).is_some()
    }

    /// Return the union of all connected clients' roots, deduplicated by URI.
    /// Note: DashMap iteration is not a point-in-time snapshot; the result is
    /// eventually consistent, which is acceptable for a roots list.
    pub fn list_roots_union(&self) -> ListRootsResult {
        let mut by_uri: HashMap<String, Root> = HashMap::new();
        for entry in self.client_roots.iter() {
            for root in entry.value().iter() {
                by_uri
                    .entry(root.uri.clone())
                    .or_insert_with(|| root.clone());
            }
        }
        let mut roots: Vec<Root> = by_uri.into_values().collect();
        roots.sort_by(|a, b| a.uri.cmp(&b.uri));
        let mut result = ListRootsResult::default();
        result.roots = roots;
        result
    }

    /// Notify all healthy upstream servers that roots have changed.
    pub async fn forward_roots_list_changed_to_upstreams(&self) {
        let upstreams = self.server_manager.healthy_upstreams();
        let futures: Vec<_> = upstreams
            .into_iter()
            .map(|(name, upstream)| async move {
                if let Err(error) = upstream.client.peer().notify_roots_list_changed().await {
                    tracing::debug!(
                        server = %name,
                        error = %error,
                        "failed to forward roots/list_changed to upstream"
                    );
                }
            })
            .collect();
        futures::future::join_all(futures).await;
    }

    // ── Downstream bridge management ────────────────────────────────────

    /// Register a downstream bridge for reverse-request forwarding.
    pub fn register_downstream_bridge(
        &self,
        target: NotificationTarget,
        bridge: Arc<dyn DownstreamBridge>,
    ) {
        self.downstream_bridges.insert(target, bridge);
    }

    /// Unregister a downstream bridge on client disconnect.
    pub fn unregister_downstream_bridge(&self, target: &NotificationTarget) {
        self.downstream_bridges.remove(target);
    }

    fn active_call_for_upstream_request(
        &self,
        server_id: &str,
        request_id: &RequestId,
    ) -> Result<ActiveCallRecord, McpError> {
        let key = UpstreamRequestKey {
            server_id: server_id.to_string(),
            request_id: request_id.clone(),
        };
        let Some(call_id) = self.upstream_request_lookup.get(&key).map(|entry| *entry) else {
            return Err(McpError::internal_error(
                format!(
                    "no active downstream call for upstream request {server_id}:{request_id:?}"
                ),
                None,
            ));
        };

        self.active_calls
            .get(&call_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| {
                McpError::internal_error(
                    format!(
                        "active call record missing for upstream request {server_id}:{request_id:?}"
                    ),
                    None,
                )
            })
    }

    fn resolve_unique_downstream_target_for_upstream(
        &self,
        server_id: &str,
    ) -> Result<NotificationTarget, McpError> {
        let targets: HashSet<NotificationTarget> = self
            .active_calls
            .iter()
            .filter(|entry| entry.upstream_server_id == server_id)
            .map(|entry| entry.downstream.notification_target())
            .collect();

        match targets.len() {
            0 => Err(McpError::internal_error(
                format!("no active downstream call for upstream server {server_id}"),
                None,
            )),
            1 => Ok(targets.into_iter().next().unwrap()),
            _ => Err(McpError::internal_error(
                format!("ambiguous downstream ownership for upstream server {server_id}"),
                None,
            )),
        }
    }

    /// Forward an upstream elicitation request to the downstream client that triggered the tool call.
    pub async fn create_elicitation_from_upstream(
        &self,
        server_id: &str,
        upstream_request_id: RequestId,
        request: ElicitRequestParams,
    ) -> Result<ElicitResult, McpError> {
        let target = match self.active_call_for_upstream_request(server_id, &upstream_request_id) {
            Ok(record) => record.downstream.notification_target(),
            Err(_) => self.resolve_unique_downstream_target_for_upstream(server_id)?,
        };
        let bridge = self
            .downstream_bridges
            .get(&target)
            .map(|entry| Arc::clone(&*entry))
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("no downstream bridge registered for target {target:?}"),
                    None,
                )
            })?;
        bridge.create_elicitation(request).await
    }

    /// Forward an upstream sampling request to the downstream client that triggered the tool call.
    pub async fn create_message_from_upstream(
        &self,
        server_id: &str,
        upstream_request_id: RequestId,
        request: CreateMessageRequestParams,
    ) -> Result<CreateMessageResult, McpError> {
        let target = match self.active_call_for_upstream_request(server_id, &upstream_request_id) {
            Ok(record) => record.downstream.notification_target(),
            Err(_) => self.resolve_unique_downstream_target_for_upstream(server_id)?,
        };
        let bridge = self
            .downstream_bridges
            .get(&target)
            .map(|entry| Arc::clone(&*entry))
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("no downstream bridge registered for target {target:?}"),
                    None,
                )
            })?;
        bridge.create_message(request).await
    }

    fn schedule_list_changed_refresh(self: &Arc<Self>, notification: ProtocolNotification) {
        match notification {
            ProtocolNotification::ToolListChanged => {
                self.pending_tool_list_changed.store(true, Ordering::SeqCst);
            }
            ProtocolNotification::ResourceListChanged => {
                self.pending_resource_list_changed
                    .store(true, Ordering::SeqCst);
            }
            ProtocolNotification::PromptListChanged => {
                self.pending_prompt_list_changed
                    .store(true, Ordering::SeqCst);
            }
            _ => {}
        }

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
            // If this task exits without cleanly clearing the in-progress
            // flag — a panic, or a cancellation — reset it so future
            // refreshes can still be scheduled. Without this, a single
            // abnormal exit permanently wedges `list_changed` delivery.
            struct RefreshGuard {
                router: Arc<ToolRouter>,
                armed: bool,
            }
            impl Drop for RefreshGuard {
                fn drop(&mut self) {
                    if self.armed {
                        self.router
                            .notification_refresh_in_progress
                            .store(false, Ordering::SeqCst);
                    }
                }
            }
            let mut guard = RefreshGuard {
                router: Arc::clone(&router),
                armed: true,
            };

            loop {
                router
                    .notification_refresh_pending
                    .store(false, Ordering::SeqCst);
                tokio::time::sleep(LIST_CHANGED_REFRESH_DEBOUNCE).await;

                // Bound the refresh as a backstop: per-server listing is
                // already timeout-bounded, but an unforeseen hang here must
                // not wedge the flag. On timeout, log and continue so the
                // task keeps making forward progress.
                if tokio::time::timeout(LIST_CHANGED_REFRESH_MAX, router.refresh_tools())
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        "tool refresh exceeded {}s backstop; retrying on next change",
                        LIST_CHANGED_REFRESH_MAX.as_secs()
                    );
                }

                if router
                    .pending_tool_list_changed
                    .swap(false, Ordering::SeqCst)
                {
                    router.publish_protocol_notification(ProtocolNotification::ToolListChanged);
                }
                if router
                    .pending_resource_list_changed
                    .swap(false, Ordering::SeqCst)
                {
                    router.publish_protocol_notification(ProtocolNotification::ResourceListChanged);
                }
                if router
                    .pending_prompt_list_changed
                    .swap(false, Ordering::SeqCst)
                {
                    router.publish_protocol_notification(ProtocolNotification::PromptListChanged);
                }

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

                // Clean exit: the flag was just cleared above, so disarm the
                // guard to avoid a redundant store.
                guard.armed = false;
                break;
            }
        });
    }

    pub fn schedule_tool_list_changed_refresh(self: &Arc<Self>) {
        self.schedule_list_changed_refresh(ProtocolNotification::ToolListChanged);
    }

    pub fn schedule_resource_list_changed_refresh(self: &Arc<Self>) {
        self.schedule_list_changed_refresh(ProtocolNotification::ResourceListChanged);
    }

    pub fn schedule_prompt_list_changed_refresh(self: &Arc<Self>) {
        self.schedule_list_changed_refresh(ProtocolNotification::PromptListChanged);
    }

    #[cfg(test)]
    pub(crate) fn active_call_count(&self) -> usize {
        self.active_calls.len()
    }

    #[cfg(test)]
    pub(crate) fn replace_snapshot(&self, snapshot: RouterSnapshot) {
        self.publish_route_snapshot(Arc::new(snapshot.with_indexes()));
    }

    fn register_active_call(&self, call_id: u64, record: ActiveCallRecord) -> Result<(), McpError> {
        let downstream_key = DownstreamCallKey::from(&record.downstream);
        self.active_calls.insert(call_id, record.clone());
        match self.active_call_lookup.entry(downstream_key) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                self.active_calls.remove(&call_id);
                return Err(crate::protocol::ProtocolOutcome::RetryableTransition
                    .into_error(record.downstream.protocol_era));
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(call_id);
            }
        }
        if let Some(request_id) = record.upstream_request_id.clone() {
            self.upstream_request_lookup.insert(
                UpstreamRequestKey {
                    server_id: record.upstream_server_id.clone(),
                    request_id,
                },
                call_id,
            );
        }
        if let Some(progress_token) = record.upstream_progress_token.clone() {
            self.upstream_progress_lookup.insert(
                UpstreamProgressKey {
                    server_id: record.upstream_server_id.clone(),
                    progress_token,
                },
                call_id,
            );
        }
        Ok(())
    }

    fn attach_upstream_request_id(&self, call_id: u64, server_id: &str, request_id: RequestId) {
        let mut pending_cancel = None;
        if let Some(mut entry) = self.active_calls.get_mut(&call_id) {
            entry.upstream_request_id = Some(request_id.clone());
            pending_cancel = entry.pending_cancel_reason.take();
        }
        self.upstream_request_lookup.insert(
            UpstreamRequestKey {
                server_id: server_id.to_string(),
                request_id: request_id.clone(),
            },
            call_id,
        );
        if let Some(reason) = pending_cancel
            && let Some(upstream) = self.server_manager.get_upstream(server_id)
        {
            let peer = upstream.client.peer().clone();
            let request_id = request_id.clone();
            tokio::spawn(async move {
                if let Err(error) = peer
                    .notify_cancelled(CancelledNotificationParam::new(Some(request_id), reason))
                    .await
                {
                    tracing::warn!(error = %error, "failed to forward pending downstream cancellation upstream");
                }
            });
        }
    }

    fn remove_active_call(&self, call_id: u64) {
        if let Some((_, record)) = self.active_calls.remove(&call_id) {
            self.active_call_lookup.remove_if(
                &DownstreamCallKey::from(&record.downstream),
                |_, registered| *registered == call_id,
            );
            if let Some(request_id) = record.upstream_request_id {
                self.upstream_request_lookup.remove_if(
                    &UpstreamRequestKey {
                        server_id: record.upstream_server_id.clone(),
                        request_id,
                    },
                    |_, registered| *registered == call_id,
                );
            }
            if let Some(progress_token) = record.upstream_progress_token {
                self.upstream_progress_lookup.remove_if(
                    &UpstreamProgressKey {
                        server_id: record.upstream_server_id,
                        progress_token,
                    },
                    |_, registered| *registered == call_id,
                );
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

        let Some(mut record) = self.active_calls.get_mut(&call_id) else {
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
            record.pending_cancel_reason = Some(reason);
            return;
        };

        let peer = upstream.client.peer().clone();
        tokio::spawn(async move {
            if let Err(error) = peer
                .notify_cancelled(CancelledNotificationParam::new(Some(request_id), reason))
                .await
            {
                tracing::warn!(error = %error, "failed to forward downstream cancellation upstream");
            }
        });
    }

    /// Cancel an active request owned by the supplied downstream context.
    ///
    /// Transport adapters use this narrow entry point when their own request
    /// lifecycle ends. The router keeps the upstream-call lookup and
    /// cancellation forwarding details private.
    pub fn cancel_downstream_request(
        &self,
        context: &DownstreamCallContext,
        reason: Option<String>,
    ) {
        self.forward_cancel_from_downstream(context, reason);
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

        let mut downstream_params = params;
        if let Some(token) = record.downstream_progress_token.clone() {
            downstream_params.progress_token = token;
        }

        self.publish_protocol_notification(ProtocolNotification::Progress {
            target: record.downstream.notification_target(),
            params: downstream_params,
        });
    }

    pub(crate) fn route_upstream_cancelled(
        &self,
        server_id: &str,
        params: CancelledNotificationParam,
    ) {
        let Some(request_id) = params.request_id.clone() else {
            tracing::debug!(
                server = %server_id,
                reason = ?params.reason,
                "ignoring anonymous upstream cancellation"
            );
            return;
        };
        let key = UpstreamRequestKey {
            server_id: server_id.to_string(),
            request_id,
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
        self.resource_subscriptions.set_engine(engine.clone());
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
    /// swapped atomically to prevent torn reads. The three live catalog
    /// families (resources, resource templates, prompts) are fetched
    /// concurrently; each is already server-concurrent internally, so a
    /// refresh's upstream latency is the max of the three instead of their
    /// sum.
    pub async fn refresh_tools(&self) {
        let upstream_tools = self.server_manager.get_tools().await;
        let (resources_result, resource_templates_result, prompts_result) = tokio::join!(
            self.server_manager.get_resources(),
            self.server_manager.get_resource_templates(),
            self.server_manager.get_prompts(),
        );

        // Recompute per-server availability from this cycle's listing outcomes.
        // A server served from last-known-good cache (its live listing was
        // unavailable) is degraded; its carried-forward entries below keep its
        // URI set unchanged, so the subscription prune logic leaves it untouched.
        let mut degraded_servers = std::collections::BTreeSet::new();
        degraded_servers.extend(resources_result.degraded.iter().cloned());
        degraded_servers.extend(resource_templates_result.degraded.iter().cloned());
        degraded_servers.extend(prompts_result.degraded.iter().cloned());
        self.server_manager.update_availability(&degraded_servers);

        let upstream_resources = resources_result.items;
        let upstream_resource_templates = resource_templates_result.items;
        let upstream_prompts = prompts_result.items;

        // ── Pass 1: classify, sanitize, and try keyword stripping ──
        // Each entry: (server_name, tool, prefix, stripped_name, full_name, matched_keyword)
        struct Classified {
            server_name: String,
            tool: Tool,
            prefix: String,
            stripped_name: String,
            full_name: String,
        }

        // Per-server data resolved at most once per refresh — the distinct
        // server count is far smaller than the tool count, so this replaces
        // up to 3 `server_manager` lookups (and a `tool_groups` Vec clone)
        // PER TOOL with at most one lookup PER SERVER. Reused in pass 3 for
        // icons so that pass doesn't re-hit `get_upstream_metadata` either.
        struct ServerRefreshCtx {
            upstream: Option<Arc<crate::server::UpstreamServer>>,
            tool_group_rules: Option<Vec<crate::config::ToolGroupRule>>,
            icons: Option<Vec<Icon>>,
        }

        let mut server_ctx: HashMap<String, ServerRefreshCtx> = HashMap::new();
        let mut classified: Vec<Classified> = Vec::new();
        let mut catalog_metadata_budget = CatalogMetadataBudget::default();
        let resolve_server_icons = |server_ctx: &HashMap<String, ServerRefreshCtx>,
                                    server_name: &str|
         -> Option<Vec<Icon>> {
            server_ctx
                .get(server_name)
                .and_then(|ctx| ctx.icons.clone())
                .or_else(|| {
                    self.server_manager
                        .get_upstream_metadata(server_name)
                        .and_then(|metadata| metadata.icons)
                })
        };

        for (server_name, mut tool) in upstream_tools {
            // Peer metadata is admitted before the descriptor enters any
            // cache, enrichment, fingerprint, or operator-observable path.
            catalog_metadata_budget.admit(&server_name, &mut tool.meta, "tool");
            let mut exposed_name = tool.name.to_string();

            let ctx = server_ctx.entry(server_name.clone()).or_insert_with(|| {
                let upstream = self.server_manager.get_upstream(&server_name);
                let tool_group_rules = upstream.as_ref().and_then(|u| {
                    if !u.config.tool_groups.is_empty() {
                        Some(u.config.tool_groups.clone())
                    } else if server_name == "workspace" {
                        Some(crate::tool_naming::default_workspace_rules())
                    } else {
                        None
                    }
                });
                let icons = self
                    .server_manager
                    .get_upstream_metadata(&server_name)
                    .and_then(|metadata| metadata.icons);
                ServerRefreshCtx {
                    upstream,
                    tool_group_rules,
                    icons,
                }
            });

            // 1. Apply manual renames if any
            if let Some(upstream) = ctx.upstream.as_ref()
                && let Some(new_name) = upstream.config.tool_renames.get(&exposed_name)
            {
                exposed_name = new_name.clone();
            }

            // 2. Sanitize to snake_case (hyphens, camelCase, dots -> snake_case)
            let sanitized = crate::tool_naming::sanitize_tool_name(&exposed_name);

            // 3. Determine prefix and tool name via rules or server name
            let (prefix, full_name, stripped_name) = if let Some(ref rules) = ctx.tool_group_rules {
                match crate::tool_naming::classify_with_rules(&sanitized, rules) {
                    Some(result) => {
                        let stripped = crate::tool_naming::strip_keywords(
                            &result.name,
                            &result.strip_keywords,
                        );
                        (result.prefix, result.name, stripped)
                    }
                    None => {
                        let prefix = crate::tool_naming::format_server_prefix(&server_name);
                        (prefix, sanitized.clone(), sanitized.clone())
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

                (prefix, name.clone(), name)
            };

            classified.push(Classified {
                server_name,
                tool,
                prefix,
                stripped_name,
                full_name,
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
        let mut tool_risk_inventory = HashMap::new();

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

            let upstream_annotations = c.tool.annotations.clone();
            let mut prefixed_tool = c.tool;
            let upstream_icons = resolve_server_icons(&server_ctx, &c.server_name);

            let inferred_annotations = {
                let mut inferred = prefixed_tool.clone();
                inferred.name = Cow::Owned(final_name.clone());
                inferred.annotations = None;
                crate::enrichment::normalize_annotations(&mut inferred, &final_name);
                inferred.annotations
            };

            // Enrich BEFORE setting wire name (so get_* patterns match)
            if self.config.enrichment_servers.contains(&c.server_name) {
                prefixed_tool.name = Cow::Owned(final_name.clone());
                crate::enrichment::enrich_tool(&mut prefixed_tool);
            }

            crate::enrichment::normalize_annotations(&mut prefixed_tool, &final_name);

            // Display titles follow the same disambiguation path as wire names.
            let title_name = crate::tool_naming::generate_title(&c.prefix, &final_name);

            // Set wire name and canonical display metadata
            prefixed_tool.name = Cow::Owned(prefixed_name.clone());
            apply_canonical_tool_title(&mut prefixed_tool, title_name);
            prefixed_tool.icons =
                normalized_icons_with_fallback(prefixed_tool.icons.as_deref(), upstream_icons);

            // Strip optional fields for token efficiency
            strip_optional_fields(&mut prefixed_tool, self.config.tool_description_max_chars);

            tool_risk_inventory.insert(
                prefixed_name.clone(),
                crate::ipc::IpcToolRiskInfo::from_annotations(
                    upstream_annotations.as_ref(),
                    inferred_annotations.as_ref(),
                    prefixed_tool.annotations.as_ref(),
                ),
            );
            tools.push(prefixed_tool);
        }

        // Sort: priority tools first, then alphabetical
        tools.sort_unstable_by(|a, b| priority_sort(a, b, &self.config.priority_tools));

        // Add search_tools meta-tool if tool count exceeds threshold
        if tools.len() >= self.config.tool_search_threshold
            && !is_disabled_tool(&self.config.disabled_tools, "plug__search_tools")
        {
            let meta_tool = build_search_tools_meta_tool();
            routes.insert(
                meta_tool.name.to_string(),
                ("__plug_internal__".to_string(), meta_tool.name.to_string()),
            );
            tool_risk_inventory.insert(
                meta_tool.name.to_string(),
                crate::ipc::IpcToolRiskInfo::from_annotations(
                    None,
                    meta_tool.annotations.as_ref(),
                    meta_tool.annotations.as_ref(),
                ),
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

        // Build pre-cached filtered views — only when tool filtering is
        // enabled. `list_tools_for_client_session` (catalog.rs) is the only
        // reader of these two fields, and it always returns early via
        // `list_tools()` (which serves `tools_all`) when filtering is
        // disabled, so these views are provably never read in that case.
        let (tools_windsurf, tools_copilot) = if self.config.tool_filter_enabled {
            (
                Arc::new(tools.iter().take(100).cloned().collect()),
                Arc::new(tools.iter().take(128).cloned().collect()),
            )
        } else {
            (Arc::new(Vec::new()), Arc::new(Vec::new()))
        };
        let tools_all = Arc::new(tools);

        let mut resource_routes = HashMap::new();
        let mut resources_vec = Vec::new();
        for (server_name, mut resource) in upstream_resources {
            catalog_metadata_budget.admit(&server_name, &mut resource.meta, "resource");
            if let Some(existing_server) = resource_routes.get(&resource.uri)
                && existing_server != &server_name
            {
                tracing::warn!(
                    uri = %resource.uri,
                    first_server = %existing_server,
                    ignored_server = %server_name,
                    "resource URI collision detected; keeping first route"
                );
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
            resource.icons = normalized_icons_with_fallback(
                resource.icons.as_deref(),
                resolve_server_icons(&server_ctx, &server_name),
            );
            resource.name = routed_name;
            resources_vec.push(resource);
        }
        resources_vec.sort_by(|a, b| a.name.cmp(&b.name));
        let resources_all = Arc::new(resources_vec);

        let mut resource_templates_vec = Vec::new();
        for (server_name, mut template) in upstream_resource_templates {
            catalog_metadata_budget.admit(&server_name, &mut template.meta, "resource-template");
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
            template.icons = normalized_icons_with_fallback(
                template.icons.as_deref(),
                resolve_server_icons(&server_ctx, &server_name),
            );
            template.name = routed_name;
            resource_templates_vec.push(template);
        }
        resource_templates_vec.sort_by(|a, b| a.name.cmp(&b.name));
        let resource_templates_all = Arc::new(resource_templates_vec);

        let mut prompt_routes = HashMap::new();
        let mut prompts_vec = Vec::new();
        for (server_name, mut prompt) in upstream_prompts {
            catalog_metadata_budget.admit(&server_name, &mut prompt.meta, "prompt");
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
            prompt.icons = normalized_icons_with_fallback(
                prompt.icons.as_deref(),
                resolve_server_icons(&server_ctx, &server_name),
            );
            prompt.name = routed_name;
            prompts_vec.push(prompt);
        }
        prompts_vec.sort_by(|a, b| a.name.cmp(&b.name));
        let prompts_all = Arc::new(prompts_vec);

        let tool_count = tools_all.len();

        // Serialize the decide-and-mutate phase across concurrent refresh
        // passes: everything from here through the rebind loop (classify →
        // prune execution → snapshot publish → rebind execution) runs under
        // this guard, so a second pass cannot classify against a
        // pre-publish snapshot and then apply those stale decisions around
        // this pass's publish. The per-server listing above stays
        // concurrent. If the notification loop's refresh backstop drops
        // this future mid-phase, the guard is released on drop — tokio's
        // Mutex does not poison.
        let reconcile_guard = self.refresh_reconcile_lock.lock().await;

        // Classify every currently-tracked subscription URI against the
        // old/new route snapshots. Pure decision — no registry mutation, no
        // upstream calls yet. See `SubscriptionRegistry::classify_route_changes`
        // for the exact rebind/prune criteria (unchanged from the historical
        // inline `retain` closure this replaced).
        let reconciliation = {
            let old_snapshot = self.cache.load();
            self.resource_subscriptions
                .classify_route_changes(&old_snapshot.resource_routes, &resource_routes)
        };

        // Execute prunes for URIs that lost their route entirely
        // (best-effort upstream unsubscribe) before publishing the new
        // snapshot — same ordering as the historical stale-unsubscribe pass.
        // Distinct URIs use distinct transition locks, so parallelize under the
        // reconcile guard (wall-clock = max RTT, not sum).
        {
            let prune_futs: Vec<_> = reconciliation
                .iter()
                .filter_map(|item| {
                    let subscriptions::RouteReconciliation::Prune { uri, old_server_id } = item
                    else {
                        return None;
                    };
                    let registry = Arc::clone(&self.resource_subscriptions);
                    let upstream = old_server_id
                        .as_deref()
                        .and_then(|server_id| self.server_manager.get_upstream(server_id))
                        .map(subscriptions::as_upstream_ops);
                    let uri = uri.clone();
                    let old_server_id = old_server_id.clone();
                    Some(async move {
                        registry
                            .prune(&uri, old_server_id.as_deref(), upstream)
                            .await;
                    })
                })
                .collect();
            futures::future::join_all(prune_futs).await;
        }

        let snapshot = Arc::new(
            RouterSnapshot {
                routes,
                routes_lower: HashMap::new(),
                tools_by_name: HashMap::new(),
                tools_by_name_lower: HashMap::new(),
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
                tool_risk_inventory,
            }
            .with_indexes(),
        );
        // Only material call-routing changes invalidate parked rounds. Tool
        // metadata, resource, or prompt refreshes publish a fresh catalog
        // without breaking a continuation whose exact tool route and upstream
        // instance remain unchanged.
        self.publish_route_snapshot(snapshot);

        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(EngineEvent::ToolCacheRefreshed { tool_count });
        }

        // Rebind subscriptions whose URI still exists but ownership changed,
        // after publishing the new snapshot (same ordering as before).
        Self::rebind_reconciled_routes(
            &self.server_manager,
            &self.resource_subscriptions,
            reconciliation.iter(),
            "resource subscription rebind failed during route refresh",
        )
        .await;

        // Release the reconcile guard before the post-publish sweep: the
        // sweep classifies against the just-published snapshot only, and
        // must not extend the serialized window it exists to double-check.
        drop(reconcile_guard);

        // Post-publish sweep. Entries whose subscribe transition confirmed
        // inside this pass's classify→publish window were invisible to the
        // classification above, leaving their recorded owner pointing at a
        // server the published snapshot no longer routes the URI to — and
        // with refreshes being purely event-driven, nothing else guarantees
        // a healing pass ever runs. Re-run the classification against the
        // published snapshot in a DETACHED task (the notification loop's
        // refresh backstop drops this future on timeout and must not kill a
        // heal already in flight) and await it, so normal callers — and
        // tests — observe a fully-reconciled registry before returning.
        let sweep = tokio::spawn(Self::sweep_subscription_routes(
            Arc::clone(&self.cache),
            Arc::clone(&self.resource_subscriptions),
            Arc::clone(&self.server_manager),
        ));
        if sweep.await.is_err() {
            tracing::warn!("post-publish subscription sweep task failed to complete");
        }
    }

    /// Post-publish sweep body: classify every registry entry's recorded
    /// owner against the CURRENT published snapshot and execute the
    /// resulting rebinds through the same machinery `refresh_tools` uses.
    /// Route-vanished entries are intentionally left to the existing
    /// paths (recorded-owner drains and the next refresh's prune) so a
    /// subscriber racing a prune keeps its grace window. Takes the Arc'd
    /// fields it needs instead of `&self` so it can run detached from the
    /// (cancellable) refresh future.
    async fn sweep_subscription_routes(
        cache: Arc<ArcSwap<RouterSnapshot>>,
        registry: Arc<subscriptions::SubscriptionRegistry>,
        server_manager: Arc<ServerManager>,
    ) {
        let snapshot = cache.load_full();
        let reconciliation =
            registry.classify_route_changes(&snapshot.resource_routes, &snapshot.resource_routes);
        Self::rebind_reconciled_routes(
            &server_manager,
            &registry,
            reconciliation.iter(),
            "post-publish sweep failed to rebind resource subscription",
        )
        .await;
    }

    /// Execute the `Rebind` and `Resubscribe` decisions from a
    /// reconciliation pass. `Prune` is handled separately by the caller,
    /// before the new snapshot is published.
    async fn rebind_reconciled_routes<'a, I>(
        server_manager: &Arc<ServerManager>,
        registry: &Arc<subscriptions::SubscriptionRegistry>,
        items: I,
        failure_message: &'static str,
    ) where
        I: IntoIterator<Item = &'a subscriptions::RouteReconciliation>,
    {
        let rebind_futs: Vec<_> = items
            .into_iter()
            .filter_map(|item| {
                let (uri, old_server_id, new_server_id) = match item {
                    subscriptions::RouteReconciliation::Rebind {
                        uri,
                        old_server_id,
                        new_server_id,
                    } => (uri, Some(old_server_id), new_server_id),
                    // Same server, new connection: no old owner to release.
                    subscriptions::RouteReconciliation::Resubscribe { uri, server_id } => {
                        (uri, None, server_id)
                    }
                    subscriptions::RouteReconciliation::Prune { .. } => return None,
                };
                let server_manager = Arc::clone(server_manager);
                let registry = Arc::clone(registry);
                let uri = uri.clone();
                let old_server_id = old_server_id.cloned();
                let new_server_id = new_server_id.clone();
                Some(async move {
                    let new_owner = Self::resolve_rebind_owner(&server_manager, &new_server_id);
                    let outcome = match old_server_id {
                        Some(old_server_id) => {
                            let old_upstream = server_manager
                                .get_upstream(&old_server_id)
                                .map(subscriptions::as_upstream_ops);
                            registry
                                .rebind(
                                    &uri,
                                    &old_server_id,
                                    old_upstream,
                                    &new_server_id,
                                    new_owner,
                                )
                                .await
                        }
                        None => registry.resubscribe(&uri, &new_server_id, new_owner).await,
                    };
                    if let Err(error) = outcome {
                        tracing::warn!(uri = %uri, error = %error, "{failure_message}");
                    }
                })
            })
            .collect();
        futures::future::join_all(rebind_futs).await;
    }

    /// Resolve the upstream handles/capabilities for a subscription rebind
    /// and execute it, awaiting completion and returning the transition's
    /// outcome. Shared by `refresh_tools`' rebind arm and the
    /// post-subscribe owner/route self-check in `subscribe_resource`
    /// (subscriptions.rs): both resolve the old owner as a route-derived
    /// fallback (the registry prefers the entry's recorded owner at drain
    /// time) and gate the new owner on subscribe support.
    async fn rebind_subscription_route(
        &self,
        uri: &str,
        old_server_id: &str,
        new_server_id: &str,
    ) -> Result<(), McpError> {
        Self::rebind_subscription_route_with(
            &self.server_manager,
            &self.resource_subscriptions,
            uri,
            old_server_id,
            new_server_id,
        )
        .await
    }

    /// `rebind_subscription_route` without `&self`, for callers that only
    /// hold the Arc'd fields: the detached post-publish sweep and the
    /// registry's post-confirm heal hook (both wired in `new()`).
    async fn rebind_subscription_route_with(
        server_manager: &Arc<ServerManager>,
        registry: &Arc<subscriptions::SubscriptionRegistry>,
        uri: &str,
        old_server_id: &str,
        new_server_id: &str,
    ) -> Result<(), McpError> {
        let old_upstream = server_manager
            .get_upstream(old_server_id)
            .map(subscriptions::as_upstream_ops);
        let new_owner = Self::resolve_rebind_owner(server_manager, new_server_id);
        registry
            .rebind(uri, old_server_id, old_upstream, new_server_id, new_owner)
            .await
    }

    /// Resolve a destination server into the handle a rebind or resubscribe
    /// should subscribe on, or the reason it isn't usable.
    fn resolve_rebind_owner(
        server_manager: &Arc<ServerManager>,
        server_id: &str,
    ) -> Result<Arc<dyn subscriptions::UpstreamResourceOps>, subscriptions::RebindSkipReason> {
        match server_manager.get_upstream(server_id) {
            None => Err(subscriptions::RebindSkipReason::NewOwnerMissing),
            Some(upstream) => {
                let supports_subscribe = upstream
                    .capabilities
                    .resources
                    .as_ref()
                    .and_then(|r| r.subscribe)
                    .unwrap_or(false);
                if supports_subscribe {
                    Ok(subscriptions::as_upstream_ops(upstream))
                } else {
                    Err(subscriptions::RebindSkipReason::NewOwnerNoSubscribeSupport)
                }
            }
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
        match self
            .call_tool_inner(
                tool_name,
                ToolRoundInput {
                    arguments,
                    input_responses: None,
                    request_state: None,
                    expected_route: None,
                },
                progress_token,
                downstream,
                true,
                false,
            )
            .await?
        {
            CallToolResponse::Complete(result) => Ok(result),
            CallToolResponse::InputRequired(_) => {
                Err(crate::protocol::ProtocolOutcome::UnsupportedBridge
                    .into_error(crate::protocol::ProtocolEra::Legacy))
            }
            CallToolResponse::Task(_) => Err(McpError::internal_error(
                "unexpected task response from synchronous tool call".to_string(),
                None,
            )),
            _ => Err(McpError::internal_error(
                "unsupported response from synchronous tool call".to_string(),
                None,
            )),
        }
    }

    /// Execute one MCP 2026 tools/call round without automatically fulfilling
    /// input requests. Continuation fields are forwarded byte-for-byte to the
    /// same routed upstream; callers must enforce era and ownership policy.
    pub async fn call_tool_round_with_context(
        &self,
        mut params: CallToolRequestParams,
        progress_token: Option<ProgressToken>,
        downstream: Option<DownstreamCallContext>,
    ) -> Result<CallToolResponse, McpError> {
        let context = downstream.clone().ok_or_else(|| {
            McpError::internal_error("modern tool round requires downstream context", None)
        })?;
        if canonical_plug_meta_tool_name(params.name.as_ref()).is_some() {
            if params.input_responses.is_some() || params.request_state.is_some() {
                return Err(crate::protocol::ProtocolOutcome::UnsupportedBridge
                    .into_error(context.protocol_era));
            }
            return self
                .call_tool_inner(
                    params.name.as_ref(),
                    ToolRoundInput {
                        arguments: params.arguments,
                        input_responses: None,
                        request_state: None,
                        expected_route: None,
                    },
                    progress_token,
                    downstream,
                    true,
                    false,
                )
                .await;
        }
        let (route, global_generation) = self.tool_route_identity(params.name.as_ref())?;
        let request_digest = continuations::canonical_tool_request_digest(&params);
        let binding = context.principal.as_ref().map(|principal| {
            continuations::ContinuationBinding::new(
                principal,
                request_digest,
                &format!("{}\0{}", route.server_id, route.original_name),
                route.route_generation,
                continuations::ContinuationKind::NativeToolRound,
            )
        });
        let mut round = 1;
        let mut chain_expires_at = std::time::Instant::now() + MRTR_CHAIN_TTL;
        let mut reservation = None;

        if params.input_responses.is_some() || params.request_state.is_some() {
            context.authorize_continuation()?;
            context.ensure_principal_lifecycle_active()?;
            let binding = binding.as_ref().ok_or_else(|| {
                crate::protocol::ProtocolOutcome::AuthorizationRequired
                    .into_error(context.protocol_era)
            })?;
            let token = params.request_state.as_deref().ok_or_else(|| {
                crate::protocol::ProtocolOutcome::ExpiredContinuation
                    .into_error(context.protocol_era)
            })?;
            let consumed = self
                .continuation_registry
                .consume(token, binding)
                .map_err(|error| error.protocol_outcome().into_error(context.protocol_era))?;
            let parked = consumed.value;
            reservation = Some(consumed.reservation);
            if parked.route != route {
                return Err(crate::protocol::ProtocolOutcome::ExpiredContinuation
                    .into_error(context.protocol_era));
            }
            if parked.round >= MRTR_MAX_ROUNDS {
                return Err(crate::protocol::ProtocolOutcome::QuotaExceeded
                    .into_error(context.protocol_era));
            }
            if parked.chain_expires_at <= std::time::Instant::now() {
                return Err(crate::protocol::ProtocolOutcome::ExpiredContinuation
                    .into_error(context.protocol_era));
            }
            round = parked.round + 1;
            chain_expires_at = parked.chain_expires_at;
            let current_upstream = self
                .server_manager
                .get_upstream(&route.server_id)
                .ok_or_else(|| {
                    crate::protocol::ProtocolOutcome::ExpiredContinuation
                        .into_error(context.protocol_era)
                })?;
            if !Arc::ptr_eq(&current_upstream, &parked.upstream) {
                return Err(crate::protocol::ProtocolOutcome::ExpiredContinuation
                    .into_error(context.protocol_era));
            }
            let responses = params.input_responses.as_ref().ok_or_else(|| {
                crate::protocol::ProtocolOutcome::ExpiredContinuation
                    .into_error(context.protocol_era)
            })?;
            continuations::validate_input_responses(&parked.input_requests, responses)
                .map_err(|error| error.protocol_outcome().into_error(context.protocol_era))?;
            params.request_state = parked.upstream_request_state;
        } else if self
            .server_manager
            .get_upstream(&route.server_id)
            .is_some_and(|upstream| upstream.protocol_era == crate::protocol::ProtocolEra::Modern)
        {
            context.authorize_continuation()?;
            context.ensure_principal_lifecycle_active()?;
            let principal = context.principal.as_ref().ok_or_else(|| {
                crate::protocol::ProtocolOutcome::AuthorizationRequired
                    .into_error(context.protocol_era)
            })?;
            reservation = Some(
                self.continuation_registry
                    .reserve_at(principal, MRTR_MAX_BYTES, global_generation)
                    .map_err(|error| error.protocol_outcome().into_error(context.protocol_era))?,
            );
        }

        let response = self
            .call_tool_inner(
                params.name.as_ref(),
                ToolRoundInput {
                    arguments: params.arguments,
                    input_responses: params.input_responses,
                    request_state: params.request_state,
                    expected_route: Some(route.clone()),
                },
                progress_token,
                downstream,
                true,
                false,
            )
            .await?;

        let CallToolResponse::InputRequired(mut input_required) = response else {
            return Ok(response);
        };
        context.ensure_principal_lifecycle_active()?;
        let (current_route, _) = self.tool_route_identity(params.name.as_ref())?;
        if current_route != route {
            return Err(crate::protocol::ProtocolOutcome::ExpiredContinuation
                .into_error(context.protocol_era));
        }
        let context = context;
        let binding = binding.ok_or_else(|| {
            crate::protocol::ProtocolOutcome::AuthorizationRequired.into_error(context.protocol_era)
        })?;
        let input_requests = input_required.input_requests.clone().unwrap_or_default();
        if input_requests.is_empty()
            || input_requests.len() > MRTR_MAX_ITEMS
            || input_required
                .request_state
                .as_ref()
                .is_some_and(|state| state.len() > MRTR_MAX_REQUEST_STATE_BYTES)
            || input_requests.values().any(|request| {
                !matches!(
                    request,
                    InputRequest::Elicitation(_) | InputRequest::CreateMessage(_)
                )
            })
        {
            return Err(crate::protocol::ProtocolOutcome::UnsupportedBridge
                .into_error(context.protocol_era));
        }
        let payload_bytes = serde_json::to_vec(&input_required)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?
            .len();
        if payload_bytes > MRTR_MAX_BYTES {
            return Err(McpError::invalid_request(
                "upstream multi-round response exceeds Plug's bounded continuation limits",
                None,
            ));
        }
        let reservation = reservation.ok_or_else(|| {
            crate::protocol::ProtocolOutcome::UnsupportedBridge.into_error(context.protocol_era)
        })?;
        let upstream = self
            .server_manager
            .get_upstream(&route.server_id)
            .filter(|upstream| upstream.generation() == route.upstream_generation)
            .ok_or_else(|| {
                crate::protocol::ProtocolOutcome::ExpiredContinuation
                    .into_error(context.protocol_era)
            })?;
        #[cfg(test)]
        if let Some(gate) = self
            .continuation_insert_gate
            .lock()
            .expect("continuation insert gate mutex poisoned")
            .clone()
        {
            gate.entered.wait();
            gate.release.wait();
        }
        let token = self
            .continuation_registry
            .insert_reserved(
                binding,
                NativeContinuation {
                    upstream_request_state: input_required.request_state.take(),
                    input_requests,
                    route,
                    upstream,
                    round,
                    chain_expires_at,
                },
                reservation,
            )
            .map_err(|error| error.protocol_outcome().into_error(context.protocol_era))?;
        input_required.request_state = Some(token);
        Ok(CallToolResponse::InputRequired(input_required))
    }

    /// Inner tool call implementation with retry support.
    /// `is_retry` prevents infinite recursion — max 1 reconnect per call.
    /// Uses `Box::pin` for the recursive call to avoid infinitely-sized future.
    fn call_tool_inner<'a>(
        &'a self,
        tool_name: &'a str,
        round: ToolRoundInput,
        progress_token: Option<ProgressToken>,
        downstream: Option<DownstreamCallContext>,
        enforce_lazy_visibility: bool,
        is_retry: bool,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CallToolResponse, McpError>> + Send + 'a>,
    > {
        Box::pin(async move {
            // Intercept plug meta-tools (case-insensitive for LLM casing drift).
            if let Some(meta_tool_name) = canonical_plug_meta_tool_name(tool_name) {
                if !self.meta_tool_visible_for_call(meta_tool_name, downstream.as_ref()) {
                    return Err(McpError::from(ProtocolError::ToolNotFound {
                        tool_name: tool_name.to_string(),
                    }));
                }
                match meta_tool_name {
                    "plug__list_servers" => return Ok(self.handle_list_servers().into()),
                    "plug__list_tools" => {
                        return self
                            .handle_list_tools(round.arguments.clone())
                            .map(Into::into);
                    }
                    "plug__search_tools" => {
                        return self
                            .handle_search_tools(round.arguments.clone(), downstream.as_ref())
                            .map(Into::into);
                    }
                    "plug__invoke_tool" => {
                        return self
                            .handle_invoke_tool(
                                round.arguments.clone(),
                                progress_token,
                                downstream,
                                is_retry,
                            )
                            .await
                            .map(Into::into);
                    }
                    _ => unreachable!("canonical plug meta-tool name is exhaustive"),
                }
            }

            let cache = self.cache.load_full();
            let (server_id, original_name) = cache.resolve_route(tool_name).ok_or_else(|| {
                McpError::from(ProtocolError::ToolNotFound {
                    tool_name: tool_name.to_string(),
                })
            })?;

            let server_id = server_id.clone();
            let original_name = original_name.to_string();
            drop(cache);

            if enforce_lazy_visibility {
                self.ensure_lazy_tool_loaded_for_direct_call(downstream.as_ref(), tool_name)?;
            }

            // Health gate — auth failure is actionable and must not be
            // flattened into a generic availability failure. The existing
            // outcome encoding tells clients not to retry until they have
            // re-authorized, without returning any credential material.
            let health = self.server_manager.health.get(&server_id).map(|h| h.health);
            if let Some(outcome) = health.and_then(health_gate_outcome) {
                let era = downstream
                    .as_ref()
                    .map(|context| context.protocol_era)
                    .unwrap_or(crate::protocol::ProtocolEra::Legacy);
                return Err(outcome.into_error(era));
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

            let call_id = next_call_id();
            let trace_id = downstream
                .as_ref()
                .map(|context| Arc::clone(&context.trace_id))
                .unwrap_or_else(|| Arc::from(new_trace_id()));
            let extension_meta = downstream
                .as_ref()
                .and_then(|context| context.extension_envelope.as_ref())
                .and_then(ExtensionEnvelope::to_meta);

            let mut upstream_params = CallToolRequestParams::new(original_name.clone());
            if let Some(ref args) = round.arguments {
                upstream_params = upstream_params.with_arguments(args.clone());
            }
            upstream_params.input_responses = round.input_responses.clone();
            upstream_params.request_state = round.request_state.clone();
            let downstream_progress_token = progress_token.clone();
            let upstream_progress_token = downstream_progress_token.as_ref().map(|_| {
                ProgressToken(NumberOrString::String(Arc::from(format!(
                    "plug-progress-{call_id}"
                ))))
            });
            if let Some(token) = upstream_progress_token.clone() {
                upstream_params.set_progress_token(token);
            }
            if let Some(meta) = extension_meta.clone() {
                upstream_params.meta = Some(RequestMetaObject(meta));
            }

            let request = ClientRequest::CallToolRequest(CallToolRequest::new(upstream_params));

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
                        downstream_progress_token,
                        upstream_progress_token: upstream_progress_token.clone(),
                        pending_cancel_reason: None,
                    },
                )?;
                active_call_guard = Some(ActiveCallGuard {
                    router: self,
                    call_id,
                    armed: true,
                });
            }

            // Reserve the downstream key before waiting for upstream
            // concurrency. A duplicate modern request must be rejected while
            // the first is queued or executing, rather than waiting for the
            // semaphore and becoming a second side effect after the first
            // call unregisters.
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

            // Resolve the live peer only after the queue wait. A reconnect may
            // replace the upstream while this call is waiting for a permit;
            // retaining the pre-wait peer would dispatch onto the retired
            // session. Continuations also need their route checked against the
            // connection that will actually receive the resumed call.
            let upstream = self
                .server_manager
                .get_upstream(&server_id)
                .ok_or_else(|| {
                    McpError::from(ProtocolError::ServerUnavailable {
                        server_id: server_id.clone(),
                    })
                })?;

            if let Some(expected) = &round.expected_route {
                let (actual, _) = self.tool_route_identity(tool_name)?;
                if actual != *expected {
                    return Err(
                        crate::protocol::ProtocolOutcome::ExpiredContinuation.into_error(
                            downstream
                                .as_ref()
                                .map(|context| context.protocol_era)
                                .unwrap_or(crate::protocol::ProtocolEra::Legacy),
                        ),
                    );
                }
            }

            let timeout_duration = Duration::from_secs(upstream.config.call_timeout_secs);
            let transport_type = upstream.config.transport.clone();
            let peer = upstream.client.peer().clone();
            drop(upstream);

            let mut options = PeerRequestOptions::default();
            options.timeout = Some(timeout_duration);
            options.meta = extension_meta.clone().map(RequestMetaObject);
            if let Some(token) = upstream_progress_token.clone() {
                options
                    .meta
                    .get_or_insert_with(RequestMetaObject::new)
                    .set_progress_token(token);
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
            tracing::info!(
                call_id,
                trace_id = %trace_id,
                downstream_transport = ?downstream.as_ref().map(|context| context.transport),
                downstream_client = ?downstream.as_ref().map(|context| context.client_id.as_ref()),
                downstream_request_id = ?downstream.as_ref().map(|context| &context.request_id),
                upstream_request_id = ?request_handle.id,
                server = %server_id,
                tool = %original_name,
                retry = is_retry,
                "proxy tool call started"
            );
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(EngineEvent::ToolCallStarted {
                    call_id,
                    trace_id: Arc::clone(&trace_id),
                    server_id: Arc::clone(&server_id_arc),
                    tool_name: Arc::clone(&tool_name_arc),
                });
            }

            let call_start = std::time::Instant::now();

            // Execute with timeout via rmcp RequestHandle so we retain upstream request ID.
            let result = request_handle.await_response().await;

            // Drop semaphore permit
            drop(permit);

            // Metrics RAII: any early exit / panic after await still records a
            // failure unless a terminal arm settles the guard first.
            struct MetricsCallGuard<'a> {
                server_manager: &'a ServerManager,
                server_id: String,
                start: std::time::Instant,
                settled: bool,
            }
            impl MetricsCallGuard<'_> {
                fn settle(&mut self, ok: bool) {
                    if self.settled {
                        return;
                    }
                    let duration_ms = self.start.elapsed().as_millis() as u64;
                    self.server_manager
                        .record_call(&self.server_id, ok, duration_ms);
                    self.settled = true;
                }
            }
            impl Drop for MetricsCallGuard<'_> {
                fn drop(&mut self) {
                    self.settle(false);
                }
            }
            let mut metrics_guard = MetricsCallGuard {
                server_manager: &self.server_manager,
                server_id: server_id.clone(),
                start: call_start,
                settled: false,
            };
            let duration_ms = call_start.elapsed().as_millis() as u64;

            // Record circuit breaker outcome
            let cb = self.server_manager.circuit_breakers.get(&server_id);

            match result {
                Ok(ServerResult::CallToolResult(mut response)) => {
                    // Sanitize before artifact serialization, cache/buffer
                    // decisions, IPC conversion, or downstream encoding.
                    admit_tool_result_meta(&mut response);
                    if let Some(cb) = &cb {
                        cb.on_success();
                    }
                    metrics_guard.settle(true);
                    if let Some(ref mut guard) = active_call_guard {
                        guard.disarm();
                    }
                    self.remove_active_call(call_id);
                    if let Some(ref tx) = self.event_tx {
                        let _ = tx.send(EngineEvent::ToolCallCompleted {
                            call_id,
                            trace_id: Arc::clone(&trace_id),
                            server_id: Arc::clone(&server_id_arc),
                            tool_name: Arc::clone(&tool_name_arc),
                            duration_ms,
                            success: true,
                        });
                    }
                    tracing::info!(
                        call_id,
                        trace_id = %trace_id,
                        server = %server_id,
                        tool = %original_name,
                        duration_ms,
                        "proxy tool call completed"
                    );
                    self.artifact_store
                        .maybe_spill_tool_result(tool_name, response)
                        .await
                        .map(Into::into)
                }
                Ok(ServerResult::InputRequiredResult(response)) => {
                    // A modern upstream completed this round and supplied its
                    // own continuation contract. Do not retain or replay the
                    // old request handle; the downstream retry is a new round.
                    if let Some(cb) = &cb {
                        cb.on_success();
                    }
                    metrics_guard.settle(true);
                    if let Some(ref mut guard) = active_call_guard {
                        guard.disarm();
                    }
                    self.remove_active_call(call_id);
                    if let Some(ref tx) = self.event_tx {
                        let _ = tx.send(EngineEvent::ToolCallCompleted {
                            call_id,
                            trace_id: Arc::clone(&trace_id),
                            server_id: Arc::clone(&server_id_arc),
                            tool_name: Arc::clone(&tool_name_arc),
                            duration_ms,
                            success: true,
                        });
                    }
                    tracing::info!(
                        call_id,
                        trace_id = %trace_id,
                        server = %server_id,
                        tool = %original_name,
                        duration_ms,
                        "proxy tool call round requires input"
                    );
                    Ok(CallToolResponse::InputRequired(response))
                }
                Err(e)
                    if is_session_error(&e)
                        && !is_retry
                        && round.input_responses.is_none()
                        && round.request_state.is_none()
                        && round.expected_route.is_none() =>
                {
                    // Session/transport error on first attempt — try to reconnect and retry
                    tracing::warn!(
                        server = %server_id,
                        tool = %original_name,
                        call_id,
                        trace_id = %trace_id,
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
                            trace_id: Arc::clone(&trace_id),
                            server_id: Arc::clone(&server_id_arc),
                            tool_name: Arc::clone(&tool_name_arc),
                            duration_ms,
                            success: false,
                        });
                    }

                    match self.reconnect_server_now(&server_id).await {
                        Ok(()) => {
                            tracing::info!(
                                server = %server_id,
                                call_id,
                                trace_id = %trace_id,
                                "reconnected, retrying tool call"
                            );
                            // Count the transient failure that triggered the
                            // reconnect so the degradation blip is visible; the
                            // retry below records its own (terminal) outcome.
                            metrics_guard.settle(false);
                        }
                        Err(reconnect_err) => {
                            tracing::error!(
                                server = %server_id,
                                call_id,
                                trace_id = %trace_id,
                                error = %reconnect_err,
                                "reconnect failed, returning original error"
                            );
                            if let Some(cb) = &cb {
                                cb.on_failure();
                            }
                            metrics_guard.settle(false);
                            return Err(McpError::internal_error(e.to_string(), None));
                        }
                    }

                    self.call_tool_inner(
                        tool_name,
                        round,
                        progress_token,
                        downstream,
                        enforce_lazy_visibility,
                        true,
                    )
                    .await
                }
                Err(rmcp::service::ServiceError::Timeout { timeout }) => {
                    tracing::error!(
                        server = %server_id,
                        tool = %original_name,
                        call_id,
                        trace_id = %trace_id,
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
                            trace_id: Arc::clone(&trace_id),
                            server_id: Arc::clone(&server_id_arc),
                            tool_name: Arc::clone(&tool_name_arc),
                            duration_ms,
                            success: false,
                        });
                    }

                    metrics_guard.settle(false);

                    if matches!(transport_type, crate::config::TransportType::Stdio) {
                        self.reconnect_server_in_background(server_id.clone());
                    }

                    Err(McpError::from(ProtocolError::Timeout { duration: timeout }))
                }
                Err(e) => {
                    tracing::error!(
                        server = %server_id,
                        tool = %original_name,
                        call_id,
                        trace_id = %trace_id,
                        error = %e,
                        "upstream tool call failed"
                    );
                    if let Some(cb) = &cb {
                        cb.on_failure();
                    }
                    metrics_guard.settle(false);
                    if let Some(ref mut guard) = active_call_guard {
                        guard.disarm();
                    }
                    self.remove_active_call(call_id);
                    if let Some(ref tx) = self.event_tx {
                        let _ = tx.send(EngineEvent::ToolCallCompleted {
                            call_id,
                            trace_id: Arc::clone(&trace_id),
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
                    // An unexpected upstream response is a terminal failure —
                    // record it like the other terminal branches.
                    metrics_guard.settle(false);
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

    fn meta_tool_visible_for_call(
        &self,
        meta_tool_name: &str,
        downstream: Option<&DownstreamCallContext>,
    ) -> bool {
        if is_disabled_tool(&self.config.disabled_tools, meta_tool_name) {
            return false;
        }

        let surface = downstream
            .map(|context| self.config.lazy_surface_for_client(context.client_type))
            .unwrap_or_else(|| self.config.lazy_surface_for_client(ClientType::Unknown));
        match surface {
            LazyToolSurface::Bridge => meta_tool_name == "plug__search_tools",
            LazyToolSurface::LegacyMeta => legacy_meta_tool_names().contains(&meta_tool_name),
            LazyToolSurface::Standard | LazyToolSurface::Native => {
                self.cache.load().resolve_route(meta_tool_name).is_some()
            }
        }
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
            return CallToolResult::success(vec![ContentBlock::text(
                "No upstream servers configured.",
            )]);
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

        CallToolResult::success(vec![ContentBlock::text(lines.join("\n"))])
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
            if let Some(filter) = server_filter.as_ref()
                && server_id.to_lowercase() != *filter
            {
                continue;
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
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "No tools matched the requested filters.",
            )]));
        }

        let mut lines = vec![format!("Tools ({})", matches.len())];
        for (server_id, tool) in matches {
            lines.push(format!("- {} (server: {})", tool.name, server_id));
            if let Some(desc) = tool.description.as_deref() {
                lines.push(format!("  {desc}"));
            }
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(
            lines.join("\n"),
        )]))
    }

    /// Handle the `plug__search_tools` meta-tool call.
    fn handle_search_tools(
        &self,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        downstream: Option<&DownstreamCallContext>,
    ) -> Result<CallToolResult, McpError> {
        let query = arguments
            .as_ref()
            .and_then(|args| args.get("query"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if query.is_empty() {
            return self.json_tool_result(serde_json::json!({
                "matches": [],
                "error": "query is required"
            }));
        }
        let tokens = tokenize_search_query(&query);
        if tokens.is_empty() {
            return self.json_tool_result(serde_json::json!({
                "matches": [],
                "error": "query must include searchable text"
            }));
        }
        let query_phrase = tokens.join(" ");

        let limit = arguments
            .as_ref()
            .and_then(|args| args.get("limit"))
            .and_then(|value| value.as_u64())
            .map(|value| value.min(BRIDGE_SEARCH_RESULT_MAX as u64) as usize)
            .unwrap_or(5);
        let snapshot = self.cache.load();
        let mut ranked = Vec::new();
        for tool in snapshot.tools_all.iter() {
            let Some((server_id, _)) = snapshot.routes.get(tool.name.as_ref()) else {
                continue;
            };
            if server_id == "__plug_internal__" {
                continue;
            }
            let Some(score) = score_tool_match(tool, server_id, &query_phrase, &tokens) else {
                continue;
            };
            ranked.push((score, server_id.as_str(), tool.name.as_ref()));
        }
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(b.2)));
        ranked.truncate(limit);

        let mut matches = Vec::new();
        let mut tools = Vec::new();
        let mut available_tools = Vec::new();
        for (score, server_id, tool_name) in ranked {
            let Some(tool) = snapshot.tool_by_name(tool_name) else {
                continue;
            };
            let canonical_name = tool.name.to_string();
            matches.push(serde_json::json!({
                "name": canonical_name.clone(),
                "server_id": server_id,
                "description": tool.description.as_deref().unwrap_or(""),
                "score": score,
            }));
            available_tools.push(canonical_name);
            tools.push(tool.clone());
        }
        drop(snapshot);

        let mut newly_loaded_tools = Vec::new();
        let mut evicted_tools = Vec::new();
        let mut visible_set_changed = false;
        if let Some(downstream) = downstream
            && matches!(
                self.config.lazy_surface_for_client(downstream.client_type),
                LazyToolSurface::Bridge
            )
            && !available_tools.is_empty()
        {
            let session_key = self.lazy_session_key_from_context(Some(downstream))?;
            let mut loaded = self.lazy_working_sets.entry(session_key).or_default();
            let before = loaded.iter().cloned().collect::<Vec<_>>();
            for tool_name in &available_tools {
                if let Some(position) = loaded.iter().position(|name| name == tool_name) {
                    loaded.remove(position);
                } else {
                    newly_loaded_tools.push(tool_name.clone());
                }
                loaded.push_back(tool_name.clone());
            }
            while loaded.len() > BRIDGE_WORKING_SET_MAX_TOOLS {
                if let Some(evicted) = loaded.pop_front() {
                    evicted_tools.push(evicted);
                }
            }
            visible_set_changed = before != loaded.iter().cloned().collect::<Vec<_>>();
            drop(loaded);
            if visible_set_changed {
                self.publish_protocol_notification(ProtocolNotification::ToolListChangedFor {
                    target: downstream.notification_target(),
                });
            }
        }

        self.json_tool_result(serde_json::json!({
            "matches": matches,
            "tools": tools,
            "available_tools": available_tools,
            "newly_loaded_tools": newly_loaded_tools,
            "evicted_tools": evicted_tools,
            "working_set_limit": BRIDGE_WORKING_SET_MAX_TOOLS,
            "visible_set_changed": visible_set_changed,
            "next_step": "Call the best matching tool directly by name.",
        }))
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
            if canonical_plug_meta_tool_name(target).is_some() {
                return Err(McpError::from(ProtocolError::InvalidRequest {
                    detail: "plug__invoke_tool cannot invoke plug meta-tools".to_string(),
                }));
            }

            let forwarded_arguments = args
                .get("arguments")
                .and_then(|value| value.as_object())
                .cloned();

            if let Some(context) = downstream.as_ref() {
                // The meta-tool itself is only indirection. Apply pair policy
                // to the resolved target before recursively entering routing,
                // otherwise a legacy caller could reach a modern upstream via
                // `plug__invoke_tool` even though a direct call is suppressed.
                self.ensure_tool_round_supported(target, context, false, false)?;
                let (route, _) = self.tool_route_identity(target)?;
                if self
                    .server_manager
                    .get_upstream(&route.server_id)
                    .is_some_and(|upstream| {
                        upstream.protocol_era == crate::protocol::ProtocolEra::Modern
                    })
                {
                    // `plug__invoke_tool` returns a legacy-shaped complete
                    // result and has nowhere to carry requestState/inputRequests.
                    // Until it is routed through the complete native MRTR
                    // coordinator, suppress every modern target before effect.
                    return Err(crate::protocol::ProtocolOutcome::UnsupportedBridge
                        .into_error(context.protocol_era));
                }
            }

            self.call_tool_inner(
                target,
                ToolRoundInput {
                    arguments: forwarded_arguments,
                    input_responses: None,
                    request_state: None,
                    expected_route: None,
                },
                progress_token,
                downstream,
                false,
                is_retry,
            )
            .await
            .and_then(|response| match response {
                CallToolResponse::Complete(result) => Ok(result),
                CallToolResponse::InputRequired(_) => {
                    Err(crate::protocol::ProtocolOutcome::UnsupportedBridge
                        .into_error(crate::protocol::ProtocolEra::Legacy))
                }
                CallToolResponse::Task(_) => Err(McpError::internal_error(
                    "unexpected task response from plug__invoke_tool".to_string(),
                    None,
                )),
                _ => Err(McpError::internal_error(
                    "unsupported response from plug__invoke_tool".to_string(),
                    None,
                )),
            })
        })
    }

    fn lazy_session_key_from_context(
        &self,
        downstream: Option<&DownstreamCallContext>,
    ) -> Result<String, McpError> {
        let Some(downstream) = downstream else {
            return Err(McpError::from(ProtocolError::InvalidRequest {
                detail: "lazy tool working-set actions require a downstream session".to_string(),
            }));
        };
        Ok(Self::lazy_session_key(
            downstream.transport,
            downstream.client_id.as_ref(),
        ))
    }

    fn json_tool_result(&self, value: serde_json::Value) -> Result<CallToolResult, McpError> {
        let text = serde_json::to_string_pretty(&value)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    fn ensure_lazy_tool_loaded_for_direct_call(
        &self,
        downstream: Option<&DownstreamCallContext>,
        tool_name: &str,
    ) -> Result<(), McpError> {
        let Some(downstream) = downstream else {
            return Ok(());
        };
        if !matches!(
            self.config.lazy_surface_for_client(downstream.client_type),
            LazyToolSurface::Bridge
        ) {
            return Ok(());
        }
        let session_key =
            Self::lazy_session_key(downstream.transport, downstream.client_id.as_ref());
        let Some(mut loaded) = self.lazy_working_sets.get_mut(&session_key) else {
            return Err(McpError::from(ProtocolError::InvalidRequest {
                detail: format!(
                    "{tool_name} is hidden in this lazy tool session; call plug__search_tools first"
                ),
            }));
        };
        if let Some(position) = loaded
            .iter()
            .position(|loaded_name| loaded_name.eq_ignore_ascii_case(tool_name))
        {
            if let Some(loaded_name) = loaded.remove(position) {
                loaded.push_back(loaded_name);
            }
            return Ok(());
        }
        Err(McpError::from(ProtocolError::InvalidRequest {
            detail: format!(
                "{tool_name} is hidden in this lazy tool session; call plug__search_tools first"
            ),
        }))
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

fn health_gate_outcome(health: ServerHealth) -> Option<crate::protocol::ProtocolOutcome> {
    match health {
        ServerHealth::AuthRequired => Some(crate::protocol::ProtocolOutcome::AuthorizationRequired),
        ServerHealth::Failed => Some(crate::protocol::ProtocolOutcome::UpstreamUnavailable),
        ServerHealth::Healthy | ServerHealth::Degraded => None,
    }
}

#[cfg(test)]
mod lifecycle_regression_tests {
    use super::*;

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

    #[test]
    fn auth_required_health_has_non_retryable_authorization_outcome() {
        let outcome = health_gate_outcome(ServerHealth::AuthRequired)
            .expect("auth-required health must reject the call");
        assert_eq!(
            outcome,
            crate::protocol::ProtocolOutcome::AuthorizationRequired
        );
        let encoded = outcome.encode(crate::protocol::ProtocolEra::Modern);
        assert!(
            !encoded.retryable,
            "client must re-authorize before retrying"
        );

        assert_eq!(
            health_gate_outcome(ServerHealth::Failed),
            Some(crate::protocol::ProtocolOutcome::UpstreamUnavailable)
        );
    }

    #[test]
    fn concurrent_duplicate_downstream_key_never_evicts_the_winner() {
        let router = Arc::new(ToolRouter::new(
            Arc::new(ServerManager::new()),
            test_router_config(),
        ));
        let mut notifications = router.subscribe_notifications();
        let context = DownstreamCallContext::stdio(
            "same-principal",
            RequestId::from(NumberOrString::Number(77)),
        )
        .with_protocol(
            crate::protocol::ProtocolEra::Modern,
            crate::protocol::SUPPORTED_PROTOCOL_VERSION,
        );
        let start = Arc::new(std::sync::Barrier::new(3));
        let (send, receive) = std::sync::mpsc::channel();

        std::thread::scope(|scope| {
            for call_id in [41_u64, 42_u64] {
                let router = Arc::clone(&router);
                let context = context.clone();
                let start = Arc::clone(&start);
                let send = send.clone();
                scope.spawn(move || {
                    start.wait();
                    let result = router.register_active_call(
                        call_id,
                        ActiveCallRecord {
                            downstream: context,
                            upstream_server_id: "upstream".to_string(),
                            upstream_request_id: Some(RequestId::from(NumberOrString::Number(
                                call_id as i64,
                            ))),
                            downstream_progress_token: None,
                            upstream_progress_token: None,
                            pending_cancel_reason: None,
                        },
                    );
                    send.send((call_id, result))
                        .expect("send registration result");
                });
            }
            start.wait();
        });

        let results = [receive.recv().unwrap(), receive.recv().unwrap()];
        let winner = results
            .iter()
            .find_map(|(call_id, result)| result.is_ok().then_some(*call_id))
            .expect("exactly one registration wins");
        let loser = results
            .iter()
            .find_map(|(call_id, result)| result.is_err().then_some(*call_id))
            .expect("duplicate registration is rejected");
        assert_ne!(winner, loser);
        let loser_error = results
            .iter()
            .find(|(call_id, _)| *call_id == loser)
            .unwrap()
            .1
            .as_ref()
            .unwrap_err();
        assert_eq!(
            loser_error.code,
            rmcp::model::ErrorCode(
                crate::protocol::ProtocolOutcome::RetryableTransition
                    .encode(crate::protocol::ProtocolEra::Modern)
                    .code
            )
        );

        // A losing guard/drop must not remove the winning key or record.
        router.remove_active_call(loser);
        let key = DownstreamCallKey::from(&context);
        assert_eq!(
            router.active_call_lookup.get(&key).map(|entry| *entry),
            Some(winner)
        );
        assert!(router.active_calls.contains_key(&winner));
        assert!(!router.active_calls.contains_key(&loser));

        // A cancellation carrying the rejected call's would-be upstream id
        // must not resolve to or notify the winning downstream call.
        router.route_upstream_cancelled(
            "upstream",
            CancelledNotificationParam::new(
                Some(RequestId::from(NumberOrString::Number(loser as i64))),
                Some("loser cancelled".to_string()),
            ),
        );
        assert!(matches!(
            notifications.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(router.active_calls.contains_key(&winner));

        router.remove_active_call(winner);
        assert!(!router.active_call_lookup.contains_key(&key));
        assert!(router.active_calls.is_empty());
    }
}

mod catalog;
mod completion;
pub mod continuations;
mod handler;
mod subscriptions;
mod tasks;
pub(crate) use catalog::*;
pub use handler::ProxyHandler;

#[cfg(test)]
mod tests;
