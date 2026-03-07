use rmcp::model::{ServerJsonRpcMessage, ServerNotification, ToolListChangedNotification};

/// Internal protocol notifications used for downstream transport fan-out.
///
/// This is intentionally separate from `EngineEvent`, which remains focused on
/// observability and UI/daemon consumers rather than wire-level MCP messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolNotification {
    ToolListChanged,
}

impl ProtocolNotification {
    /// Convert the internal notification to a server-to-client JSON-RPC message.
    pub fn to_server_jsonrpc_message(&self) -> ServerJsonRpcMessage {
        match self {
            ProtocolNotification::ToolListChanged => ServerJsonRpcMessage::notification(
                ServerNotification::ToolListChangedNotification(ToolListChangedNotification::default()),
            ),
        }
    }

    /// Convert the internal notification to a JSON value suitable for SSE.
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self.to_server_jsonrpc_message())
            .expect("protocol notification should always serialize")
    }
}
