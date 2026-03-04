use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Router, extract::Request};
use rmcp::model::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tower_http::limit::RequestBodyLimitLayer;

use super::error::HttpError;
use super::session::SessionManager;
use super::sse::sse_stream;
use crate::proxy::ToolRouter;

/// rmcp header constant for session ID.
const SESSION_ID_HEADER: &str = "Mcp-Session-Id";

/// MCP protocol version header name.
const PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";

/// The MCP protocol version we implement.
const PROTOCOL_VERSION: &str = "2025-11-05";

/// Shared state for all HTTP handlers.
pub struct HttpState {
    pub router: Arc<ToolRouter>,
    pub sessions: SessionManager,
    pub cancel: CancellationToken,
    pub sse_channel_capacity: usize,
}

/// Build the axum Router with all middleware and handlers.
pub fn build_router(state: Arc<HttpState>) -> Router {
    Router::new()
        .route("/mcp", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .layer(middleware::from_fn(validate_origin))
        .layer(RequestBodyLimitLayer::new(4 * 1024 * 1024)) // 4MB DoS prevention
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Validate Origin header for DNS rebinding prevention.
///
/// - Missing Origin: allowed (non-browser MCP clients don't send it)
/// - localhost/127.0.0.1/[::1]: allowed
/// - "null" literal: rejected (DNS rebinding vector)
/// - Anything else: rejected
async fn validate_origin(req: Request, next: Next) -> Result<Response, HttpError> {
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let origin = origin.to_str().map_err(|_| HttpError::InvalidOrigin)?;

        if origin == "null" {
            return Err(HttpError::InvalidOrigin);
        }

        // Parse origin to extract host — prevents bypass via localhost.evil.com
        let is_local = if let Some(host) = extract_origin_host(origin) {
            host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1"
        } else {
            false
        };

        if !is_local {
            return Err(HttpError::InvalidOrigin);
        }
    }

    Ok(next.run(req).await)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /mcp — handle JSON-RPC requests, notifications, and client responses.
async fn post_mcp(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, HttpError> {
    // 1. Validate Content-Type
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        return Err(HttpError::InvalidContentType);
    }

    // 2. Parse JSON-RPC message
    let message: ClientJsonRpcMessage = serde_json::from_slice(&body).map_err(|e| {
        tracing::debug!(error = %e, "invalid JSON-RPC message from client");
        HttpError::BadRequest("invalid JSON-RPC message".into())
    })?;

    // 3. Route based on message type
    match message {
        JsonRpcMessage::Request(req) => handle_request(req, &headers, &state).await,
        JsonRpcMessage::Response(_) => {
            // Client response for sampling/elicitation — validate session
            validate_session_header(&headers, &state.sessions)?;
            Ok(StatusCode::ACCEPTED.into_response())
        }
        JsonRpcMessage::Notification(_) => {
            // Notification (e.g. initialized, cancelled) — validate session
            validate_session_header(&headers, &state.sessions)?;
            Ok(StatusCode::ACCEPTED.into_response())
        }
        JsonRpcMessage::Error(_) => {
            Err(HttpError::BadRequest("unexpected error message from client".into()))
        }
    }
}

/// GET /mcp — open SSE stream for server-initiated notifications.
async fn get_mcp(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    // 1. Validate session
    let session_id = extract_session_id(&headers)?;
    state.sessions.validate(&session_id)?;

    // 2. Validate Accept header
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !accept.contains("text/event-stream") {
        return Err(HttpError::InvalidAcceptHeader);
    }

    // 3. Create channel and register SSE sender
    let (tx, rx) = mpsc::channel(state.sse_channel_capacity);
    state.sessions.set_sse_sender(&session_id, tx)?;

    // 4. Build SSE response with appropriate headers
    let sse = sse_stream(rx, state.cancel.clone());
    let mut response = sse.into_response();
    response.headers_mut().insert(
        "X-Accel-Buffering",
        HeaderValue::from_static("no"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );

    Ok(response)
}

/// DELETE /mcp — terminate a session.
async fn delete_mcp(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let session_id = extract_session_id(&headers)?;

    if state.sessions.remove(&session_id) {
        tracing::info!(session_id = %session_id, "session terminated via DELETE");
        Ok(StatusCode::OK.into_response())
    } else {
        Err(HttpError::SessionNotFound)
    }
}

// ---------------------------------------------------------------------------
// Request routing
// ---------------------------------------------------------------------------

/// Route a typed JSON-RPC request to the appropriate handler.
async fn handle_request(
    req: JsonRpcRequest<ClientRequest>,
    headers: &HeaderMap,
    state: &HttpState,
) -> Result<Response, HttpError> {
    let request_id = req.id.clone();

    match req.request {
        ClientRequest::InitializeRequest(init_req) => {
            // Initialize: create session, return server info
            let session_id = state.sessions.create_session()?;

            let client_name = &init_req.params.client_info.name;
            let client_type = crate::client_detect::detect_client(client_name);
            tracing::info!(
                client = %client_name,
                detected = %client_type,
                session = %session_id,
                "HTTP client connected"
            );

            let result = build_initialize_result();

            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::InitializeResult(result),
                request_id,
            );

            json_response_with_session(&session_id, &response_msg)
        }

        ClientRequest::PingRequest(_) => {
            validate_session_header(headers, &state.sessions)?;
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::EmptyResult(EmptyResult {}),
                request_id,
            );
            json_response(&response_msg)
        }

        ClientRequest::ListToolsRequest(_) => {
            validate_session_header(headers, &state.sessions)?;
            let tools = state.router.list_tools();
            let result = ListToolsResult::with_all_items((*tools).clone());
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::ListToolsResult(result),
                request_id,
            );
            json_response(&response_msg)
        }

        ClientRequest::CallToolRequest(call_req) => {
            validate_session_header(headers, &state.sessions)?;
            match state
                .router
                .call_tool(call_req.params.name.as_ref(), call_req.params.arguments)
                .await
            {
                Ok(result) => {
                    let response_msg = ServerJsonRpcMessage::response(
                        ServerResult::CallToolResult(result),
                        request_id,
                    );
                    json_response(&response_msg)
                }
                Err(mcp_err) => {
                    let response_msg = ServerJsonRpcMessage::error(mcp_err, request_id);
                    json_response(&response_msg)
                }
            }
        }

        ClientRequest::ListResourcesRequest(_) => {
            validate_session_header(headers, &state.sessions)?;
            let result = ListResourcesResult::default();
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::ListResourcesResult(result),
                request_id,
            );
            json_response(&response_msg)
        }

        ClientRequest::ListPromptsRequest(_) => {
            validate_session_header(headers, &state.sessions)?;
            let result = ListPromptsResult::default();
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::ListPromptsResult(result),
                request_id,
            );
            json_response(&response_msg)
        }

        _ => {
            // Unsupported method — return JSON-RPC method not found error
            validate_session_header(headers, &state.sessions)?;
            let error = ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "method not supported",
                None,
            );
            let response_msg = ServerJsonRpcMessage::error(error, request_id);
            json_response(&response_msg)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build the InitializeResult (same as ProxyHandler::get_info).
fn build_initialize_result() -> InitializeResult {
    let mut capabilities = ServerCapabilities::default();
    capabilities.tools = Some(ToolsCapability {
        list_changed: Some(true),
    });
    capabilities.resources = Some(ResourcesCapability {
        list_changed: Some(false),
        subscribe: None,
    });

    InitializeResult::new(capabilities)
        .with_server_info(Implementation::new("plug", env!("CARGO_PKG_VERSION")))
}

/// Extract the host from an Origin header value.
///
/// Origin format is `scheme://host[:port]`. We parse manually to avoid
/// pulling in the `url` crate dependency for this single use case.
fn extract_origin_host(origin: &str) -> Option<&str> {
    let after_scheme = origin.split("://").nth(1)?;
    // Strip port and path: take everything before ':' or '/'
    let host = after_scheme
        .split(':')
        .next()
        .unwrap_or(after_scheme)
        .split('/')
        .next()
        .unwrap_or(after_scheme);
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// Extract session ID from request headers.
fn extract_session_id(headers: &HeaderMap) -> Result<String, HttpError> {
    headers
        .get(SESSION_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or(HttpError::SessionRequired)
}

/// Validate that the session exists and is not expired.
fn validate_session_header(
    headers: &HeaderMap,
    sessions: &SessionManager,
) -> Result<(), HttpError> {
    let session_id = extract_session_id(headers)?;
    sessions.validate(&session_id)
}

/// Build a JSON response from a ServerJsonRpcMessage.
fn json_response(msg: &ServerJsonRpcMessage) -> Result<Response, HttpError> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| HttpError::Internal(format!("failed to serialize response: {e}")))?;

    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );

    Ok(response)
}

/// Build a JSON response with MCP-Session-Id header (used for initialize).
fn json_response_with_session(
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
        HeaderValue::from_static(PROTOCOL_VERSION),
    );

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::Request as HttpRequest;
    use tower::ServiceExt;

    fn test_state() -> Arc<HttpState> {
        let sm = Arc::new(crate::server::ServerManager::new());
        let router = Arc::new(ToolRouter::new(sm, "__".to_string()));
        Arc::new(HttpState {
            router,
            sessions: SessionManager::new(1800, 100),
            cancel: CancellationToken::new(),
            sse_channel_capacity: 32,
        })
    }

    #[tokio::test]
    async fn post_without_content_type_returns_415() {
        let app = build_router(test_state());
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn post_initialize_returns_session_id() {
        let state = test_state();
        let app = build_router(state);

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0"
                }
            }
        });

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(SESSION_ID_HEADER).is_some());
        assert!(resp.headers().get(PROTOCOL_VERSION_HEADER).is_some());
    }

    #[tokio::test]
    async fn post_tools_list_without_session_returns_400() {
        let app = build_router(test_state());

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        });

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_without_session_returns_400() {
        let app = build_router(test_state());

        let req = HttpRequest::builder()
            .method("DELETE")
            .uri("/mcp")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_with_valid_session_returns_200() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("DELETE")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_without_accept_header_returns_406() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("GET")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_ACCEPTABLE);
    }

    #[tokio::test]
    async fn origin_null_rejected() {
        let app = build_router(test_state());

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("origin", "null")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn origin_external_rejected() {
        let app = build_router(test_state());

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("origin", "https://evil.example.com")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// Full session lifecycle: initialize → tools/list → ping → delete
    #[tokio::test]
    async fn full_session_lifecycle() {
        let state = test_state();

        // 1. Initialize — get session ID
        let app = build_router(state.clone());
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-05",
                "capabilities": {},
                "clientInfo": { "name": "lifecycle-test", "version": "1.0" }
            }
        });
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let session_id = resp
            .headers()
            .get(SESSION_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // 2. tools/list with session
        let app = build_router(state.clone());
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        });
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header(SESSION_ID_HEADER, &session_id)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 3. ping with session
        let app = build_router(state.clone());
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "ping"
        });
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header(SESSION_ID_HEADER, &session_id)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 4. DELETE session
        let app = build_router(state.clone());
        let req = HttpRequest::builder()
            .method("DELETE")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 5. Verify session is gone — tools/list should fail
        let app = build_router(state);
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/list"
        });
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header(SESSION_ID_HEADER, &session_id)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn notification_without_session_returns_400() {
        let app = build_router(test_state());
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn notification_with_valid_session_returns_202() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();
        let app = build_router(state);

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header(SESSION_ID_HEADER, &session_id)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn initialize_response_contains_server_info() {
        let state = test_state();
        let app = build_router(state);

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-05",
                "capabilities": {},
                "clientInfo": { "name": "info-test", "version": "1.0" }
            }
        });
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp_body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(json["result"]["serverInfo"]["name"], "plug");
        assert!(json["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn origin_localhost_subdomain_rejected() {
        let app = build_router(test_state());

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("origin", "http://localhost.evil.com")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn origin_localhost_accepted() {
        let state = test_state();
        let app = build_router(state);

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0"
                }
            }
        });

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("origin", "http://localhost:3282")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
