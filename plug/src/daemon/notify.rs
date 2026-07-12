//! Notification delivery to IPC clients — control-channel fanout.

use plug_core::ipc::{self, IpcResponse};

/// Send a protocol (control) notification to the IPC client, handling broadcast errors.
///
/// Broadcast notifications (list_changed) are sent to all registered IPC clients.
/// Targeted notifications (progress, cancelled) are only sent if the target matches
/// this connection's session ID.
pub(super) async fn send_ipc_control_notification(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    recv: Result<
        plug_core::notifications::ProtocolNotification,
        tokio::sync::broadcast::error::RecvError,
    >,
    session_id: Option<&str>,
) -> anyhow::Result<()> {
    use plug_core::notifications::{NotificationTarget, ProtocolNotification, fanout};
    use tokio::sync::broadcast::error::RecvError;

    match recv {
        Ok(notification) => {
            // classify -> resolve -> (per-notification-kind delivery below).
            // `deliver` collapses the four `matches!(target, NotificationTarget::Ipc
            // {..} if session_id.is_some_and(..))` checks this function used to
            // repeat into one shared comparison; it's unused (and harmless) for
            // broadcast-shaped notifications. See
            // plug-core/src/notifications.rs::fanout.
            let identity = session_id.map(|sid| NotificationTarget::Ipc {
                client_id: sid.into(),
            });
            let deliver = identity
                .as_ref()
                .is_some_and(|id| fanout::resolve(fanout::classify(&notification)).deliver_to(id));
            match notification {
                ProtocolNotification::ToolListChanged => {
                    ipc::send_response(writer, &IpcResponse::ToolListChangedNotification)
                        .await
                        .ok();
                }
                ProtocolNotification::ToolListChangedFor { .. } => {
                    if deliver {
                        ipc::send_response(writer, &IpcResponse::ToolListChangedNotification)
                            .await
                            .ok();
                    }
                }
                ProtocolNotification::ResourceListChanged => {
                    ipc::send_response(writer, &IpcResponse::ResourceListChangedNotification)
                        .await
                        .ok();
                }
                ProtocolNotification::ResourceUpdated { params, .. } => {
                    if deliver {
                        let notif = IpcResponse::ResourceUpdatedNotification {
                            params: serde_json::to_value(params).unwrap_or_default(),
                        };
                        ipc::send_response(writer, &notif).await.ok();
                    }
                }
                ProtocolNotification::PromptListChanged => {
                    ipc::send_response(writer, &IpcResponse::PromptListChangedNotification)
                        .await
                        .ok();
                }
                ProtocolNotification::Progress { params, .. } => {
                    // Only forward if this notification targets our session
                    if deliver {
                        let notif = IpcResponse::ProgressNotification {
                            params: serde_json::to_value(params).unwrap_or_default(),
                        };
                        ipc::send_response(writer, &notif).await.ok();
                    }
                }
                ProtocolNotification::Cancelled { params, .. } => {
                    if deliver {
                        let notif = IpcResponse::CancelledNotification {
                            params: serde_json::to_value(params).unwrap_or_default(),
                        };
                        ipc::send_response(writer, &notif).await.ok();
                    }
                }
                ProtocolNotification::AuthStateChanged {
                    server_id,
                    new_state,
                } => {
                    let notif = IpcResponse::AuthStateChanged {
                        server_id: server_id.to_string(),
                        state: new_state,
                    };
                    ipc::send_response(writer, &notif).await.ok();
                }
                notification @ ProtocolNotification::TokenRefreshExchanged { .. } => {
                    if let Some(params) = notification.as_logging_message_params() {
                        let notif = IpcResponse::LoggingNotification {
                            params: serde_json::to_value(params).unwrap_or_default(),
                        };
                        ipc::send_response(writer, &notif).await.ok();
                    }
                }
                ProtocolNotification::LoggingMessage { .. } => {
                    // Logging is handled by the dedicated logging channel.
                }
            }
        }
        Err(RecvError::Lagged(skipped)) => {
            tracing::warn!(skipped, "IPC control notification lagged");
            let notif = IpcResponse::LoggingNotification {
                params: serde_json::to_value(
                    plug_core::notifications::ProtocolNotification::control_lagged_logging_params(
                        skipped as u64,
                        "ipc",
                    ),
                )
                .unwrap_or_default(),
            };
            ipc::send_response(writer, &notif).await.ok();
        }
        Err(RecvError::Closed) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ── IPC control notification forwarding tests ────────────────────────

    /// Helper: send a control notification through the daemon helper and read
    /// the IpcResponse that was written to the socket.
    async fn send_and_read_control_notification(
        notification: plug_core::notifications::ProtocolNotification,
        session_id: Option<&str>,
    ) -> Option<IpcResponse> {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (mut r_client, _w_client) = client.into_split();
        let (_r_server, mut w_server) = server.into_split();

        send_ipc_control_notification(&mut w_server, Ok(notification), session_id)
            .await
            .expect("send should not fail");

        // Drop the writer so the reader gets EOF instead of blocking
        drop(w_server);

        // Try to read a frame — None means nothing was written (filtered out)
        match ipc::read_frame(&mut r_client).await {
            Ok(Some(frame)) => Some(serde_json::from_slice(&frame).unwrap()),
            Ok(None) => None,
            Err(e) => panic!("unexpected read error: {e}"),
        }
    }

    #[tokio::test]
    async fn control_notification_broadcasts_tool_list_changed() {
        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ToolListChanged,
            Some("sess-1"),
        )
        .await;

        assert!(
            matches!(resp, Some(IpcResponse::ToolListChangedNotification)),
            "expected ToolListChangedNotification, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_routes_targeted_tool_list_changed() {
        let matching = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ToolListChangedFor {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: std::sync::Arc::from("sess-1"),
                },
            },
            Some("sess-1"),
        )
        .await;
        assert!(
            matches!(matching, Some(IpcResponse::ToolListChangedNotification)),
            "expected targeted ToolListChangedNotification, got: {matching:?}"
        );

        let non_matching = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ToolListChangedFor {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: std::sync::Arc::from("sess-2"),
                },
            },
            Some("sess-1"),
        )
        .await;
        assert!(
            non_matching.is_none(),
            "expected non-matching targeted notification to be filtered, got: {non_matching:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_broadcasts_resource_list_changed() {
        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ResourceListChanged,
            Some("sess-1"),
        )
        .await;

        assert!(
            matches!(resp, Some(IpcResponse::ResourceListChangedNotification)),
            "expected ResourceListChangedNotification, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_forwards_resource_updated_for_matching_session() {
        let matching = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ResourceUpdated {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: Arc::from("sess-42"),
                },
                params: rmcp::model::ResourceUpdatedNotificationParam::new(
                    "file:///tmp/mock-resource.txt",
                ),
            },
            Some("sess-42"),
        )
        .await;

        match matching {
            Some(IpcResponse::ResourceUpdatedNotification { params }) => {
                assert_eq!(params["uri"], "file:///tmp/mock-resource.txt");
            }
            other => panic!("expected ResourceUpdatedNotification, got: {other:?}"),
        }

        let non_matching = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::ResourceUpdated {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: Arc::from("sess-other"),
                },
                params: rmcp::model::ResourceUpdatedNotificationParam::new(
                    "file:///tmp/mock-resource.txt",
                ),
            },
            Some("sess-42"),
        )
        .await;
        assert!(
            non_matching.is_none(),
            "expected non-matching resource update to be filtered, got: {non_matching:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_broadcasts_prompt_list_changed() {
        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::PromptListChanged,
            Some("sess-1"),
        )
        .await;

        assert!(
            matches!(resp, Some(IpcResponse::PromptListChangedNotification)),
            "expected PromptListChangedNotification, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_forwards_progress_for_matching_session() {
        use rmcp::model::{NumberOrString, ProgressNotificationParam, ProgressToken};

        let progress_token = ProgressToken(NumberOrString::String(Arc::from("tok-1")));
        let params = ProgressNotificationParam {
            progress_token: progress_token.clone(),
            progress: 50.0,
            total: Some(100.0),
            message: Some("halfway".to_string()),
        };

        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::Progress {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: Arc::from("sess-42"),
                },
                params,
            },
            Some("sess-42"), // matches target
        )
        .await;

        match resp {
            Some(IpcResponse::ProgressNotification { params }) => {
                // Verify the serialized params contain the progress data
                assert_eq!(params["progress"], 50.0);
                assert_eq!(params["total"], 100.0);
                assert_eq!(params["message"], "halfway");
            }
            other => panic!("expected ProgressNotification, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn control_notification_filters_progress_for_different_session() {
        use rmcp::model::{NumberOrString, ProgressNotificationParam, ProgressToken};

        let progress_token = ProgressToken(NumberOrString::String(Arc::from("tok-1")));
        let params = ProgressNotificationParam {
            progress_token,
            progress: 50.0,
            total: Some(100.0),
            message: None,
        };

        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::Progress {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: Arc::from("sess-OTHER"),
                },
                params,
            },
            Some("sess-42"), // does NOT match target
        )
        .await;

        assert!(
            resp.is_none(),
            "progress for a different session should be filtered out, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_forwards_cancelled_for_matching_session() {
        use rmcp::model::{CancelledNotificationParam, NumberOrString, RequestId};

        let params = CancelledNotificationParam {
            request_id: RequestId::from(NumberOrString::Number(99)),
            reason: Some("user cancelled".to_string()),
        };

        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::Cancelled {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: Arc::from("sess-7"),
                },
                params,
            },
            Some("sess-7"), // matches target
        )
        .await;

        match resp {
            Some(IpcResponse::CancelledNotification { params }) => {
                assert_eq!(params["reason"], "user cancelled");
            }
            other => panic!("expected CancelledNotification, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn control_notification_filters_cancelled_for_different_session() {
        use rmcp::model::{CancelledNotificationParam, NumberOrString, RequestId};

        let params = CancelledNotificationParam {
            request_id: RequestId::from(NumberOrString::Number(99)),
            reason: None,
        };

        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::Cancelled {
                target: plug_core::notifications::NotificationTarget::Ipc {
                    client_id: Arc::from("sess-OTHER"),
                },
                params,
            },
            Some("sess-7"), // does NOT match target
        )
        .await;

        assert!(
            resp.is_none(),
            "cancelled for a different session should be filtered out, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_ignores_logging_on_control_channel() {
        use rmcp::model::LoggingMessageNotificationParam;

        let params = LoggingMessageNotificationParam::new(
            rmcp::model::LoggingLevel::Info,
            serde_json::json!("test log"),
        );
        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::LoggingMessage { params },
            Some("sess-1"),
        )
        .await;

        assert!(
            resp.is_none(),
            "logging messages should not be forwarded on the control channel, got: {resp:?}"
        );
    }

    #[tokio::test]
    async fn control_notification_forwards_token_refresh_exchanged_as_logging() {
        let resp = send_and_read_control_notification(
            plug_core::notifications::ProtocolNotification::TokenRefreshExchanged {
                server_id: Arc::from("github"),
            },
            Some("sess-1"),
        )
        .await;

        match resp {
            Some(IpcResponse::LoggingNotification { params }) => {
                assert_eq!(params["logger"], "plug.auth");
                assert_eq!(params["data"]["event"], "token_refresh_exchanged");
                assert_eq!(params["data"]["server_id"], "github");
            }
            other => panic!("expected LoggingNotification, got: {other:?}"),
        }
    }
}
