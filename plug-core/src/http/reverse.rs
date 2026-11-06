use super::error::HttpError;
use super::server::*;
use crate::notifications::NotificationTarget;
use crate::proxy::DownstreamBridge;
use crate::session::{SessionSendOutcome, SseMessage, SseReplayKey};
use rmcp::ErrorData as McpError;
use rmcp::model::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::oneshot;

/// HTTP-specific bridge for forwarding reverse requests (elicitation, sampling)
/// to a downstream HTTP client via its SSE stream.
pub(super) struct HttpBridge {
    pub(super) state: Arc<HttpState>,
    pub(super) session_id: Arc<str>,
    pub(super) capabilities: ClientCapabilities,
}

impl DownstreamBridge for HttpBridge {
    fn create_elicitation(
        &self,
        request: ElicitRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<ElicitResult, McpError>> + Send + '_>> {
        if self.capabilities.elicitation.is_none() {
            return Box::pin(async {
                Err(McpError::internal_error(
                    "client does not support elicitation".to_string(),
                    None,
                ))
            });
        }
        let state = Arc::clone(&self.state);
        let session_id = Arc::clone(&self.session_id);
        Box::pin(async move {
            let result = send_http_client_request(
                &state,
                &session_id,
                ServerRequest::ElicitRequest(ElicitRequest::new(request)),
                Duration::from_secs(600), // 10-minute upper bound prevents resource leaks
            )
            .await?;
            match result {
                ClientResult::ElicitResult(r) => Ok(r),
                other => Err(McpError::internal_error(
                    format!("unexpected elicitation response: {other:?}"),
                    None,
                )),
            }
        })
    }

    fn create_message(
        &self,
        request: CreateMessageRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<CreateMessageResult, McpError>> + Send + '_>> {
        if self.capabilities.sampling.is_none() {
            return Box::pin(async {
                Err(McpError::internal_error(
                    "client does not support sampling".to_string(),
                    None,
                ))
            });
        }
        let state = Arc::clone(&self.state);
        let session_id = Arc::clone(&self.session_id);
        Box::pin(async move {
            let result = send_http_client_request(
                &state,
                &session_id,
                ServerRequest::CreateMessageRequest(CreateMessageRequest::new(request)),
                Duration::from_secs(60), // sampling has bounded timeout
            )
            .await?;
            match result {
                ClientResult::CreateMessageResult(r) => Ok(*r),
                other => Err(McpError::internal_error(
                    format!("unexpected sampling response: {other:?}"),
                    None,
                )),
            }
        })
    }
}

/// Send a reverse JSON-RPC request to an HTTP client via its SSE stream and
/// await the response posted back via POST.
pub(super) async fn send_http_client_request(
    state: &HttpState,
    session_id: &str,
    request: ServerRequest,
    timeout: Duration,
) -> Result<ClientResult, McpError> {
    let id = i64::try_from(state.reverse_request_counter.fetch_add(1, Ordering::SeqCst))
        .map_err(|_| McpError::internal_error("reverse request id overflow".to_string(), None))?;
    let request_id = RequestId::from(NumberOrString::Number(id));
    let message = ServerJsonRpcMessage::request(request, request_id);
    let message = serde_json::to_value(message)
        .map_err(|e| McpError::internal_error(e.to_string(), None))
        .and_then(|value| {
            SseMessage::from_json_value_with_replay_key(value, SseReplayKey::ReverseRequest(id))
                .map_err(|e| McpError::internal_error(e.to_string(), None))
        })?;

    let has_live_sse_sender = state
        .sessions
        .has_live_sse_sender(session_id)
        .map_err(|_| {
            McpError::internal_error(
                "HTTP client SSE stream disappeared before reverse request delivery".to_string(),
                None,
            )
        })?;
    if !has_live_sse_sender {
        return Err(McpError::internal_error(
            "HTTP client SSE stream disappeared before reverse request delivery".to_string(),
            None,
        ));
    }

    let (tx, rx) = oneshot::channel();
    state
        .pending_client_requests
        .insert((session_id.to_string(), id), tx);
    // From here on every exit, including the caller dropping this future
    // (a cancelled elicitation or sampling call), releases the pending entry
    // and the replay copy of the request.
    let _pending = PendingReverseRequest {
        state,
        session_id,
        id,
    };
    match state.sessions.send_to_live_session(session_id, message) {
        SessionSendOutcome::Delivered | SessionSendOutcome::Queued => {}
        SessionSendOutcome::SessionNotFound => {
            return Err(McpError::internal_error(
                "HTTP client SSE stream could not accept reverse request delivery".to_string(),
                None,
            ));
        }
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(_)) => Err(McpError::internal_error(
            "HTTP client response channel closed".to_string(),
            None,
        )),
        Err(_) => Err(McpError::internal_error(
            "HTTP client request timed out".to_string(),
            None,
        )),
    }
}

/// Releases a reverse request's bookkeeping when its caller stops waiting,
/// for whatever reason. Both removals are no-ops once the response handler
/// has already taken them.
pub(super) struct PendingReverseRequest<'a> {
    pub(super) state: &'a HttpState,
    pub(super) session_id: &'a str,
    pub(super) id: i64,
}

impl Drop for PendingReverseRequest<'_> {
    fn drop(&mut self) {
        self.state
            .pending_client_requests
            .remove(&(self.session_id.to_string(), self.id));
        self.state
            .sessions
            .remove_replay_events_by_key(self.session_id, &SseReplayKey::ReverseRequest(self.id));
    }
}

/// Handle a client response (POST) that is the answer to a reverse request
/// we sent via SSE (e.g. roots/list).
pub(super) async fn handle_client_response(
    response: JsonRpcResponse<ClientResult>,
    session_id: &str,
    state: &HttpState,
) -> Result<(), HttpError> {
    let request_id = match response.id {
        RequestId::Number(id) => id,
        RequestId::String(_) => return Ok(()),
    };

    if let Some((_, tx)) = state
        .pending_client_requests
        .remove(&(session_id.to_string(), request_id))
    {
        state
            .sessions
            .remove_replay_events_by_key(session_id, &SseReplayKey::ReverseRequest(request_id));
        let _ = tx.send(response.result);
    }

    Ok(())
}

/// If the session supports roots, spawn a task to request roots via SSE
/// and cache the result.
pub(super) fn maybe_request_http_roots(state: Arc<HttpState>, session_id: String) {
    if !state
        .client_capabilities
        .get(&session_id)
        .is_some_and(|capabilities| capabilities.roots.is_some())
    {
        return;
    }
    tokio::spawn(async move {
        match send_http_client_request(
            &state,
            &session_id,
            ServerRequest::ListRootsRequest(ListRootsRequest {
                method: Default::default(),
                extensions: Default::default(),
            }),
            Duration::from_secs(10),
        )
        .await
        {
            Ok(ClientResult::ListRootsResult(result)) => {
                let target = NotificationTarget::Http {
                    session_id: Arc::from(session_id.as_str()),
                };
                if state.router.set_roots_for_target(target, result.roots) {
                    state.router.forward_roots_list_changed_to_upstreams().await;
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    session_id = %session_id,
                    "failed to refresh HTTP roots"
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_support::*;

    use std::time::Duration;

    #[tokio::test]
    async fn reverse_request_fails_fast_without_live_sse_sender() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();

        let result = send_http_client_request(
            &state,
            &session_id,
            ServerRequest::ListRootsRequest(ListRootsRequest {
                method: Default::default(),
                extensions: Default::default(),
            }),
            Duration::from_millis(50),
        )
        .await;

        assert!(result.is_err());
        assert!(state.pending_client_requests.is_empty());
    }

    #[tokio::test]
    async fn dropped_reverse_request_releases_pending_entry_and_replay_copy() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        state
            .sessions
            .set_sse_sender(&session_id, tx, None)
            .unwrap();

        let state_for_request = Arc::clone(&state);
        let session_id_for_request = session_id.clone();
        let pending = tokio::spawn(async move {
            send_http_client_request(
                &state_for_request,
                &session_id_for_request,
                ServerRequest::ListRootsRequest(ListRootsRequest {
                    method: Default::default(),
                    extensions: Default::default(),
                }),
                Duration::from_secs(600),
            )
            .await
        });
        rx.recv().await.expect("reverse request delivered");
        assert_eq!(state.pending_client_requests.len(), 1);

        // The caller gives up, as a cancelled elicitation does.
        pending.abort();
        let _ = pending.await;

        assert!(
            state.pending_client_requests.is_empty(),
            "an abandoned reverse request must not stay pending"
        );
        let (reconnect_tx, mut reconnect_rx) = mpsc::channel(4);
        state
            .sessions
            .set_sse_sender(&session_id, reconnect_tx, Some(0))
            .unwrap();
        assert!(
            reconnect_rx.try_recv().is_err(),
            "an abandoned reverse request must not replay on reconnect"
        );
    }

    #[tokio::test]
    async fn reverse_request_succeeds_with_live_sse_sender() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        state
            .sessions
            .set_sse_sender(&session_id, tx, None)
            .unwrap();

        let state_for_response = Arc::clone(&state);
        let session_id_for_response = session_id.clone();
        tokio::spawn(async move {
            let message = rx.recv().await.expect("reverse request message");
            let message: ServerJsonRpcMessage = serde_json::from_str(message.message.as_str())
                .expect("deserialize reverse request");
            let request_id = match message {
                ServerJsonRpcMessage::Request(request) => request.id,
                other => panic!("unexpected reverse request payload: {other:?}"),
            };

            handle_client_response(
                JsonRpcResponse {
                    jsonrpc: Default::default(),
                    id: request_id,
                    result: ClientResult::ListRootsResult(ListRootsResult::default()),
                },
                &session_id_for_response,
                &state_for_response,
            )
            .await
            .expect("post reverse request response");
        });

        let result = send_http_client_request(
            &state,
            &session_id,
            ServerRequest::ListRootsRequest(ListRootsRequest {
                method: Default::default(),
                extensions: Default::default(),
            }),
            Duration::from_secs(1),
        )
        .await
        .expect("reverse request should succeed");

        match result {
            ClientResult::ListRootsResult(_) => {}
            other => panic!("unexpected reverse request result: {other:?}"),
        }
        assert!(state.pending_client_requests.is_empty());
    }

    // Paused time: send_http_client_request and the session store here are
    // purely in-memory (oneshot channel + DashMap, no socket I/O), so the
    // 20ms sleep giving the spawned request time to register is a same-
    // runtime scheduling gap, not a real-I/O wait; auto-advance resolves it
    // instantly while the spawned task's 1s reverse-request timeout (the
    // next-nearest timer) stays correctly ordered behind it.
    #[tokio::test(start_paused = true)]
    async fn queued_reverse_request_replays_on_reconnect() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();
        let (blocked_tx, _blocked_rx) = mpsc::channel(1);
        blocked_tx
            .try_send(crate::session::SseEvent {
                id: 999,
                message: SseMessage::from_json_value(serde_json::json!({"blocked": true})).unwrap(),
            })
            .unwrap();
        state
            .sessions
            .set_sse_sender(&session_id, blocked_tx, None)
            .unwrap();

        let state_for_request = Arc::clone(&state);
        let session_id_for_request = session_id.clone();
        let pending = tokio::spawn(async move {
            send_http_client_request(
                &state_for_request,
                &session_id_for_request,
                ServerRequest::ListRootsRequest(ListRootsRequest {
                    method: Default::default(),
                    extensions: Default::default(),
                }),
                Duration::from_secs(1),
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        let (reconnect_tx, mut reconnect_rx) = mpsc::channel(4);
        state
            .sessions
            .set_sse_sender(&session_id, reconnect_tx, Some(0))
            .unwrap();

        let event = reconnect_rx.recv().await.expect("replayed reverse request");
        let message: ServerJsonRpcMessage =
            serde_json::from_str(event.message.as_str()).expect("deserialize reverse request");
        let request_id = match message {
            ServerJsonRpcMessage::Request(request) => request.id,
            other => panic!("unexpected reverse request payload: {other:?}"),
        };

        handle_client_response(
            JsonRpcResponse {
                jsonrpc: Default::default(),
                id: request_id,
                result: ClientResult::ListRootsResult(ListRootsResult::default()),
            },
            &session_id,
            &state,
        )
        .await
        .expect("post reverse request response");

        let result = pending
            .await
            .expect("request task")
            .expect("reverse request should complete after reconnect");
        match result {
            ClientResult::ListRootsResult(_) => {}
            other => panic!("unexpected reverse request result: {other:?}"),
        }
        assert!(state.pending_client_requests.is_empty());
    }
}
