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

use crate::types::{SecretString, ServerHealth, ServerStatus, UpstreamServerMetadata};

/// Maximum IPC message size (4 MB). Reject before allocating buffer.
pub const MAX_FRAME_SIZE: u32 = 4 * 1024 * 1024;
/// Raw payload bytes per chunk when a logical daemon response exceeds one frame.
pub const RESPONSE_CHUNK_BYTES: usize = 512 * 1024;

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
/// Current daemon/client IPC protocol version.
pub const IPC_PROTOCOL_VERSION: u16 = 3;
pub const OPERATOR_IPC_MIN: u16 = 3;
pub const OPERATOR_IPC_MAX: u16 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonOwnershipMode {
    Unmanaged,
    CliManaged,
    AppManaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorCapability {
    ServerMutation,
    ClientMutation,
    AuthMutation,
    ConfigMutation,
    ActivityStream,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorHandshake {
    pub daemon_version: String,
    pub ipc_min: u16,
    pub ipc_max: u16,
    pub ownership: DaemonOwnershipMode,
    pub capabilities: Vec<OperatorCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorClientVisibility {
    pub session_id: String,
    pub client_type: crate::types::ClientType,
    pub visible_tool_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorSnapshot {
    pub runtime_version: String,
    pub uptime_secs: u64,
    pub ownership: DaemonOwnershipMode,
    pub configured_servers: Vec<crate::operator::OperatorServerSummary>,
    pub servers: Vec<ServerStatus>,
    pub live_sessions: Vec<IpcLiveSessionInfo>,
    pub client_visibility: Vec<OperatorClientVisibility>,
    pub upstream_auth: Vec<IpcAuthServerInfo>,
    pub downstream_clients: Vec<crate::downstream_oauth::RegisteredClientSummary>,
}

/// Requests sent from CLI → daemon over Unix socket.
///
/// Admin variants (RestartServer, Reload, Shutdown) require the daemon auth token.
/// MCP proxy variants (Register, Deregister, McpRequest) use socket ACL — any
/// process that can connect to the socket can register and proxy MCP calls.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcRequest {
    /// Query daemon status (servers, client count, uptime).
    Status,

    /// Negotiate the operator API used by the CLI and native app.
    OperatorHandshake {
        client_version: String,
        ipc_min: u16,
        ipc_max: u16,
    },
    /// Read the bounded metadata-only activity ring.
    ActivitySnapshot {
        auth_token: String,
        after_sequence: u64,
        limit: usize,
        failures_only: bool,
    },
    OperatorSnapshot {
        auth_token: String,
    },
    RevokeDownstreamClient {
        auth_token: String,
        client_id: String,
    },
    ValidateServer {
        auth_token: String,
        name: String,
        server: Box<crate::config::ServerConfig>,
    },
    AddServer {
        auth_token: String,
        name: String,
        server: Box<crate::config::ServerConfig>,
    },
    UpdateServer {
        auth_token: String,
        name: String,
        server: Box<crate::config::ServerConfig>,
    },
    RemoveServer {
        auth_token: String,
        name: String,
    },
    SetServerEnabled {
        auth_token: String,
        name: String,
        enabled: bool,
    },

    /// Restart a specific upstream server.
    RestartServer {
        server_id: String,
        auth_token: String,
    },
    /// Reload configuration from disk.
    Reload {
        auth_token: String,
    },

    /// Graceful daemon shutdown.
    Shutdown {
        auth_token: String,
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
    },

    /// Deregister a proxy client session (clean disconnect).
    Deregister {
        session_id: String,
    },

    /// Update a session's client info (sent after MCP initialize handshake).
    UpdateSession {
        session_id: String,
        client_info: String,
    },

    /// Liveness probe for long-lived proxy connections.
    Ping {
        session_id: String,
    },

    /// List all available tools across all servers.
    ListTools,
    /// List live proxy client sessions connected to the daemon.
    ListClients,
    /// List live downstream sessions with explicit transport/scope.
    ListLiveSessions,
    /// Get the daemon runtime's synthesized MCP capabilities.
    Capabilities {
        session_id: String,
    },
    /// Query the daemon-authoritative modern downstream gate.
    ModernDownstreamGate {
        session_id: String,
    },

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

    /// Inject OAuth credentials into the running daemon and trigger reconnect.
    InjectToken {
        auth_token: String,
        server_name: String,
        access_token: String,
        refresh_token: Option<String>,
        expires_in: Option<u64>,
    },
}

/// Custom Debug that redacts auth_token fields to prevent log leakage.
impl fmt::Debug for IpcRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status => write!(f, "Status"),
            Self::OperatorHandshake {
                client_version,
                ipc_min,
                ipc_max,
            } => f
                .debug_struct("OperatorHandshake")
                .field("client_version", client_version)
                .field("ipc_min", ipc_min)
                .field("ipc_max", ipc_max)
                .finish(),
            Self::ActivitySnapshot {
                after_sequence,
                limit,
                failures_only,
                ..
            } => f
                .debug_struct("ActivitySnapshot")
                .field("auth_token", &"[REDACTED]")
                .field("after_sequence", after_sequence)
                .field("limit", limit)
                .field("failures_only", failures_only)
                .finish(),
            Self::OperatorSnapshot { .. } => f
                .debug_struct("OperatorSnapshot")
                .field("auth_token", &"[REDACTED]")
                .finish(),
            Self::RevokeDownstreamClient { client_id, .. } => f
                .debug_struct("RevokeDownstreamClient")
                .field("auth_token", &"[REDACTED]")
                .field("client_id", client_id)
                .finish(),
            Self::ValidateServer { name, .. } => f
                .debug_struct("ValidateServer")
                .field("auth_token", &"[REDACTED]")
                .field("name", name)
                .finish(),
            Self::AddServer { name, .. } => f
                .debug_struct("AddServer")
                .field("auth_token", &"[REDACTED]")
                .field("name", name)
                .finish(),
            Self::UpdateServer { name, .. } => f
                .debug_struct("UpdateServer")
                .field("auth_token", &"[REDACTED]")
                .field("name", name)
                .finish(),
            Self::RemoveServer { name, .. } => f
                .debug_struct("RemoveServer")
                .field("auth_token", &"[REDACTED]")
                .field("name", name)
                .finish(),
            Self::SetServerEnabled { name, enabled, .. } => f
                .debug_struct("SetServerEnabled")
                .field("auth_token", &"[REDACTED]")
                .field("name", name)
                .field("enabled", enabled)
                .finish(),
            Self::RestartServer { server_id, .. } => f
                .debug_struct("RestartServer")
                .field("server_id", server_id)
                .field("auth_token", &"[REDACTED]")
                .finish(),
            Self::Reload { .. } => f
                .debug_struct("Reload")
                .field("auth_token", &"[REDACTED]")
                .finish(),
            Self::Shutdown { .. } => f
                .debug_struct("Shutdown")
                .field("auth_token", &"[REDACTED]")
                .finish(),
            Self::Register {
                protocol_version,
                client_id,
                client_info,
            } => f
                .debug_struct("Register")
                .field("protocol_version", protocol_version)
                .field("client_id", client_id)
                .field("client_info", client_info)
                .finish(),
            Self::Deregister { session_id } => f
                .debug_struct("Deregister")
                .field("session_id", session_id)
                .finish(),
            Self::UpdateSession {
                session_id,
                client_info,
            } => f
                .debug_struct("UpdateSession")
                .field("session_id", session_id)
                .field("client_info", client_info)
                .finish(),
            Self::Ping { session_id } => f
                .debug_struct("Ping")
                .field("session_id", session_id)
                .finish(),
            Self::ListTools => write!(f, "ListTools"),
            Self::ListClients => write!(f, "ListClients"),
            Self::ListLiveSessions => write!(f, "ListLiveSessions"),
            Self::Capabilities { session_id } => f
                .debug_struct("Capabilities")
                .field("session_id", session_id)
                .finish(),
            Self::ModernDownstreamGate { session_id } => f
                .debug_struct("ModernDownstreamGate")
                .field("session_id", session_id)
                .finish(),
            Self::McpRequest {
                session_id, method, ..
            } => f
                .debug_struct("McpRequest")
                .field("session_id", session_id)
                .field("method", method)
                .finish(),
            Self::McpRequestWithContext {
                session_id,
                method,
                context,
                ..
            } => f
                .debug_struct("McpRequestWithContext")
                .field("session_id", session_id)
                .field("method", method)
                .field("context", context)
                .finish(),
            Self::CancelMcpRequest {
                session_id,
                client_id,
                cancellation_capability: _,
                request_id,
                reason,
            } => f
                .debug_struct("CancelMcpRequest")
                .field("session_id", session_id)
                .field("client_id", client_id)
                .field("cancellation_capability", &"[REDACTED]")
                .field("request_id", request_id)
                .field("reason", reason)
                .finish(),
            Self::UpdateRoots { session_id, .. } => f
                .debug_struct("UpdateRoots")
                .field("session_id", session_id)
                .finish(),
            Self::UpdateCapabilities { session_id, .. } => f
                .debug_struct("UpdateCapabilities")
                .field("session_id", session_id)
                .finish(),
            Self::RestoreResourceSubscriptions { session_id, uris } => f
                .debug_struct("RestoreResourceSubscriptions")
                .field("session_id", session_id)
                .field("uris", uris)
                .finish(),
            Self::AuthStatus => write!(f, "AuthStatus"),
            Self::InjectToken {
                server_name,
                refresh_token,
                expires_in,
                ..
            } => f
                .debug_struct("InjectToken")
                .field("auth_token", &"[REDACTED]")
                .field("server_name", server_name)
                .field("access_token", &"[REDACTED]")
                .field(
                    "refresh_token",
                    if refresh_token.is_some() {
                        &"[REDACTED]"
                    } else {
                        &"None"
                    },
                )
                .field("expires_in", expires_in)
                .finish(),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
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
        let auth = if config.auth.as_deref() == Some("oauth") || config.oauth_client_id.is_some() {
            "oauth"
        } else if config.auth_token.is_some() {
            "bearer"
        } else {
            "none"
        };
        Self {
            transport: match config.transport {
                crate::config::TransportType::Stdio => "stdio",
                crate::config::TransportType::Http => "http",
                crate::config::TransportType::Sse => "sse",
            }
            .to_string(),
            enabled: config.enabled,
            command: config.command.clone(),
            args: config.args.clone(),
            url: config.url.clone(),
            auth: auth.to_string(),
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
        if server_id == "__plug_internal__" {
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
            Some(crate::config::TransportType::Http | crate::config::TransportType::Sse) => Self {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcClientInfo {
    pub client_id: String,
    pub session_id: String,
    pub client_info: Option<String>,
    pub connected_secs: u64,
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
    DaemonProxyOnly,
    HttpOnly,
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
    DownstreamClientRevoked {
        client_id: String,
    },
    ServerValidated {
        server: crate::operator::OperatorServerSummary,
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
    /// List of live client sessions connected to the daemon.
    Clients {
        clients: Vec<IpcClientInfo>,
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
    /// Normal response to an `IpcRequest`.
    Response { inner: IpcResponse },
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

/// Check whether a request requires the daemon master auth token.
///
/// Admin operations (RestartServer, Reload, Shutdown) require it.
/// MCP proxy operations (Register, Deregister, McpRequest) rely on
/// Unix socket file permissions for access control instead.
pub fn requires_auth(request: &IpcRequest) -> bool {
    matches!(
        request,
        IpcRequest::RestartServer { .. }
            | IpcRequest::Reload { .. }
            | IpcRequest::Shutdown { .. }
            | IpcRequest::InjectToken { .. }
            | IpcRequest::ActivitySnapshot { .. }
            | IpcRequest::OperatorSnapshot { .. }
            | IpcRequest::RevokeDownstreamClient { .. }
            | IpcRequest::ValidateServer { .. }
            | IpcRequest::AddServer { .. }
            | IpcRequest::UpdateServer { .. }
            | IpcRequest::RemoveServer { .. }
            | IpcRequest::SetServerEnabled { .. }
    )
}

/// Extract the auth_token from a request, if present.
pub fn extract_auth_token(request: &IpcRequest) -> Option<&str> {
    match request {
        IpcRequest::RestartServer { auth_token, .. }
        | IpcRequest::Reload { auth_token, .. }
        | IpcRequest::Shutdown { auth_token, .. }
        | IpcRequest::InjectToken { auth_token, .. } => Some(auth_token.as_str()),
        IpcRequest::ActivitySnapshot { auth_token, .. }
        | IpcRequest::OperatorSnapshot { auth_token, .. }
        | IpcRequest::RevokeDownstreamClient { auth_token, .. } => Some(auth_token.as_str()),
        IpcRequest::ValidateServer { auth_token, .. }
        | IpcRequest::AddServer { auth_token, .. }
        | IpcRequest::UpdateServer { auth_token, .. }
        | IpcRequest::RemoveServer { auth_token, .. }
        | IpcRequest::SetServerEnabled { auth_token, .. } => Some(auth_token.as_str()),
        _ => None,
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
    let payload = encode_response_payload(response)?;
    if payload.len() <= MAX_FRAME_SIZE as usize {
        return write_frame(writer, payload.as_ref()).await;
    }

    let chunk_count = payload.len().div_ceil(RESPONSE_CHUNK_BYTES);
    for (chunk_index, chunk) in payload.chunks(RESPONSE_CHUNK_BYTES).enumerate() {
        let message = DaemonToProxyMessage::ResponseChunk {
            chunk_index: chunk_index as u32,
            chunk_count: chunk_count as u32,
            payload_b64: base64::engine::general_purpose::STANDARD.encode(chunk),
        };
        send_daemon_message(writer, &message).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn request_serialization_round_trip() {
        let requests = vec![
            IpcRequest::Status,
            IpcRequest::OperatorHandshake {
                client_version: "0.5.0".to_string(),
                ipc_min: 3,
                ipc_max: 4,
            },
            IpcRequest::ActivitySnapshot {
                auth_token: "token".to_string(),
                after_sequence: 7,
                limit: 25,
                failures_only: true,
            },
            IpcRequest::RestartServer {
                server_id: "test-server".to_string(),
                auth_token: "abc123".to_string(),
            },
            IpcRequest::Shutdown {
                auth_token: "token".to_string(),
            },
            IpcRequest::Register {
                protocol_version: IPC_PROTOCOL_VERSION,
                client_id: "client-123".to_string(),
                client_info: Some("claude-code".to_string()),
            },
            IpcRequest::Register {
                protocol_version: IPC_PROTOCOL_VERSION,
                client_id: "client-456".to_string(),
                client_info: None,
            },
            IpcRequest::Deregister {
                session_id: "sess-123".to_string(),
            },
            IpcRequest::UpdateSession {
                session_id: "sess-123".to_string(),
                client_info: "claude-code".to_string(),
            },
            IpcRequest::McpRequest {
                session_id: "sess-123".to_string(),
                method: "tools/list".to_string(),
                params: None,
            },
            IpcRequest::McpRequest {
                session_id: "sess-123".to_string(),
                method: "tools/call".to_string(),
                params: Some(serde_json::json!({"name": "test_tool", "arguments": {}})),
            },
            IpcRequest::AuthStatus,
            IpcRequest::InjectToken {
                auth_token: "token".to_string(),
                server_name: "my-server".to_string(),
                access_token: "at-123".to_string(),
                refresh_token: Some("rt-456".to_string()),
                expires_in: Some(3600),
            },
            IpcRequest::InjectToken {
                auth_token: "token".to_string(),
                server_name: "other".to_string(),
                access_token: "at".to_string(),
                refresh_token: None,
                expires_in: None,
            },
        ];

        for req in &requests {
            let json = serde_json::to_string(req).unwrap();
            let deserialized: IpcRequest = serde_json::from_str(&json).unwrap();
            // Verify round-trip produces valid JSON
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn operator_handshake_round_trips_with_compatibility_range() {
        let response = IpcResponse::OperatorHandshake {
            handshake: OperatorHandshake {
                daemon_version: "0.5.0".to_string(),
                ipc_min: 3,
                ipc_max: 4,
                ownership: DaemonOwnershipMode::AppManaged,
                capabilities: vec![OperatorCapability::ServerMutation],
            },
        };
        let json = serde_json::to_value(&response).expect("serialize handshake");
        assert_eq!(json["handshake"]["ipc_min"], 3);

        let decoded: IpcResponse = serde_json::from_value(json).expect("decode handshake");
        let IpcResponse::OperatorHandshake { handshake } = decoded else {
            panic!("expected operator handshake");
        };
        assert_eq!(handshake.daemon_version, "0.5.0");
        assert_eq!(handshake.ipc_max, 4);
        assert_eq!(handshake.ownership, DaemonOwnershipMode::AppManaged);
    }

    #[test]
    fn response_serialization_round_trip() {
        let responses = vec![
            IpcResponse::Ok,
            IpcResponse::Error {
                code: "AUTH_FAILED".to_string(),
                message: "invalid token".to_string(),
            },
            IpcResponse::Status {
                servers: vec![],
                clients: 0,
                uptime_secs: 42,
                runtime_version: "0.3.0".to_string(),
                resource_subscriptions: 3,
            },
            IpcResponse::Tools {
                tools: vec![IpcToolInfo {
                    name: "Imessage__send".to_string(),
                    server_id: "imessage".to_string(),
                    description: Some("Send an iMessage".to_string()),
                    title: Some("iMessage: Send".to_string()),
                    icons: Some(vec![
                        Icon::new("https://example.com/imessage.png").with_mime_type("image/png"),
                    ]),
                    risk: IpcToolRiskInfo::default(),
                    source: None,
                    upstream: Some(UpstreamServerMetadata {
                        name: "iMessage Max".to_string(),
                        version: "1.2.1".to_string(),
                        title: Some("iMessage Max".to_string()),
                        description: None,
                        website_url: None,
                        icons: Some(vec![
                            Icon::new("data:image/png;base64,aGVsbG8=").with_mime_type("image/png"),
                        ]),
                        selected_protocol_version: None,
                    }),
                    trust: IpcTrustInfo::default(),
                }],
            },
            IpcResponse::LiveSessions {
                sessions: vec![IpcLiveSessionInfo {
                    transport: LiveSessionTransport::DaemonProxy,
                    client_id: Some("client-123".to_string()),
                    session_id: "sess-456".to_string(),
                    client_type: crate::types::ClientType::ClaudeCode,
                    client_info: Some("claude-code".to_string()),
                    connected_secs: 12,
                    last_activity_secs: Some(1),
                }],
                scope: LiveSessionInventoryScope::DaemonProxyOnly,
            },
            IpcResponse::Registered {
                protocol_version: IPC_PROTOCOL_VERSION,
                client_id: "client-123".to_string(),
                session_id: "sess-456".to_string(),
                modern_downstream_enabled: false,
                cancellation_capability: IpcCancellationCapability::default(),
            },
            IpcResponse::Capabilities {
                capabilities: serde_json::json!({"tools": {"listChanged": true}}),
            },
            IpcResponse::McpResponse {
                payload: serde_json::json!({"tools": []}),
            },
            IpcResponse::ToolListChangedNotification,
            IpcResponse::ResourceListChangedNotification,
            IpcResponse::ResourceUpdatedNotification {
                params: serde_json::json!({"uri": "file:///tmp/test.txt"}),
            },
            IpcResponse::PromptListChangedNotification,
            IpcResponse::ProgressNotification {
                params: serde_json::json!({"progressToken": "tok-1", "progress": 50, "total": 100}),
            },
            IpcResponse::CancelledNotification {
                params: serde_json::json!({"requestId": 42, "reason": "user cancelled"}),
            },
            IpcResponse::AuthStatus {
                servers: vec![IpcAuthServerInfo {
                    name: "my-server".to_string(),
                    url: Some("https://example.com".to_string()),
                    authenticated: true,
                    health: ServerHealth::Healthy,
                    scopes: Some(vec!["read".to_string()]),
                    token_expires_in_secs: Some(3600),
                    warnings: vec![],
                }],
            },
            IpcResponse::AuthStateChanged {
                server_id: "my-server".to_string(),
                state: ServerHealth::AuthRequired,
            },
        ];

        for resp in &responses {
            let json = serde_json::to_string(resp).unwrap();
            let deserialized: IpcResponse = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn tools_response_json_includes_icons_and_upstream_metadata() {
        let response = IpcResponse::Tools {
            tools: vec![IpcToolInfo {
                name: "Imessage__send".to_string(),
                server_id: "imessage".to_string(),
                description: None,
                title: Some("iMessage: Send".to_string()),
                icons: Some(vec![
                    Icon::new("https://example.com/imessage.png").with_mime_type("image/png"),
                ]),
                risk: IpcToolRiskInfo::default(),
                source: None,
                upstream: Some(UpstreamServerMetadata {
                    name: "iMessage Max".to_string(),
                    version: "1.2.1".to_string(),
                    title: Some("iMessage Max".to_string()),
                    description: None,
                    website_url: None,
                    icons: Some(vec![
                        Icon::new("data:image/png;base64,aGVsbG8=").with_mime_type("image/png"),
                    ]),
                    selected_protocol_version: Some("2025-11-25".to_string()),
                }),
                trust: IpcTrustInfo::default(),
            }],
        };

        let json = serde_json::to_value(response).expect("serialize tools response");

        assert_eq!(
            json["tools"][0]["icons"][0]["src"],
            "https://example.com/imessage.png"
        );
        assert_eq!(json["tools"][0]["upstream"]["name"], "iMessage Max");
        assert_eq!(
            json["tools"][0]["upstream"]["icons"][0]["src"],
            "data:image/png;base64,aGVsbG8="
        );
    }

    #[test]
    fn source_and_trust_metadata_do_not_expose_secrets() {
        let config = crate::config::ServerConfig {
            command: Some("uvx".to_string()),
            args: vec!["third-party-mcp".to_string()],
            env: std::collections::HashMap::new(),
            enabled: true,
            transport: crate::config::TransportType::Stdio,
            protocol_mode: Default::default(),
            url: None,
            auth_token: Some(crate::types::SecretString::from("secret-token".to_string())),
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: std::collections::HashMap::new(),
            tool_groups: Vec::new(),

            sandbox: None,
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
    fn requires_auth_identifies_admin_commands() {
        assert!(!requires_auth(&IpcRequest::Status));

        assert!(requires_auth(&IpcRequest::RestartServer {
            server_id: "s".to_string(),
            auth_token: "t".to_string(),
        }));
        assert!(requires_auth(&IpcRequest::Reload {
            auth_token: "t".to_string(),
        }));
        assert!(requires_auth(&IpcRequest::Shutdown {
            auth_token: "t".to_string(),
        }));
        assert!(requires_auth(&IpcRequest::OperatorSnapshot {
            auth_token: "t".to_string(),
        }));
        assert!(requires_auth(&IpcRequest::RevokeDownstreamClient {
            auth_token: "t".to_string(),
            client_id: "client".to_string(),
        }));
        assert!(requires_auth(&IpcRequest::InjectToken {
            auth_token: "t".to_string(),
            server_name: "s".to_string(),
            access_token: "a".to_string(),
            refresh_token: None,
            expires_in: None,
        }));
        assert!(requires_auth(&IpcRequest::ActivitySnapshot {
            auth_token: "t".to_string(),
            after_sequence: 0,
            limit: 500,
            failures_only: false,
        }));
        assert!(requires_auth(&IpcRequest::RemoveServer {
            auth_token: "t".to_string(),
            name: "server".to_string(),
        }));

        // MCP proxy variants do NOT require auth (socket ACL suffices)
        assert!(!requires_auth(&IpcRequest::Register {
            protocol_version: IPC_PROTOCOL_VERSION,
            client_id: "client-123".to_string(),
            client_info: None,
        }));
        assert!(!requires_auth(&IpcRequest::Deregister {
            session_id: "s".to_string(),
        }));
        assert!(!requires_auth(&IpcRequest::Capabilities {
            session_id: "s".to_string(),
        }));
        assert!(!requires_auth(&IpcRequest::ListLiveSessions));
        assert!(!requires_auth(&IpcRequest::UpdateSession {
            session_id: "s".to_string(),
            client_info: "claude-code".to_string(),
        }));
        assert!(!requires_auth(&IpcRequest::McpRequest {
            session_id: "s".to_string(),
            method: "tools/list".to_string(),
            params: None,
        }));
        assert!(!requires_auth(&IpcRequest::McpRequestWithContext {
            session_id: "s".to_string(),
            method: "tools/list".to_string(),
            params: None,
            context: IpcMcpRequestContext {
                request_id: RequestId::from(rmcp::model::NumberOrString::Number(1)),
                protocol_version: "2026-07-28".to_string(),
                client_name: Some("client".to_string()),
                client_version: Some("1".to_string()),
            },
        }));
        assert!(!requires_auth(&IpcRequest::CancelMcpRequest {
            session_id: "s".to_string(),
            client_id: "client".to_string(),
            cancellation_capability: IpcCancellationCapability::new("secret".to_string()),
            request_id: RequestId::from(rmcp::model::NumberOrString::Number(1)),
            reason: Some("stop".to_string()),
        }));
        assert!(!requires_auth(&IpcRequest::AuthStatus));
    }

    #[test]
    fn extract_auth_token_works() {
        assert_eq!(extract_auth_token(&IpcRequest::Status), None);
        assert_eq!(
            extract_auth_token(&IpcRequest::RestartServer {
                server_id: "s".to_string(),
                auth_token: "my_token".to_string(),
            }),
            Some("my_token")
        );
        assert_eq!(
            extract_auth_token(&IpcRequest::InjectToken {
                auth_token: "inject_tok".to_string(),
                server_name: "s".to_string(),
                access_token: "a".to_string(),
                refresh_token: None,
                expires_in: None,
            }),
            Some("inject_tok")
        );
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
    fn daemon_to_proxy_message_response_round_trip() {
        let msg = DaemonToProxyMessage::Response {
            inner: IpcResponse::Ok,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: DaemonToProxyMessage = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&deserialized).unwrap();
        assert_eq!(json, json2);
        assert!(json.contains("\"envelope\":\"Response\""));
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
    fn tagged_json_format() {
        let req = IpcRequest::Status;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"Status"}"#);

        let resp = IpcResponse::Ok;
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"type":"Ok"}"#);
    }

    #[test]
    fn debug_redacts_auth_token() {
        let req = IpcRequest::RestartServer {
            server_id: "srv".to_string(),
            auth_token: "super_secret_token".to_string(),
        };
        let debug_str = format!("{:?}", req);
        assert!(!debug_str.contains("super_secret"));
        assert!(debug_str.contains("[REDACTED]"));
    }

    #[test]
    fn debug_redacts_inject_token_secrets() {
        let req = IpcRequest::InjectToken {
            auth_token: "daemon_secret".to_string(),
            server_name: "my-server".to_string(),
            access_token: "bearer_secret".to_string(),
            refresh_token: Some("refresh_secret".to_string()),
            expires_in: Some(3600),
        };
        let debug_str = format!("{:?}", req);
        assert!(!debug_str.contains("daemon_secret"));
        assert!(!debug_str.contains("bearer_secret"));
        assert!(!debug_str.contains("refresh_secret"));
        assert!(debug_str.contains("[REDACTED]"));
        assert!(debug_str.contains("my-server"));
        assert!(debug_str.contains("3600"));
    }

    #[test]
    fn register_json_includes_protocol_and_client_identity() {
        let req = IpcRequest::Register {
            protocol_version: IPC_PROTOCOL_VERSION,
            client_id: "client-123".to_string(),
            client_info: Some("claude-code".to_string()),
        };

        let value = serde_json::to_value(req).unwrap();
        assert_eq!(value["type"], "Register");
        assert_eq!(value["protocol_version"], IPC_PROTOCOL_VERSION);
        assert_eq!(value["client_id"], "client-123");
        assert_eq!(value["client_info"], "claude-code");
    }

    #[test]
    fn registered_json_includes_protocol_and_client_identity() {
        let resp = IpcResponse::Registered {
            protocol_version: IPC_PROTOCOL_VERSION,
            client_id: "client-123".to_string(),
            session_id: "sess-123".to_string(),
            modern_downstream_enabled: false,
            cancellation_capability: IpcCancellationCapability::default(),
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
