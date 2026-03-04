//! IPC types for CLI ↔ daemon communication.
//!
//! Shared between `plug-core` (types) and `plug` (socket listener).
//! Wire format: 4-byte big-endian u32 length prefix + JSON payload.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::ServerStatus;

/// Maximum IPC message size (4 MB). Reject before allocating buffer.
pub const MAX_FRAME_SIZE: u32 = 4 * 1024 * 1024;

/// Requests sent from CLI → daemon over Unix socket.
///
/// Only 3 variants — Status (read-only), RestartServer and Shutdown (mutating).
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
    /// Graceful daemon shutdown.
    Shutdown { auth_token: String },
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
            Self::Shutdown { .. } => f
                .debug_struct("Shutdown")
                .field("auth_token", &"[REDACTED]")
                .finish(),
        }
    }
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
    /// Success acknowledgement for mutating commands.
    Ok,
    /// Error with machine-parseable code and human-readable message.
    Error { code: String, message: String },
}

/// Check whether a request requires authentication.
pub fn requires_auth(request: &IpcRequest) -> bool {
    matches!(
        request,
        IpcRequest::RestartServer { .. } | IpcRequest::Shutdown { .. }
    )
}

/// Extract the auth_token from a request, if present.
pub fn extract_auth_token(request: &IpcRequest) -> Option<&str> {
    match request {
        IpcRequest::RestartServer { auth_token, .. } | IpcRequest::Shutdown { auth_token, .. } => {
            Some(auth_token.as_str())
        }
        _ => None,
    }
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
        ];

        for resp in &responses {
            let json = serde_json::to_string(resp).unwrap();
            let deserialized: IpcResponse = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn requires_auth_identifies_mutating_commands() {
        assert!(!requires_auth(&IpcRequest::Status));

        assert!(requires_auth(&IpcRequest::RestartServer {
            server_id: "s".to_string(),
            auth_token: "t".to_string(),
        }));
        assert!(requires_auth(&IpcRequest::Shutdown {
            auth_token: "t".to_string(),
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
}
