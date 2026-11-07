use super::discovery::discovery_router;
use super::events::{event_request, events_discoverable, ingest_router};
use super::fanout::drain_targeted_notifications;
use super::guard::{validate_bearer_auth, validate_modern_request, validate_origin};
use super::identity::{
    AuthStatus, HttpDownstreamContext, HttpRequestCancellationGuard, grant_task_owner,
};
use super::response::{
    LONG_CALL_AFTER, answer_or_keep_open, header_mismatch_response, json_response_for_era,
    json_response_with_session, mark_private, modern_sse_response,
};
use super::reverse::{handle_client_response, maybe_request_http_roots};
use super::session_binding::{
    bind_session_to_caller, extract_session_id, reopen_lost_session, session_is_the_callers,
    validate_session_header, wire_session,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Router, extract::DefaultBodyLimit};
use dashmap::DashMap;
use rmcp::ErrorData as McpError;
use rmcp::model::*;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tower_http::limit::RequestBodyLimitLayer;

use super::error::HttpError;
use super::sse::sse_stream_with_heartbeat;
use crate::events::EventSource;
use crate::mcp_http_headers::{
    TRACEPARENT_HEADER, inject_trace_context, validate_mirrored_headers,
    validate_required_mirrored_headers,
};
use crate::proxy::ToolRouter;
use crate::session::{SessionEnd, SessionStore};

pub(super) const SESSION_ID_HEADER: &str = "Mcp-Session-Id";

pub(super) const PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";
pub(super) const PLUG_TRACE_ID_HEADER: &str = "x-plug-trace-id";

use crate::protocol::SUPPORTED_PROTOCOL_VERSION;

/// How callers of `/mcp` prove who they are.
pub enum HttpAuth {
    /// No credential. Config validation allows this only for a listener no
    /// other machine can reach.
    Open,
    /// The one shared token every client presents.
    Bearer(Arc<str>),
    /// Access tokens Plug's own authorization server issued.
    Oauth(crate::downstream_oauth::DownstreamOauthManager),
}

pub struct HttpState {
    pub router: Arc<ToolRouter>,
    pub sessions: Arc<SessionStore>,
    pub(crate) cancel: CancellationToken,
    pub(crate) auth: HttpAuth,
    /// Where MCP Events come from. See `crate::events`.
    pub(crate) event_sources: Vec<Arc<dyn EventSource>>,
    pub(crate) sse_channel_capacity: usize,
    /// Browser origins allowed besides this Mac's own, exactly as written.
    /// Their hosts are also hosts a modern request may name in Host.
    pub(crate) allowed_origins: Vec<Arc<str>>,
    /// The host of `http.public_base_url`. A modern request may name it in
    /// Host; a page served from it is not thereby allowed.
    pub(crate) public_host: Option<String>,
    pub(crate) notification_task_started: AtomicBool,
    /// Reverse requests waiting for the client's answer, by session and id.
    pub(crate) pending_client_requests: Arc<PendingClientRequests>,
    pub(crate) reverse_request_counter: AtomicU64,
    /// What each session's client declared at `initialize`.
    pub(crate) client_capabilities: Arc<DashMap<String, ClientCapabilities>>,
    pub(crate) lapsed_capabilities: Arc<LapsedCapabilities>,
    /// Held while a lapsed session's teardown runs and while a session is
    /// reopened, so the one never undoes the other.
    pub(super) session_turnover: Arc<tokio::sync::Mutex<()>>,
}

pub(super) type PendingClientRequests = DashMap<(String, i64), oneshot::Sender<ClientResult>>;

/// The capabilities of sessions that timed out, newest last, for clients
/// that go on using their session. A client that never returns is forgotten
/// once this many others have lapsed after it.
#[derive(Default)]
pub(crate) struct LapsedCapabilities(
    std::sync::Mutex<std::collections::VecDeque<(String, ClientCapabilities)>>,
);

impl LapsedCapabilities {
    const KEPT: usize = 256;

    fn keep(&self, session_id: &str, capabilities: ClientCapabilities) {
        let mut kept = self.0.lock().expect("lapsed capabilities mutex poisoned");
        if kept.len() == Self::KEPT {
            kept.pop_front();
        }
        kept.push_back((session_id.to_string(), capabilities));
    }

    pub(super) fn take(&self, session_id: &str) -> Option<ClientCapabilities> {
        let mut kept = self.0.lock().expect("lapsed capabilities mutex poisoned");
        let index = kept.iter().position(|(id, _)| id == session_id)?;
        kept.remove(index).map(|(_, capabilities)| capabilities)
    }
}

/// What HTTP holds for its sessions outside the session store.
pub(super) struct HeldForSessions {
    router: Arc<ToolRouter>,
    pending_client_requests: Arc<PendingClientRequests>,
    client_capabilities: Arc<DashMap<String, ClientCapabilities>>,
    lapsed_capabilities: Arc<LapsedCapabilities>,
}

impl HeldForSessions {
    /// Release what is held for a session that has left the store. What the
    /// client of a session that timed out said at `initialize` is kept, so
    /// the session works as before if the client comes back to it.
    async fn release(&self, session_id: &str, lapsed: bool) {
        if let Some((_, capabilities)) = self.client_capabilities.remove(session_id)
            && lapsed
        {
            self.lapsed_capabilities.keep(session_id, capabilities);
        }
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
    /// Saving the revocation failed. The grant stopped working before the
    /// save was tried, so its sessions and tasks went all the same.
    SaveFailed(crate::downstream_oauth::DownstreamOauthError),
}

impl HttpState {
    /// State with [`HttpAuth::Open`], no events, no extra allowed origins,
    /// and the default SSE channel capacity.
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
            auth: HttpAuth::Open,
            event_sources: Vec::new(),
            sse_channel_capacity: crate::config::DEFAULT_SSE_CHANNEL_CAPACITY,
            allowed_origins: Vec::new(),
            public_host: None,
            notification_task_started: AtomicBool::new(false),
            pending_client_requests: Arc::default(),
            reverse_request_counter: AtomicU64::new(1),
            client_capabilities: Arc::default(),
            lapsed_capabilities: Arc::default(),
            session_turnover: Arc::default(),
        };
        let (lapsed_tx, mut lapsed_rx) = mpsc::unbounded_channel::<String>();
        state
            .sessions
            .report_lapses_to(lapsed_tx)
            .expect("a session store serves one HttpState");
        state.sessions.spawn_cleanup_task(state.cancel.clone());
        let held = state.held_for_sessions();
        let sessions = Arc::clone(&state.sessions);
        let turnover = Arc::clone(&state.session_turnover);
        tokio::spawn(async move {
            while let Some(session_id) = lapsed_rx.recv().await {
                let _turn = turnover.lock().await;
                // Its client came back for it before this ran, and has it again.
                if sessions.is_open(&session_id) {
                    continue;
                }
                held.release(&session_id, true).await;
            }
        });
        state
    }

    fn held_for_sessions(&self) -> HeldForSessions {
        HeldForSessions {
            router: Arc::clone(&self.router),
            pending_client_requests: Arc::clone(&self.pending_client_requests),
            client_capabilities: Arc::clone(&self.client_capabilities),
            lapsed_capabilities: Arc::clone(&self.lapsed_capabilities),
        }
    }

    #[must_use]
    pub fn with_auth(mut self, auth: HttpAuth) -> Self {
        self.auth = auth;
        self
    }

    /// The authorization server, when callers sign in with OAuth.
    pub fn downstream_oauth(&self) -> Option<&crate::downstream_oauth::DownstreamOauthManager> {
        match &self.auth {
            HttpAuth::Oauth(manager) => Some(manager),
            HttpAuth::Open | HttpAuth::Bearer(_) => None,
        }
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

    /// The public address a tunnel or proxy reaches this listener by.
    #[must_use]
    pub fn with_public_base_url(mut self, public_base_url: Option<&str>) -> Self {
        self.public_host = public_base_url.and_then(super::guard::host_of_url);
        self
    }

    /// End a session and release what Plug holds for it. Returns whether it
    /// was open.
    pub(crate) async fn end_session(&self, session_id: &str, why: SessionEnd) -> bool {
        if !self.sessions.end(session_id, why) {
            return false;
        }
        self.held_for_sessions().release(session_id, false).await;
        true
    }

    /// Take a downstream OAuth client's access away: its tokens stop
    /// working, its sessions end, and the tasks it owns as its principal go.
    pub async fn revoke_downstream_client(&self, client_id: &str) -> ClientRevocation {
        let Some(manager) = self.downstream_oauth() else {
            return ClientRevocation::OauthDisabled;
        };
        let revoked = manager.revoke_client(client_id).await;
        if matches!(revoked, Ok(false)) {
            return ClientRevocation::UnknownClient;
        }
        // Saved or not, the manager already refuses the grant's tokens, and
        // the task store's create guard keeps a create that is under way
        // from outliving this cleanup.
        for session_id in self.sessions.sessions_of(client_id) {
            self.end_session(&session_id, SessionEnd::Revoked).await;
        }
        self.router
            .cleanup_tasks_for_owner(&grant_task_owner(manager, client_id))
            .await;
        match revoked {
            Ok(_) => ClientRevocation::Revoked,
            Err(error) => ClientRevocation::SaveFailed(error),
        }
    }
}

pub fn build_router(state: Arc<HttpState>) -> Router {
    state.spawn_notification_fanout();

    // The last layer added runs first: the body limit, then auth, then the
    // Origin check, which an authenticated caller skips.
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
        .layer(RequestBodyLimitLayer::new(4 * 1024 * 1024))
        .with_state(state.clone());

    let mut router =
        discovery_router(Arc::clone(&state)).merge(ingest_router(&state.event_sources));
    if let HttpAuth::Oauth(manager) = &state.auth {
        router = router.merge(super::oauth::router(manager.clone()));
    }
    // axum runs a merged router's layers on its fallback too. A path Plug
    // does not serve is not found, whatever the `/mcp` guards would say.
    router
        .merge(mcp)
        .fallback(|| async { StatusCode::NOT_FOUND })
}

pub(super) async fn post_mcp(
    State(state): State<Arc<HttpState>>,
    axum::Extension(auth_status): axum::Extension<AuthStatus>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, HttpError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/json") {
        return Err(HttpError::InvalidContentType);
    }
    session_is_the_callers(&headers, state.sessions.as_ref(), &auth_status)?;

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
        validate_modern_request(&headers, &state)?;
        if headers.contains_key(SESSION_ID_HEADER) {
            return Err(HttpError::BadRequest(
                "modern requests must not use Mcp-Session-Id".to_string(),
            ));
        }
    } else {
        crate::protocol::rewrite_legacy_request(&mut raw_message);
        // `initialize` opens a session of its own.
        if raw_message.get("method").and_then(|method| method.as_str()) != Some("initialize") {
            reopen_lost_session(&state, &headers, &auth_status).await?;
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

    let mirrored = if era == crate::protocol::ProtocolEra::Modern {
        validate_required_mirrored_headers(&headers, &message)
    } else {
        validate_legacy_protocol_version(&headers)?;
        validate_mirrored_headers(&headers, &message)
    };
    if let Err(err) = mirrored {
        return Ok(header_mismatch_response(&message, err, era));
    }
    let trace_id = Arc::<str>::from(extract_trace_id(&headers));

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
                        // Built as the call it cancels was, so both name the
                        // same downstream call.
                        let context = crate::dispatch::DownstreamContext::downstream_call_context(
                            &HttpDownstreamContext::new(
                                &state,
                                auth_status,
                                Some(&session_id),
                                request_id,
                                trace_id,
                                None,
                            ),
                        );
                        state
                            .router
                            .cancel_downstream_request(&context, cancelled.params.reason);
                    }
                }
                ClientNotification::InitializedNotification(_) => {
                    wire_session(&state, &session_id);
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

fn validate_legacy_protocol_version(headers: &HeaderMap) -> Result<(), HttpError> {
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

pub(super) fn validate_modern_request_metadata(value: &serde_json::Value) -> Result<(), HttpError> {
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

/// The trace a request belongs to: the W3C `traceparent` it carries, else
/// the id a Plug client sent, else a new one.
pub(super) fn extract_trace_id(headers: &HeaderMap) -> String {
    let header = |name| headers.get(name).and_then(|value| value.to_str().ok());
    if let Some(trace_id) = header(TRACEPARENT_HEADER)
        .filter(|value| crate::types::valid_traceparent(value))
        .and_then(|value| value.split('-').nth(1))
    {
        return trace_id.to_string();
    }
    if let Some(trace_id) =
        header(PLUG_TRACE_ID_HEADER).filter(|value| crate::types::valid_trace_id(value))
    {
        return trace_id.to_ascii_lowercase();
    }
    crate::proxy::new_trace_id()
}

pub(super) async fn get_mcp(
    State(state): State<Arc<HttpState>>,
    axum::Extension(auth_status): axum::Extension<AuthStatus>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let session_id = extract_session_id(&headers)?;
    reopen_lost_session(&state, &headers, &auth_status).await?;
    state.sessions.validate(&session_id)?;

    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !accept.contains("text/event-stream") {
        return Err(HttpError::InvalidAcceptHeader);
    }

    let (tx, rx) = mpsc::channel(state.sse_channel_capacity);
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let seen_through = state
        .sessions
        .set_sse_sender(&session_id, tx, last_event_id)?;

    let session_store = Arc::clone(&state.sessions);
    let keepalive_session_id = session_id.clone();
    let sse = sse_stream_with_heartbeat(rx, seen_through, state.cancel.clone(), move || {
        let _ = session_store.validate(&keepalive_session_id);
    });
    let mut response = sse.into_response();
    mark_private(&mut response, true);
    Ok(response)
}

pub(super) async fn delete_mcp(
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

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_request(
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
        tracing::debug!(
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
        return event_request(state, &auth_status, custom, request_id).await;
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
            state.sessions.set_client_type(&session_id, client_type)?;
            bind_session_to_caller(state, &session_id, &auth_status)?;
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
    )
    .in_own_exchange();

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

    let outcome = crate::dispatch::handle(&state.router, &ctx, &method, params).await;
    let message = dispatch_message(outcome, request_id);
    match (&req.request, session_id) {
        (ClientRequest::InitializeRequest(_), Some(session_id)) => {
            json_response_with_session(&session_id, &message)
        }
        _ => json_response_for_era(&message, era),
    }
}

pub(super) fn dispatch_message(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_support::*;

    use axum::body::Body;
    use http::Request as HttpRequest;
    use std::time::Duration;
    use tower::ServiceExt;

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

        // The one W3C rule: version 00 and a parent id that is not all zero.
        for traceparent in [
            "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
        ] {
            headers.insert(TRACEPARENT_HEADER, HeaderValue::from_static(traceparent));
            assert_ne!(
                extract_trace_id(&headers),
                "4bf92f3577b34da6a3ce929d0e0e4736"
            );
        }
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

    #[tokio::test]
    async fn legacy_oauth_principal_scopes_gate_method_families() {
        let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
        let access_token = issue_test_oauth_token(&manager, "tools:read").await;
        let claims = manager
            .validate_access_token_for(&access_token, &manager.resource())
            .await
            .expect("issued token must validate");
        let state = test_state();
        let context = crate::dispatch::DownstreamContext::downstream_call_context(
            &HttpDownstreamContext::new(
                &state,
                AuthStatus::Authenticated(Some(claims)),
                Some("legacy-oauth-scope-gate"),
                RequestId::Number(91),
                Arc::from("legacy-oauth-scope-gate"),
                None,
            ),
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
            crate::admission::MethodFamily::ToolsList,
            crate::admission::MethodFamily::ToolsCall,
        ] {
            assert!(
                context.policy_decision(family).is_allowed(),
                "granted tools:read scope must keep {family:?} allowed"
            );
        }
        for family in [
            crate::admission::MethodFamily::ResourcesList,
            crate::admission::MethodFamily::ResourcesRead,
            crate::admission::MethodFamily::ResourcesSubscribe,
            crate::admission::MethodFamily::PromptsList,
            crate::admission::MethodFamily::PromptsGet,
            crate::admission::MethodFamily::Completion,
            crate::admission::MethodFamily::Tasks,
        ] {
            assert_eq!(
                context.policy_decision(family),
                crate::admission::PolicyDecision::Deny(
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
        let default_scopes: Vec<String> = crate::admission::DEFAULT_DOWNSTREAM_OAUTH_SCOPES
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
        let full = advertised(&crate::admission::DEFAULT_DOWNSTREAM_OAUTH_SCOPES);
        assert_eq!(full, every_legacy_capability());

        // The live deployment pins the six families that predate
        // `logging:configure`; those clients keep everything they use.
        let six: Vec<&str> = crate::admission::DEFAULT_DOWNSTREAM_OAUTH_SCOPES
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
        let default_scopes: Vec<String> = crate::admission::DEFAULT_DOWNSTREAM_OAUTH_SCOPES
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

    /// Tasks are offered to a modern caller when its own tool surface lists
    /// tools as they are, whatever the surface of a client of unknown type.
    #[tokio::test]
    async fn modern_tasks_follow_the_callers_own_tool_surface() {
        let mut lazy_tools = crate::config::LazyToolsConfig {
            mode: crate::types::LazyToolSetting::Bridge,
            ..Default::default()
        };
        lazy_tools.clients.insert(
            "claude-code".to_string(),
            crate::types::LazyToolSetting::Standard,
        );
        let state = test_state_with_router_config(crate::proxy::RouterConfig {
            lazy_tools,
            ..Default::default()
        });
        enable_test_task_surface(state.router.as_ref());
        let caller = |client_type| {
            let mut call = modern_task_policy_context(true, ["tasks:use".to_string()]);
            call.client_type = client_type;
            call
        };
        let task_meta = modern_task_meta(true);

        admit_modern_task_operation(
            state.router.as_ref(),
            &caller(crate::types::ClientType::ClaudeCode),
            Some(&task_meta),
        )
        .expect("a Standard client is offered tasks");
        let bridged = admit_modern_task_operation(
            state.router.as_ref(),
            &caller(crate::types::ClientType::Unknown),
            Some(&task_meta),
        )
        .expect_err("a client behind the search bridge is not");
        assert_eq!(bridged.code, ErrorCode::METHOD_NOT_FOUND);
    }

    /// Every modern tools/call from a task-capable client becomes a task, but
    /// one that cannot be a task runs as a plain call instead of failing.
    #[tokio::test]
    async fn a_modern_call_that_cannot_be_a_task_runs_as_a_plain_call() {
        let state = test_state();
        state.router.set_modern_downstream_enabled(true);
        state.router.replace_snapshot(crate::proxy::RouterSnapshot {
            routes: std::collections::HashMap::from([
                (
                    "Mock__echo".to_string(),
                    ("mock".to_string(), "echo".to_string()),
                ),
                (
                    "plug__search_tools".to_string(),
                    (
                        crate::tool_naming::INTERNAL_SERVER_ID.to_string(),
                        "plug__search_tools".to_string(),
                    ),
                ),
            ]),
            tools_all: Arc::new(vec![rmcp::model::Tool::new(
                "Mock__echo",
                "Echo",
                Arc::new(serde_json::Map::new()),
            )]),
            ..Default::default()
        });
        let call = |scopes: &[&str], tool: &str| {
            let caller = ModernTaskCaller {
                call: modern_task_policy_context(true, scopes.iter().map(|s| s.to_string())),
                client: ClientCapabilities::builder().enable_tasks().build(),
            };
            let router = Arc::clone(&state.router);
            let params = json!({"name": tool, "arguments": {"query": "echo"}});
            async move { crate::dispatch::handle(&router, &caller, "tools/call", Some(params)).await }
        };

        let task = call(&["tools:read", "tasks:use"], "Mock__echo")
            .await
            .expect("a task-capable caller gets a task");
        assert_eq!(task["resultType"], "task", "{task}");

        // With no upstream behind `mock`, a plain call fails as unavailable;
        // a task would have been created instead.
        let plain = call(&["tools:read"], "Mock__echo")
            .await
            .expect_err("a plain call reaches the missing upstream");
        assert!(plain.message.contains("unavailable"), "{plain:?}");

        let meta = call(&["tools:read", "tasks:use"], "plug__search_tools")
            .await
            .expect("a meta-tool runs as a plain call");
        assert_ne!(meta["resultType"], "task", "{meta}");
        assert!(meta.get("content").is_some(), "{meta}");
    }

    #[tokio::test]
    async fn the_modern_gate_refuses_every_http_task_method() {
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
            let answer = |enabled: bool| {
                state.router.set_modern_downstream_enabled(enabled);
                let request = modern_request(method, params.clone());
                let app = build_router(Arc::clone(&state));
                async move {
                    let response = app.oneshot(request).await.expect("task response");
                    let status = response.status();
                    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                        .await
                        .expect("body");
                    (status, String::from_utf8_lossy(&body).into_owned())
                }
            };
            let refused = answer(false).await;
            assert_eq!(refused.0, StatusCode::BAD_REQUEST, "{method}: {refused:?}");
            assert!(
                refused.1.contains(crate::protocol::MODERN_PROTOCOL_VERSION),
                "{method}: {refused:?}"
            );
            // With the gate open the same request reaches the task methods,
            // so the refusal above is the gate's.
            assert_ne!(answer(true).await, refused, "{method}");
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
            .with_auth(crate::http::server::HttpAuth::Oauth(manager))
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
        let context = |scopes: &[&str]| {
            DownstreamCallContext::http(
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
                scopes.iter().map(ToString::to_string),
            )
        };
        let mut everything = every_legacy_capability();
        everything.extensions = Some(
            [("example.test/extension".to_string(), serde_json::Map::new())]
                .into_iter()
                .collect(),
        );

        let tools_only = crate::dispatch::project_capabilities(
            everything.clone(),
            &context(&["tools:read"]),
            true,
        );
        assert!(tools_only.tools.is_some());
        assert!(tools_only.resources.is_none());
        assert!(tools_only.prompts.is_none());
        assert!(tools_only.completions.is_none());
        assert!(
            tools_only.extensions.is_none(),
            "no tasks without tasks:use"
        );
        assert!(tools_only.experimental.is_none());
        assert!(tools_only.logging.is_none());

        // With every scope the families come through, still without logging,
        // legacy extras, subscriptions or extensions Plug cannot serve.
        let all_scopes = crate::admission::DEFAULT_DOWNSTREAM_OAUTH_SCOPES;
        let full = crate::dispatch::project_capabilities(everything, &context(&all_scopes), true);
        assert!(full.tools.is_some() && full.prompts.is_some() && full.completions.is_some());
        assert!(
            full.resources
                .as_ref()
                .is_some_and(|resources| resources.subscribe.is_none())
        );
        assert!(full.logging.is_none());
        assert!(full.experimental.is_none());
        assert_eq!(
            full.extensions
                .as_ref()
                .map(|extensions| extensions.keys().cloned().collect::<Vec<_>>()),
            Some(vec![rmcp::model::TASKS_EXTENSION_ID.to_string()])
        );
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
            routes: std::collections::HashMap::from([(
                "git__commit".to_string(),
                ("git".to_string(), "commit".to_string()),
            )]),
            tools_all: Arc::new(vec![Tool::new(
                std::borrow::Cow::Borrowed("git__commit"),
                std::borrow::Cow::Borrowed("Create a git commit"),
                Arc::new(serde_json::Map::new()),
            )]),
            ..Default::default()
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

    #[tokio::test]
    async fn a_session_that_times_out_is_released_without_being_asked() {
        let router = bare_router();
        enable_test_task_surface(&router);
        let state = HttpState::new(
            Arc::clone(&router),
            Arc::new(crate::session::SessionStore::new(60, 100)),
            CancellationToken::new(),
        );
        let session_id = state.sessions.create_session().unwrap();
        router.set_client_log_level(&session_id, LoggingLevel::Debug);
        let owner = crate::downstream_identity::http_session_task_owner(&session_id);
        router
            .enqueue_tool_task("Mock__echo", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue a task for the session");

        state.sessions.backdate_for_tests(&session_id);
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
        for save_fails in [false, true] {
            let manager = isolated_oauth_manager(vec!["tools:read".to_string()]);
            let (client_id, _tokens) = issue_test_oauth_grant(&manager, "tools:read").await;
            let router = bare_router();
            enable_test_task_surface(&router);
            let state = HttpState::new(
                Arc::clone(&router),
                Arc::new(crate::session::SessionStore::new(1800, 100)),
                CancellationToken::new(),
            )
            .with_auth(crate::http::server::HttpAuth::Oauth(manager.clone()));
            let theirs = state.sessions.create_session().unwrap();
            bind_for_tests(&state.sessions, &theirs, Some(&client_id));
            let other = state.sessions.create_session().unwrap();
            let owner = grant_task_owner(&manager, &client_id);
            router
                .enqueue_tool_task("Mock__echo", None, None, owner.clone(), None, None)
                .await
                .expect("enqueue a task for the client");

            if save_fails {
                crate::fs_perm::faults::fail_next_rename();
            }
            let revocation = state.revoke_downstream_client(&client_id).await;
            assert_eq!(
                matches!(revocation, ClientRevocation::SaveFailed(_)),
                save_fails,
                "{revocation:?}"
            );
            assert!(state.sessions.validate(&theirs).is_err());
            assert!(state.sessions.validate(&other).is_ok());
            assert_eq!(router.task_count_for_owner(&owner).await, 0);
            if !save_fails {
                assert!(matches!(
                    state.revoke_downstream_client(&client_id).await,
                    ClientRevocation::UnknownClient
                ));
            }
        }
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
            ..Default::default()
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
}
