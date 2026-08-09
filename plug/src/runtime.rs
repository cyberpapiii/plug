use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::OutputFormat;
use crate::daemon;
use crate::ui::{print_banner, print_info_line, print_success_line};
use axum::extract::{DefaultBodyLimit, Path as AxumPath, State};
use axum::http::header::{CACHE_CONTROL, HOST, PRAGMA, REFERRER_POLICY};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use tokio_util::sync::CancellationToken;

const OPERATOR_LIVE_SESSIONS_PATH: &str = "/_plug/live-sessions";
const OPERATOR_OAUTH_CLIENTS_PATH: &str = "/_plug/oauth/clients";
const OPERATOR_OWNER_BOOTSTRAP_PATH: &str = "/_plug/oauth/owner/bootstrap";
const OPERATOR_OWNER_CREDENTIALS_PATH: &str = "/_plug/oauth/owner/credentials";
const OPERATOR_PROOF_PATH: &str = "/_plug/operator/proof";
#[cfg(test)]
const OPERATOR_TOKEN_HEADER: &str = "x-plug-operator-token";
const OPERATOR_CLIENT_NONCE_HEADER: &str = "x-plug-operator-client-nonce";
const OPERATOR_SERVER_NONCE_HEADER: &str = "x-plug-operator-server-nonce";
const OPERATOR_PROOF_HEADER: &str = "x-plug-operator-proof";
const OPERATOR_ALLOW_EMPTY_HEADER: &str = "x-plug-operator-allow-empty";
const OPERATOR_PROOF_LIFETIME: Duration = Duration::from_secs(10);
const MAX_OPERATOR_PROOFS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonQueryAvailability {
    Unreachable,
    IpcUnavailable,
    Available,
}

impl DaemonQueryAvailability {
    pub(crate) fn runtime_available(self) -> bool {
        matches!(self, Self::Available)
    }

    pub(crate) fn daemon_reachable(self) -> bool {
        !matches!(self, Self::Unreachable)
    }

    pub(crate) fn status_source(self) -> &'static str {
        match self {
            Self::Available => "live_daemon",
            Self::IpcUnavailable => "ipc_unavailable",
            Self::Unreachable => "runtime_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveClientSupport {
    Supported,
    DaemonRestartRequired,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LiveInventoryAvailability {
    pub(crate) partial: bool,
    pub(crate) unavailable_sources: Vec<&'static str>,
}

#[derive(Debug, Clone, Copy, serde::Serialize, Default)]
pub(crate) struct LiveSessionTransportCounts {
    pub(crate) daemon_proxy: usize,
    pub(crate) http: usize,
    pub(crate) sse: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LiveInventoryMetadata {
    pub(crate) session_count: usize,
    pub(crate) session_transports: LiveSessionTransportCounts,
    pub(crate) scope: plug_core::ipc::LiveSessionInventoryScope,
    pub(crate) availability: LiveInventoryAvailability,
    pub(crate) http_sessions_included: bool,
}

pub(crate) fn live_inventory_availability(
    scope: plug_core::ipc::LiveSessionInventoryScope,
) -> LiveInventoryAvailability {
    match scope {
        plug_core::ipc::LiveSessionInventoryScope::TransportComplete => LiveInventoryAvailability {
            partial: false,
            unavailable_sources: Vec::new(),
        },
        plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly => LiveInventoryAvailability {
            partial: true,
            unavailable_sources: vec!["http"],
        },
        plug_core::ipc::LiveSessionInventoryScope::HttpOnly => LiveInventoryAvailability {
            partial: true,
            unavailable_sources: vec!["daemon_proxy"],
        },
        plug_core::ipc::LiveSessionInventoryScope::Unavailable => LiveInventoryAvailability {
            partial: true,
            unavailable_sources: vec!["daemon_proxy", "http"],
        },
    }
}

pub(crate) fn live_session_transport_counts(
    sessions: &[plug_core::ipc::IpcLiveSessionInfo],
) -> LiveSessionTransportCounts {
    let mut counts = LiveSessionTransportCounts::default();
    for session in sessions {
        match session.transport {
            plug_core::ipc::LiveSessionTransport::DaemonProxy => counts.daemon_proxy += 1,
            plug_core::ipc::LiveSessionTransport::Http => counts.http += 1,
            plug_core::ipc::LiveSessionTransport::Sse => counts.sse += 1,
        }
    }
    counts
}

pub(crate) fn live_inventory_metadata(
    sessions: &[plug_core::ipc::IpcLiveSessionInfo],
    scope: plug_core::ipc::LiveSessionInventoryScope,
) -> LiveInventoryMetadata {
    LiveInventoryMetadata {
        session_count: sessions.len(),
        session_transports: live_session_transport_counts(sessions),
        availability: live_inventory_availability(scope),
        http_sessions_included: matches!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete
                | plug_core::ipc::LiveSessionInventoryScope::HttpOnly
        ),
        scope,
    }
}

enum LiveSessionSourceState {
    Available(Vec<plug_core::ipc::IpcLiveSessionInfo>),
    Unavailable,
}

impl LiveSessionSourceState {
    fn available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    fn into_sessions(self) -> Vec<plug_core::ipc::IpcLiveSessionInfo> {
        match self {
            Self::Available(sessions) => sessions,
            Self::Unavailable => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct OperatorLiveSessionsResponse {
    sessions: Vec<plug_core::ipc::IpcLiveSessionInfo>,
}

struct OperatorHttpState {
    http_state: Arc<plug_core::http::server::HttpState>,
    operator_token: Arc<str>,
    loopback_authority: Arc<str>,
    proofs: tokio::sync::Mutex<std::collections::HashMap<String, PendingOperatorProof>>,
}

struct PendingOperatorProof {
    client_nonce: String,
    method: String,
    path: String,
    allow_empty: bool,
    expires_at: Instant,
}

#[derive(serde::Deserialize)]
struct OperatorProofRequest {
    client_nonce: String,
    method: String,
    path: String,
    #[serde(default)]
    allow_empty: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OperatorProofResponse {
    server_nonce: String,
    proof: String,
}

#[derive(serde::Serialize)]
struct OwnerBootstrapResponse {
    enrollment_url: String,
}

async fn operator_live_sessions(
    State(state): State<Arc<OperatorHttpState>>,
    headers: HeaderMap,
) -> Result<Json<OperatorLiveSessionsResponse>, StatusCode> {
    operator_proof_authorized(&state, &headers, "GET", OPERATOR_LIVE_SESSIONS_PATH, false).await?;

    let sessions = state
        .http_state
        .sessions
        .session_snapshots()
        .into_iter()
        .map(|snapshot| plug_core::ipc::IpcLiveSessionInfo {
            transport: match snapshot.transport {
                plug_core::session::DownstreamTransport::Http => {
                    plug_core::ipc::LiveSessionTransport::Http
                }
                plug_core::session::DownstreamTransport::Sse => {
                    plug_core::ipc::LiveSessionTransport::Sse
                }
            },
            client_id: None,
            session_id: snapshot.session_id,
            client_type: snapshot.client_type,
            client_info: None,
            connected_secs: snapshot.connected_seconds,
            last_activity_secs: Some(snapshot.idle_seconds),
        })
        .collect();

    Ok(Json(OperatorLiveSessionsResponse { sessions }))
}

fn operator_hmac(token: &str, domain: &[u8], fields: &[&str]) -> String {
    use base64::Engine as _;
    use hmac::{Hmac, Mac as _};
    use sha2::Sha256;

    let mut mac = Hmac::<Sha256>::new_from_slice(token.as_bytes())
        .expect("HMAC accepts operator token of any size");
    mac.update(&(domain.len() as u64).to_be_bytes());
    mac.update(domain);
    for field in fields {
        mac.update(&(field.len() as u64).to_be_bytes());
        mac.update(field.as_bytes());
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn operator_challenge_proof(
    token: &str,
    client_nonce: &str,
    server_nonce: &str,
    method: &str,
    path: &str,
    allow_empty: bool,
) -> String {
    let empty_intent = if allow_empty {
        "allow-empty"
    } else {
        "preserve-one"
    };
    operator_hmac(
        token,
        b"plug-operator-server-proof-v1",
        &[client_nonce, server_nonce, method, path, empty_intent],
    )
}

fn operator_request_proof(
    token: &str,
    client_nonce: &str,
    server_nonce: &str,
    method: &str,
    path: &str,
    allow_empty: bool,
) -> String {
    let empty_intent = if allow_empty {
        "allow-empty"
    } else {
        "preserve-one"
    };
    operator_hmac(
        token,
        b"plug-operator-request-proof-v1",
        &[client_nonce, server_nonce, method, path, empty_intent],
    )
}

fn operator_intent_allowed(method: &str, path: &str) -> bool {
    match (method, path) {
        ("GET", OPERATOR_LIVE_SESSIONS_PATH)
        | ("GET", OPERATOR_OAUTH_CLIENTS_PATH)
        | ("POST", OPERATOR_OWNER_BOOTSTRAP_PATH)
        | ("GET", OPERATOR_OWNER_CREDENTIALS_PATH) => true,
        ("DELETE", path) => [OPERATOR_OAUTH_CLIENTS_PATH, OPERATOR_OWNER_CREDENTIALS_PATH]
            .iter()
            .any(|prefix| {
                path.strip_prefix(prefix)
                    .and_then(|suffix| suffix.strip_prefix('/'))
                    .is_some_and(|segment| !segment.is_empty() && !segment.contains('/'))
            }),
        _ => false,
    }
}

fn exact_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

fn operator_local_boundary_allowed(headers: &HeaderMap, expected_authority: &str) -> bool {
    let host_matches = exact_header(headers, HOST.as_str()) == Some(expected_authority);
    let forwarded = headers.contains_key("forwarded")
        || headers.contains_key("x-forwarded-for")
        || headers.contains_key("cf-connecting-ip");
    host_matches && !forwarded
}

async fn operator_proof(
    State(state): State<Arc<OperatorHttpState>>,
    headers: HeaderMap,
    Json(request): Json<OperatorProofRequest>,
) -> Result<Response, StatusCode> {
    if !operator_local_boundary_allowed(&headers, &state.loopback_authority) {
        return Err(StatusCode::FORBIDDEN);
    }
    if request.client_nonce.len() != 64
        || !request
            .client_nonce
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !operator_intent_allowed(&request.method, &request.path)
        || (request.allow_empty
            && !(request.method == "DELETE"
                && request
                    .path
                    .starts_with(&format!("{OPERATOR_OWNER_CREDENTIALS_PATH}/"))))
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let server_nonce = plug_core::auth::generate_auth_token();
    let proof = operator_challenge_proof(
        &state.operator_token,
        &request.client_nonce,
        &server_nonce,
        &request.method,
        &request.path,
        request.allow_empty,
    );
    let now = Instant::now();
    let mut proofs = state.proofs.lock().await;
    proofs.retain(|_, pending| pending.expires_at > now);
    if proofs.len() >= MAX_OPERATOR_PROOFS
        && let Some(oldest) = proofs
            .iter()
            .min_by_key(|(_, pending)| pending.expires_at)
            .map(|(nonce, _)| nonce.clone())
    {
        proofs.remove(&oldest);
    }
    proofs.insert(
        server_nonce.clone(),
        PendingOperatorProof {
            client_nonce: request.client_nonce,
            method: request.method,
            path: request.path,
            allow_empty: request.allow_empty,
            expires_at: now + OPERATOR_PROOF_LIFETIME,
        },
    );
    drop(proofs);
    Ok(sensitive_json(OperatorProofResponse {
        server_nonce,
        proof,
    }))
}

async fn operator_proof_authorized(
    state: &OperatorHttpState,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    allow_empty: bool,
) -> Result<(), StatusCode> {
    if !operator_local_boundary_allowed(headers, &state.loopback_authority) {
        return Err(StatusCode::FORBIDDEN);
    }
    let client_nonce =
        exact_header(headers, OPERATOR_CLIENT_NONCE_HEADER).ok_or(StatusCode::UNAUTHORIZED)?;
    let server_nonce =
        exact_header(headers, OPERATOR_SERVER_NONCE_HEADER).ok_or(StatusCode::UNAUTHORIZED)?;
    let provided = exact_header(headers, OPERATOR_PROOF_HEADER).ok_or(StatusCode::UNAUTHORIZED)?;

    let pending = state
        .proofs
        .lock()
        .await
        .remove(server_nonce)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if pending.expires_at <= Instant::now()
        || pending.client_nonce != client_nonce
        || pending.method != method
        || pending.path != path
        || pending.allow_empty != allow_empty
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let expected = operator_request_proof(
        &state.operator_token,
        client_nonce,
        server_nonce,
        method,
        path,
        allow_empty,
    );
    if !plug_core::auth::verify_auth_token(provided, &expected) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

fn sensitive_json<T: serde::Serialize>(value: T) -> Response {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(PRAGMA, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

async fn operator_owner_bootstrap(
    State(state): State<Arc<OperatorHttpState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    operator_proof_authorized(
        &state,
        &headers,
        "POST",
        OPERATOR_OWNER_BOOTSTRAP_PATH,
        false,
    )
    .await?;
    let manager = state
        .http_state
        .downstream_oauth
        .as_ref()
        .ok_or(StatusCode::NOT_FOUND)?;
    let bootstrap = manager
        .create_owner_bootstrap()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let enrollment_url = format!(
        "{}/oauth/owner/enroll#bootstrap={bootstrap}",
        manager.base_url()
    );
    Ok(sensitive_json(OwnerBootstrapResponse { enrollment_url }))
}

async fn operator_owner_credentials(
    State(state): State<Arc<OperatorHttpState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    operator_proof_authorized(
        &state,
        &headers,
        "GET",
        OPERATOR_OWNER_CREDENTIALS_PATH,
        false,
    )
    .await?;
    let manager = state
        .http_state
        .downstream_oauth
        .as_ref()
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(sensitive_json(manager.list_owner_credentials().await))
}

async fn operator_remove_owner_credential(
    State(state): State<Arc<OperatorHttpState>>,
    AxumPath(credential_id): AxumPath<String>,
    headers: HeaderMap,
) -> StatusCode {
    let path = format!("{OPERATOR_OWNER_CREDENTIALS_PATH}/{credential_id}");
    let allow_empty = exact_header(&headers, OPERATOR_ALLOW_EMPTY_HEADER) == Some("true");
    if let Err(status) =
        operator_proof_authorized(&state, &headers, "DELETE", &path, allow_empty).await
    {
        return status;
    }
    let Some(manager) = state.http_state.downstream_oauth.as_ref() else {
        return StatusCode::NOT_FOUND;
    };
    match manager
        .remove_owner_credential(&credential_id, allow_empty)
        .await
    {
        Ok(plug_core::downstream_oauth::RemoveOwnerCredentialOutcome::Removed) => {
            StatusCode::NO_CONTENT
        }
        Ok(plug_core::downstream_oauth::RemoveOwnerCredentialOutcome::NotFound) => {
            StatusCode::NOT_FOUND
        }
        Ok(
            plug_core::downstream_oauth::RemoveOwnerCredentialOutcome::FinalCredentialConfirmationRequired,
        ) => StatusCode::CONFLICT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn operator_oauth_clients(
    State(state): State<Arc<OperatorHttpState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<plug_core::downstream_oauth::RegisteredClientSummary>>, StatusCode> {
    operator_proof_authorized(&state, &headers, "GET", OPERATOR_OAUTH_CLIENTS_PATH, false).await?;
    let manager = state
        .http_state
        .downstream_oauth
        .as_ref()
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(manager.list_clients().await))
}

async fn operator_revoke_oauth_client(
    State(state): State<Arc<OperatorHttpState>>,
    AxumPath(client_id): AxumPath<String>,
    headers: HeaderMap,
) -> StatusCode {
    let path = format!("{OPERATOR_OAUTH_CLIENTS_PATH}/{client_id}");
    if let Err(status) = operator_proof_authorized(&state, &headers, "DELETE", &path, false).await {
        return status;
    }
    let Some(manager) = state.http_state.downstream_oauth.as_ref() else {
        return StatusCode::NOT_FOUND;
    };
    match manager.revoke_client(&client_id).await {
        Ok(true) => {
            // Revocation is the durable-owner lifecycle boundary. The OAuth
            // manager rejects future token use before this point; task cleanup
            // then uses TaskStore's create guard/tombstone ledger so an
            // in-flight create cannot survive the revocation.
            let principal = plug_core::types::PrincipalId::downstream_oauth(
                manager.base_url(),
                client_id,
                manager.resource(),
            );
            state
                .http_state
                .router
                .cleanup_tasks_for_owner(&plug_core::tasks::TaskOwner::new(principal.owner_key()))
                .await;
            StatusCode::NO_CONTENT
        }
        Ok(false) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn build_runtime_router(
    http_state: Arc<plug_core::http::server::HttpState>,
    operator_token: Arc<str>,
    loopback_authority: Arc<str>,
) -> Router {
    let operator_state = Arc::new(OperatorHttpState {
        http_state: http_state.clone(),
        operator_token,
        loopback_authority,
        proofs: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    });
    let operator_router = Router::new()
        .route(OPERATOR_LIVE_SESSIONS_PATH, get(operator_live_sessions))
        .route(OPERATOR_PROOF_PATH, post(operator_proof))
        .route(OPERATOR_OAUTH_CLIENTS_PATH, get(operator_oauth_clients))
        .route(
            OPERATOR_OWNER_BOOTSTRAP_PATH,
            post(operator_owner_bootstrap),
        )
        .route(
            OPERATOR_OWNER_CREDENTIALS_PATH,
            get(operator_owner_credentials),
        )
        .route(
            &format!("{OPERATOR_OWNER_CREDENTIALS_PATH}/{{credential_id}}"),
            delete(operator_remove_owner_credential),
        )
        .route(
            &format!("{OPERATOR_OAUTH_CLIENTS_PATH}/{{client_id}}"),
            delete(operator_revoke_oauth_client),
        )
        .layer(DefaultBodyLimit::max(4 * 1024))
        .with_state(operator_state);

    plug_core::http::server::build_router(http_state).merge(operator_router)
}

pub(crate) fn local_operator_authority(bind_address: &str, port: u16) -> String {
    if matches!(bind_address, "::1" | "[::1]" | "::" | "[::]") {
        format!("[::1]:{port}")
    } else {
        format!("127.0.0.1:{port}")
    }
}

pub(crate) fn local_operator_connection_authority(bind_address: &str, port: u16) -> String {
    match bind_address {
        "0.0.0.0" => format!("127.0.0.1:{port}"),
        "::" | "[::]" => format!("[::1]:{port}"),
        address if address.contains(':') => {
            format!("[{}]:{port}", address.trim_matches(['[', ']']))
        }
        address => format!("{address}:{port}"),
    }
}

pub(crate) async fn send_authenticated_operator_request(
    client: &reqwest::Client,
    endpoint: reqwest::Url,
    host_authority: &str,
    operator_token: &str,
    method: reqwest::Method,
    allow_empty: bool,
) -> anyhow::Result<reqwest::Response> {
    let method_name = method.as_str();
    let path = endpoint.path().to_string();
    if !operator_intent_allowed(method_name, &path) {
        anyhow::bail!("invalid local operator request intent");
    }
    let client_nonce = plug_core::auth::generate_auth_token();
    let mut proof_url = endpoint.clone();
    proof_url.set_path(OPERATOR_PROOF_PATH);
    proof_url.set_query(None);
    proof_url.set_fragment(None);
    let proof_response = client
        .post(proof_url)
        .header(reqwest::header::HOST, host_authority)
        .json(&serde_json::json!({
            "client_nonce": client_nonce,
            "method": method_name,
            "path": path.as_str(),
            "allow_empty": allow_empty,
        }))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("could not authenticate local Plug service"))?;
    if !proof_response.status().is_success() {
        anyhow::bail!("could not authenticate local Plug service");
    }
    let proof_bytes = proof_response
        .bytes()
        .await
        .map_err(|_| anyhow::anyhow!("could not authenticate local Plug service"))?;
    if proof_bytes.len() > 4 * 1024 {
        anyhow::bail!("could not authenticate local Plug service");
    }
    let challenge: OperatorProofResponse = serde_json::from_slice(&proof_bytes)
        .map_err(|_| anyhow::anyhow!("could not authenticate local Plug service"))?;
    let expected = operator_challenge_proof(
        operator_token,
        &client_nonce,
        &challenge.server_nonce,
        method_name,
        &path,
        allow_empty,
    );
    if !plug_core::auth::verify_auth_token(&challenge.proof, &expected) {
        anyhow::bail!("could not authenticate local Plug service");
    }
    let request_proof = operator_request_proof(
        operator_token,
        &client_nonce,
        &challenge.server_nonce,
        method_name,
        &path,
        allow_empty,
    );
    let mut request = client
        .request(method, endpoint)
        .header(reqwest::header::HOST, host_authority)
        .header(OPERATOR_CLIENT_NONCE_HEADER, client_nonce)
        .header(OPERATOR_SERVER_NONCE_HEADER, challenge.server_nonce)
        .header(OPERATOR_PROOF_HEADER, request_proof);
    if allow_empty {
        request = request.header(OPERATOR_ALLOW_EMPTY_HEADER, "true");
    }
    request.send().await.map_err(Into::into)
}

struct ConfiguredHttpRuntime {
    router: Router,
    sessions: Arc<dyn plug_core::session::SessionStore>,
}

fn public_host_allowance(public_base_url: &str) -> Option<Arc<str>> {
    (!public_base_url.trim().is_empty()).then(|| {
        Arc::from(format!(
            "{}/.plug-public-host-only",
            public_base_url.trim_end_matches('/')
        ))
    })
}

fn build_configured_http_runtime(
    config: &plug_core::config::Config,
    engine: &Arc<plug_core::engine::Engine>,
) -> anyhow::Result<ConfiguredHttpRuntime> {
    let (expiry_tx, mut expiry_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let sessions: Arc<dyn plug_core::session::SessionStore> = Arc::new(
        plug_core::session::StatefulSessionStore::new(
            config.http.session_timeout_secs,
            config.http.max_sessions,
        )
        .with_expiry_notifier(expiry_tx),
    );
    sessions.spawn_cleanup_task(engine.cancel_token().clone());
    let tool_router = engine.tool_router().clone();
    let auth_token = resolve_downstream_bearer_token(&config.http)?;
    let operator_token = Arc::<str>::from(plug_core::auth::load_or_generate_token(
        &plug_core::auth::http_operator_token_path(config.http.port),
    )?);
    let downstream_oauth =
        plug_core::downstream_oauth::DownstreamOauthConfig::from_http_config(&config.http)
            .map(plug_core::downstream_oauth::DownstreamOauthManager::try_new)
            .transpose()
            .map_err(|error| anyhow::anyhow!(error))?;

    let http_state = Arc::new(plug_core::http::server::HttpState {
        router: tool_router.clone(),
        sessions: Arc::clone(&sessions),
        cancel: engine.cancel_token().clone(),
        auth_mode: config.http.auth_mode.clone(),
        downstream_oauth,
        sse_channel_capacity: config.http.sse_channel_capacity,
        allowed_origins: config
            .http
            .allowed_origins
            .iter()
            .cloned()
            .map(Arc::<str>::from)
            .chain(
                config
                    .http
                    .public_base_url
                    .as_deref()
                    .and_then(public_host_allowance),
            )
            .collect(),
        notification_task_started: std::sync::atomic::AtomicBool::new(false),
        auth_token,
        roots_capable_sessions: dashmap::DashMap::new(),
        pending_client_requests: dashmap::DashMap::new(),
        reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
        client_capabilities: dashmap::DashMap::new(),
    });

    let http_state_for_expiry = Arc::clone(&http_state);
    tokio::spawn(async move {
        while let Some(session_id) = expiry_rx.recv().await {
            cleanup_expired_http_session(&http_state_for_expiry, &tool_router, &session_id).await;
        }
    });

    Ok(ConfiguredHttpRuntime {
        router: build_runtime_router(
            http_state,
            operator_token,
            Arc::from(local_operator_authority(
                &config.http.bind_address,
                config.http.port,
            )),
        ),
        sessions,
    })
}

/// Clean up all per-session state for an HTTP session that expired via idle
/// timeout. Mirrors the teardown `delete_mcp` performs for an explicit
/// `DELETE /mcp`, including the session's task records.
pub(crate) async fn cleanup_expired_http_session(
    http_state: &Arc<plug_core::http::server::HttpState>,
    tool_router: &Arc<plug_core::proxy::ToolRouter>,
    session_id: &str,
) {
    let target = plug_core::notifications::NotificationTarget::Http {
        session_id: Arc::from(session_id),
    };
    tool_router.cleanup_subscriptions_for_target(&target).await;
    http_state.roots_capable_sessions.remove(session_id);
    http_state.client_capabilities.remove(session_id);
    http_state
        .pending_client_requests
        .retain(|(pending_session_id, _), _| pending_session_id != session_id);
    if tool_router.clear_roots_for_target(&target) {
        tool_router.forward_roots_list_changed_to_upstreams().await;
    }
    tool_router.remove_client_log_level(session_id);
    let lazy_session_key = plug_core::proxy::ToolRouter::lazy_session_key(
        plug_core::proxy::DownstreamTransport::Http,
        session_id,
    );
    tool_router.clear_lazy_session(&lazy_session_key);
    tool_router.unregister_downstream_bridge(&target);
    let owner = plug_core::proxy::ToolRouter::task_owner_for_http_session(session_id);
    tool_router.cleanup_tasks_for_owner(&owner).await;
}

fn local_http_inventory_url(http: &plug_core::config::HttpConfig) -> String {
    let scheme = if http.tls_cert_path.is_some() && http.tls_key_path.is_some() {
        "https"
    } else {
        "http"
    };
    let authority = local_operator_connection_authority(&http.bind_address, http.port);
    format!("{scheme}://{authority}{OPERATOR_LIVE_SESSIONS_PATH}")
}

async fn fetch_http_live_sessions(config_path: Option<&PathBuf>) -> LiveSessionSourceState {
    let config = match plug_core::config::load_config(config_path) {
        Ok(config) => config,
        Err(_) => return LiveSessionSourceState::Unavailable,
    };
    let token_path = plug_core::auth::http_operator_token_path(config.http.port);
    fetch_http_live_sessions_from(
        local_http_inventory_url(&config.http),
        local_operator_authority(&config.http.bind_address, config.http.port),
        &token_path,
    )
    .await
}

async fn fetch_http_live_sessions_from(
    url: String,
    host_authority: String,
    token_path: &std::path::Path,
) -> LiveSessionSourceState {
    let token = match std::fs::read_to_string(token_path) {
        Ok(token) => token,
        Err(_) => return LiveSessionSourceState::Unavailable,
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        return LiveSessionSourceState::Unavailable;
    }

    plug_core::tls::ensure_rustls_provider_installed();
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(2))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build();
    let client = match client {
        Ok(client) => client,
        Err(_) => return LiveSessionSourceState::Unavailable,
    };
    let endpoint = match reqwest::Url::parse(&url) {
        Ok(endpoint) => endpoint,
        Err(_) => return LiveSessionSourceState::Unavailable,
    };
    let response = send_authenticated_operator_request(
        &client,
        endpoint,
        &host_authority,
        &token,
        reqwest::Method::GET,
        false,
    )
    .await;
    let response = match response {
        Ok(response) => response,
        Err(_) => return LiveSessionSourceState::Unavailable,
    };
    if !response.status().is_success() {
        return LiveSessionSourceState::Unavailable;
    }
    match response.json::<OperatorLiveSessionsResponse>().await {
        Ok(body) => LiveSessionSourceState::Available(body.sessions),
        Err(_) => LiveSessionSourceState::Unavailable,
    }
}

fn merge_live_session_sources(
    daemon_scope: plug_core::ipc::LiveSessionInventoryScope,
    daemon_source: LiveSessionSourceState,
    http_source: LiveSessionSourceState,
) -> (
    Vec<plug_core::ipc::IpcLiveSessionInfo>,
    plug_core::ipc::LiveSessionInventoryScope,
) {
    let daemon_available = daemon_source.available();
    let http_available = http_source.available();
    let mut daemon_sessions = daemon_source.into_sessions();
    let mut http_sessions = http_source.into_sessions();
    let scope = match (daemon_available, http_available) {
        (true, true) => plug_core::ipc::LiveSessionInventoryScope::TransportComplete,
        (true, false) => daemon_scope,
        (false, true) => plug_core::ipc::LiveSessionInventoryScope::HttpOnly,
        (false, false) => plug_core::ipc::LiveSessionInventoryScope::Unavailable,
    };

    daemon_sessions.append(&mut http_sessions);
    daemon_sessions.sort_by(|a, b| {
        let transport_order = |transport: plug_core::ipc::LiveSessionTransport| match transport {
            plug_core::ipc::LiveSessionTransport::DaemonProxy => 0,
            plug_core::ipc::LiveSessionTransport::Http => 1,
            plug_core::ipc::LiveSessionTransport::Sse => 2,
        };
        transport_order(a.transport)
            .cmp(&transport_order(b.transport))
            .then(a.client_type.to_string().cmp(&b.client_type.to_string()))
            .then(a.session_id.cmp(&b.session_id))
    });

    (daemon_sessions, scope)
}

pub(crate) async fn fetch_live_sessions(
    config_path: Option<&PathBuf>,
) -> (
    Vec<plug_core::ipc::IpcLiveSessionInfo>,
    plug_core::ipc::LiveSessionInventoryScope,
    LiveClientSupport,
) {
    let (daemon_source, daemon_scope, support) =
        match daemon::ipc_request(&plug_core::ipc::IpcRequest::ListLiveSessions).await {
            Ok(plug_core::ipc::IpcResponse::LiveSessions { sessions, scope }) => (
                LiveSessionSourceState::Available(sessions),
                scope,
                LiveClientSupport::Supported,
            ),
            Ok(plug_core::ipc::IpcResponse::Clients { clients }) => {
                let sessions = clients
                    .into_iter()
                    .map(|client| plug_core::ipc::IpcLiveSessionInfo {
                        transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
                        client_id: Some(client.client_id),
                        session_id: client.session_id,
                        client_type: client
                            .client_info
                            .as_deref()
                            .map(plug_core::client_detect::detect_client)
                            .unwrap_or(plug_core::types::ClientType::Unknown),
                        client_info: client.client_info,
                        connected_secs: client.connected_secs,
                        last_activity_secs: None,
                    })
                    .collect();
                (
                    LiveSessionSourceState::Available(sessions),
                    plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly,
                    LiveClientSupport::Supported,
                )
            }
            Ok(plug_core::ipc::IpcResponse::Error { code, .. }) if code == "PARSE_ERROR" => (
                LiveSessionSourceState::Unavailable,
                plug_core::ipc::LiveSessionInventoryScope::Unavailable,
                LiveClientSupport::DaemonRestartRequired,
            ),
            _ => (
                LiveSessionSourceState::Unavailable,
                plug_core::ipc::LiveSessionInventoryScope::Unavailable,
                LiveClientSupport::Supported,
            ),
        };

    if matches!(
        daemon_scope,
        plug_core::ipc::LiveSessionInventoryScope::TransportComplete
    ) {
        let sessions = daemon_source.into_sessions();
        return (sessions, daemon_scope, support);
    }

    let http_source = fetch_http_live_sessions(config_path).await;

    let (sessions, scope) = merge_live_session_sources(daemon_scope, daemon_source, http_source);

    (sessions, scope, support)
}

pub(crate) struct DaemonProxySession {
    pub(crate) reader: tokio::net::unix::OwnedReadHalf,
    pub(crate) writer: tokio::net::unix::OwnedWriteHalf,
    pub(crate) client_id: String,
    pub(crate) client_info: Option<String>,
    pub(crate) session_id: String,
    pub(crate) capabilities: rmcp::model::ServerCapabilities,
    pub(crate) modern_downstream_enabled: bool,
    pub(crate) cancellation_capability: plug_core::ipc::IpcCancellationCapability,
    pub(crate) pending_notifications: Vec<plug_core::ipc::IpcResponse>,
}

enum PendingIpcResponse {
    Registered {
        session_id: String,
        modern_downstream_enabled: bool,
        cancellation_capability: plug_core::ipc::IpcCancellationCapability,
    },
    Capabilities(rmcp::model::ServerCapabilities),
}

async fn read_pending_or_matching_response(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    expected_client_id: &str,
    pending_notifications: &mut Vec<plug_core::ipc::IpcResponse>,
    matcher: impl Fn(&plug_core::ipc::IpcResponse) -> Option<PendingIpcResponse>,
) -> anyhow::Result<PendingIpcResponse> {
    loop {
        let frame = plug_core::ipc::read_frame(reader).await?.ok_or_else(|| {
            anyhow::anyhow!("daemon closed connection while waiting for response")
        })?;
        let response: plug_core::ipc::IpcResponse = serde_json::from_slice(&frame)
            .map_err(|e| anyhow::anyhow!("invalid daemon response: {e}"))?;

        if let Some(matched) = matcher(&response) {
            return Ok(matched);
        }

        match response {
            plug_core::ipc::IpcResponse::Error { code, message } => {
                anyhow::bail!("{code}: {message}");
            }
            plug_core::ipc::IpcResponse::Registered {
                protocol_version,
                client_id,
                session_id,
                modern_downstream_enabled,
                cancellation_capability,
            } => {
                if protocol_version != plug_core::ipc::IPC_PROTOCOL_VERSION {
                    anyhow::bail!(
                        "daemon/client protocol mismatch: daemon=v{protocol_version}, client=v{}",
                        plug_core::ipc::IPC_PROTOCOL_VERSION
                    );
                }
                if client_id != expected_client_id {
                    anyhow::bail!(
                        "daemon/client registration mismatch: expected client_id {expected_client_id}, got {client_id}"
                    );
                }
                return Ok(PendingIpcResponse::Registered {
                    session_id,
                    modern_downstream_enabled,
                    cancellation_capability,
                });
            }
            resp @ (plug_core::ipc::IpcResponse::LoggingNotification { .. }
            | plug_core::ipc::IpcResponse::ToolListChangedNotification
            | plug_core::ipc::IpcResponse::ResourceListChangedNotification
            | plug_core::ipc::IpcResponse::PromptListChangedNotification
            | plug_core::ipc::IpcResponse::ProgressNotification { .. }
            | plug_core::ipc::IpcResponse::CancelledNotification { .. }
            | plug_core::ipc::IpcResponse::AuthStateChanged { .. }
            | plug_core::ipc::IpcResponse::ModernDownstreamGateChanged { .. }) => {
                pending_notifications.push(resp);
            }
            other => {
                anyhow::bail!("unexpected daemon response while waiting for IPC setup: {other:?}");
            }
        }
    }
}

pub(crate) async fn establish_daemon_proxy_session(
    config_path: Option<&PathBuf>,
    client_id: String,
    client_info: Option<String>,
) -> anyhow::Result<DaemonProxySession> {
    let stream = match daemon::connect_to_daemon().await {
        Some(stream) => stream,
        None => {
            let mut child = auto_start_daemon(config_path)?;
            wait_for_daemon_ready(Some(&mut child)).await?
        }
    };

    let (mut reader, mut writer) = stream.into_split();
    let mut pending_notifications = Vec::new();
    let register_req = plug_core::ipc::IpcRequest::Register {
        protocol_version: plug_core::ipc::IPC_PROTOCOL_VERSION,
        client_id: client_id.clone(),
        client_info: client_info.clone(),
    };
    let payload = serde_json::to_vec(&register_req)?;
    plug_core::ipc::write_frame(&mut writer, &payload).await?;
    let (session_id, modern_downstream_enabled, cancellation_capability) =
        match read_pending_or_matching_response(
            &mut reader,
            &client_id,
            &mut pending_notifications,
            |response| match response {
                plug_core::ipc::IpcResponse::Registered {
                    session_id,
                    modern_downstream_enabled,
                    cancellation_capability,
                    ..
                } => Some(PendingIpcResponse::Registered {
                    session_id: session_id.clone(),
                    modern_downstream_enabled: *modern_downstream_enabled,
                    cancellation_capability: cancellation_capability.clone(),
                }),
                _ => None,
            },
        )
        .await?
        {
            PendingIpcResponse::Registered {
                session_id,
                modern_downstream_enabled,
                cancellation_capability,
            } => (
                session_id,
                modern_downstream_enabled,
                cancellation_capability,
            ),
            PendingIpcResponse::Capabilities(_) => unreachable!("registration response expected"),
        };
    let capabilities_req = plug_core::ipc::IpcRequest::Capabilities {
        session_id: session_id.clone(),
    };
    let capabilities_payload = serde_json::to_vec(&capabilities_req)?;
    plug_core::ipc::write_frame(&mut writer, &capabilities_payload).await?;
    let capabilities = match read_pending_or_matching_response(
        &mut reader,
        &client_id,
        &mut pending_notifications,
        |response| match response {
            plug_core::ipc::IpcResponse::Capabilities { capabilities } => {
                serde_json::from_value(capabilities.clone())
                    .ok()
                    .map(PendingIpcResponse::Capabilities)
            }
            _ => None,
        },
    )
    .await?
    {
        PendingIpcResponse::Capabilities(capabilities) => capabilities,
        PendingIpcResponse::Registered { .. } => unreachable!("capabilities response expected"),
    };
    Ok(DaemonProxySession {
        reader,
        writer,
        client_id,
        client_info,
        session_id,
        capabilities,
        modern_downstream_enabled,
        cancellation_capability,
        pending_notifications,
    })
}

fn is_modern_stdio_message(value: &serde_json::Value) -> bool {
    value.get("method").and_then(serde_json::Value::as_str) == Some("server/discover")
        || value
            .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
            .and_then(serde_json::Value::as_str)
            == Some(plug_core::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION)
}

#[derive(Default)]
struct StdioProtocolState {
    modern_confirmed: bool,
    pending_discovery_ids: std::collections::HashSet<String>,
}

impl StdioProtocolState {
    fn observe_inbound(
        &mut self,
        value: &serde_json::Value,
        modern_downstream_enabled: bool,
    ) -> bool {
        if !modern_downstream_enabled {
            self.modern_confirmed = false;
            self.pending_discovery_ids.clear();
            return false;
        }

        let method = value.get("method").and_then(serde_json::Value::as_str);
        let explicit_legacy_initialize = method == Some("initialize")
            && value
                .pointer("/params/protocolVersion")
                .and_then(serde_json::Value::as_str)
                != Some(plug_core::protocol::ANNOUNCED_FUTURE_PROTOCOL_VERSION);
        if explicit_legacy_initialize {
            self.modern_confirmed = false;
            self.pending_discovery_ids.clear();
            return false;
        }

        if method == Some("server/discover") {
            if let Some(id) = value.get("id") {
                self.pending_discovery_ids.insert(id.to_string());
            }
        } else if is_modern_stdio_message(value) {
            self.modern_confirmed = true;
        }
        self.modern_confirmed
    }

    fn observe_outbound(&mut self, value: &serde_json::Value) -> bool {
        if let Some(id) = value.get("id")
            && self.pending_discovery_ids.remove(&id.to_string())
            && value.get("result").is_some()
            && value.get("error").is_none()
        {
            self.modern_confirmed = true;
        }
        self.modern_confirmed
    }
}

/// Byte-level protocol adapter. Legacy sessions retain the exact SEP-1686
/// request/response vocabulary; a gated modern session passes through without
/// those rewrites, beginning with `server/discover` as its first message.
fn stdio_transport(modern_gate: Arc<dyn Fn() -> bool + Send + Sync>) -> tokio::io::DuplexStream {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    let (service, bridge) = tokio::io::duplex(256 * 1024);
    let (bridge_read, mut bridge_write) = tokio::io::split(bridge);
    let task_requests = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::<
        String,
    >::new()));
    let protocol_state = std::sync::Arc::new(std::sync::Mutex::new(StdioProtocolState::default()));

    let inbound_tasks = std::sync::Arc::clone(&task_requests);
    let inbound_protocol = std::sync::Arc::clone(&protocol_state);
    tokio::spawn(async move {
        let mut input = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = input.next_line().await {
            let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&line) else {
                let _ = bridge_write.write_all(line.as_bytes()).await;
                let _ = bridge_write.write_all(b"\n").await;
                continue;
            };
            let modern_downstream_enabled = modern_gate();
            let is_modern = inbound_protocol
                .lock()
                .map(|mut state| state.observe_inbound(&value, modern_downstream_enabled))
                .unwrap_or(false);
            let task_request = !is_modern
                && value.get("method").and_then(serde_json::Value::as_str) == Some("tools/call")
                && value
                    .get("params")
                    .and_then(|params| params.get("task"))
                    .is_some();
            if task_request && let Some(id) = value.get("id") {
                inbound_tasks.lock().await.insert(id.to_string());
            }
            if !is_modern {
                plug_core::protocol::rewrite_legacy_request(&mut value);
            }
            if let Ok(mut encoded) = serde_json::to_vec(&value) {
                encoded.push(b'\n');
                if bridge_write.write_all(&encoded).await.is_err() {
                    break;
                }
            }
        }
    });

    let outbound_protocol = protocol_state;
    tokio::spawn(async move {
        let mut output = tokio::io::stdout();
        let mut lines = BufReader::new(bridge_read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&line) else {
                let _ = output.write_all(line.as_bytes()).await;
                let _ = output.write_all(b"\n").await;
                continue;
            };
            let is_modern = outbound_protocol
                .lock()
                .map(|mut state| state.observe_outbound(&value))
                .unwrap_or(false);
            let task_response = if !is_modern && let Some(id) = value.get("id") {
                task_requests.lock().await.remove(&id.to_string())
            } else {
                false
            };
            if !is_modern {
                plug_core::protocol::rewrite_legacy_response(&mut value, task_response);
            }
            if let Ok(mut encoded) = serde_json::to_vec(&value) {
                encoded.push(b'\n');
                if output.write_all(&encoded).await.is_err() {
                    break;
                }
                let _ = output.flush().await;
            }
        }
    });

    service
}

pub(crate) async fn connect_via_daemon(
    config_path: Option<&std::path::PathBuf>,
) -> anyhow::Result<()> {
    let client_id = uuid::Uuid::new_v4().to_string();
    let session = establish_daemon_proxy_session(config_path, client_id, None).await?;
    let proxy = crate::ipc_proxy::IpcProxyHandler::new(session, config_path.cloned());
    let modern_gate = proxy.modern_gate_reader();
    use rmcp::ServiceExt as _;
    let transport = stdio_transport(modern_gate);
    let service = proxy
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let _ = service.waiting().await;
    Ok(())
}

pub(crate) fn auto_start_daemon(
    config_path: Option<&std::path::PathBuf>,
) -> anyhow::Result<std::process::Child> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve").arg("--daemon");
    if let Some(path) = config_path {
        cmd.arg("--config").arg(path);
    }
    for (key, value) in
        plug_core::dotenv::read_dotenv_vars_for_config(config_path.map(|path| path.as_path()))
    {
        cmd.env(key, value);
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    Ok(cmd.spawn()?)
}

pub(crate) async fn wait_for_daemon_ready(
    child: Option<&mut std::process::Child>,
) -> anyhow::Result<tokio::net::UnixStream> {
    wait_for_daemon_ready_with_timeouts(
        child,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_secs(300),
    )
    .await
}

#[derive(Debug)]
struct DaemonStartupContention;

impl std::fmt::Display for DaemonStartupContention {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("another Plug daemon still owns startup but is not ready")
    }
}

impl std::error::Error for DaemonStartupContention {}

#[derive(Debug, PartialEq, Eq)]
enum DaemonProxyFailurePolicy {
    RefuseStandalone,
}

fn daemon_proxy_failure_policy(_error: &anyhow::Error) -> DaemonProxyFailurePolicy {
    DaemonProxyFailurePolicy::RefuseStandalone
}

async fn wait_for_daemon_ready_with_timeouts(
    mut child: Option<&mut std::process::Child>,
    initial_timeout: std::time::Duration,
    contention_timeout: std::time::Duration,
) -> anyhow::Result<tokio::net::UnixStream> {
    let mut delay = std::time::Duration::from_millis(10);
    let started_at = std::time::Instant::now();
    let mut deadline = started_at + initial_timeout;
    let mut child_exit = None;
    while std::time::Instant::now() < deadline {
        if let Some(stream) = daemon::connect_to_daemon().await {
            return Ok(stream);
        }
        if child_exit.is_none()
            && let Some(child) = child.as_mut()
            && let Some(status) = child.try_wait()?
        {
            if daemon::runtime_lock_is_held() {
                // This child correctly lost a concurrent auto-start race. Keep
                // waiting for the lock owner instead of falling back to a
                // standalone Engine that would duplicate upstream startup.
                child_exit = Some(status);
                deadline = started_at + contention_timeout;
            } else {
                anyhow::bail!("daemon exited before becoming ready (status: {status})");
            }
        }
        if let Some(status) = child_exit
            && !daemon::runtime_lock_is_held()
        {
            anyhow::bail!(
                "concurrent daemon exited before becoming ready (losing child status: {status})"
            );
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(std::time::Duration::from_millis(500));
    }
    if child_exit.is_some() && daemon::runtime_lock_is_held() {
        return Err(DaemonStartupContention.into());
    }
    anyhow::bail!("daemon failed to start")
}

pub(crate) async fn ensure_daemon_with_feedback(
    config_path: Option<&std::path::PathBuf>,
    announce: bool,
) -> anyhow::Result<bool> {
    if daemon::connect_to_daemon().await.is_none() {
        let mut child = auto_start_daemon(config_path)?;
        wait_for_daemon_ready(Some(&mut child)).await?;
        if announce {
            print_info_line("Started background service.");
        }
        return Ok(true);
    }
    Ok(false)
}

pub(crate) async fn daemon_running() -> bool {
    daemon::connect_to_daemon().await.is_some()
}

pub(crate) async fn daemon_query<T>(
    request: &plug_core::ipc::IpcRequest,
    decode: impl FnOnce(plug_core::ipc::IpcResponse) -> Option<T>,
) -> (DaemonQueryAvailability, Option<T>) {
    if !daemon_running().await {
        return (DaemonQueryAvailability::Unreachable, None);
    }

    match daemon::ipc_request(request).await {
        Ok(response) => match decode(response) {
            Some(value) => (DaemonQueryAvailability::Available, Some(value)),
            None => (DaemonQueryAvailability::IpcUnavailable, None),
        },
        Err(_) => (DaemonQueryAvailability::IpcUnavailable, None),
    }
}

pub(crate) async fn cmd_connect(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    if let Err(error) = connect_via_daemon(config_path).await {
        match daemon_proxy_failure_policy(&error) {
            DaemonProxyFailurePolicy::RefuseStandalone => {
                tracing::error!(
                    error = %error,
                        "daemon proxy failed; refusing to start a private engine"
                );
                return Err(error);
            }
        }
    }
    Ok(())
}

pub(crate) async fn cmd_start(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let started = ensure_daemon_with_feedback(config_path, false).await?;

    if matches!(output, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "command": "start",
                "started": started,
                "running": daemon::connect_to_daemon().await.is_some(),
            }))?
        );
        return Ok(());
    }

    print_banner("◆", "Service", "Shared background runtime");
    if started {
        print_success_line("Started shared background service.");
    } else {
        print_info_line("Shared background service is already running.");
    }
    Ok(())
}

pub(crate) async fn cmd_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    // Claim daemon ownership before Engine::start() can read Keychain entries
    // or spawn upstream processes. Concurrent `plug connect` auto-starts lose
    // here and simply retry the winning daemon's socket.
    let runtime_lock = daemon::acquire_runtime_lock()?;
    let config_path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    let config = plug_core::config::load_config(Some(&config_path))?;
    preflight_http_bind(&config.http)?;
    let engine = std::sync::Arc::new(plug_core::engine::Engine::new(config));
    engine.start().await?;
    let http_runtime = build_configured_http_runtime(&engine.config(), &engine)?;
    let cancel = engine.cancel_token().clone();
    drop(plug_core::watcher::spawn_config_watcher(
        engine.clone(),
        config_path.clone(),
        cancel.clone(),
        engine.tracker(),
    ));
    let http_config = engine.config().http.clone();
    let http_future = serve_router(
        http_runtime.router,
        &http_config,
        engine.cancel_token().clone(),
    );
    tokio::pin!(http_future);
    let daemon_future = daemon::run_daemon_with_lock(
        engine.clone(),
        config_path,
        engine.config().daemon_grace_period_secs,
        Some(http_runtime.sessions),
        runtime_lock,
    );
    tokio::pin!(daemon_future);
    tokio::select! {
        result = &mut http_future => {
            result?;
        }
        result = &mut daemon_future => {
            result?;
        }
        _ = daemon::shutdown_signal(cancel) => {}
    }
    engine.shutdown().await;
    Ok(())
}

pub(crate) async fn cmd_daemon_stop() -> anyhow::Result<()> {
    let auth_token = daemon::read_auth_token()?;
    let req = plug_core::ipc::IpcRequest::Shutdown { auth_token };
    match daemon::ipc_request(&req).await? {
        plug_core::ipc::IpcResponse::Ok => println!("stopped"),
        plug_core::ipc::IpcResponse::Error { code, message } => {
            anyhow::bail!("{code}: {message}");
        }
        other => anyhow::bail!("unexpected daemon response: {other:?}"),
    }
    Ok(())
}

pub(crate) async fn cmd_serve(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    cmd_daemon(config_path).await
}

fn resolve_downstream_bearer_token(
    http: &plug_core::config::HttpConfig,
) -> anyhow::Result<Option<Arc<str>>> {
    match http.auth_mode {
        plug_core::config::DownstreamAuthMode::Auto => {
            let externally_exposed = !plug_core::config::http_bind_is_loopback(&http.bind_address)
                || plug_core::config::http_public_base_url_is_non_loopback(
                    http.public_base_url.as_deref(),
                );
            if externally_exposed {
                let token_path = plug_core::auth::http_auth_token_path(http.port);
                let token = plug_core::auth::load_or_generate_token(&token_path)?;
                tracing::info!("HTTP auth enabled (auto mode: server reachable off-loopback)");
                Ok(Some(Arc::<str>::from(token.as_str())))
            } else {
                Ok(None)
            }
        }
        plug_core::config::DownstreamAuthMode::None => Ok(None),
        plug_core::config::DownstreamAuthMode::Bearer => {
            let token_path = plug_core::auth::http_auth_token_path(http.port);
            let token = plug_core::auth::load_or_generate_token(&token_path)?;
            tracing::info!("HTTP bearer auth enabled");
            Ok(Some(Arc::<str>::from(token.as_str())))
        }
        plug_core::config::DownstreamAuthMode::Oauth => Ok(None),
    }
}

fn preflight_http_bind(http: &plug_core::config::HttpConfig) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("{}:{}", http.bind_address, http.port).parse()?;
    let listener = std::net::TcpListener::bind(addr).map_err(|error| {
        anyhow::anyhow!("failed to bind downstream HTTP address {addr}: {error}")
    })?;
    drop(listener);
    Ok(())
}

async fn serve_router(
    router: Router,
    http: &plug_core::config::HttpConfig,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("{}:{}", http.bind_address, http.port).parse()?;
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        cancel.cancelled().await;
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
    });

    if let (Some(cert_path), Some(key_path)) = (&http.tls_cert_path, &http.tls_key_path) {
        plug_core::tls::ensure_rustls_provider_installed();
        let tls_config =
            axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path).await?;
        println!("serving on https://{addr}");
        axum_server::bind_rustls(addr, tls_config)
            .handle(handle)
            .serve(router.into_make_service())
            .await?;
    } else {
        println!("serving on http://{addr}");
        axum_server::bind(addr)
            .handle(handle)
            .serve(router.into_make_service())
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{clear_test_runtime_paths, run_daemon, set_test_runtime_paths};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use plug_core::session::SessionStore;
    use rcgen::generate_simple_self_signed;
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, RootCertStore};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::TlsConnector;
    use tower::util::ServiceExt;

    fn unique_temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "plug-runtime-{}-{}-{}",
            label,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn local_operator_authority_matches_loopback_listener_family() {
        assert_eq!(
            local_operator_authority("127.0.0.1", 3282),
            "127.0.0.1:3282"
        );
        assert_eq!(local_operator_authority("0.0.0.0", 3282), "127.0.0.1:3282");
        assert_eq!(local_operator_authority("::1", 3282), "[::1]:3282");
        assert_eq!(local_operator_authority("::", 3282), "[::1]:3282");
        assert_eq!(
            local_operator_connection_authority("192.0.2.10", 3282),
            "192.0.2.10:3282"
        );
        assert_eq!(
            local_operator_connection_authority("127.0.0.2", 3282),
            "127.0.0.2:3282"
        );
        assert_eq!(
            local_operator_connection_authority("2001:db8::10", 3282),
            "[2001:db8::10]:3282"
        );
    }

    #[test]
    fn failed_modern_discovery_probe_falls_back_to_explicit_legacy_initialize() {
        let mut protocol = StdioProtocolState::default();
        let discovery = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "server/discover",
            "params": {}
        });
        assert!(!protocol.observe_inbound(&discovery, true));
        assert!(!protocol.observe_outbound(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "error": {"code": -32601, "message": "not supported"}
        })));

        let mut initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tasks": {"list": {}}},
                "clientInfo": {"name": "legacy-after-probe", "version": "1.0"}
            }
        });
        assert!(!protocol.observe_inbound(&initialize, true));
        plug_core::protocol::rewrite_legacy_request(&mut initialize);
        assert!(initialize.pointer("/params/capabilities/tasks").is_none());
        assert!(
            initialize
                .pointer("/params/capabilities/experimental/plug.dev~1legacy-tasks")
                .is_some()
        );

        let mut response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "result": {"resultType": "complete", "tools": []}
        });
        assert!(!protocol.observe_outbound(&response));
        plug_core::protocol::rewrite_legacy_response(&mut response, false);
        assert!(response.pointer("/result/resultType").is_none());
    }

    #[test]
    fn successful_modern_discovery_confirms_modern_stdio_era() {
        let mut protocol = StdioProtocolState::default();
        let discovery = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "server/discover",
            "params": {}
        });
        assert!(!protocol.observe_inbound(&discovery, true));
        assert!(protocol.observe_outbound(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "result": {"supportedVersions": ["2026-07-28"]}
        })));
        assert!(
            !protocol.observe_inbound(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 10,
                    "method": "tools/list",
                    "params": {
                        "_meta": {
                            "io.modelcontextprotocol/protocolVersion": "2026-07-28"
                        }
                    }
                }),
                false,
            ),
            "a live gate disable must clear the confirmed modern era"
        );
    }

    // Shared with the daemon and ipc_proxy test modules so all global runtime-path
    // tests serialize on one lock (see daemon::runtime_paths_test_lock).
    fn runtime_path_test_lock() -> &'static tokio::sync::Mutex<()> {
        crate::daemon::runtime_paths_test_lock()
    }

    async fn spawn_http_test_server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });
        (
            format!("http://{}{}", addr, OPERATOR_LIVE_SESSIONS_PATH),
            handle,
        )
    }

    async fn spawn_https_test_server(
        router: Router,
    ) -> anyhow::Result<(
        SocketAddr,
        CancellationToken,
        rustls::pki_types::CertificateDer<'static>,
    )> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        drop(listener);

        let cert = generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
        let cert_der = cert.cert.der().clone();
        let cert_pem = cert.cert.pem();
        let key_pem = cert.signing_key.serialize_pem();

        let temp = std::env::temp_dir().join(format!(
            "plug-https-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir_all(&temp)?;
        let cert_path = temp.join("cert.pem");
        let key_path = temp.join("key.pem");
        std::fs::write(&cert_path, &cert_pem)?;
        std::fs::write(&key_path, &key_pem)?;

        let cancel = CancellationToken::new();
        let http = plug_core::config::HttpConfig {
            modern_downstream_enabled: false,
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            public_base_url: None,
            oauth_scopes: None,
            bind_address: "127.0.0.1".to_string(),
            port: addr.port(),
            allowed_origins: Vec::new(),
            tls_cert_path: Some(cert_path),
            tls_key_path: Some(key_path),
            session_timeout_secs: 1800,
            max_sessions: 100,
            sse_channel_capacity: 32,
        };

        tokio::spawn({
            let cancel = cancel.clone();
            async move {
                let _ = serve_router(router, &http, cancel).await;
            }
        });

        Ok((addr, cancel, cert_der))
    }

    async fn send_https_request(
        addr: SocketAddr,
        cert_der: rustls::pki_types::CertificateDer<'static>,
        request: String,
    ) -> String {
        let mut tls = connect_https_stream(addr, cert_der).await;
        tls.write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        String::from_utf8(response).expect("utf8 response")
    }

    async fn connect_https_stream(
        addr: SocketAddr,
        cert_der: rustls::pki_types::CertificateDer<'static>,
    ) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
        plug_core::tls::ensure_rustls_provider_installed();
        let mut roots = RootCertStore::empty();
        roots.add(cert_der).expect("add test cert to roots");
        let client_config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(client_config));
        let tcp = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect to https server");
        let server_name = ServerName::try_from("localhost").expect("valid server name");
        connector
            .connect(server_name, tcp)
            .await
            .expect("complete tls handshake")
    }

    #[tokio::test]
    async fn serve_router_supports_https() {
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");
        let sessions: Arc<dyn plug_core::session::SessionStore> =
            Arc::new(plug_core::session::StatefulSessionStore::new(1800, 100));
        sessions.spawn_cleanup_task(engine.cancel_token().clone());
        let state = Arc::new(plug_core::http::server::HttpState {
            router: engine.tool_router().clone(),
            sessions,
            cancel: engine.cancel_token().clone(),
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
            client_capabilities: dashmap::DashMap::new(),
        });
        let router = build_runtime_router(
            state,
            Arc::from("test-operator-token"),
            Arc::from("127.0.0.1:3282"),
        );

        let (addr, cancel, cert_der) = spawn_https_test_server(router)
            .await
            .expect("start https test server");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let initialize_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "https-test", "version": "1.0"}
            }
        })
        .to_string();
        let initialize_request = format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            initialize_body.len(),
            initialize_body
        );
        let initialize_response =
            send_https_request(addr, cert_der.clone(), initialize_request).await;
        assert!(
            initialize_response.contains("200 OK"),
            "unexpected initialize response: {initialize_response}"
        );
        assert!(
            initialize_response
                .to_ascii_lowercase()
                .contains("mcp-session-id:"),
            "missing session id header: {initialize_response}"
        );
        assert!(
            initialize_response.contains("\"serverInfo\""),
            "missing initialize payload: {initialize_response}"
        );

        let session_header = initialize_response
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("mcp-session-id:"))
            .and_then(|line| line.split_once(':'))
            .map(|(_, value)| value.trim().to_string())
            .expect("session id header");

        let list_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })
        .to_string();
        let list_request = format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nMcp-Session-Id: {}\r\nMCP-Protocol-Version: 2025-11-25\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            session_header,
            list_body.len(),
            list_body
        );
        let list_response = send_https_request(addr, cert_der.clone(), list_request).await;
        assert!(
            list_response.contains("200 OK"),
            "unexpected tools/list response: {list_response}"
        );
        assert!(
            list_response.contains("\"tools\""),
            "missing tools payload: {list_response}"
        );

        let mut sse = connect_https_stream(addr, cert_der).await;
        let sse_request = format!(
            "GET /mcp HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\nMcp-Session-Id: {}\r\nConnection: close\r\n\r\n",
            session_header
        );
        sse.write_all(sse_request.as_bytes())
            .await
            .expect("write sse request");
        let mut buf = vec![0_u8; 1024];
        let n = tokio::time::timeout(std::time::Duration::from_secs(1), sse.read(&mut buf))
            .await
            .expect("sse read timeout")
            .expect("read sse bytes");
        let sse_response = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(
            sse_response.contains("200 OK"),
            "unexpected sse response: {sse_response}"
        );
        assert!(
            sse_response.contains("text/event-stream"),
            "missing sse content type: {sse_response}"
        );
        assert!(
            sse_response.contains("id: 0"),
            "missing sse priming event: {sse_response}"
        );

        cancel.cancel();
        engine.shutdown().await;
    }

    #[test]
    fn resolve_downstream_bearer_token_auto_loopback_disables_auth() {
        let http = plug_core::config::HttpConfig::default();
        let token = resolve_downstream_bearer_token(&http).expect("resolve token");
        assert!(token.is_none());
    }

    #[test]
    fn resolve_downstream_bearer_token_auto_loopback_no_public_base_url_disables_auth() {
        let http = plug_core::config::HttpConfig {
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            public_base_url: None,
            ..plug_core::config::HttpConfig::default()
        };
        let token = resolve_downstream_bearer_token(&http).expect("resolve token");
        assert!(token.is_none());
    }

    #[test]
    fn resolve_downstream_bearer_token_none_disables_auth() {
        let http = plug_core::config::HttpConfig {
            auth_mode: plug_core::config::DownstreamAuthMode::None,
            bind_address: "0.0.0.0".to_string(),
            ..plug_core::config::HttpConfig::default()
        };
        let token = resolve_downstream_bearer_token(&http).expect("resolve token");
        assert!(token.is_none());
    }

    #[test]
    fn resolve_downstream_bearer_token_oauth_uses_non_bearer_path() {
        let http = plug_core::config::HttpConfig {
            auth_mode: plug_core::config::DownstreamAuthMode::Oauth,
            public_base_url: Some("https://plug.example.com".to_string()),
            ..plug_core::config::HttpConfig::default()
        };
        let token = resolve_downstream_bearer_token(&http).expect("oauth should skip bearer token");
        assert!(token.is_none());
    }

    #[test]
    fn preflight_http_bind_fails_fast_when_port_is_occupied() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind occupied port");
        let port = listener.local_addr().expect("listener addr").port();

        let http = plug_core::config::HttpConfig {
            bind_address: "127.0.0.1".to_string(),
            port,
            ..plug_core::config::HttpConfig::default()
        };

        let error = preflight_http_bind(&http).expect_err("expected preflight bind failure");
        assert!(
            error
                .to_string()
                .contains("failed to bind downstream HTTP address"),
            "unexpected preflight error: {error}"
        );
    }

    #[test]
    fn merge_live_session_sources_marks_transport_complete_when_both_sources_exist() {
        let daemon = vec![plug_core::ipc::IpcLiveSessionInfo {
            transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
            client_id: Some("daemon".to_string()),
            session_id: "daemon-1".to_string(),
            client_type: plug_core::types::ClientType::ClaudeCode,
            client_info: Some("Claude Code".to_string()),
            connected_secs: 10,
            last_activity_secs: None,
        }];
        let http = vec![plug_core::ipc::IpcLiveSessionInfo {
            transport: plug_core::ipc::LiveSessionTransport::Http,
            client_id: None,
            session_id: "http-1".to_string(),
            client_type: plug_core::types::ClientType::ClaudeDesktop,
            client_info: None,
            connected_secs: 5,
            last_activity_secs: Some(1),
        }];

        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly,
            LiveSessionSourceState::Available(daemon),
            LiveSessionSourceState::Available(http),
        );

        assert_eq!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete
        );
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn merge_live_session_sources_marks_transport_complete_when_http_source_is_idle() {
        let daemon = vec![plug_core::ipc::IpcLiveSessionInfo {
            transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
            client_id: Some("daemon".to_string()),
            session_id: "daemon-1".to_string(),
            client_type: plug_core::types::ClientType::ClaudeCode,
            client_info: Some("Claude Code".to_string()),
            connected_secs: 10,
            last_activity_secs: None,
        }];

        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly,
            LiveSessionSourceState::Available(daemon),
            LiveSessionSourceState::Available(Vec::new()),
        );

        assert_eq!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete
        );
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn merge_live_session_sources_preserves_daemon_proxy_only_scope_when_http_unavailable() {
        let daemon = vec![plug_core::ipc::IpcLiveSessionInfo {
            transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
            client_id: Some("daemon".to_string()),
            session_id: "daemon-1".to_string(),
            client_type: plug_core::types::ClientType::ClaudeCode,
            client_info: Some("Claude Code".to_string()),
            connected_secs: 10,
            last_activity_secs: None,
        }];

        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly,
            LiveSessionSourceState::Available(daemon),
            LiveSessionSourceState::Unavailable,
        );

        assert_eq!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly
        );
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn merge_live_session_sources_marks_http_only_without_daemon() {
        let http = vec![plug_core::ipc::IpcLiveSessionInfo {
            transport: plug_core::ipc::LiveSessionTransport::Http,
            client_id: None,
            session_id: "http-1".to_string(),
            client_type: plug_core::types::ClientType::ClaudeDesktop,
            client_info: None,
            connected_secs: 5,
            last_activity_secs: Some(1),
        }];

        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::Unavailable,
            LiveSessionSourceState::Unavailable,
            LiveSessionSourceState::Available(http),
        );

        assert_eq!(scope, plug_core::ipc::LiveSessionInventoryScope::HttpOnly);
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn merge_live_session_sources_marks_http_only_when_http_source_is_idle() {
        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::Unavailable,
            LiveSessionSourceState::Unavailable,
            LiveSessionSourceState::Available(Vec::new()),
        );

        assert_eq!(scope, plug_core::ipc::LiveSessionInventoryScope::HttpOnly);
        assert!(sessions.is_empty());
    }

    #[test]
    fn merge_live_session_sources_marks_unavailable_when_no_sources_exist() {
        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::Unavailable,
            LiveSessionSourceState::Unavailable,
            LiveSessionSourceState::Unavailable,
        );

        assert_eq!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::Unavailable
        );
        assert!(sessions.is_empty());
    }

    #[test]
    fn live_inventory_availability_marks_missing_sources() {
        let complete = live_inventory_availability(
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete,
        );
        assert!(!complete.partial);
        assert!(complete.unavailable_sources.is_empty());

        let daemon_only =
            live_inventory_availability(plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly);
        assert!(daemon_only.partial);
        assert_eq!(daemon_only.unavailable_sources, vec!["http"]);

        let http_only =
            live_inventory_availability(plug_core::ipc::LiveSessionInventoryScope::HttpOnly);
        assert!(http_only.partial);
        assert_eq!(http_only.unavailable_sources, vec!["daemon_proxy"]);

        let unavailable =
            live_inventory_availability(plug_core::ipc::LiveSessionInventoryScope::Unavailable);
        assert!(unavailable.partial);
        assert_eq!(
            unavailable.unavailable_sources,
            vec!["daemon_proxy", "http"]
        );
    }

    #[test]
    fn live_inventory_metadata_reports_counts_and_availability() {
        let sessions = vec![
            plug_core::ipc::IpcLiveSessionInfo {
                transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
                client_id: Some("daemon".to_string()),
                session_id: "daemon-1".to_string(),
                client_type: plug_core::types::ClientType::ClaudeCode,
                client_info: Some("Claude Code".to_string()),
                connected_secs: 10,
                last_activity_secs: None,
            },
            plug_core::ipc::IpcLiveSessionInfo {
                transport: plug_core::ipc::LiveSessionTransport::Http,
                client_id: None,
                session_id: "http-1".to_string(),
                client_type: plug_core::types::ClientType::ClaudeDesktop,
                client_info: None,
                connected_secs: 5,
                last_activity_secs: Some(1),
            },
        ];

        let metadata = live_inventory_metadata(
            &sessions,
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete,
        );

        assert_eq!(metadata.session_count, 2);
        assert_eq!(metadata.session_transports.daemon_proxy, 1);
        assert_eq!(metadata.session_transports.http, 1);
        assert_eq!(metadata.session_transports.sse, 0);
        assert!(!metadata.availability.partial);
        assert!(metadata.http_sessions_included);
    }

    #[tokio::test]
    async fn wait_for_daemon_ready_fails_fast_when_spawned_process_exits() {
        let _guard = runtime_path_test_lock().lock().await;
        let runtime_root = unique_temp_dir("daemon-ready-runtime");
        let state_root = unique_temp_dir("daemon-ready-state");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn short-lived child");

        let error = wait_for_daemon_ready(Some(&mut child))
            .await
            .expect_err("expected readiness wait to fail");
        assert!(
            error
                .to_string()
                .contains("daemon exited before becoming ready"),
            "unexpected readiness failure: {error}"
        );

        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    #[tokio::test]
    async fn wait_for_daemon_ready_follows_the_concurrent_lock_winner() {
        let _guard = runtime_path_test_lock().lock().await;
        let suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let runtime_root = std::path::PathBuf::from(format!("/tmp/plug-dr-{suffix}"));
        let state_root = std::path::PathBuf::from(format!("/tmp/plug-ds-{suffix}"));
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let winning_lock = daemon::acquire_runtime_lock().expect("winning daemon lock");
        let mut losing_child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 0.02; exit 1")
            .spawn()
            .expect("spawn losing daemon stand-in");

        let listener_task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let socket = daemon::socket_path();
            let listener = tokio::net::UnixListener::bind(&socket).expect("bind winner socket");
            tokio::time::sleep(Duration::from_secs(1)).await;
            listener
        });

        let stream = tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_daemon_ready(Some(&mut losing_child)),
        )
        .await
        .expect("readiness wait timed out")
        .expect("losing auto-start should wait for the lock winner");
        drop(stream);

        listener_task.abort();
        drop(winning_lock);
        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    #[tokio::test]
    async fn daemon_failures_never_allow_standalone_fallback() {
        let _guard = runtime_path_test_lock().lock().await;
        let suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let runtime_root = std::path::PathBuf::from(format!("/tmp/plug-cr-{suffix}"));
        let state_root = std::path::PathBuf::from(format!("/tmp/plug-cs-{suffix}"));
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let winning_lock = daemon::acquire_runtime_lock().expect("winning daemon lock");
        let mut losing_child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 1")
            .spawn()
            .expect("spawn losing daemon stand-in");

        let error = wait_for_daemon_ready_with_timeouts(
            Some(&mut losing_child),
            Duration::from_millis(100),
            Duration::from_millis(50),
        )
        .await
        .expect_err("held winner without a socket must fail closed");
        assert!(error.downcast_ref::<DaemonStartupContention>().is_some());
        assert_eq!(
            daemon_proxy_failure_policy(&error),
            DaemonProxyFailurePolicy::RefuseStandalone,
            "lock contention must refuse duplicate standalone startup"
        );
        assert_eq!(
            daemon_proxy_failure_policy(&anyhow::anyhow!("ordinary daemon failure")),
            DaemonProxyFailurePolicy::RefuseStandalone,
            "ordinary daemon failures must not fork a private engine"
        );

        drop(winning_lock);
        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    #[tokio::test]
    async fn wait_for_daemon_ready_succeeds_when_daemon_is_running() {
        let _guard = runtime_path_test_lock().lock().await;
        let temp = std::env::temp_dir().join(format!(
            "pr-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");

        let daemon_engine = Arc::clone(&engine);
        let daemon_handle =
            tokio::spawn(async move { run_daemon(daemon_engine, config_path, 0, None).await });

        tokio::time::sleep(Duration::from_millis(100)).await;
        if daemon_handle.is_finished() {
            let daemon_result = daemon_handle.await.expect("daemon task join");
            panic!("daemon exited before readiness: {daemon_result:?}");
        }

        let stream = tokio::time::timeout(Duration::from_secs(5), wait_for_daemon_ready(None))
            .await
            .expect("daemon readiness wait timed out")
            .expect("daemon should become ready");
        drop(stream);

        engine.shutdown().await;
        let daemon_result = tokio::time::timeout(Duration::from_secs(5), daemon_handle)
            .await
            .expect("daemon join timed out")
            .expect("daemon task join");
        assert!(daemon_result.is_ok(), "daemon should shut down cleanly");

        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    #[tokio::test]
    async fn ensure_daemon_with_feedback_returns_false_when_daemon_is_already_running() {
        let _guard = runtime_path_test_lock().lock().await;
        let temp = std::env::temp_dir().join(format!(
            "pr-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");

        let daemon_engine = Arc::clone(&engine);
        let daemon_config_path = config_path.clone();
        let daemon_handle =
            tokio::spawn(
                async move { run_daemon(daemon_engine, daemon_config_path, 0, None).await },
            );

        tokio::time::sleep(Duration::from_millis(100)).await;
        if daemon_handle.is_finished() {
            let daemon_result = daemon_handle.await.expect("daemon task join");
            panic!("daemon exited before readiness: {daemon_result:?}");
        }

        tokio::time::timeout(Duration::from_secs(5), wait_for_daemon_ready(None))
            .await
            .expect("daemon readiness wait timed out")
            .expect("daemon should become ready");

        let started = ensure_daemon_with_feedback(Some(&config_path), false)
            .await
            .expect("ensure_daemon_with_feedback should succeed");
        assert!(
            !started,
            "already-running daemon should not report fresh start"
        );

        engine.shutdown().await;
        let daemon_result = tokio::time::timeout(Duration::from_secs(5), daemon_handle)
            .await
            .expect("daemon join timed out")
            .expect("daemon task join");
        assert!(daemon_result.is_ok(), "daemon should shut down cleanly");

        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    #[tokio::test]
    async fn operator_live_sessions_requires_token() {
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");
        let sessions: Arc<dyn plug_core::session::SessionStore> =
            Arc::new(plug_core::session::StatefulSessionStore::new(1800, 100));
        let state = Arc::new(plug_core::http::server::HttpState {
            router: engine.tool_router().clone(),
            sessions,
            cancel: engine.cancel_token().clone(),
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
            client_capabilities: dashmap::DashMap::new(),
        });

        let app = build_runtime_router(
            state,
            Arc::from("expected-token"),
            Arc::from("127.0.0.1:3282"),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri(OPERATOR_LIVE_SESSIONS_PATH)
                    .header("host", "127.0.0.1:3282")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn operator_live_sessions_returns_http_snapshot_inventory() {
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");
        let store = plug_core::session::StatefulSessionStore::new(1800, 100);
        let session_id = store.create_session().expect("session");
        store
            .set_client_type(&session_id, plug_core::types::ClientType::ClaudeDesktop)
            .expect("set client type");
        let sessions: Arc<dyn plug_core::session::SessionStore> = Arc::new(store);
        let state = Arc::new(plug_core::http::server::HttpState {
            router: engine.tool_router().clone(),
            sessions,
            cancel: engine.cancel_token().clone(),
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
            client_capabilities: dashmap::DashMap::new(),
        });

        let app = build_runtime_router(
            state,
            Arc::from("expected-token"),
            Arc::from("127.0.0.1:3282"),
        );
        let response = proof_authenticated_request(
            &app,
            "GET",
            OPERATOR_LIVE_SESSIONS_PATH,
            "expected-token",
            false,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let parsed: OperatorLiveSessionsResponse =
            serde_json::from_slice(&body).expect("json body");
        assert_eq!(parsed.sessions.len(), 1);
        assert_eq!(
            parsed.sessions[0].transport,
            plug_core::ipc::LiveSessionTransport::Http
        );
        assert_eq!(
            parsed.sessions[0].client_type,
            plug_core::types::ClientType::ClaudeDesktop
        );
        engine.shutdown().await;
    }

    async fn owner_operator_test_router_with_authority(authority: Arc<str>) -> (Router, PathBuf) {
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");
        let sessions: Arc<dyn plug_core::session::SessionStore> =
            Arc::new(plug_core::session::StatefulSessionStore::new(1800, 100));
        let state_path = unique_temp_dir("owner-operator").join("oauth.json");
        let manager = plug_core::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            plug_core::downstream_oauth::DownstreamOauthConfig {
                public_base_url: "https://plug.example.com".to_string(),
                oauth_scopes: vec!["tools:read".to_string()],
                local_port: 3282,
            },
            state_path.clone(),
        )
        .expect("OAuth manager");
        let state = Arc::new(plug_core::http::server::HttpState {
            router: engine.tool_router().clone(),
            sessions,
            cancel: engine.cancel_token().clone(),
            auth_mode: plug_core::config::DownstreamAuthMode::Oauth,
            downstream_oauth: Some(manager),
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
            client_capabilities: dashmap::DashMap::new(),
        });
        (
            build_runtime_router(state, Arc::from("operator-secret"), authority),
            state_path,
        )
    }

    async fn owner_operator_test_router() -> (Router, PathBuf) {
        owner_operator_test_router_with_authority(Arc::from("127.0.0.1:3282")).await
    }

    async fn proof_authenticated_request(
        app: &Router,
        method: &str,
        path: &str,
        token: &str,
        allow_empty: bool,
    ) -> axum::response::Response {
        let client_nonce = plug_core::auth::generate_auth_token();
        let challenge = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(OPERATOR_PROOF_PATH)
                    .header("host", "127.0.0.1:3282")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "client_nonce": client_nonce,
                            "method": method,
                            "path": path,
                            "allow_empty": allow_empty,
                        })
                        .to_string(),
                    ))
                    .expect("challenge request"),
            )
            .await
            .expect("challenge response");
        assert_eq!(challenge.status(), StatusCode::OK);
        let challenge = serde_json::from_slice::<OperatorProofResponse>(
            &to_bytes(challenge.into_body(), usize::MAX)
                .await
                .expect("challenge body"),
        )
        .expect("challenge JSON");
        let proof = operator_request_proof(
            token,
            &client_nonce,
            &challenge.server_nonce,
            method,
            path,
            allow_empty,
        );
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:3282")
            .header(OPERATOR_CLIENT_NONCE_HEADER, client_nonce)
            .header(OPERATOR_SERVER_NONCE_HEADER, challenge.server_nonce)
            .header(OPERATOR_PROOF_HEADER, proof);
        if allow_empty {
            request = request.header(OPERATOR_ALLOW_EMPTY_HEADER, "true");
        }
        app.clone()
            .oneshot(request.body(Body::empty()).expect("operator request"))
            .await
            .expect("operator response")
    }

    #[tokio::test]
    async fn owner_bootstrap_rejects_public_forwarded_request() {
        let (app, state_path) = owner_operator_test_router().await;
        for forwarded_header in ["forwarded", "x-forwarded-for", "cf-connecting-ip"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(OPERATOR_OWNER_BOOTSTRAP_PATH)
                        .header("host", "127.0.0.1:3282")
                        .header(OPERATOR_TOKEN_HEADER, "operator-secret")
                        .header(forwarded_header, "203.0.113.4")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let _ = std::fs::remove_file(state_path);
    }

    #[tokio::test]
    async fn operator_request_proof_is_bound_to_nonce_intent_and_single_use() {
        let (app, state_path) = owner_operator_test_router().await;
        let client_nonce = "11".repeat(32);
        let method = "POST";
        let path = OPERATOR_OWNER_BOOTSTRAP_PATH;
        let challenge = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(OPERATOR_PROOF_PATH)
                    .header("host", "127.0.0.1:3282")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "client_nonce": client_nonce,
                            "method": method,
                            "path": path,
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(challenge.status(), StatusCode::OK);
        let body = to_bytes(challenge.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let challenge: OperatorProofResponse = serde_json::from_slice(&body).expect("proof JSON");
        assert!(plug_core::auth::verify_auth_token(
            &challenge.proof,
            &operator_challenge_proof(
                "operator-secret",
                &client_nonce,
                &challenge.server_nonce,
                method,
                path,
                false,
            )
        ));

        let request_proof = operator_request_proof(
            "operator-secret",
            &client_nonce,
            &challenge.server_nonce,
            method,
            path,
            false,
        );
        let authenticated = || {
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:3282")
                .header(OPERATOR_CLIENT_NONCE_HEADER, &client_nonce)
                .header(OPERATOR_SERVER_NONCE_HEADER, &challenge.server_nonce)
                .header(OPERATOR_PROOF_HEADER, &request_proof)
                .body(Body::empty())
                .expect("request")
        };
        let first = app
            .clone()
            .oneshot(authenticated())
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::OK);
        let replay = app.oneshot(authenticated()).await.expect("response");
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_file(state_path);
    }

    #[tokio::test]
    async fn operator_final_removal_intent_is_bound_into_request_proof() {
        let (app, state_path) = owner_operator_test_router().await;
        let client_nonce = "33".repeat(32);
        let method = "DELETE";
        let path = format!("{OPERATOR_OWNER_CREDENTIALS_PATH}/missing");
        let challenge = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(OPERATOR_PROOF_PATH)
                    .header("host", "127.0.0.1:3282")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "client_nonce": client_nonce,
                            "method": method,
                            "path": path,
                            "allow_empty": false,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let challenge: OperatorProofResponse =
            serde_json::from_slice(&to_bytes(challenge.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        let proof = operator_request_proof(
            "operator-secret",
            &client_nonce,
            &challenge.server_nonce,
            method,
            &path,
            false,
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&path)
                    .header("host", "127.0.0.1:3282")
                    .header(OPERATOR_CLIENT_NONCE_HEADER, client_nonce)
                    .header(OPERATOR_SERVER_NONCE_HEADER, challenge.server_nonce)
                    .header(OPERATOR_PROOF_HEADER, proof)
                    .header(OPERATOR_ALLOW_EMPTY_HEADER, "true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_file(state_path);
    }

    #[tokio::test]
    async fn auth_owner_operator_http_e2e_bootstraps_and_lists_without_token_disclosure() {
        plug_core::tls::ensure_rustls_provider_installed();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener address");
        let authority = addr.to_string();
        let (app, state_path) =
            owner_operator_test_router_with_authority(Arc::from(authority.as_str())).await;
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve operator router");
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP client");

        let bootstrap = send_authenticated_operator_request(
            &client,
            reqwest::Url::parse(&format!("http://{addr}{OPERATOR_OWNER_BOOTSTRAP_PATH}")).unwrap(),
            &authority,
            "operator-secret",
            reqwest::Method::POST,
            false,
        )
        .await
        .expect("authenticated bootstrap");
        assert_eq!(bootstrap.status(), reqwest::StatusCode::OK);
        let bootstrap: serde_json::Value = bootstrap.json().await.expect("bootstrap JSON");
        assert!(
            bootstrap["enrollment_url"]
                .as_str()
                .unwrap()
                .starts_with("https://plug.example.com/oauth/owner/enroll#bootstrap=")
        );

        let list = send_authenticated_operator_request(
            &client,
            reqwest::Url::parse(&format!("http://{addr}{OPERATOR_OWNER_CREDENTIALS_PATH}"))
                .unwrap(),
            &authority,
            "operator-secret",
            reqwest::Method::GET,
            false,
        )
        .await
        .expect("authenticated list");
        assert_eq!(list.status(), reqwest::StatusCode::OK);
        assert_eq!(
            list.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!([])
        );

        server.abort();
        let _ = std::fs::remove_file(state_path);
    }

    #[tokio::test]
    async fn operator_owner_boundary_rejects_missing_token_and_non_loopback_host() {
        let (app, state_path) = owner_operator_test_router().await;
        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(OPERATOR_OWNER_BOOTSTRAP_PATH)
                    .header("host", "127.0.0.1:3282")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let wrong = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(OPERATOR_OWNER_BOOTSTRAP_PATH)
                    .header("host", "127.0.0.1:3282")
                    .header(OPERATOR_TOKEN_HEADER, "wrong-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let public_host = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(OPERATOR_OWNER_BOOTSTRAP_PATH)
                    .header("host", "plug.example.com")
                    .header(OPERATOR_TOKEN_HEADER, "operator-secret")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(public_host.status(), StatusCode::FORBIDDEN);

        let duplicate_host = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(OPERATOR_OWNER_BOOTSTRAP_PATH)
                    .header("host", "127.0.0.1:3282")
                    .header("host", "127.0.0.1:3282")
                    .header(OPERATOR_TOKEN_HEADER, "operator-secret")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(duplicate_host.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_file(state_path);
    }

    #[tokio::test]
    async fn operator_owner_bootstrap_list_and_remove() {
        let (app, state_path) = owner_operator_test_router().await;
        let bootstrap = proof_authenticated_request(
            &app,
            "POST",
            OPERATOR_OWNER_BOOTSTRAP_PATH,
            "operator-secret",
            false,
        )
        .await;
        assert_eq!(bootstrap.status(), StatusCode::OK);
        assert_eq!(bootstrap.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(bootstrap.headers()[PRAGMA], "no-cache");
        assert_eq!(bootstrap.headers()[REFERRER_POLICY], "no-referrer");
        let body = to_bytes(bootstrap.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("JSON");
        let enrollment_url = json["enrollment_url"].as_str().expect("enrollment URL");
        assert!(
            enrollment_url.starts_with("https://plug.example.com/oauth/owner/enroll#bootstrap=")
        );
        assert!(!enrollment_url.contains('?'));
        assert!(!enrollment_url.contains("operator-secret"));

        let list = proof_authenticated_request(
            &app,
            "GET",
            OPERATOR_OWNER_CREDENTIALS_PATH,
            "operator-secret",
            false,
        )
        .await;
        assert_eq!(list.status(), StatusCode::OK);
        let body = to_bytes(list.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!([])
        );

        let remove_path = format!("{OPERATOR_OWNER_CREDENTIALS_PATH}/missing");
        let remove =
            proof_authenticated_request(&app, "DELETE", &remove_path, "operator-secret", false)
                .await;
        assert_eq!(remove.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(state_path);
    }

    /// Minimal stdio mock-upstream `ServerConfig` with an `echo` tool.
    fn task_test_mock_config() -> plug_core::config::ServerConfig {
        plug_core::config::ServerConfig {
            command: Some(
                plug_test_harness::mock_server_bin()
                    .to_string_lossy()
                    .into_owned(),
            ),
            args: vec!["--tools".to_string(), "echo".to_string()],
            env: std::collections::HashMap::new(),
            enabled: true,
            transport: plug_core::config::TransportType::Stdio,
            protocol_mode: Default::default(),
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 10,
            call_timeout_secs: 5,
            max_concurrent: 4,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: std::collections::HashMap::new(),
            tool_groups: Vec::new(),
            sandbox: None,
        }
    }

    #[tokio::test]
    async fn oauth_revoke_204_blocks_request_validated_before_revocation_from_creating_task() {
        let mut config = plug_core::config::Config::default();
        config
            .servers
            .insert("mock".to_string(), task_test_mock_config());
        let engine = Arc::new(plug_core::engine::Engine::new(config));
        engine.start().await.expect("engine start");
        let tool_router = engine.tool_router().clone();

        let oauth_path = std::env::temp_dir().join(format!(
            "plug-runtime-revoke-race-{}.json",
            uuid::Uuid::new_v4()
        ));
        let manager = plug_core::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            plug_core::downstream_oauth::DownstreamOauthConfig {
                public_base_url: "https://plug.example.com".to_string(),
                oauth_scopes: vec!["tools:read".to_string(), "tasks:use".to_string()],
                local_port: 3282,
            },
            oauth_path.clone(),
        )
        .expect("OAuth manager");
        let client = manager
            .register_client(
                plug_core::downstream_oauth::ClientRegistrationRequest {
                    redirect_uris: vec!["https://client.example/callback".to_string()],
                    client_name: Some("race client".to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: Some(vec!["authorization_code".to_string()]),
                    response_types: Some(vec!["code".to_string()]),
                    scope: None,
                },
                "runtime-revoke-test",
            )
            .await
            .expect("register client");
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let consent = manager
            .begin_authorization(plug_core::downstream_oauth::AuthorizationRequest {
                response_type: "code",
                client_id: &client.client_id,
                redirect_uri: &client.redirect_uris[0],
                state: "runtime-revoke-state",
                code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                code_challenge_method: "S256",
                scope: Some("tools:read tasks:use"),
                resource: "https://plug.example.com/mcp",
            })
            .await
            .expect("begin authorization");
        let redirect = manager
            .decide_consent(&consent.consent_id, true)
            .await
            .expect("approve consent");
        let code = reqwest::Url::parse(&redirect.location)
            .expect("redirect URL")
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .expect("authorization code");
        let tokens = manager
            .exchange_authorization_code(
                &client.client_id,
                &code,
                &client.redirect_uris[0],
                verifier,
                "https://plug.example.com/mcp",
            )
            .await
            .expect("exchange code");

        // Barrier: middleware validation has completed and captured the old
        // client generation, but task dispatch has not begun.
        let claims = match manager
            .validate_access_token_for(
                &tokens.access_token,
                &["tools:read".to_string()],
                &manager.resource(),
            )
            .await
        {
            plug_core::downstream_oauth::AccessTokenValidation::Valid(claims) => claims,
            other => panic!("token must validate before revocation: {other:?}"),
        };
        let principal = plug_core::types::PrincipalId::downstream_oauth(
            manager.base_url(),
            claims.client_id.clone(),
            claims.resource.clone(),
        );
        let owner = plug_core::tasks::TaskOwner::new(principal.owner_key());
        let context = plug_core::proxy::DownstreamCallContext::http_for_client_with_trace(
            "validated-before-revoke",
            rmcp::model::RequestId::from(rmcp::model::NumberOrString::Number(1)),
            plug_core::types::ClientType::Unknown,
            Arc::<str>::from("00000000000000000000000000000001"),
        )
        .with_authorization(principal, claims.scopes)
        .with_principal_lifecycle(claims.principal_lifecycle);

        let sessions: Arc<dyn plug_core::session::SessionStore> =
            Arc::new(plug_core::session::StatefulSessionStore::new(1800, 100));
        let http_state = Arc::new(plug_core::http::server::HttpState {
            router: tool_router.clone(),
            sessions,
            cancel: engine.cancel_token().clone(),
            auth_mode: plug_core::config::DownstreamAuthMode::Oauth,
            downstream_oauth: Some(manager),
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
            client_capabilities: dashmap::DashMap::new(),
        });
        let app = build_runtime_router(
            http_state,
            Arc::from("operator-secret"),
            Arc::from("127.0.0.1:3282"),
        );
        let revoke_path = format!("{OPERATOR_OAUTH_CLIENTS_PATH}/{}", client.client_id);
        let response =
            proof_authenticated_request(&app, "DELETE", &revoke_path, "operator-secret", false)
                .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let error = tool_router
            .enqueue_tool_task("Mock__echo", None, None, owner.clone(), None, Some(context))
            .await
            .expect_err("a pre-revocation validation cannot create after the 204 boundary");
        assert_eq!(error.code, rmcp::model::ErrorCode(-32001));
        assert_eq!(tool_router.task_count_for_owner(&owner).await, 0);

        engine.shutdown().await;
        let _ = std::fs::remove_file(oauth_path);
    }

    #[tokio::test]
    async fn expired_http_session_cleans_up_tasks() {
        let mut config = plug_core::config::Config::default();
        config
            .servers
            .insert("mock".to_string(), task_test_mock_config());
        let engine = Arc::new(plug_core::engine::Engine::new(config));
        engine.start().await.expect("engine start");
        let tool_router = engine.tool_router().clone();

        let sessions: Arc<dyn plug_core::session::SessionStore> =
            Arc::new(plug_core::session::StatefulSessionStore::new(1800, 100));
        let http_state = Arc::new(plug_core::http::server::HttpState {
            router: tool_router.clone(),
            sessions,
            cancel: engine.cancel_token().clone(),
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
            client_capabilities: dashmap::DashMap::new(),
        });

        let session_id = "expiring-session";
        let owner = plug_core::proxy::ToolRouter::task_owner_for_http_session(session_id);

        // Spot-check a second cleanup also runs, guarding the extraction from
        // silently dropping one of the other seven teardown steps.
        tool_router.set_client_log_level(session_id, rmcp::model::LoggingLevel::Debug);
        assert_eq!(tool_router.log_level(), rmcp::model::LoggingLevel::Debug);

        tool_router
            .enqueue_tool_task("Mock__echo", None, None, owner.clone(), None, None)
            .await
            .expect("enqueue task for expiring session");
        assert_eq!(tool_router.task_count_for_owner(&owner).await, 1);

        cleanup_expired_http_session(&http_state, &tool_router, session_id).await;

        assert_eq!(tool_router.task_count_for_owner(&owner).await, 0);
        assert_eq!(
            tool_router.log_level(),
            rmcp::model::LoggingLevel::Warning,
            "expiry cleanup should also have removed the session's log level"
        );

        engine.shutdown().await;
    }

    #[tokio::test]
    async fn fetch_http_live_sessions_from_returns_unavailable_when_token_missing() {
        let dir = unique_temp_dir("missing-token");
        let token_path = dir.join("operator.token");

        let state = fetch_http_live_sessions_from(
            "http://127.0.0.1:9/nowhere".to_string(),
            "127.0.0.1:9".to_string(),
            &token_path,
        )
        .await;

        assert!(matches!(state, LiveSessionSourceState::Unavailable));
        std::fs::remove_dir_all(dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn fetch_http_live_sessions_from_returns_unavailable_when_token_empty() {
        let dir = unique_temp_dir("empty-token");
        let token_path = dir.join("operator.token");
        std::fs::write(&token_path, "\n").expect("write empty token");

        let state = fetch_http_live_sessions_from(
            "http://127.0.0.1:9/nowhere".to_string(),
            "127.0.0.1:9".to_string(),
            &token_path,
        )
        .await;

        assert!(matches!(state, LiveSessionSourceState::Unavailable));
        std::fs::remove_dir_all(dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn fetch_http_live_sessions_from_returns_unavailable_on_unauthorized() {
        let dir = unique_temp_dir("unauthorized");
        let token_path = dir.join("operator.token");
        std::fs::write(&token_path, "expected-token").expect("write token");
        let app = Router::new().route(
            OPERATOR_LIVE_SESSIONS_PATH,
            get(|| async { StatusCode::UNAUTHORIZED }),
        );
        let (url, handle) = spawn_http_test_server(app).await;

        let parsed = reqwest::Url::parse(&url).unwrap();
        let authority = format!("127.0.0.1:{}", parsed.port().unwrap());
        let state = fetch_http_live_sessions_from(url, authority, &token_path).await;

        handle.abort();
        assert!(matches!(state, LiveSessionSourceState::Unavailable));
        std::fs::remove_dir_all(dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn fetch_http_live_sessions_from_returns_unavailable_on_invalid_json() {
        let dir = unique_temp_dir("invalid-json");
        let token_path = dir.join("operator.token");
        std::fs::write(&token_path, "expected-token").expect("write token");
        let app = Router::new().route(OPERATOR_LIVE_SESSIONS_PATH, get(|| async { "not-json" }));
        let (url, handle) = spawn_http_test_server(app).await;

        let parsed = reqwest::Url::parse(&url).unwrap();
        let authority = format!("127.0.0.1:{}", parsed.port().unwrap());
        let state = fetch_http_live_sessions_from(url, authority, &token_path).await;

        handle.abort();
        assert!(matches!(state, LiveSessionSourceState::Unavailable));
        std::fs::remove_dir_all(dir).expect("cleanup temp dir");
    }
}
