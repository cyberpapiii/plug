//! The operator routes the daemon serves on its own HTTP port, and the
//! client the CLI reaches them with: owner sign-in setup and downstream OAuth
//! clients, each request proven with the operator token, which is never sent.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, OriginalUri, Path as AxumPath, State};
use axum::http::header::{CACHE_CONTROL, HOST, PRAGMA, REFERRER_POLICY};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use plug_core::http::server::ClientRevocation;
pub(crate) const OPERATOR_OAUTH_CLIENTS_PATH: &str = "/_plug/oauth/clients";
pub(crate) const OPERATOR_OWNER_BOOTSTRAP_PATH: &str = "/_plug/oauth/owner/bootstrap";
pub(crate) const OPERATOR_OWNER_CREDENTIALS_PATH: &str = "/_plug/oauth/owner/credentials";
pub(crate) const OPERATOR_PROOF_PATH: &str = "/_plug/operator/proof";
const OPERATOR_CLIENT_NONCE_HEADER: &str = "x-plug-operator-client-nonce";
const OPERATOR_SERVER_NONCE_HEADER: &str = "x-plug-operator-server-nonce";
const OPERATOR_PROOF_HEADER: &str = "x-plug-operator-proof";
const OPERATOR_ALLOW_EMPTY_HEADER: &str = "x-plug-operator-allow-empty";
const OPERATOR_PROOF_LIFETIME: Duration = Duration::from_secs(10);
const MAX_OPERATOR_PROOFS: usize = 64;

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

pub(crate) fn build_runtime_router(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn unique_temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "plug-operator-{label}-{}",
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
        let redirect =
            plug_core::downstream_oauth::test_owner::approve_consent(&initial, &consent.consent_id)
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
        let redirect =
            plug_core::downstream_oauth::test_owner::approve_consent(&manager, &consent.consent_id)
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
