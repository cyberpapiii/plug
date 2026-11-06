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

/// One downstream connection: a legacy HTTP session, or a `plug connect`
/// session on the daemon socket.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NotificationTarget {
    Http { session_id: Arc<str> },
    Ipc { client_id: Arc<str> },
}

impl ProtocolNotification {
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

/// Which connected clients a notification is for.
///
/// Targets are resolved when a notification is published, so a transport's
/// fan-out loop only has to tell a notification meant for every client on it
/// from one meant for the single client an embedded [`NotificationTarget`]
/// names. The module holds no transport type, so `plug-core` and `plug` share
/// it; each transport keeps its own delivery code.
pub mod fanout {
    use super::{NotificationTarget, ProtocolNotification};

    /// What a notification is, after classification — transport-independent.
    ///
    /// One arm per `ProtocolNotification` variant (rather than collapsing
    /// straight to broadcast/targeted) so `classify` stays self-documenting
    /// and each mapping is independently unit-testable.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NotificationClass<'a> {
        ToolListChanged,
        ToolListChangedFor(&'a NotificationTarget),
        ResourceListChanged,
        PromptListChanged,
        Progress(&'a NotificationTarget),
        Cancelled(&'a NotificationTarget),
        ResourceUpdated(&'a NotificationTarget),
        /// `LoggingMessage` / `TokenRefreshExchanged` — broadcast; HTTP renders
        /// both via `as_logging_message_params()`.
        Logging,
        /// `AuthStateChanged` — broadcast, but NOT necessarily rendered the
        /// same way as `Logging` on every transport (daemon IPC renders it
        /// as a native structured push instead of flattening to a logging
        /// message). Kept as its own
        /// class specifically so that per-transport divergence stays
        /// visible at the classify() call site rather than being silently
        /// merged with `Logging`.
        AuthState,
    }

    /// Classify a notification by its fan-out shape. Pure function, no I/O.
    pub fn classify(notification: &ProtocolNotification) -> NotificationClass<'_> {
        match notification {
            ProtocolNotification::ToolListChanged => NotificationClass::ToolListChanged,
            ProtocolNotification::ToolListChangedFor { target } => {
                NotificationClass::ToolListChangedFor(target)
            }
            ProtocolNotification::ResourceListChanged => NotificationClass::ResourceListChanged,
            ProtocolNotification::PromptListChanged => NotificationClass::PromptListChanged,
            ProtocolNotification::Progress { target, .. } => NotificationClass::Progress(target),
            ProtocolNotification::Cancelled { target, .. } => NotificationClass::Cancelled(target),
            ProtocolNotification::ResourceUpdated { target, .. } => {
                NotificationClass::ResourceUpdated(target)
            }
            ProtocolNotification::LoggingMessage { .. }
            | ProtocolNotification::TokenRefreshExchanged { .. } => NotificationClass::Logging,
            ProtocolNotification::AuthStateChanged { .. } => NotificationClass::AuthState,
        }
    }

    /// Who should receive it — resolved against the classification, still
    /// transport-independent. Delivery mechanics (which wire call to make)
    /// stay with each transport.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ResolvedDelivery<'a> {
        /// Every connected client on this transport should receive it, no
        /// target check needed.
        Broadcast,
        /// Only the client identified by this target should receive it.
        /// Callers use [`ResolvedDelivery::deliver_to`] to check whether a
        /// specific connection's identity matches, or (for transports that
        /// route dynamically by key, e.g. HTTP session lookup) inspect the
        /// target directly.
        ToTarget(&'a NotificationTarget),
    }

    impl<'a> ResolvedDelivery<'a> {
        /// Whether a client identified by `identity` should receive this
        /// delivery.
        ///
        /// `Broadcast` always returns `true`. `ToTarget` returns `true` only
        /// when the resolved target equals `identity` exactly — both the
        /// `NotificationTarget` variant (`Http`/`Ipc`) AND the id
        /// must match, matching every fan-out site's existing `matches!`
        /// check today. A target for a different transport, or a different
        /// client on the same transport, resolves to `false` (dropped) —
        /// this is the "unknown/mismatched target" default every site uses.
        pub fn deliver_to(&self, identity: &NotificationTarget) -> bool {
            match self {
                ResolvedDelivery::Broadcast => true,
                ResolvedDelivery::ToTarget(target) => *target == identity,
            }
        }
    }

    /// Resolve a classification into a delivery decision.
    pub fn resolve(class: NotificationClass<'_>) -> ResolvedDelivery<'_> {
        match class {
            NotificationClass::ToolListChanged
            | NotificationClass::ResourceListChanged
            | NotificationClass::PromptListChanged
            | NotificationClass::Logging
            | NotificationClass::AuthState => ResolvedDelivery::Broadcast,
            NotificationClass::ToolListChangedFor(target)
            | NotificationClass::Progress(target)
            | NotificationClass::Cancelled(target)
            | NotificationClass::ResourceUpdated(target) => ResolvedDelivery::ToTarget(target),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::Arc;

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

        fn progress_params() -> rmcp::model::ProgressNotificationParam {
            rmcp::model::ProgressNotificationParam::new(
                rmcp::model::ProgressToken(rmcp::model::NumberOrString::String(Arc::from("tok"))),
                0.5,
            )
        }

        fn cancelled_params() -> rmcp::model::CancelledNotificationParam {
            rmcp::model::CancelledNotificationParam::new(
                Some(rmcp::model::RequestId::Number(1)),
                None,
            )
        }

        fn resource_updated_params() -> rmcp::model::ResourceUpdatedNotificationParam {
            rmcp::model::ResourceUpdatedNotificationParam::new("file:///a".to_string())
        }

        /// A notification naming a client reaches only that client, on its
        /// own transport. Any other reaches every client.
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
                    params: progress_params(),
                },
                ProtocolNotification::Cancelled {
                    target: named.clone(),
                    params: cancelled_params(),
                },
                ProtocolNotification::ResourceUpdated {
                    target: named.clone(),
                    params: resource_updated_params(),
                },
            ];
            for notification in &targeted {
                let delivery = resolve(classify(notification));
                assert!(delivery.deliver_to(&named), "{notification:?}");
                for other in &others {
                    assert!(!delivery.deliver_to(other), "{notification:?}");
                }
            }
            let broadcast = [
                ProtocolNotification::ToolListChanged,
                ProtocolNotification::ResourceListChanged,
                ProtocolNotification::PromptListChanged,
                ProtocolNotification::LoggingMessage {
                    params: rmcp::model::LoggingMessageNotificationParam::new(
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
                let delivery = resolve(classify(notification));
                for client in others.iter().chain([&named]) {
                    assert!(delivery.deliver_to(client), "{notification:?}");
                }
            }
        }

        #[test]
        fn classify_auth_state_changed_is_its_own_class() {
            assert_eq!(
                classify(&ProtocolNotification::AuthStateChanged {
                    server_id: Arc::from("github"),
                    new_state: crate::types::ServerHealth::AuthRequired,
                }),
                NotificationClass::AuthState
            );
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
