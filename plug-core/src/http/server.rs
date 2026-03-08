use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, extract::Request};
use dashmap::DashMap;
use rmcp::ErrorData as McpError;
use rmcp::model::*;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tower_http::limit::RequestBodyLimitLayer;

use super::error::HttpError;
use super::sse::sse_stream;
use crate::notifications::{NotificationTarget, ProtocolNotification};
use crate::proxy::{DownstreamCallContext, ToolRouter};
use crate::session::SessionStore;

/// rmcp header constant for session ID.
const SESSION_ID_HEADER: &str = "Mcp-Session-Id";

/// MCP protocol version header name.
const PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";

/// The MCP protocol version we implement.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// Shared state for all HTTP handlers.
pub struct HttpState {
    pub router: Arc<ToolRouter>,
    pub sessions: Arc<dyn SessionStore>,
    pub cancel: CancellationToken,
    pub sse_channel_capacity: usize,
    pub notification_task_started: AtomicBool,
    /// Bearer token for downstream client authentication.
    /// `None` means no auth required (loopback-only server).
    pub auth_token: Option<Arc<str>>,
    /// Sessions whose clients advertise `roots` capability.
    pub roots_capable_sessions: DashMap<String, ()>,
    /// Pending reverse requests sent to HTTP clients (keyed by session_id + request_id).
    pub pending_client_requests: DashMap<(String, i64), oneshot::Sender<ClientResult>>,
    /// Counter for generating unique reverse-request IDs.
    pub reverse_request_counter: AtomicU64,
}

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
                                match notification {
                                    ProtocolNotification::ToolListChanged => {
                                        state.sessions.broadcast(
                                            ProtocolNotification::ToolListChanged.to_json_value(),
                                        );
                                    }
                                    ProtocolNotification::ResourceListChanged => {
                                        state.sessions.broadcast(
                                            ProtocolNotification::ResourceListChanged
                                                .to_json_value(),
                                        );
                                    }
                                    ProtocolNotification::PromptListChanged => {
                                        state.sessions.broadcast(
                                            ProtocolNotification::PromptListChanged.to_json_value(),
                                        );
                                    }
                                    ProtocolNotification::Progress { target, params } => {
                                        if let NotificationTarget::Http { session_id } = target {
                                            let session_key = session_id.to_string();
                                            state.sessions.send_to_session(
                                                &session_key,
                                                ProtocolNotification::Progress {
                                                    target: NotificationTarget::Http {
                                                        session_id,
                                                    },
                                                    params,
                                                }
                                                .to_json_value(),
                                            );
                                        }
                                    }
                                    ProtocolNotification::Cancelled { target, params } => {
                                        if let NotificationTarget::Http { session_id } = target {
                                            let session_key = session_id.to_string();
                                            state.sessions.send_to_session(
                                                &session_key,
                                                ProtocolNotification::Cancelled {
                                                    target: NotificationTarget::Http {
                                                        session_id,
                                                    },
                                                    params,
                                                }
                                                .to_json_value(),
                                            );
                                        }
                                    }
                                    ProtocolNotification::ResourceUpdated { target, params } => {
                                        if let NotificationTarget::Http { session_id } = target {
                                            let session_key = session_id.to_string();
                                            state.sessions.send_to_session(
                                                &session_key,
                                                ProtocolNotification::ResourceUpdated {
                                                    target: NotificationTarget::Http {
                                                        session_id,
                                                    },
                                                    params,
                                                }
                                                .to_json_value(),
                                            );
                                        }
                                    }
                                    ProtocolNotification::LoggingMessage { .. } => {
                                        // Logging is handled by the dedicated logging fan-out task below
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "HTTP notification fan-out lagged");
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
                            Ok(ref notif @ ProtocolNotification::LoggingMessage { .. }) => {
                                log_state.sessions.broadcast(notif.to_json_value());
                            }
                            Ok(_) => {} // non-logging notifications on wrong channel
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "HTTP logging fan-out lagged");
                                // Emit synthetic warning to all connected clients
                                let synthetic = ProtocolNotification::LoggingMessage {
                                    params: rmcp::model::LoggingMessageNotificationParam {
                                        level: rmcp::model::LoggingLevel::Warning,
                                        logger: Some("plug".to_string()),
                                        data: serde_json::json!(format!(
                                            "skipped {skipped} log messages"
                                        )),
                                    },
                                };
                                log_state.sessions.broadcast(synthetic.to_json_value());
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
    }
}

/// Build the axum Router with all middleware and handlers.
pub fn build_router(state: Arc<HttpState>) -> Router {
    state.spawn_notification_fanout();

    // Discovery endpoint — exempt from origin validation
    let discovery = Router::new()
        .route("/.well-known/mcp.json", get(get_server_card))
        .with_state(state.clone());

    // MCP protocol routes — protected by auth + origin validation middleware
    // Layer order (innermost first): origin validation → bearer auth → body limit
    // Bearer auth runs first; if authenticated, origin validation is skipped.
    let mcp = Router::new()
        .route("/mcp", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .layer(middleware::from_fn(validate_origin))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            validate_bearer_auth,
        ))
        .layer(RequestBodyLimitLayer::new(4 * 1024 * 1024)) // 4MB DoS prevention
        .with_state(state);

    discovery.merge(mcp)
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Whether the request has been authenticated via bearer token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthStatus {
    /// Request authenticated with valid bearer token.
    Authenticated,
    /// No auth required (loopback-only server).
    NoAuthRequired,
}

/// Check if a request's bearer token is valid against the expected token.
fn check_bearer_token(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|token| crate::auth::verify_auth_token(token, expected))
}

/// Validate bearer token for non-loopback HTTP servers.
///
/// When `HttpState.auth_token` is `Some`, requests must include a valid
/// `Authorization: Bearer <token>` header. When `None`, all requests pass through.
async fn validate_bearer_auth(
    State(state): State<Arc<HttpState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, HttpError> {
    let auth_status = match &state.auth_token {
        None => AuthStatus::NoAuthRequired,
        Some(expected) => {
            if check_bearer_token(req.headers(), expected.as_ref()) {
                AuthStatus::Authenticated
            } else {
                tracing::warn!("bearer auth failed from downstream client");
                return Err(HttpError::Unauthorized);
            }
        }
    };

    req.extensions_mut().insert(auth_status);
    Ok(next.run(req).await)
}

/// Validate Origin header for DNS rebinding prevention.
///
/// - Missing Origin: allowed (non-browser MCP clients don't send it)
/// - localhost/127.0.0.1/[::1]: allowed
/// - "null" literal: rejected (DNS rebinding vector)
/// - Anything else: rejected
/// - Authenticated requests (via bearer token): origin check skipped
async fn validate_origin(req: Request, next: Next) -> Result<Response, HttpError> {
    // Skip origin check for authenticated remote clients
    if req.extensions().get::<AuthStatus>() == Some(&AuthStatus::Authenticated) {
        return Ok(next.run(req).await);
    }

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

    validate_protocol_version_for_post(&headers, &message)?;

    // 3. Route based on message type
    match message {
        JsonRpcMessage::Request(req) => handle_request(req, &headers, &state).await,
        JsonRpcMessage::Response(response) => {
            let session_id = extract_session_id(&headers)?;
            validate_session_header(&headers, state.sessions.as_ref())?;
            handle_client_response(response, &session_id, &state).await?;
            Ok(StatusCode::ACCEPTED.into_response())
        }
        JsonRpcMessage::Notification(notification) => {
            let session_id = extract_session_id(&headers)?;
            validate_session_header(&headers, state.sessions.as_ref())?;
            match notification.notification {
                ClientNotification::CancelledNotification(cancelled) => {
                    state.router.forward_cancel_from_downstream(
                        &DownstreamCallContext::http(
                            Arc::<str>::from(session_id.as_str()),
                            cancelled.params.request_id.clone(),
                        ),
                        cancelled.params.reason,
                    );
                }
                ClientNotification::InitializedNotification(_) => {
                    maybe_request_http_roots(Arc::clone(&state), session_id.clone());
                }
                ClientNotification::RootsListChangedNotification(_) => {
                    maybe_request_http_roots(Arc::clone(&state), session_id.clone());
                }
                _ => {}
            }
            Ok(StatusCode::ACCEPTED.into_response())
        }
        JsonRpcMessage::Error(_) => Err(HttpError::BadRequest(
            "unexpected error message from client".into(),
        )),
    }
}

fn validate_protocol_version_for_post(
    headers: &HeaderMap,
    message: &ClientJsonRpcMessage,
) -> Result<(), HttpError> {
    let require_header = !matches!(
        message,
        JsonRpcMessage::Request(req)
            if matches!(req.request, ClientRequest::InitializeRequest(_))
    );

    match headers.get(PROTOCOL_VERSION_HEADER) {
        Some(value) => {
            let version = value
                .to_str()
                .map_err(|_| HttpError::BadRequest("invalid MCP-Protocol-Version header".into()))?;
            if version != PROTOCOL_VERSION {
                return Err(HttpError::UnsupportedProtocolVersion(version.to_string()));
            }
            Ok(())
        }
        None if require_header => Err(HttpError::MissingProtocolVersion),
        None => Ok(()),
    }
}

/// Send a reverse JSON-RPC request to an HTTP client via its SSE stream and
/// await the response posted back via POST.
async fn send_http_client_request(
    state: &HttpState,
    session_id: &str,
    request: ServerRequest,
) -> Result<ClientResult, McpError> {
    let id = state.reverse_request_counter.fetch_add(1, Ordering::SeqCst) as i64;
    let request_id = RequestId::from(NumberOrString::Number(id));
    let (tx, rx) = oneshot::channel();
    state
        .pending_client_requests
        .insert((session_id.to_string(), id), tx);
    let message = ServerJsonRpcMessage::request(request, request_id);
    state.sessions.send_to_session(
        session_id,
        serde_json::to_value(message).map_err(|e| McpError::internal_error(e.to_string(), None))?,
    );
    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(_)) => Err(McpError::internal_error(
            "HTTP client response channel closed".to_string(),
            None,
        )),
        Err(_) => {
            // Clean up the pending request on timeout
            state
                .pending_client_requests
                .remove(&(session_id.to_string(), id));
            Err(McpError::internal_error(
                "HTTP client request timed out".to_string(),
                None,
            ))
        }
    }
}

/// Handle a client response (POST) that is the answer to a reverse request
/// we sent via SSE (e.g. roots/list).
async fn handle_client_response(
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
        let _ = tx.send(response.result);
    }

    Ok(())
}

/// If the session supports roots, spawn a task to request roots via SSE
/// and cache the result.
fn maybe_request_http_roots(state: Arc<HttpState>, session_id: String) {
    if !state.roots_capable_sessions.contains_key(&session_id) {
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

/// DELETE /mcp — terminate a session.
async fn delete_mcp(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let session_id = extract_session_id(&headers)?;

    if state.sessions.remove(&session_id) {
        // Clean up resource subscriptions for this departing session
        let target = NotificationTarget::Http {
            session_id: Arc::from(session_id.as_str()),
        };
        state.router.cleanup_subscriptions_for_target(&target).await;
        state.roots_capable_sessions.remove(&session_id);
        state
            .pending_client_requests
            .retain(|(pending_session_id, _), _| pending_session_id != &session_id);
        if state.router.clear_roots_for_target(&target) {
            state.router.forward_roots_list_changed_to_upstreams().await;
        }
        // Clean up per-client log level to prevent stale entries from
        // keeping the effective level permanently at a permissive value.
        state.router.remove_client_log_level(&session_id);
        tracing::info!(session_id = %session_id, "session terminated via DELETE");
        Ok(StatusCode::OK.into_response())
    } else {
        Err(HttpError::SessionNotFound)
    }
}

/// GET /.well-known/mcp.json — server discovery card.
///
/// When auth is required but not provided, returns a minimal card (name,
/// version, endpoint) to preserve discoverability without leaking server
/// inventory details (server names, tool counts).
async fn get_server_card(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Check if this is an authenticated request (manual check since discovery
    // is on a separate router without the auth middleware layer)
    let is_authenticated = match &state.auth_token {
        None => true, // No auth required (loopback)
        Some(expected) => check_bearer_token(&headers, expected.as_ref()),
    };

    let card = if is_authenticated {
        let tool_count = state.router.tool_count();
        let servers: Vec<String> = state
            .router
            .server_manager()
            .server_statuses()
            .into_iter()
            .map(|s| s.server_id)
            .collect();

        json!({
            "name": "plug",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "MCP multiplexer",
            "tools": tool_count,
            "servers": servers,
            "transports": ["stdio", "streamable-http", "sse"],
        })
    } else {
        // Minimal card: no server names, no tool counts
        json!({
            "name": "plug",
            "version": env!("CARGO_PKG_VERSION"),
            "endpoint": "/mcp",
            "transport": "streamable-http",
            "auth_required": true,
        })
    };

    let mut response = (StatusCode::OK, Json(card)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=60"),
    );
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

// ---------------------------------------------------------------------------
// Request routing
// ---------------------------------------------------------------------------

/// Route a typed JSON-RPC request to the appropriate handler.
async fn handle_request(
    req: JsonRpcRequest<ClientRequest>,
    headers: &HeaderMap,
    state: &Arc<HttpState>,
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
            // Store client type in session
            let _ = state.sessions.set_client_type(&session_id, client_type);

            // Track roots capability for reverse-request roots fetching
            if init_req.params.capabilities.roots.is_some() {
                state.roots_capable_sessions.insert(session_id.clone(), ());
            }

            let result = build_initialize_result(state.router.as_ref());

            let response_msg =
                ServerJsonRpcMessage::response(ServerResult::InitializeResult(result), request_id);

            json_response_with_session(&session_id, &response_msg)
        }

        ClientRequest::PingRequest(_) => {
            validate_session_header(headers, state.sessions.as_ref())?;
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::EmptyResult(EmptyResult {}),
                request_id,
            );
            json_response(&response_msg)
        }

        ClientRequest::ListToolsRequest(list_req) => {
            let session_id_str = extract_session_id(headers)?;
            validate_session_header(headers, state.sessions.as_ref())?;
            let client_type = state
                .sessions
                .get_client_type(&session_id_str)
                .unwrap_or(crate::types::ClientType::Unknown);
            let result = state
                .router
                .list_tools_page_for_client(client_type, list_req.params);
            let response_msg =
                ServerJsonRpcMessage::response(ServerResult::ListToolsResult(result), request_id);
            json_response(&response_msg)
        }

        ClientRequest::CallToolRequest(call_req) => {
            let session_id = extract_session_id(headers)?;
            validate_session_header(headers, state.sessions.as_ref())?;
            let progress_token = call_req.params.progress_token();
            match state
                .router
                .call_tool_with_context(
                    call_req.params.name.as_ref(),
                    call_req.params.arguments,
                    progress_token,
                    Some(DownstreamCallContext::http(
                        Arc::<str>::from(session_id.as_str()),
                        request_id.clone(),
                    )),
                )
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

        ClientRequest::ListResourcesRequest(list_req) => {
            validate_session_header(headers, state.sessions.as_ref())?;
            let result = state.router.list_resources_page(list_req.params);
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::ListResourcesResult(result),
                request_id,
            );
            json_response(&response_msg)
        }

        ClientRequest::ListResourceTemplatesRequest(list_req) => {
            validate_session_header(headers, state.sessions.as_ref())?;
            let result = state.router.list_resource_templates_page(list_req.params);
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::ListResourceTemplatesResult(result),
                request_id,
            );
            json_response(&response_msg)
        }

        ClientRequest::ReadResourceRequest(read_req) => {
            validate_session_header(headers, state.sessions.as_ref())?;
            match state.router.read_resource(&read_req.params.uri).await {
                Ok(result) => {
                    let response_msg = ServerJsonRpcMessage::response(
                        ServerResult::ReadResourceResult(result),
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

        ClientRequest::ListPromptsRequest(list_req) => {
            validate_session_header(headers, state.sessions.as_ref())?;
            let result = state.router.list_prompts_page(list_req.params);
            let response_msg =
                ServerJsonRpcMessage::response(ServerResult::ListPromptsResult(result), request_id);
            json_response(&response_msg)
        }

        ClientRequest::GetPromptRequest(prompt_req) => {
            validate_session_header(headers, state.sessions.as_ref())?;
            match state
                .router
                .get_prompt(&prompt_req.params.name, prompt_req.params.arguments)
                .await
            {
                Ok(result) => {
                    let response_msg = ServerJsonRpcMessage::response(
                        ServerResult::GetPromptResult(result),
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

        ClientRequest::SubscribeRequest(sub_req) => {
            let session_id = extract_session_id(headers)?;
            validate_session_header(headers, state.sessions.as_ref())?;
            let target = NotificationTarget::Http {
                session_id: Arc::from(session_id.as_str()),
            };
            match state
                .router
                .subscribe_resource(&sub_req.params.uri, target)
                .await
            {
                Ok(()) => {
                    let response_msg = ServerJsonRpcMessage::response(
                        ServerResult::EmptyResult(().into()),
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

        ClientRequest::UnsubscribeRequest(unsub_req) => {
            let session_id = extract_session_id(headers)?;
            validate_session_header(headers, state.sessions.as_ref())?;
            let target = NotificationTarget::Http {
                session_id: Arc::from(session_id.as_str()),
            };
            match state
                .router
                .unsubscribe_resource(&unsub_req.params.uri, &target)
                .await
            {
                Ok(()) => {
                    let response_msg = ServerJsonRpcMessage::response(
                        ServerResult::EmptyResult(().into()),
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

        ClientRequest::CompleteRequest(complete_req) => {
            validate_session_header(headers, state.sessions.as_ref())?;
            match state.router.complete_request(complete_req.params).await {
                Ok(result) => {
                    let response_msg = ServerJsonRpcMessage::response(
                        ServerResult::CompleteResult(result),
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

        ClientRequest::SetLevelRequest(set_level_req) => {
            let session_id = extract_session_id(headers)?;
            validate_session_header(headers, state.sessions.as_ref())?;
            tracing::info!(
                session = %session_id,
                level = ?set_level_req.params.level,
                "HTTP client set log level"
            );
            state
                .router
                .set_client_log_level(&session_id, set_level_req.params.level);
            state.router.forward_set_level_to_upstreams().await;
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::EmptyResult(EmptyResult {}),
                request_id,
            );
            json_response(&response_msg)
        }

        _ => {
            // Unsupported method — return JSON-RPC method not found error
            validate_session_header(headers, state.sessions.as_ref())?;
            let error = ErrorData::new(ErrorCode::METHOD_NOT_FOUND, "method not supported", None);
            let response_msg = ServerJsonRpcMessage::error(error, request_id);
            json_response(&response_msg)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build the InitializeResult (same as ProxyHandler::get_info).
fn build_initialize_result(router: &ToolRouter) -> InitializeResult {
    InitializeResult::new(router.synthesized_capabilities())
        .with_server_info(Implementation::new("plug", env!("CARGO_PKG_VERSION")))
}

/// Extract the host from an Origin header value.
///
/// Origin format is `scheme://host[:port]`. We parse manually to avoid
/// pulling in the `url` crate dependency for this single use case.
fn extract_origin_host(origin: &str) -> Option<&str> {
    let after_scheme = origin.split("://").nth(1)?;
    // Handle IPv6 bracket notation: [::1]:port
    if after_scheme.starts_with('[') {
        let end = after_scheme.find(']')?;
        let host = &after_scheme[..=end]; // includes brackets
        return if host.len() > 2 { Some(host) } else { None };
    }
    // Strip port and path: take everything before ':' or '/'
    let host = after_scheme
        .split(':')
        .next()
        .unwrap_or(after_scheme)
        .split('/')
        .next()
        .unwrap_or(after_scheme);
    if host.is_empty() { None } else { Some(host) }
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
    sessions: &dyn SessionStore,
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
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

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
    use std::time::Duration;
    use tower::ServiceExt;

    async fn collect_sse_events(body: Body, max_events: usize) -> Vec<String> {
        let mut events = Vec::new();
        let mut stream = body.into_data_stream();
        use futures::StreamExt;

        let timeout = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(Ok(chunk)) = stream.next().await {
                let text = String::from_utf8_lossy(&chunk).to_string();
                for part in text.split("\n\n") {
                    let trimmed = part.trim();
                    if !trimmed.is_empty() {
                        events.push(trimmed.to_string());
                    }
                }
                if events.len() >= max_events {
                    break;
                }
            }
        });

        let _ = timeout.await;
        events
    }

    fn test_state_with_router_config(router_config: crate::proxy::RouterConfig) -> Arc<HttpState> {
        let sm = Arc::new(crate::server::ServerManager::new());
        let router = Arc::new(ToolRouter::new(sm, router_config));
        Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            sse_channel_capacity: 32,
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
        })
    }

    fn test_state() -> Arc<HttpState> {
        test_state_with_router_config(crate::proxy::RouterConfig {
            prefix_delimiter: "__".to_string(),
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            meta_tool_mode: false,
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
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
        assert_eq!(
            resp.headers()
                .get(PROTOCOL_VERSION_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(PROTOCOL_VERSION)
        );
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
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION)
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
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION)
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
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn http_tools_list_uses_meta_tool_mode_surface() {
        let state = test_state_with_router_config(crate::proxy::RouterConfig {
            prefix_delimiter: "__".to_string(),
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            meta_tool_mode: true,
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        });

        state.router.replace_snapshot(crate::proxy::RouterSnapshot {
            routes: std::collections::HashMap::from([(
                "Git__commit".to_string(),
                ("git".to_string(), "commit".to_string()),
            )]),
            tools_all: Arc::new(vec![Tool::new(
                std::borrow::Cow::Borrowed("Git__commit"),
                std::borrow::Cow::Borrowed("Create a git commit"),
                Arc::new(serde_json::Map::new()),
            )]),
            meta_tools_all: Arc::new(vec![
                Tool::new(
                    std::borrow::Cow::Borrowed("plug__list_servers"),
                    std::borrow::Cow::Borrowed("List servers"),
                    Arc::new(serde_json::Map::new()),
                ),
                Tool::new(
                    std::borrow::Cow::Borrowed("plug__list_tools"),
                    std::borrow::Cow::Borrowed("List tools"),
                    Arc::new(serde_json::Map::new()),
                ),
                Tool::new(
                    std::borrow::Cow::Borrowed("plug__search_tools"),
                    std::borrow::Cow::Borrowed("Search tools"),
                    Arc::new(serde_json::Map::new()),
                ),
                Tool::new(
                    std::borrow::Cow::Borrowed("plug__invoke_tool"),
                    std::borrow::Cow::Borrowed("Invoke tool"),
                    Arc::new(serde_json::Map::new()),
                ),
            ]),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: std::collections::HashMap::new(),
            prompt_routes: std::collections::HashMap::new(),
            tool_definition_fingerprints: std::collections::HashMap::new(),
        });

        let app = build_router(state.clone());
        let session_id = state.sessions.create_session().unwrap();
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
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let names = json["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "plug__list_servers",
                "plug__list_tools",
                "plug__search_tools",
                "plug__invoke_tool",
            ]
        );
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
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION)
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
        assert!(json["result"]["capabilities"]["tools"].is_null());
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
    async fn server_card_returns_json() {
        let app = build_router(test_state());

        let req = HttpRequest::builder()
            .method("GET")
            .uri("/.well-known/mcp.json")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("max-age=60")
        );
        assert_eq!(
            resp.headers()
                .get("X-Content-Type-Options")
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "plug");
        assert_eq!(json["description"], "MCP multiplexer");
        assert!(json["version"].is_string());
        assert!(json["tools"].is_number());
        assert!(json["servers"].is_array());
        assert_eq!(
            json["transports"],
            serde_json::json!(["stdio", "streamable-http", "sse"])
        );
    }

    #[tokio::test]
    async fn server_card_accessible_with_external_origin() {
        // Discovery endpoint must NOT be blocked by origin validation
        let app = build_router(test_state());

        let req = HttpRequest::builder()
            .method("GET")
            .uri("/.well-known/mcp.json")
            .header("origin", "https://evil.example.com")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
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

    #[tokio::test]
    async fn origin_ipv6_localhost_accepted() {
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
            .header("origin", "http://[::1]:3282")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
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

    // -- Bearer auth middleware tests --

    fn test_state_with_auth(token: &str) -> Arc<HttpState> {
        let sm = Arc::new(crate::server::ServerManager::new());
        let router = Arc::new(ToolRouter::new(
            sm,
            crate::proxy::RouterConfig {
                prefix_delimiter: "__".to_string(),
                priority_tools: Vec::new(),
                disabled_tools: Vec::new(),
                tool_description_max_chars: None,
                tool_search_threshold: 50,
                meta_tool_mode: false,
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            sse_channel_capacity: 32,
            notification_task_started: AtomicBool::new(false),
            auth_token: Some(Arc::from(token)),
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
        })
    }

    #[tokio::test]
    async fn auth_required_no_header_returns_401() {
        let state = test_state_with_auth("test_token_abc123");
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(resp.headers().get("WWW-Authenticate").unwrap(), "Bearer");
    }

    #[tokio::test]
    async fn auth_required_invalid_token_returns_401() {
        let state = test_state_with_auth("correct_token");
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer wrong_token")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_required_valid_token_passes_through() {
        let token = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let state = test_state_with_auth(token);
        let app = build_router(state);

        // Valid token should pass auth and reach the handler (which will fail
        // on content type, not on auth — proving auth middleware passed)
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // Should get past auth (not 401) — will hit content type check (415)
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn no_auth_required_loopback_passes_through() {
        // State with auth_token = None (loopback)
        let state = test_state();
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // Should NOT be 401 — should hit content type check instead
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_valid_token_bypasses_origin_check() {
        let token = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let state = test_state_with_auth(token);
        let app = build_router(state);

        // Remote origin with valid bearer token — should bypass origin check
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("Authorization", format!("Bearer {token}"))
            .header("Origin", "https://remote-client.example.com")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // Should NOT be 403 (origin rejected) — should pass through to handler
        assert_ne!(resp.status(), StatusCode::FORBIDDEN);
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn discovery_minimal_card_when_unauth_on_protected_server() {
        let state = test_state_with_auth("secret_token");
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("GET")
            .uri("/.well-known/mcp.json")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 10_000)
            .await
            .unwrap();
        let card: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Minimal card should NOT contain servers or tool count
        assert!(card.get("servers").is_none());
        assert!(card.get("tools").is_none());
        assert_eq!(card["auth_required"], true);
        assert_eq!(card["endpoint"], "/mcp");
    }

    #[tokio::test]
    async fn discovery_full_card_when_authenticated() {
        let token = "secret_token";
        let state = test_state_with_auth(token);
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("GET")
            .uri("/.well-known/mcp.json")
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 10_000)
            .await
            .unwrap();
        let card: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Full card should contain servers and tools
        assert!(card.get("servers").is_some());
        assert!(card.get("tools").is_some());
        assert!(card.get("auth_required").is_none());
    }
}
