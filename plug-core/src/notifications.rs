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
    pub fn as_logging_message_params(&self) -> Option<LoggingMessageNotificationParam> {
        match self {
            ProtocolNotification::LoggingMessage { params } => Some(params.clone()),
            ProtocolNotification::AuthStateChanged {
                server_id,
                new_state,
            } => Some(LoggingMessageNotificationParam {
                level: match new_state {
                    crate::types::ServerHealth::AuthRequired => rmcp::model::LoggingLevel::Warning,
                    _ => rmcp::model::LoggingLevel::Info,
                },
                logger: Some("plug.auth".into()),
                data: serde_json::json!({
                    "event": "auth_state_changed",
                    "server_id": server_id,
                    "new_state": new_state,
                }),
            }),
            _ => None,
        }
    }

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
            ProtocolNotification::LoggingMessage { .. }
            | ProtocolNotification::AuthStateChanged { .. } => ServerJsonRpcMessage::notification(
                ServerNotification::LoggingMessageNotification(LoggingMessageNotification::new(
                    self.as_logging_message_params()
                        .expect("logging message params"),
                )),
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_state_changed_serializes_as_structured_logging_message() {
        let value = ProtocolNotification::AuthStateChanged {
            server_id: Arc::from("github"),
            new_state: crate::types::ServerHealth::AuthRequired,
        }
        .to_json_value();

        assert_eq!(
            value.get("method").and_then(|v| v.as_str()),
            Some("notifications/message")
        );
        assert_eq!(
            value
                .get("params")
                .and_then(|v| v.get("logger"))
                .and_then(|v| v.as_str()),
            Some("plug.auth")
        );
        assert_eq!(
            value
                .get("params")
                .and_then(|v| v.get("data"))
                .and_then(|v| v.get("event"))
                .and_then(|v| v.as_str()),
            Some("auth_state_changed")
        );
        assert_eq!(
            value
                .get("params")
                .and_then(|v| v.get("data"))
                .and_then(|v| v.get("server_id"))
                .and_then(|v| v.as_str()),
            Some("github")
        );
        assert_eq!(
            value
                .get("params")
                .and_then(|v| v.get("data"))
                .and_then(|v| v.get("new_state"))
                .and_then(|v| v.as_str()),
            Some("AuthRequired")
        );
    }
}
