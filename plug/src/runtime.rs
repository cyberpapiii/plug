use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::OutputFormat;
use crate::daemon;
use crate::ui::{print_banner, print_info_line, print_success_line};
use anyhow::Context as _;
use axum::extract::{DefaultBodyLimit, OriginalUri, Path as AxumPath, State};
use axum::http::header::{CACHE_CONTROL, HOST, PRAGMA, REFERRER_POLICY};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use plug_core::http::server::ClientRevocation;
use tokio_util::sync::CancellationToken;

const OPERATOR_OAUTH_CLIENTS_PATH: &str = "/_plug/oauth/clients";
const OPERATOR_OWNER_BOOTSTRAP_PATH: &str = "/_plug/oauth/owner/bootstrap";
const OPERATOR_OWNER_CREDENTIALS_PATH: &str = "/_plug/oauth/owner/credentials";
const OPERATOR_PROOF_PATH: &str = "/_plug/operator/proof";
const OPERATOR_CLIENT_NONCE_HEADER: &str = "x-plug-operator-client-nonce";
const OPERATOR_SERVER_NONCE_HEADER: &str = "x-plug-operator-server-nonce";
const OPERATOR_PROOF_HEADER: &str = "x-plug-operator-proof";
const OPERATOR_ALLOW_EMPTY_HEADER: &str = "x-plug-operator-allow-empty";
const OPERATOR_PROOF_LIFETIME: Duration = Duration::from_secs(10);
const MAX_OPERATOR_PROOFS: usize = 64;

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

impl LiveSessionTransportCounts {
    /// `19 local, 1 HTTP`. No downstream session reports itself as SSE today
    /// (a legacy GET SSE stream belongs to its HTTP session), so the SSE count
    /// shows only when it is non-zero.
    pub(crate) fn summary(&self) -> String {
        let mut parts = vec![
            format!("{} local", self.daemon_proxy),
            format!("{} HTTP", self.http),
        ];
        if self.sse > 0 {
            parts.push(format!("{} SSE", self.sse));
        }
        parts.join(", ")
    }
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
        ),
        scope,
    }
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
        ("GET", OPERATOR_OAUTH_CLIENTS_PATH)
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
        .downstream_oauth()
        .ok_or(StatusCode::NOT_FOUND)?;
    let bootstrap = manager
        .create_owner_bootstrap()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let enrollment_url = format!(
        "{}#bootstrap={bootstrap}",
        manager.url(plug_core::downstream_oauth::paths::OWNER_ENROLL)
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
        .downstream_oauth()
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(sensitive_json(manager.list_owner_credentials().await))
}

async fn operator_remove_owner_credential(
    State(state): State<Arc<OperatorHttpState>>,
    AxumPath(credential_id): AxumPath<String>,
    OriginalUri(original_uri): OriginalUri,
    headers: HeaderMap,
) -> StatusCode {
    let path = original_uri.path();
    let allow_empty = exact_header(&headers, OPERATOR_ALLOW_EMPTY_HEADER) == Some("true");
    if let Err(status) =
        operator_proof_authorized(&state, &headers, "DELETE", path, allow_empty).await
    {
        return status;
    }
    let Some(manager) = state.http_state.downstream_oauth() else {
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
        .downstream_oauth()
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(manager.list_clients().await))
}

async fn operator_revoke_oauth_client(
    State(state): State<Arc<OperatorHttpState>>,
    AxumPath(client_id): AxumPath<String>,
    OriginalUri(original_uri): OriginalUri,
    headers: HeaderMap,
) -> StatusCode {
    let path = original_uri.path();
    if let Err(status) = operator_proof_authorized(&state, &headers, "DELETE", path, false).await {
        return status;
    }
    match state.http_state.revoke_downstream_client(&client_id).await {
        ClientRevocation::Revoked => StatusCode::NO_CONTENT,
        ClientRevocation::UnknownClient | ClientRevocation::OauthDisabled => StatusCode::NOT_FOUND,
        ClientRevocation::SaveFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
    state: Arc<plug_core::http::server::HttpState>,
}

fn build_configured_http_runtime(
    config: &plug_core::config::Config,
    engine: &Arc<plug_core::engine::Engine>,
) -> anyhow::Result<ConfiguredHttpRuntime> {
    let sessions = Arc::new(plug_core::session::SessionStore::new(
        config.http.session_timeout_secs,
        config.http.max_sessions,
    ));
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
    if config.http.auth_mode == plug_core::config::DownstreamAuthMode::Oauth
        && downstream_oauth.is_none()
    {
        anyhow::bail!("http.public_base_url is required when http.auth_mode = \"oauth\"");
    }

    // Slack events are one feature among many: a secret that cannot be read
    // or state that cannot be opened leaves them off, and everything else on.
    let slack_events = config.http.slack_events.as_ref().and_then(|event_config| {
        let open = || -> anyhow::Result<_> {
            let oauth = downstream_oauth
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Slack events require OAuth"))?;
            let secret = plug_core::slack_events::credentials::SigningCredential::new(
                &event_config.team_id,
                &event_config.app_id,
            )?
            .load(&plug_core::slack_events::credentials::KeychainSigningSecrets)?;
            let key = plug_core::slack_events::state_key(event_config)?;
            plug_core::slack_events::SlackEvents::open(
                event_config.clone(),
                secret,
                &plug_core::paths::dir().join("slack-events").join(key),
                Arc::new(plug_core::slack_events::RuntimeAccess::new(
                    engine,
                    oauth,
                    event_config.clone(),
                )),
                Arc::new(plug_core::events::HttpsDelivery),
            )
        };
        let events = open()
            .inspect_err(|error| tracing::error!(%error, "Slack events unavailable"))
            .ok()?;
        events.spawn(engine.cancel_token().clone());
        Some(events)
    });

    // Opened whenever OAuth is on, so a watch added by a reload needs no restart.
    // A watch is a convenience: failing to open its state must not stop Plug.
    let watch_events = downstream_oauth.clone().and_then(|oauth| {
        plug_core::events::WatchEvents::open(
            &plug_core::paths::events_dir().join("watch"),
            Arc::new(plug_core::engine::RuntimeWatchAccess::new(engine, oauth)),
            Arc::new(plug_core::events::HttpsDelivery),
        )
        .inspect_err(|error| tracing::warn!(%error, "watch events unavailable"))
        .ok()
    });
    if let Some(events) = &watch_events {
        engine.set_watch_events(Arc::clone(events));
        events.spawn(engine.cancel_token().clone());
    }
    let event_sources: Vec<Arc<dyn plug_core::events::EventSource>> = slack_events
        .into_iter()
        .map(|events| events as Arc<dyn plug_core::events::EventSource>)
        .chain(watch_events.map(|events| events as Arc<dyn plug_core::events::EventSource>))
        .collect();

    let auth = match (downstream_oauth.clone(), auth_token) {
        (Some(manager), _) => plug_core::http::server::HttpAuth::Oauth(manager),
        (None, Some(token)) => plug_core::http::server::HttpAuth::Bearer(token),
        (None, None) => plug_core::http::server::HttpAuth::Open,
    };
    let http_state = Arc::new(
        plug_core::http::server::HttpState::new(
            tool_router.clone(),
            sessions,
            engine.cancel_token().clone(),
        )
        .with_auth(auth)
        .with_event_sources(event_sources)
        .with_sse_channel_capacity(config.http.sse_channel_capacity)
        .with_allowed_origins(
            config
                .http
                .allowed_origins
                .iter()
                .cloned()
                .map(Arc::<str>::from)
                .collect(),
        )
        .with_public_base_url(config.http.public_base_url.as_deref()),
    );

    Ok(ConfiguredHttpRuntime {
        router: build_runtime_router(
            Arc::clone(&http_state),
            operator_token,
            Arc::from(local_operator_authority(
                &config.http.bind_address,
                config.http.port,
            )),
        ),
        state: http_state,
    })
}

/// Every live downstream session, as the daemon reports them.
pub(crate) async fn fetch_live_sessions() -> (
    Vec<plug_core::ipc::IpcLiveSessionInfo>,
    plug_core::ipc::LiveSessionInventoryScope,
    LiveClientSupport,
) {
    match crate::ipc_client::request(plug_core::ipc::IpcRequest::ListLiveSessions).await {
        Ok(plug_core::ipc::IpcResponse::LiveSessions { sessions, scope }) => {
            (sessions, scope, LiveClientSupport::Supported)
        }
        Ok(response) if response.error_code() == Some(plug_core::ipc::IpcErrorCode::ParseError) => {
            (
                Vec::new(),
                plug_core::ipc::LiveSessionInventoryScope::Unavailable,
                LiveClientSupport::DaemonRestartRequired,
            )
        }
        _ => (
            Vec::new(),
            plug_core::ipc::LiveSessionInventoryScope::Unavailable,
            LiveClientSupport::Supported,
        ),
    }
}

pub(crate) async fn ensure_daemon_with_feedback(
    config_path: Option<&std::path::PathBuf>,
    announce: bool,
) -> anyhow::Result<bool> {
    let started = crate::service::ensure_started(config_path.map(|path| path.as_path())).await?;
    if started && announce {
        print_info_line("Started background service.");
    }
    Ok(started)
}

pub(crate) async fn cmd_start(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let started = crate::service::ensure_started(config_path.map(|path| path.as_path())).await?;

    if matches!(output, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "command": "start",
                "started": started,
                "running": crate::ipc_client::daemon_running().await,
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
    require_canonical_daemon_owner()?;
    serve_until(config_path, daemon::termination_signal()).await
}

/// Run the daemon until `stop` resolves or an operator asks it to shut down.
/// Either way it lets running calls drain, then the socket, the PID file and
/// the token are removed and HTTP shuts down gracefully before this returns.
async fn serve_until(
    config_path: Option<&std::path::PathBuf>,
    stop: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    // Claim daemon ownership before Engine::start() can read Keychain entries
    // or spawn upstream processes.
    let runtime_lock = daemon::acquire_runtime_lock()?;
    let config_path = config_path
        .cloned()
        .unwrap_or_else(plug_core::paths::default_config_path);
    let config = plug_core::config::load_config(Some(&config_path))?;
    preflight_http_bind(&config.http)?;
    let engine = plug_core::engine::Engine::new(config);

    // The IPC socket binds at once; local clients see servers arrive through
    // `list_changed`. Remote clients cannot be told the catalog grew, so HTTP
    // binds only after the whole catalog has started.
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<anyhow::Result<()>>();
    let startup = {
        let engine = engine.clone();
        let config_path = config_path.clone();
        tokio::spawn(async move {
            let started = engine.start().await;
            let ready = started.is_ok();
            let _ = started_tx.send(started);
            if ready {
                engine.pin_tool_blocks(&config_path).await;
            }
        })
    };
    let http_runtime = build_configured_http_runtime(&engine.config(), &engine)?;
    let cancel = engine.cancel_token().clone();
    drop(plug_core::watcher::spawn_config_watcher(
        engine.clone(),
        config_path.clone(),
        cancel.clone(),
        engine.tracker(),
    ));
    let http_config = engine.config().http.clone();
    let http_router = http_runtime.router;
    let http = async {
        let started = tokio::select! {
            started = started_rx => started,
            () = cancel.cancelled() => return Ok(()),
        };
        match started {
            Ok(result) => result?,
            Err(_) => anyhow::bail!("daemon startup ended without reporting a result"),
        }
        serve_router(http_router, &http_config, cancel.clone()).await
    };
    // launchd's record of who started this process does not change while
    // it runs, and reading it runs `launchctl`.
    let ownership = tokio::task::spawn_blocking(crate::service::ipc_ownership)
        .await
        .unwrap_or(plug_core::ipc::DaemonOwnershipMode::Unknown);
    let ipc = daemon::run_daemon_with_lock(
        engine.clone(),
        config_path,
        http_runtime.state,
        runtime_lock,
        ownership,
    );
    // Whichever side ends first ends the other through the cancel token, and
    // both are awaited, so each runs its own teardown.
    let (http_result, ipc_result, ()) = tokio::join!(
        async {
            let result = http.await;
            cancel.cancel();
            result
        },
        async {
            let result = ipc.await;
            cancel.cancel();
            result
        },
        daemon::shutdown_on(stop, cancel.clone(), engine.tool_router().clone()),
    );
    // A cold start still in flight owns upstreams that shutdown has to retire,
    // so give it a bounded chance to finish before anything is torn down.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), startup).await;
    engine.shutdown().await;
    ipc_result.and(http_result)
}

#[cfg(any(target_os = "macos", test))]
fn daemon_invocation_is_canonical(
    app_installed: bool,
    app_managed: bool,
    parent_is_launchd: bool,
    development: bool,
) -> bool {
    development || !app_installed || (app_managed && parent_is_launchd)
}

#[cfg(target_os = "macos")]
fn require_canonical_daemon_owner() -> anyhow::Result<()> {
    #[cfg(test)]
    return Ok(());

    #[cfg(not(test))]
    {
        let development = plug_core::paths::is_development();
        let app_installed = crate::install::resolve_verified_app()?.is_some();
        let app_managed =
            crate::service::inspect()?.ownership == crate::service::ServiceOwnership::AppManaged;
        let parent = std::process::Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &std::process::id().to_string()])
            .output()?;
        let parent_is_launchd =
            parent.status.success() && String::from_utf8_lossy(&parent.stdout).trim() == "1";
        anyhow::ensure!(
            daemon_invocation_is_canonical(
                app_installed,
                app_managed,
                parent_is_launchd,
                development,
            ),
            "Plug.app owns daemon startup; open Plug.app to start or repair the service"
        );
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
fn require_canonical_daemon_owner() -> anyhow::Result<()> {
    Ok(())
}

pub(crate) async fn cmd_daemon_stop() -> anyhow::Result<()> {
    match crate::ipc_client::operator(plug_core::ipc::OperatorRequest::Shutdown).await? {
        plug_core::ipc::IpcResponse::Ok => {
            print_success_line("Stopped the shared background service.")
        }
        plug_core::ipc::IpcResponse::Error { code, message } => {
            anyhow::bail!("{code}: {message}");
        }
        other => anyhow::bail!("unexpected daemon response: {other:?}"),
    }
    Ok(())
}

/// The one restart instruction every message gives, worded for whoever owns
/// the daemon. With Plug.app installed its Restart button pauses connected
/// clients across the swap; `plug stop` would drop them instead.
pub(crate) fn restart_advice() -> &'static str {
    restart_advice_for(matches!(
        crate::install::resolve_verified_app(),
        Ok(Some(_))
    ))
}

fn restart_advice_for(app_installed: bool) -> &'static str {
    if app_installed {
        "Restart Plug from Plug.app (Settings → Plug → Restart)"
    } else {
        "Run `plug stop`, then `plug start`"
    }
}

fn resolve_downstream_bearer_token(
    http: &plug_core::config::HttpConfig,
) -> anyhow::Result<Option<Arc<str>>> {
    if !http.requires_bearer() {
        return Ok(None);
    }
    let token_path = plug_core::auth::http_auth_token_path(http.port);
    let token = plug_core::auth::load_or_generate_token(&token_path)?;
    tracing::info!(mode = http.auth_mode.label(), "HTTP bearer auth enabled");
    Ok(Some(Arc::<str>::from(token.as_str())))
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
    let listener = std::net::TcpListener::bind(addr)
        .with_context(|| format!("failed to bind HTTP on {addr}"))?;
    tracing::info!(%addr, tls = http.tls_enabled(), "serving HTTP");
    serve_listener(listener, router, http, cancel).await
}

/// Serve `router` on a bound listener, over TLS when `http` configures it,
/// until `cancel` fires; then shut down gracefully.
async fn serve_listener(
    listener: std::net::TcpListener,
    router: Router,
    http: &plug_core::config::HttpConfig,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    listener.set_nonblocking(true)?;
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        cancel.cancelled().await;
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
    });
    let service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();

    match (&http.tls_cert_path, &http.tls_key_path) {
        (Some(cert_path), Some(key_path)) => {
            plug_core::tls::ensure_rustls_provider_installed();
            let tls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path).await?;
            axum_server::from_tcp_rustls(listener, tls_config)?
                .handle(handle)
                .serve(service)
                .await?;
        }
        _ => {
            axum_server::from_tcp(listener)?
                .handle(handle)
                .serve(service)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{run_daemon, set_test_runtime_paths};
    use std::path::PathBuf;
    use std::sync::Arc;

    use std::time::Duration;

    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use rcgen::generate_simple_self_signed;
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, RootCertStore};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::TlsConnector;
    use tower::util::ServiceExt;

    #[test]
    fn installed_app_rejects_foreign_daemon_spawners() {
        assert!(daemon_invocation_is_canonical(true, true, true, false));
        assert!(!daemon_invocation_is_canonical(true, true, false, false));
        assert!(!daemon_invocation_is_canonical(true, false, true, false));
        assert!(daemon_invocation_is_canonical(true, false, false, true));
        assert!(daemon_invocation_is_canonical(false, false, false, false));
    }

    #[test]
    fn transport_summary_shows_sse_only_when_something_reports_it() {
        let mut counts = LiveSessionTransportCounts {
            daemon_proxy: 19,
            http: 1,
            sse: 0,
        };
        assert_eq!(counts.summary(), "19 local, 1 HTTP");
        counts.sse = 2;
        assert_eq!(counts.summary(), "19 local, 1 HTTP, 2 SSE");
    }

    #[test]
    fn restart_advice_sends_app_users_to_the_app() {
        assert!(restart_advice_for(true).contains("Plug.app"));
        assert!(!restart_advice_for(true).contains("plug stop"));
        assert!(restart_advice_for(false).contains("`plug stop`, then `plug start`"));
    }

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

    fn runtime_path_test_lock() -> &'static tokio::sync::Mutex<()> {
        plug_core::paths::test::lock()
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
            bind_address: "127.0.0.1".to_string(),
            port: addr.port(),
            tls_cert_path: Some(cert_path),
            tls_key_path: Some(key_path),
            ..Default::default()
        };

        tokio::spawn({
            let cancel = cancel.clone();
            async move {
                let _ = serve_listener(listener, router, &http, cancel).await;
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
        let engine = plug_core::engine::Engine::new(plug_core::config::Config::default());
        engine.start().await.expect("engine start");
        let sessions = Arc::new(plug_core::session::SessionStore::new(1800, 100));
        let state = Arc::new(plug_core::http::server::HttpState::new(
            engine.tool_router().clone(),
            sessions,
            engine.cancel_token().clone(),
        ));
        let router = build_runtime_router(
            state,
            Arc::from("test-operator-token"),
            Arc::from("127.0.0.1:3282"),
        );

        let (addr, cancel, cert_der) = spawn_https_test_server(router)
            .await
            .expect("start https test server");

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
    fn live_inventory_availability_marks_missing_sources() {
        let complete = live_inventory_availability(
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete,
        );
        assert!(!complete.partial);
        assert!(complete.unavailable_sources.is_empty());

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
                adapter_version: Some("0.6.5".to_string()),
                host: None,
                grant: None,
                link_target: None,
                client_key: None,
                access_key: None,
                target: None,
                connected_secs: 10,
                last_activity_secs: None,
            },
            plug_core::ipc::IpcLiveSessionInfo {
                transport: plug_core::ipc::LiveSessionTransport::Http,
                client_id: None,
                session_id: "http-1".to_string(),
                client_type: plug_core::types::ClientType::ClaudeDesktop,
                client_info: None,
                adapter_version: None,
                host: None,
                grant: None,
                link_target: None,
                client_key: None,
                access_key: None,
                target: None,
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
        let _paths = set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = plug_core::engine::Engine::new(plug_core::config::Config::default());
        engine.start().await.expect("engine start");

        let daemon_engine = Arc::clone(&engine);
        let daemon_config_path = config_path.clone();
        let daemon_handle =
            tokio::spawn(async move { run_daemon(daemon_engine, daemon_config_path).await });

        tokio::time::sleep(Duration::from_millis(100)).await;
        if daemon_handle.is_finished() {
            let daemon_result = daemon_handle.await.expect("daemon task join");
            panic!("daemon exited before readiness: {daemon_result:?}");
        }

        tokio::time::timeout(Duration::from_secs(5), crate::ipc_client::wait_for_daemon())
            .await
            .expect("daemon readiness wait timed out");

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
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    /// A stop signal and an operator `Shutdown` end in the same teardown: when
    /// the daemon returns, its socket, PID file and token are gone and its
    /// HTTP port no longer accepts connections.
    #[tokio::test]
    async fn every_way_of_stopping_the_daemon_cleans_up() {
        let _guard = runtime_path_test_lock().lock().await;
        for via_operator in [false, true] {
            let temp = std::env::temp_dir().join(format!(
                "pst-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ));
            let _paths = set_test_runtime_paths(temp.join("r"), temp.join("s"));
            std::fs::create_dir_all(plug_core::paths::dir()).expect("create runtime dir");
            let mut config = plug_core::config::Config::default();
            let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("reserve HTTP port");
            let port = probe.local_addr().expect("HTTP address").port();
            drop(probe);
            config.http.port = port;
            let config_path = plug_core::paths::dir().join("config.toml");
            std::fs::write(
                &config_path,
                toml::to_string(&config).expect("serialize config"),
            )
            .expect("write config");

            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
            let daemon = tokio::spawn(async move {
                serve_until(Some(&config_path), async {
                    let _ = stop_rx.await;
                })
                .await
            });
            tokio::time::timeout(Duration::from_secs(5), crate::ipc_client::wait_for_daemon())
                .await
                .expect("daemon socket never came up");
            tokio::time::timeout(Duration::from_secs(5), async {
                while tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .is_err()
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("HTTP never came up");

            if via_operator {
                let response =
                    crate::ipc_client::operator(plug_core::ipc::OperatorRequest::Shutdown)
                        .await
                        .expect("send Shutdown");
                assert!(matches!(response, plug_core::ipc::IpcResponse::Ok));
            } else {
                stop_tx.send(()).expect("daemon is listening for the stop");
            }
            tokio::time::timeout(Duration::from_secs(15), daemon)
                .await
                .expect("daemon did not stop")
                .expect("daemon task join")
                .expect("daemon stops cleanly");

            assert!(
                !plug_core::paths::socket_path().exists(),
                "socket left behind"
            );
            assert!(
                !plug_core::paths::pid_path().exists(),
                "PID file left behind"
            );
            assert!(
                !plug_core::paths::token_path().exists(),
                "token left behind"
            );
            assert!(
                tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .is_err(),
                "HTTP still accepts connections"
            );
            let _ = std::fs::remove_dir_all(&temp);
        }
    }

    async fn owner_operator_test_router_with_authority(authority: Arc<str>) -> (Router, PathBuf) {
        let engine = plug_core::engine::Engine::new(plug_core::config::Config::default());
        engine.start().await.expect("engine start");
        let sessions = Arc::new(plug_core::session::SessionStore::new(1800, 100));
        let state_path = unique_temp_dir("owner-operator").join("oauth.json");
        let manager = plug_core::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            plug_core::downstream_oauth::DownstreamOauthConfig {
                public_base_url: "https://plug.example.com".to_string(),
                oauth_scopes: vec!["tools:read".to_string()],
            },
            state_path.clone(),
        )
        .expect("OAuth manager");
        let state = Arc::new(
            plug_core::http::server::HttpState::new(
                engine.tool_router().clone(),
                sessions,
                engine.cancel_token().clone(),
            )
            .with_auth(plug_core::http::server::HttpAuth::Oauth(manager)),
        );
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
        let client = plug_core::tls::http_client_builder()
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
    async fn operator_http_e2e_revokes_url_valued_metadata_client_and_its_token() {
        plug_core::tls::ensure_rustls_provider_installed();
        let state_path = unique_temp_dir("operator-cimd-revoke").join("oauth.json");
        let oauth_config = plug_core::downstream_oauth::DownstreamOauthConfig {
            public_base_url: "https://plug.example.com".to_string(),
            oauth_scopes: vec!["tools:read".to_string()],
        };
        let initial = plug_core::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            oauth_config.clone(),
            state_path.clone(),
        )
        .expect("OAuth manager");
        let registered = initial
            .register_client(
                plug_core::downstream_oauth::ClientRegistrationRequest {
                    redirect_uris: vec!["https://client.example/callback".to_string()],
                    client_name: Some("metadata client".to_string()),
                    token_endpoint_auth_method: Some("none".to_string()),
                    grant_types: Some(vec!["authorization_code".to_string()]),
                    response_types: Some(vec!["code".to_string()]),
                    scope: None,
                },
                "operator-cimd-revoke",
            )
            .await
            .expect("register fixture client");
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let consent = initial
            .begin_authorization(plug_core::downstream_oauth::AuthorizationRequest {
                response_type: "code",
                client_id: &registered.client_id,
                redirect_uri: &registered.redirect_uris[0],
                state: "operator-cimd-revoke-state",
                code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                code_challenge_method: "S256",
                scope: Some("tools:read"),
                resource: "https://plug.example.com/mcp",
            })
            .await
            .expect("begin authorization");
        let redirect = initial
            .decide_consent(&consent.consent_id, true)
            .await
            .expect("approve fixture consent");
        let code = reqwest::Url::parse(&redirect.location)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .unwrap();
        let tokens = initial
            .exchange_authorization_code(
                &registered.client_id,
                &code,
                &registered.redirect_uris[0],
                verifier,
                "https://plug.example.com/mcp",
            )
            .await
            .expect("issue fixture token");
        drop(initial);

        let cimd_client_id = "https://client.example/oauth/metadata.json";
        let persisted = std::fs::read_to_string(&state_path)
            .expect("read persisted OAuth fixture")
            .replace(&registered.client_id, cimd_client_id)
            .replace("dynamic_registration", "metadata_document");
        std::fs::write(&state_path, persisted).expect("write CIMD fixture");
        let manager = plug_core::downstream_oauth::DownstreamOauthManager::new_with_state_path(
            oauth_config,
            state_path.clone(),
        )
        .expect("reopen CIMD fixture");

        let engine = plug_core::engine::Engine::new(plug_core::config::Config::default());
        engine.start().await.expect("engine start");
        let sessions = Arc::new(plug_core::session::SessionStore::new(1800, 100));
        let state = Arc::new(
            plug_core::http::server::HttpState::new(
                engine.tool_router().clone(),
                sessions,
                engine.cancel_token().clone(),
            )
            .with_auth(plug_core::http::server::HttpAuth::Oauth(manager.clone())),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind operator listener");
        let addr = listener.local_addr().unwrap();
        let authority = addr.to_string();
        let app = build_runtime_router(
            state,
            Arc::from("operator-secret"),
            Arc::from(authority.as_str()),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = plug_core::tls::http_client_builder()
            .no_proxy()
            .build()
            .unwrap();
        let mut endpoint =
            reqwest::Url::parse(&format!("http://{addr}{OPERATOR_OAUTH_CLIENTS_PATH}")).unwrap();
        endpoint.path_segments_mut().unwrap().push(cimd_client_id);

        let response = send_authenticated_operator_request(
            &client,
            endpoint,
            &authority,
            "operator-secret",
            reqwest::Method::DELETE,
            false,
        )
        .await
        .expect("authenticated CIMD revocation");
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        assert!(
            manager
                .validate_access_token_for(&tokens.access_token, &manager.resource(),)
                .await
                .is_none()
        );

        server.abort();
        engine.shutdown().await;
        let _ = std::fs::remove_dir_all(state_path.parent().unwrap());
    }

    #[tokio::test]
    async fn operator_owner_boundary_rejects_a_missing_or_wrong_proof_and_non_loopback_host() {
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

        let wrong = proof_authenticated_request(
            &app,
            "POST",
            OPERATOR_OWNER_BOOTSTRAP_PATH,
            "wrong-token",
            false,
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let public_host = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(OPERATOR_OWNER_BOOTSTRAP_PATH)
                    .header("host", "plug.example.com")
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

    #[tokio::test]
    async fn oauth_revoke_204_blocks_request_validated_before_revocation_from_creating_task() {
        let mut config = plug_core::config::Config::default();
        config.servers.insert(
            "mock".to_string(),
            plug_core::test_helpers::mock_server("echo"),
        );
        let engine = plug_core::engine::Engine::new(config);
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
        let claims = manager
            .validate_access_token_for(&tokens.access_token, &manager.resource())
            .await
            .expect("token must validate before revocation");
        let principal = plug_core::types::PrincipalId::downstream_oauth(
            manager.base_url(),
            claims.client_id.clone(),
            claims.resource.clone(),
        );
        let owner = plug_core::tasks::TaskOwner::new(principal.owner_key());
        let context = plug_core::proxy::DownstreamCallContext::http(
            "validated-before-revoke",
            rmcp::model::RequestId::from(rmcp::model::NumberOrString::Number(1)),
            plug_core::types::ClientType::Unknown,
        )
        .with_trace_id(Arc::<str>::from("00000000000000000000000000000001"))
        .with_authorization(principal, claims.scopes)
        .with_principal_lifecycle(claims.principal_lifecycle);

        let sessions = Arc::new(plug_core::session::SessionStore::new(1800, 100));
        let http_state = Arc::new(
            plug_core::http::server::HttpState::new(
                tool_router.clone(),
                sessions,
                engine.cancel_token().clone(),
            )
            .with_auth(plug_core::http::server::HttpAuth::Oauth(manager)),
        );
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
}
