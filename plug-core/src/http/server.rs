use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, extract::DefaultBodyLimit, extract::Request};
use dashmap::DashMap;
use rmcp::ErrorData as McpError;
use rmcp::model::*;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tower_http::limit::RequestBodyLimitLayer;

use super::error::HttpError;
use super::sse::sse_stream_with_heartbeat;
use crate::downstream_oauth::{
    AccessTokenClaims, AccessTokenValidation, AuthorizationRequest, ClientRegistrationRequest,
    DownstreamOauthError, resource_scopes,
};
use crate::mcp_http_headers::{
    HEADER_MISMATCH_CODE, HeaderMismatch, inject_trace_context, validate_mirrored_headers,
    validate_required_mirrored_headers,
};
use crate::notifications::{NotificationTarget, ProtocolNotification};
use crate::proxy::{DownstreamBridge, DownstreamCallContext, ToolRouter};
use crate::session::{SessionSendOutcome, SessionStore, SseMessage, SseReplayKey};
use crate::tasks::TaskOwner;

/// rmcp header constant for session ID.
const SESSION_ID_HEADER: &str = "Mcp-Session-Id";

/// MCP protocol version header name.
const PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";
const TRACEPARENT_HEADER: &str = "traceparent";
const PLUG_TRACE_ID_HEADER: &str = "x-plug-trace-id";

/// The MCP protocol version we implement.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// Shared state for all HTTP handlers.
pub struct HttpState {
    pub router: Arc<ToolRouter>,
    pub sessions: Arc<dyn SessionStore>,
    pub cancel: CancellationToken,
    pub auth_mode: crate::config::DownstreamAuthMode,
    pub downstream_oauth: Option<crate::downstream_oauth::DownstreamOauthManager>,
    pub sse_channel_capacity: usize,
    pub allowed_origins: Vec<Arc<str>>,
    pub notification_task_started: AtomicBool,
    /// Bearer token for downstream client authentication.
    /// `None` means the current downstream auth mode does not require bearer auth.
    pub auth_token: Option<Arc<str>>,
    /// Sessions whose clients advertise `roots` capability.
    pub roots_capable_sessions: DashMap<String, ()>,
    /// Pending reverse requests sent to HTTP clients (keyed by session_id + request_id).
    pub pending_client_requests: DashMap<(String, i64), oneshot::Sender<ClientResult>>,
    /// Counter for generating unique reverse-request IDs.
    pub reverse_request_counter: AtomicU64,
    /// Per-session client capabilities for reverse-request gating.
    pub client_capabilities: DashMap<String, ClientCapabilities>,
}

/// HTTP adapter for the shared `tools/call` dispatcher. Carries the per-call
/// session identity, request id, and trace id the dispatcher needs, plus the
/// session store so task creation can re-check the session's liveness.
struct HttpDownstreamContext {
    session_id: Arc<str>,
    request_id: RequestId,
    client_type: crate::types::ClientType,
    trace_id: Arc<str>,
    sessions: Arc<dyn SessionStore>,
    auth_status: AuthStatus,
    oauth_issuer: Option<Arc<str>>,
    protocol_era: crate::protocol::ProtocolEra,
    modern_direction_enabled: bool,
    cancellation: CancellationToken,
    session_bound: bool,
    client_metadata: Option<crate::protocol::ClientMetadata>,
}

fn http_principal(
    auth_status: &AuthStatus,
    oauth_issuer: Option<&str>,
) -> Option<crate::types::PrincipalId> {
    match auth_status {
        AuthStatus::Authenticated(Some(claims)) => {
            Some(crate::types::PrincipalId::downstream_oauth(
                oauth_issuer.unwrap_or("unknown-issuer"),
                claims.client_id.clone(),
                claims.resource.clone(),
            ))
        }
        AuthStatus::Authenticated(None) => Some(crate::types::PrincipalId::configured_credential(
            "downstream-http-bearer",
            0,
        )),
        AuthStatus::NoAuthRequired => None,
    }
}

fn http_task_owner(
    session_id: &str,
    auth_status: &AuthStatus,
    oauth_issuer: Option<&str>,
) -> crate::tasks::TaskOwner {
    http_principal(auth_status, oauth_issuer)
        .map(|principal| crate::tasks::TaskOwner::new(principal.owner_key()))
        .unwrap_or_else(|| crate::proxy::ToolRouter::task_owner_for_http_session(session_id))
}

fn modern_http_call_context(
    state: &HttpState,
    auth_status: &AuthStatus,
    request_id: RequestId,
    trace_id: Arc<str>,
    meta: Option<&RequestMetaObject>,
) -> DownstreamCallContext {
    let client_info = meta.and_then(RequestMetaObject::client_info);
    let oauth_issuer = state
        .downstream_oauth
        .as_ref()
        .map(|manager| manager.base_url());
    let principal = http_principal(auth_status, oauth_issuer);
    let client_id: Arc<str> = principal
        .as_ref()
        .map(|principal| Arc::from(principal.owner_key()))
        .unwrap_or_else(|| {
            client_info
                .as_ref()
                .map(|info| Arc::from(format!("anonymous:{}:{}", info.name, info.version)))
                .unwrap_or_else(|| Arc::from(format!("anonymous:{trace_id}")))
        });
    let mut context = DownstreamCallContext::http_for_client_with_trace(
        client_id,
        request_id,
        client_info
            .as_ref()
            .map(|info| crate::client_detect::detect_client(&info.name))
            .unwrap_or(crate::types::ClientType::Unknown),
        trace_id,
    )
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(state.router.modern_downstream_enabled());
    if let Some(info) = client_info {
        context = context.with_client_metadata(info.name, info.version);
    }
    match (auth_status, principal) {
        (AuthStatus::Authenticated(Some(claims)), Some(principal)) => context
            .with_authorization(principal, claims.scopes.clone())
            .with_principal_lifecycle(claims.principal_lifecycle.clone()),
        (AuthStatus::Authenticated(None), Some(principal)) => {
            context.with_local_principal(principal)
        }
        _ => context,
    }
}

fn legacy_http_policy_context(
    state: &HttpState,
    auth_status: &AuthStatus,
    request_id: RequestId,
    trace_id: Arc<str>,
) -> DownstreamCallContext {
    let context = DownstreamCallContext::http_for_client_with_trace(
        "legacy-http-policy",
        request_id,
        crate::types::ClientType::Unknown,
        trace_id,
    );
    match (
        auth_status,
        http_principal(
            auth_status,
            state
                .downstream_oauth
                .as_ref()
                .map(|manager| manager.base_url()),
        ),
    ) {
        (AuthStatus::Authenticated(Some(claims)), Some(principal)) => context
            // The legacy OAuth contract predates method-family scopes. Keep
            // the verified identity and revocation lifecycle, but retain the
            // legacy all-MCP-method compatibility policy for that principal.
            .with_local_principal(principal)
            .with_principal_lifecycle(claims.principal_lifecycle.clone()),
        (AuthStatus::Authenticated(None), Some(principal)) => {
            context.with_local_principal(principal)
        }
        _ => context,
    }
}

fn projected_modern_capabilities(
    router: &ToolRouter,
    context: &DownstreamCallContext,
) -> ServerCapabilities {
    let source = router.synthesized_capabilities_for_client(context.client_type);
    let mut projected = ServerCapabilities::default();
    if context
        .policy_decision(crate::protocol::MethodFamily::ToolsList)
        .is_allowed()
        && context
            .policy_decision(crate::protocol::MethodFamily::ToolsCall)
            .is_allowed()
    {
        projected.tools = source.tools;
    }
    if context
        .policy_decision(crate::protocol::MethodFamily::ResourcesList)
        .is_allowed()
        && context
            .policy_decision(crate::protocol::MethodFamily::ResourcesRead)
            .is_allowed()
    {
        projected.resources = source.resources;
    }
    if context
        .policy_decision(crate::protocol::MethodFamily::PromptsList)
        .is_allowed()
        && context
            .policy_decision(crate::protocol::MethodFamily::PromptsGet)
            .is_allowed()
    {
        projected.prompts = source.prompts;
    }
    if context
        .policy_decision(crate::protocol::MethodFamily::Completion)
        .is_allowed()
    {
        projected.completions = source.completions;
    }
    crate::protocol::suppress_unimplemented_modern_capabilities(&mut projected);
    if context
        .policy_decision(crate::protocol::MethodFamily::Tasks)
        .is_allowed()
        && router.supports_tasks()
    {
        projected
            .extensions
            .get_or_insert_with(Default::default)
            .insert(
                rmcp::model::TASKS_EXTENSION_ID.to_string(),
                Default::default(),
            );
    }
    projected
}

fn admit_modern_task_operation(
    router: &ToolRouter,
    context: &DownstreamCallContext,
    request_meta: Option<&RequestMetaObject>,
) -> Result<TaskOwner, McpError> {
    if !router.supports_tasks() {
        return Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "tasks extension is not available",
            None,
        ));
    }
    if !request_meta
        .and_then(RequestMetaObject::client_capabilities)
        .is_some_and(|capabilities| capabilities.supports_tasks())
    {
        return Err(McpError::missing_required_client_capability(
            ClientCapabilities::builder().enable_tasks().build(),
        ));
    }
    context.authorize(crate::protocol::MethodFamily::Tasks)?;
    if context
        .principal_lifecycle
        .as_ref()
        .is_some_and(|lifecycle| !lifecycle.is_active())
    {
        return Err(crate::protocol::ProtocolOutcome::AuthorizationRequired
            .into_error(context.protocol_era));
    }
    let principal = context.principal.as_ref().ok_or_else(|| {
        crate::protocol::ProtocolOutcome::AuthorizationRequired.into_error(context.protocol_era)
    })?;
    Ok(TaskOwner::new(principal.owner_key()))
}

impl crate::dispatch::DownstreamContext for HttpDownstreamContext {
    fn downstream_call_context(&self) -> DownstreamCallContext {
        let mut context = DownstreamCallContext::http_for_client_with_trace(
            Arc::clone(&self.session_id),
            self.request_id.clone(),
            self.client_type,
            Arc::clone(&self.trace_id),
        )
        .with_protocol(
            self.protocol_era,
            match self.protocol_era {
                crate::protocol::ProtocolEra::Legacy => crate::protocol::SUPPORTED_PROTOCOL_VERSION,
                crate::protocol::ProtocolEra::Modern => {
                    crate::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION
                }
            },
        )
        .with_modern_direction_enabled(self.modern_direction_enabled)
        .with_lifecycle(None, self.cancellation.clone(), None);
        if let Some(metadata) = &self.client_metadata {
            context = context
                .with_client_metadata(Arc::clone(&metadata.name), Arc::clone(&metadata.version));
        }
        match (
            &self.auth_status,
            http_principal(&self.auth_status, self.oauth_issuer.as_deref()),
        ) {
            (AuthStatus::Authenticated(Some(claims)), Some(principal)) => context
                .with_authorization(principal, claims.scopes.clone())
                .with_principal_lifecycle(claims.principal_lifecycle.clone()),
            (AuthStatus::Authenticated(None), Some(principal)) => {
                context.with_local_principal(principal)
            }
            (AuthStatus::NoAuthRequired, None)
                if self.protocol_era == crate::protocol::ProtocolEra::Legacy =>
            {
                // Legacy loopback sessions retain their long-standing task
                // behavior. The trust comes from the loopback-only listener,
                // not from client-provided metadata; ownership remains scoped
                // to the server-minted session id in `task_owner`.
                context.with_local_principal(crate::types::PrincipalId::configured_credential(
                    "downstream-http-loopback",
                    0,
                ))
            }
            _ => context,
        }
    }

    fn task_owner(&self) -> Result<crate::tasks::TaskOwner, McpError> {
        Ok(http_task_owner(
            &self.session_id,
            &self.auth_status,
            self.oauth_issuer.as_deref(),
        ))
    }

    fn supports_tasks(&self) -> bool {
        true
    }

    /// Session-existence probe for the enqueue path's post-guard liveness
    /// re-check. HTTP teardown (DELETE / idle expiry) removes the session
    /// from this store BEFORE running task cleanup, and session ids are
    /// server-minted UUIDv4 (never reused), so "still in the store" is a
    /// sound "teardown has not started" signal — see the ordering argument
    /// at the check site in `proxy::tasks`.
    fn owner_liveness_probe(&self) -> Option<crate::tasks::OwnerLivenessProbe> {
        if !self.session_bound {
            return None;
        }
        let sessions = Arc::clone(&self.sessions);
        let session_id = Arc::clone(&self.session_id);
        Some(Arc::new(move || sessions.validate(&session_id).is_ok()))
    }
}

struct HttpRequestCancellationGuard {
    router: Arc<ToolRouter>,
    context: DownstreamCallContext,
    armed: bool,
}

impl HttpRequestCancellationGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for HttpRequestCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.context.cancellation.cancel();
            self.router.cancel_downstream_request(
                &self.context,
                Some("downstream HTTP request ended".to_string()),
            );
        }
    }
}

/// HTTP-specific bridge for forwarding reverse requests (elicitation, sampling)
/// to a downstream HTTP client via its SSE stream.
struct HttpBridge {
    state: Arc<HttpState>,
    session_id: Arc<str>,
    capabilities: ClientCapabilities,
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
                Some(Duration::from_secs(600)), // 10-minute upper bound prevents resource leaks
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
                Some(Duration::from_secs(60)), // sampling has bounded timeout
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
                                // classify -> resolve -> per-notification-kind delivery below.
                                // Unlike stdio/daemon (one fan-out task per client, comparing
                                // target to "self"), this task is shared by every HTTP session,
                                // so a `Targeted` resolution is used to look up which session to
                                // route to rather than to answer "is this me?". See
                                // plug-core/src/notifications.rs::fanout.
                                let resolved_target: Option<NotificationTarget> = match crate::notifications::fanout::resolve(
                                    crate::notifications::fanout::classify(&notification),
                                ) {
                                    crate::notifications::fanout::ResolvedDelivery::Broadcast => None,
                                    crate::notifications::fanout::ResolvedDelivery::ToTarget(target) => {
                                        Some(target.clone())
                                    }
                                };
                                match notification {
                                    ProtocolNotification::ToolListChanged => {
                                        if let Some(message) = notification_to_sse_message(
                                            ProtocolNotification::ToolListChanged,
                                        ) {
                                            state.sessions.broadcast(message);
                                        }
                                    }
                                    ProtocolNotification::ToolListChangedFor { .. } => {
                                        if let Some(NotificationTarget::Http { session_id }) = resolved_target {
                                            let session_key = session_id.to_string();
                                            if let Some(message) = notification_to_sse_message(
                                                ProtocolNotification::ToolListChangedFor {
                                                    target: NotificationTarget::Http {
                                                        session_id,
                                                    },
                                                },
                                            ) {
                                                state.sessions.send_to_session(&session_key, message);
                                            }
                                        }
                                    }
                                    ProtocolNotification::ResourceListChanged => {
                                        if let Some(message) = notification_to_sse_message(
                                            ProtocolNotification::ResourceListChanged,
                                        ) {
                                            state.sessions.broadcast(message);
                                        }
                                    }
                                    ProtocolNotification::PromptListChanged => {
                                        if let Some(message) = notification_to_sse_message(
                                            ProtocolNotification::PromptListChanged,
                                        ) {
                                            state.sessions.broadcast(message);
                                        }
                                    }
                                    ProtocolNotification::Progress { params, .. } => {
                                        if let Some(NotificationTarget::Http { session_id }) = resolved_target {
                                            let session_key = session_id.to_string();
                                            if let Some(message) = notification_to_sse_message(
                                                ProtocolNotification::Progress {
                                                    target: NotificationTarget::Http {
                                                        session_id,
                                                    },
                                                    params,
                                                },
                                            ) {
                                                state.sessions.send_to_session(&session_key, message);
                                            }
                                        }
                                    }
                                    ProtocolNotification::Cancelled { params, .. } => {
                                        if let Some(NotificationTarget::Http { session_id }) = resolved_target {
                                            let session_key = session_id.to_string();
                                            if let Some(message) = notification_to_sse_message(
                                                ProtocolNotification::Cancelled {
                                                    target: NotificationTarget::Http {
                                                        session_id,
                                                    },
                                                    params,
                                                },
                                            ) {
                                                state.sessions.send_to_session(&session_key, message);
                                            }
                                        }
                                    }
                                    ProtocolNotification::ResourceUpdated { params, .. } => {
                                        if let Some(NotificationTarget::Http { session_id }) = resolved_target {
                                            let session_key = session_id.to_string();
                                            if let Some(message) = notification_to_sse_message(
                                                ProtocolNotification::ResourceUpdated {
                                                    target: NotificationTarget::Http {
                                                        session_id,
                                                    },
                                                    params,
                                                },
                                            ) {
                                                state.sessions.send_to_session(&session_key, message);
                                            }
                                        }
                                    }
                                    ref notification @ (
                                        ProtocolNotification::LoggingMessage { .. }
                                        | ProtocolNotification::TokenRefreshExchanged { .. }
                                        | ProtocolNotification::AuthStateChanged { .. }
                                    ) => {
                                        if let Some(params) = notification.as_logging_message_params()
                                            && let Some(message) = notification_to_sse_message(
                                                ProtocolNotification::LoggingMessage { params },
                                            ) {
                                                state.sessions.broadcast(message);
                                            }
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!(skipped, "HTTP notification fan-out lagged");
                                if let Some(message) = notification_to_sse_message(
                                    ProtocolNotification::LoggingMessage {
                                        params: ProtocolNotification::control_lagged_logging_params(
                                            skipped,
                                            "http",
                                        ),
                                    },
                                ) {
                                    state.sessions.broadcast(message);
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
                                if let Some(message) = notification_to_sse_message(notif) {
                                    log_state.sessions.broadcast(message);
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
                                if let Some(message) = notification_to_sse_message(synthetic) {
                                    log_state.sessions.broadcast(message);
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

fn notification_to_sse_message(notification: ProtocolNotification) -> Option<SseMessage> {
    SseMessage::from_json_value(notification.to_json_value())
        .map_err(|error| {
            tracing::error!(%error, "failed to serialize SSE notification payload");
        })
        .ok()
}

/// Build the axum Router with all middleware and handlers.
pub fn build_router(state: Arc<HttpState>) -> Router {
    state.spawn_notification_fanout();

    // Discovery endpoint — exempt from origin validation
    let discovery = Router::new()
        .route("/.well-known/mcp.json", get(get_server_card))
        .route("/.well-known/mcp-server-card", get(get_server_card))
        .route(
            "/.well-known/oauth-authorization-server",
            get(get_oauth_authorization_server_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(get_oauth_protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(get_oauth_protected_resource_metadata),
        )
        .route("/oauth/register", post(oauth_register))
        .route("/oauth/authorize", get(oauth_authorize))
        .route("/_plug/oauth/authorize", post(oauth_authorize_decision))
        .route("/oauth/token", post(oauth_token))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state.clone());

    // MCP protocol routes — protected by auth + origin validation middleware
    // Layer order (innermost first): origin validation → bearer auth → body limit
    // Bearer auth runs first; if authenticated, origin validation is skipped.
    let mcp = Router::new()
        .route("/mcp", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            validate_origin,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            validate_bearer_auth,
        ))
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(4 * 1024 * 1024)) // 4MB DoS prevention
        .with_state(state);

    discovery.merge(mcp)
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Whether the request has been authenticated via bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthStatus {
    /// Request authenticated with valid bearer token.
    Authenticated(Option<AccessTokenClaims>),
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

#[derive(Debug, serde::Deserialize)]
struct OAuthAuthorizeParams {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    code_challenge_method: String,
    scope: Option<String>,
    resource: String,
}

#[derive(Debug, serde::Deserialize)]
struct OAuthConsentDecision {
    consent_id: String,
    decision: String,
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
    if state.auth_mode == crate::config::DownstreamAuthMode::Oauth {
        let manager = state
            .downstream_oauth
            .as_ref()
            .ok_or_else(|| HttpError::Internal("missing downstream OAuth manager".to_string()))?;
        let metadata_url = protected_resource_metadata_url(manager.base_url());
        let advertised_scopes = resource_scopes(&manager.config.oauth_scopes);
        let scope = (!advertised_scopes.is_empty()).then(|| advertised_scopes.join(" "));

        let auth_status = if let Some(auth_header) = req.headers().get(header::AUTHORIZATION) {
            let auth = auth_header
                .to_str()
                .map_err(|_| HttpError::UnauthorizedWithMetadata {
                    metadata_url: metadata_url.clone(),
                    scope: scope.clone(),
                })?
                .strip_prefix("Bearer ")
                .ok_or(HttpError::UnauthorizedWithMetadata {
                    metadata_url: metadata_url.clone(),
                    scope: scope.clone(),
                })?;
            match manager
                .validate_access_token_for(auth, &[], &manager.resource())
                .await
            {
                AccessTokenValidation::Valid(claims) => AuthStatus::Authenticated(Some(claims)),
                AccessTokenValidation::InsufficientScope => {
                    return Err(HttpError::InsufficientScopeWithMetadata {
                        metadata_url,
                        scope: scope.unwrap_or_else(|| "tools:read".to_string()),
                    });
                }
                AccessTokenValidation::Invalid => {
                    return Err(HttpError::UnauthorizedWithMetadata {
                        metadata_url,
                        scope,
                    });
                }
            }
        } else {
            return Err(HttpError::UnauthorizedWithMetadata {
                metadata_url,
                scope,
            });
        };

        req.extensions_mut().insert(auth_status);
        return Ok(next.run(req).await);
    }

    let auth_status = match &state.auth_token {
        None => AuthStatus::NoAuthRequired,
        Some(expected) => {
            if check_bearer_token(req.headers(), expected.as_ref()) {
                AuthStatus::Authenticated(None)
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
async fn validate_origin(
    State(state): State<Arc<HttpState>>,
    req: Request,
    next: Next,
) -> Result<Response, HttpError> {
    // Skip origin check for authenticated remote clients
    if matches!(
        req.extensions().get::<AuthStatus>(),
        Some(AuthStatus::Authenticated(_))
    ) {
        return Ok(next.run(req).await);
    }

    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let origin = origin.to_str().map_err(|_| HttpError::InvalidOrigin)?;

        if state
            .allowed_origins
            .iter()
            .any(|allowed| allowed.as_ref() == origin)
        {
            return Ok(next.run(req).await);
        }

        if origin == "null" {
            return Err(HttpError::InvalidOrigin);
        }

        // Parse origin to extract host — prevents bypass via localhost.evil.com
        let is_local =
            extract_origin_host(origin).is_some_and(crate::config::http_bind_is_loopback);

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
    axum::Extension(auth_status): axum::Extension<AuthStatus>,
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
    let mut raw_message: serde_json::Value = serde_json::from_slice(&body).map_err(|e| {
        tracing::debug!(error = %e, "invalid JSON-RPC message from client");
        HttpError::MalformedJsonRpc {
            code: -32700,
            message: "Parse error",
        }
    })?;
    let header_version = headers
        .get(PROTOCOL_VERSION_HEADER)
        .and_then(|value| value.to_str().ok());
    let era = crate::protocol::classify_http_request_era(&raw_message, header_version)
        .map_err(HttpError::BadRequest)?;
    inject_trace_context(&headers, &mut raw_message)
        .map_err(|error| HttpError::BadRequest(error.message))?;
    if era == crate::protocol::ProtocolEra::Modern {
        if !state.router.modern_downstream_enabled() {
            return Err(HttpError::UnsupportedProtocolVersion(
                crate::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION.to_string(),
            ));
        }
        if raw_message.get("id").is_some() {
            validate_modern_request_metadata(&raw_message)?;
        }
        validate_modern_host(
            &headers,
            &state.allowed_origins,
            state
                .downstream_oauth
                .as_ref()
                .map(crate::downstream_oauth::DownstreamOauthManager::base_url),
        )?;
        validate_modern_origin(&headers, &state.allowed_origins)?;
        if headers.contains_key(SESSION_ID_HEADER) {
            return Err(HttpError::BadRequest(
                "modern requests must not use Mcp-Session-Id".to_string(),
            ));
        }
    } else {
        crate::protocol::rewrite_legacy_request(&mut raw_message);
    }
    let message: ClientJsonRpcMessage = serde_json::from_value(raw_message).map_err(|e| {
        tracing::debug!(error = %e, "invalid JSON-RPC message from client");
        HttpError::MalformedJsonRpc {
            code: -32600,
            message: "Invalid Request",
        }
    })?;

    validate_protocol_version_for_post(&headers, &message, era)?;
    let mirrored = if era == crate::protocol::ProtocolEra::Modern {
        validate_required_mirrored_headers(&headers, &message)
    } else {
        validate_mirrored_headers(&headers, &message)
    };
    if let Err(err) = mirrored {
        return Ok(header_mismatch_response(&message, err, era));
    }
    let trace_id = Arc::<str>::from(extract_trace_id(&headers));

    // 3. Route based on message type
    match message {
        JsonRpcMessage::Request(req) => {
            handle_request(req, &headers, &state, trace_id, auth_status, era).await
        }
        JsonRpcMessage::Response(response) if era == crate::protocol::ProtocolEra::Legacy => {
            let session_id = extract_session_id(&headers)?;
            validate_session_header(&headers, state.sessions.as_ref())?;
            handle_client_response(response, &session_id, &state).await?;
            Ok(StatusCode::ACCEPTED.into_response())
        }
        JsonRpcMessage::Notification(notification)
            if era == crate::protocol::ProtocolEra::Modern =>
        {
            match notification.notification {
                ClientNotification::CancelledNotification(cancelled) => {
                    let oauth_issuer = state
                        .downstream_oauth
                        .as_ref()
                        .map(|manager| manager.base_url());
                    if http_principal(&auth_status, oauth_issuer).is_some()
                        && let Some(request_id) = cancelled.params.request_id.clone()
                    {
                        let context = modern_http_call_context(
                            &state,
                            &auth_status,
                            request_id,
                            trace_id,
                            None,
                        );
                        state
                            .router
                            .cancel_downstream_request(&context, cancelled.params.reason);
                    }
                    Ok(StatusCode::ACCEPTED.into_response())
                }
                _ => Err(HttpError::BadRequest(
                    "modern HTTP notification is not supported".into(),
                )),
            }
        }
        JsonRpcMessage::Notification(notification)
            if era == crate::protocol::ProtocolEra::Legacy =>
        {
            let session_id = extract_session_id(&headers)?;
            validate_session_header(&headers, state.sessions.as_ref())?;
            match notification.notification {
                ClientNotification::CancelledNotification(cancelled) => {
                    if let Some(request_id) = cancelled.params.request_id.clone() {
                        state.router.forward_cancel_from_downstream(
                            &DownstreamCallContext::http_for_client_with_trace(
                                Arc::<str>::from(session_id.as_str()),
                                request_id,
                                crate::types::ClientType::Unknown,
                                Arc::clone(&trace_id),
                            ),
                            cancelled.params.reason,
                        );
                    }
                }
                ClientNotification::InitializedNotification(_) => {
                    maybe_request_http_roots(Arc::clone(&state), session_id.clone());
                    let caps = state
                        .client_capabilities
                        .get(&session_id)
                        .map(|c| c.clone())
                        .unwrap_or_default();
                    let session_arc = Arc::<str>::from(session_id.as_str());
                    let bridge = Arc::new(HttpBridge {
                        state: Arc::clone(&state),
                        session_id: Arc::clone(&session_arc),
                        capabilities: caps,
                    });
                    state.router.register_downstream_bridge(
                        NotificationTarget::Http {
                            session_id: session_arc,
                        },
                        bridge,
                    );
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
        _ => Err(HttpError::BadRequest(
            "modern HTTP accepts request messages only".into(),
        )),
    }
}

fn validate_modern_host(
    headers: &HeaderMap,
    allowed_origins: &[Arc<str>],
    public_base_url: Option<&str>,
) -> Result<(), HttpError> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            HttpError::BadRequest("modern requests require a valid Host header".into())
        })?;
    let host_name = if host.starts_with('[') {
        host.split(']').next().map(|value| format!("{value}]"))
    } else {
        host.split(':').next().map(str::to_string)
    }
    .ok_or_else(|| HttpError::BadRequest("modern request Host is malformed".into()))?;
    let local = crate::config::http_bind_is_loopback(&host_name);
    let explicitly_allowed = allowed_origins.iter().any(|origin| {
        extract_origin_host(origin)
            .is_some_and(|allowed_host| allowed_host.eq_ignore_ascii_case(&host_name))
    }) || public_base_url.is_some_and(|base_url| {
        extract_origin_host(base_url)
            .is_some_and(|public_host| public_host.eq_ignore_ascii_case(&host_name))
    });
    if !local && !explicitly_allowed {
        return Err(HttpError::BadRequest(
            "modern request Host is not allowed".into(),
        ));
    }
    Ok(())
}

fn validate_modern_origin(
    headers: &HeaderMap,
    allowed_origins: &[Arc<str>],
) -> Result<(), HttpError> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let origin = origin.to_str().map_err(|_| HttpError::InvalidOrigin)?;
    if origin == "null" {
        return Err(HttpError::InvalidOrigin);
    }
    if allowed_origins
        .iter()
        .any(|allowed| allowed.as_ref() == origin)
    {
        return Ok(());
    }
    if extract_origin_host(origin).is_some_and(crate::config::http_bind_is_loopback) {
        Ok(())
    } else {
        Err(HttpError::InvalidOrigin)
    }
}

fn header_mismatch_response(
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
            HeaderValue::from_static(crate::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION),
        );
    }
    response
}

fn validate_protocol_version_for_post(
    headers: &HeaderMap,
    message: &ClientJsonRpcMessage,
    era: crate::protocol::ProtocolEra,
) -> Result<(), HttpError> {
    if era == crate::protocol::ProtocolEra::Modern {
        return Ok(());
    }
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

fn validate_modern_request_metadata(value: &serde_json::Value) -> Result<(), HttpError> {
    let meta = value
        .get("params")
        .and_then(|params| params.get("_meta"))
        .cloned()
        .ok_or_else(|| HttpError::BadRequest("modern request metadata is required".into()))?;
    let meta: RequestMetaObject = serde_json::from_value(meta)
        .map_err(|_| HttpError::BadRequest("modern request metadata is malformed".into()))?;
    let missing = meta.missing_required_keys(&ProtocolVersion::V_2026_07_28);
    if !missing.is_empty() {
        return Err(HttpError::BadRequest(format!(
            "modern request metadata is missing: {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

fn extract_trace_id(headers: &HeaderMap) -> String {
    if let Some(trace_id) = headers
        .get(TRACEPARENT_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(trace_id_from_traceparent)
    {
        return trace_id.to_string();
    }

    if let Some(trace_id) = headers
        .get(PLUG_TRACE_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_valid_trace_id(value))
    {
        return trace_id.to_ascii_lowercase();
    }

    crate::proxy::new_trace_id()
}

fn trace_id_from_traceparent(value: &str) -> Option<&str> {
    let mut parts = value.split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let parent_id = parts.next()?;
    let flags = parts.next()?;
    if parts.next().is_some()
        || version.len() != 2
        || parent_id.len() != 16
        || flags.len() != 2
        || !is_valid_trace_id(trace_id)
    {
        return None;
    }
    Some(trace_id)
}

fn is_valid_trace_id(value: &str) -> bool {
    value.len() == 32
        && value != "00000000000000000000000000000000"
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Send a reverse JSON-RPC request to an HTTP client via its SSE stream and
/// await the response posted back via POST.
async fn send_http_client_request(
    state: &HttpState,
    session_id: &str,
    request: ServerRequest,
    timeout: Option<Duration>,
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
    match state.sessions.send_to_live_session(session_id, message) {
        SessionSendOutcome::Delivered | SessionSendOutcome::Queued => {}
        SessionSendOutcome::SessionNotFound => {
            state
                .pending_client_requests
                .remove(&(session_id.to_string(), id));
            return Err(McpError::internal_error(
                "HTTP client SSE stream could not accept reverse request delivery".to_string(),
                None,
            ));
        }
    }
    match timeout {
        Some(duration) => match tokio::time::timeout(duration, rx).await {
            Ok(Ok(result)) => {
                state
                    .sessions
                    .remove_replay_events_by_key(session_id, &SseReplayKey::ReverseRequest(id));
                Ok(result)
            }
            Ok(Err(_)) => Err(McpError::internal_error(
                "HTTP client response channel closed".to_string(),
                None,
            )),
            Err(_) => {
                state
                    .pending_client_requests
                    .remove(&(session_id.to_string(), id));
                state
                    .sessions
                    .remove_replay_events_by_key(session_id, &SseReplayKey::ReverseRequest(id));
                Err(McpError::internal_error(
                    "HTTP client request timed out".to_string(),
                    None,
                ))
            }
        },
        None => match rx.await {
            Ok(result) => {
                state
                    .sessions
                    .remove_replay_events_by_key(session_id, &SseReplayKey::ReverseRequest(id));
                Ok(result)
            }
            Err(_) => Err(McpError::internal_error(
                "HTTP client response channel closed".to_string(),
                None,
            )),
        },
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
        state
            .sessions
            .remove_replay_events_by_key(session_id, &SseReplayKey::ReverseRequest(request_id));
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
            Some(Duration::from_secs(10)),
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
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    state
        .sessions
        .set_sse_sender(&session_id, tx, last_event_id)?;

    // 4. Build SSE response with appropriate headers
    let session_store = Arc::clone(&state.sessions);
    let keepalive_session_id = session_id.clone();
    let sse = sse_stream_with_heartbeat(rx, state.cancel.clone(), move || {
        let _ = session_store.touch(&keepalive_session_id);
    });
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
        state.client_capabilities.remove(&session_id);
        // Drop pending reverse-request senders so receivers get RecvError
        state
            .pending_client_requests
            .retain(|(pending_session_id, _), _| pending_session_id != &session_id);
        state.router.unregister_downstream_bridge(&target);
        if state.router.clear_roots_for_target(&target) {
            state.router.forward_roots_list_changed_to_upstreams().await;
        }
        // Clean up per-client log level to prevent stale entries from
        // keeping the effective level permanently at a permissive value.
        state.router.remove_client_log_level(&session_id);
        let lazy_session_key = crate::proxy::ToolRouter::lazy_session_key(
            crate::proxy::DownstreamTransport::Http,
            &session_id,
        );
        state.router.clear_lazy_session(&lazy_session_key);
        // Session-owned legacy records end with the wire session. Durable
        // principal-owned records intentionally survive and can be resumed
        // by a later session authenticated as the same principal.
        let owner = crate::proxy::ToolRouter::task_owner_for_http_session(&session_id);
        state.router.cleanup_tasks_for_owner(&owner).await;
        tracing::info!(session_id = %session_id, "session terminated via DELETE");
        Ok(StatusCode::OK.into_response())
    } else {
        Err(HttpError::SessionNotFound)
    }
}

/// GET /.well-known/mcp-server-card and legacy /.well-known/mcp.json.
async fn get_server_card(
    State(state): State<Arc<HttpState>>,
    _headers: HeaderMap,
) -> impl IntoResponse {
    let mut remote = json!({
        "type": "streamable-http",
        "url": "/mcp",
        "supportedProtocolVersions": [PROTOCOL_VERSION],
    });
    if state.auth_mode == crate::config::DownstreamAuthMode::Oauth || state.auth_token.is_some() {
        remote["headers"] = json!([
            {
                "name": "Authorization",
                "description": "Bearer access token",
                "isRequired": true,
                "isSecret": true
            }
        ]);
    };

    let card = json!({
        "$schema": "https://static.modelcontextprotocol.io/schemas/v1/server-card.schema.json",
        "name": "io.github.cyberpapiii/plug",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "MCP multiplexer for sharing one server configuration across many AI clients",
        "title": "Plug",
        "websiteUrl": "https://github.com/cyberpapiii/plug",
        "repository": {
            "url": "https://github.com/cyberpapiii/plug",
            "source": "github"
        },
        "remotes": [remote],
    });

    let mut response = (StatusCode::OK, Json(card)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Content-Type"),
    );
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn get_oauth_authorization_server_metadata(
    State(state): State<Arc<HttpState>>,
) -> impl IntoResponse {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let base = manager.base_url();
    let response = Json(json!({
        "issuer": base,
        "authorization_endpoint": manager.authorization_endpoint(),
        "token_endpoint": manager.token_endpoint(),
        "registration_endpoint": manager.registration_endpoint(),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": manager.config.oauth_scopes,
        "client_id_metadata_document_supported": true,
    }));
    (StatusCode::OK, response).into_response()
}

async fn get_oauth_protected_resource_metadata(
    State(state): State<Arc<HttpState>>,
) -> impl IntoResponse {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let base = manager.base_url();
    let response = Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "scopes_supported": resource_scopes(&manager.config.oauth_scopes),
        "bearer_methods_supported": ["header"],
    }));
    (StatusCode::OK, response).into_response()
}

async fn oauth_register(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(request): Json<ClientRegistrationRequest>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let rate_key = registration_rate_key(&headers);
    match manager.register_client(request, &rate_key).await {
        Ok(registration) => (StatusCode::CREATED, Json(registration)).into_response(),
        Err(error) => oauth_error_response(&error),
    }
}

async fn oauth_authorize(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Query(params): Query<OAuthAuthorizeParams>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match manager
        .begin_authorization(AuthorizationRequest {
            response_type: &params.response_type,
            client_id: &params.client_id,
            redirect_uri: &params.redirect_uri,
            state: &params.state,
            code_challenge: &params.code_challenge,
            code_challenge_method: &params.code_challenge_method,
            scope: params.scope.as_deref(),
            resource: &params.resource,
        })
        .await
    {
        Ok(consent) => {
            let scopes = html_escape(&consent.scopes.join(" "));
            let local_consent_endpoint = manager.local_consent_endpoint();
            let html = format!(
                "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Authorize Plug</title></head><body><main><h1>Authorize {}</h1><p><strong>{}</strong> wants access to Plug tools at <strong>{}</strong>.</p><p>Scope: <code>{}</code></p><form method=\"post\" action=\"{}\"><input type=\"hidden\" name=\"consent_id\" value=\"{}\"><button type=\"submit\" name=\"decision\" value=\"approve\">Allow</button><button type=\"submit\" name=\"decision\" value=\"deny\">Deny</button></form><p><small>Approval is submitted directly to Plug on this Mac. Remote requests cannot approve access.</small></p></main></body></html>",
                html_escape(&consent.client_name),
                html_escape(&consent.client_name),
                html_escape(&consent.redirect_host),
                scopes,
                html_escape(&local_consent_endpoint),
                html_escape(&consent.consent_id),
            );
            let mut response = (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                html,
            )
                .into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response.headers_mut().insert(
                "X-Content-Type-Options",
                HeaderValue::from_static("nosniff"),
            );
            if let Ok(csp) = HeaderValue::from_str(&format!(
                "default-src 'none'; form-action {}; base-uri 'none'; frame-ancestors 'none'",
                local_consent_endpoint
            )) {
                response
                    .headers_mut()
                    .insert("Content-Security-Policy", csp);
            }
            response
        }
        Err(error) => {
            if !matches!(
                error,
                DownstreamOauthError::InvalidClient
                    | DownstreamOauthError::InvalidClientMetadata
                    | DownstreamOauthError::InvalidRedirectUri
                    | DownstreamOauthError::MetadataFetch
            ) && manager
                .client_redirect_allowed(&params.client_id, &params.redirect_uri)
                .await
            {
                let location =
                    oauth_authorization_error_redirect(&params.redirect_uri, &params.state, &error);
                if let Ok(location) = HeaderValue::from_str(&location) {
                    let mut response = StatusCode::FOUND.into_response();
                    response.headers_mut().insert(header::LOCATION, location);
                    return response;
                }
            }
            if accepts_html(&headers) {
                oauth_authorization_error_response(&error)
            } else {
                oauth_error_response(&error)
            }
        }
    }
}

async fn oauth_authorize_decision(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Form(decision): Form<OAuthConsentDecision>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !manager.local_approval_request_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let approved = match decision.decision.as_str() {
        "approve" => true,
        "deny" => false,
        _ => return oauth_error_response(&DownstreamOauthError::InvalidAuthorizationRequest),
    };
    match manager.decide_consent(&decision.consent_id, approved).await {
        Ok(redirect) => match HeaderValue::from_str(&redirect.location) {
            Ok(location) => {
                let mut response = StatusCode::FOUND.into_response();
                response.headers_mut().insert(header::LOCATION, location);
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                response
            }
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Err(error) => oauth_error_response(&error),
    }
}

async fn oauth_token(
    State(state): State<Arc<HttpState>>,
    Form(params): Form<HashMap<String, String>>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if params.contains_key("client_secret") {
        return oauth_error_response(&DownstreamOauthError::UnsupportedClientAuthMethod);
    }
    let Some(client_id) = params.get("client_id") else {
        return oauth_error_response(&DownstreamOauthError::InvalidClient);
    };
    let Some(resource) = params.get("resource") else {
        return oauth_error_response(&DownstreamOauthError::InvalidResource);
    };
    let result = match params.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let (Some(code), Some(redirect_uri), Some(code_verifier)) = (
                params.get("code"),
                params.get("redirect_uri"),
                params.get("code_verifier"),
            ) else {
                return oauth_error_response(&DownstreamOauthError::InvalidGrant);
            };
            manager
                .exchange_authorization_code(client_id, code, redirect_uri, code_verifier, resource)
                .await
        }
        Some("refresh_token") => {
            let Some(refresh_token) = params.get("refresh_token") else {
                return oauth_error_response(&DownstreamOauthError::InvalidGrant);
            };
            manager
                .exchange_refresh_token(client_id, refresh_token, resource)
                .await
        }
        _ => Err(DownstreamOauthError::UnsupportedGrantType),
    };

    match result {
        Ok(token) => {
            let mut body = json!({
                "access_token": token.access_token,
                "token_type": "Bearer",
                "expires_in": token.expires_in,
                "scope": token.scope,
            });
            if let Some(refresh_token) = token.refresh_token {
                body["refresh_token"] = json!(refresh_token);
            }
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(error) => oauth_error_response(&error),
    }
}

fn oauth_error_response(error: &DownstreamOauthError) -> Response {
    let (status, code, description) = oauth_public_error(error);
    let mut response = (
        status,
        Json(json!({
            "error": code,
            "error_description": description,
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn oauth_public_error(error: &DownstreamOauthError) -> (StatusCode, &'static str, &'static str) {
    match error {
        DownstreamOauthError::InvalidClient => (
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "Plug could not recognize this client. Try connecting again from your MCP client.",
        ),
        DownstreamOauthError::InvalidClientMetadata | DownstreamOauthError::InvalidRedirectUri => (
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "The client registration details are invalid. Try connecting again from your MCP client.",
        ),
        DownstreamOauthError::InvalidScope => (
            StatusCode::BAD_REQUEST,
            "invalid_scope",
            "The requested permission is not available. Try connecting again from your MCP client.",
        ),
        DownstreamOauthError::AccessDenied => (
            StatusCode::BAD_REQUEST,
            "access_denied",
            "Authorization was not approved. Try connecting again and approve access in Plug.",
        ),
        DownstreamOauthError::InvalidGrant | DownstreamOauthError::PkceVerificationFailed => (
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "The authorization grant is invalid or expired. Try connecting again from your MCP client.",
        ),
        DownstreamOauthError::UnsupportedGrantType => (
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "This OAuth grant type is not supported. Try connecting again from your MCP client.",
        ),
        DownstreamOauthError::UnsupportedClientAuthMethod => (
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "This client authentication method is not supported. Try connecting again without a client secret.",
        ),
        DownstreamOauthError::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "There were too many registration attempts. Wait a moment, then try connecting again.",
        ),
        DownstreamOauthError::RegistrationQuotaExceeded => (
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "The client registration limit was reached. Remove an unused connection, then try connecting again.",
        ),
        DownstreamOauthError::InvalidResource
        | DownstreamOauthError::InvalidAuthorizationRequest
        | DownstreamOauthError::MetadataFetch => (
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The authorization request could not be completed. Try connecting again from your MCP client.",
        ),
        DownstreamOauthError::Persistence(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Plug could not save the authorization state. Try connecting again.",
        ),
    }
}

fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(html_media_range_is_acceptable)
}

fn html_media_range_is_acceptable(value: &str) -> bool {
    let mut parts = value.split(';');
    if !parts
        .next()
        .is_some_and(|media_range| media_range.trim().eq_ignore_ascii_case("text/html"))
    {
        return false;
    }

    let mut quality = 1.0;
    for parameter in parts {
        let Some((name, value)) = parameter.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("q") {
            quality = match value.trim().parse::<f32>() {
                Ok(quality) if (0.0..=1.0).contains(&quality) => quality,
                _ => return false,
            };
        }
    }
    quality > 0.0
}

fn oauth_authorization_error_response(error: &DownstreamOauthError) -> Response {
    let (status, code, description) = oauth_public_error(error);
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Plug authorization failed</title></head><body><main><h1>Plug authorization failed</h1><p><strong>Error: <code>{}</code></strong></p><p>{}</p></main></body></html>",
        html_escape(code),
        html_escape(description),
    );
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "Content-Security-Policy",
        HeaderValue::from_static("default-src 'none'; base-uri 'none'; frame-ancestors 'none'"),
    );
    response
}

fn registration_rate_key(headers: &HeaderMap) -> String {
    headers
        .get("cf-connecting-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .and_then(|value| value.parse::<std::net::IpAddr>().ok())
        .map(|address| address.to_string())
        .unwrap_or_else(|| "local-or-unknown".to_string())
}

fn oauth_authorization_error_redirect(
    redirect_uri: &str,
    state: &str,
    error: &DownstreamOauthError,
) -> String {
    let (_, code, description) = oauth_public_error(error);
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("error", code)
        .append_pair("error_description", description)
        .append_pair("state", state)
        .finish();
    format!(
        "{redirect_uri}{}{query}",
        if redirect_uri.contains('?') { '&' } else { '?' }
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn protected_resource_metadata_url(base_url: &str) -> String {
    format!(
        "{}/.well-known/oauth-protected-resource",
        base_url.trim_end_matches('/')
    )
}

// ---------------------------------------------------------------------------
// Request routing
// ---------------------------------------------------------------------------

/// Route a typed JSON-RPC request to the appropriate handler.
async fn handle_request(
    req: JsonRpcRequest<ClientRequest>,
    headers: &HeaderMap,
    state: &Arc<HttpState>,
    trace_id: Arc<str>,
    auth_status: AuthStatus,
    era: crate::protocol::ProtocolEra,
) -> Result<Response, HttpError> {
    let request_id = req.id.clone();
    let modern = era == crate::protocol::ProtocolEra::Modern;
    let request_meta = modern.then(|| rmcp::model::GetMeta::get_meta(&req.request).clone());
    let policy_context = if modern {
        modern_http_call_context(
            state,
            &auth_status,
            request_id.clone(),
            Arc::clone(&trace_id),
            request_meta.as_ref(),
        )
    } else {
        legacy_http_policy_context(
            state,
            &auth_status,
            request_id.clone(),
            Arc::clone(&trace_id),
        )
    };
    let _modern_request_lease = if modern {
        match state.router.admit_modern_request(&policy_context) {
            Ok(lease) => Some(lease),
            Err(error) => {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(error, Some(request_id)),
                    era,
                );
            }
        }
    } else {
        None
    };

    match req.request {
        ClientRequest::DiscoverRequest(_) if modern => {
            // Reaching this arm means the modern gate is already open (the
            // fail-closed check above rejects `2026-07-28` otherwise), so the
            // list is the full dual-era set — the same one stdio and IPC return
            // from `supported_protocol_versions`. Legacy belongs in it: the same
            // port still serves `2025-11-25` via `initialize`.
            let mut result = DiscoverResult::new(
                crate::protocol::supported_downstream_protocol_versions(true),
                projected_modern_capabilities(state.router.as_ref(), &policy_context),
            );
            result.set_server_info(crate::branding::plug_implementation(env!(
                "CARGO_PKG_VERSION"
            )));
            json_response_for_era(
                &ServerJsonRpcMessage::response(ServerResult::DiscoverResult(result), request_id),
                era,
            )
        }
        ClientRequest::InitializeRequest(init_req) => {
            if modern {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(
                        McpError::method_not_found::<InitializeResultMethod>(),
                        Some(request_id),
                    ),
                    era,
                );
            }
            // Initialize: create session, return server info
            let session_id = state.sessions.create_session()?;

            let client_name = &init_req.params.client_info.name;
            let client_type = crate::client_detect::detect_client(client_name);
            tracing::info!(
                client = %client_name,
                detected = %client_type,
                session = %session_id,
                trace_id = %trace_id,
                requested_protocol = %init_req.params.protocol_version,
                selected_protocol = crate::protocol::SUPPORTED_PROTOCOL_VERSION,
                "HTTP client connected"
            );
            // Store client type in session
            let _ = state.sessions.set_client_type(&session_id, client_type);

            // Track roots capability for reverse-request roots fetching
            if init_req.params.capabilities.roots.is_some() {
                state.roots_capable_sessions.insert(session_id.clone(), ());
            }

            // Store client capabilities for bridge capability gating
            state
                .client_capabilities
                .insert(session_id.clone(), init_req.params.capabilities.clone());

            let result = build_initialize_result(state.router.as_ref(), client_type);

            let response_msg =
                ServerJsonRpcMessage::response(ServerResult::InitializeResult(result), request_id);

            json_response_with_session(&session_id, &response_msg)
        }

        ClientRequest::PingRequest(_) => {
            if modern {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(
                        McpError::method_not_found::<PingRequestMethod>(),
                        Some(request_id),
                    ),
                    era,
                );
            }
            validate_session_header(headers, state.sessions.as_ref())?;
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::EmptyResult(EmptyResult {}),
                request_id,
            );
            json_response(&response_msg)
        }

        ClientRequest::ListToolsRequest(list_req) => {
            if let Err(error) = policy_context.authorize(crate::protocol::MethodFamily::ToolsList) {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(error, Some(request_id)),
                    era,
                );
            }
            let session_id_str = if modern {
                None
            } else {
                let session_id = extract_session_id(headers)?;
                validate_session_header(headers, state.sessions.as_ref())?;
                Some(session_id)
            };
            let client_type = session_id_str
                .as_deref()
                .map(|session_id| {
                    state
                        .sessions
                        .get_client_type(session_id)
                        .unwrap_or(crate::types::ClientType::Unknown)
                })
                .unwrap_or(crate::types::ClientType::Unknown);
            let lazy_session_key = session_id_str.as_deref().map(|session_id| {
                crate::proxy::ToolRouter::lazy_session_key(
                    crate::proxy::DownstreamTransport::Http,
                    session_id,
                )
            });
            let result = state.router.list_tools_page_for_client_session(
                client_type,
                lazy_session_key.as_deref(),
                list_req.params,
            );
            let response_msg =
                ServerJsonRpcMessage::response(ServerResult::ListToolsResult(result), request_id);
            json_response_for_era(&response_msg, era)
        }

        ClientRequest::CallToolRequest(call_req) => {
            if let Err(error) = policy_context.authorize(crate::protocol::MethodFamily::ToolsCall) {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(error, Some(request_id)),
                    era,
                );
            }
            let session_id = if modern {
                policy_context.client_id.to_string()
            } else {
                let session_id = extract_session_id(headers)?;
                validate_session_header(headers, state.sessions.as_ref())?;
                session_id
            };
            let client_type = if modern {
                crate::types::ClientType::Unknown
            } else {
                state
                    .sessions
                    .get_client_type(&session_id)
                    .unwrap_or(crate::types::ClientType::Unknown)
            };
            let cancellation = CancellationToken::new();
            let ctx = HttpDownstreamContext {
                session_id: Arc::<str>::from(session_id.as_str()),
                request_id: request_id.clone(),
                client_type,
                trace_id: Arc::clone(&trace_id),
                sessions: Arc::clone(&state.sessions),
                auth_status,
                oauth_issuer: state
                    .downstream_oauth
                    .as_ref()
                    .map(|manager| Arc::<str>::from(manager.base_url())),
                protocol_era: era,
                modern_direction_enabled: state.router.modern_downstream_enabled(),
                cancellation: cancellation.clone(),
                session_bound: !modern,
                client_metadata: request_meta
                    .as_ref()
                    .and_then(RequestMetaObject::client_info)
                    .map(|info| crate::protocol::ClientMetadata {
                        name: Arc::from(info.name),
                        version: Arc::from(info.version),
                    }),
            };
            let downstream = crate::dispatch::DownstreamContext::downstream_call_context(&ctx);
            let mut cancellation_guard = HttpRequestCancellationGuard {
                router: Arc::clone(&state.router),
                context: downstream,
                armed: modern,
            };
            // RMCP 3.x treats the request envelope's `extensions` as the
            // canonical runtime home for wire-level `params._meta`: its
            // deserializer deliberately removes `_meta` from the typed params
            // and stores a `RequestMetaObject` in `extensions`. The shared
            // dispatcher accepts params alone, so materialize that metadata
            // back onto the params at this adapter boundary. Otherwise both
            // progress tokens and Plug's rewritten legacy `params.task`
            // marker disappear before dispatch, turning task-wrapped calls
            // into synchronous calls.
            let modern_tasks = modern
                && request_meta
                    .as_ref()
                    .and_then(RequestMetaObject::client_capabilities)
                    .is_some_and(|capabilities| capabilities.supports_tasks())
                && state.router.supports_tasks();
            let mut params = call_req.params;
            if let Some(extension_meta) = call_req.extensions.get::<RequestMetaObject>() {
                match params.meta.as_mut() {
                    Some(params_meta) => params_meta.extend(extension_meta.clone()),
                    None => params.meta = Some(extension_meta.clone()),
                }
            }
            if modern_tasks {
                params.meta.get_or_insert_with(Default::default).insert(
                    crate::protocol::LEGACY_TASK_REQUEST_KEY.to_string(),
                    serde_json::json!({}),
                );
            }
            let response_msg =
                match crate::dispatch::dispatch_tools_call(&state.router, &ctx, params).await {
                    Ok(crate::dispatch::ToolCallOutcome::Called(result)) => {
                        ServerJsonRpcMessage::response(
                            ServerResult::CallToolResult(result),
                            request_id,
                        )
                    }
                    Ok(crate::dispatch::ToolCallOutcome::InputRequired(result)) => {
                        ServerJsonRpcMessage::response(
                            ServerResult::InputRequiredResult(result),
                            request_id,
                        )
                    }
                    Ok(crate::dispatch::ToolCallOutcome::TaskCreated(result)) => {
                        let result = if modern_tasks {
                            ServerResult::CreateTaskResult(rmcp::model::CreateTaskResult::new(
                                (&result.task).into(),
                            ))
                        } else {
                            ServerResult::CustomResult(CustomResult::new(
                                serde_json::to_value(result)
                                    .expect("legacy task result serializes"),
                            ))
                        };
                        ServerJsonRpcMessage::response(result, request_id)
                    }
                    Err(mcp_err) => ServerJsonRpcMessage::error(mcp_err, Some(request_id)),
                };
            cancellation_guard.disarm();
            json_response_for_era(&response_msg, era)
        }

        ClientRequest::CustomRequest(custom)
            if !modern && custom.method.starts_with("plug/legacy/tasks/") =>
        {
            let session_id = extract_session_id(headers)?;
            validate_session_header(headers, state.sessions.as_ref())?;
            let client_type = state
                .sessions
                .get_client_type(&session_id)
                .unwrap_or(crate::types::ClientType::Unknown);
            let ctx = HttpDownstreamContext {
                session_id: Arc::<str>::from(session_id.as_str()),
                request_id: request_id.clone(),
                client_type,
                trace_id: Arc::clone(&trace_id),
                sessions: Arc::clone(&state.sessions),
                auth_status,
                oauth_issuer: state
                    .downstream_oauth
                    .as_ref()
                    .map(|manager| Arc::<str>::from(manager.base_url())),
                protocol_era: era,
                modern_direction_enabled: false,
                cancellation: CancellationToken::new(),
                session_bound: true,
                client_metadata: None,
            };
            let downstream = crate::dispatch::DownstreamContext::downstream_call_context(&ctx);
            if let Err(error) = downstream.authorize(crate::protocol::MethodFamily::Tasks) {
                return json_response(&ServerJsonRpcMessage::error(error, Some(request_id)));
            }
            let owner = match crate::dispatch::DownstreamContext::task_owner(&ctx) {
                Ok(owner) => owner,
                Err(error) => {
                    return json_response(&ServerJsonRpcMessage::error(error, Some(request_id)));
                }
            };
            let result = match custom.method.as_str() {
                "plug/legacy/tasks/list" => {
                    let params = custom
                        .params_as::<PaginatedRequestParams>()
                        .map_err(|_| HttpError::BadRequest("invalid task params".into()))?;
                    state
                        .router
                        .list_tasks_for_owner(&owner, params)
                        .await
                        .and_then(|v| {
                            serde_json::to_value(v)
                                .map_err(|e| ErrorData::internal_error(e.to_string(), None))
                        })
                }
                "plug/legacy/tasks/get" => match legacy_task_params(&custom) {
                    Ok(params) => state
                        .router
                        .get_task_info_for_owner(&owner, &params.task_id)
                        .await
                        .and_then(json_task_value),
                    Err(e) => Err(e),
                },
                "plug/legacy/tasks/result" => match legacy_task_params(&custom) {
                    Ok(params) => state
                        .router
                        .get_task_result_for_owner(&owner, &params.task_id)
                        .await
                        .and_then(json_task_value),
                    Err(e) => Err(e),
                },
                "plug/legacy/tasks/cancel" => match legacy_task_params(&custom) {
                    Ok(params) => state
                        .router
                        .cancel_task_for_owner(&owner, &params.task_id)
                        .await
                        .and_then(json_task_value),
                    Err(e) => Err(e),
                },
                _ => Err(ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    "method not supported",
                    None,
                )),
            };
            match result {
                Ok(value) => Ok(axum::Json(
                    serde_json::json!({"jsonrpc":"2.0","id":request_id,"result":value}),
                )
                .into_response()),
                Err(error) => json_response(&ServerJsonRpcMessage::error(error, Some(request_id))),
            }
        }

        ClientRequest::GetTaskRequest(task_req) if modern => {
            let owner = match admit_modern_task_operation(
                state.router.as_ref(),
                &policy_context,
                request_meta.as_ref(),
            ) {
                Ok(owner) => owner,
                Err(error) => {
                    return json_response_for_era(
                        &ServerJsonRpcMessage::error(error, Some(request_id)),
                        era,
                    );
                }
            };
            let response = match state
                .router
                .get_modern_task_for_owner(&owner, &task_req.params.task_id)
                .await
            {
                Ok(result) => {
                    ServerJsonRpcMessage::response(ServerResult::GetTaskResult(result), request_id)
                }
                Err(error) => ServerJsonRpcMessage::error(error, Some(request_id)),
            };
            json_response_for_era(&response, era)
        }

        ClientRequest::CancelTaskRequest(task_req) if modern => {
            let owner = match admit_modern_task_operation(
                state.router.as_ref(),
                &policy_context,
                request_meta.as_ref(),
            ) {
                Ok(owner) => owner,
                Err(error) => {
                    return json_response_for_era(
                        &ServerJsonRpcMessage::error(error, Some(request_id)),
                        era,
                    );
                }
            };
            let response = match state
                .router
                .cancel_task_for_owner(&owner, &task_req.params.task_id)
                .await
            {
                Ok(_) => ServerJsonRpcMessage::response(
                    ServerResult::TaskAckResult(TaskAckResult::new()),
                    request_id,
                ),
                Err(error) => ServerJsonRpcMessage::error(error, Some(request_id)),
            };
            json_response_for_era(&response, era)
        }

        ClientRequest::UpdateTaskRequest(task_req) if modern => {
            let error = match admit_modern_task_operation(
                state.router.as_ref(),
                &policy_context,
                request_meta.as_ref(),
            ) {
                Ok(owner) => match state
                    .router
                    .validate_task_owner(&owner, &task_req.params.task_id)
                    .await
                {
                    Ok(()) => McpError::new(
                        ErrorCode::INVALID_REQUEST,
                        "this task has no outstanding input request",
                        None,
                    ),
                    Err(error) => error,
                },
                Err(error) => error,
            };
            json_response_for_era(&ServerJsonRpcMessage::error(error, Some(request_id)), era)
        }

        ClientRequest::ListResourcesRequest(list_req) => {
            if let Err(error) =
                policy_context.authorize(crate::protocol::MethodFamily::ResourcesList)
            {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(error, Some(request_id)),
                    era,
                );
            }
            if !modern {
                validate_session_header(headers, state.sessions.as_ref())?;
            }
            let result = state.router.list_resources_page(list_req.params);
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::ListResourcesResult(result),
                request_id,
            );
            json_response_for_era(&response_msg, era)
        }

        ClientRequest::ListResourceTemplatesRequest(list_req) => {
            if let Err(error) =
                policy_context.authorize(crate::protocol::MethodFamily::ResourcesList)
            {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(error, Some(request_id)),
                    era,
                );
            }
            if !modern {
                validate_session_header(headers, state.sessions.as_ref())?;
            }
            let result = state.router.list_resource_templates_page(list_req.params);
            let response_msg = ServerJsonRpcMessage::response(
                ServerResult::ListResourceTemplatesResult(result),
                request_id,
            );
            json_response_for_era(&response_msg, era)
        }

        ClientRequest::ReadResourceRequest(read_req) => {
            if let Err(error) =
                policy_context.authorize(crate::protocol::MethodFamily::ResourcesRead)
            {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(error, Some(request_id)),
                    era,
                );
            }
            if !modern {
                validate_session_header(headers, state.sessions.as_ref())?;
            }
            match state.router.read_resource(&read_req.params.uri).await {
                Ok(result) => {
                    let response_msg = ServerJsonRpcMessage::response(
                        ServerResult::ReadResourceResult(result),
                        request_id,
                    );
                    json_response_for_era(&response_msg, era)
                }
                Err(mcp_err) => {
                    let response_msg = ServerJsonRpcMessage::error(mcp_err, Some(request_id));
                    json_response_for_era(&response_msg, era)
                }
            }
        }

        ClientRequest::ListPromptsRequest(list_req) => {
            if let Err(error) = policy_context.authorize(crate::protocol::MethodFamily::PromptsList)
            {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(error, Some(request_id)),
                    era,
                );
            }
            if !modern {
                validate_session_header(headers, state.sessions.as_ref())?;
            }
            let result = state.router.list_prompts_page(list_req.params);
            let response_msg =
                ServerJsonRpcMessage::response(ServerResult::ListPromptsResult(result), request_id);
            json_response_for_era(&response_msg, era)
        }

        ClientRequest::GetPromptRequest(prompt_req) => {
            if let Err(error) = policy_context.authorize(crate::protocol::MethodFamily::PromptsGet)
            {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(error, Some(request_id)),
                    era,
                );
            }
            if !modern {
                validate_session_header(headers, state.sessions.as_ref())?;
            }
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
                    json_response_for_era(&response_msg, era)
                }
                Err(mcp_err) => {
                    let response_msg = ServerJsonRpcMessage::error(mcp_err, Some(request_id));
                    json_response_for_era(&response_msg, era)
                }
            }
        }

        ClientRequest::SubscribeRequest(sub_req) => {
            if modern {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(
                        McpError::method_not_found::<SubscribeRequestMethod>(),
                        Some(request_id),
                    ),
                    era,
                );
            }
            if let Err(error) =
                policy_context.authorize(crate::protocol::MethodFamily::ResourcesSubscribe)
            {
                return json_response(&ServerJsonRpcMessage::error(error, Some(request_id)));
            }
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
                    let response_msg = ServerJsonRpcMessage::error(mcp_err, Some(request_id));
                    json_response(&response_msg)
                }
            }
        }

        ClientRequest::UnsubscribeRequest(unsub_req) => {
            if modern {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(
                        McpError::method_not_found::<UnsubscribeRequestMethod>(),
                        Some(request_id),
                    ),
                    era,
                );
            }
            if let Err(error) =
                policy_context.authorize(crate::protocol::MethodFamily::ResourcesSubscribe)
            {
                return json_response(&ServerJsonRpcMessage::error(error, Some(request_id)));
            }
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
                    let response_msg = ServerJsonRpcMessage::error(mcp_err, Some(request_id));
                    json_response(&response_msg)
                }
            }
        }

        ClientRequest::CompleteRequest(complete_req) => {
            if let Err(error) = policy_context.authorize(crate::protocol::MethodFamily::Completion)
            {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(error, Some(request_id)),
                    era,
                );
            }
            if !modern {
                validate_session_header(headers, state.sessions.as_ref())?;
            }
            match state.router.complete_request(complete_req.params).await {
                Ok(result) => {
                    let response_msg = ServerJsonRpcMessage::response(
                        ServerResult::CompleteResult(result),
                        request_id,
                    );
                    json_response_for_era(&response_msg, era)
                }
                Err(mcp_err) => {
                    let response_msg = ServerJsonRpcMessage::error(mcp_err, Some(request_id));
                    json_response_for_era(&response_msg, era)
                }
            }
        }

        ClientRequest::SetLevelRequest(set_level_req) => {
            if modern {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(
                        McpError::method_not_found::<SetLevelRequestMethod>(),
                        Some(request_id),
                    ),
                    era,
                );
            }
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
            if !modern {
                validate_session_header(headers, state.sessions.as_ref())?;
            }
            let error = ErrorData::new(ErrorCode::METHOD_NOT_FOUND, "method not supported", None);
            let response_msg = ServerJsonRpcMessage::error(error, Some(request_id));
            json_response_for_era(&response_msg, era)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn legacy_task_params(
    request: &CustomRequest,
) -> Result<crate::legacy_tasks::TaskIdParams, ErrorData> {
    request
        .params_as::<crate::legacy_tasks::TaskIdParams>()
        .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?
        .ok_or_else(|| ErrorData::invalid_params("missing task params", None))
}

fn json_task_value<T: serde::Serialize>(value: T) -> Result<serde_json::Value, ErrorData> {
    serde_json::to_value(value).map_err(|error| ErrorData::internal_error(error.to_string(), None))
}

/// Build the InitializeResult (same as ProxyHandler::get_info).
fn build_initialize_result(
    router: &ToolRouter,
    client_type: crate::types::ClientType,
) -> InitializeResult {
    InitializeResult::new(router.synthesized_capabilities_for_client(client_type))
        .with_server_info(crate::branding::plug_implementation(env!(
            "CARGO_PKG_VERSION"
        )))
        .with_protocol_version(crate::protocol::supported_protocol_version())
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
    json_response_for_era(msg, crate::protocol::ProtocolEra::Legacy)
}

fn json_response_for_era(
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
    let mut value = serde_json::to_value(msg)
        .map_err(|e| HttpError::Internal(format!("failed to serialize response: {e}")))?;
    if era == crate::protocol::ProtocolEra::Legacy {
        crate::protocol::rewrite_legacy_response(&mut value, false);
    }
    let body = serde_json::to_vec(&value)
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
            HeaderValue::from_static(crate::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION),
        );
    }

    Ok(response)
}

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
    use crate::proxy::RouterSnapshot;
    use axum::body::Body;
    use http::Request as HttpRequest;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    use tower::ServiceExt;

    fn isolated_oauth_manager(
        scopes: Vec<String>,
    ) -> crate::downstream_oauth::DownstreamOauthManager {
        let path = std::env::temp_dir().join(format!(
            "plug-oauth-http-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        crate::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            crate::downstream_oauth::DownstreamOauthConfig {
                public_base_url: "https://plug.example.com".to_string(),
                oauth_scopes: scopes,
                local_port: 3282,
            },
            path,
        )
        .expect("isolated OAuth manager")
    }

    fn oauth_test_state() -> Arc<HttpState> {
        oauth_test_state_with_manager(isolated_oauth_manager(vec!["tools:read".to_string()]))
    }

    fn oauth_test_state_with_manager(
        manager: crate::downstream_oauth::DownstreamOauthManager,
    ) -> Arc<HttpState> {
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
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Oauth,
            downstream_oauth: Some(manager),
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
        })
    }

    fn oauth_tools_list_request(access_token: &str, session_id: &str) -> HttpRequest<Body> {
        HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
            .header(SESSION_ID_HEADER, session_id)
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION)
            .body(Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/list"
                })
                .to_string(),
            ))
            .expect("tools/list request")
    }

    #[test]
    fn trace_id_prefers_valid_w3c_traceparent() {
        let mut headers = HeaderMap::new();
        headers.insert(
            TRACEPARENT_HEADER,
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );

        assert_eq!(
            extract_trace_id(&headers),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
    }

    #[test]
    fn trace_id_falls_back_when_header_is_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert(TRACEPARENT_HEADER, HeaderValue::from_static("invalid"));

        let trace_id = extract_trace_id(&headers);
        assert_eq!(trace_id.len(), 32);
        assert_ne!(trace_id, "00000000000000000000000000000000");
    }

    #[test]
    fn shared_loopback_classifier_matches_http_parser_outputs() {
        for origin in [
            "http://localhost:3282",
            "http://127.0.0.1:3282",
            "http://[::1]:3282",
        ] {
            let host = extract_origin_host(origin).expect("loopback origin host");
            assert!(
                crate::config::http_bind_is_loopback(host),
                "expected {host} parsed from {origin} to be loopback"
            );
        }

        for origin in [
            "http://localhost.evil.example:3282",
            "http://192.0.2.1:3282",
            "https://plug.example.com",
        ] {
            let host = extract_origin_host(origin).expect("non-loopback origin host");
            assert!(
                !crate::config::http_bind_is_loopback(host),
                "expected {host} parsed from {origin} to remain non-loopback"
            );
        }

        for host in ["localhost:3282", "127.0.0.1:3282", "[::1]:3282"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, HeaderValue::from_str(host).unwrap());
            assert!(
                validate_modern_host(&headers, &[], None).is_ok(),
                "host {host}"
            );
        }

        for host in ["localhost.evil.example:3282", "192.0.2.1:3282", "::1"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, HeaderValue::from_str(host).unwrap());
            assert!(
                validate_modern_host(&headers, &[], None).is_err(),
                "host {host} must not become loopback"
            );
        }
    }

    #[test]
    fn modern_host_accepts_public_tunnel_without_allowing_unrelated_browser_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("plug-tunnel.example.com"),
        );
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://unrelated.example.com"),
        );

        // OAuth mode supplies the manager's downstream base URL directly.
        assert!(
            validate_modern_host(&headers, &[], Some("https://plug-tunnel.example.com/mcp"))
                .is_ok()
        );
        // Non-OAuth public_base_url configuration contributes a host-only URL
        // marker to this list; it can satisfy Host without satisfying Origin.
        let configured_public_host = [Arc::from(
            "https://plug-tunnel.example.com/.plug-public-host-only",
        )];
        assert!(validate_modern_host(&headers, &configured_public_host, None).is_ok());
        assert!(validate_modern_origin(&headers, &configured_public_host).is_err());
        assert!(validate_modern_origin(&headers, &[]).is_err());
    }

    #[test]
    fn authenticated_http_task_owner_is_stable_across_sessions() {
        let authenticated = AuthStatus::Authenticated(None);
        assert_eq!(
            http_task_owner("session-a", &authenticated, None),
            http_task_owner("session-b", &authenticated, None),
            "the configured credential, not the replaceable HTTP session, owns durable tasks"
        );

        assert_ne!(
            http_task_owner("session-a", &AuthStatus::NoAuthRequired, None),
            http_task_owner("session-b", &AuthStatus::NoAuthRequired, None),
            "legacy unauthenticated sessions retain session-scoped ownership"
        );
    }

    async fn collect_sse_events(body: Body, max_events: usize) -> Vec<String> {
        crate::http::sse::collect_sse_events(body, max_events).await
    }

    fn sse_event_id(event: &str) -> Option<u64> {
        event.lines().find_map(|line| {
            line.strip_prefix("id: ")
                .and_then(|value| value.trim().parse::<u64>().ok())
        })
    }

    fn test_state_with_router_config(router_config: crate::proxy::RouterConfig) -> Arc<HttpState> {
        let sm = Arc::new(crate::server::ServerManager::new());
        let router = Arc::new(ToolRouter::new(sm, router_config));
        Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
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
            lazy_tools: crate::config::LazyToolsConfig::default(),
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        })
    }

    fn enable_test_task_surface(router: &ToolRouter) {
        router.replace_snapshot(RouterSnapshot {
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),

            routes: HashMap::from([(
                "Mock__echo".to_string(),
                ("mock".to_string(), "echo".to_string()),
            )]),
            tools_all: Arc::new(vec![Tool::new(
                std::borrow::Cow::Borrowed("Mock__echo"),
                std::borrow::Cow::Borrowed("Echo"),
                Arc::new(serde_json::Map::new()),
            )]),
            meta_tools_all: Arc::new(Vec::new()),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        });
    }

    fn modern_task_meta(client_supports_tasks: bool) -> RequestMetaObject {
        let capabilities = if client_supports_tasks {
            ClientCapabilities::builder().enable_tasks().build()
        } else {
            ClientCapabilities::default()
        };
        RequestMetaObject::with_client_context(
            ProtocolVersion::V_2026_07_28,
            Implementation::new("modern-task-test", "1.0"),
            capabilities,
        )
    }

    fn modern_task_policy_context(
        gate_enabled: bool,
        scopes: impl IntoIterator<Item = String>,
    ) -> DownstreamCallContext {
        DownstreamCallContext::http("task-client", RequestId::Number(1))
            .with_protocol(
                crate::protocol::ProtocolEra::Modern,
                crate::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION,
            )
            .with_modern_direction_enabled(gate_enabled)
            .with_authorization(
                crate::types::PrincipalId::configured_credential("task-test", 1),
                scopes,
            )
    }

    fn modern_request(method: &str, mut params: serde_json::Value) -> HttpRequest<Body> {
        params.as_object_mut().expect("params object").insert(
            "_meta".to_string(),
            json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {
                    "name": "plug-modern-test",
                    "version": "1.0.0"
                },
                "io.modelcontextprotocol/clientCapabilities": {}
            }),
        );
        HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::HOST, "localhost:3282")
            .header(PROTOCOL_VERSION_HEADER, "2026-07-28")
            .header(crate::mcp_http_headers::MCP_METHOD_HEADER, method)
            .body(Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": method,
                    "params": params
                })
                .to_string(),
            ))
            .unwrap()
    }

    async fn issue_test_oauth_token(
        manager: &crate::downstream_oauth::DownstreamOauthManager,
        scope: &str,
    ) -> String {
        let redirect_uri = "https://client.example.com/callback";
        let registration = manager
            .register_client(
                ClientRegistrationRequest {
                    redirect_uris: vec![redirect_uri.to_string()],
                    client_name: Some("modern-scope-test".to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: Some(vec![
                        "authorization_code".to_string(),
                        "refresh_token".to_string(),
                    ]),
                    response_types: Some(vec!["code".to_string()]),
                    scope: None,
                },
                "modern-scope-test",
            )
            .await
            .expect("register client");
        let consent = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &registration.client_id,
                redirect_uri,
                state: "scope-test",
                code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                code_challenge_method: "S256",
                scope: Some(scope),
                resource: &manager.resource(),
            })
            .await
            .expect("begin authorization");
        let redirect = manager
            .decide_consent(&consent.consent_id, true)
            .await
            .expect("approve consent");
        let code = url::Url::parse(&redirect.location)
            .expect("redirect URL")
            .query_pairs()
            .find_map(|(key, value)| (key == "code").then(|| value.into_owned()))
            .expect("authorization code");
        manager
            .exchange_authorization_code(
                &registration.client_id,
                &code,
                redirect_uri,
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
                &manager.resource(),
            )
            .await
            .expect("exchange authorization code")
            .access_token
    }

    #[tokio::test]
    async fn legacy_tools_read_oauth_principal_keeps_pre_scope_method_compatibility() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let access_token = issue_test_oauth_token(&manager, "tools:read").await;
        let claims = match manager
            .validate_access_token_for(&access_token, &[], &manager.resource())
            .await
        {
            AccessTokenValidation::Valid(claims) => claims,
            other => panic!("issued token must validate, got {other:?}"),
        };
        let state = test_state();
        let context = legacy_http_policy_context(
            &state,
            &AuthStatus::Authenticated(Some(claims)),
            RequestId::Number(91),
            Arc::from("legacy-oauth-regression"),
        );

        assert!(
            context.principal.is_some(),
            "verified principal is preserved"
        );
        assert!(
            context.principal_lifecycle.is_some(),
            "revocation lifecycle is preserved"
        );
        for family in [
            crate::protocol::MethodFamily::ToolsList,
            crate::protocol::MethodFamily::ResourcesList,
            crate::protocol::MethodFamily::ResourcesRead,
            crate::protocol::MethodFamily::ResourcesSubscribe,
            crate::protocol::MethodFamily::PromptsList,
            crate::protocol::MethodFamily::PromptsGet,
            crate::protocol::MethodFamily::Completion,
            crate::protocol::MethodFamily::Tasks,
            crate::protocol::MethodFamily::Listeners,
        ] {
            assert!(
                context.policy_decision(family).is_allowed(),
                "legacy OAuth principal lost access to {family:?}"
            );
        }
    }

    #[tokio::test]
    async fn modern_discovery_is_default_off_then_sessionless_when_enabled() {
        let state = test_state();
        let disabled = build_router(Arc::clone(&state))
            .oneshot(modern_request("server/discover", json!({})))
            .await
            .unwrap();
        assert_ne!(disabled.status(), StatusCode::OK);

        state.router.set_modern_downstream_enabled(true);
        let response = build_router(Arc::clone(&state))
            .oneshot(modern_request("server/discover", json!({})))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(SESSION_ID_HEADER).is_none());
        assert_eq!(
            response
                .headers()
                .get(PROTOCOL_VERSION_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("2026-07-28")
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["result"]["resultType"], "complete");
        // Both revisions, matching what stdio and IPC advertise: the same port
        // still serves `2025-11-25` through `initialize`, so omitting it would
        // tell a modern client that legacy support had been dropped.
        assert_eq!(
            value["result"]["supportedVersions"],
            json!([PROTOCOL_VERSION, "2026-07-28"])
        );
        assert!(value["result"]["capabilities"]["extensions"].is_null());
        assert!(value["result"]["capabilities"]["experimental"].is_null());
        assert!(value["result"]["capabilities"]["logging"].is_null());
    }

    #[test]
    fn modern_http_task_admission_matrix_is_consistent() {
        let state = test_state();
        let allowed = modern_task_policy_context(true, ["tasks:use".to_string()]);
        let task_meta = modern_task_meta(true);

        let unavailable =
            admit_modern_task_operation(state.router.as_ref(), &allowed, Some(&task_meta))
                .expect_err("server without task surface must reject all task methods");
        assert_eq!(unavailable.code, ErrorCode::METHOD_NOT_FOUND);

        enable_test_task_surface(state.router.as_ref());
        let missing_client = admit_modern_task_operation(
            state.router.as_ref(),
            &allowed,
            Some(&modern_task_meta(false)),
        )
        .expect_err("client capability is mandatory");
        assert_eq!(
            missing_client.code,
            ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY
        );

        let denied = modern_task_policy_context(true, ["tools:read".to_string()]);
        let permission =
            admit_modern_task_operation(state.router.as_ref(), &denied, Some(&task_meta))
                .expect_err("task scope is mandatory");
        assert_eq!(
            permission.code.0,
            crate::protocol::ProtocolOutcome::PermissionDenied
                .encode(crate::protocol::ProtocolEra::Modern)
                .code
        );

        let gate_off = modern_task_policy_context(false, ["tasks:use".to_string()]);
        let disabled =
            admit_modern_task_operation(state.router.as_ref(), &gate_off, Some(&task_meta))
                .expect_err("live gate must apply to direct task methods");
        assert_eq!(
            disabled,
            crate::protocol::ProtocolOutcome::UnsupportedBridge
                .into_error(crate::protocol::ProtocolEra::Modern)
        );

        let owner = admit_modern_task_operation(state.router.as_ref(), &allowed, Some(&task_meta))
            .expect("fully admitted task method");
        assert_eq!(
            owner.as_key(),
            allowed.principal.as_ref().expect("principal").owner_key()
        );
    }

    #[tokio::test]
    async fn gate_off_rejects_every_modern_http_task_method_before_dispatch() {
        let state = test_state();
        enable_test_task_surface(state.router.as_ref());
        for (method, params) in [
            ("tasks/get", json!({"taskId":"task_1"})),
            (
                "tasks/update",
                json!({"taskId":"task_1","inputResponses":{}}),
            ),
            ("tasks/cancel", json!({"taskId":"task_1"})),
        ] {
            let response = build_router(Arc::clone(&state))
                .oneshot(modern_request(method, params))
                .await
                .expect("task response");
            assert_ne!(response.status(), StatusCode::OK, "{method} escaped gate");
        }
    }

    #[tokio::test]
    async fn modern_catalog_is_sessionless_and_deterministic() {
        let state = test_state();
        state.router.set_modern_downstream_enabled(true);
        let app = build_router(state);
        let first = app
            .clone()
            .oneshot(modern_request("tools/list", json!({})))
            .await
            .unwrap();
        let second = app
            .oneshot(modern_request("tools/list", json!({})))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        let first = axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap();
        let second = axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn modern_unsupported_method_uses_modern_http_status() {
        let state = test_state();
        state.router.set_modern_downstream_enabled(true);
        let response = build_router(state)
            .oneshot(modern_request("ping", json!({})))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn resource_only_oauth_token_can_discover_and_list_resources_but_not_tools() {
        let manager = isolated_oauth_manager(vec!["resources:read".to_string()]);
        let access_token = issue_test_oauth_token(&manager, "resources:read").await;
        let state = test_state();
        state.router.set_modern_downstream_enabled(true);
        let state = Arc::new(HttpState {
            router: Arc::clone(&state.router),
            sessions: Arc::clone(&state.sessions),
            cancel: state.cancel.clone(),
            auth_mode: crate::config::DownstreamAuthMode::Oauth,
            downstream_oauth: Some(manager),
            sse_channel_capacity: state.sse_channel_capacity,
            allowed_origins: state.allowed_origins.clone(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
        });
        let app = build_router(state);
        let authorized = |method: &str| {
            let mut request = modern_request(method, json!({}));
            request.headers_mut().insert(
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {access_token}"))
                    .expect("authorization header"),
            );
            request
        };

        let discovery = app
            .clone()
            .oneshot(authorized("server/discover"))
            .await
            .unwrap();
        assert_eq!(discovery.status(), StatusCode::OK);

        let resources = app
            .clone()
            .oneshot(authorized("resources/list"))
            .await
            .unwrap();
        assert_eq!(resources.status(), StatusCode::OK);

        let tools = app.oneshot(authorized("tools/list")).await.unwrap();
        assert_eq!(tools.status(), StatusCode::OK);
        let body = axum::body::to_bytes(tools.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["data"]["kind"], "permission_denied");
    }

    #[test]
    fn modern_capability_projection_is_permission_filtered_and_bridge_free() {
        let state = test_state();
        state.router.set_modern_downstream_enabled(true);
        let context = DownstreamCallContext::http("scoped-client", RequestId::Number(1))
            .with_protocol(
                crate::protocol::ProtocolEra::Modern,
                crate::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION,
            )
            .with_modern_direction_enabled(true)
            .with_authorization(
                crate::types::PrincipalId::configured_credential("test", 1),
                ["tools:read".to_string()],
            );
        let capabilities = projected_modern_capabilities(state.router.as_ref(), &context);
        assert!(
            context
                .policy_decision(crate::protocol::MethodFamily::ToolsList)
                .is_allowed()
        );
        assert!(capabilities.resources.is_none());
        assert!(capabilities.prompts.is_none());
        assert!(capabilities.completions.is_none());
        assert!(capabilities.extensions.is_none());
        assert!(capabilities.experimental.is_none());
        assert!(capabilities.logging.is_none());
    }

    /// Session-store wrapper that parks the owner-liveness validation (the
    /// second validation in a task-wrapped POST) until a concurrent DELETE
    /// has removed the session. It delegates every other operation to the
    /// production store, so the regression drives the real HTTP handlers.
    struct GateSecondValidateSessionStore {
        inner: crate::session::StatefulSessionStore,
        validate_calls: AtomicUsize,
        second_validate_notify: tokio::sync::Notify,
        released: std::sync::Mutex<bool>,
        release_cv: std::sync::Condvar,
    }

    impl GateSecondValidateSessionStore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                inner: crate::session::StatefulSessionStore::new(1800, 100),
                validate_calls: AtomicUsize::new(0),
                second_validate_notify: tokio::sync::Notify::new(),
                released: std::sync::Mutex::new(false),
                release_cv: std::sync::Condvar::new(),
            })
        }

        fn release_second_validate(&self) {
            *self.released.lock().unwrap() = true;
            self.release_cv.notify_all();
        }
    }

    struct SecondValidateReleaseGuard(Arc<GateSecondValidateSessionStore>);

    impl Drop for SecondValidateReleaseGuard {
        fn drop(&mut self) {
            self.0.release_second_validate();
        }
    }

    impl SessionStore for GateSecondValidateSessionStore {
        fn create_session(&self) -> Result<String, HttpError> {
            self.inner.create_session()
        }

        fn validate(&self, session_id: &str) -> Result<(), HttpError> {
            if self.validate_calls.fetch_add(1, Ordering::SeqCst) == 1 {
                self.second_validate_notify.notify_one();
                let mut released = self.released.lock().unwrap();
                while !*released {
                    released = self.release_cv.wait(released).unwrap();
                }
            }
            self.inner.validate(session_id)
        }

        fn touch(&self, session_id: &str) -> Result<(), HttpError> {
            self.inner.touch(session_id)
        }

        fn has_live_sse_sender(&self, session_id: &str) -> Result<bool, HttpError> {
            self.inner.has_live_sse_sender(session_id)
        }

        fn set_sse_sender(
            &self,
            session_id: &str,
            sender: mpsc::Sender<crate::session::SseEvent>,
            last_event_id: Option<u64>,
        ) -> Result<(), HttpError> {
            self.inner.set_sse_sender(session_id, sender, last_event_id)
        }

        fn set_client_type(
            &self,
            session_id: &str,
            client_type: crate::types::ClientType,
        ) -> Result<(), HttpError> {
            self.inner.set_client_type(session_id, client_type)
        }

        fn get_client_type(&self, session_id: &str) -> Result<crate::types::ClientType, HttpError> {
            self.inner.get_client_type(session_id)
        }

        fn remove(&self, session_id: &str) -> bool {
            self.inner.remove(session_id)
        }

        fn broadcast(&self, message: SseMessage) {
            self.inner.broadcast(message);
        }

        fn send_to_session(&self, session_id: &str, message: SseMessage) {
            self.inner.send_to_session(session_id, message);
        }

        fn send_to_live_session(
            &self,
            session_id: &str,
            message: SseMessage,
        ) -> SessionSendOutcome {
            self.inner.send_to_live_session(session_id, message)
        }

        fn remove_replay_events_by_key(&self, session_id: &str, key: &SseReplayKey) {
            self.inner.remove_replay_events_by_key(session_id, key);
        }

        fn spawn_cleanup_task(&self, cancel: CancellationToken) {
            self.inner.spawn_cleanup_task(cancel);
        }

        fn session_count(&self) -> usize {
            self.inner.session_count()
        }

        fn session_snapshots(&self) -> Vec<crate::session::DownstreamSessionSnapshot> {
            self.inner.session_snapshots()
        }
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
    async fn request_body_limit_accepts_exactly_four_mebibytes() {
        const LIMIT: usize = 4 * 1024 * 1024;
        let app = build_router(test_state());
        let mut body = b"{}".to_vec();
        body.resize(LIMIT, b' ');
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("content-length", LIMIT)
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_ne!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn request_body_limit_rejects_content_length_over_four_mebibytes() {
        const OVER_LIMIT: usize = 4 * 1024 * 1024 + 1;
        let app = build_router(test_state());
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("content-length", OVER_LIMIT)
            .body(Body::from(vec![b' '; OVER_LIMIT]))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn request_body_limit_rejects_chunked_body_over_four_mebibytes() {
        let app = build_router(test_state());
        let chunks = vec![
            Ok::<_, std::convert::Infallible>(bytes::Bytes::from_static(b"{}")),
            Ok(bytes::Bytes::from(vec![b' '; 2 * 1024 * 1024])),
            Ok(bytes::Bytes::from(vec![b' '; 2 * 1024 * 1024])),
        ];
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from_stream(futures::stream::iter(chunks)))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
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
    async fn post_rejects_mismatched_mcp_method_header() {
        let app = build_router(test_state());

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
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

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("Mcp-Method", "tools/list")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], HEADER_MISMATCH_CODE);
        assert_eq!(value["id"], 9);
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Mcp-Method")
        );
    }

    #[tokio::test]
    async fn post_rejects_mismatched_mcp_name_header() {
        let state = test_state();
        let app = build_router(state.clone());

        let init_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "header-test", "version": "1.0" }
            }
        });
        let init_req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&init_body).unwrap()))
            .unwrap();
        let init_resp = app.oneshot(init_req).await.unwrap();
        let session_id = init_resp
            .headers()
            .get(SESSION_ID_HEADER)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let app = build_router(state);
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "actual_tool",
                "arguments": {}
            }
        });
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header(SESSION_ID_HEADER, session_id)
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION)
            .header("Mcp-Method", "tools/call")
            .header("Mcp-Name", "spoofed_tool")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], HEADER_MISMATCH_CODE);
        assert_eq!(value["id"], 2);
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Mcp-Name")
        );
    }

    #[tokio::test]
    async fn modern_missing_mcp_name_error_carries_modern_protocol_header() {
        let state = test_state();
        state.router.set_modern_downstream_enabled(true);
        let response = build_router(state)
            .oneshot(modern_request(
                "tools/call",
                json!({"name": "actual_tool", "arguments": {}}),
            ))
            .await
            .expect("header mismatch response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response
                .headers()
                .get(PROTOCOL_VERSION_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(crate::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION)
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], HEADER_MISMATCH_CODE);
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("Mcp-Name"))
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

    /// End-to-end transport regression for the create-vs-teardown boundary:
    /// the POST passes its initial session validation, registers the task
    /// owner guard, then DELETE removes that same session before the enqueue
    /// path's liveness re-check. The real handler must refuse the create.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_wrapped_post_racing_delete_refuses_late_create() {
        let sessions = GateSecondValidateSessionStore::new();
        let session_id = sessions.create_session().unwrap();
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
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        let state = Arc::new(HttpState {
            router: Arc::clone(&router),
            sessions: Arc::clone(&sessions) as Arc<dyn SessionStore>,
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
        });
        let app = build_router(state);

        let post_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "missing__tool",
                "arguments": {},
                "task": {}
            }
        });
        let post_request = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header(SESSION_ID_HEADER, &session_id)
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION)
            .body(Body::from(serde_json::to_vec(&post_body).unwrap()))
            .unwrap();
        let post_app = app.clone();
        let post = tokio::spawn(async move { post_app.oneshot(post_request).await.unwrap() });

        if tokio::time::timeout(
            Duration::from_secs(5),
            sessions.second_validate_notify.notified(),
        )
        .await
        .is_err()
        {
            let validate_calls = sessions.validate_calls.load(Ordering::SeqCst);
            sessions.release_second_validate();
            let response = post.await.unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            panic!(
                "task POST never reached its post-guard owner-liveness validation; validate_calls={validate_calls}, status={status}, body={}",
                String::from_utf8_lossy(&body)
            );
        }
        let release_guard = SecondValidateReleaseGuard(Arc::clone(&sessions));

        let delete_request = HttpRequest::builder()
            .method("DELETE")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .body(Body::empty())
            .unwrap();
        let delete_response = app.oneshot(delete_request).await.unwrap();
        assert_eq!(delete_response.status(), StatusCode::OK);

        sessions.release_second_validate();
        drop(release_guard);
        let post_response = post.await.unwrap();
        assert_eq!(post_response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(post_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], ErrorCode::INVALID_REQUEST.0);
        assert_eq!(
            value["error"]["message"],
            "session closed during task creation"
        );
        let owner = ToolRouter::task_owner_for_http_session(&session_id);
        assert_eq!(router.task_count_for_owner(&owner).await, 0);
    }

    #[tokio::test]
    async fn delete_mcp_cleans_up_session_tasks() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();

        // Give the router a routable tool so `enqueue_tool_task` can create a
        // task record for this session without needing a live upstream.
        state.router.replace_snapshot(crate::proxy::RouterSnapshot {
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),

            routes: std::collections::HashMap::from([(
                "git__commit".to_string(),
                ("git".to_string(), "commit".to_string()),
            )]),
            tools_all: Arc::new(vec![Tool::new(
                std::borrow::Cow::Borrowed("git__commit"),
                std::borrow::Cow::Borrowed("Create a git commit"),
                Arc::new(serde_json::Map::new()),
            )]),
            meta_tools_all: Arc::new(Vec::new()),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: std::collections::HashMap::new(),
            prompt_routes: std::collections::HashMap::new(),
            tool_definition_fingerprints: std::collections::HashMap::new(),
            tool_risk_inventory: std::collections::HashMap::new(),
        });

        let owner = ToolRouter::task_owner_for_http_session(&session_id);
        state
            .router
            .enqueue_tool_task("git__commit", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue task for departing session");
        assert_eq!(state.router.task_count_for_owner(&owner).await, 1);

        let app = build_router(state.clone());
        let req = HttpRequest::builder()
            .method("DELETE")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.router.task_count_for_owner(&owner).await, 0);
    }

    /// Drop guard that flips a flag when the future holding it stops
    /// running. Proves teardown actually stops execution, not just that the
    /// task's store record disappears — a bare `retain` that drops a
    /// `JoinHandle` without aborting it would detach the future (it keeps
    /// running) while still passing a record-count-only assertion.
    struct AbortObserver(Arc<AtomicBool>);

    impl Drop for AbortObserver {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn delete_mcp_aborts_still_running_session_task() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();
        let owner = ToolRouter::task_owner_for_http_session(&session_id);

        let dropped = Arc::new(AtomicBool::new(false));
        let observer = AbortObserver(Arc::clone(&dropped));
        let handle = tokio::spawn(async move {
            let _observer = observer;
            // Never resolves on its own — only `DELETE /mcp` teardown
            // aborting the handle can stop it, mirroring a real task future
            // parked waiting on a slow/unresponsive upstream.
            std::future::pending::<()>().await;
        });
        state
            .router
            .attach_test_task_with_abort_handle(owner.clone(), "long_running_tool", handle)
            .await;

        assert_eq!(state.router.task_count_for_owner(&owner).await, 1);
        assert!(
            !dropped.load(Ordering::SeqCst),
            "task must still be running before teardown"
        );

        let app = build_router(state.clone());
        let req = HttpRequest::builder()
            .method("DELETE")
            .uri("/mcp")
            .header(SESSION_ID_HEADER, &session_id)
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.router.task_count_for_owner(&owner).await, 0);

        // `JoinHandle::abort()` only requests cancellation — the aborted
        // future is dropped the next time the runtime polls/schedules it,
        // which is inherently async relative to the DELETE response above —
        // so poll within a bound instead of asserting immediately.
        let mut stopped = false;
        for _ in 0..200 {
            if dropped.load(Ordering::SeqCst) {
                stopped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            stopped,
            "DELETE /mcp teardown must abort the still-running task future, not just its record"
        );
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

    #[tokio::test]
    async fn origin_allowlisted_external_origin_accepted() {
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
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        let state = Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: vec![Arc::from("https://claude.ai")],
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
        });
        let app = build_router(state);

        let body = serde_json::json!({
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

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("origin", "https://claude.ai")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
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
    async fn routed_http_error_preserves_request_id() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();
        let app = build_router(state);

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "request-123",
            "method": "resources/read",
            "params": {
                "uri": "memory://missing"
            }
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

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], "request-123");
        assert!(json["error"]["code"].is_number());
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
            lazy_tools: crate::config::LazyToolsConfig::default(),
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        });

        state.router.replace_snapshot(crate::proxy::RouterSnapshot {
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),

            routes: std::collections::HashMap::from([(
                "Git__commit".to_string(),
                ("git".to_string(), "commit".to_string()),
            )]),
            tools_all: Arc::new(vec![Tool::new(
                std::borrow::Cow::Borrowed("Git__commit"),
                std::borrow::Cow::Borrowed("Create a git commit"),
                Arc::new(serde_json::Map::new()),
            )]),
            meta_tools_all: Arc::new(vec![Tool::new(
                std::borrow::Cow::Borrowed("plug__search_tools"),
                std::borrow::Cow::Borrowed("Search tools"),
                Arc::new(serde_json::Map::new()),
            )]),
            tools_windsurf: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: std::collections::HashMap::new(),
            prompt_routes: std::collections::HashMap::new(),
            tool_definition_fingerprints: std::collections::HashMap::new(),
            tool_risk_inventory: std::collections::HashMap::new(),
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

    /// A body that was meant to be JSON-RPC gets a JSON-RPC error back, not the
    /// plain-text 400 this path used to return. `-32700` for malformed JSON,
    /// `-32600` for well-formed JSON of the wrong shape, `id: null` for both
    /// (there is no id to echo). Other 400s — session, header, metadata
    /// rejections — deliberately keep their plain-text bodies.
    #[tokio::test]
    async fn malformed_body_returns_jsonrpc_error_envelope() {
        for (raw, expected_code) in [
            (r#"{"jsonrpc": "2.0", "#, -32700),
            (r#"{"not": "json-rpc at all"}"#, -32600),
        ] {
            let app = build_router(test_state());
            let req = HttpRequest::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(raw))
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();

            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "input: {raw}");
            assert_eq!(
                resp.headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok()),
                Some("application/json"),
                "input: {raw}"
            );

            let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("body was not JSON for input {raw}: {e}"));
            assert_eq!(json["jsonrpc"], "2.0", "input: {raw}");
            assert_eq!(json["error"]["code"], expected_code, "input: {raw}");
            assert!(json["id"].is_null(), "input: {raw}");
        }
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

        // Header and body must advertise the same version on one response. A
        // remote Claude connector saw no tools when they disagreed, so pin the
        // pair together rather than in two separate tests.
        assert_eq!(
            resp.headers()
                .get(PROTOCOL_VERSION_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(PROTOCOL_VERSION)
        );

        let resp_body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(json["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(json["result"]["serverInfo"]["name"], "plug");
        assert_initialize_icons_sequence(&json["result"]["serverInfo"]["icons"]);
        assert!(json["result"]["capabilities"]["tools"].is_null());
    }

    fn assert_initialize_icons_sequence(icons: &serde_json::Value) {
        let expected_sizes = ["16x16", "32x32", "64x64", "128x128", "256x256", "512x512"];
        let icon_array = icons.as_array().expect("icons array");
        assert_eq!(icon_array.len(), expected_sizes.len() + 1);

        for (icon, expected_size) in icon_array.iter().zip(expected_sizes) {
            assert_eq!(icon["mimeType"], "image/png");
            assert_eq!(icon["sizes"][0], expected_size);
            assert!(
                icon["src"]
                    .as_str()
                    .expect("png icon src")
                    .starts_with("data:image/png;base64,")
            );
        }

        let svg = icon_array.last().expect("svg icon");
        assert_eq!(svg["mimeType"], "image/svg+xml");
        assert_eq!(svg["sizes"][0], "any");
        assert!(
            svg["src"]
                .as_str()
                .expect("svg icon src")
                .starts_with("data:image/svg+xml;base64,")
        );
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
            .uri("/.well-known/mcp-server-card")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=3600")
        );
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("*")
        );
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .and_then(|v| v.to_str().ok()),
            Some("GET")
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
        assert_eq!(
            json["$schema"],
            "https://static.modelcontextprotocol.io/schemas/v1/server-card.schema.json"
        );
        assert_eq!(json["name"], "io.github.cyberpapiii/plug");
        assert_eq!(json["title"], "Plug");
        assert!(json["version"].is_string());
        assert_eq!(json["remotes"][0]["type"], "streamable-http");
        assert_eq!(json["remotes"][0]["url"], "/mcp");
        assert_eq!(
            json["remotes"][0]["supportedProtocolVersions"],
            serde_json::json!([PROTOCOL_VERSION])
        );
        assert!(json.get("tools").is_none());
        assert!(json.get("servers").is_none());
    }

    #[tokio::test]
    async fn legacy_server_card_path_returns_same_card() {
        let app = build_router(test_state());

        let req = HttpRequest::builder()
            .method("GET")
            .uri("/.well-known/mcp.json")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "io.github.cyberpapiii/plug");
        assert_eq!(json["remotes"][0]["url"], "/mcp");
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
            Some(Duration::from_millis(50)),
        )
        .await;

        assert!(result.is_err());
        assert!(state.pending_client_requests.is_empty());
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
            Some(Duration::from_secs(1)),
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
                Some(Duration::from_secs(1)),
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
    async fn last_event_id_replays_missed_http_sse_notifications() {
        let state = test_state();
        let app = build_router(state.clone());

        let session_id = state.sessions.create_session().unwrap();
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
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Bearer,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: Some(Arc::from(token)),
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
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
    async fn oauth_mode_metadata_endpoint_returns_server_metadata() {
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
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        let state = Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Oauth,
            downstream_oauth: Some(isolated_oauth_manager(vec!["tools:read".to_string()])),
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
        });
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("GET")
            .uri("/.well-known/oauth-authorization-server")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 10_000)
            .await
            .expect("body bytes");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
        assert_eq!(
            value["authorization_endpoint"],
            "https://plug.example.com/oauth/authorize"
        );
        assert_eq!(
            value["token_endpoint"],
            "https://plug.example.com/oauth/token"
        );
        assert_eq!(value["scopes_supported"][0], "tools:read");
        assert_eq!(
            value["token_endpoint_auth_methods_supported"],
            json!(["none"])
        );
    }

    #[tokio::test]
    async fn oauth_mode_metadata_advertises_public_clients_only() {
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
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        let state = Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Oauth,
            downstream_oauth: Some(isolated_oauth_manager(vec!["tools:read".to_string()])),
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
        });
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("GET")
            .uri("/.well-known/oauth-authorization-server")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 10_000)
            .await
            .expect("body bytes");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
        assert_eq!(
            value["token_endpoint_auth_methods_supported"],
            json!(["none"])
        );
        assert_eq!(
            value["grant_types_supported"],
            json!(["authorization_code", "refresh_token"])
        );
        assert_eq!(
            value["registration_endpoint"],
            "https://plug.example.com/oauth/register"
        );
        assert_eq!(value["client_id_metadata_document_supported"], true);
    }

    #[tokio::test]
    async fn oauth_protected_resource_metadata_endpoint_returns_rfc9728_metadata() {
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
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        let state = Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Oauth,
            downstream_oauth: Some(isolated_oauth_manager(vec![
                "tools:read".to_string(),
                "offline_access".to_string(),
            ])),
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
        });
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("GET")
            .uri("/.well-known/oauth-protected-resource")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 10_000)
            .await
            .expect("body bytes");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
        assert_eq!(value["resource"], "https://plug.example.com/mcp");
        assert_eq!(
            value["authorization_servers"],
            json!(["https://plug.example.com"])
        );
        assert_eq!(value["scopes_supported"], json!(["tools:read"]));
        assert_eq!(value["bearer_methods_supported"], json!(["header"]));
    }

    #[tokio::test]
    async fn oauth_error_json_includes_actionable_safe_description() {
        let app = build_router(oauth_test_state());
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(
                "grant_type=authorization_code&client_id=unknown-client",
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let body = axum::body::to_bytes(response.into_body(), 10_000)
            .await
            .expect("OAuth error body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("OAuth error JSON");
        assert_eq!(body["error"], "invalid_request");
        assert!(
            body["error_description"]
                .as_str()
                .expect("error description")
                .contains("Try connecting again")
        );
    }

    #[tokio::test]
    async fn oauth_error_authorization_html_is_readable_and_not_cached() {
        let app = build_router(oauth_test_state());
        let request = HttpRequest::builder()
            .method("GET")
            .uri("/oauth/authorize?response_type=code&client_id=unknown-client&redirect_uri=https%3A%2F%2Fclient.example.com%2Fcallback&state=abc123&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&scope=tools%3Aread&resource=https%3A%2F%2Fplug.example.com%2Fmcp")
            .header(header::ACCEPT, "text/html")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let body = axum::body::to_bytes(response.into_body(), 10_000)
            .await
            .expect("OAuth authorization error body");
        let body = String::from_utf8(body.to_vec()).expect("OAuth authorization error HTML");
        assert!(body.contains("<title>Plug authorization failed</title>"));
        assert!(body.contains("invalid_client"));
        assert!(body.contains("Try connecting again"));
    }

    #[test]
    fn oauth_error_html_accept_rejects_zero_quality() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json, text/html; q=0"),
        );

        assert!(!accepts_html(&headers));
    }

    #[test]
    fn oauth_error_html_accept_rejects_lookalike_subtype() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/htmlx; q=1"));

        assert!(!accepts_html(&headers));
    }

    #[test]
    fn oauth_error_html_accept_matches_case_insensitively() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("Text/HTML; Charset=utf-8; Q=0.7"),
        );

        assert!(accepts_html(&headers));
    }

    #[test]
    fn oauth_error_html_accept_checks_repeated_header_values() {
        let mut headers = HeaderMap::new();
        headers.append(header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.append(header::ACCEPT, HeaderValue::from_static("text/html; q=0.5"));

        assert!(accepts_html(&headers));
    }

    #[tokio::test]
    async fn oauth_client_credentials_flow_is_rejected() {
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
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        let state = Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Oauth,
            downstream_oauth: Some(isolated_oauth_manager(vec![
                "tools:read".to_string(),
                "offline_access".to_string(),
            ])),
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
        });
        let app = build_router(state);

        let token_req = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header(header::AUTHORIZATION, "Basic Y2xpZW50LTEyMzpzZWNyZXQtMTIz")
            .body(Body::from(
                "grant_type=client_credentials&scope=tools%3Aread+offline_access",
            ))
            .unwrap();

        let token_resp = app.clone().oneshot(token_req).await.unwrap();
        assert_eq!(token_resp.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(token_resp.into_body(), 10_000)
            .await
            .expect("body bytes");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
        assert_eq!(value["error"], "invalid_client");
    }

    #[tokio::test]
    async fn oauth_mode_requires_access_token_for_mcp_requests() {
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
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        let state = Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Oauth,
            downstream_oauth: Some(isolated_oauth_manager(vec!["tools:read".to_string()])),
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
        });
        let app = build_router(state);

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers()
                .get(header::WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok())
                .expect("www-authenticate"),
            "Bearer resource_metadata=\"https://plug.example.com/.well-known/oauth-protected-resource\", scope=\"tools:read\""
        );
    }

    #[tokio::test]
    async fn client_neutral_oauth_lifecycle_acceptance_matrix() {
        let state_path = std::env::temp_dir().join(format!(
            "plug-oauth-lifecycle-matrix-{}.json",
            uuid::Uuid::new_v4()
        ));
        let oauth_config = crate::downstream_oauth::DownstreamOauthConfig {
            public_base_url: "https://plug.example.com".to_string(),
            oauth_scopes: vec!["tools:read".to_string()],
            local_port: 3282,
        };
        let manager = crate::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            oauth_config.clone(),
            state_path.clone(),
        )
        .expect("lifecycle OAuth manager");
        let app = build_router(oauth_test_state_with_manager(manager.clone()));

        let registration_req = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/register")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"client_name":"Matrix public client","redirect_uris":["https://client.example.com/callback"],"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"]}"#,
            ))
            .unwrap();
        let registration_resp = app.clone().oneshot(registration_req).await.unwrap();
        assert_eq!(registration_resp.status(), StatusCode::CREATED);
        let registration_body = axum::body::to_bytes(registration_resp.into_body(), 10_000)
            .await
            .expect("registration body");
        let registration: serde_json::Value =
            serde_json::from_slice(&registration_body).expect("registration json");
        let client_id = registration["client_id"]
            .as_str()
            .expect("client id")
            .to_string();

        let authorize_req = HttpRequest::builder()
            .method("GET")
            .uri(format!("/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fclient.example.com%2Fcallback&state=matrix-state&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&scope=tools%3Aread&resource=https%3A%2F%2Fplug.example.com%2Fmcp"))
            .body(Body::empty())
            .unwrap();
        let authorize_resp = app.clone().oneshot(authorize_req).await.unwrap();
        assert_eq!(authorize_resp.status(), StatusCode::OK);
        let consent_body = axum::body::to_bytes(authorize_resp.into_body(), 20_000)
            .await
            .expect("consent body");
        let consent_html = String::from_utf8(consent_body.to_vec()).expect("consent html");
        let consent_id = consent_html
            .split("name=\"consent_id\" value=\"")
            .nth(1)
            .and_then(|value| value.split('\"').next())
            .expect("consent id");
        let consent_req = HttpRequest::builder()
            .method("POST")
            .uri("/_plug/oauth/authorize")
            .header(header::HOST, "127.0.0.1:3282")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "consent_id={consent_id}&decision=approve"
            )))
            .unwrap();
        let consent_resp = app.clone().oneshot(consent_req).await.unwrap();
        assert_eq!(consent_resp.status(), StatusCode::FOUND);
        let location = consent_resp
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .expect("redirect location");
        let code = location
            .split("code=")
            .nth(1)
            .and_then(|v| v.split('&').next())
            .expect("authorization code");

        let token_req = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "grant_type=authorization_code&client_id={client_id}&code={code}&redirect_uri=https%3A%2F%2Fclient.example.com%2Fcallback&code_verifier=dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk&resource=https%3A%2F%2Fplug.example.com%2Fmcp"
            )))
            .unwrap();
        let token_resp = app.clone().oneshot(token_req).await.unwrap();
        assert_eq!(token_resp.status(), StatusCode::OK);
        let token_body = axum::body::to_bytes(token_resp.into_body(), 10_000)
            .await
            .expect("token body");
        let token_value: serde_json::Value =
            serde_json::from_slice(&token_body).expect("token json");
        let access_token = token_value["access_token"]
            .as_str()
            .expect("access token")
            .to_string();
        let refresh_token = token_value["refresh_token"]
            .as_str()
            .expect("refresh token")
            .to_string();
        assert_eq!(token_value["token_type"], "Bearer");

        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "matrix-client", "version": "1.0" }
            }
        });
        let initialize_req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {access_token}"))
            .body(Body::from(initialize.to_string()))
            .unwrap();
        let initialize_resp = app.clone().oneshot(initialize_req).await.unwrap();
        assert_eq!(initialize_resp.status(), StatusCode::OK);
        let session_id = initialize_resp
            .headers()
            .get(SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("session ID")
            .to_string();

        let list_req = oauth_tools_list_request(&access_token, &session_id);
        let list_resp = app.clone().oneshot(list_req).await.unwrap();
        assert_eq!(list_resp.status(), StatusCode::OK);
        let list_body = axum::body::to_bytes(list_resp.into_body(), 10_000)
            .await
            .expect("tools/list body");
        let list_value: serde_json::Value =
            serde_json::from_slice(&list_body).expect("tools/list JSON");
        assert!(list_value["result"]["tools"].is_array());

        let refresh_req = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "grant_type=refresh_token&client_id={client_id}&refresh_token={refresh_token}&resource=https%3A%2F%2Fplug.example.com%2Fmcp"
            )))
            .unwrap();
        let refresh_resp = app.clone().oneshot(refresh_req).await.unwrap();
        assert_eq!(refresh_resp.status(), StatusCode::OK);
        let refresh_body = axum::body::to_bytes(refresh_resp.into_body(), 10_000)
            .await
            .expect("refresh body");
        let refresh_value: serde_json::Value =
            serde_json::from_slice(&refresh_body).expect("refresh JSON");
        let rotated_access_token = refresh_value["access_token"]
            .as_str()
            .expect("rotated access token")
            .to_string();
        let rotated_refresh_token = refresh_value["refresh_token"]
            .as_str()
            .expect("rotated refresh token")
            .to_string();
        assert_ne!(rotated_access_token, access_token);
        assert_ne!(rotated_refresh_token, refresh_token);

        let replayed_refresh = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "grant_type=refresh_token&client_id={client_id}&refresh_token={refresh_token}&resource=https%3A%2F%2Fplug.example.com%2Fmcp"
            )))
            .unwrap();
        let replayed_refresh = app.clone().oneshot(replayed_refresh).await.unwrap();
        assert_eq!(replayed_refresh.status(), StatusCode::BAD_REQUEST);
        let replayed_body = axum::body::to_bytes(replayed_refresh.into_body(), 10_000)
            .await
            .expect("replayed refresh body");
        let replayed_value: serde_json::Value =
            serde_json::from_slice(&replayed_body).expect("replayed refresh JSON");
        assert_eq!(replayed_value["error"], "invalid_grant");

        drop(app);
        drop(manager);
        let restarted = crate::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            oauth_config.clone(),
            state_path.clone(),
        )
        .expect("restarted OAuth manager");
        let restarted_app = build_router(oauth_test_state_with_manager(restarted.clone()));

        let restarted_initialize = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {rotated_access_token}"))
            .body(Body::from(initialize.to_string()))
            .unwrap();
        let restarted_initialize = restarted_app
            .clone()
            .oneshot(restarted_initialize)
            .await
            .unwrap();
        assert_eq!(restarted_initialize.status(), StatusCode::OK);
        let restarted_session_id = restarted_initialize
            .headers()
            .get(SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("restarted session ID")
            .to_string();
        let restarted_list = restarted_app
            .clone()
            .oneshot(oauth_tools_list_request(
                &rotated_access_token,
                &restarted_session_id,
            ))
            .await
            .unwrap();
        assert_eq!(restarted_list.status(), StatusCode::OK);

        let refresh_after_restart = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "grant_type=refresh_token&client_id={client_id}&refresh_token={rotated_refresh_token}&resource=https%3A%2F%2Fplug.example.com%2Fmcp"
            )))
            .unwrap();
        let refresh_after_restart = restarted_app
            .clone()
            .oneshot(refresh_after_restart)
            .await
            .unwrap();
        assert_eq!(refresh_after_restart.status(), StatusCode::OK);
        let refresh_after_restart = axum::body::to_bytes(refresh_after_restart.into_body(), 10_000)
            .await
            .expect("post-restart refresh body");
        let refresh_after_restart: serde_json::Value =
            serde_json::from_slice(&refresh_after_restart).expect("post-restart refresh JSON");
        let final_access_token = refresh_after_restart["access_token"]
            .as_str()
            .expect("post-restart access token")
            .to_string();

        assert!(
            restarted
                .revoke_client(&client_id)
                .await
                .expect("revoke client")
        );
        let revoked = restarted_app
            .oneshot(oauth_tools_list_request(
                &final_access_token,
                &restarted_session_id,
            ))
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            revoked
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
            Some(
                "Bearer resource_metadata=\"https://plug.example.com/.well-known/oauth-protected-resource\", scope=\"tools:read\""
            )
        );
        let revoked_body = axum::body::to_bytes(revoked.into_body(), 10_000)
            .await
            .expect("revoked access body");
        let revoked_value: serde_json::Value =
            serde_json::from_slice(&revoked_body).expect("revoked access JSON");
        assert_eq!(revoked_value["error"]["code"], -32001);

        drop(restarted);
        let after_revoke = crate::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            oauth_config,
            state_path,
        )
        .expect("restart after revoke");
        assert!(after_revoke.list_clients().await.is_empty());
        let after_revoke_app = build_router(oauth_test_state_with_manager(after_revoke));
        let revoked_after_restart = after_revoke_app
            .oneshot(oauth_tools_list_request(
                &final_access_token,
                &restarted_session_id,
            ))
            .await
            .unwrap();
        assert_eq!(revoked_after_restart.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn discovery_server_card_when_unauth_on_protected_server_is_static() {
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

        assert!(card.get("servers").is_none());
        assert!(card.get("tools").is_none());
        assert_eq!(card["remotes"][0]["url"], "/mcp");
        assert_eq!(card["remotes"][0]["headers"][0]["name"], "Authorization");
        assert_eq!(card["remotes"][0]["headers"][0]["isSecret"], true);
    }

    #[tokio::test]
    async fn discovery_server_card_when_unauth_on_oauth_protected_server_is_static() {
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
                lazy_tools: crate::config::LazyToolsConfig::default(),
                tool_filter_enabled: true,
                enrichment_servers: std::collections::HashSet::new(),
            },
        ));
        let state = Arc::new(HttpState {
            router,
            sessions: Arc::new(crate::session::StatefulSessionStore::new(1800, 100)),
            cancel: CancellationToken::new(),
            auth_mode: crate::config::DownstreamAuthMode::Oauth,
            downstream_oauth: Some(isolated_oauth_manager(vec!["tools:read".to_string()])),
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: DashMap::new(),
            pending_client_requests: DashMap::new(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: DashMap::new(),
        });
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

        assert!(card.get("servers").is_none());
        assert!(card.get("tools").is_none());
        assert_eq!(card["remotes"][0]["headers"][0]["name"], "Authorization");
    }

    #[tokio::test]
    async fn discovery_server_card_does_not_expand_when_authenticated() {
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

        assert!(card.get("servers").is_none());
        assert!(card.get("tools").is_none());
        assert_eq!(card["remotes"][0]["headers"][0]["name"], "Authorization");
    }
}
