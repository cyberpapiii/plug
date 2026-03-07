use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// HTTP-layer errors that map to HTTP status codes.
/// Separate from `ProtocolError` which maps to JSON-RPC error codes.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("forbidden: invalid origin")]
    InvalidOrigin,

    #[error("session ID required")]
    SessionRequired,

    #[error("session not found")]
    SessionNotFound,

    #[error("unsupported content type")]
    InvalidContentType,

    #[error("accept header must include text/event-stream")]
    InvalidAcceptHeader,

    #[error("unauthorized: authentication required")]
    Unauthorized,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("too many sessions")]
    TooManySessions,

    #[error("request body too large")]
    BodyTooLarge,

    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        // SECURITY: Do NOT include session IDs or internal details in error bodies.
        let (status, message) = match &self {
            HttpError::Unauthorized => {
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": { "code": -32001, "message": "authentication required" },
                    "id": null
                });
                let mut response = (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response();
                response
                    .headers_mut()
                    .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                return response;
            }
            HttpError::InvalidOrigin => (StatusCode::FORBIDDEN, "forbidden"),
            HttpError::SessionRequired => (StatusCode::BAD_REQUEST, "session ID required"),
            HttpError::SessionNotFound => (StatusCode::NOT_FOUND, "session not found"),
            HttpError::InvalidContentType => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "content type must be application/json",
            ),
            HttpError::InvalidAcceptHeader => (
                StatusCode::NOT_ACCEPTABLE,
                "accept header must include text/event-stream",
            ),
            HttpError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.as_str()),
            HttpError::TooManySessions => (StatusCode::TOO_MANY_REQUESTS, "too many sessions"),
            HttpError::BodyTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
            HttpError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal server error"),
        };

        (status, message.to_string()).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_status_codes() {
        let cases: Vec<(HttpError, StatusCode)> = vec![
            (HttpError::Unauthorized, StatusCode::UNAUTHORIZED),
            (HttpError::InvalidOrigin, StatusCode::FORBIDDEN),
            (HttpError::SessionRequired, StatusCode::BAD_REQUEST),
            (HttpError::SessionNotFound, StatusCode::NOT_FOUND),
            (
                HttpError::InvalidContentType,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ),
            (HttpError::InvalidAcceptHeader, StatusCode::NOT_ACCEPTABLE),
            (
                HttpError::BadRequest("test".into()),
                StatusCode::BAD_REQUEST,
            ),
            (HttpError::TooManySessions, StatusCode::TOO_MANY_REQUESTS),
            (HttpError::BodyTooLarge, StatusCode::PAYLOAD_TOO_LARGE),
            (
                HttpError::Internal("err".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (error, expected_status) in cases {
            let response = error.into_response();
            assert_eq!(response.status(), expected_status);
        }
    }

    #[test]
    fn session_not_found_does_not_leak_session_id() {
        // Security: error response body must not contain session IDs
        let error = HttpError::SessionNotFound;
        let display = format!("{error}");
        assert!(!display.contains("session_id"));
        assert!(!display.contains("uuid"));
    }
}
