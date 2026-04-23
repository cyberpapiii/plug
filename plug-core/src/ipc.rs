//! IPC types for CLI ↔ daemon communication.
//!
//! Shared between `plug-core` (types) and `plug` (socket listener).
//! Wire format: 4-byte big-endian u32 length prefix + JSON payload.

use base64::Engine as _;
use std::fmt;

use rmcp::model::{
    ClientCapabilities, CreateElicitationRequestParams, CreateElicitationResult,
    CreateMessageRequestParams, CreateMessageResult,
};
use serde::{Deserialize, Serialize};

use crate::types::{ServerHealth, ServerStatus};

/// Maximum IPC message size (4 MB). Reject before allocating buffer.
pub const MAX_FRAME_SIZE: u32 = 4 * 1024 * 1024;
/// Raw payload bytes per chunk when a logical daemon response exceeds one frame.
pub const RESPONSE_CHUNK_BYTES: usize = 512 * 1024;
/// Current daemon/client IPC protocol version.
pub const IPC_PROTOCOL_VERSION: u16 = 3;

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

    /// Restart a specific upstream server.
    RestartServer {
        server_id: String,
        auth_token: String,
    },
    /// Reload configuration from disk.
    Reload { auth_token: String },

    /// Graceful daemon shutdown.
    Shutdown { auth_token: String },

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
    /// List live proxy client sessions connected to the daemon.
    ListClients,
    /// List live downstream sessions with explicit transport/scope.
    ListLiveSessions,
    /// Get the daemon runtime's synthesized MCP capabilities.
    Capabilities { session_id: String },

    /// Proxy an MCP JSON-RPC request through the daemon's shared Engine.
    McpRequest {
        session_id: String,
        /// Raw MCP JSON-RPC method name (e.g., "tools/list", "tools/call").
        method: String,
        /// JSON-RPC params object.
        params: Option<serde_json::Value>,
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
            Self::McpRequest {
                session_id, method, ..
            } => f
                .debug_struct("McpRequest")
                .field("session_id", session_id)
                .field("method", method)
                .finish(),
            Self::UpdateRoots { session_id, .. } => f
                .debug_struct("UpdateRoots")
                .field("session_id", session_id)
                .finish(),
            Self::UpdateCapabilities { session_id, .. } => f
                .debug_struct("UpdateCapabilities")
                .field("session_id", session_id)
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcToolInfo {
    pub name: String,
    pub server_id: String,
    pub description: Option<String>,
    pub title: Option<String>,
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
    /// Status response with server info, client count, and uptime.
    Status {
        servers: Vec<ServerStatus>,
        clients: usize,
        uptime_secs: u64,
    },
    /// List of all tools available.
    Tools { tools: Vec<IpcToolInfo> },
    /// List of live client sessions connected to the daemon.
    Clients { clients: Vec<IpcClientInfo> },
    /// List of live downstream sessions with explicit transport/scope.
    LiveSessions {
        sessions: Vec<IpcLiveSessionInfo>,
        scope: LiveSessionInventoryScope,
    },
    /// Synthesized MCP capabilities for the daemon-backed shared runtime.
    Capabilities { capabilities: serde_json::Value },
    /// Success acknowledgement for mutating commands.
    Ok,
    /// Config reload result with restart-required warnings.
    Reloaded { report: crate::reload::ReloadReport },
    /// Liveness acknowledgement for long-lived proxy connections.
    Pong,
    /// Error with machine-parseable code and human-readable message.
    Error { code: String, message: String },

    /// Registration acknowledgement with assigned session ID.
    Registered {
        protocol_version: u16,
        client_id: String,
        session_id: String,
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
    LoggingNotification { params: serde_json::Value },

    // ── Protocol push notifications ──────────────────────────────────────
    /// Push notification: the tool list changed (upstream server added/removed tools).
    ToolListChangedNotification,
    /// Push notification: the resource list changed.
    ResourceListChangedNotification,
    /// Push notification: the prompt list changed.
    PromptListChangedNotification,
    /// Push notification: progress update for an in-flight tool call.
    /// Payload is a serialized `ProgressNotificationParam`.
    ProgressNotification { params: serde_json::Value },
    /// Push notification: an in-flight tool call was cancelled.
    /// Payload is a serialized `CancelledNotificationParam`.
    CancelledNotification { params: serde_json::Value },

    /// OAuth authentication status for all configured servers.
    AuthStatus { servers: Vec<IpcAuthServerInfo> },

    /// Push notification: a server's authentication state changed.
    AuthStateChanged {
        server_id: String,
        state: ServerHealth,
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
    CreateElicitation {
        params: CreateElicitationRequestParams,
    },
    CreateMessage {
        params: CreateMessageRequestParams,
    },
}

/// Proxy-to-daemon response for a reverse request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcClientResponse {
    CreateElicitation { result: CreateElicitationResult },
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
    )
}

/// Extract the auth_token from a request, if present.
pub fn extract_auth_token(request: &IpcRequest) -> Option<&str> {
    match request {
        IpcRequest::RestartServer { auth_token, .. }
        | IpcRequest::Reload { auth_token, .. }
        | IpcRequest::Shutdown { auth_token, .. }
        | IpcRequest::InjectToken { auth_token, .. } => Some(auth_token.as_str()),
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

/// Send an IpcResponse as a length-prefixed JSON frame.
pub async fn send_response<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &IpcResponse,
) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(response)?;
    write_frame(writer, &payload).await
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
    let payload = serde_json::to_vec(response)?;
    if payload.len() <= MAX_FRAME_SIZE as usize {
        return write_frame(writer, &payload).await;
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
    fn request_serialization_round_trip() {
        let requests = vec![
            IpcRequest::Status,
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
            },
            IpcResponse::Capabilities {
                capabilities: serde_json::json!({"tools": {"listChanged": true}}),
            },
            IpcResponse::McpResponse {
                payload: serde_json::json!({"tools": []}),
            },
            IpcResponse::ToolListChangedNotification,
            IpcResponse::ResourceListChangedNotification,
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
        assert!(requires_auth(&IpcRequest::InjectToken {
            auth_token: "t".to_string(),
            server_name: "s".to_string(),
            access_token: "a".to_string(),
            refresh_token: None,
            expires_in: None,
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
        };

        let value = serde_json::to_value(resp).unwrap();
        assert_eq!(value["type"], "Registered");
        assert_eq!(value["protocol_version"], IPC_PROTOCOL_VERSION);
        assert_eq!(value["client_id"], "client-123");
        assert_eq!(value["session_id"], "sess-123");
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
