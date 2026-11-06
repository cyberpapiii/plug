use super::error::HttpError;
use super::server::*;
use crate::mcp_http_headers::{HEADER_MISMATCH_CODE, HeaderMismatch};
use crate::notifications::ProtocolNotification;
use crate::protocol::SUPPORTED_PROTOCOL_VERSION;
use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rmcp::model::*;
use serde_json::json;
use std::convert::Infallible;
use std::future::Future;
use std::time::Duration;

pub(super) fn header_mismatch_response(
    message: &ClientJsonRpcMessage,
    err: HeaderMismatch,
    era: crate::protocol::ProtocolEra,
) -> Response {
    let id = match message {
        JsonRpcMessage::Request(request) => serde_json::to_value(&request.id).unwrap_or_default(),
        _ => serde_json::Value::Null,
    };
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": HEADER_MISMATCH_CODE,
            "message": err.message,
        }
    });
    let mut response = (StatusCode::BAD_REQUEST, Json(body)).into_response();
    if era == crate::protocol::ProtocolEra::Modern {
        response.headers_mut().insert(
            PROTOCOL_VERSION_HEADER,
            HeaderValue::from_static(crate::protocol::MODERN_PROTOCOL_VERSION),
        );
    }
    response
}

/// Build a JSON response from a ServerJsonRpcMessage.
pub(super) fn json_response(msg: &ServerJsonRpcMessage) -> Result<Response, HttpError> {
    json_response_for_era(msg, crate::protocol::ProtocolEra::Legacy)
}

pub(super) fn json_response_for_era(
    msg: &ServerJsonRpcMessage,
    era: crate::protocol::ProtocolEra,
) -> Result<Response, HttpError> {
    let status = if era == crate::protocol::ProtocolEra::Modern {
        match msg {
            JsonRpcMessage::Error(error) if error.error.code == ErrorCode::METHOD_NOT_FOUND => {
                StatusCode::NOT_FOUND
            }
            JsonRpcMessage::Error(error)
                if matches!(
                    error.error.code,
                    ErrorCode::UNSUPPORTED_PROTOCOL_VERSION
                        | ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY
                        | ErrorCode::INVALID_PARAMS
                ) =>
            {
                StatusCode::BAD_REQUEST
            }
            _ => StatusCode::OK,
        }
    } else {
        StatusCode::OK
    };
    let body = serde_json::to_vec(msg)
        .map_err(|e| HttpError::Internal(format!("failed to serialize response: {e}")))?;

    let mut response = (status, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if era == crate::protocol::ProtocolEra::Modern {
        response.headers_mut().insert(
            PROTOCOL_VERSION_HEADER,
            HeaderValue::from_static(crate::protocol::MODERN_PROTOCOL_VERSION),
        );
    }

    Ok(response)
}

/// How long a legacy tool call may run before its reply becomes an event
/// stream. Tunnels and proxies drop a request that is silent for about a
/// hundred seconds.
pub(super) const LONG_CALL_AFTER: Duration = Duration::from_secs(45);

/// How often a held-open reply says it is still there.
pub(super) const LONG_CALL_KEEP_ALIVE: Duration = Duration::from_secs(15);

/// Answer a legacy request with plain JSON when it finishes within `patience`.
/// Past that the reply becomes an event stream that carries keep-alives until
/// the one message is ready, so nothing between Plug and the client gives up
/// on a call that is still running.
pub(super) async fn answer_or_keep_open(
    call: impl Future<Output = ServerJsonRpcMessage> + Send + 'static,
    patience: Duration,
) -> Result<Response, HttpError> {
    use axum::response::sse::{Event, KeepAlive, Sse};

    let mut call = Box::pin(call);
    tokio::select! {
        message = &mut call => return json_response(&message),
        _ = tokio::time::sleep(patience) => {}
    }
    let stream = futures::stream::once(async move {
        let message = call.await;
        let data = serde_json::to_string(&message).unwrap_or_else(|_| "null".to_string());
        Ok::<Event, Infallible>(Event::default().data(data))
    });
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(LONG_CALL_KEEP_ALIVE))
        .into_response();
    response
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// Return modern server notifications and the final JSON-RPC response on one
/// finite SSE stream. Streamable HTTP has no session-wide GET channel in the
/// modern era, so request-scoped progress must travel on the POST response.
pub(super) fn modern_sse_response(
    notifications: &[ProtocolNotification],
    response_message: &ServerJsonRpcMessage,
) -> Result<Response, HttpError> {
    use axum::response::sse::{Event, Sse};

    let mut payloads = notifications
        .iter()
        .map(ProtocolNotification::to_server_jsonrpc_message)
        .map(|message| {
            serde_json::to_string(&message).map_err(|error| {
                HttpError::Internal(format!("failed to serialize SSE event: {error}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    payloads.push(serde_json::to_string(response_message).map_err(|error| {
        HttpError::Internal(format!("failed to serialize SSE response: {error}"))
    })?);

    let stream = futures::stream::iter(
        payloads
            .into_iter()
            .map(|payload| Ok::<Event, Infallible>(Event::default().data(payload))),
    );
    let mut response = Sse::new(stream).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        PROTOCOL_VERSION_HEADER,
        HeaderValue::from_static(crate::protocol::MODERN_PROTOCOL_VERSION),
    );
    Ok(response)
}

pub(super) fn json_response_with_session(
    session_id: &str,
    msg: &ServerJsonRpcMessage,
) -> Result<Response, HttpError> {
    let mut response = json_response(msg)?;
    response.headers_mut().insert(
        SESSION_ID_HEADER,
        HeaderValue::from_str(session_id)
            .map_err(|_| HttpError::Internal("invalid session ID".into()))?,
    );
    response.headers_mut().insert(
        PROTOCOL_VERSION_HEADER,
        HeaderValue::from_static(SUPPORTED_PROTOCOL_VERSION),
    );

    Ok(response)
}

#[cfg(test)]
mod tests {

    use super::*;

    use std::time::Duration;

    async fn pong(after: Duration) -> ServerJsonRpcMessage {
        tokio::time::sleep(after).await;
        ServerJsonRpcMessage::response(
            ServerResult::EmptyResult(EmptyObject {}),
            RequestId::Number(7),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn a_quick_call_is_answered_with_plain_json() {
        let resp = answer_or_keep_open(pong(Duration::from_secs(1)), LONG_CALL_AFTER)
            .await
            .unwrap();
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/json");
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_call_is_held_open_and_still_answered() {
        let resp = answer_or_keep_open(pong(Duration::from_secs(600)), LONG_CALL_AFTER)
            .await
            .unwrap();
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/event-stream");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        // Keep-alives while it ran, then the one answer.
        assert!(body.matches(":\n").count() >= 30, "{body}");
        assert!(
            body.contains("data: {") && body.contains("\"id\":7"),
            "{body}"
        );
    }
}
