use std::sync::Arc;

use rmcp::model::{
    CancelledNotification, CancelledNotificationParam, LoggingMessageNotification,
    LoggingMessageNotificationParam, ProgressNotification, ProgressNotificationParam,
    PromptListChangedNotification, ResourceListChangedNotification, ResourceUpdatedNotification,
    ResourceUpdatedNotificationParam, ServerJsonRpcMessage, ServerNotification,
    ToolListChangedNotification,
};

/// Internal protocol notifications used for downstream transport fan-out.
#[derive(Clone, Debug, PartialEq)]
pub enum ProtocolNotification {
    ToolListChanged,
    ToolListChangedFor {
        target: NotificationTarget,
    },
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
    TokenRefreshExchanged {
        server_id: Arc<str>,
    },
    AuthStateChanged {
        server_id: Arc<str>,
        new_state: crate::types::ServerHealth,
    },
}

/// Which of the lists every client is served may have changed: tools,
/// resources (with resource templates) and prompts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListsChanged {
    pub tools: bool,
    pub resources: bool,
    pub prompts: bool,
}

impl ListsChanged {
    /// All three, for a client that may have missed word of a change.
    pub const ALL: Self = Self {
        tools: true,
        resources: true,
        prompts: true,
    };

    /// The `list_changed` notifications that say so.
    pub fn notifications(self) -> impl Iterator<Item = ProtocolNotification> {
        [
            (self.tools, ProtocolNotification::ToolListChanged),
            (self.resources, ProtocolNotification::ResourceListChanged),
            (self.prompts, ProtocolNotification::PromptListChanged),
        ]
        .into_iter()
        .filter_map(|(changed, notification)| changed.then_some(notification))
    }
}

/// One downstream connection: a legacy HTTP session, or a `plug connect`
/// session on the daemon socket.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NotificationTarget {
    Http { session_id: Arc<str> },
    Ipc { client_id: Arc<str> },
}

impl ProtocolNotification {
    /// The one client this notification is for, or `None` when it is for
    /// every client on a transport. Targets are resolved when a notification
    /// is published, so a fan-out loop only compares them.
    pub fn target(&self) -> Option<&NotificationTarget> {
        match self {
            Self::ToolListChangedFor { target }
            | Self::Progress { target, .. }
            | Self::Cancelled { target, .. }
            | Self::ResourceUpdated { target, .. } => Some(target),
            Self::ToolListChanged
            | Self::ResourceListChanged
            | Self::PromptListChanged
            | Self::LoggingMessage { .. }
            | Self::TokenRefreshExchanged { .. }
            | Self::AuthStateChanged { .. } => None,
        }
    }

    /// Whether the client `identity` names should receive it: any client
    /// for a broadcast, else only the one it names, on the same transport.
    pub fn is_for(&self, identity: &NotificationTarget) -> bool {
        self.target().is_none_or(|target| target == identity)
    }

    /// The log line a client gets in place of `skipped` log messages its
    /// connection fell too far behind to receive.
    pub fn lagged_logging_params(skipped: u64) -> LoggingMessageNotificationParam {
        LoggingMessageNotificationParam::new(
            rmcp::model::LoggingLevel::Warning,
            serde_json::json!(format!("skipped {skipped} log messages")),
        )
        .with_logger("plug")
    }

    pub fn control_lagged_logging_params(
        skipped: u64,
        transport: &'static str,
    ) -> LoggingMessageNotificationParam {
        LoggingMessageNotificationParam::new(
            rmcp::model::LoggingLevel::Warning,
            serde_json::json!({
                "event": "control_notification_lagged",
                "transport": transport,
                "skipped": skipped,
            }),
        )
        .with_logger("plug.control")
    }

    pub fn as_logging_message_params(&self) -> Option<LoggingMessageNotificationParam> {
        match self {
            ProtocolNotification::LoggingMessage { params } => Some(params.clone()),
            ProtocolNotification::TokenRefreshExchanged { server_id } => Some(
                LoggingMessageNotificationParam::new(
                    rmcp::model::LoggingLevel::Info,
                    serde_json::json!({
                        "event": "token_refresh_exchanged",
                        "server_id": server_id,
                    }),
                )
                .with_logger("plug.auth"),
            ),
            ProtocolNotification::AuthStateChanged {
                server_id,
                new_state,
            } => Some(
                LoggingMessageNotificationParam::new(
                    match new_state {
                        crate::types::ServerHealth::AuthRequired => {
                            rmcp::model::LoggingLevel::Warning
                        }
                        _ => rmcp::model::LoggingLevel::Info,
                    },
                    serde_json::json!({
                        "event": "auth_state_changed",
                        "server_id": server_id,
                        "new_state": new_state,
                    }),
                )
                .with_logger("plug.auth"),
            ),
            _ => None,
        }
    }

    /// Convert the internal notification to a server-to-client JSON-RPC message.
    pub fn to_server_jsonrpc_message(&self) -> ServerJsonRpcMessage {
        match self {
            ProtocolNotification::ToolListChanged
            | ProtocolNotification::ToolListChangedFor { .. } => {
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
            | ProtocolNotification::TokenRefreshExchanged { .. }
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

    fn http(id: &str) -> NotificationTarget {
        NotificationTarget::Http {
            session_id: Arc::from(id),
        }
    }

    fn ipc(id: &str) -> NotificationTarget {
        NotificationTarget::Ipc {
            client_id: Arc::from(id),
        }
    }

    /// A notification naming a client reaches only that client, on its own
    /// transport. Any other reaches every client.
    #[test]
    fn a_notification_reaches_the_client_it_names_or_everyone() {
        let named = ipc("client-1");
        let others = [ipc("client-2"), http("client-1")];
        let targeted = [
            ProtocolNotification::ToolListChangedFor {
                target: named.clone(),
            },
            ProtocolNotification::Progress {
                target: named.clone(),
                params: rmcp::model::ProgressNotificationParam::new(
                    rmcp::model::ProgressToken(rmcp::model::NumberOrString::String(Arc::from(
                        "tok",
                    ))),
                    0.5,
                ),
            },
            ProtocolNotification::Cancelled {
                target: named.clone(),
                params: CancelledNotificationParam::new(
                    Some(rmcp::model::RequestId::Number(1)),
                    None,
                ),
            },
            ProtocolNotification::ResourceUpdated {
                target: named.clone(),
                params: ResourceUpdatedNotificationParam::new("file:///a".to_string()),
            },
        ];
        for notification in &targeted {
            assert!(notification.is_for(&named), "{notification:?}");
            for other in &others {
                assert!(!notification.is_for(other), "{notification:?}");
            }
        }
        let broadcast = [
            ProtocolNotification::ToolListChanged,
            ProtocolNotification::ResourceListChanged,
            ProtocolNotification::PromptListChanged,
            ProtocolNotification::LoggingMessage {
                params: LoggingMessageNotificationParam::new(
                    rmcp::model::LoggingLevel::Info,
                    serde_json::json!("hi"),
                ),
            },
            ProtocolNotification::TokenRefreshExchanged {
                server_id: Arc::from("github"),
            },
            ProtocolNotification::AuthStateChanged {
                server_id: Arc::from("github"),
                new_state: crate::types::ServerHealth::AuthRequired,
            },
        ];
        for notification in &broadcast {
            for client in others.iter().chain([&named]) {
                assert!(notification.is_for(client), "{notification:?}");
            }
        }
    }

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

    #[test]
    fn token_refresh_exchanged_serializes_as_structured_logging_message() {
        let value = ProtocolNotification::TokenRefreshExchanged {
            server_id: Arc::from("github"),
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
            Some("token_refresh_exchanged")
        );
        assert_eq!(
            value
                .get("params")
                .and_then(|v| v.get("data"))
                .and_then(|v| v.get("server_id"))
                .and_then(|v| v.as_str()),
            Some("github")
        );
    }

    #[test]
    fn control_lagged_logging_params_are_structured() {
        let params = ProtocolNotification::control_lagged_logging_params(7, "stdio");
        assert_eq!(params.logger.as_deref(), Some("plug.control"));
        assert_eq!(params.level, rmcp::model::LoggingLevel::Warning);
        assert_eq!(params.data["event"], "control_notification_lagged");
        assert_eq!(params.data["transport"], "stdio");
        assert_eq!(params.data["skipped"], 7);
    }
}
