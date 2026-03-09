use std::sync::Arc;

use rmcp::model::{
    CancelledNotification, CancelledNotificationParam, LoggingMessageNotification,
    LoggingMessageNotificationParam, ProgressNotification, ProgressNotificationParam,
    PromptListChangedNotification, ResourceListChangedNotification, ResourceUpdatedNotification,
    ResourceUpdatedNotificationParam, ServerJsonRpcMessage, ServerNotification,
    ToolListChangedNotification,
};

/// Internal protocol notifications used for downstream transport fan-out.
///
/// This is intentionally separate from `EngineEvent`, which remains focused on
/// observability and UI/daemon consumers rather than wire-level MCP messages.
#[derive(Clone, Debug, PartialEq)]
pub enum ProtocolNotification {
    ToolListChanged,
    ResourceListChanged,
    PromptListChanged,
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
    LoggingMessage {
        params: LoggingMessageNotificationParam,
    },
    AuthStateChanged {
        server_id: Arc<str>,
        new_state: crate::types::ServerHealth,
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
            ProtocolNotification::ResourceListChanged => ServerJsonRpcMessage::notification(
                ServerNotification::ResourceListChangedNotification(
                    ResourceListChangedNotification::default(),
                ),
            ),
            ProtocolNotification::PromptListChanged => ServerJsonRpcMessage::notification(
                ServerNotification::PromptListChangedNotification(
                    PromptListChangedNotification::default(),
                ),
            ),
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
            ProtocolNotification::LoggingMessage { params } => {
                ServerJsonRpcMessage::notification(ServerNotification::LoggingMessageNotification(
                    LoggingMessageNotification::new(params.clone()),
                ))
            }
            ProtocolNotification::AuthStateChanged { .. } => {
                // AuthStateChanged is a plug-internal notification only delivered
                // over IPC push; it has no MCP wire equivalent. Emit a synthetic
                // logging message for any code path that calls this generically.
                ServerJsonRpcMessage::notification(ServerNotification::LoggingMessageNotification(
                    LoggingMessageNotification::new(LoggingMessageNotificationParam {
                        level: rmcp::model::LoggingLevel::Warning,
                        logger: Some("plug".into()),
                        data: serde_json::json!("auth state changed (internal)"),
                    }),
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
