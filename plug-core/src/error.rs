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
            ProtocolError::ToolNotFound { .. } => -32601,
            ProtocolError::ServerUnavailable { .. } => -32603,
            ProtocolError::ServerBusy { .. } => -32603,
            ProtocolError::Timeout { .. } => -32603,
            ProtocolError::InvalidRequest { .. } => -32600,
        }
    }
}

impl From<ProtocolError> for rmcp::ErrorData {
    fn from(err: ProtocolError) -> Self {
        let code = rmcp::model::ErrorCode(err.code() as i32);
        rmcp::ErrorData::new(code, err.to_string(), None)
    }
}
