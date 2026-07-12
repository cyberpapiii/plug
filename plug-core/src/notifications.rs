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

// `Stdio` = an in-process stdio client served directly by a `ProxyHandler`/
// `StdioBridge`. `Ipc` = a daemon IPC client served over the Unix socket
// (`plug connect`) — a first-class identity rather than masquerading as `Stdio`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NotificationTarget {
    Stdio { client_id: Arc<str> },
    Http { session_id: Arc<str> },
    Ipc { client_id: Arc<str> },
}

impl ProtocolNotification {
    pub fn control_lagged_logging_params(
        skipped: u64,
        transport: &'static str,
    ) -> LoggingMessageNotificationParam {
        LoggingMessageNotificationParam {
            level: rmcp::model::LoggingLevel::Warning,
            logger: Some("plug.control".into()),
            data: serde_json::json!({
                "event": "control_notification_lagged",
                "transport": transport,
                "skipped": skipped,
            }),
        }
    }

    pub fn as_logging_message_params(&self) -> Option<LoggingMessageNotificationParam> {
        match self {
            ProtocolNotification::LoggingMessage { params } => Some(params.clone()),
            ProtocolNotification::TokenRefreshExchanged { server_id } => {
                Some(LoggingMessageNotificationParam {
                    level: rmcp::model::LoggingLevel::Info,
                    logger: Some("plug.auth".into()),
                    data: serde_json::json!({
                        "event": "token_refresh_exchanged",
                        "server_id": server_id,
                    }),
                })
            }
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

/// Transport-independent notification fan-out decision logic.
///
/// By the time a `ProtocolNotification` reaches a downstream transport's
/// fan-out loop (via [`crate::proxy::ToolRouter::subscribe_notifications`] /
/// `subscribe_logging`), target resolution against the progress-token and
/// subscription registries has ALREADY happened at publish time (see
/// `ToolRouter::publish_protocol_notification` and its callers). What
/// remains — and what was previously re-implemented three times, once per
/// transport (stdio in `plug-core/src/proxy/handler.rs`, HTTP in
/// `plug-core/src/http/server.rs`, daemon IPC in `plug/src/daemon.rs`) — is
/// strictly simpler: decide whether a given notification is meant for
/// *every* connected client on a transport, or only for the single client
/// identified by an embedded [`NotificationTarget`].
///
/// This module intentionally stays free of any transport type (no axum, no
/// SSE, no IPC frames, no stdio `Peer`) so both `plug-core` (stdio, HTTP) and
/// `plug` (daemon IPC) can share it. Each transport keeps its own delivery
/// code (which wire method to call, how to serialize the payload) — see the
/// "Maintenance notes" in `plans/015-notification-fanout-dedup-claude-fable.md`.
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
        /// `LoggingMessage` / `TokenRefreshExchanged` — broadcast; today's
        /// stdio and HTTP sites render both via `as_logging_message_params()`.
        Logging,
        /// `AuthStateChanged` — broadcast, but NOT necessarily rendered the
        /// same way as `Logging` on every transport (daemon IPC renders it
        /// as a native structured push instead of flattening to a logging
        /// message — see the plan's difference table). Kept as its own
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
        /// `NotificationTarget` variant (`Stdio`/`Http`/`Ipc`) AND the id
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

        fn stdio(id: &str) -> NotificationTarget {
            NotificationTarget::Stdio {
                client_id: Arc::from(id),
            }
        }

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
            rmcp::model::CancelledNotificationParam {
                request_id: rmcp::model::RequestId::Number(1),
                reason: None,
            }
        }

        fn resource_updated_params() -> rmcp::model::ResourceUpdatedNotificationParam {
            rmcp::model::ResourceUpdatedNotificationParam::new("file:///a".to_string())
        }

        // ── classify: one test per NotificationClass arm ───────────────

        #[test]
        fn classify_tool_list_changed_is_broadcast_shaped() {
            assert_eq!(
                classify(&ProtocolNotification::ToolListChanged),
                NotificationClass::ToolListChanged
            );
        }

        #[test]
        fn classify_resource_list_changed_is_broadcast_shaped() {
            assert_eq!(
                classify(&ProtocolNotification::ResourceListChanged),
                NotificationClass::ResourceListChanged
            );
        }

        #[test]
        fn classify_prompt_list_changed_is_broadcast_shaped() {
            assert_eq!(
                classify(&ProtocolNotification::PromptListChanged),
                NotificationClass::PromptListChanged
            );
        }

        #[test]
        fn classify_tool_list_changed_for_is_targeted() {
            let target = stdio("client-1");
            let notification = ProtocolNotification::ToolListChangedFor {
                target: target.clone(),
            };
            assert_eq!(
                classify(&notification),
                NotificationClass::ToolListChangedFor(&target)
            );
        }

        #[test]
        fn classify_progress_is_targeted() {
            let target = http("session-1");
            let notification = ProtocolNotification::Progress {
                target: target.clone(),
                params: progress_params(),
            };
            assert_eq!(
                classify(&notification),
                NotificationClass::Progress(&target)
            );
        }

        #[test]
        fn classify_cancelled_is_targeted() {
            let target = ipc("client-2");
            let notification = ProtocolNotification::Cancelled {
                target: target.clone(),
                params: cancelled_params(),
            };
            assert_eq!(
                classify(&notification),
                NotificationClass::Cancelled(&target)
            );
        }

        #[test]
        fn classify_resource_updated_is_targeted() {
            let target = stdio("client-3");
            let notification = ProtocolNotification::ResourceUpdated {
                target: target.clone(),
                params: resource_updated_params(),
            };
            assert_eq!(
                classify(&notification),
                NotificationClass::ResourceUpdated(&target)
            );
        }

        #[test]
        fn classify_logging_message_is_logging() {
            assert_eq!(
                classify(&ProtocolNotification::LoggingMessage {
                    params: rmcp::model::LoggingMessageNotificationParam {
                        level: rmcp::model::LoggingLevel::Info,
                        logger: None,
                        data: serde_json::json!("hi"),
                    },
                }),
                NotificationClass::Logging
            );
        }

        #[test]
        fn classify_token_refresh_exchanged_is_logging() {
            assert_eq!(
                classify(&ProtocolNotification::TokenRefreshExchanged {
                    server_id: Arc::from("github"),
                }),
                NotificationClass::Logging
            );
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

        // ── resolve: one test per ResolvedDelivery decision ─────────────

        #[test]
        fn resolve_broadcast_shaped_classes_resolve_to_broadcast() {
            assert_eq!(
                resolve(NotificationClass::ToolListChanged),
                ResolvedDelivery::Broadcast
            );
            assert_eq!(
                resolve(NotificationClass::ResourceListChanged),
                ResolvedDelivery::Broadcast
            );
            assert_eq!(
                resolve(NotificationClass::PromptListChanged),
                ResolvedDelivery::Broadcast
            );
            assert_eq!(
                resolve(NotificationClass::Logging),
                ResolvedDelivery::Broadcast
            );
            assert_eq!(
                resolve(NotificationClass::AuthState),
                ResolvedDelivery::Broadcast
            );
        }

        #[test]
        fn resolve_targeted_classes_resolve_to_target() {
            let target = stdio("client-1");
            assert_eq!(
                resolve(NotificationClass::ToolListChangedFor(&target)),
                ResolvedDelivery::ToTarget(&target)
            );
            assert_eq!(
                resolve(NotificationClass::Progress(&target)),
                ResolvedDelivery::ToTarget(&target)
            );
            assert_eq!(
                resolve(NotificationClass::Cancelled(&target)),
                ResolvedDelivery::ToTarget(&target)
            );
            assert_eq!(
                resolve(NotificationClass::ResourceUpdated(&target)),
                ResolvedDelivery::ToTarget(&target)
            );
        }

        // ── deliver_to: matching identity, mismatched id, mismatched
        //    variant (the "unknown target" case — every site today drops) ──

        #[test]
        fn deliver_to_broadcast_is_always_true() {
            assert!(ResolvedDelivery::Broadcast.deliver_to(&stdio("anyone")));
            assert!(ResolvedDelivery::Broadcast.deliver_to(&http("anyone")));
            assert!(ResolvedDelivery::Broadcast.deliver_to(&ipc("anyone")));
        }

        #[test]
        fn deliver_to_target_matching_identity_is_true() {
            let target = stdio("client-1");
            assert!(ResolvedDelivery::ToTarget(&target).deliver_to(&stdio("client-1")));
        }

        #[test]
        fn deliver_to_target_mismatched_id_same_variant_is_false() {
            let target = stdio("client-1");
            assert!(!ResolvedDelivery::ToTarget(&target).deliver_to(&stdio("client-2")));
        }

        #[test]
        fn deliver_to_target_mismatched_variant_same_id_is_false() {
            // Same string id, different transport variant — must not match.
            // This is the "progress token resolved to a target on a
            // different transport" case; every site today silently drops it.
            let target = stdio("shared-id");
            assert!(!ResolvedDelivery::ToTarget(&target).deliver_to(&http("shared-id")));
            assert!(!ResolvedDelivery::ToTarget(&target).deliver_to(&ipc("shared-id")));
        }

        #[test]
        fn deliver_to_target_http_matches_by_session_id() {
            let target = http("session-1");
            assert!(ResolvedDelivery::ToTarget(&target).deliver_to(&http("session-1")));
            assert!(!ResolvedDelivery::ToTarget(&target).deliver_to(&http("session-2")));
        }

        #[test]
        fn deliver_to_target_ipc_matches_by_client_id() {
            let target = ipc("ipc-client-1");
            assert!(ResolvedDelivery::ToTarget(&target).deliver_to(&ipc("ipc-client-1")));
            assert!(!ResolvedDelivery::ToTarget(&target).deliver_to(&ipc("ipc-client-2")));
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
