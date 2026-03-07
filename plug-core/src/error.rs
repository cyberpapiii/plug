use std::time::Duration;

/// Errors returned as JSON-RPC error responses to clients.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProtocolError {
    #[error("tool not found: {tool_name}")]
    ToolNotFound { tool_name: String },

    #[error("server unavailable: {server_id}")]
    ServerUnavailable { server_id: String },

    #[error("server overloaded while waiting for capacity: {server_id}")]
    ServerBusy { server_id: String },

    #[error("request timed out after {duration:?}")]
    Timeout { duration: Duration },

    #[error("invalid request: {detail}")]
    InvalidRequest { detail: String },
}

impl ProtocolError {
    /// Returns the JSON-RPC error code for this error.
    pub fn code(&self) -> i64 {
        match self {
            ProtocolError::ToolNotFound { .. } => -32601, // Method not found
            ProtocolError::ServerUnavailable { .. } => -32603, // Internal error
            ProtocolError::ServerBusy { .. } => -32603,   // Internal error
            ProtocolError::Timeout { .. } => -32603,      // Internal error
            ProtocolError::InvalidRequest { .. } => -32600, // Invalid request
        }
    }

    /// Creates a JSON-RPC error object from this error.
    pub fn to_json_rpc_error(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code(),
            "message": self.to_string(),
            "data": null
        })
    }
}

impl From<ProtocolError> for rmcp::ErrorData {
    fn from(err: ProtocolError) -> Self {
        let code = rmcp::model::ErrorCode(err.code() as i32);
        rmcp::ErrorData::new(code, err.to_string(), None)
    }
}

/// Internal operational errors. Logged but not returned to clients.
#[derive(Debug, thiserror::Error)]
pub enum InternalError {
    #[error("config parse error at {path}: {detail}")]
    ConfigParseError {
        path: std::path::PathBuf,
        detail: String,
    },

    #[error("failed to start server {server_id}: {reason}")]
    ServerStartFailed { server_id: String, reason: String },

    #[error("transport error ({context}): {source}")]
    TransportError {
        context: String,
        #[source]
        source: anyhow::Error,
    },
}
