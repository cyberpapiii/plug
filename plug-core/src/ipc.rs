//! IPC types for CLI ↔ daemon communication.
//!
//! Shared between `plug-core` (types) and `plug` (socket listener).
//! Wire format: 4-byte big-endian u32 length prefix + JSON payload.

use base64::Engine as _;
use std::borrow::Cow;
use std::fmt;

use rmcp::model::{
    ClientCapabilities, CreateMessageRequestParams, CreateMessageResult, ElicitRequestParams,
    ElicitResult, Icon, RequestId, ToolAnnotations,
};
use serde::{Deserialize, Serialize};

use crate::downstream_identity::ClientHost;
use crate::types::{SecretString, ServerHealth, ServerStatus, UpstreamServerMetadata};

/// Maximum IPC message size (4 MB). Reject before allocating buffer.
pub const MAX_FRAME_SIZE: u32 = 4 * 1024 * 1024;
/// Raw payload bytes per chunk when a logical daemon response exceeds one frame.
const RESPONSE_CHUNK_BYTES: usize = 512 * 1024;

/// Opaque, per-registration capability authorizing out-of-band cancellation.
/// It is serialized over the private IPC socket but always redacted in Debug.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IpcCancellationCapability(SecretString);

impl IpcCancellationCapability {
    pub fn new(secret: String) -> Self {
        Self(secret.into())
    }

    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for IpcCancellationCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

/// The proxy protocol `plug connect` registers at, and the only one the daemon
/// registers. A connector checks the daemon's version in the operator
/// handshake before it registers, so both ends always come from one build.
///
/// v4: a registered proxy connection tags requests with `ipc_id` and the
/// daemon answers them concurrently, echoing the id; replies to reverse
/// requests carry `reverse_id`. Untagged requests keep the one-at-a-time
/// behavior, so operator clients (the app, one-shot CLI commands) are
/// unaffected.
pub const IPC_PROTOCOL_VERSION: u16 = 4;

/// Requests one registered connection may have dispatching at once. The
/// daemon answers a request over it at once with [`IpcErrorCode::Busy`]
/// rather than queueing it, so the proxy keeps to the same limit. Requests
/// for which [`IpcRequest::runs_inline`] holds do not count.
pub const MAX_CONCURRENT_REQUESTS_PER_CONNECTION: usize = 32;

/// Longest a proxy waits for one reply before it gives up on the request.
///
/// The daemon bounds every upstream call by that server's
/// `call_timeout_secs`, elicitation or sampling inside the call included, so
/// twice the longest configured timeout plus a minute only trips on a request
/// the daemon has lost. Never below the default call timeout. The daemon sends
/// it at registration, from the config it is running.
pub fn proxy_request_ceiling(config: &crate::config::Config) -> std::time::Duration {
    let floor = crate::config::ServerConfig::default().call_timeout_secs;
    let longest = config
        .servers
        .values()
        .map(|server| server.call_timeout_secs.max(server.timeout_secs))
        .max()
        .unwrap_or(floor)
        .max(floor);
    std::time::Duration::from_secs(longest.saturating_mul(2).saturating_add(60))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonOwnershipMode {
    Unmanaged,
    Unknown,
    CliManaged,
    AppManaged,
}

/// The daemon's answer to [`IpcRequest::OperatorHandshake`]. A client goes
/// on only when `daemon_version` is its own, so the rest of the protocol is
/// one build talking to itself.
///
/// This is the one message a client of another build reads, to learn that it
/// has to be restarted. Connectors from 0.8.8 on refuse a handshake that lacks
/// any field they know, so its shape stays as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorHandshake {
    pub daemon_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_executable: Option<std::path::PathBuf>,
    pub ownership: DaemonOwnershipMode,
    #[serde(flatten)]
    retired: RetiredHandshakeFields,
}

impl OperatorHandshake {
    pub fn new(
        daemon_version: String,
        daemon_executable: Option<std::path::PathBuf>,
        ownership: DaemonOwnershipMode,
    ) -> Self {
        Self {
            daemon_version,
            daemon_executable,
            ownership,
            retired: RetiredHandshakeFields::default(),
        }
    }
}

/// A version band and a capability list that connectors of older builds
/// require. Nothing reads them now; they hold values those connectors accept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct RetiredHandshakeFields {
    ipc_min: u16,
    ipc_max: u16,
    capabilities: Vec<String>,
}

impl Default for RetiredHandshakeFields {
    fn default() -> Self {
        Self {
            ipc_min: 3,
            ipc_max: 6,
            capabilities: [
                "server_mutation",
                "client_mutation",
                "auth_mutation",
                "config_mutation",
                "activity_stream",
                "tool_mutation",
                "server_config_read",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

/// A name the owner gave a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientName {
    pub key: String,
    pub name: String,
}

/// Where the owner says a client runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPlace {
    pub key: String,
    pub place: String,
}

/// What the owner keeps a client from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientBlocks {
    pub key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// The client gets only what the two lists below name.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub only_allowed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_servers: Vec<String>,
    /// Single tools on the allow list, by the name each is listed under now.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    /// The configured servers the client gets nothing of, as the daemon
    /// enforces the lists above.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub off_servers: Vec<String>,
    /// The listed tools of every other server that the client is kept from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub off_tools: Vec<OffTool>,
}

/// A listed tool a client is kept from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OffTool {
    pub name: String,
    /// The entry of [`ClientBlocks::tools`] with a `*` in it that covers this
    /// tool, when one does. Turning the tool back on means changing the rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorSnapshot {
    pub runtime_version: String,
    pub uptime_secs: u64,
    /// Changes when the tool catalog could look different to a reader.
    ///
    /// The snapshot is polled every couple of seconds; the tool list is nearly
    /// a megabyte and almost never changes between two polls. Carrying the
    /// revision on the cheap request lets a reader refetch the expensive one
    /// only when it would return something new.
    #[serde(default)]
    pub tool_catalog_revision: u64,
    pub ownership: DaemonOwnershipMode,
    pub configured_servers: Vec<crate::operator::OperatorServerSummary>,
    pub servers: Vec<ServerStatus>,
    pub live_sessions: Vec<IpcLiveSessionInfo>,
    pub upstream_auth: Vec<IpcAuthServerInfo>,
    pub downstream_clients: Vec<crate::downstream_oauth::RegisteredClientSummary>,
    /// Names the owner gave clients. A list, not a map, because the keys hold
    /// paths and a reader may rewrite map keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_names: Vec<ClientName>,
    /// Where the owner says each client runs. Clients on this Mac are left
    /// out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_places: Vec<ClientPlace>,
    /// What each client is kept from. Clients kept from nothing are left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_blocks: Vec<ClientBlocks>,
    /// The watches in the config and how each is doing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<crate::events::EventStatus>,
    /// The address a client that is not on the list of known ones is given to
    /// reach Plug: the public one when there is one, else this Mac's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_address: Option<String>,
    /// What joins a server's prefix to a tool's name in the names clients
    /// call (`prefix_delimiter` in the config).
    #[serde(default)]
    pub prefix_delimiter: String,
}

/// Requests sent to the daemon over its Unix socket.
///
/// [`IpcRequest::Operator`] carries everything that needs the daemon token.
/// The proxy requests (Register, McpRequest and the rest) rely on the
/// socket's file permissions instead: any process that can connect may
/// register and proxy MCP calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcRequest {
    /// Query daemon status (servers, client count, uptime).
    Status,

    /// Ask which daemon this is, before anything else.
    OperatorHandshake { client_version: String },

    /// A request only the holder of the daemon token may make.
    Operator {
        auth_token: SecretString,
        request: OperatorRequest,
    },

    /// Register a new proxy client session with the daemon.
    /// Returns `Registered` with an assigned session ID.
    Register {
        /// Daemon/client IPC protocol version.
        protocol_version: u16,
        /// Stable logical client identity across reconnects.
        client_id: String,
        /// Client type from MCP initialize (e.g., "claude-code", "cursor").
        client_info: Option<String>,
        /// Connector binary version. Absent for older adapters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        adapter_version: Option<String>,
        /// The program that started this connector. Absent for older adapters
        /// and when the process table could not be read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<ClientHost>,
        /// The target `plug link` wrote into this client's config
        /// (`plug connect --client <target>`). Absent for a connector started
        /// any other way.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link_target: Option<String>,
    },

    /// Deregister a proxy client session (clean disconnect).
    Deregister { session_id: String },

    /// Update a session's client info (sent after MCP initialize handshake).
    UpdateSession {
        session_id: String,
        client_info: String,
    },

    /// Liveness probe for long-lived proxy connections.
    Ping { session_id: String },

    /// List all available tools across all servers.
    ListTools,
    /// List live downstream sessions.
    ListLiveSessions,
    /// Get the daemon runtime's synthesized MCP capabilities.
    Capabilities { session_id: String },
    /// Query the daemon-authoritative modern downstream gate.
    ModernDownstreamGate { session_id: String },

    /// Proxy an MCP JSON-RPC request through the daemon's shared Engine.
    McpRequest {
        session_id: String,
        /// Raw MCP JSON-RPC method name (e.g., "tools/list", "tools/call").
        method: String,
        /// JSON-RPC params object.
        params: Option<serde_json::Value>,
    },

    /// Context-preserving MCP request used by current proxy clients.
    /// Legacy `McpRequest` remains for old callers; subscription reconnect
    /// replay uses `RestoreResourceSubscriptions`.
    McpRequestWithContext {
        session_id: String,
        method: String,
        params: Option<serde_json::Value>,
        context: IpcMcpRequestContext,
    },

    /// Cancel one in-flight downstream request. The daemon verifies that the
    /// transport session still belongs to `client_id` before routing it.
    CancelMcpRequest {
        session_id: String,
        client_id: String,
        cancellation_capability: IpcCancellationCapability,
        request_id: RequestId,
        reason: Option<String>,
    },

    /// Push updated workspace roots from a downstream client to the daemon.
    UpdateRoots {
        session_id: String,
        /// Serialized `Vec<Root>` from the downstream client.
        roots: serde_json::Value,
    },

    /// Update a session's MCP client capabilities after initialize.
    UpdateCapabilities {
        session_id: String,
        capabilities: Box<ClientCapabilities>,
    },

    /// Restore many resource subscriptions in one IPC round-trip (reconnect replay).
    RestoreResourceSubscriptions {
        session_id: String,
        uris: Vec<String>,
    },

    /// Query OAuth authentication status for all configured servers.
    AuthStatus,
}

/// What the owner of the daemon token asks of it: the command line and the
/// app. It travels inside [`IpcRequest::Operator`], which carries the token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OperatorRequest {
    /// The servers, clients, sessions and events, as the app shows them.
    Snapshot,
    /// Read the bounded metadata-only activity ring.
    Activity {
        after_sequence: u64,
        limit: usize,
        failures_only: bool,
    },
    /// One server's config, with its secrets replaced by placeholders.
    GetServerConfig {
        name: String,
    },
    RevokeDownstreamClient {
        client_id: String,
    },
    /// Check a server that is not in the config yet.
    ValidateServer {
        name: String,
        server: Box<crate::config::ServerConfig>,
    },
    /// List the operations of the OpenAPI document at `spec`, so a person
    /// can choose among them before adding the server.
    DescribeApi {
        spec: String,
    },
    /// Change the config file and apply the change.
    Mutate {
        mutation: Box<crate::operator::OperatorMutation>,
        /// Where the keys typed into a server being added or updated are
        /// kept: `keychain` when absent, or `file` for Plug's `.env` file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret_store: Option<String>,
    },
    RestartServer {
        server_id: String,
    },
    /// Reload configuration from disk.
    Reload,
    /// Graceful daemon shutdown.
    Shutdown,
}

impl IpcRequest {
    /// The session this request acts in, when only the connection that
    /// registered that session may send it. The daemon checks that before
    /// the request runs. Cancellation names a session too, but arrives on a
    /// socket of its own and proves itself with the session's cancellation
    /// capability instead.
    pub fn connection_session_id(&self) -> Option<&str> {
        match self {
            Self::Deregister { session_id }
            | Self::UpdateSession { session_id, .. }
            | Self::Ping { session_id }
            | Self::Capabilities { session_id }
            | Self::ModernDownstreamGate { session_id }
            | Self::McpRequest { session_id, .. }
            | Self::McpRequestWithContext { session_id, .. }
            | Self::UpdateRoots { session_id, .. }
            | Self::UpdateCapabilities { session_id, .. }
            | Self::RestoreResourceSubscriptions { session_id, .. } => Some(session_id),
            Self::CancelMcpRequest { .. }
            | Self::Status
            | Self::OperatorHandshake { .. }
            | Self::Operator { .. }
            | Self::Register { .. }
            | Self::ListTools
            | Self::ListLiveSessions
            | Self::AuthStatus => None,
        }
    }
}

/// Request-scoped downstream identity that must survive the private daemon
/// hop. Principal and durable owner are deliberately not serialized: the
/// daemon derives both from its authenticated client registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcMcpRequestContext {
    pub request_id: RequestId,
    pub protocol_version: String,
    pub client_name: Option<String>,
    pub client_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcToolInfo {
    pub name: String,
    pub server_id: String,
    pub description: Option<String>,
    pub title: Option<String>,
    /// True when `disabled_tools` hides this tool from downstream clients.
    /// The listing itself is unfiltered so operators can see and undo the
    /// choice.
    #[serde(default)]
    pub disabled: bool,
    /// The `disabled_tools` entry responsible, when it is a wildcard rather
    /// than this tool's exact name. A single tool inside a wildcard cannot be
    /// turned back on without removing the pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_by_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    /// The tool's name on its own server, which a watch is written with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_name: Option<String>,
    /// Whether Plug will watch this tool without being told to allow writes.
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub risk: IpcToolRiskInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<IpcServerSourceInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamServerMetadata>,
    #[serde(default)]
    pub trust: IpcTrustInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IpcToolRiskInfo {
    pub upstream_declared: IpcToolRiskHints,
    pub plug_inferred: IpcToolRiskHints,
    pub effective: IpcToolRiskHints,
    pub has_conflict: bool,
}

impl IpcToolRiskInfo {
    pub fn from_annotations(
        upstream_declared: Option<&ToolAnnotations>,
        plug_inferred: Option<&ToolAnnotations>,
        effective: Option<&ToolAnnotations>,
    ) -> Self {
        let upstream_declared = IpcToolRiskHints::from_annotations(upstream_declared);
        let plug_inferred = IpcToolRiskHints::from_annotations(plug_inferred);
        let effective = IpcToolRiskHints::from_annotations(effective);
        let has_conflict = upstream_declared.conflicts_with(&plug_inferred);

        Self {
            upstream_declared,
            plug_inferred,
            effective,
            has_conflict,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IpcToolRiskHints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world: Option<bool>,
}

impl IpcToolRiskHints {
    fn from_annotations(annotations: Option<&ToolAnnotations>) -> Self {
        match annotations {
            Some(annotations) => Self {
                read_only: annotations.read_only_hint,
                destructive: annotations.destructive_hint,
                idempotent: annotations.idempotent_hint,
                open_world: annotations.open_world_hint,
            },
            None => Self::default(),
        }
    }

    fn conflicts_with(&self, other: &Self) -> bool {
        option_conflicts(self.read_only, other.read_only)
            || option_conflicts(self.destructive, other.destructive)
            || option_conflicts(self.idempotent, other.idempotent)
            || option_conflicts(self.open_world, other.open_world)
    }
}

fn option_conflicts(left: Option<bool>, right: Option<bool>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcServerSourceInfo {
    pub transport: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub auth: String,
}

impl IpcServerSourceInfo {
    pub fn from_config(config: &crate::config::ServerConfig) -> Self {
        Self {
            transport: match config.transport {
                crate::config::TransportType::Stdio => "stdio",
                crate::config::TransportType::Http => "http",
                crate::config::TransportType::Sse => "sse",
                crate::config::TransportType::OpenApi => "openapi",
            }
            .to_string(),
            enabled: config.enabled,
            command: config.command.clone(),
            args: config.args.clone(),
            url: config.url.clone(),
            auth: config.auth_label().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcTrustInfo {
    pub tier: String,
    pub source: String,
    pub boundary: String,
}

impl Default for IpcTrustInfo {
    fn default() -> Self {
        Self {
            tier: "unknown".to_string(),
            source: "unknown".to_string(),
            boundary: "unknown".to_string(),
        }
    }
}

impl IpcTrustInfo {
    pub fn for_server(server_id: &str, config: Option<&crate::config::ServerConfig>) -> Self {
        if server_id == crate::tool_naming::INTERNAL_SERVER_ID {
            return Self {
                tier: "plug_internal".to_string(),
                source: "plug_builtin".to_string(),
                boundary: "internal".to_string(),
            };
        }

        match config.map(|cfg| &cfg.transport) {
            Some(crate::config::TransportType::Stdio) => Self {
                tier: "configured_local_process".to_string(),
                source: "local_config".to_string(),
                boundary: "local_process".to_string(),
            },
            Some(
                crate::config::TransportType::Http
                | crate::config::TransportType::Sse
                | crate::config::TransportType::OpenApi,
            ) => Self {
                tier: "configured_remote_server".to_string(),
                source: "local_config".to_string(),
                boundary: "network".to_string(),
            },
            None => Self {
                tier: "runtime_unknown".to_string(),
                source: "runtime".to_string(),
                boundary: "unknown".to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveSessionTransport {
    DaemonProxy,
    Http,
    Sse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveSessionInventoryScope {
    TransportComplete,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcLiveSessionInfo {
    pub transport: LiveSessionTransport,
    pub client_id: Option<String>,
    pub session_id: String,
    pub client_type: crate::types::ClientType,
    pub client_info: Option<String>,
    #[serde(default)]
    pub adapter_version: Option<String>,
    /// The program that started a local connector. Never set for a remote
    /// session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<ClientHost>,
    /// The OAuth client whose token opened a remote session. Never set for a
    /// local connector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<String>,
    /// The target `plug link` wrote into a local client's config. Never set
    /// for a remote session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
    /// What this client's settings are stored under. See
    /// [`crate::downstream_identity::local_client_key`] and
    /// [`crate::downstream_identity::remote_client_key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key: Option<String>,
    /// The key this session's requests carry, which per-client blocks are
    /// read under. It differs from `client_key` only for a remote session
    /// with no grant: its requests cannot be told from any other on the
    /// shared token, so they share one key, while its row is still named
    /// after what it reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_key: Option<String>,
    /// The client Plug takes this one for, by its link target. See
    /// [`crate::downstream_identity::client_target`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub connected_secs: u64,
    pub last_activity_secs: Option<u64>,
}

/// Per-server OAuth authentication info returned by `AuthStatus`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcAuthServerInfo {
    pub name: String,
    pub url: Option<String>,
    pub authenticated: bool,
    pub health: ServerHealth,
    pub scopes: Option<Vec<String>>,
    pub token_expires_in_secs: Option<u64>,
    pub warnings: Vec<String>,
}

/// Responses sent from daemon → CLI over Unix socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcResponse {
    /// Daemon version, compatibility range, ownership, and operator features.
    OperatorHandshake {
        handshake: OperatorHandshake,
    },
    ActivitySnapshot {
        events: Vec<crate::activity::ActivityEvent>,
    },
    OperatorSnapshot {
        snapshot: Box<OperatorSnapshot>,
    },
    ServerConfig {
        name: String,
        server: Box<crate::config::ServerConfig>,
    },
    DownstreamClientRevoked {
        client_id: String,
    },
    ServerValidated {
        server: crate::operator::OperatorServerSummary,
    },
    ApiDescribed {
        api: crate::openapi::ApiSummary,
    },
    OperatorMutation {
        result: crate::operator::OperatorMutationResult,
        reload: crate::reload::ReloadReport,
    },
    /// Status response with server info, client count, and uptime.
    Status {
        servers: Vec<ServerStatus>,
        clients: usize,
        uptime_secs: u64,
        /// Version of the binary hosting this daemon process.
        #[serde(default)]
        runtime_version: String,
        /// Resource URIs with at least one downstream subscriber.
        ///
        /// Defaulted so a newer CLI still reads an older daemon's response,
        /// where the count reads as zero rather than failing the whole query.
        #[serde(default)]
        resource_subscriptions: usize,
    },
    /// List of all tools available.
    Tools {
        tools: Vec<IpcToolInfo>,
    },
    /// List of live downstream sessions with explicit transport/scope.
    LiveSessions {
        sessions: Vec<IpcLiveSessionInfo>,
        scope: LiveSessionInventoryScope,
    },
    /// Synthesized MCP capabilities for the daemon-backed shared runtime.
    Capabilities {
        capabilities: serde_json::Value,
    },
    /// Current daemon-authoritative modern downstream gate.
    ModernDownstreamGate {
        enabled: bool,
    },
    /// Success acknowledgement for mutating commands.
    Ok,
    /// Config reload result with restart-required warnings.
    Reloaded {
        report: crate::reload::ReloadReport,
    },
    /// Liveness acknowledgement for long-lived proxy connections.
    Pong,
    /// Error with machine-parseable code and human-readable message.
    Error {
        code: String,
        message: String,
    },

    /// Registration acknowledgement with assigned session ID.
    Registered {
        protocol_version: u16,
        client_id: String,
        session_id: String,
        /// Shared, daemon-authoritative modern downstream gate.
        #[serde(default)]
        modern_downstream_enabled: bool,
        /// Secret capability for auxiliary cancellation sockets.
        #[serde(default)]
        cancellation_capability: IpcCancellationCapability,
        /// Longest the proxy waits for one reply; see [`proxy_request_ceiling`].
        request_ceiling_ms: u64,
    },

    /// MCP JSON-RPC response from the daemon's shared Engine.
    McpResponse {
        /// The JSON-RPC result payload.
        payload: serde_json::Value,
    },

    /// Push notification: logging message from an upstream server.
    ///
    /// Sent asynchronously by the daemon (interleaved with responses) after
    /// a proxy client registers. The payload is a serialized
    /// `LoggingMessageNotificationParam`.
    LoggingNotification {
        params: serde_json::Value,
    },

    // ── Protocol push notifications ──────────────────────────────────────
    /// Push notification: the tool list changed (upstream server added/removed tools).
    ToolListChangedNotification,
    /// Push notification: the resource list changed.
    ResourceListChangedNotification,
    /// Push notification: a subscribed resource changed.
    /// Payload is a serialized `ResourceUpdatedNotificationParam`.
    ResourceUpdatedNotification {
        params: serde_json::Value,
    },
    /// Push notification: the prompt list changed.
    PromptListChangedNotification,
    /// Push notification: progress update for an in-flight tool call.
    /// Payload is a serialized `ProgressNotificationParam`.
    ProgressNotification {
        params: serde_json::Value,
    },
    /// Push notification: an in-flight tool call was cancelled.
    /// Payload is a serialized `CancelledNotificationParam`.
    CancelledNotification {
        params: serde_json::Value,
    },

    /// OAuth authentication status for all configured servers.
    AuthStatus {
        servers: Vec<IpcAuthServerInfo>,
    },

    /// Push notification: a server's authentication state changed.
    AuthStateChanged {
        server_id: String,
        state: ServerHealth,
    },
    /// Push update for the daemon-authoritative modern downstream gate.
    ModernDownstreamGateChanged {
        enabled: bool,
    },
}

// ──────────────────────── Reverse-request IPC types ──────────────────────────
//
// During an active tool call the daemon may need to forward "reverse requests"
// (elicitation, sampling) from the upstream MCP server back to the proxy client
// that initiated the call. These types model the daemon-to-proxy direction.

/// Daemon-to-proxy reverse request (sent during an active tool call).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcClientRequest {
    CreateElicitation { params: ElicitRequestParams },
    CreateMessage { params: CreateMessageRequestParams },
}

/// Proxy-to-daemon response for a reverse request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcClientResponse {
    CreateElicitation { result: ElicitResult },
    CreateMessage { result: CreateMessageResult },
    Error { message: String },
}

/// Messages the daemon can send to a proxy client.
///
/// The IPC socket is normally request-response (client sends `IpcRequest`,
/// daemon replies `IpcResponse`). However, during a long-running `tools/call`
/// the daemon may need to interleave reverse requests. This tagged envelope
/// lets the proxy's read loop distinguish the two cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "envelope")]
pub enum DaemonToProxyMessage {
    /// Fragment of a large logical `IpcResponse`.
    ResponseChunk {
        chunk_index: u32,
        chunk_count: u32,
        payload_b64: String,
    },
    /// Reverse request requiring the proxy to respond with an `IpcClientResponse`.
    ReverseRequest {
        id: u64,
        request: Box<IpcClientRequest>,
    },
}

impl IpcRequest {
    /// The daemon token this request presents, if it is one that needs it.
    pub fn auth_token(&self) -> Option<&str> {
        match self {
            IpcRequest::Operator { auth_token, .. } => Some(auth_token.as_str()),
            _ => None,
        }
    }

    /// Requests the daemon runs on the connection loop even when tagged:
    /// those that change the connection's own session and so must run in
    /// order, and cheap control requests that must answer even when every
    /// request slot is taken by a long call (the proxy's heartbeat is a
    /// `Ping`). They take no request slot on either end.
    pub fn runs_inline(&self) -> bool {
        matches!(
            self,
            IpcRequest::Register { .. }
                | IpcRequest::Deregister { .. }
                | IpcRequest::Operator {
                    request: OperatorRequest::Shutdown,
                    ..
                }
                | IpcRequest::Ping { .. }
                | IpcRequest::ModernDownstreamGate { .. }
        )
    }
}

impl IpcResponse {
    /// Pushes the daemon sends a registered connection unasked. Everything
    /// else answers a request.
    pub fn is_push(&self) -> bool {
        matches!(
            self,
            IpcResponse::LoggingNotification { .. }
                | IpcResponse::ToolListChangedNotification
                | IpcResponse::ResourceListChangedNotification
                | IpcResponse::ResourceUpdatedNotification { .. }
                | IpcResponse::PromptListChangedNotification
                | IpcResponse::ProgressNotification { .. }
                | IpcResponse::CancelledNotification { .. }
                | IpcResponse::AuthStateChanged { .. }
                | IpcResponse::ModernDownstreamGateChanged { .. }
        )
    }

    pub fn error(code: IpcErrorCode, message: impl Into<String>) -> Self {
        IpcResponse::Error {
            code: code.as_str().to_string(),
            message: message.into(),
        }
    }

    /// The code of an `Error` reply, when it is one a client acts on.
    pub fn error_code(&self) -> Option<IpcErrorCode> {
        match self {
            IpcResponse::Error { code, .. } => IpcErrorCode::parse(code),
            _ => None,
        }
    }

    /// An MCP answer as an IPC reply. An error rides the same `McpResponse`
    /// as a result, told apart by its `code` field.
    pub fn from_mcp_result(result: Result<serde_json::Value, rmcp::ErrorData>) -> Self {
        let payload = match result {
            Ok(payload) => payload,
            Err(error) => match serde_json::to_value(error) {
                Ok(payload) => payload,
                Err(error) => {
                    return IpcResponse::error(IpcErrorCode::SerializeError, error.to_string());
                }
            },
        };
        IpcResponse::McpResponse { payload }
    }
}

/// The MCP answer an `McpResponse` payload carries: the reverse of
/// [`IpcResponse::from_mcp_result`].
pub fn mcp_result(payload: serde_json::Value) -> Result<serde_json::Value, rmcp::ErrorData> {
    if payload.get("code").is_some()
        && let Ok(error) = serde_json::from_value::<rmcp::ErrorData>(payload.clone())
    {
        return Err(error);
    }
    Ok(payload)
}

/// `Error` codes a client acts on. The wire carries the string, so a code
/// missing here still reaches a client as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcErrorCode {
    /// The frame did not parse as a request. From a daemon that predates a
    /// request the client sends, this means the daemon needs a restart.
    ParseError,
    /// Register asked for a protocol version this daemon does not speak.
    ProtocolVersionUnsupported,
    /// The request names a session this connection did not register.
    SessionMismatch,
    /// A newer registration for the same client replaced this session.
    SessionReplaced,
    /// The connection already has [`MAX_CONCURRENT_REQUESTS_PER_CONNECTION`]
    /// requests running.
    Busy,
    SerializeError,
    /// Raised by `plug connect`, never by the daemon: the connection dropped
    /// while a request that must not run twice was in flight, so it is not
    /// resent and the host has to retry it.
    RequestRetryUnsafe,
}

impl IpcErrorCode {
    const ALL: [IpcErrorCode; 7] = [
        IpcErrorCode::ParseError,
        IpcErrorCode::ProtocolVersionUnsupported,
        IpcErrorCode::SessionMismatch,
        IpcErrorCode::SessionReplaced,
        IpcErrorCode::Busy,
        IpcErrorCode::SerializeError,
        IpcErrorCode::RequestRetryUnsafe,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            IpcErrorCode::ParseError => "PARSE_ERROR",
            IpcErrorCode::ProtocolVersionUnsupported => "PROTOCOL_VERSION_UNSUPPORTED",
            IpcErrorCode::SessionMismatch => "SESSION_MISMATCH",
            IpcErrorCode::SessionReplaced => "SESSION_REPLACED",
            IpcErrorCode::Busy => "BUSY",
            IpcErrorCode::SerializeError => "SERIALIZE_ERROR",
            IpcErrorCode::RequestRetryUnsafe => "REQUEST_RETRY_UNSAFE",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|known| known.as_str() == code)
    }
}

impl fmt::Display for IpcErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ──────────────────────── Length-prefixed framing ─────────────────────────────

/// Read a length-prefixed JSON frame from an async reader.
///
/// Wire format: 4-byte big-endian u32 length + JSON payload.
/// Returns None on clean EOF (connection closed).
pub async fn read_frame<R: tokio::io::AsyncReadExt + Unpin>(
    reader: &mut R,
) -> anyhow::Result<Option<Vec<u8>>> {
    let len = match reader.read_u32().await {
        Ok(len) => len,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    if len > MAX_FRAME_SIZE {
        anyhow::bail!("frame too large: {len} bytes (max {MAX_FRAME_SIZE})");
    }

    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Write a length-prefixed JSON frame to an async writer.
pub async fn write_frame<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> anyhow::Result<()> {
    if payload.len() > MAX_FRAME_SIZE as usize {
        anyhow::bail!(
            "payload too large: {} bytes (max {MAX_FRAME_SIZE})",
            payload.len()
        );
    }
    let len = u32::try_from(payload.len())
        .map_err(|_| anyhow::anyhow!("payload too large: {} bytes", payload.len()))?;
    writer.write_u32(len).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Pre-encoded payloads for unit-shaped `IpcResponse` values (no params).
fn cached_unit_response_payload(response: &IpcResponse) -> Option<&'static [u8]> {
    use std::sync::OnceLock;
    macro_rules! unit_payload {
        ($variant:expr) => {{
            static PAYLOAD: OnceLock<Vec<u8>> = OnceLock::new();
            Some(
                PAYLOAD
                    .get_or_init(|| {
                        serde_json::to_vec(&$variant).expect("unit IpcResponse serializes")
                    })
                    .as_slice(),
            )
        }};
    }
    match response {
        IpcResponse::ToolListChangedNotification => {
            unit_payload!(IpcResponse::ToolListChangedNotification)
        }
        IpcResponse::ResourceListChangedNotification => {
            unit_payload!(IpcResponse::ResourceListChangedNotification)
        }
        IpcResponse::PromptListChangedNotification => {
            unit_payload!(IpcResponse::PromptListChangedNotification)
        }
        IpcResponse::Ok => unit_payload!(IpcResponse::Ok),
        IpcResponse::Pong => unit_payload!(IpcResponse::Pong),
        _ => None,
    }
}

fn encode_response_payload(response: &IpcResponse) -> anyhow::Result<Cow<'_, [u8]>> {
    if let Some(payload) = cached_unit_response_payload(response) {
        return Ok(Cow::Borrowed(payload));
    }
    Ok(Cow::Owned(serde_json::to_vec(response)?))
}

/// Send an IpcResponse as a length-prefixed JSON frame.
pub async fn send_response<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &IpcResponse,
) -> anyhow::Result<()> {
    let payload = encode_response_payload(response)?;
    write_frame(writer, payload.as_ref()).await
}

/// Send a `DaemonToProxyMessage` as a length-prefixed JSON frame.
pub async fn send_daemon_message<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
    message: &DaemonToProxyMessage,
) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(message)?;
    write_frame(writer, &payload).await
}

/// Send an `IpcResponse`, chunking it into daemon envelopes if it exceeds one frame.
pub async fn send_chunked_response<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &IpcResponse,
) -> anyhow::Result<()> {
    send_chunked_response_with_id(writer, None, response).await
}

/// `send_chunked_response` for a reply to a request that carried `ipc_id`.
///
/// The id travels inside the logical response, so a chunked response carries
/// it once, in the reassembled payload. A connection has one writer, which
/// writes all chunks of a response back to back; chunks of two responses never
/// interleave.
pub async fn send_chunked_response_with_id<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
    ipc_id: Option<u64>,
    response: &IpcResponse,
) -> anyhow::Result<()> {
    let payload = match ipc_id {
        None => encode_response_payload(response)?,
        Some(id) => Cow::Owned(encode_tagged(Some(id), response)?),
    };
    if payload.len() <= MAX_FRAME_SIZE as usize {
        return write_frame(writer, payload.as_ref()).await;
    }

    for message in response_chunks(&payload, RESPONSE_CHUNK_BYTES) {
        send_daemon_message(writer, &message).await?;
    }
    Ok(())
}

/// The `ResponseChunk` envelopes a response payload too large for one frame
/// travels as, `chunk_bytes` of it in each.
pub fn response_chunks(
    payload: &[u8],
    chunk_bytes: usize,
) -> impl Iterator<Item = DaemonToProxyMessage> + '_ {
    let chunk_count = payload.len().div_ceil(chunk_bytes) as u32;
    payload
        .chunks(chunk_bytes)
        .enumerate()
        .map(
            move |(chunk_index, chunk)| DaemonToProxyMessage::ResponseChunk {
                chunk_index: chunk_index as u32,
                chunk_count,
                payload_b64: base64::engine::general_purpose::STANDARD.encode(chunk),
            },
        )
}

/// Every `DaemonToProxyMessage` frame starts with its tag: serde writes an
/// internally tagged enum's tag first, and [`send_daemon_message`] is the only
/// writer of envelopes. Matching the prefix keeps plain `IpcResponse` frames
/// (the hot path) to one parse, and a payload that merely contains an
/// `"envelope"` key somewhere is never mistaken for one.
const ENVELOPE_FRAME_PREFIX: &[u8] = b"{\"envelope\":";

/// One logical message from the daemon, after chunk reassembly.
#[derive(Debug)]
pub enum DaemonFrame {
    Reply {
        ipc_id: Option<u64>,
        response: IpcResponse,
    },
    Reverse {
        id: u64,
        request: IpcClientRequest,
    },
}

/// A daemon frame that could not be decoded. It concerns that frame alone.
#[derive(Debug)]
pub struct FrameDecodeError(String);

impl fmt::Display for FrameDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FrameDecodeError {}

/// Decodes frames written by [`send_chunked_response_with_id`] and
/// [`send_daemon_message`], reassembling chunked responses. The daemon writes
/// all chunks of a response back to back, so only one is ever in progress.
#[derive(Default)]
pub struct DaemonFrameDecoder {
    buffer: Vec<u8>,
    expected: Option<u32>,
}

impl DaemonFrameDecoder {
    /// `Ok(None)` means a chunk was taken and its response is not complete.
    pub fn decode(&mut self, frame: &[u8]) -> Result<Option<DaemonFrame>, FrameDecodeError> {
        let message = if frame.starts_with(ENVELOPE_FRAME_PREFIX) {
            Some(
                serde_json::from_slice::<DaemonToProxyMessage>(frame)
                    .map_err(|e| FrameDecodeError(format!("invalid envelope message: {e}")))?,
            )
        } else {
            None
        };
        if let Some(DaemonToProxyMessage::ResponseChunk {
            chunk_index,
            chunk_count,
            payload_b64,
        }) = message
        {
            return self.take_chunk(chunk_index, chunk_count, &payload_b64);
        }
        if self.expected.is_some() {
            *self = Self::default();
            return Err(FrameDecodeError(
                "a frame interleaved with a chunked response".to_string(),
            ));
        }
        match message {
            Some(DaemonToProxyMessage::ReverseRequest { id, request }) => {
                Ok(Some(DaemonFrame::Reverse {
                    id,
                    request: *request,
                }))
            }
            _ => decode_reply(frame, "invalid IPC response").map(Some),
        }
    }

    fn take_chunk(
        &mut self,
        chunk_index: u32,
        chunk_count: u32,
        payload_b64: &str,
    ) -> Result<Option<DaemonFrame>, FrameDecodeError> {
        match self.append(chunk_index, chunk_count, payload_b64) {
            Ok(false) => Ok(None),
            Ok(true) => {
                let whole = std::mem::take(self);
                decode_reply(&whole.buffer, "invalid chunked IPC response").map(Some)
            }
            Err(message) => {
                *self = Self::default();
                Err(FrameDecodeError(message))
            }
        }
    }

    /// Add one chunk; true once the response is whole.
    fn append(
        &mut self,
        chunk_index: u32,
        chunk_count: u32,
        payload_b64: &str,
    ) -> Result<bool, String> {
        if chunk_count == 0 {
            return Err("invalid response chunk count 0".to_string());
        }
        if chunk_index == 0 {
            self.buffer.clear();
            self.expected = Some(chunk_count);
        } else if self.expected != Some(chunk_count) {
            return Err("response chunk count changed mid-stream".to_string());
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload_b64)
            .map_err(|e| format!("invalid chunk payload: {e}"))?;
        self.buffer.extend_from_slice(&decoded);
        Ok(chunk_index + 1 == chunk_count)
    }
}

fn decode_reply(bytes: &[u8], context: &str) -> Result<DaemonFrame, FrameDecodeError> {
    let response =
        serde_json::from_slice(bytes).map_err(|e| FrameDecodeError(format!("{context}: {e}")))?;
    Ok(DaemonFrame::Reply {
        ipc_id: FrameIds::peek(bytes).ipc_id,
        response,
    })
}

// ──────────────────────── Request correlation (v4) ────────────────────────────
//
// A registered proxy connection carries many requests at once. A request may
// carry a top-level `ipc_id`; the daemon echoes it on the response so the proxy
// can route replies that arrive out of order. The proxy's reply to a daemon
// `ReverseRequest { id, .. }` carries that id as `reverse_id`. Both fields sit
// beside the variant's own fields, so a peer that does not know them ignores
// them, and a frame without them keeps the one-at-a-time meaning. Push
// notifications never carry an id.

/// Correlation ids read off the top level of a frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct FrameIds {
    #[serde(default)]
    pub ipc_id: Option<u64>,
    #[serde(default)]
    pub reverse_id: Option<u64>,
}

impl FrameIds {
    /// Ids present at the top level of `frame`. A frame that is not a JSON
    /// object reports none; its own parse reports the error.
    pub fn peek(frame: &[u8]) -> Self {
        serde_json::from_slice(frame).unwrap_or_default()
    }
}

#[derive(Serialize)]
struct Tagged<'a, T: Serialize> {
    #[serde(skip_serializing_if = "Option::is_none")]
    ipc_id: Option<u64>,
    #[serde(flatten)]
    inner: &'a T,
}

#[derive(Serialize)]
struct ReverseReply<'a> {
    reverse_id: u64,
    #[serde(flatten)]
    inner: &'a IpcClientResponse,
}

/// Encode an `IpcRequest` or `IpcResponse` with an optional top-level `ipc_id`.
pub fn encode_tagged<T: Serialize>(ipc_id: Option<u64>, inner: &T) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(&Tagged { ipc_id, inner })
}

/// Encode the proxy's reply to the daemon's `ReverseRequest { id, .. }`.
pub fn encode_reverse_response(
    reverse_id: u64,
    response: &IpcClientResponse,
) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(&ReverseReply {
        reverse_id,
        inner: response,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_frames_round_trip_and_stay_readable_without_the_id() {
        for request in [
            IpcRequest::Status,
            IpcRequest::Ping {
                session_id: "s".to_string(),
            },
            IpcRequest::McpRequest {
                session_id: "s".to_string(),
                method: "tools/list".to_string(),
                params: Some(serde_json::json!({ "ipc_id": 99 })),
            },
        ] {
            let bytes = encode_tagged(Some(7), &request).expect("encode request");
            assert_eq!(FrameIds::peek(&bytes).ipc_id, Some(7));
            let decoded: IpcRequest = serde_json::from_slice(&bytes).expect("decode request");
            assert_eq!(
                serde_json::to_value(&decoded).unwrap(),
                serde_json::to_value(&request).unwrap()
            );
            let untagged = encode_tagged(None, &request).expect("encode untagged");
            assert_eq!(untagged, serde_json::to_vec(&request).unwrap());
            assert_eq!(FrameIds::peek(&untagged), FrameIds::default());
        }

        for response in [
            IpcResponse::Pong,
            IpcResponse::Ok,
            IpcResponse::McpResponse {
                payload: serde_json::json!({ "ipc_id": 5, "content": [] }),
            },
        ] {
            let bytes = encode_tagged(Some(u64::MAX), &response).expect("encode response");
            assert_eq!(FrameIds::peek(&bytes).ipc_id, Some(u64::MAX));
            let decoded: IpcResponse = serde_json::from_slice(&bytes).expect("decode response");
            assert_eq!(
                serde_json::to_value(&decoded).unwrap(),
                serde_json::to_value(&response).unwrap()
            );
        }

        let reply = IpcClientResponse::Error {
            message: "declined".to_string(),
        };
        let bytes = encode_reverse_response(3, &reply).expect("encode reverse reply");
        assert_eq!(
            FrameIds::peek(&bytes),
            FrameIds {
                ipc_id: None,
                reverse_id: Some(3)
            }
        );
        assert!(matches!(
            serde_json::from_slice::<IpcClientResponse>(&bytes).expect("decode reverse reply"),
            IpcClientResponse::Error { message } if message == "declined"
        ));
        assert_eq!(FrameIds::peek(b"not json"), FrameIds::default());
    }

    #[tokio::test]
    async fn the_decoder_reassembles_what_the_encoder_chunks() {
        let text = "x".repeat(MAX_FRAME_SIZE as usize + RESPONSE_CHUNK_BYTES);
        let response = IpcResponse::McpResponse {
            payload: serde_json::json!({ "text": text }),
        };
        let mut wire = Vec::new();
        send_chunked_response_with_id(&mut wire, Some(9), &response)
            .await
            .expect("encode");
        send_daemon_message(
            &mut wire,
            &DaemonToProxyMessage::ReverseRequest {
                id: 4,
                request: Box::new(
                    serde_json::from_value(serde_json::json!({
                        "type": "CreateElicitation",
                        "params": { "message": "?", "requestedSchema": { "type": "object", "properties": {} } }
                    }))
                    .expect("reverse request"),
                ),
            },
        )
        .await
        .expect("encode reverse");

        let mut reader = wire.as_slice();
        let mut decoder = DaemonFrameDecoder::default();
        let mut decoded = Vec::new();
        let mut frames = 0;
        while let Some(frame) = read_frame(&mut reader).await.expect("frame") {
            assert!(frame.len() <= MAX_FRAME_SIZE as usize);
            frames += 1;
            if let Some(message) = decoder.decode(&frame).expect("decode") {
                decoded.push(message);
            }
        }
        assert!(frames > 2, "the reply must have been chunked");
        let [
            DaemonFrame::Reply {
                ipc_id: Some(9),
                response: IpcResponse::McpResponse { payload },
            },
            DaemonFrame::Reverse { id: 4, .. },
        ] = decoded.as_slice()
        else {
            panic!("unexpected frames: {decoded:?}");
        };
        assert_eq!(payload["text"].as_str().map(str::len), Some(text.len()));
    }

    #[test]
    fn envelopes_start_with_their_tag_and_replies_never_do() {
        let chunk = DaemonToProxyMessage::ResponseChunk {
            chunk_index: 0,
            chunk_count: 1,
            payload_b64: String::new(),
        };
        assert!(
            serde_json::to_vec(&chunk)
                .unwrap()
                .starts_with(ENVELOPE_FRAME_PREFIX)
        );
        let reply = IpcResponse::McpResponse {
            payload: serde_json::json!({ "envelope": "ResponseChunk" }),
        };
        let bytes = encode_tagged(Some(1), &reply).unwrap();
        assert!(!bytes.starts_with(ENVELOPE_FRAME_PREFIX));
        assert!(matches!(
            DaemonFrameDecoder::default().decode(&bytes),
            Ok(Some(DaemonFrame::Reply {
                ipc_id: Some(1),
                ..
            }))
        ));
    }

    #[test]
    fn a_bad_chunk_resets_the_decoder() {
        let mut decoder = DaemonFrameDecoder::default();
        let chunk = |index, count, payload: &str| {
            serde_json::to_vec(&DaemonToProxyMessage::ResponseChunk {
                chunk_index: index,
                chunk_count: count,
                payload_b64: payload.to_string(),
            })
            .unwrap()
        };
        assert!(decoder.decode(&chunk(0, 2, "e30=")).unwrap().is_none());
        assert!(decoder.decode(&chunk(1, 3, "e30=")).is_err());
        assert!(decoder.decode(&chunk(0, 0, "")).is_err());
        assert!(decoder.decode(&chunk(0, 2, "e30=")).unwrap().is_none());
        assert!(
            decoder.decode(br#"{"type":"Pong"}"#).is_err(),
            "chunks of one response are written back to back"
        );
        let ok = base64::engine::general_purpose::STANDARD.encode(b"{\"type\":\"Ok\"}");
        assert!(matches!(
            decoder.decode(&chunk(0, 1, &ok)),
            Ok(Some(DaemonFrame::Reply {
                response: IpcResponse::Ok,
                ..
            }))
        ));
    }

    #[test]
    fn error_codes_round_trip_through_the_wire_string() {
        for code in IpcErrorCode::ALL {
            let response = IpcResponse::error(code, "message");
            assert_eq!(response.error_code(), Some(code));
        }
        let unknown = IpcResponse::Error {
            code: "SOMETHING_ELSE".to_string(),
            message: String::new(),
        };
        assert_eq!(unknown.error_code(), None);
    }

    #[test]
    fn an_mcp_error_rides_an_mcp_response_and_comes_back_out() {
        let IpcResponse::McpResponse { payload } =
            IpcResponse::from_mcp_result(Ok(serde_json::json!({ "ok": true })))
        else {
            panic!("expected McpResponse");
        };
        assert_eq!(
            mcp_result(payload).unwrap(),
            serde_json::json!({ "ok": true })
        );
        let error = IpcResponse::from_mcp_result(Err(rmcp::ErrorData::invalid_params(
            "boom".to_string(),
            None,
        )));
        let IpcResponse::McpResponse { payload } = error else {
            panic!("expected McpResponse, got {error:?}");
        };
        assert_eq!(payload["code"].as_i64(), Some(-32602));
        assert_eq!(mcp_result(payload).unwrap_err().message, "boom");
    }

    #[test]
    fn the_proxy_ceiling_follows_the_longest_call_timeout_and_never_drops_below_the_default() {
        let mut config = crate::config::Config::default();
        assert_eq!(proxy_request_ceiling(&config).as_secs(), 300 * 2 + 60);
        config.servers.insert(
            "slow".to_string(),
            crate::config::ServerConfig {
                call_timeout_secs: 1_000,
                ..Default::default()
            },
        );
        config.servers.insert(
            "quick".to_string(),
            crate::config::ServerConfig {
                call_timeout_secs: 5,
                ..Default::default()
            },
        );
        assert_eq!(proxy_request_ceiling(&config).as_secs(), 1_000 * 2 + 60);
    }

    #[test]
    fn every_push_is_classified_and_no_reply_is() {
        assert!(
            IpcResponse::ResourceUpdatedNotification {
                params: serde_json::Value::Null
            }
            .is_push()
        );
        assert!(IpcResponse::ModernDownstreamGateChanged { enabled: true }.is_push());
        assert!(!IpcResponse::Pong.is_push());
        assert!(!IpcResponse::ModernDownstreamGate { enabled: true }.is_push());
    }

    #[test]
    fn cancellation_capability_preserves_wire_value_and_redaction() {
        let capability = IpcCancellationCapability::new("capability-secret".to_string());

        assert_eq!(capability.expose_secret(), "capability-secret");
        assert_eq!(format!("{capability:?}"), "[REDACTED]");
        assert_eq!(
            serde_json::to_value(&capability).expect("serialize capability"),
            serde_json::Value::String("capability-secret".to_string())
        );

        let decoded: IpcCancellationCapability =
            serde_json::from_value(serde_json::Value::String("capability-secret".to_string()))
                .expect("deserialize capability");
        assert_eq!(decoded, capability);
    }

    #[test]
    fn source_and_trust_metadata_do_not_expose_secrets() {
        let config = crate::config::ServerConfig {
            command: Some("uvx".to_string()),
            args: vec!["third-party-mcp".to_string()],
            auth_token: Some(crate::types::SecretString::from("secret-token".to_string())),
            ..Default::default()
        };

        let source = IpcServerSourceInfo::from_config(&config);
        assert_eq!(source.transport, "stdio");
        assert_eq!(source.command.as_deref(), Some("uvx"));
        assert_eq!(source.auth, "bearer");
        let serialized = serde_json::to_string(&source).expect("serialize source");
        assert!(!serialized.contains("secret-token"));

        let trust = IpcTrustInfo::for_server("workspace", Some(&config));
        assert_eq!(trust.tier, "configured_local_process");
        assert_eq!(trust.source, "local_config");
        assert_eq!(trust.boundary, "local_process");
    }

    #[test]
    fn tool_risk_metadata_separates_declared_inferred_and_effective_hints() {
        let declared = ToolAnnotations::new().read_only(true).destructive(false);
        let inferred = ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(true);

        let risk =
            IpcToolRiskInfo::from_annotations(Some(&declared), Some(&inferred), Some(&inferred));

        assert_eq!(risk.upstream_declared.read_only, Some(true));
        assert_eq!(risk.plug_inferred.destructive, Some(true));
        assert_eq!(risk.effective.idempotent, Some(false));
        assert!(risk.has_conflict);
    }

    #[test]
    fn ipc_client_response_serialization_round_trip() {
        let error_resp = IpcClientResponse::Error {
            message: "test error".to_string(),
        };
        let json = serde_json::to_string(&error_resp).unwrap();
        let deserialized: IpcClientResponse = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&deserialized).unwrap();
        assert_eq!(json, json2);
        assert!(json.contains("\"type\":\"Error\""));
    }

    #[test]
    fn daemon_to_proxy_message_reverse_request_has_envelope_tag() {
        // Build a CreateMessage request via JSON to avoid needing constructors
        let create_msg_json = serde_json::json!({
            "type": "CreateMessage",
            "params": {
                "messages": [{"role": "user", "content": {"type": "text", "text": "hello"}}],
                "maxTokens": 100,
            }
        });
        let request: IpcClientRequest = serde_json::from_value(create_msg_json).unwrap();
        let msg = DaemonToProxyMessage::ReverseRequest {
            id: 42,
            request: Box::new(request),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"envelope\":\"ReverseRequest\""));
        assert!(json.contains("\"id\":42"));
        // Verify it round-trips
        let deserialized: DaemonToProxyMessage = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&deserialized).unwrap();
        assert_eq!(json, json2);
    }

    #[test]
    fn only_operator_requests_carry_the_token_and_debug_never_shows_it() {
        let request = IpcRequest::Operator {
            auth_token: "super_secret_token".to_string().into(),
            request: OperatorRequest::RestartServer {
                server_id: "srv".to_string(),
            },
        };
        assert_eq!(request.auth_token(), Some("super_secret_token"));
        let debug = format!("{request:?}");
        assert!(!debug.contains("super_secret"), "{debug}");
        assert!(debug.contains("srv"), "{debug}");
        assert_eq!(IpcRequest::Status.auth_token(), None);
        assert_eq!(IpcRequest::AuthStatus.auth_token(), None);
    }

    #[test]
    fn register_json_includes_protocol_and_client_identity() {
        let req = IpcRequest::Register {
            protocol_version: IPC_PROTOCOL_VERSION,
            client_id: "client-123".to_string(),
            client_info: Some("claude-code".to_string()),
            adapter_version: Some("0.6.5".to_string()),
            host: None,
            link_target: None,
        };

        let value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["type"], "Register");
        assert_eq!(value["protocol_version"], IPC_PROTOCOL_VERSION);
        assert_eq!(value["client_id"], "client-123");
        assert_eq!(value["client_info"], "claude-code");
        assert_eq!(value["adapter_version"], "0.6.5");

        let decoded: IpcRequest = serde_json::from_value(value).unwrap();
        assert!(matches!(
            decoded,
            IpcRequest::Register {
                adapter_version: Some(version),
                host: None,
                link_target: None,
                ..
            } if version == "0.6.5"
        ));
    }

    #[test]
    fn register_carries_the_host_program_and_omits_it_when_unknown() {
        let host = ClientHost {
            name: "Hermes".to_string(),
            executable: "/Applications/Hermes.app/Contents/MacOS/Hermes".to_string(),
            app: Some("/Applications/Hermes.app".to_string()),
            script: None,
        };
        let request = |host| IpcRequest::Register {
            protocol_version: IPC_PROTOCOL_VERSION,
            client_id: "client-123".to_string(),
            client_info: None,
            adapter_version: None,
            host,
            link_target: None,
        };

        let value = serde_json::to_value(request(Some(host.clone()))).unwrap();
        assert_eq!(value["host"]["name"], "Hermes");
        assert!(value.get("link_target").is_none());
        assert!(value["host"].get("script").is_none());
        match serde_json::from_value::<IpcRequest>(value).unwrap() {
            IpcRequest::Register { host: parsed, .. } => assert_eq!(parsed, Some(host)),
            other => panic!("expected Register, got {other:?}"),
        }
        assert!(
            serde_json::to_value(request(None))
                .unwrap()
                .get("host")
                .is_none()
        );
    }

    #[test]
    fn register_json_without_adapter_version_remains_compatible() {
        let decoded: IpcRequest = serde_json::from_value(serde_json::json!({
            "type": "Register",
            "protocol_version": IPC_PROTOCOL_VERSION,
            "client_id": "legacy-client",
            "client_info": "claude-code"
        }))
        .unwrap();

        assert!(matches!(
            decoded,
            IpcRequest::Register {
                adapter_version: None,
                host: None,
                link_target: None,
                ..
            }
        ));
    }

    #[test]
    fn registered_json_includes_protocol_and_client_identity() {
        let resp = IpcResponse::Registered {
            protocol_version: IPC_PROTOCOL_VERSION,
            client_id: "client-123".to_string(),
            session_id: "sess-123".to_string(),
            modern_downstream_enabled: false,
            cancellation_capability: IpcCancellationCapability::default(),
            request_ceiling_ms: 660_000,
        };

        let value = serde_json::to_value(resp).unwrap();
        assert_eq!(value["type"], "Registered");
        assert_eq!(value["protocol_version"], IPC_PROTOCOL_VERSION);
        assert_eq!(value["client_id"], "client-123");
        assert_eq!(value["session_id"], "sess-123");
    }

    #[test]
    fn contextual_request_and_typed_cancellation_round_trip() {
        let request_id = RequestId::from(rmcp::model::NumberOrString::String(
            std::sync::Arc::from("req-7"),
        ));
        let request = IpcRequest::McpRequestWithContext {
            session_id: "session-1".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({"name": "server__tool"})),
            context: IpcMcpRequestContext {
                request_id: request_id.clone(),
                protocol_version: "2026-07-28".to_string(),
                client_name: Some("modern-client".to_string()),
                client_version: Some("2.0".to_string()),
            },
        };
        let encoded = serde_json::to_vec(&request).expect("serialize contextual request");
        let decoded: IpcRequest =
            serde_json::from_slice(&encoded).expect("deserialize contextual request");
        assert!(matches!(
            decoded,
            IpcRequest::McpRequestWithContext { context, .. }
                if context.request_id == request_id
                    && context.protocol_version == "2026-07-28"
        ));

        let cancellation = IpcRequest::CancelMcpRequest {
            session_id: "session-1".to_string(),
            client_id: "stable-client".to_string(),
            cancellation_capability: IpcCancellationCapability::new("secret".to_string()),
            request_id: request_id.clone(),
            reason: Some("user cancelled".to_string()),
        };
        let debug = format!("{cancellation:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret"));
        let encoded = serde_json::to_vec(&cancellation).expect("serialize cancellation");
        let decoded: IpcRequest =
            serde_json::from_slice(&encoded).expect("deserialize cancellation");
        assert!(matches!(
            decoded,
            IpcRequest::CancelMcpRequest { client_id, request_id: id, .. }
                if client_id == "stable-client" && id == request_id
        ));
    }

    #[test]
    fn capabilities_json_round_trips() {
        let req = IpcRequest::Capabilities {
            session_id: "sess-123".to_string(),
        };
        let req_json = serde_json::to_value(req).unwrap();
        assert_eq!(req_json["type"], "Capabilities");
        assert_eq!(req_json["session_id"], "sess-123");

        let resp = IpcResponse::Capabilities {
            capabilities: serde_json::json!({
                "tools": { "listChanged": true },
                "resources": { "listChanged": false }
            }),
        };
        let resp_json = serde_json::to_value(resp).unwrap();
        assert_eq!(resp_json["type"], "Capabilities");
        assert!(resp_json["capabilities"]["tools"].is_object());
    }
}
