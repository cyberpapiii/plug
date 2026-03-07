use std::sync::Arc;

use rmcp::model::{
    CancelledNotification, CancelledNotificationParam, ProgressNotification,
    ProgressNotificationParam, ServerJsonRpcMessage, ServerNotification,
    ToolListChangedNotification,
};

/// Internal protocol notifications used for downstream transport fan-out.
///
/// This is intentionally separate from `EngineEvent`, which remains focused on
/// observability and UI/daemon consumers rather than wire-level MCP messages.
#[derive(Clone, Debug, PartialEq)]
pub enum ProtocolNotification {
    ToolListChanged,
    Progress {
        target: NotificationTarget,
        params: ProgressNotificationParam,
    },
    Cancelled {
        target: NotificationTarget,
        params: CancelledNotificationParam,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NotificationTarget {
    Stdio { client_id: Arc<str> },
    Http { session_id: Arc<str> },
}

impl ProtocolNotification {
    /// Convert the internal notification to a server-to-client JSON-RPC message.
    pub fn to_server_jsonrpc_message(&self) -> ServerJsonRpcMessage {
        match self {
            ProtocolNotification::ToolListChanged => {
                ServerJsonRpcMessage::notification(ServerNotification::ToolListChangedNotification(
                    ToolListChangedNotification::default(),
                ))
            }
            ProtocolNotification::Progress { params, .. } => ServerJsonRpcMessage::notification(
                ServerNotification::ProgressNotification(ProgressNotification::new(params.clone())),
            ),
            ProtocolNotification::Cancelled { params, .. } => {
                ServerJsonRpcMessage::notification(ServerNotification::CancelledNotification(
                    CancelledNotification::new(params.clone()),
                ))
            }
        }
    }

    /// Convert the internal notification to a JSON value suitable for SSE.
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self.to_server_jsonrpc_message())
            .expect("protocol notification should always serialize")
    }
}
