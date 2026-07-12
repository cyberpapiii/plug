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
