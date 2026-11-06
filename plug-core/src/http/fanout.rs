use super::server::*;
use crate::notifications::{NotificationTarget, ProtocolNotification};
use crate::session::SseMessage;
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl HttpState {
    pub fn spawn_notification_fanout(self: &Arc<Self>) {
        if self
            .notification_task_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let state = Arc::clone(self);
        let mut rx = state.router.subscribe_notifications();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = state.cancel.cancelled() => break,
                    recv = rx.recv() => {
                        match recv {
                            Ok(notification) => {
                                // Unlike the daemon (one fan-out task per client, comparing
                                // target to "self"), this task is shared by every HTTP session,
                                // so a targeted resolution names which session to route to
                                // rather than answering "is this me?". See
                                // plug-core/src/notifications.rs::fanout.
                                use crate::notifications::fanout::{
                                    NotificationClass, ResolvedDelivery, classify, resolve,
                                };
                                let class = classify(&notification);
                                // Targeted classes carry no broadcast audience; the gate that
                                // admitted the originating request already authorized the reply.
                                let kind = match class {
                                    NotificationClass::ToolListChanged => {
                                        Some(crate::session::BroadcastKind::ToolList)
                                    }
                                    NotificationClass::ResourceListChanged => {
                                        Some(crate::session::BroadcastKind::ResourceList)
                                    }
                                    NotificationClass::PromptListChanged => {
                                        Some(crate::session::BroadcastKind::PromptList)
                                    }
                                    NotificationClass::Logging | NotificationClass::AuthState => {
                                        Some(crate::session::BroadcastKind::Logging)
                                    }
                                    NotificationClass::ToolListChangedFor(_)
                                    | NotificationClass::Progress(_)
                                    | NotificationClass::Cancelled(_)
                                    | NotificationClass::ResourceUpdated(_) => None,
                                };
                                match resolve(class) {
                                    ResolvedDelivery::ToTarget(NotificationTarget::Http {
                                        session_id,
                                    }) => {
                                        if let Some(message) =
                                            notification_to_sse_message(&notification)
                                        {
                                            state.sessions.send_to_live_session(session_id, message);
                                        }
                                    }
                                    ResolvedDelivery::ToTarget(_) => {}
                                    ResolvedDelivery::Broadcast => {
                                        if let Some(kind) = kind
                                            && let Some(message) =
                                                notification_to_sse_message(&notification)
                                        {
                                            let router = &state.router;
                                            state.sessions.broadcast_where(message, kind, &|grant| {
                                                may_be_told(router, grant, &notification)
                                            });
                                        }
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "HTTP notification fan-out lagged");
                                if let Some(message) = notification_to_sse_message(&
                                    ProtocolNotification::LoggingMessage {
                                        params: ProtocolNotification::control_lagged_logging_params(
                                            skipped,
                                            "http",
                                        ),
                                    },
                                ) {
                                    state.sessions.broadcast(
                                        message,
                                        crate::session::BroadcastKind::Logging,
                                    );
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });

        // Separate logging fan-out task (isolated from control notifications)
        let log_state = Arc::clone(self);
        let mut log_rx = log_state.router.subscribe_logging();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = log_state.cancel.cancelled() => break,
                    recv = log_rx.recv() => {
                        match recv {
                            Ok(notif @ ProtocolNotification::LoggingMessage { .. }) => {
                                if let Some(message) = notification_to_sse_message(&notif) {
                                    // A log line does not say which server
                                    // wrote it, so a client kept from any
                                    // server gets none. Asked per line: a
                                    // block added to an open stream applies
                                    // to the next one.
                                    let router = &log_state.router;
                                    log_state.sessions.broadcast_where(
                                        message,
                                        crate::session::BroadcastKind::Logging,
                                        &|grant| may_be_told(router, grant, &notif),
                                    );
                                }
                            }
                            Ok(_) => {} // non-logging notifications on wrong channel
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "HTTP logging fan-out lagged");
                                // Emit synthetic warning to all connected clients
                                let synthetic = ProtocolNotification::LoggingMessage {
                                    params: rmcp::model::LoggingMessageNotificationParam::new(
                                        rmcp::model::LoggingLevel::Warning,
                                        serde_json::json!(format!(
                                            "skipped {skipped} log messages"
                                        )),
                                    )
                                    .with_logger("plug"),
                                };
                                if let Some(message) = notification_to_sse_message(&synthetic) {
                                    log_state.sessions.broadcast(
                                        message,
                                        crate::session::BroadcastKind::Logging,
                                    );
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
    }
}

pub(super) fn notification_to_sse_message(
    notification: &ProtocolNotification,
) -> Option<SseMessage> {
    SseMessage::from_json_value(notification.to_json_value())
        .map_err(|error| {
            tracing::error!(%error, "failed to serialize SSE notification payload");
        })
        .ok()
}

/// Whether a session opened under `grant` may be sent `notification`. See
/// [`crate::proxy::ToolRouter::client_may_be_told`].
pub(super) fn may_be_told(
    router: &crate::proxy::ToolRouter,
    grant: Option<&str>,
    notification: &ProtocolNotification,
) -> bool {
    let key = crate::downstream_identity::remote_access_key(grant);
    router.client_may_be_told(Some(&key), notification)
}

pub(super) fn drain_targeted_notifications(
    receiver: &mut tokio::sync::broadcast::Receiver<ProtocolNotification>,
    target: &NotificationTarget,
) -> Vec<ProtocolNotification> {
    let mut notifications = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(notification) => {
                let addressed_here = matches!(
                    crate::notifications::fanout::resolve(
                        crate::notifications::fanout::classify(&notification)
                    ),
                    crate::notifications::fanout::ResolvedDelivery::ToTarget(candidate)
                        if candidate == target
                );
                if addressed_here {
                    notifications.push(notification);
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
        }
    }
    notifications
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_support::*;

    use crate::test_helpers::collect_sse_events;
    use axum::body::Body;
    use http::Request as HttpRequest;

    use tower::ServiceExt;

    fn sse_event_id(event: &str) -> Option<u64> {
        event.lines().find_map(|line| {
            line.strip_prefix("id: ")
                .and_then(|value| value.trim().parse::<u64>().ok())
        })
    }

    #[tokio::test]
    async fn word_about_a_server_goes_only_to_clients_allowed_that_server() {
        let state = test_state();
        let mut clients = std::collections::BTreeMap::new();
        clients.insert(
            crate::downstream_identity::grant_client_key("watcher"),
            crate::config::ClientSettings {
                blocked_servers: vec!["git".to_string()],
                ..Default::default()
            },
        );
        state.router.set_client_access(&clients);
        let told = |grant, notification| may_be_told(&state.router, Some(grant), &notification);
        let sign_in = |server: &str| ProtocolNotification::AuthStateChanged {
            server_id: server.into(),
            new_state: crate::types::ServerHealth::AuthRequired,
        };
        let refreshed = |server: &str| ProtocolNotification::TokenRefreshExchanged {
            server_id: server.into(),
        };
        let log = || ProtocolNotification::LoggingMessage {
            params: rmcp::model::LoggingMessageNotificationParam::new(
                rmcp::model::LoggingLevel::Info,
                serde_json::json!("line"),
            ),
        };
        assert!(!told("watcher", sign_in("git")));
        assert!(!told("watcher", refreshed("git")));
        assert!(told("watcher", sign_in("mail")));
        assert!(!told("watcher", log()), "a log line names no server");
        assert!(told("watcher", ProtocolNotification::ToolListChanged));
        for notification in [sign_in("git"), refreshed("git"), log()] {
            assert!(told("someone-else", notification));
        }
        // The block is lifted while streams are open: the next one arrives.
        state.router.set_client_access(&Default::default());
        assert!(told("watcher", sign_in("git")));
    }

    #[tokio::test]
    async fn tools_list_changed_reaches_http_sse_client() {
        let state = test_state();
        let app = build_router(state.clone());

        let init_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0"
                }
            }
        });

        let init_req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&init_body).unwrap()))
            .unwrap();

        let init_resp = app.clone().oneshot(init_req).await.unwrap();
        let session_id = init_resp
            .headers()
            .get(SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("session id header")
            .to_string();

        let sse_req = HttpRequest::builder()
            .method("GET")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, session_id)
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .unwrap();

        let sse_resp = app.oneshot(sse_req).await.unwrap();
        assert_eq!(sse_resp.status(), StatusCode::OK);
        let body = sse_resp.into_body();

        state.router.publish_protocol_notification(
            crate::notifications::ProtocolNotification::ToolListChanged,
        );

        let events = collect_sse_events(body, 3).await;
        assert!(
            events
                .iter()
                .any(|event| event.contains("notifications/tools/list_changed")),
            "expected SSE stream to contain tools/list_changed notification, got {events:?}"
        );
    }

    #[tokio::test]
    async fn last_event_id_replays_missed_http_sse_notifications() {
        let state = test_state();
        let app = build_router(state.clone());

        let session_id = state.sessions.create_session().unwrap();
        // This test drives the SSE stream without going through initialize, so
        // it has to record the audience initialize would have resolved for an
        // unauthenticated loopback listener.
        state
            .sessions
            .set_broadcast_audience(
                &session_id,
                crate::session::BroadcastAudience::unrestricted(),
            )
            .unwrap();
        let sse_req = HttpRequest::builder()
            .method("GET")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .unwrap();
        let sse_resp = app.clone().oneshot(sse_req).await.unwrap();
        assert_eq!(sse_resp.status(), StatusCode::OK);
        let body = sse_resp.into_body();

        state.router.publish_protocol_notification(
            crate::notifications::ProtocolNotification::ToolListChanged,
        );
        state.router.publish_protocol_notification(
            crate::notifications::ProtocolNotification::ResourceListChanged,
        );

        let events = collect_sse_events(body, 3).await;
        let tool_event = events
            .iter()
            .find(|event| event.contains("notifications/tools/list_changed"))
            .expect("tool list changed event");
        let last_seen_id = sse_event_id(tool_event).expect("event id");

        let replay_req = HttpRequest::builder()
            .method("GET")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .header("accept", "text/event-stream")
            .header("last-event-id", last_seen_id.to_string())
            .body(Body::empty())
            .unwrap();
        let replay_resp = app.oneshot(replay_req).await.unwrap();
        assert_eq!(replay_resp.status(), StatusCode::OK);

        let replayed = collect_sse_events(replay_resp.into_body(), 2).await;
        assert!(
            replayed
                .iter()
                .any(|event| event.contains("notifications/resources/list_changed")),
            "expected replayed resource list_changed after id {last_seen_id}, got {replayed:?}"
        );
    }

    #[tokio::test]
    async fn targeted_progress_reaches_http_sse_session() {
        let state = test_state();
        let app = build_router(state.clone());

        let init_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0"
                }
            }
        });

        let init_req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&init_body).unwrap()))
            .unwrap();

        let init_resp = app.clone().oneshot(init_req).await.unwrap();
        let session_id = init_resp
            .headers()
            .get(SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("session id header")
            .to_string();

        let sse_req = HttpRequest::builder()
            .method("GET")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .unwrap();

        let sse_resp = app.oneshot(sse_req).await.unwrap();
        assert_eq!(sse_resp.status(), StatusCode::OK);
        let body = sse_resp.into_body();

        state.router.publish_protocol_notification(
            crate::notifications::ProtocolNotification::Progress {
                target: crate::notifications::NotificationTarget::Http {
                    session_id: Arc::from(session_id),
                },
                params: ProgressNotificationParam::new(
                    ProgressToken(NumberOrString::String(Arc::from("http-progress"))),
                    0.5,
                )
                .with_message("halfway"),
            },
        );

        let events = collect_sse_events(body, 3).await;
        assert!(
            events.iter().any(|event| {
                event.contains("notifications/progress")
                    && event.contains("http-progress")
                    && event.contains("halfway")
            }),
            "expected SSE stream to contain targeted progress notification, got {events:?}"
        );
    }

    #[tokio::test]
    async fn auth_state_changed_reaches_http_sse_as_logging_message() {
        let state = test_state();
        let app = build_router(state.clone());

        let init_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0"
                }
            }
        });

        let init_req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&init_body).unwrap()))
            .unwrap();

        let init_resp = app.clone().oneshot(init_req).await.unwrap();
        let session_id = init_resp
            .headers()
            .get(SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("session id header")
            .to_string();

        let sse_req = HttpRequest::builder()
            .method("GET")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .unwrap();

        let sse_resp = app.oneshot(sse_req).await.unwrap();
        assert_eq!(sse_resp.status(), StatusCode::OK);
        let body = sse_resp.into_body();

        state.router.publish_protocol_notification(
            crate::notifications::ProtocolNotification::AuthStateChanged {
                server_id: Arc::from("github"),
                new_state: crate::types::ServerHealth::AuthRequired,
            },
        );

        let events = collect_sse_events(body, 3).await;
        assert!(
            events.iter().any(|event| {
                event.contains("notifications/message")
                    && event.contains("auth_state_changed")
                    && event.contains("github")
                    && event.contains("AuthRequired")
            }),
            "expected SSE stream to contain auth state logging notification, got {events:?}"
        );
    }

    #[tokio::test]
    async fn token_refresh_exchanged_reaches_http_sse_as_logging_message() {
        let state = test_state();
        let app = build_router(state.clone());

        let init_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0"
                }
            }
        });

        let init_req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&init_body).unwrap()))
            .unwrap();

        let init_resp = app.clone().oneshot(init_req).await.unwrap();
        let session_id = init_resp
            .headers()
            .get(SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("session id header")
            .to_string();

        let sse_req = HttpRequest::builder()
            .method("GET")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .unwrap();

        let sse_resp = app.oneshot(sse_req).await.unwrap();
        assert_eq!(sse_resp.status(), StatusCode::OK);
        let body = sse_resp.into_body();

        state.router.publish_protocol_notification(
            crate::notifications::ProtocolNotification::TokenRefreshExchanged {
                server_id: Arc::from("github"),
            },
        );

        let events = collect_sse_events(body, 3).await;
        assert!(
            events.iter().any(|event| {
                event.contains("notifications/message")
                    && event.contains("token_refresh_exchanged")
                    && event.contains("github")
            }),
            "expected SSE stream to contain token refresh logging notification, got {events:?}"
        );
    }
}
