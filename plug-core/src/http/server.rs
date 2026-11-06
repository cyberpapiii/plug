use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
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
use super::oauth::{
    get_oauth_authorization_server_metadata, get_oauth_protected_resource_metadata,
    oauth_authorize, oauth_consent_challenge, oauth_consent_decision, oauth_consent_javascript,
    oauth_enroll_javascript, oauth_icon, oauth_owner_enroll, oauth_owner_enroll_challenge,
    oauth_owner_enroll_complete, oauth_register, oauth_stylesheet, oauth_token,
    protected_resource_metadata_url,
};
use super::sse::sse_stream_with_heartbeat;
use crate::downstream_identity::HttpCredential;
use crate::downstream_oauth::{AccessTokenClaims, AccessTokenValidation, resource_scopes};
use crate::events::{EVENT_SCOPE, EventSource};
use crate::mcp_http_headers::{
    HEADER_MISMATCH_CODE, HeaderMismatch, inject_trace_context, validate_mirrored_headers,
    validate_required_mirrored_headers,
};
use crate::notifications::{NotificationTarget, ProtocolNotification};
use crate::proxy::{DownstreamBridge, DownstreamCallContext, ToolRouter};
use crate::session::{SessionEnd, SessionSendOutcome, SessionStore, SseMessage, SseReplayKey};

const SESSION_ID_HEADER: &str = "Mcp-Session-Id";

/// MCP protocol version header name.
const PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";
const TRACEPARENT_HEADER: &str = "traceparent";
const PLUG_TRACE_ID_HEADER: &str = "x-plug-trace-id";

/// The MCP protocol version we implement.
use crate::protocol::SUPPORTED_PROTOCOL_VERSION;

/// Shared state for all HTTP handlers.
pub struct HttpState {
    pub router: Arc<ToolRouter>,
    pub sessions: Arc<SessionStore>,
    pub(crate) cancel: CancellationToken,
    pub(crate) auth_mode: crate::config::DownstreamAuthMode,
    pub downstream_oauth: Option<crate::downstream_oauth::DownstreamOauthManager>,
    /// Where MCP Events come from. See `crate::events`.
    pub(crate) event_sources: Vec<Arc<dyn EventSource>>,
    pub(crate) sse_channel_capacity: usize,
    pub(crate) allowed_origins: Vec<Arc<str>>,
    pub(crate) notification_task_started: AtomicBool,
    /// Bearer token for downstream client authentication.
    /// `None` means the current downstream auth mode does not require bearer auth.
    pub(crate) auth_token: Option<Arc<str>>,
    /// Sessions whose clients advertise `roots` capability.
    pub(crate) roots_capable_sessions: Arc<DashMap<String, ()>>,
    /// Pending reverse requests sent to HTTP clients (keyed by session_id + request_id).
    pub(crate) pending_client_requests: Arc<PendingClientRequests>,
    /// Counter for generating unique reverse-request IDs.
    pub(crate) reverse_request_counter: AtomicU64,
    /// Per-session client capabilities for reverse-request gating.
    pub(crate) client_capabilities: Arc<DashMap<String, ClientCapabilities>>,
}

type PendingClientRequests = DashMap<(String, i64), oneshot::Sender<ClientResult>>;

/// What HTTP holds for its sessions outside the session store.
struct HeldForSessions {
    router: Arc<ToolRouter>,
    roots_capable_sessions: Arc<DashMap<String, ()>>,
    pending_client_requests: Arc<PendingClientRequests>,
    client_capabilities: Arc<DashMap<String, ClientCapabilities>>,
}

impl HeldForSessions {
    /// Release what is held for a session that has left the store.
    async fn release(&self, session_id: &str) {
        self.roots_capable_sessions.remove(session_id);
        self.client_capabilities.remove(session_id);
        // Dropping pending reverse-request senders wakes their receivers with
        // RecvError instead of leaving them to time out.
        self.pending_client_requests
            .retain(|(pending_session_id, _), _| pending_session_id != session_id);
        self.router
            .end_downstream_session(crate::proxy::EndedSession::Http { session_id })
            .await;
    }
}

/// What revoking a downstream OAuth client came to.
#[derive(Debug)]
pub enum ClientRevocation {
    /// Its grant is gone, its sessions ended, and the tasks it owned went.
    Revoked,
    UnknownClient,
    OauthDisabled,
    /// The grant stopped working before the save was tried, so its sessions
    /// ended even though saving the revocation failed.
    SaveFailed(crate::downstream_oauth::DownstreamOauthError),
}

/// One HTTP request as the dispatcher sees it: a legacy request inside its
/// session, or a stateless modern request identified by its credential.
struct HttpDownstreamContext {
    session_id: Arc<str>,
    request_id: RequestId,
    client_type: crate::types::ClientType,
    trace_id: Arc<str>,
    sessions: Arc<SessionStore>,
    auth_status: AuthStatus,
    oauth_issuer: Option<Arc<str>>,
    protocol_era: crate::protocol::ProtocolEra,
    modern_direction_enabled: bool,
    cancellation: CancellationToken,
    client_metadata: Option<crate::protocol::ClientMetadata>,
    /// What a modern request declared in `_meta`. HTTP serves the modern
    /// tasks extension, so a modern request always has one.
    modern_task_client: Option<ClientCapabilities>,
}

impl HttpDownstreamContext {
    /// `session` is the legacy session the request belongs to; a modern
    /// request has none and goes by its credential's owner key instead.
    fn new(
        state: &HttpState,
        auth_status: AuthStatus,
        session: Option<&str>,
        request_id: RequestId,
        trace_id: Arc<str>,
        request_meta: Option<&RequestMetaObject>,
    ) -> Self {
        let oauth_issuer = state
            .downstream_oauth
            .as_ref()
            .map(|manager| Arc::<str>::from(manager.base_url()));
        let (session_id, client_type, client_metadata, modern_task_client, protocol_era) =
            match session {
                Some(session_id) => (
                    Arc::<str>::from(session_id),
                    state
                        .sessions
                        .get_client_type(session_id)
                        .unwrap_or(crate::types::ClientType::Unknown),
                    None,
                    None,
                    crate::protocol::ProtocolEra::Legacy,
                ),
                None => {
                    let client_info = request_meta.and_then(RequestMetaObject::client_info);
                    (
                        Arc::<str>::from(
                            http_credential(&auth_status, oauth_issuer.as_deref())
                                .principal()
                                .owner_key(),
                        ),
                        client_info
                            .as_ref()
                            .map(|info| crate::client_detect::detect_client(&info.name))
                            .unwrap_or(crate::types::ClientType::Unknown),
                        client_info.map(|info| crate::protocol::ClientMetadata {
                            name: Arc::from(info.name),
                            version: Arc::from(info.version),
                        }),
                        Some(
                            request_meta
                                .and_then(RequestMetaObject::client_capabilities)
                                .unwrap_or_default(),
                        ),
                        crate::protocol::ProtocolEra::Modern,
                    )
                }
            };
        Self {
            session_id,
            request_id,
            client_type,
            trace_id,
            sessions: Arc::clone(&state.sessions),
            auth_status,
            oauth_issuer,
            protocol_era,
            modern_direction_enabled: state.router.modern_downstream_enabled(),
            cancellation: CancellationToken::new(),
            client_metadata,
            modern_task_client,
        }
    }
}

/// What a request proved about its caller. `NoAuthRequired` is produced only
/// when `auth_mode` is not OAuth and no bearer token is configured, a
/// combination config validation permits only for a loopback bind.
fn http_credential<'a>(
    auth_status: &'a AuthStatus,
    oauth_issuer: Option<&'a str>,
) -> HttpCredential<'a> {
    match auth_status {
        AuthStatus::Authenticated(Some(claims)) => HttpCredential::Grant {
            issuer: oauth_issuer.unwrap_or("unknown-issuer"),
            client_id: &claims.client_id,
            resource: &claims.resource,
        },
        AuthStatus::Authenticated(None) => HttpCredential::SharedBearer,
        AuthStatus::NoAuthRequired => HttpCredential::Loopback,
    }
}

fn legacy_http_policy_context(
    state: &HttpState,
    auth_status: &AuthStatus,
    request_id: RequestId,
    trace_id: Arc<str>,
) -> DownstreamCallContext {
    let context = DownstreamCallContext::http(
        "legacy-http-policy",
        request_id,
        crate::types::ClientType::Unknown,
    )
    .with_trace_id(trace_id);
    apply_http_auth(
        context,
        auth_status,
        state
            .downstream_oauth
            .as_ref()
            .map(|manager| manager.base_url()),
    )
}

/// Attach what the request proved about its caller to a call context.
///
/// An OAuth token carries its granted scopes and principal lifecycle. The
/// configured bearer and an unauthenticated request, which only a
/// loopback-only listener accepts, are local principals: the trust comes from
/// the credential or the listener, not from client-provided metadata, and
/// holds in both protocol eras.
fn apply_http_auth(
    context: DownstreamCallContext,
    auth_status: &AuthStatus,
    oauth_issuer: Option<&str>,
) -> DownstreamCallContext {
    let credential = http_credential(auth_status, oauth_issuer);
    let context = context.with_client_key(Some(credential.client_key()));
    match auth_status {
        AuthStatus::Authenticated(Some(claims)) => context
            .with_authorization(credential.principal(), claims.scopes.clone())
            .with_principal_lifecycle(claims.principal_lifecycle.clone()),
        AuthStatus::Authenticated(None) | AuthStatus::NoAuthRequired => {
            context.with_local_principal(credential.principal())
        }
    }
}

impl crate::dispatch::DownstreamContext for HttpDownstreamContext {
    fn downstream_call_context(&self) -> DownstreamCallContext {
        let mut context = DownstreamCallContext::http(
            Arc::clone(&self.session_id),
            self.request_id.clone(),
            self.client_type,
        )
        .with_trace_id(Arc::clone(&self.trace_id))
        .with_protocol(self.protocol_era, self.protocol_era.version_str())
        .with_modern_direction_enabled(self.modern_direction_enabled)
        .with_lifecycle(None, self.cancellation.clone(), None);
        if let Some(metadata) = &self.client_metadata {
            context = context
                .with_client_metadata(Arc::clone(&metadata.name), Arc::clone(&metadata.version));
        }
        apply_http_auth(context, &self.auth_status, self.oauth_issuer.as_deref())
    }

    fn session_bound(&self) -> bool {
        self.protocol_era == crate::protocol::ProtocolEra::Legacy
    }

    fn task_owner(&self) -> Result<crate::tasks::TaskOwner, McpError> {
        let session_id = self.session_bound().then_some(self.session_id.as_ref());
        Ok(http_credential(&self.auth_status, self.oauth_issuer.as_deref()).task_owner(session_id))
    }

    /// A session leaves the store before what it holds is released, so one
    /// still in the store has not begun to end.
    fn owner_liveness_probe(&self) -> Option<crate::tasks::OwnerLivenessProbe> {
        if !self.session_bound() {
            return None;
        }
        let sessions = Arc::clone(&self.sessions);
        let session_id = Arc::clone(&self.session_id);
        Some(Arc::new(move || sessions.validate(&session_id).is_ok()))
    }

    fn modern_task_client(&self) -> Option<&ClientCapabilities> {
        self.modern_task_client.as_ref()
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

impl HttpState {
    /// State with `auth_mode = auto`, no bearer token, no downstream OAuth, no
    /// events, no extra allowed origins, and the default SSE channel capacity.
    ///
    /// From here on `sessions` drops expired sessions on its own, and what
    /// Plug holds for any session that lapses is released. Must be called on
    /// a Tokio runtime. A session store serves one `HttpState`.
    pub fn new(
        router: Arc<ToolRouter>,
        sessions: Arc<SessionStore>,
        cancel: CancellationToken,
    ) -> Self {
        let state = Self {
            router,
            sessions,
            cancel,
            auth_mode: crate::config::DownstreamAuthMode::default(),
            downstream_oauth: None,
            event_sources: Vec::new(),
            sse_channel_capacity: crate::config::DEFAULT_SSE_CHANNEL_CAPACITY,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: Arc::default(),
            pending_client_requests: Arc::default(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: Arc::default(),
        };
        let (lapsed_tx, mut lapsed_rx) = mpsc::unbounded_channel::<String>();
        state
            .sessions
            .report_lapses_to(lapsed_tx)
            .expect("a session store serves one HttpState");
        state.sessions.spawn_cleanup_task(state.cancel.clone());
        let held = state.held_for_sessions();
        tokio::spawn(async move {
            while let Some(session_id) = lapsed_rx.recv().await {
                held.release(&session_id).await;
            }
        });
        state
    }

    fn held_for_sessions(&self) -> HeldForSessions {
        HeldForSessions {
            router: Arc::clone(&self.router),
            roots_capable_sessions: Arc::clone(&self.roots_capable_sessions),
            pending_client_requests: Arc::clone(&self.pending_client_requests),
            client_capabilities: Arc::clone(&self.client_capabilities),
        }
    }

    #[must_use]
    pub fn with_auth_mode(mut self, auth_mode: crate::config::DownstreamAuthMode) -> Self {
        self.auth_mode = auth_mode;
        self
    }

    #[must_use]
    pub fn with_auth_token(mut self, auth_token: Option<Arc<str>>) -> Self {
        self.auth_token = auth_token;
        self
    }

    #[must_use]
    pub fn with_downstream_oauth(
        mut self,
        downstream_oauth: Option<crate::downstream_oauth::DownstreamOauthManager>,
    ) -> Self {
        self.downstream_oauth = downstream_oauth;
        self
    }

    /// Event sources, in the order `events/list` shows their events.
    #[must_use]
    pub fn with_event_sources(mut self, event_sources: Vec<Arc<dyn EventSource>>) -> Self {
        self.event_sources = event_sources;
        self
    }

    #[must_use]
    pub fn with_sse_channel_capacity(mut self, sse_channel_capacity: usize) -> Self {
        self.sse_channel_capacity = sse_channel_capacity;
        self
    }

    #[must_use]
    pub fn with_allowed_origins(mut self, allowed_origins: Vec<Arc<str>>) -> Self {
        self.allowed_origins = allowed_origins;
        self
    }

    /// End a session and release what Plug holds for it. Returns whether it
    /// was open.
    pub(crate) async fn end_session(&self, session_id: &str, why: SessionEnd) -> bool {
        if !self.sessions.end(session_id, why) {
            return false;
        }
        self.held_for_sessions().release(session_id).await;
        true
    }

    /// Take a downstream OAuth client's access away: its tokens stop
    /// working, its sessions end, and the tasks it owns as its principal go.
    pub async fn revoke_downstream_client(&self, client_id: &str) -> ClientRevocation {
        let Some(manager) = self.downstream_oauth.as_ref() else {
            return ClientRevocation::OauthDisabled;
        };
        match manager.revoke_client(client_id).await {
            Ok(true) => {
                self.end_sessions_of(client_id).await;
                // The manager already refuses the grant's tokens, and the
                // task store's create guard keeps a create that is under way
                // from outliving this cleanup.
                let resource = manager.resource();
                let owner = HttpCredential::Grant {
                    issuer: manager.base_url(),
                    client_id,
                    resource: &resource,
                }
                .task_owner(None);
                self.router.cleanup_tasks_for_owner(&owner).await;
                ClientRevocation::Revoked
            }
            Ok(false) => ClientRevocation::UnknownClient,
            Err(error) => {
                self.end_sessions_of(client_id).await;
                ClientRevocation::SaveFailed(error)
            }
        }
    }

    async fn end_sessions_of(&self, client_id: &str) {
        for session_id in self.sessions.sessions_of(client_id) {
            self.end_session(&session_id, SessionEnd::Revoked).await;
        }
    }

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

fn notification_to_sse_message(notification: &ProtocolNotification) -> Option<SseMessage> {
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
        .route("/oauth/assets/consent.js", get(oauth_consent_javascript))
        .route("/oauth/consent/challenge", post(oauth_consent_challenge))
        .route("/oauth/consent/decision", post(oauth_consent_decision))
        .route("/oauth/owner/enroll", get(oauth_owner_enroll))
        .route("/oauth/assets/enroll.js", get(oauth_enroll_javascript))
        .route("/oauth/assets/plug.css", get(oauth_stylesheet))
        .route("/oauth/assets/plug-icon.svg", get(oauth_icon))
        .route(
            "/oauth/owner/enroll/challenge",
            post(oauth_owner_enroll_challenge),
        )
        .route(
            "/oauth/owner/enroll/complete",
            post(oauth_owner_enroll_complete),
        )
        .route("/oauth/token", post(oauth_token))
        // A client that renews before it has read the metadata again posts
        // to the address older MCP clients assume. Without it a stored
        // sign-in cannot renew after the client restarts.
        .route("/token", post(oauth_token))
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
        .with_state(state.clone());

    // Sources that take events pushed from outside sign those requests
    // themselves, so these routes sit outside the MCP auth layers.
    let mut router = discovery.merge(mcp);
    for source in &state.event_sources {
        let Some(path) = source.ingest_path() else {
            continue;
        };
        let source = Arc::clone(source);
        let ingest = Router::new()
            .route(
                path,
                post(move |headers: HeaderMap, body: Bytes| async move {
                    match source.ingest(&headers, &body).await {
                        Ok(value) => Json(value).into_response(),
                        Err(error) => (error.http_status(), Json(json!({"error":error.message})))
                            .into_response(),
                    }
                }),
            )
            .layer(DefaultBodyLimit::max(crate::events::MAX_BODY));
        router = ingest.merge(router);
    }
    router
}

/// The asking client and the event sources it can see, when there are any.
fn event_sources_for<'a>(
    state: &'a HttpState,
    auth: &'a AuthStatus,
) -> Option<(&'a AccessTokenClaims, Vec<&'a Arc<dyn EventSource>>)> {
    let AuthStatus::Authenticated(Some(claims)) = auth else {
        return None;
    };
    if !claims.principal_lifecycle.is_active() {
        return None;
    }
    let sources: Vec<_> = state
        .event_sources
        .iter()
        .filter(|source| source.discoverable_to(&claims.client_id))
        .collect();
    (!sources.is_empty()).then_some((claims, sources))
}

fn events_discoverable(state: &HttpState, auth: &AuthStatus) -> bool {
    event_sources_for(state, auth).is_some()
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
        let is_local = extract_origin_host(origin).is_some_and(crate::config::is_loopback_host);

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
    session_is_the_callers(&headers, state.sessions.as_ref(), &auth_status)?;

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
                crate::protocol::MODERN_PROTOCOL_VERSION.to_string(),
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
        // `initialize` opens a session of its own.
        if raw_message.get("method").and_then(|method| method.as_str()) != Some("initialize") {
            reopen_lost_session(&state, &headers, &auth_status);
        }
    }
    let method = raw_message
        .get("method")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let params = raw_message.get("params").cloned();
    let message: ClientJsonRpcMessage = serde_json::from_value(raw_message).map_err(|e| {
        tracing::debug!(error = %e, "invalid JSON-RPC message from client");
        HttpError::MalformedJsonRpc {
            code: -32600,
            message: "Invalid Request",
        }
    })?;

    validate_protocol_version_for_post(&headers, era)?;
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
            let method = method.unwrap_or_default();
            handle_request(
                req,
                method,
                params,
                &headers,
                &state,
                trace_id,
                auth_status,
                era,
            )
            .await
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
                    if let Some(request_id) = cancelled.params.request_id.clone() {
                        let context = crate::dispatch::DownstreamContext::downstream_call_context(
                            &HttpDownstreamContext::new(
                                &state,
                                auth_status,
                                None,
                                request_id,
                                trace_id,
                                None,
                            ),
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
                            &DownstreamCallContext::http(
                                Arc::<str>::from(session_id.as_str()),
                                request_id,
                                crate::types::ClientType::Unknown,
                            )
                            .with_trace_id(Arc::clone(&trace_id)),
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
    let local = crate::config::is_loopback_host(&host_name);
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
    if extract_origin_host(origin).is_some_and(crate::config::is_loopback_host) {
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
            HeaderValue::from_static(crate::protocol::MODERN_PROTOCOL_VERSION),
        );
    }
    response
}

fn validate_protocol_version_for_post(
    headers: &HeaderMap,
    era: crate::protocol::ProtocolEra,
) -> Result<(), HttpError> {
    if era == crate::protocol::ProtocolEra::Modern {
        return Ok(());
    }
    match headers.get(PROTOCOL_VERSION_HEADER) {
        Some(value) => {
            let version = value
                .to_str()
                .map_err(|_| HttpError::BadRequest("invalid MCP-Protocol-Version header".into()))?;
            if version != SUPPORTED_PROTOCOL_VERSION {
                return Err(HttpError::UnsupportedProtocolVersion(version.to_string()));
            }
            Ok(())
        }
        // A client that leaves the header out is on the version its session
        // settled at `initialize`. Some clients never send it.
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
    let missing = meta.missing_required_keys(&crate::protocol::ProtocolEra::Modern.version());
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
struct PendingReverseRequest<'a> {
    state: &'a HttpState,
    session_id: &'a str,
    id: i64,
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

/// GET /mcp — open SSE stream for server-initiated notifications.
async fn get_mcp(
    State(state): State<Arc<HttpState>>,
    axum::Extension(auth_status): axum::Extension<AuthStatus>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    // 1. Validate session
    let session_id = extract_session_id(&headers)?;
    reopen_lost_session(&state, &headers, &auth_status);
    state.sessions.validate(&session_id)?;
    session_is_the_callers(&headers, state.sessions.as_ref(), &auth_status)?;

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
        let _ = session_store.validate(&keepalive_session_id);
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
    axum::Extension(auth_status): axum::Extension<AuthStatus>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let session_id = extract_session_id(&headers)?;
    session_is_the_callers(&headers, state.sessions.as_ref(), &auth_status)?;

    if state
        .end_session(&session_id, SessionEnd::ClosedByClient)
        .await
    {
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
        "supportedProtocolVersions": [SUPPORTED_PROTOCOL_VERSION],
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

// ---------------------------------------------------------------------------
// Request routing
// ---------------------------------------------------------------------------

/// Route a typed JSON-RPC request to the appropriate handler.
#[allow(clippy::too_many_arguments)]
async fn handle_request(
    req: JsonRpcRequest<ClientRequest>,
    method: String,
    params: Option<serde_json::Value>,
    headers: &HeaderMap,
    state: &Arc<HttpState>,
    trace_id: Arc<str>,
    auth_status: AuthStatus,
    era: crate::protocol::ProtocolEra,
) -> Result<Response, HttpError> {
    let request_id = req.id.clone();
    let modern = era == crate::protocol::ProtocolEra::Modern;
    // Fixed protocol metadata only: never log credentials, identifiers or parameters.
    let discovery_method = match &req.request {
        ClientRequest::InitializeRequest(_) => Some("initialize"),
        ClientRequest::DiscoverRequest(_) => Some("server/discover"),
        ClientRequest::CustomRequest(custom) if custom.method == "events/list" => {
            Some("events/list")
        }
        _ => None,
    };
    if let Some(method) = discovery_method {
        tracing::info!(
            method,
            protocol_era = if modern { "modern" } else { "legacy" },
            selected_event_client = events_discoverable(state, &auth_status),
            "MCP Events discovery request"
        );
    }
    let request_meta = modern.then(|| rmcp::model::GetMeta::get_meta(&req.request).clone());

    if let ClientRequest::CustomRequest(custom) = &req.request
        && modern
        && matches!(
            custom.method.as_str(),
            "events/list" | "events/subscribe" | "events/unsubscribe"
        )
    {
        let policy_context = HttpDownstreamContext::new(
            state,
            auth_status.clone(),
            None,
            request_id.clone(),
            Arc::clone(&trace_id),
            request_meta.as_ref(),
        );
        let _modern_request_lease = match state.router.admit_modern_request(
            &crate::dispatch::DownstreamContext::downstream_call_context(&policy_context),
        ) {
            Ok(lease) => lease,
            Err(error) => {
                return json_response_for_era(
                    &ServerJsonRpcMessage::error(error, Some(request_id)),
                    era,
                );
            }
        };
        return event_request(state, &auth_status, custom, request_id, era).await;
    }

    // A legacy request belongs to a session, which only `initialize` opens.
    let session_id = match &req.request {
        _ if modern => None,
        ClientRequest::InitializeRequest(init_req) => {
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
            let _ = state.sessions.set_client_type(&session_id, client_type);
            let policy_context = legacy_http_policy_context(
                state,
                &auth_status,
                request_id.clone(),
                Arc::clone(&trace_id),
            );
            bind_session_to_caller(state, &session_id, &auth_status, &policy_context)?;
            if init_req.params.capabilities.roots.is_some() {
                state.roots_capable_sessions.insert(session_id.clone(), ());
            }
            state
                .client_capabilities
                .insert(session_id.clone(), init_req.params.capabilities.clone());
            Some(session_id)
        }
        _ => {
            let session_id = extract_session_id(headers)?;
            state.sessions.validate(&session_id)?;
            Some(session_id)
        }
    };
    let ctx = HttpDownstreamContext::new(
        state,
        auth_status,
        session_id.as_deref(),
        request_id.clone(),
        trace_id,
        request_meta.as_ref(),
    );

    if matches!(req.request, ClientRequest::CallToolRequest(_)) {
        // A call through a tunnel dies if nothing is sent for too long. A
        // client that reads event streams gets one once the call runs long,
        // kept open until the answer is ready.
        if !modern
            && headers
                .get(header::ACCEPT)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|accept| accept.contains("text/event-stream"))
        {
            let router = Arc::clone(&state.router);
            let call = async move {
                let outcome = crate::dispatch::handle(&router, &ctx, &method, params).await;
                dispatch_message(outcome, request_id)
            };
            return answer_or_keep_open(call, LONG_CALL_AFTER).await;
        }
        let downstream = crate::dispatch::DownstreamContext::downstream_call_context(&ctx);
        let downstream_target = downstream.notification_target();
        let mut modern_notifications = modern.then(|| state.router.subscribe_notifications());
        // Streamable HTTP has no other channel to say a modern caller went
        // away: a dropped POST cancels the call it carried.
        let mut cancellation_guard = HttpRequestCancellationGuard {
            router: Arc::clone(&state.router),
            context: downstream,
            armed: modern,
        };
        let message = dispatch_message(
            crate::dispatch::handle(&state.router, &ctx, &method, params).await,
            request_id,
        );
        cancellation_guard.disarm();
        if let Some(receiver) = modern_notifications.as_mut() {
            let notifications = drain_targeted_notifications(receiver, &downstream_target);
            if !notifications.is_empty() {
                return modern_sse_response(&notifications, &message);
            }
        }
        return json_response_for_era(&message, era);
    }

    let mut outcome = crate::dispatch::handle(&state.router, &ctx, &method, params).await;
    if matches!(req.request, ClientRequest::DiscoverRequest(_))
        && let Ok(result) = outcome.as_mut()
        && events_discoverable(state, &ctx.auth_status)
    {
        tracing::info!(
            method = "server/discover",
            status = 200,
            events_advertised = true,
            "MCP Events discovery outcome"
        );
        result["capabilities"]["events"] = json!({});
    }
    let message = dispatch_message(outcome, request_id);
    match (&req.request, session_id) {
        (ClientRequest::InitializeRequest(_), Some(session_id)) => {
            json_response_with_session(&session_id, &message)
        }
        _ => json_response_for_era(&message, era),
    }
}

fn dispatch_message(
    outcome: Result<serde_json::Value, McpError>,
    request_id: RequestId,
) -> ServerJsonRpcMessage {
    match outcome {
        Ok(result) => ServerJsonRpcMessage::response(
            ServerResult::CustomResult(CustomResult::new(result)),
            request_id,
        ),
        Err(error) => ServerJsonRpcMessage::error(error, Some(request_id)),
    }
}

/// MCP Events: an HTTP-only, modern-only surface whose callers are OAuth
/// grants. See `crate::events`.
async fn event_request(
    state: &Arc<HttpState>,
    auth_status: &AuthStatus,
    custom: &CustomRequest,
    request_id: RequestId,
    era: crate::protocol::ProtocolEra,
) -> Result<Response, HttpError> {
    let subscription_method = match custom.method.as_str() {
        "events/subscribe" => Some("events/subscribe"),
        "events/unsubscribe" => Some("events/unsubscribe"),
        _ => None,
    };
    if let Some(method) = subscription_method {
        tracing::info!(
            method,
            status = "received",
            "MCP Events subscription request"
        );
    }
    let sources = event_sources_for(state, auth_status);
    let has_scope = sources
        .as_ref()
        .is_some_and(|(claims, _)| claims.scopes.iter().any(|s| s == EVENT_SCOPE));
    // Advertise a scope challenge only to an active client that can see
    // events. This is incremental consent, never a grant or an event
    // subscription.
    if let Some(method) = subscription_method
        && sources.is_some()
        && !has_scope
        && let Some(manager) = &state.downstream_oauth
    {
        tracing::info!(
            method,
            status = 403,
            missing_scope = EVENT_SCOPE,
            "MCP Events subscription outcome"
        );
        return Err(HttpError::InsufficientScopeWithMetadata {
            metadata_url: protected_resource_metadata_url(manager.base_url()),
            scope: EVENT_SCOPE.to_string(),
        });
    }
    let params = custom.params.clone().unwrap_or_else(|| json!({}));
    // Static catalog discovery must precede incremental subscription consent.
    let result = match sources.filter(|_| subscription_method.is_none() || has_scope) {
        None => Err(crate::events::denied()),
        Some((claims, sources)) => match custom.method.as_str() {
            "events/list" => {
                if params
                    .as_object()
                    .is_none_or(|p| p.keys().any(|k| k != "_meta" && k != "cursor"))
                    || params.get("cursor").is_some_and(|v| !v.is_null())
                {
                    Err(crate::events::invalid("invalid event list parameters"))
                } else {
                    tracing::info!(
                        method = "events/list",
                        status = 200,
                        "MCP Events discovery outcome"
                    );
                    let events: Vec<_> = sources
                        .iter()
                        .flat_map(|source| source.catalog(&claims.client_id))
                        .collect();
                    Ok(json!({ "events": events }))
                }
            }
            method => match params["name"]
                .as_str()
                .and_then(|name| sources.iter().find(|source| source.owns(name)))
            {
                None => Err(crate::events::invalid("unsupported event")),
                Some(source) if method == "events/subscribe" => {
                    source.subscribe(&claims.client_id, &params).await
                }
                Some(source) => source.unsubscribe(&claims.client_id, &params).await,
            },
        },
    };
    if let Some(method) = subscription_method {
        tracing::info!(
            method,
            status = if result.is_ok() {
                "200"
            } else {
                "jsonrpc_error"
            },
            "MCP Events subscription outcome"
        );
    }
    let response = match result {
        Ok(mut value) => {
            crate::protocol::rewrite_modern_result(&mut value);
            ServerJsonRpcMessage::response(
                ServerResult::CustomResult(CustomResult::new(value)),
                request_id,
            )
        }
        Err(error) => ServerJsonRpcMessage::error(
            McpError::new(
                ErrorCode(error.code),
                error.message,
                error.reason.map(|reason| json!({"reason":reason})),
            ),
            Some(request_id),
        ),
    };
    json_response_for_era(&response, era)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Whether a session opened under `grant` may be sent `notification`. See
/// [`crate::proxy::ToolRouter::client_may_be_told`].
fn may_be_told(
    router: &crate::proxy::ToolRouter,
    grant: Option<&str>,
    notification: &ProtocolNotification,
) -> bool {
    let key = crate::downstream_identity::remote_access_key(grant);
    router.client_may_be_told(Some(&key), notification)
}

/// Resolve which broadcast notifications this principal may observe.
///
/// The SSE fan-out task is shared by every HTTP session and holds no request
/// context, so the decision has to be made here, at `initialize`, while the
/// full policy input is still in hand. `decide_method` stays the single source
/// of policy; this only records its verdict per session.
fn broadcast_audience_for(context: &DownstreamCallContext) -> crate::session::BroadcastAudience {
    use crate::protocol::MethodFamily;
    crate::session::BroadcastAudience {
        tools: context
            .policy_decision(MethodFamily::ToolsList)
            .is_allowed(),
        resources: context
            .policy_decision(MethodFamily::ResourcesList)
            .is_allowed(),
        prompts: context
            .policy_decision(MethodFamily::PromptsList)
            .is_allowed(),
        logging: context
            .policy_decision(MethodFamily::LoggingRead)
            .is_allowed(),
    }
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

/// A session opened with one client's token answers only to that client.
/// Anyone else is told what they would be told about a session that does
/// not exist.
fn session_is_the_callers(
    headers: &HeaderMap,
    sessions: &SessionStore,
    auth_status: &AuthStatus,
) -> Result<(), HttpError> {
    let Ok(session_id) = extract_session_id(headers) else {
        return Ok(());
    };
    let Some(owner) = sessions.grant_of(&session_id) else {
        return Ok(());
    };
    match auth_status {
        AuthStatus::Authenticated(Some(claims)) if claims.client_id == owner => Ok(()),
        _ => Err(HttpError::SessionNotFound),
    }
}

/// Tie a session to the caller that proved who it is.
fn bind_session_to_caller(
    state: &HttpState,
    session_id: &str,
    auth_status: &AuthStatus,
    policy_context: &DownstreamCallContext,
) -> Result<(), HttpError> {
    // The grant is what Plug verified. The name a client gives is only what
    // it said, so settings for a remote client hang on the grant.
    if let AuthStatus::Authenticated(Some(claims)) = auth_status {
        let _ = state
            .sessions
            .set_grant(session_id, claims.client_id.clone());
        // The session lives only while the grant does. Asked of the grant
        // itself, so it holds however the grant ended: removed, expired, or
        // removed with the save failing afterward.
        let lifecycle = claims.principal_lifecycle.clone();
        let _ = state
            .sessions
            .set_access_check(session_id, Arc::new(move || lifecycle.is_active()));
    }

    // Record what this principal may observe on the shared SSE fan-out. Not
    // ignorable: until this lands the session denies every broadcast, so a
    // failure here would silently mute the client.
    state
        .sessions
        .set_broadcast_audience(session_id, broadcast_audience_for(policy_context))
        .map_err(Into::into)
}

/// A client that was connected when Plug restarted, or that stayed quiet past
/// the session timeout, goes on sending the session it was given. Many never
/// start a new one when told theirs is gone, and fail every call from then
/// on. The caller has already proved who it is, so its session is opened
/// again under the same id, as its own. What it said about itself at
/// `initialize` is not known again until it next initializes.
fn reopen_lost_session(state: &Arc<HttpState>, headers: &HeaderMap, auth_status: &AuthStatus) {
    let Ok(session_id) = extract_session_id(headers) else {
        return;
    };
    if !matches!(state.sessions.reopen_session(&session_id), Ok(true)) {
        return;
    }
    let policy_context = legacy_http_policy_context(
        state,
        auth_status,
        RequestId::Number(0),
        Arc::<str>::from(extract_trace_id(headers)),
    );
    // Binding fails only for a session that is already gone again.
    if bind_session_to_caller(state, &session_id, auth_status, &policy_context).is_err() {
        return;
    }
    // The client will not say `initialized` again, so the session is wired for
    // notifications here, and told its lists may have changed while it was
    // gone: what it holds was read from a Plug that no longer exists.
    let session_arc = Arc::<str>::from(session_id.as_str());
    state.router.register_downstream_bridge(
        NotificationTarget::Http {
            session_id: Arc::clone(&session_arc),
        },
        Arc::new(HttpBridge {
            state: Arc::clone(state),
            session_id: session_arc,
            capabilities: Default::default(),
        }),
    );
    for notification in [
        ProtocolNotification::ToolListChanged,
        ProtocolNotification::ResourceListChanged,
        ProtocolNotification::PromptListChanged,
    ] {
        if let Some(message) = notification_to_sse_message(&notification) {
            state.sessions.send_to_live_session(&session_id, message);
        }
    }
    tracing::info!(
        session = %session_id,
        "reopened a session this client still held and Plug no longer had"
    );
}

/// Validate that the session exists and is not expired.
fn validate_session_header(headers: &HeaderMap, sessions: &SessionStore) -> Result<(), HttpError> {
    let session_id = extract_session_id(headers)?;
    Ok(sessions.validate(&session_id)?)
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

fn drain_targeted_notifications(
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

/// How long a legacy tool call may run before its reply becomes an event
/// stream. Tunnels and proxies drop a request that is silent for about a
/// hundred seconds.
const LONG_CALL_AFTER: Duration = Duration::from_secs(45);

/// How often a held-open reply says it is still there.
const LONG_CALL_KEEP_ALIVE: Duration = Duration::from_secs(15);

/// Answer a legacy request with plain JSON when it finishes within `patience`.
/// Past that the reply becomes an event stream that carries keep-alives until
/// the one message is ready, so nothing between Plug and the client gives up
/// on a call that is still running.
async fn answer_or_keep_open(
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
fn modern_sse_response(
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
        HeaderValue::from_static(SUPPORTED_PROTOCOL_VERSION),
    );

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::super::oauth::{
        accepts_html, oauth_authorization_error_response, oauth_error_response, oauth_public_error,
        registration_rate_key,
    };
    use super::*;
    use crate::downstream_oauth::owner::tests::BrowserAuthenticator;
    use crate::downstream_oauth::{
        AuthorizationRequest, ClientRegistrationRequest, DownstreamOauthError,
    };
    use crate::proxy::RouterSnapshot;
    use crate::test_helpers::collect_sse_events;
    use axum::body::Body;
    use http::Request as HttpRequest;
    use std::collections::HashMap;
    use std::time::Duration;
    use tower::ServiceExt;

    include!("events_tests.rs");

    #[test]
    fn modern_tool_call_deserialization_preserves_progress_metadata() {
        let message: ClientJsonRpcMessage = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "test_tool_with_progress",
                "arguments": {},
                "_meta": {
                    "progressToken": "progress-test-1",
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }))
        .expect("modern tool call");

        let JsonRpcMessage::Request(request) = message else {
            panic!("request")
        };
        let ClientRequest::CallToolRequest(call) = request.request else {
            panic!("tools/call")
        };
        assert_eq!(
            call.extensions
                .get::<RequestMetaObject>()
                .and_then(RequestMetaObject::get_progress_token),
            Some(ProgressToken(NumberOrString::String(Arc::from(
                "progress-test-1"
            ))))
        );
    }

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
        let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
        Arc::new(
            HttpState::new(
                router,
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_auth_mode(crate::config::DownstreamAuthMode::Oauth)
            .with_downstream_oauth(Some(manager)),
        )
    }

    async fn oauth_test_client(
        manager: &crate::downstream_oauth::DownstreamOauthManager,
        client_name: &str,
    ) -> crate::downstream_oauth::ClientRegistrationResponse {
        let redirect_uri = "https://client.example.com/callback";
        manager
            .register_client(
                ClientRegistrationRequest {
                    redirect_uris: vec![redirect_uri.to_string()],
                    client_name: Some(client_name.to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: Some(vec!["authorization_code".to_string()]),
                    response_types: Some(vec!["code".to_string()]),
                    scope: None,
                },
                "public-ui-test",
            )
            .await
            .expect("register test client")
    }

    async fn oauth_test_consent(
        manager: &crate::downstream_oauth::DownstreamOauthManager,
        client_name: &str,
    ) -> crate::downstream_oauth::ConsentRequest {
        let redirect_uri = "https://client.example.com/callback";
        let registration = oauth_test_client(manager, client_name).await;
        manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &registration.client_id,
                redirect_uri,
                state: "public-ui-state",
                code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: &manager.resource(),
            })
            .await
            .expect("begin test authorization")
    }

    fn oauth_authorize_request(client_id: &str) -> HttpRequest<Body> {
        HttpRequest::builder()
            .method("GET")
            .uri(format!(
                "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fclient.example.com%2Fcallback&state=public-ui-state&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&scope=tools%3Aread&resource=https%3A%2F%2Fplug.example.com%2Fmcp"
            ))
            .header(header::HOST, "plug.example.com")
            .header(header::ACCEPT, "text/html")
            .body(Body::empty())
            .expect("authorization request")
    }

    async fn enroll_test_owner(
        manager: &crate::downstream_oauth::DownstreamOauthManager,
    ) -> BrowserAuthenticator {
        let authenticator = BrowserAuthenticator::new();
        let bootstrap = manager
            .create_owner_bootstrap()
            .await
            .expect("create owner bootstrap");
        let challenge = manager
            .start_owner_registration(&bootstrap)
            .await
            .expect("start owner registration");
        let response = authenticator.registration_response(
            &challenge.public_key.challenge,
            "plug.example.com",
            "https://plug.example.com",
            true,
        );
        manager
            .finish_owner_registration(&challenge.ceremony_id, response)
            .await
            .expect("finish owner registration");
        authenticator
    }

    fn oauth_tools_list_request(access_token: &str, session_id: &str) -> HttpRequest<Body> {
        legacy_oauth_session_request(access_token, session_id, 2, "tools/list")
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
                crate::config::is_loopback_host(host),
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
                !crate::config::is_loopback_host(host),
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

    fn sse_event_id(event: &str) -> Option<u64> {
        event.lines().find_map(|line| {
            line.strip_prefix("id: ")
                .and_then(|value| value.trim().parse::<u64>().ok())
        })
    }

    fn test_state_with_router_config(router_config: crate::proxy::RouterConfig) -> Arc<HttpState> {
        let sm = Arc::new(crate::server::ServerManager::new());
        let router = Arc::new(ToolRouter::new(sm, router_config));
        Arc::new(HttpState::new(
            router,
            Arc::new(crate::session::SessionStore::new(1800, 100)),
            CancellationToken::new(),
        ))
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

    fn test_state() -> Arc<HttpState> {
        test_state_with_router_config(crate::proxy::RouterConfig::default())
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
            tools_devin: Arc::new(Vec::new()),
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
        DownstreamCallContext::http(
            "task-client",
            RequestId::Number(1),
            crate::types::ClientType::Unknown,
        )
        .with_protocol(
            crate::protocol::ProtocolEra::Modern,
            crate::protocol::MODERN_PROTOCOL_VERSION,
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
        issue_test_oauth_grant(manager, scope).await.1.access_token
    }

    /// Same flow as `issue_test_oauth_token`, but keeps the client id and the
    /// refresh token, which rotation and reuse-detection tests need.
    async fn issue_test_oauth_grant(
        manager: &crate::downstream_oauth::DownstreamOauthManager,
        scope: &str,
    ) -> (String, crate::downstream_oauth::TokenResponsePayload) {
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
        let tokens = manager
            .exchange_authorization_code(
                &registration.client_id,
                &code,
                redirect_uri,
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
                &manager.resource(),
            )
            .await
            .expect("exchange authorization code");
        (registration.client_id, tokens)
    }

    #[tokio::test]
    async fn token_endpoint_rejects_unsupported_auth_without_consuming_refresh_token() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let (client_id, grant) = issue_test_oauth_grant(&manager, "tools:read").await;
        let refresh_token = grant.refresh_token.expect("refresh token");
        let app = build_router(oauth_test_state_with_manager(manager));
        for (extra, authorization) in [
            ("&client_assertion=fake-assertion", None),
            (
                "&client_assertion_type=urn%3Aietf%3Aparams%3Aoauth%3Aclient-assertion-type%3Ajwt-bearer",
                None,
            ),
            ("&client_assertion=&client_assertion_type=", None),
            ("", Some("Basic ZmFrZTpmYWtl")),
            ("", Some("Bearer fake-token")),
            ("", None),
        ] {
            let mut request = HttpRequest::builder()
                .method("POST")
                .uri("/oauth/token")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
            if let Some(authorization) = authorization {
                request = request.header(header::AUTHORIZATION, authorization);
            }
            let response = app.clone().oneshot(request.body(Body::from(format!(
                "grant_type=refresh_token&client_id={client_id}&refresh_token={refresh_token}&resource=https%3A%2F%2Fplug.example.com%2Fmcp{extra}"
            ))).unwrap()).await.unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 10_000)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if extra.is_empty() && authorization.is_none() {
                assert_eq!(status, StatusCode::OK);
                assert!(value["access_token"].is_string());
            } else {
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{extra} {authorization:?}"
                );
                assert_eq!(value["error"], "invalid_client");
                assert!(value.get("access_token").is_none());
            }
        }
    }

    #[tokio::test]
    async fn a_restarted_client_renews_at_the_assumed_address_without_a_resource() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let (client_id, grant) = issue_test_oauth_grant(&manager, "tools:read").await;
        let refresh_token = grant.refresh_token.expect("refresh token");
        let app = build_router(oauth_test_state_with_manager(manager));
        let post = |body: String| {
            HttpRequest::builder()
                .method("POST")
                .uri("/token")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap()
        };

        let response = app
            .clone()
            .oneshot(post(format!(
                "grant_type=refresh_token&client_id={client_id}&refresh_token={refresh_token}"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 10_000)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value["access_token"].is_string());

        // A first sign-in still has to say which resource it is for.
        let response = app
            .oneshot(post(format!(
                "grant_type=authorization_code&client_id={client_id}&code=x&redirect_uri=x&code_verifier=x"
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn legacy_oauth_principal_scopes_gate_method_families() {
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
            Arc::from("legacy-oauth-scope-gate"),
        );

        assert!(
            context.principal().is_some(),
            "verified principal is preserved"
        );
        assert!(
            context.has_principal_lifecycle(),
            "revocation lifecycle is preserved"
        );
        for family in [
            crate::protocol::MethodFamily::ToolsList,
            crate::protocol::MethodFamily::ToolsCall,
        ] {
            assert!(
                context.policy_decision(family).is_allowed(),
                "granted tools:read scope must keep {family:?} allowed"
            );
        }
        for family in [
            crate::protocol::MethodFamily::ResourcesList,
            crate::protocol::MethodFamily::ResourcesRead,
            crate::protocol::MethodFamily::ResourcesSubscribe,
            crate::protocol::MethodFamily::PromptsList,
            crate::protocol::MethodFamily::PromptsGet,
            crate::protocol::MethodFamily::Completion,
            crate::protocol::MethodFamily::Tasks,
            crate::protocol::MethodFamily::Listeners,
        ] {
            assert_eq!(
                context.policy_decision(family),
                crate::protocol::PolicyDecision::Deny(
                    crate::protocol::ProtocolOutcome::PermissionDenied
                ),
                "ungranted {family:?} must deny with PermissionDenied in the legacy era"
            );
        }
        assert_eq!(
            crate::protocol::ProtocolOutcome::PermissionDenied
                .encode(crate::protocol::ProtocolEra::Legacy)
                .code,
            -32005,
            "legacy-era permission denial must encode as JSON-RPC -32005"
        );
    }

    #[tokio::test]
    async fn a_session_answers_only_to_the_client_that_opened_it() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let access_token = issue_test_oauth_token(&manager, "tools:read").await;
        let claims = match manager
            .validate_access_token_for(&access_token, &[], &manager.resource())
            .await
        {
            AccessTokenValidation::Valid(claims) => claims,
            other => panic!("issued token must validate, got {other:?}"),
        };
        let sessions = crate::session::SessionStore::new(60, 10);
        let theirs = sessions.create_session().unwrap();
        let someone_elses = sessions.create_session().unwrap();
        let unowned = sessions.create_session().unwrap();
        sessions
            .set_grant(&theirs, claims.client_id.clone())
            .unwrap();
        sessions
            .set_grant(&someone_elses, "another-client".to_string())
            .unwrap();
        let with = |session_id: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(
                SESSION_ID_HEADER,
                HeaderValue::from_str(session_id).unwrap(),
            );
            headers
        };
        let caller = AuthStatus::Authenticated(Some(claims));

        assert!(session_is_the_callers(&with(&theirs), &sessions, &caller).is_ok());
        assert!(session_is_the_callers(&with(&unowned), &sessions, &caller).is_ok());
        assert!(session_is_the_callers(&HeaderMap::new(), &sessions, &caller).is_ok());
        assert!(matches!(
            session_is_the_callers(&with(&someone_elses), &sessions, &caller),
            Err(HttpError::SessionNotFound)
        ));
        assert!(matches!(
            session_is_the_callers(&with(&theirs), &sessions, &AuthStatus::NoAuthRequired),
            Err(HttpError::SessionNotFound)
        ));
    }

    #[tokio::test]
    async fn broadcast_audience_follows_the_principal_scopes() {
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
        let scoped = broadcast_audience_for(&legacy_http_policy_context(
            &state,
            &AuthStatus::Authenticated(Some(claims)),
            RequestId::Number(92),
            Arc::from("broadcast-audience-scoped"),
        ));

        assert!(scoped.tools, "tools:read admits tool list notifications");
        assert!(
            !scoped.resources,
            "a token without resources:read must not observe resource list changes"
        );
        assert!(
            !scoped.prompts,
            "a token without prompts:read must not observe prompt list changes"
        );
        assert!(
            !scoped.logging,
            "a token without logging:read must not observe upstream log output"
        );

        // `logging:read` is what admits log delivery -- not `logging:configure`,
        // which only permits `logging/setLevel`. A grant holding the write
        // scope alone still hears nothing.
        for (scope, expected) in [("logging:configure", false), ("logging:read", true)] {
            let manager = isolated_oauth_manager(vec![scope.to_string()]);
            let token = issue_test_oauth_token(&manager, scope).await;
            let claims = match manager
                .validate_access_token_for(&token, &[], &manager.resource())
                .await
            {
                AccessTokenValidation::Valid(claims) => claims,
                other => panic!("issued token must validate, got {other:?}"),
            };
            let audience = broadcast_audience_for(&legacy_http_policy_context(
                &state,
                &AuthStatus::Authenticated(Some(claims)),
                RequestId::Number(94),
                Arc::from("broadcast-audience-logging"),
            ));
            assert_eq!(
                audience.logging,
                expected,
                "{scope} must {} admit log delivery",
                if expected { "" } else { "not" }
            );
        }

        // A loopback listener that requires no auth keeps the full audience,
        // so turning auth off does not silence notifications.
        let loopback = broadcast_audience_for(&legacy_http_policy_context(
            &state,
            &AuthStatus::NoAuthRequired,
            RequestId::Number(93),
            Arc::from("broadcast-audience-loopback"),
        ));
        assert_eq!(
            loopback,
            crate::session::BroadcastAudience::unrestricted(),
            "a no-auth loopback listener has no scopes to narrow"
        );

        // The stored default must deny, because a session is visible to the
        // fan-out task from `create_session` until initialize records the real
        // audience, and anything delivered in that window is replayed later.
        assert_eq!(
            crate::session::BroadcastAudience::default(),
            crate::session::BroadcastAudience {
                tools: false,
                resources: false,
                prompts: false,
                logging: false,
            },
            "an unresolved audience must not admit broadcasts"
        );
    }

    fn legacy_oauth_session_request(
        access_token: &str,
        session_id: &str,
        id: u64,
        method: &str,
    ) -> HttpRequest<Body> {
        HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
            .header(SESSION_ID_HEADER, session_id)
            .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
            .body(Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method
                })
                .to_string(),
            ))
            .expect("legacy session request")
    }

    async fn legacy_oauth_session(app: &axum::Router, access_token: &str) -> String {
        let initialize = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
            .body(Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
                        "capabilities": {},
                        "clientInfo": { "name": "legacy-scope-test", "version": "1.0" }
                    }
                })
                .to_string(),
            ))
            .expect("initialize request");
        let response = app.clone().oneshot(initialize).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "initialize must succeed");
        response
            .headers()
            .get(SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("session ID")
            .to_string()
    }

    #[tokio::test]
    async fn a_session_opened_with_an_oauth_token_records_the_grant_behind_it() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let access_token = issue_test_oauth_token(&manager, "tools:read").await;
        let state = oauth_test_state_with_manager(manager);
        let app = build_router(Arc::clone(&state));
        let session_id = legacy_oauth_session(&app, &access_token).await;

        let oauth = state.downstream_oauth.as_ref().expect("OAuth manager");
        let client_id = match oauth
            .validate_access_token_for(
                &access_token,
                &["tools:read".to_string()],
                &oauth.resource(),
            )
            .await
        {
            crate::downstream_oauth::AccessTokenValidation::Valid(claims) => claims,
            other => panic!("token must validate: {other:?}"),
        };
        let claims = client_id;
        let client_id = claims.client_id.clone();
        let snapshots = state.sessions.snapshots();
        let session = snapshots
            .iter()
            .find(|snapshot| snapshot.session_id == session_id)
            .expect("session is tracked");
        assert_eq!(session.grant.as_deref(), Some(client_id.as_str()));

        // Every request carries the same key, session or no session, so a
        // stateless request is placed as surely as a legacy one.
        let context = |auth: &AuthStatus| {
            apply_http_auth(
                DownstreamCallContext::http(
                    "any",
                    RequestId::Number(1),
                    crate::types::ClientType::Unknown,
                ),
                auth,
                None,
            )
        };
        assert_eq!(
            context(&AuthStatus::Authenticated(Some(claims)))
                .client_key
                .as_deref(),
            Some(format!("oauth:{client_id}").as_str())
        );
        // A shared bearer and an open loopback listener cannot tell their
        // callers apart, so those callers share one key.
        for auth in [AuthStatus::Authenticated(None), AuthStatus::NoAuthRequired] {
            assert_eq!(
                context(&auth).client_key.as_deref(),
                Some(crate::downstream_identity::SHARED_REMOTE_CLIENT_KEY)
            );
        }
    }

    #[tokio::test]
    async fn legacy_tools_read_only_token_is_denied_resources_over_http() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let access_token = issue_test_oauth_token(&manager, "tools:read").await;
        let app = build_router(oauth_test_state_with_manager(manager));
        let session_id = legacy_oauth_session(&app, &access_token).await;

        let tools = app
            .clone()
            .oneshot(legacy_oauth_session_request(
                &access_token,
                &session_id,
                2,
                "tools/list",
            ))
            .await
            .unwrap();
        assert_eq!(tools.status(), StatusCode::OK);
        let tools_body = axum::body::to_bytes(tools.into_body(), usize::MAX)
            .await
            .unwrap();
        let tools_value: serde_json::Value = serde_json::from_slice(&tools_body).unwrap();
        assert!(
            tools_value["result"]["tools"].is_array(),
            "granted tools:read must keep tools/list working, got {tools_value}"
        );

        let resources = app
            .oneshot(legacy_oauth_session_request(
                &access_token,
                &session_id,
                3,
                "resources/list",
            ))
            .await
            .unwrap();
        assert_eq!(
            resources.status(),
            StatusCode::OK,
            "legacy denial stays inside an HTTP 200 envelope"
        );
        let resources_body = axum::body::to_bytes(resources.into_body(), usize::MAX)
            .await
            .unwrap();
        let resources_value: serde_json::Value = serde_json::from_slice(&resources_body).unwrap();
        assert_eq!(
            resources_value["error"]["code"],
            serde_json::json!(-32005),
            "ungranted resources/list must deny with -32005, got {resources_value}"
        );
    }

    #[tokio::test]
    async fn legacy_default_grant_token_reaches_resources_and_prompts_over_http() {
        let default_scopes: Vec<String> = crate::protocol::DEFAULT_DOWNSTREAM_OAUTH_SCOPES
            .iter()
            .map(ToString::to_string)
            .collect();
        let manager = isolated_oauth_manager(default_scopes.clone());
        let access_token = issue_test_oauth_token(&manager, &default_scopes.join(" ")).await;
        let app = build_router(oauth_test_state_with_manager(manager));
        let session_id = legacy_oauth_session(&app, &access_token).await;

        for (id, method) in [(2, "resources/list"), (3, "prompts/list")] {
            let response = app
                .clone()
                .oneshot(legacy_oauth_session_request(
                    &access_token,
                    &session_id,
                    id,
                    method,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(
                value.get("error").is_none(),
                "default-grant token must not be denied {method}, got {value}"
            );
            assert!(
                value.get("result").is_some(),
                "default-grant token must get a {method} result, got {value}"
            );
        }
    }

    #[test]
    fn registration_rate_key_ignores_forwarding_headers_from_untrusted_peers() {
        use std::net::IpAddr;

        fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(
                    axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    HeaderValue::from_str(value).unwrap(),
                );
            }
            map
        }

        let remote: IpAddr = "203.0.113.7".parse().unwrap();
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();

        // A client connecting straight to the listener cannot buy itself a
        // fresh budget by claiming an address, however it spells the claim.
        for spoof in [
            vec![("cf-connecting-ip", "198.51.100.1")],
            vec![("x-forwarded-for", "198.51.100.1")],
            vec![("x-forwarded-for", "198.51.100.1, 198.51.100.2")],
        ] {
            assert_eq!(
                registration_rate_key(Some(remote), &headers(&spoof)),
                "203.0.113.7",
                "a non-loopback peer must be bucketed by its own address"
            );
        }

        // Behind the same-host tunnel the edge-written header is the caller.
        assert_eq!(
            registration_rate_key(
                Some(loopback),
                &headers(&[("cf-connecting-ip", "198.51.100.1")])
            ),
            "198.51.100.1"
        );

        // `x-forwarded-for` is appended to, so the client's own value sits on
        // the left and the nearest proxy's on the right. Only the right one is
        // written by something we trust.
        assert_eq!(
            registration_rate_key(
                Some(loopback),
                &headers(&[("x-forwarded-for", "198.51.100.1, 203.0.113.9")])
            ),
            "203.0.113.9",
            "the rightmost forwarded entry is the trusted one"
        );

        // Nothing usable to key on still has to produce a bucket.
        assert_eq!(
            registration_rate_key(
                Some(loopback),
                &headers(&[("x-forwarded-for", "not-an-ip")])
            ),
            "127.0.0.1"
        );
        assert_eq!(
            registration_rate_key(None, &headers(&[("cf-connecting-ip", "198.51.100.1")])),
            "unknown-peer",
            "without connection info no header is trustworthy"
        );
    }

    /// Everything the upstream union can offer, so the projection has something
    /// to remove in every family.
    fn every_legacy_capability() -> ServerCapabilities {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(Default::default());
        capabilities.resources = Some(Default::default());
        capabilities.prompts = Some(Default::default());
        capabilities.completions = Some(Default::default());
        capabilities.logging = Some(serde_json::Map::new());
        capabilities.experimental = Some(
            [(
                crate::protocol::LEGACY_TASKS_CAPABILITY_KEY.to_string(),
                serde_json::Map::new(),
            )]
            .into_iter()
            .collect(),
        );
        capabilities
    }

    #[test]
    fn legacy_initialize_projects_capabilities_through_the_principal_scopes() {
        fn advertised(scopes: &[&str]) -> ServerCapabilities {
            let context = DownstreamCallContext::http(
                "cap-session",
                RequestId::Number(1),
                crate::types::ClientType::Unknown,
            )
            .with_authorization(
                crate::types::PrincipalId::configured_credential("cap-test", 0),
                scopes.iter().map(ToString::to_string),
            );
            crate::dispatch::project_capabilities(every_legacy_capability(), &context, false)
        }

        // Telling a client a capability exists and then denying every method
        // behind it is the inaccuracy this guards against. Before this
        // projection, legacy advertised the full upstream union to everyone.
        let tools_only = advertised(&["tools:read"]);
        assert!(tools_only.tools.is_some());
        assert!(
            tools_only.resources.is_none()
                && tools_only.prompts.is_none()
                && tools_only.completions.is_none()
                && tools_only.logging.is_none()
                && tools_only.experimental.is_none(),
            "a tools-only grant must not be offered the other families"
        );

        let resources_only = advertised(&["resources:read"]);
        assert!(resources_only.resources.is_some());
        assert!(resources_only.tools.is_none() && resources_only.prompts.is_none());

        // The logging capability covers both halves -- `logging/setLevel` and
        // log delivery -- so it is advertised only to a grant holding both
        // scopes. Half a grant would promise something the gate then refuses.
        assert!(
            advertised(&["logging:configure", "logging:read"])
                .logging
                .is_some()
        );
        for partial in [
            vec!["logging:configure"],
            vec!["logging:read"],
            vec!["tools:read"],
        ] {
            assert!(
                advertised(&partial).logging.is_none(),
                "{partial:?} must not be told the logging capability exists"
            );
        }

        // The full default grant sees the whole union, so this is not a
        // behavior change for a client that consented to everything.
        let full = advertised(&crate::protocol::DEFAULT_DOWNSTREAM_OAUTH_SCOPES);
        assert_eq!(full, every_legacy_capability());

        // The live deployment pins the six families that predate
        // `logging:configure`; those clients keep everything they use.
        let six: Vec<&str> = crate::protocol::DEFAULT_DOWNSTREAM_OAUTH_SCOPES
            .iter()
            .copied()
            .filter(|scope| *scope != "logging:configure")
            .collect();
        let pinned = advertised(&six);
        assert!(
            pinned.tools.is_some()
                && pinned.resources.is_some()
                && pinned.prompts.is_some()
                && pinned.completions.is_some(),
            "the pinned six-family grant must keep every family it can exercise"
        );
        assert!(pinned.logging.is_none());

        // A loopback listener that requires no auth keeps the whole union.
        let local = crate::dispatch::project_capabilities(
            every_legacy_capability(),
            &DownstreamCallContext::http(
                "loopback",
                RequestId::Number(2),
                crate::types::ClientType::Unknown,
            )
            .with_local_principal(HttpCredential::Loopback.principal()),
            false,
        );
        assert_eq!(local, every_legacy_capability());
    }

    #[tokio::test]
    async fn legacy_set_level_requires_the_logging_scope() {
        fn set_level_request(access_token: &str, session_id: &str) -> HttpRequest<Body> {
            HttpRequest::builder()
                .method("POST")
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
                .header(SESSION_ID_HEADER, session_id)
                .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 7,
                        "method": "logging/setLevel",
                        "params": { "level": "debug" }
                    })
                    .to_string(),
                ))
                .expect("setLevel request")
        }

        // `logging/setLevel` moves the effective level for every downstream
        // client and forwards it to every upstream, so a principal without the
        // scope must not reach it.
        let narrow_scopes = vec!["tools:read".to_string()];
        let narrow_manager = isolated_oauth_manager(narrow_scopes);
        let narrow_token = issue_test_oauth_token(&narrow_manager, "tools:read").await;
        let narrow_app = build_router(oauth_test_state_with_manager(narrow_manager));
        let narrow_session = legacy_oauth_session(&narrow_app, &narrow_token).await;
        let denied = narrow_app
            .oneshot(set_level_request(&narrow_token, &narrow_session))
            .await
            .unwrap();
        assert_eq!(
            denied.status(),
            StatusCode::OK,
            "legacy denial stays inside an HTTP 200 envelope"
        );
        let denied_body = axum::body::to_bytes(denied.into_body(), usize::MAX)
            .await
            .unwrap();
        let denied_value: serde_json::Value = serde_json::from_slice(&denied_body).unwrap();
        assert_eq!(
            denied_value["error"]["code"],
            serde_json::json!(-32005),
            "ungranted logging/setLevel must deny with -32005, got {denied_value}"
        );

        // The default grant carries the scope, so no ordinary client regresses.
        let default_scopes: Vec<String> = crate::protocol::DEFAULT_DOWNSTREAM_OAUTH_SCOPES
            .iter()
            .map(ToString::to_string)
            .collect();
        let manager = isolated_oauth_manager(default_scopes.clone());
        let access_token = issue_test_oauth_token(&manager, &default_scopes.join(" ")).await;
        let app = build_router(oauth_test_state_with_manager(manager));
        let session_id = legacy_oauth_session(&app, &access_token).await;
        let allowed = app
            .oneshot(set_level_request(&access_token, &session_id))
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
        let allowed_body = axum::body::to_bytes(allowed.into_body(), usize::MAX)
            .await
            .unwrap();
        let allowed_value: serde_json::Value = serde_json::from_slice(&allowed_body).unwrap();
        assert!(
            allowed_value.get("error").is_none(),
            "default-grant token must not be denied logging/setLevel, got {allowed_value}"
        );
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
        // Both revisions: the same port still serves `2025-11-25` through
        // `initialize`, so omitting it would tell a modern client that legacy
        // support had been dropped.
        assert_eq!(
            value["result"]["supportedVersions"],
            json!([SUPPORTED_PROTOCOL_VERSION, "2026-07-28"])
        );
        assert!(value["result"]["capabilities"]["extensions"].is_null());
        assert!(value["result"]["capabilities"]["experimental"].is_null());
        assert!(value["result"]["capabilities"]["logging"].is_null());
    }

    /// A modern caller as the dispatcher sees it, for the admission matrix.
    struct ModernTaskCaller {
        call: DownstreamCallContext,
        client: ClientCapabilities,
    }

    impl crate::dispatch::DownstreamContext for ModernTaskCaller {
        fn downstream_call_context(&self) -> DownstreamCallContext {
            self.call.clone()
        }

        fn session_bound(&self) -> bool {
            false
        }

        fn task_owner(&self) -> Result<crate::tasks::TaskOwner, McpError> {
            Ok(crate::downstream_identity::principal_task_owner(
                self.call.principal().expect("principal"),
            ))
        }

        fn modern_task_client(&self) -> Option<&ClientCapabilities> {
            Some(&self.client)
        }
    }

    fn admit_modern_task_operation(
        router: &ToolRouter,
        call: &DownstreamCallContext,
        meta: Option<&RequestMetaObject>,
    ) -> Result<crate::tasks::TaskOwner, McpError> {
        let caller = ModernTaskCaller {
            call: call.clone(),
            client: meta
                .and_then(RequestMetaObject::client_capabilities)
                .unwrap_or_default(),
        };
        crate::dispatch::admit_modern_task(router, &caller, call)
    }

    #[tokio::test]
    async fn modern_http_task_admission_matrix_is_consistent() {
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
            allowed.principal().expect("principal").owner_key()
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
        let state = Arc::new(
            HttpState::new(
                Arc::clone(&state.router),
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                state.cancel.clone(),
            )
            .with_auth_mode(crate::config::DownstreamAuthMode::Oauth)
            .with_downstream_oauth(Some(manager))
            .with_sse_channel_capacity(state.sse_channel_capacity)
            .with_allowed_origins(state.allowed_origins.clone()),
        );
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

    #[tokio::test]
    async fn modern_capability_projection_is_permission_filtered_and_bridge_free() {
        let state = test_state();
        state.router.set_modern_downstream_enabled(true);
        let context = DownstreamCallContext::http(
            "scoped-client",
            RequestId::Number(1),
            crate::types::ClientType::Unknown,
        )
        .with_protocol(
            crate::protocol::ProtocolEra::Modern,
            crate::protocol::MODERN_PROTOCOL_VERSION,
        )
        .with_modern_direction_enabled(true)
        .with_authorization(
            crate::types::PrincipalId::configured_credential("test", 1),
            ["tools:read".to_string()],
        );
        let capabilities = crate::dispatch::project_capabilities(
            state
                .router
                .synthesized_capabilities_for_client(context.client_type),
            &context,
            state.router.supports_tasks(),
        );
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
            Some(SUPPORTED_PROTOCOL_VERSION)
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
            .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
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
            Some(crate::protocol::MODERN_PROTOCOL_VERSION)
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
    async fn a_client_holding_a_session_plug_lost_is_served() {
        let state = test_state();
        let lost = uuid::Uuid::new_v4().to_string();
        let list = |id: u32, session: &str| {
            HttpRequest::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header(SESSION_ID_HEADER, session)
                .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "tools/list"
                    }))
                    .unwrap(),
                ))
                .unwrap()
        };

        let resp = build_router(state.clone())
            .oneshot(list(1, &lost))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(state.sessions.validate(&lost).is_ok());
        // It is told to read its lists again: what it holds came from a Plug
        // that is gone.
        let (stream, mut events) = tokio::sync::mpsc::channel(8);
        state.sessions.set_sse_sender(&lost, stream, None).unwrap();
        let mut told = Vec::new();
        while let Ok(event) = events.try_recv() {
            told.push(format!("{:?}", event.message));
        }
        assert_eq!(told.len(), 3, "{told:?}");
        assert!(told[0].contains("tools/list_changed"), "{told:?}");

        let resp = build_router(state)
            .oneshot(list(2, "never-issued"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

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

    /// A task created in a legacy session asks whether the session is still
    /// open after it registers its create guard, so a create racing DELETE is
    /// refused: the probe must see the session closed once DELETE returns.
    #[tokio::test]
    async fn the_task_owner_probe_sees_a_deleted_session_closed() {
        let state = test_state();
        let session_id = state.sessions.create_session().unwrap();
        let context = HttpDownstreamContext::new(
            &state,
            AuthStatus::Authenticated(None),
            Some(&session_id),
            RequestId::Number(1),
            Arc::from("trace"),
            None,
        );
        let probe = crate::dispatch::DownstreamContext::owner_liveness_probe(&context)
            .expect("a legacy session has a probe");
        assert!(probe());

        let response = build_router(Arc::clone(&state))
            .oneshot(
                HttpRequest::builder()
                    .method("DELETE")
                    .uri("/mcp")
                    .header(SESSION_ID_HEADER, &session_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!probe());
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
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: std::collections::HashMap::new(),
            prompt_routes: std::collections::HashMap::new(),
            tool_definition_fingerprints: std::collections::HashMap::new(),
            tool_risk_inventory: std::collections::HashMap::new(),
        });

        let owner = crate::downstream_identity::http_session_task_owner(&session_id);
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

    fn bare_router() -> Arc<ToolRouter> {
        let sm = Arc::new(crate::server::ServerManager::new());
        Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()))
    }

    #[tokio::test]
    async fn a_session_that_times_out_is_released_without_being_asked() {
        let router = bare_router();
        enable_test_task_surface(&router);
        let state = HttpState::new(
            Arc::clone(&router),
            Arc::new(crate::session::SessionStore::new(0, 100)),
            CancellationToken::new(),
        );
        let session_id = state.sessions.create_session().unwrap();
        router.set_client_log_level(&session_id, LoggingLevel::Debug);
        let owner = crate::downstream_identity::http_session_task_owner(&session_id);
        router
            .enqueue_tool_task("Mock__echo", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue a task for the session");

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(state.sessions.validate(&session_id).is_err());

        tokio::time::timeout(Duration::from_secs(5), async {
            while router.task_count_for_owner(&owner).await > 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the lapsed session's tasks are released");
        assert_eq!(router.log_level(), LoggingLevel::Warning);
    }

    #[tokio::test]
    async fn revoking_a_client_ends_its_sessions_and_the_tasks_it_owns() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let (client_id, _tokens) = issue_test_oauth_grant(&manager, "tools:read").await;
        let router = bare_router();
        enable_test_task_surface(&router);
        let state = HttpState::new(
            Arc::clone(&router),
            Arc::new(crate::session::SessionStore::new(1800, 100)),
            CancellationToken::new(),
        )
        .with_downstream_oauth(Some(manager.clone()));
        let theirs = state.sessions.create_session().unwrap();
        state
            .sessions
            .set_grant(&theirs, client_id.clone())
            .unwrap();
        let other = state.sessions.create_session().unwrap();
        let resource = manager.resource();
        let owner = HttpCredential::Grant {
            issuer: manager.base_url(),
            client_id: &client_id,
            resource: &resource,
        }
        .task_owner(None);
        router
            .enqueue_tool_task("Mock__echo", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue a task for the client");

        assert!(matches!(
            state.revoke_downstream_client(&client_id).await,
            ClientRevocation::Revoked
        ));
        assert!(state.sessions.validate(&theirs).is_err());
        assert!(state.sessions.validate(&other).is_ok());
        assert_eq!(router.task_count_for_owner(&owner).await, 0);
        assert!(matches!(
            state.revoke_downstream_client(&client_id).await,
            ClientRevocation::UnknownClient
        ));
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
        let owner = crate::downstream_identity::http_session_task_owner(&session_id);

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
        let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
        let state = Arc::new(
            HttpState::new(
                router,
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_allowed_origins(vec![Arc::from("https://claude.ai")]),
        );
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
            .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 3. ping with session, and no MCP-Protocol-Version header
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
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a request without MCP-Protocol-Version must be served"
        );

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
            .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
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
            .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
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
        let state = test_state_with_router_config(crate::proxy::RouterConfig::from(
            &crate::config::Config {
                meta_tool_mode: true,
                ..Default::default()
            },
        ));

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
            tools_devin: Arc::new(Vec::new()),
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
            .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
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
            .header(PROTOCOL_VERSION_HEADER, SUPPORTED_PROTOCOL_VERSION)
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
            Some(SUPPORTED_PROTOCOL_VERSION)
        );

        let resp_body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(
            json["result"]["protocolVersion"],
            SUPPORTED_PROTOCOL_VERSION
        );
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
            serde_json::json!([SUPPORTED_PROTOCOL_VERSION])
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

    // -- Bearer auth middleware tests --

    fn test_state_with_auth(token: &str) -> Arc<HttpState> {
        let sm = Arc::new(crate::server::ServerManager::new());
        let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
        Arc::new(
            HttpState::new(
                router,
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_auth_mode(crate::config::DownstreamAuthMode::Bearer)
            .with_auth_token(Some(Arc::from(token))),
        )
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
        let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
        let state = Arc::new(
            HttpState::new(
                router,
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_auth_mode(crate::config::DownstreamAuthMode::Oauth)
            .with_downstream_oauth(Some(isolated_oauth_manager(vec!["tools:read".to_string()]))),
        );
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
        let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
        let state = Arc::new(
            HttpState::new(
                router,
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_auth_mode(crate::config::DownstreamAuthMode::Oauth)
            .with_downstream_oauth(Some(isolated_oauth_manager(vec!["tools:read".to_string()]))),
        );
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
        let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
        let state = Arc::new(
            HttpState::new(
                router,
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_auth_mode(crate::config::DownstreamAuthMode::Oauth)
            .with_downstream_oauth(Some(isolated_oauth_manager(vec![
                "tools:read".to_string(),
                "offline_access".to_string(),
            ]))),
        );
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

    #[test]
    fn owner_oauth_errors_have_stable_codes_and_fail_closed_messages() {
        let cases = [
            (
                DownstreamOauthError::OwnerNotEnrolled,
                "owner_not_enrolled",
                "Finish Plug owner setup on the Mac running Plug.",
            ),
            (
                DownstreamOauthError::OwnerChallengeExpired,
                "owner_challenge_expired",
                "Approval expired. Select Allow again.",
            ),
            (
                DownstreamOauthError::InvalidOwnerAssertion,
                "owner_verification_failed",
                "Passkey verification failed. No access was granted.",
            ),
            (
                DownstreamOauthError::AuthorizationExpired(
                    crate::downstream_oauth::ValidatedAuthorizationCallback {
                        redirect_uri: "https://client.example.com/callback".to_string(),
                        state: "state".to_string(),
                    },
                ),
                "authorization_expired",
                "This connection request expired. Return to your MCP client and select Connect again.",
            ),
            (
                DownstreamOauthError::Persistence("secret path and token".to_string()),
                "server_error",
                "Plug could not save this authorization. No access was granted.",
            ),
        ];

        for (error, expected_code, expected_message) in cases {
            let (_, code, message) = oauth_public_error(&error);
            assert_eq!(code, expected_code);
            assert_eq!(message, expected_message);
            assert!(!message.contains("secret path and token"));
        }
    }

    #[test]
    fn authorization_expiration_debug_redacts_validated_callback() {
        let error = DownstreamOauthError::AuthorizationExpired(
            crate::downstream_oauth::ValidatedAuthorizationCallback {
                redirect_uri: "https://client.example.com/private/callback".to_string(),
                state: "secret-state-value".to_string(),
            },
        );

        let debug = format!("{error:?}");
        assert!(!debug.contains("private/callback"));
        assert!(!debug.contains("secret-state-value"));
    }

    async fn oauth_error_json(response: Response) -> serde_json::Value {
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
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
            .expect("OAuth error body");
        serde_json::from_slice(&body).expect("OAuth error JSON")
    }

    async fn oauth_error_html(response: Response) -> String {
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
        assert_eq!(
            response
                .headers()
                .get("Content-Security-Policy")
                .and_then(|value| value.to_str().ok()),
            Some(
                "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"
            )
        );
        let body = axum::body::to_bytes(response.into_body(), 10_000)
            .await
            .expect("OAuth error body");
        String::from_utf8(body.to_vec()).expect("OAuth error HTML")
    }

    fn assert_oauth_html_security_headers(response: &Response) {
        for (name, expected) in [
            (header::CACHE_CONTROL.as_str(), "no-store"),
            (
                "content-security-policy",
                "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
            ),
            ("referrer-policy", "no-referrer"),
            ("x-content-type-options", "nosniff"),
            ("x-frame-options", "DENY"),
        ] {
            assert_eq!(
                response
                    .headers()
                    .get(name)
                    .and_then(|value| value.to_str().ok()),
                Some(expected),
                "missing or wrong {name}"
            );
        }
    }

    #[tokio::test]
    async fn consent_page_never_references_loopback_approval() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        enroll_test_owner(&manager).await;
        let registration = oauth_test_client(&manager, "Matrix <Owner>").await;
        let app = build_router(oauth_test_state_with_manager(manager));

        let response = app
            .oneshot(oauth_authorize_request(&registration.client_id))
            .await
            .expect("consent response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_oauth_html_security_headers(&response);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("consent body");
        let body = String::from_utf8(body.to_vec()).expect("consent HTML");
        assert!(!body.contains("127.0.0.1"));
        assert!(!body.contains("localhost"));
        assert!(!body.contains("/_plug/oauth/authorize"));
        assert!(body.contains("/oauth/consent/challenge"));
        assert!(body.contains("Allow Matrix &lt;Owner&gt; to use Plug?"));
        assert!(body.contains("Unverified dynamically registered client"));
        assert!(body.contains("https://client.example.com/callback"));
        assert!(body.contains("https://plug.example.com/mcp"));
        assert!(body.contains("tools:read"));
        assert!(body.contains("This request expires in 5 minutes."));
        assert!(body.contains("Allow with Touch ID or passkey"));
    }

    #[tokio::test]
    async fn consent_page_without_owner_explains_setup_and_has_no_allow_action() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let registration = oauth_test_client(&manager, "Setup client").await;
        let app = build_router(oauth_test_state_with_manager(manager));

        let response = app
            .oneshot(oauth_authorize_request(&registration.client_id))
            .await
            .expect("consent response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("consent body");
        let body = String::from_utf8(body.to_vec()).expect("consent HTML");
        assert!(body.contains("plug auth owner enroll"));
        assert!(!body.contains("Allow with Touch ID or passkey"));
    }

    #[tokio::test]
    async fn consent_page_shows_local_callback_hostname_and_port() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let redirect_uri = "http://127.0.0.1:8787/callback";
        let registration = manager
            .register_client(
                ClientRegistrationRequest {
                    redirect_uris: vec![redirect_uri.to_string()],
                    client_name: Some("Local callback test".to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: Some(vec!["authorization_code".to_string()]),
                    response_types: Some(vec!["code".to_string()]),
                    scope: None,
                },
                "local-callback-ui-test",
            )
            .await
            .expect("register local callback client");
        let request = HttpRequest::builder()
            .method("GET")
            .uri(format!(
                "/oauth/authorize?response_type=code&client_id={}&redirect_uri=http%3A%2F%2F127.0.0.1%3A8787%2Fcallback&state=local-state&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&scope=tools%3Aread&resource=https%3A%2F%2Fplug.example.com%2Fmcp",
                registration.client_id
            ))
            .header(header::HOST, "plug.example.com")
            .header(header::ACCEPT, "text/html")
            .body(Body::empty())
            .expect("local callback authorization request");
        let response = build_router(oauth_test_state_with_manager(manager))
            .oneshot(request)
            .await
            .expect("consent response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("consent body");
        let body = String::from_utf8(body.to_vec()).expect("consent HTML");
        assert!(body.contains("Callback: <strong>127.0.0.1:8787</strong>"));
        assert!(body.contains("Local app callback:"));
        assert!(body.contains("returns to <code>127.0.0.1:8787</code>"));
    }

    #[tokio::test]
    async fn public_approval_requires_exact_origin_and_owner_assertion() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        enroll_test_owner(&manager).await;
        let consent = oauth_test_consent(&manager, "Origin test").await;
        let app = build_router(oauth_test_state_with_manager(manager));

        for (host, origin) in [
            ("plug.example.com", None),
            ("plug.example.com", Some("https://evil.example.com")),
            ("evil.example.com", Some("https://plug.example.com")),
        ] {
            let mut request = HttpRequest::builder()
                .method("POST")
                .uri("/oauth/consent/challenge")
                .header(header::HOST, host)
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            let response = app
                .clone()
                .oneshot(
                    request
                        .body(Body::from(
                            json!({"consent_id": consent.consent_id}).to_string(),
                        ))
                        .expect("challenge request"),
                )
                .await
                .expect("challenge response");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn public_post_rejects_duplicate_host_or_origin_headers() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        enroll_test_owner(&manager).await;
        let consent = oauth_test_consent(&manager, "Duplicate header test").await;
        let app = build_router(oauth_test_state_with_manager(manager));

        for duplicate in [header::HOST, header::ORIGIN] {
            let mut request = HttpRequest::builder()
                .method("POST")
                .uri("/oauth/consent/challenge")
                .header(header::HOST, "plug.example.com")
                .header(header::ORIGIN, "https://plug.example.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"consent_id": consent.consent_id}).to_string(),
                ))
                .expect("challenge request");
            let duplicate_value = if duplicate == header::HOST {
                "plug.example.com"
            } else {
                "https://plug.example.com"
            };
            request
                .headers_mut()
                .append(duplicate, HeaderValue::from_static(duplicate_value));
            let response = app
                .clone()
                .oneshot(request)
                .await
                .expect("challenge response");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn public_consent_challenge_uses_exact_same_origin_json_contract() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        enroll_test_owner(&manager).await;
        let consent = oauth_test_consent(&manager, "Challenge test").await;
        let app = build_router(oauth_test_state_with_manager(manager));
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/challenge")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"consent_id": consent.consent_id}).to_string(),
            ))
            .expect("challenge request");

        let response = app.oneshot(request).await.expect("challenge response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_oauth_html_security_headers(&response);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("challenge body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("challenge JSON");
        assert!(
            body["ceremony_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(body["public_key"]["challenge"].as_str().is_some());
    }

    #[tokio::test]
    async fn expired_validated_consent_returns_exact_callback_for_browser_navigation() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        enroll_test_owner(&manager).await;
        let consent = oauth_test_consent(&manager, "Expired route test").await;
        manager
            .expire_pending_consent_for_tests(&consent.consent_id)
            .await;
        let app = build_router(oauth_test_state_with_manager(manager));
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/challenge")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"consent_id": consent.consent_id}).to_string(),
            ))
            .expect("challenge request");

        let response = app.oneshot(request).await.expect("challenge response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::LOCATION).is_none());
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("challenge callback body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("callback JSON");
        assert_eq!(
            body["redirect_uri"],
            "https://client.example.com/callback?error=authorization_expired&error_description=This+connection+request+expired.+Return+to+your+MCP+client+and+select+Connect+again.&state=public-ui-state"
        );
    }

    #[tokio::test]
    async fn expired_validated_denial_returns_exact_callback_for_browser_navigation() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let consent = oauth_test_consent(&manager, "Expired denial test").await;
        manager
            .expire_pending_consent_for_tests(&consent.consent_id)
            .await;
        let app = build_router(oauth_test_state_with_manager(manager));
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/decision")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "decision": "deny",
                    "consent_id": consent.consent_id,
                    "csrf_token": consent.csrf_token,
                })
                .to_string(),
            ))
            .expect("denial request");

        let response = app.oneshot(request).await.expect("denial response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::LOCATION).is_none());
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("denial callback body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("callback JSON");
        assert_eq!(
            body["redirect_uri"],
            "https://client.example.com/callback?error=authorization_expired&error_description=This+connection+request+expired.+Return+to+your+MCP+client+and+select+Connect+again.&state=public-ui-state"
        );
    }

    #[tokio::test]
    async fn unknown_consent_never_redirects_to_an_unvalidated_callback() {
        let app = build_router(oauth_test_state());
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/challenge")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"consent_id": "unknown"}).to_string()))
            .expect("challenge request");

        let response = app.oneshot(request).await.expect("challenge response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response.headers().get(header::LOCATION).is_none());
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("unknown consent body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("OAuth error JSON");
        assert!(body.get("redirect_uri").is_none());
    }

    #[tokio::test]
    async fn public_denial_is_same_origin_json_and_idempotent() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let consent = oauth_test_consent(&manager, "Denial test").await;
        let app = build_router(oauth_test_state_with_manager(manager));
        let body = json!({
            "decision": "deny",
            "consent_id": consent.consent_id,
            "csrf_token": consent.csrf_token,
        })
        .to_string();

        let mut locations = Vec::new();
        for _ in 0..2 {
            let request = HttpRequest::builder()
                .method("POST")
                .uri("/oauth/consent/decision")
                .header(header::HOST, "plug.example.com")
                .header(header::ORIGIN, "https://plug.example.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.clone()))
                .expect("denial request");
            let response = app.clone().oneshot(request).await.expect("denial response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("denial body");
            let body: serde_json::Value = serde_json::from_slice(&body).expect("denial JSON");
            locations.push(
                body["redirect_uri"]
                    .as_str()
                    .expect("redirect URI")
                    .to_string(),
            );
        }
        assert_eq!(locations[0], locations[1]);
        assert!(locations[0].contains("error=access_denied"));
        assert!(locations[0].contains("state=public-ui-state"));
    }

    #[tokio::test]
    async fn public_approval_verifies_owner_and_replays_exact_redirect() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let mut authenticator = enroll_test_owner(&manager).await;
        let consent = oauth_test_consent(&manager, "Approval test").await;
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .expect("owner approval challenge");
        let credential = authenticator.authentication_response(
            &challenge.public_key.challenge,
            "plug.example.com",
            "https://plug.example.com",
            true,
        );
        let body = json!({
            "decision": "approve",
            "ceremony_id": challenge.ceremony_id,
            "credential": {
                "id": credential.id,
                "authenticatorData": credential.authenticator_data,
                "signature": credential.signature,
                "clientDataJSON": credential.client_data_json,
                "userHandle": credential.user_handle,
            },
        })
        .to_string();
        let app = build_router(oauth_test_state_with_manager(manager));
        let mut locations = Vec::new();
        for _ in 0..2 {
            let request = HttpRequest::builder()
                .method("POST")
                .uri("/oauth/consent/decision")
                .header(header::HOST, "plug.example.com")
                .header(header::ORIGIN, "https://plug.example.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.clone()))
                .expect("approval request");
            let response = app
                .clone()
                .oneshot(request)
                .await
                .expect("approval response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("approval body");
            let body: serde_json::Value = serde_json::from_slice(&body).expect("approval JSON");
            locations.push(
                body["redirect_uri"]
                    .as_str()
                    .expect("redirect URI")
                    .to_string(),
            );
        }
        assert_eq!(locations[0], locations[1]);
        assert!(locations[0].starts_with("https://client.example.com/callback?code="));
        assert!(locations[0].contains("state=public-ui-state"));
    }

    #[tokio::test]
    async fn public_approval_challenge_survives_manager_restart() {
        let state_path = std::env::temp_dir().join(format!(
            "plug-oauth-public-restart-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        let config = crate::downstream_oauth::DownstreamOauthConfig {
            public_base_url: "https://plug.example.com".to_string(),
            oauth_scopes: vec!["tools:read".to_string()],
            local_port: 3282,
        };
        let manager = crate::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            config.clone(),
            state_path.clone(),
        )
        .expect("restart test manager");
        let mut authenticator = enroll_test_owner(&manager).await;
        let consent = oauth_test_consent(&manager, "Restart test").await;
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .expect("owner approval challenge");
        let credential = authenticator.authentication_response(
            &challenge.public_key.challenge,
            "plug.example.com",
            "https://plug.example.com",
            true,
        );
        drop(manager);

        let restarted = crate::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            config,
            state_path.clone(),
        )
        .expect("restarted manager");
        let app = build_router(oauth_test_state_with_manager(restarted));
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/decision")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "decision": "approve",
                    "ceremony_id": challenge.ceremony_id,
                    "credential": {
                        "id": credential.id,
                        "authenticatorData": credential.authenticator_data,
                        "signature": credential.signature,
                        "clientDataJSON": credential.client_data_json,
                        "userHandle": credential.user_handle,
                    }
                })
                .to_string(),
            ))
            .expect("approval request");
        let response = app.oneshot(request).await.expect("approval response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("approval body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("approval JSON");
        assert!(
            body["redirect_uri"]
                .as_str()
                .is_some_and(|uri| uri.starts_with("https://client.example.com/callback?code="))
        );
        let _ = std::fs::remove_file(state_path);
    }

    #[tokio::test]
    async fn public_enrollment_routes_complete_signed_registration() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let bootstrap = manager
            .create_owner_bootstrap()
            .await
            .expect("owner bootstrap");
        let app = build_router(oauth_test_state_with_manager(manager));
        let page = HttpRequest::builder()
            .method("GET")
            .uri("/oauth/owner/enroll")
            .header(header::HOST, "plug.example.com")
            .body(Body::empty())
            .expect("enrollment page request");
        let page = app.clone().oneshot(page).await.expect("enrollment page");
        assert_eq!(page.status(), StatusCode::OK);
        assert_oauth_html_security_headers(&page);
        let page_body = axum::body::to_bytes(page.into_body(), 64 * 1024)
            .await
            .expect("enrollment page body");
        assert!(!String::from_utf8_lossy(&page_body).contains(&bootstrap));

        let challenge_request = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/owner/enroll/challenge")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"bootstrap": bootstrap}).to_string()))
            .expect("registration challenge request");
        let challenge_response = app
            .clone()
            .oneshot(challenge_request)
            .await
            .expect("registration challenge response");
        assert_eq!(challenge_response.status(), StatusCode::OK);
        let challenge_body = axum::body::to_bytes(challenge_response.into_body(), 64 * 1024)
            .await
            .expect("registration challenge body");
        let challenge: serde_json::Value =
            serde_json::from_slice(&challenge_body).expect("registration challenge JSON");
        let ceremony_id = challenge["ceremony_id"].as_str().expect("ceremony ID");
        let challenge_value = challenge["public_key"]["challenge"]
            .as_str()
            .expect("registration challenge");
        let authenticator = BrowserAuthenticator::new();
        let credential = authenticator.registration_response(
            challenge_value,
            "plug.example.com",
            "https://plug.example.com",
            true,
        );
        let credential = json!({
            "id": credential.id,
            "transports": credential.transports,
            "attestationObject": credential.attestation_object,
            "clientDataJSON": credential.client_data_json,
        });
        let complete_request = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/owner/enroll/complete")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"ceremony_id": ceremony_id, "credential": credential}).to_string(),
            ))
            .expect("registration complete request");
        let complete_response = app
            .oneshot(complete_request)
            .await
            .expect("registration complete response");
        assert_eq!(complete_response.status(), StatusCode::OK);
        let complete_body = axum::body::to_bytes(complete_response.into_body(), 64 * 1024)
            .await
            .expect("registration complete body");
        let complete: serde_json::Value =
            serde_json::from_slice(&complete_body).expect("registration complete JSON");
        assert!(complete["credential_id"].as_str().is_some());
        assert!(complete.get("passkey").is_none());
    }

    #[tokio::test]
    async fn oauth_assets_are_first_party_immutable_and_legacy_route_is_gone() {
        use sha2::Digest as _;

        let app = build_router(oauth_test_state());
        use crate::http::oauth_ui;
        for (path, source, content_type) in [
            (
                "/oauth/assets/consent.js",
                oauth_ui::CONSENT_JAVASCRIPT,
                oauth_ui::JAVASCRIPT_TYPE,
            ),
            (
                "/oauth/assets/enroll.js",
                oauth_ui::ENROLL_JAVASCRIPT,
                oauth_ui::JAVASCRIPT_TYPE,
            ),
            (
                "/oauth/assets/plug.css",
                oauth_ui::STYLESHEET,
                oauth_ui::STYLESHEET_TYPE,
            ),
            (
                "/oauth/assets/plug-icon.svg",
                oauth_ui::ICON,
                oauth_ui::ICON_TYPE,
            ),
        ] {
            let expected_etag = format!(
                "\"plug-{}\"",
                hex::encode(sha2::Sha256::digest(source.as_bytes()))
            );
            let request = HttpRequest::builder()
                .method("GET")
                .uri(path)
                .header(header::HOST, "plug.example.com")
                .body(Body::empty())
                .expect("asset request");
            let response = app.clone().oneshot(request).await.expect("asset response");
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some(content_type)
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some("public, max-age=31536000, immutable")
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::ETAG)
                    .and_then(|value| value.to_str().ok()),
                Some(expected_etag.as_str())
            );
            for (name, expected) in [
                (
                    "content-security-policy",
                    "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
                ),
                ("referrer-policy", "no-referrer"),
                ("x-content-type-options", "nosniff"),
                ("x-frame-options", "DENY"),
            ] {
                assert_eq!(
                    response
                        .headers()
                        .get(name)
                        .and_then(|value| value.to_str().ok()),
                    Some(expected),
                    "missing or wrong {name}"
                );
            }
            let body = axum::body::to_bytes(response.into_body(), 128 * 1024)
                .await
                .expect("asset body");
            let body = String::from_utf8(body.to_vec()).expect("text asset");
            assert_eq!(body, source);
            assert!(!body.contains("operator_token"));
            assert!(!body.contains("127.0.0.1"));
            assert!(!body.contains("localhost"));
        }

        let legacy = HttpRequest::builder()
            .method("POST")
            .uri("/_plug/oauth/authorize")
            .header(header::HOST, "127.0.0.1:3282")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("consent_id=unknown&decision=approve"))
            .expect("legacy request");
        let legacy = app.oneshot(legacy).await.expect("legacy response");
        assert_eq!(legacy.status(), StatusCode::UNAUTHORIZED);
        assert!(legacy.headers().get(header::LOCATION).is_none());
    }

    #[tokio::test]
    async fn malformed_authorization_query_uses_safe_negotiated_oauth_error() {
        for (accept, expected_content_type) in [
            ("text/html", "text/html; charset=utf-8"),
            ("application/json", "application/json"),
        ] {
            let app = build_router(oauth_test_state());
            let request = HttpRequest::builder()
                .method("GET")
                .uri("/oauth/authorize?response_type=code&client_id=missing-fields")
                .header(header::ACCEPT, accept)
                .body(Body::empty())
                .unwrap();

            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some(expected_content_type)
            );
            if accept == "text/html" {
                let body = oauth_error_html(response).await;
                assert!(body.contains("invalid_request"));
                assert!(!body.contains("missing field"));
            } else {
                let body = oauth_error_json(response).await;
                assert_eq!(body["error"], "invalid_request");
                assert!(!body.to_string().contains("missing field"));
            }
        }
    }

    #[tokio::test]
    async fn malformed_registration_and_token_bodies_use_oauth_json_even_for_html_accept() {
        let app = build_router(oauth_test_state());
        let registration = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/register")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "text/html")
            .body(Body::from("{not-json"))
            .unwrap();
        let registration = app.clone().oneshot(registration).await.unwrap();
        assert_eq!(registration.status(), StatusCode::BAD_REQUEST);
        let registration = oauth_error_json(registration).await;
        assert_eq!(registration["error"], "invalid_client_metadata");
        assert!(!registration.to_string().contains("not-json"));

        let token = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/token")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "text/html")
            .body(Body::from("{not-form"))
            .unwrap();
        let token = app.oneshot(token).await.unwrap();
        assert_eq!(token.status(), StatusCode::BAD_REQUEST);
        let token = oauth_error_json(token).await;
        assert_eq!(token["error"], "invalid_request");
        assert!(!token.to_string().contains("not-form"));
    }

    #[tokio::test]
    async fn malformed_and_unknown_public_consent_are_safe_and_wrong_origin_is_forbidden() {
        let app = build_router(oauth_test_state());
        let malformed = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/decision")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{not-json"))
            .unwrap();
        let malformed = app.clone().oneshot(malformed).await.unwrap();
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
        let malformed = oauth_error_json(malformed).await;
        assert_eq!(malformed["error"], "invalid_request");
        assert!(!malformed.to_string().contains("not-json"));

        let unknown = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/challenge")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"consent_id":"unknown"}"#))
            .unwrap();
        let unknown = app.clone().oneshot(unknown).await.unwrap();
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
        let unknown = oauth_error_json(unknown).await;
        assert_eq!(unknown["error"], "owner_not_enrolled");

        let wrong_origin = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/decision")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://evil.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{not-json"))
            .unwrap();
        let wrong_origin = app.oneshot(wrong_origin).await.unwrap();
        assert_eq!(wrong_origin.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn public_consent_persistence_failure_is_hardened_and_nondisclosing() {
        let state_path = std::env::temp_dir().join(format!(
            "plug-oauth-http-persistence-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        let manager = crate::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            crate::downstream_oauth::DownstreamOauthConfig {
                public_base_url: "https://plug.example.com".to_string(),
                oauth_scopes: vec!["tools:read".to_string()],
                local_port: 3282,
            },
            state_path.clone(),
        )
        .expect("isolated OAuth manager");
        let mut authenticator = enroll_test_owner(&manager).await;
        let registration = manager
            .register_client(
                ClientRegistrationRequest {
                    redirect_uris: vec!["https://client.example.com/callback".to_string()],
                    client_name: Some("Persistence test".to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: Some(vec!["authorization_code".to_string()]),
                    response_types: Some(vec!["code".to_string()]),
                    scope: None,
                },
                "persistence-test",
            )
            .await
            .expect("register client");
        let consent = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &registration.client_id,
                redirect_uri: &registration.redirect_uris[0],
                state: "state-123",
                code_challenge: "challenge",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: &manager.resource(),
            })
            .await
            .expect("begin authorization");
        let challenge = manager
            .start_owner_approval(&consent.consent_id)
            .await
            .expect("start owner approval");
        let credential = authenticator.authentication_response(
            &challenge.public_key.challenge,
            "plug.example.com",
            "https://plug.example.com",
            true,
        );
        std::fs::remove_file(&state_path).expect("remove state file");
        std::fs::create_dir(&state_path).expect("block state-file rename");
        let app = build_router(oauth_test_state_with_manager(manager));
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/decision")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "decision": "approve",
                    "ceremony_id": challenge.ceremony_id,
                    "credential": {
                        "id": credential.id,
                        "authenticatorData": credential.authenticator_data,
                        "signature": credential.signature,
                        "clientDataJSON": credential.client_data_json,
                        "userHandle": credential.user_handle,
                    }
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = oauth_error_json(response).await;
        assert_eq!(body["error"], "server_error");
        assert!(
            !body
                .to_string()
                .contains(state_path.to_string_lossy().as_ref())
        );
        assert!(!body.to_string().contains("OAuth state persistence failed"));
        std::fs::remove_dir(&state_path).expect("remove state blocker");
        let _ = std::fs::remove_file(state_path.with_extension("json.tmp"));
    }

    #[tokio::test]
    async fn persistence_error_payloads_never_disclose_internal_text() {
        let secret = "database password=correct-horse-battery-staple";
        for response in [
            oauth_error_response(&DownstreamOauthError::Persistence(secret.to_string())),
            oauth_authorization_error_response(&DownstreamOauthError::Persistence(
                secret.to_string(),
            )),
        ] {
            let body = axum::body::to_bytes(response.into_body(), 10_000)
                .await
                .expect("OAuth error body");
            assert!(!String::from_utf8_lossy(&body).contains(secret));
        }
        let (_, code, description) =
            oauth_public_error(&DownstreamOauthError::Persistence(secret.to_string()));
        assert_eq!(code, "server_error");
        assert!(!description.contains(secret));
    }

    #[tokio::test]
    async fn authorization_code_http_replay_returns_invalid_grant() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let redirect_uri = "https://client.example.com/callback";
        let registration = manager
            .register_client(
                ClientRegistrationRequest {
                    redirect_uris: vec![redirect_uri.to_string()],
                    client_name: Some("Replay test".to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: Some(vec!["authorization_code".to_string()]),
                    response_types: Some(vec!["code".to_string()]),
                    scope: None,
                },
                "replay-test",
            )
            .await
            .expect("register client");
        let consent = manager
            .begin_authorization(AuthorizationRequest {
                response_type: "code",
                client_id: &registration.client_id,
                redirect_uri,
                state: "state-123",
                code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
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
        let app = build_router(oauth_test_state_with_manager(manager));
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("grant_type", "authorization_code")
            .append_pair("client_id", &registration.client_id)
            .append_pair("code", &code)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair(
                "code_verifier",
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            )
            .append_pair("resource", "https://plug.example.com/mcp");
        let form = form.finish();

        for (attempt, expected_status) in [
            ("first", StatusCode::OK),
            ("replay", StatusCode::BAD_REQUEST),
        ] {
            let request = HttpRequest::builder()
                .method("POST")
                .uri("/oauth/token")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form.clone()))
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), expected_status, "{attempt} exchange");
            if attempt == "replay" {
                let body = oauth_error_json(response).await;
                assert_eq!(body["error"], "invalid_grant");
            }
        }
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
        let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
        let state = Arc::new(
            HttpState::new(
                router,
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_auth_mode(crate::config::DownstreamAuthMode::Oauth)
            .with_downstream_oauth(Some(isolated_oauth_manager(vec![
                "tools:read".to_string(),
                "offline_access".to_string(),
            ]))),
        );
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
        let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
        let state = Arc::new(
            HttpState::new(
                router,
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_auth_mode(crate::config::DownstreamAuthMode::Oauth)
            .with_downstream_oauth(Some(isolated_oauth_manager(vec!["tools:read".to_string()]))),
        );
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
    async fn dcr_oauth_lifecycle_acceptance_matrix() {
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
        let mut authenticator = enroll_test_owner(&manager).await;
        let http_state = oauth_test_state_with_manager(manager.clone());
        let app = build_router(http_state.clone());

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
            .header(header::HOST, "plug.example.com")
            .body(Body::empty())
            .unwrap();
        let authorize_resp = app.clone().oneshot(authorize_req).await.unwrap();
        assert_eq!(authorize_resp.status(), StatusCode::OK);
        let consent_body = axum::body::to_bytes(authorize_resp.into_body(), 20_000)
            .await
            .expect("consent body");
        let consent_html = String::from_utf8(consent_body.to_vec()).expect("consent html");
        let consent_id = consent_html
            .split("data-consent-id=\"")
            .nth(1)
            .and_then(|value| value.split('\"').next())
            .expect("consent id");

        let challenge_request = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/challenge")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"consent_id": consent_id}).to_string()))
            .unwrap();
        let challenge_response = app.clone().oneshot(challenge_request).await.unwrap();
        assert_eq!(challenge_response.status(), StatusCode::OK);
        let challenge_body = axum::body::to_bytes(challenge_response.into_body(), 20_000)
            .await
            .expect("approval challenge body");
        let challenge: serde_json::Value =
            serde_json::from_slice(&challenge_body).expect("approval challenge JSON");
        let ceremony_id = challenge["ceremony_id"]
            .as_str()
            .expect("approval ceremony ID");
        let challenge_value = challenge["public_key"]["challenge"]
            .as_str()
            .expect("approval challenge");
        let credential = authenticator.authentication_response(
            challenge_value,
            "plug.example.com",
            "https://plug.example.com",
            true,
        );

        let consent_req = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/consent/decision")
            .header(header::HOST, "plug.example.com")
            .header(header::ORIGIN, "https://plug.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "decision": "approve",
                    "ceremony_id": ceremony_id,
                    "credential": {
                        "id": credential.id,
                        "authenticatorData": credential.authenticator_data,
                        "signature": credential.signature,
                        "clientDataJSON": credential.client_data_json,
                        "userHandle": credential.user_handle,
                    }
                })
                .to_string(),
            ))
            .unwrap();
        let consent_resp = app.clone().oneshot(consent_req).await.unwrap();
        assert_eq!(consent_resp.status(), StatusCode::OK);
        let consent_body = axum::body::to_bytes(consent_resp.into_body(), 20_000)
            .await
            .expect("approval decision body");
        let consent: serde_json::Value =
            serde_json::from_slice(&consent_body).expect("approval decision JSON");
        let location = consent["redirect_uri"].as_str().expect("redirect location");
        let code = location
            .split("code=")
            .nth(1)
            .and_then(|v| v.split('&').next())
            .expect("authorization code")
            .to_string();

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
        assert_eq!(
            token_resp
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert_eq!(
            token_resp
                .headers()
                .get(header::PRAGMA)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
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
                "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
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
        drop(initialize_resp);

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
        assert_eq!(
            refresh_resp
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert_eq!(
            refresh_resp
                .headers()
                .get(header::PRAGMA)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
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

        // Replaying `refresh_token` here would revoke the whole family and kill
        // every token this test goes on to use. Reuse detection has its own
        // end-to-end coverage in
        // `replayed_refresh_token_returns_invalid_grant_and_kills_the_family`.

        drop(app);
        http_state.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), async {
            while Arc::strong_count(&http_state) > 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("initial HTTP state fan-out tasks stop");
        drop(http_state);
        drop(manager);
        let restarted = crate::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            oauth_config.clone(),
            state_path.clone(),
        )
        .expect("restarted OAuth manager");
        let restarted_http_state = oauth_test_state_with_manager(restarted.clone());
        let restarted_app = build_router(restarted_http_state.clone());

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
        drop(restarted_initialize);
        let restarted_list = restarted_app
            .clone()
            .oneshot(oauth_tools_list_request(
                &rotated_access_token,
                &restarted_session_id,
            ))
            .await
            .unwrap();
        assert_eq!(restarted_list.status(), StatusCode::OK);
        drop(restarted_list);

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

        restarted_http_state.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), async {
            while Arc::strong_count(&restarted_http_state) > 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("restarted HTTP state fan-out tasks stop");
        drop(restarted_http_state);
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

    /// RFC 9700 section 4.14.2: a refresh token presented after it was already
    /// spent means the chain leaked, so the whole rotation family dies — not
    /// just the replayed token. Covered end to end here because the damage is
    /// only visible through the resource endpoint, and it has to outlive a
    /// restart to be worth anything.
    #[tokio::test]
    async fn replayed_refresh_token_returns_invalid_grant_and_kills_the_family() {
        let state_path = std::env::temp_dir().join(format!(
            "plug-oauth-refresh-replay-{}.json",
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
        .expect("replay OAuth manager");
        let (client_id, grant) = issue_test_oauth_grant(&manager, "tools:read").await;
        let original_refresh_token = grant.refresh_token.expect("refresh token");
        let http_state = oauth_test_state_with_manager(manager.clone());
        let app = build_router(http_state.clone());

        let rotate = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "grant_type=refresh_token&client_id={client_id}&refresh_token={original_refresh_token}&resource=https%3A%2F%2Fplug.example.com%2Fmcp"
            )))
            .unwrap();
        let rotate = app.clone().oneshot(rotate).await.unwrap();
        assert_eq!(rotate.status(), StatusCode::OK);
        let rotate = axum::body::to_bytes(rotate.into_body(), 10_000)
            .await
            .expect("rotation body");
        let rotate: serde_json::Value = serde_json::from_slice(&rotate).expect("rotation JSON");
        let rotated_access_token = rotate["access_token"]
            .as_str()
            .expect("rotated access token")
            .to_string();
        let rotated_refresh_token = rotate["refresh_token"]
            .as_str()
            .expect("rotated refresh token")
            .to_string();

        // The rotated pair works before the replay, so the assertions after it
        // are measuring the replay and not some unrelated breakage.
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "replay-client", "version": "1.0" }
            }
        });
        let initialize_req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {rotated_access_token}"))
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
        drop(initialize_resp);
        let before_replay = app
            .clone()
            .oneshot(oauth_tools_list_request(&rotated_access_token, &session_id))
            .await
            .unwrap();
        assert_eq!(before_replay.status(), StatusCode::OK);
        drop(before_replay);

        // A second use within moments is the client racing itself; a replay
        // is one that comes later.
        manager.age_spent_refresh_tokens_for_tests().await;
        let replay = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "grant_type=refresh_token&client_id={client_id}&refresh_token={original_refresh_token}&resource=https%3A%2F%2Fplug.example.com%2Fmcp"
            )))
            .unwrap();
        let replay = app.clone().oneshot(replay).await.unwrap();
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
        let replay_body = axum::body::to_bytes(replay.into_body(), 10_000)
            .await
            .expect("replay body");
        let replay_value: serde_json::Value =
            serde_json::from_slice(&replay_body).expect("replay JSON");
        assert_eq!(replay_value["error"], "invalid_grant");

        // The replayed token was already dead. What the replay buys the
        // attacker's victim is the rest of the chain going with it.
        let after_replay = app
            .clone()
            .oneshot(oauth_tools_list_request(&rotated_access_token, &session_id))
            .await
            .unwrap();
        assert_eq!(after_replay.status(), StatusCode::UNAUTHORIZED);
        drop(after_replay);

        let rotated_refresh_after_replay = HttpRequest::builder()
            .method("POST")
            .uri("/oauth/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "grant_type=refresh_token&client_id={client_id}&refresh_token={rotated_refresh_token}&resource=https%3A%2F%2Fplug.example.com%2Fmcp"
            )))
            .unwrap();
        let rotated_refresh_after_replay = app
            .clone()
            .oneshot(rotated_refresh_after_replay)
            .await
            .unwrap();
        assert_eq!(
            rotated_refresh_after_replay.status(),
            StatusCode::BAD_REQUEST
        );

        drop(app);
        http_state.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), async {
            while Arc::strong_count(&http_state) > 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("HTTP state fan-out tasks stop");
        drop(http_state);
        drop(manager);

        // Revocation is only real if it is on disk: a restart must not resurrect
        // the family.
        let restarted = crate::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            oauth_config,
            state_path,
        )
        .expect("restarted OAuth manager");
        let restarted_app = build_router(oauth_test_state_with_manager(restarted));
        let after_restart = restarted_app
            .oneshot(oauth_tools_list_request(&rotated_access_token, &session_id))
            .await
            .unwrap();
        assert_eq!(after_restart.status(), StatusCode::UNAUTHORIZED);
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
        let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
        let state = Arc::new(
            HttpState::new(
                router,
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_auth_mode(crate::config::DownstreamAuthMode::Oauth)
            .with_downstream_oauth(Some(isolated_oauth_manager(vec!["tools:read".to_string()]))),
        );
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
