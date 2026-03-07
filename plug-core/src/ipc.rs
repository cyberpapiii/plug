//! IPC types for CLI ↔ daemon communication.
//!
//! Shared between `plug-core` (types) and `plug` (socket listener).
//! Wire format: 4-byte big-endian u32 length prefix + JSON payload.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::ServerStatus;

/// Maximum IPC message size (4 MB). Reject before allocating buffer.
pub const MAX_FRAME_SIZE: u32 = 4 * 1024 * 1024;
/// Current daemon/client IPC protocol version.
pub const IPC_PROTOCOL_VERSION: u16 = 2;

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
    /// Synthesized MCP capabilities for the daemon-backed shared runtime.
    Capabilities { capabilities: serde_json::Value },
    /// Success acknowledgement for mutating commands.
    Ok,
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
    LoggingNotification {
        params: serde_json::Value,
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
        IpcRequest::RestartServer { .. } | IpcRequest::Reload { .. } | IpcRequest::Shutdown { .. }
    )
}

/// Extract the auth_token from a request, if present.
pub fn extract_auth_token(request: &IpcRequest) -> Option<&str> {
    match request {
        IpcRequest::RestartServer { auth_token, .. }
        | IpcRequest::Reload { auth_token, .. }
        | IpcRequest::Shutdown { auth_token, .. } => Some(auth_token.as_str()),
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
        assert!(!requires_auth(&IpcRequest::UpdateSession {
            session_id: "s".to_string(),
            client_info: "claude-code".to_string(),
        }));
        assert!(!requires_auth(&IpcRequest::McpRequest {
            session_id: "s".to_string(),
            method: "tools/list".to_string(),
            params: None,
        }));
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
