//! Notification delivery to IPC clients — control-channel fanout.

use plug_core::ipc::{self, IpcResponse};
use plug_core::notifications::{ListsChanged, NotificationTarget, ProtocolNotification};

/// Send a protocol (control) notification to the IPC client, handling
/// broadcast errors. A failed write is returned, so a dead connection ends at
/// once.
///
/// Broadcast notifications (list_changed, auth state, token refresh) go to
/// every registered IPC client; targeted ones (progress, cancelled, resource
/// updated, a client's own tool list) only to the session they name.
pub(super) async fn send_ipc_control_notification(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    recv: Result<ProtocolNotification, tokio::sync::broadcast::error::RecvError>,
    session_id: Option<&str>,
) -> anyhow::Result<()> {
    use tokio::sync::broadcast::error::RecvError;

    let messages: Vec<IpcResponse> = match recv {
        Ok(notification) => ipc_message(notification, session_id).into_iter().collect(),
        Err(RecvError::Lagged(skipped)) => {
            tracing::warn!(skipped, "IPC control notification lagged");
            let lagged = IpcResponse::LoggingNotification {
                params: serde_json::to_value(ProtocolNotification::control_lagged_logging_params(
                    skipped, "ipc",
                ))
                .unwrap_or_default(),
            };
            // What was skipped may have said a list changed.
            std::iter::once(lagged)
                .chain(
                    ListsChanged::ALL
                        .notifications()
                        .filter_map(|notification| ipc_message(notification, session_id)),
                )
                .collect()
        }
        Err(RecvError::Closed) => Vec::new(),
    };
    for message in &messages {
        ipc::send_response(writer, message).await?;
    }
    Ok(())
}

/// What the IPC client of `session_id` is sent for `notification`, if
/// anything.
fn ipc_message(
    notification: ProtocolNotification,
    session_id: Option<&str>,
) -> Option<IpcResponse> {
    let identity = session_id.map(|sid| NotificationTarget::Ipc {
        client_id: sid.into(),
    });
    let deliver = identity.as_ref().is_some_and(|id| notification.is_for(id));
    match notification {
        ProtocolNotification::ToolListChanged => Some(IpcResponse::ToolListChangedNotification),
        ProtocolNotification::ToolListChangedFor { .. } => {
            deliver.then_some(IpcResponse::ToolListChangedNotification)
        }
        ProtocolNotification::ResourceListChanged => {
            Some(IpcResponse::ResourceListChangedNotification)
        }
        ProtocolNotification::ResourceUpdated { params, .. } => {
            deliver.then(|| IpcResponse::ResourceUpdatedNotification {
                params: serde_json::to_value(params).unwrap_or_default(),
            })
        }
        ProtocolNotification::PromptListChanged => Some(IpcResponse::PromptListChangedNotification),
        ProtocolNotification::Progress { params, .. } => {
            deliver.then(|| IpcResponse::ProgressNotification {
                params: serde_json::to_value(params).unwrap_or_default(),
            })
        }
        ProtocolNotification::Cancelled { params, .. } => {
            deliver.then(|| IpcResponse::CancelledNotification {
                params: serde_json::to_value(params).unwrap_or_default(),
            })
        }
        ProtocolNotification::AuthStateChanged {
            server_id,
            new_state,
        } => Some(IpcResponse::AuthStateChanged {
            server_id: server_id.to_string(),
            state: new_state,
        }),
        notification @ ProtocolNotification::TokenRefreshExchanged { .. } => notification
            .as_logging_message_params()
            .map(|params| IpcResponse::LoggingNotification {
                params: serde_json::to_value(params).unwrap_or_default(),
            }),
        // Logging has its own channel.
        ProtocolNotification::LoggingMessage { .. } => None,
    }
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

    /// What the client fell too far behind to read may have said a list
    /// changed, so it is told all three may have.
    #[tokio::test]
    async fn a_lagged_client_hears_every_list_may_have_changed() {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let (mut r_client, _w_client) = client.into_split();
        let (_r_server, mut w_server) = server.into_split();

        send_ipc_control_notification(
            &mut w_server,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(7)),
            Some("sess-1"),
        )
        .await
        .expect("send should not fail");
        drop(w_server);

        let mut sent = Vec::new();
        while let Some(frame) = ipc::read_frame(&mut r_client).await.unwrap() {
            sent.push(serde_json::from_slice::<IpcResponse>(&frame).unwrap());
        }
        assert!(
            matches!(
                sent.as_slice(),
                [
                    IpcResponse::LoggingNotification { .. },
                    IpcResponse::ToolListChangedNotification,
                    IpcResponse::ResourceListChangedNotification,
                    IpcResponse::PromptListChangedNotification,
                ]
            ),
            "{sent:?}"
        );
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
        let params = ProgressNotificationParam::new(progress_token.clone(), 50.0)
            .with_total(100.0)
            .with_message("halfway");

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
        let params = ProgressNotificationParam::new(progress_token, 50.0).with_total(100.0);

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

        let params = CancelledNotificationParam::new(
            Some(RequestId::from(NumberOrString::Number(99))),
            Some("user cancelled".to_string()),
        );

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

        let params = CancelledNotificationParam::new(
            Some(RequestId::from(NumberOrString::Number(99))),
            None,
        );

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
