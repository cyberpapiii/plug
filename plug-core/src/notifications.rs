use std::sync::Arc;

use rmcp::model::{
    CancelledNotification, CancelledNotificationParam, ProgressNotification,
    ProgressNotificationParam, ResourceUpdatedNotification, ResourceUpdatedNotificationParam,
    ServerJsonRpcMessage, ServerNotification, ToolListChangedNotification,
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
    ResourceUpdated {
        target: NotificationTarget,
        params: ResourceUpdatedNotificationParam,
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
            ProtocolNotification::ResourceUpdated { params, .. } => {
                ServerJsonRpcMessage::notification(ServerNotification::ResourceUpdatedNotification(
                    ResourceUpdatedNotification::new(params.clone()),
                ))
            }
        }
    }

    /// Convert the internal notification to a JSON value suitable for SSE.
    pub fn to_json_value(&self) -> serde_json::Value {
        match serde_json::to_value(self.to_server_jsonrpc_message()) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(error = %error, "failed to serialize protocol notification");
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/message",
                    "params": {
                        "level": "error",
                        "message": "failed to serialize protocol notification"
                    }
                })
            }
        }
    }
}
