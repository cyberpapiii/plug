//! Downstream OAuth authorization-server and owner-passkey HTTP handlers.

use std::collections::HashMap;

use axum::Json;
use axum::extract::rejection::{FormRejection, JsonRejection, QueryRejection};
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::downstream_oauth::{
    AuthorizationRequest, ClientRegistrationRequest, DownstreamOauthError, DownstreamOauthManager,
    paths, resource_scopes,
};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use passkey_auth::{AuthenticationResponse, RegistrationResponse};

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
struct OAuthConsentChallengeRequest {
    consent_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
enum OAuthConsentDecision {
    Approve {
        ceremony_id: String,
        credential: AuthenticationResponse,
    },
    Deny {
        consent_id: String,
        csrf_token: String,
    },
}

#[derive(Debug, serde::Deserialize)]
struct OAuthOwnerEnrollmentChallengeRequest {
    bootstrap: String,
}

#[derive(Debug, serde::Deserialize)]
struct OAuthOwnerEnrollmentCompleteRequest {
    ceremony_id: String,
    credential: RegistrationResponse,
}

async fn get_oauth_authorization_server_metadata(
    State(manager): State<DownstreamOauthManager>,
) -> impl IntoResponse {
    let response = Json(json!({
        "issuer": manager.base_url(),
        "authorization_endpoint": manager.url(paths::AUTHORIZE),
        "token_endpoint": manager.url(paths::TOKEN),
        "registration_endpoint": manager.url(paths::REGISTER),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": manager.scopes(),
        "client_id_metadata_document_supported": true,
    }));
    (StatusCode::OK, response).into_response()
}

async fn get_oauth_protected_resource_metadata(
    State(manager): State<DownstreamOauthManager>,
) -> impl IntoResponse {
    let response = Json(json!({
        "resource": manager.resource(),
        "authorization_servers": [manager.base_url()],
        "scopes_supported": resource_scopes(manager.scopes()),
        "bearer_methods_supported": ["header"],
    }));
    (StatusCode::OK, response).into_response()
}

async fn oauth_register(
    State(manager): State<DownstreamOauthManager>,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
    request: Result<Json<ClientRegistrationRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(request) => request,
        Err(_) => return oauth_error_response(&DownstreamOauthError::InvalidClientMetadata),
    };
    let rate_key = registration_rate_key(
        peer.map(|axum::Extension(axum::extract::ConnectInfo(address))| address.ip()),
        &headers,
    );
    match manager.register_client(request, &rate_key).await {
        Ok(registration) => (StatusCode::CREATED, Json(registration)).into_response(),
        Err(error) => oauth_error_response(&error),
    }
}

async fn oauth_authorize(
    State(manager): State<DownstreamOauthManager>,
    headers: HeaderMap,
    params: Result<Query<OAuthAuthorizeParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(params) => params,
        Err(_) if accepts_html(&headers) => {
            return oauth_authorization_error_response(
                &DownstreamOauthError::InvalidAuthorizationRequest,
            );
        }
        Err(_) => {
            return oauth_error_response(&DownstreamOauthError::InvalidAuthorizationRequest);
        }
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
        Ok(consent) => super::oauth_ui::consent_page(&consent, manager.owner_enrolled().await),
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

async fn oauth_consent_javascript(
    State(manager): State<DownstreamOauthManager>,
    headers: HeaderMap,
) -> Response {
    browser_asset(
        &manager,
        &headers,
        super::oauth_ui::CONSENT_JAVASCRIPT,
        super::oauth_ui::JAVASCRIPT_TYPE,
    )
}

async fn oauth_enroll_javascript(
    State(manager): State<DownstreamOauthManager>,
    headers: HeaderMap,
) -> Response {
    browser_asset(
        &manager,
        &headers,
        super::oauth_ui::ENROLL_JAVASCRIPT,
        super::oauth_ui::JAVASCRIPT_TYPE,
    )
}

async fn oauth_stylesheet(
    State(manager): State<DownstreamOauthManager>,
    headers: HeaderMap,
) -> Response {
    browser_asset(
        &manager,
        &headers,
        super::oauth_ui::STYLESHEET,
        super::oauth_ui::STYLESHEET_TYPE,
    )
}

async fn oauth_icon(State(manager): State<DownstreamOauthManager>, headers: HeaderMap) -> Response {
    browser_asset(
        &manager,
        &headers,
        super::oauth_ui::ICON,
        super::oauth_ui::ICON_TYPE,
    )
}

/// A file the sign-in pages load, served only where those pages are.
fn browser_asset(
    manager: &DownstreamOauthManager,
    headers: &HeaderMap,
    source: &'static str,
    content_type: &'static str,
) -> Response {
    if !public_browser_request_allowed(manager, headers, false) {
        return oauth_forbidden_response();
    }
    super::oauth_ui::asset(source, content_type)
}

async fn oauth_owner_enroll(
    State(manager): State<DownstreamOauthManager>,
    headers: HeaderMap,
) -> Response {
    if !public_browser_request_allowed(&manager, &headers, false) {
        return oauth_forbidden_response();
    }
    super::oauth_ui::enrollment_page()
}

async fn oauth_consent_challenge(
    State(manager): State<DownstreamOauthManager>,
    headers: HeaderMap,
    request: Result<Json<OAuthConsentChallengeRequest>, JsonRejection>,
) -> Response {
    if !public_browser_request_allowed(&manager, &headers, true) {
        return oauth_forbidden_response();
    }
    let Json(request) = match request {
        Ok(request) => request,
        Err(_) => return oauth_error_response(&DownstreamOauthError::InvalidAuthorizationRequest),
    };
    match manager.start_owner_approval(&request.consent_id).await {
        Ok(challenge) => oauth_json_response(StatusCode::OK, challenge),
        Err(error) => oauth_validated_callback_error_response(&error)
            .unwrap_or_else(|| oauth_error_response(&error)),
    }
}

async fn oauth_consent_decision(
    State(manager): State<DownstreamOauthManager>,
    headers: HeaderMap,
    decision: Result<Json<OAuthConsentDecision>, JsonRejection>,
) -> Response {
    if !public_browser_request_allowed(&manager, &headers, true) {
        return oauth_forbidden_response();
    }
    let Json(decision) = match decision {
        Ok(decision) => decision,
        Err(_) => return oauth_error_response(&DownstreamOauthError::InvalidAuthorizationRequest),
    };
    let result = match decision {
        OAuthConsentDecision::Approve {
            ceremony_id,
            credential,
        } => {
            manager
                .finish_owner_approval(&ceremony_id, credential)
                .await
        }
        OAuthConsentDecision::Deny {
            consent_id,
            csrf_token,
        } => manager.deny_consent(&consent_id, &csrf_token).await,
    };
    match result {
        Ok(redirect) => {
            oauth_json_response(StatusCode::OK, json!({ "redirect_uri": redirect.location }))
        }
        Err(error) => oauth_validated_callback_error_response(&error)
            .unwrap_or_else(|| oauth_error_response(&error)),
    }
}

async fn oauth_owner_enroll_challenge(
    State(manager): State<DownstreamOauthManager>,
    headers: HeaderMap,
    request: Result<Json<OAuthOwnerEnrollmentChallengeRequest>, JsonRejection>,
) -> Response {
    if !public_browser_request_allowed(&manager, &headers, true) {
        return oauth_forbidden_response();
    }
    let Json(request) = match request {
        Ok(request) => request,
        Err(_) => return oauth_error_response(&DownstreamOauthError::InvalidOwnerBootstrap),
    };
    match manager.start_owner_registration(&request.bootstrap).await {
        Ok(challenge) => oauth_json_response(StatusCode::OK, challenge),
        Err(error) => oauth_error_response(&error),
    }
}

async fn oauth_owner_enroll_complete(
    State(manager): State<DownstreamOauthManager>,
    headers: HeaderMap,
    request: Result<Json<OAuthOwnerEnrollmentCompleteRequest>, JsonRejection>,
) -> Response {
    if !public_browser_request_allowed(&manager, &headers, true) {
        return oauth_forbidden_response();
    }
    let Json(request) = match request {
        Ok(request) => request,
        Err(_) => return oauth_error_response(&DownstreamOauthError::InvalidOwnerAssertion),
    };
    match manager
        .finish_owner_registration(&request.ceremony_id, request.credential)
        .await
    {
        Ok(credential) => oauth_json_response(StatusCode::OK, credential),
        Err(error) => oauth_error_response(&error),
    }
}

async fn oauth_token(
    State(manager): State<DownstreamOauthManager>,
    headers: HeaderMap,
    params: Result<Form<HashMap<String, String>>, FormRejection>,
) -> Response {
    let Form(params) = match params {
        Ok(params) => params,
        Err(_) => return oauth_error_response(&DownstreamOauthError::InvalidAuthorizationRequest),
    };
    if params.contains_key("client_secret") {
        return oauth_error_response(&DownstreamOauthError::UnsupportedClientAuthMethod);
    }
    // This endpoint advertises only public-client authentication. Never ignore
    // an assertion or an Authorization header and treat it as verified.
    if params.contains_key("client_assertion")
        || params.contains_key("client_assertion_type")
        || headers.contains_key(header::AUTHORIZATION)
    {
        return oauth_error_response(&DownstreamOauthError::InvalidClient);
    }
    let Some(client_id) = params.get("client_id") else {
        return oauth_error_response(&DownstreamOauthError::InvalidClient);
    };
    // A refresh token is already bound to this resource, and a client that
    // renews before it has read the metadata again leaves the field out.
    let renewing = params.get("grant_type").map(String::as_str) == Some("refresh_token");
    let own_resource = manager.resource().to_string();
    let Some(resource) = params.get("resource").or(renewing.then_some(&own_resource)) else {
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
            oauth_credential_response(body)
        }
        Err(error) => oauth_error_response(&error),
    }
}

fn oauth_credential_response<T: serde::Serialize>(payload: T) -> Response {
    let mut response = oauth_json_response(StatusCode::OK, payload);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn oauth_error_response(error: &DownstreamOauthError) -> Response {
    let (status, code, description) = oauth_public_error(error);
    oauth_json_response(
        status,
        json!({
            "error": code,
            "error_description": description,
        }),
    )
}

fn oauth_validated_callback_error_response(error: &DownstreamOauthError) -> Option<Response> {
    let DownstreamOauthError::AuthorizationExpired(callback) = error else {
        return None;
    };
    let location =
        oauth_authorization_error_redirect(&callback.redirect_uri, &callback.state, error);
    Some(oauth_json_response(
        StatusCode::OK,
        json!({ "redirect_uri": location }),
    ))
}

fn oauth_json_response<T: serde::Serialize>(status: StatusCode, payload: T) -> Response {
    let mut response = (status, Json(payload)).into_response();
    super::oauth_ui::apply_oauth_security_headers(&mut response);
    response
}

static OWNER_CREDENTIAL_LIMIT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "Plug already has {} owner passkeys. Remove one locally before enrolling another.",
        crate::downstream_oauth::owner::MAX_OWNER_CREDENTIALS
    )
});

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
            "There were too many authorization attempts. Wait a moment, then try connecting again.",
        ),
        DownstreamOauthError::RegistrationQuotaExceeded => (
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "The client registration limit was reached. Remove an unused connection, then try connecting again.",
        ),
        DownstreamOauthError::OwnerNotEnrolled => (
            StatusCode::BAD_REQUEST,
            "owner_not_enrolled",
            "Finish Plug owner setup on the Mac running Plug.",
        ),
        DownstreamOauthError::InvalidOwnerBootstrap => (
            StatusCode::BAD_REQUEST,
            "invalid_owner_bootstrap",
            "This owner enrollment link is invalid, expired, or already used. On the Mac running Plug, run `plug auth owner enroll` again.",
        ),
        DownstreamOauthError::OwnerChallengeExpired => (
            StatusCode::BAD_REQUEST,
            "owner_challenge_expired",
            "Approval expired. Select Allow again.",
        ),
        DownstreamOauthError::InvalidOwnerAssertion => (
            StatusCode::BAD_REQUEST,
            "owner_verification_failed",
            "Passkey verification failed. No access was granted.",
        ),
        DownstreamOauthError::OwnerCredentialLimit => (
            StatusCode::BAD_REQUEST,
            "owner_credential_limit",
            OWNER_CREDENTIAL_LIMIT.as_str(),
        ),
        DownstreamOauthError::OwnerCredentialNotFound => (
            StatusCode::BAD_REQUEST,
            "owner_credential_not_found",
            "That owner passkey is no longer enrolled. Try another owner passkey.",
        ),
        DownstreamOauthError::InvalidResource
        | DownstreamOauthError::InvalidAuthorizationRequest
        | DownstreamOauthError::MetadataFetch => (
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The authorization request could not be completed. Try connecting again from your MCP client.",
        ),
        DownstreamOauthError::AuthorizationExpired(_) => (
            StatusCode::BAD_REQUEST,
            "authorization_expired",
            "This connection request expired. Return to your MCP client and select Connect again.",
        ),
        DownstreamOauthError::Persistence(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Plug could not save this authorization. No access was granted.",
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
    super::oauth_ui::authorization_error_page(status, code, description)
}

fn oauth_forbidden_response() -> Response {
    let mut response = StatusCode::FORBIDDEN.into_response();
    super::oauth_ui::apply_oauth_security_headers(&mut response);
    response
}

fn public_browser_request_allowed(
    manager: &DownstreamOauthManager,
    headers: &HeaderMap,
    require_origin: bool,
) -> bool {
    if manager.durability_degraded() {
        return false;
    }
    let Ok(base_url) = url::Url::parse(manager.base_url()) else {
        return false;
    };
    let Some(host) = base_url.host_str() else {
        return false;
    };
    let expected_host = match base_url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let mut hosts = headers.get_all(header::HOST).iter();
    if hosts.next().and_then(|value| value.to_str().ok()) != Some(expected_host.as_str())
        || hosts.next().is_some()
    {
        return false;
    }
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let origin = origins.next().and_then(|value| value.to_str().ok());
    if origins.next().is_some() {
        return false;
    }
    if !require_origin && origin.is_none() {
        return true;
    }
    origin == Some(base_url.origin().ascii_serialization().as_str())
}

/// Bucket a registration attempt by caller.
///
/// The forwarding headers are attacker-controlled unless something trustworthy
/// wrote them, so they are consulted only when the request arrived from a
/// same-host reverse proxy — a loopback peer. That is the deployment plug
/// actually has: `cloudflared` runs beside the daemon and connects over
/// loopback. A caller reaching the listener directly is bucketed by the address
/// it connected from, which it cannot forge.
///
/// Within a trusted hop the two headers are read differently on purpose.
/// `cf-connecting-ip` is written by the edge and overwrites whatever the client
/// sent. `x-forwarded-for` is *appended* to, so the leftmost entry is the value
/// the client supplied and the rightmost is the one the nearest proxy added;
/// only the rightmost is worth anything.
fn registration_rate_key(peer: Option<std::net::IpAddr>, headers: &HeaderMap) -> String {
    let Some(peer) = peer else {
        // No connection info means no way to tell a proxy from a client, so
        // neither source is trustworthy. One shared bucket is a worse rate
        // limit than a per-caller one, and a safer one.
        return "unknown-peer".to_string();
    };
    if !peer.is_loopback() {
        return peer.to_string();
    }
    let forwarded = headers
        .get("cf-connecting-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .or_else(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.rsplit(',').next())
                .map(str::trim)
        })
        .and_then(|value| value.parse::<std::net::IpAddr>().ok());
    match forwarded {
        Some(address) => address.to_string(),
        None => peer.to_string(),
    }
}

fn oauth_authorization_error_redirect(
    redirect_uri: &str,
    state: &str,
    error: &DownstreamOauthError,
) -> String {
    let (_, code, description) = oauth_public_error(error);
    crate::downstream_oauth::redirect_with_params(
        redirect_uri,
        &[
            ("error", code),
            ("error_description", description),
            ("state", state),
        ],
    )
}

/// Where a client learns how to sign in to this resource.
pub(super) fn protected_resource_metadata_url(manager: &DownstreamOauthManager) -> String {
    manager.url(paths::PROTECTED_RESOURCE_METADATA)
}

/// The authorization server and sign-in pages. Served only when the
/// listener signs clients in with OAuth.
pub(super) fn router(manager: DownstreamOauthManager) -> Router {
    Router::new()
        .route(
            paths::AUTHORIZATION_SERVER_METADATA,
            get(get_oauth_authorization_server_metadata),
        )
        .route(
            paths::PROTECTED_RESOURCE_METADATA,
            get(get_oauth_protected_resource_metadata),
        )
        .route(
            paths::PROTECTED_RESOURCE_METADATA_FOR_MCP,
            get(get_oauth_protected_resource_metadata),
        )
        .route(paths::REGISTER, post(oauth_register))
        .route(paths::AUTHORIZE, get(oauth_authorize))
        .route(paths::CONSENT_SCRIPT, get(oauth_consent_javascript))
        .route(paths::CONSENT_CHALLENGE, post(oauth_consent_challenge))
        .route(paths::CONSENT_DECISION, post(oauth_consent_decision))
        .route(paths::OWNER_ENROLL, get(oauth_owner_enroll))
        .route(paths::ENROLL_SCRIPT, get(oauth_enroll_javascript))
        .route(paths::STYLESHEET, get(oauth_stylesheet))
        .route(paths::ICON, get(oauth_icon))
        .route(
            paths::OWNER_ENROLL_CHALLENGE,
            post(oauth_owner_enroll_challenge),
        )
        .route(
            paths::OWNER_ENROLL_COMPLETE,
            post(oauth_owner_enroll_complete),
        )
        .route(paths::TOKEN, post(oauth_token))
        .route(paths::ASSUMED_TOKEN, post(oauth_token))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(manager)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downstream_oauth::test_owner::BrowserAuthenticator;
    use crate::downstream_oauth::{
        AuthorizationRequest, ClientRegistrationRequest, DownstreamOauthError,
    };
    use crate::http::test_support::*;

    use axum::body::Body;
    use http::Request as HttpRequest;

    use std::time::Duration;
    use tower::ServiceExt;

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

    #[tokio::test]
    async fn oauth_routes_are_not_served_without_oauth() {
        for state in [test_state(), test_state_with_auth("token")] {
            let app = build_router(state);
            for path in [
                paths::AUTHORIZATION_SERVER_METADATA,
                paths::PROTECTED_RESOURCE_METADATA,
                paths::AUTHORIZE,
                paths::CONSENT_SCRIPT,
            ] {
                let response = app
                    .clone()
                    .oneshot(
                        HttpRequest::builder()
                            .uri(path)
                            .header(header::HOST, "localhost:3282")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            }
        }
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
            .with_auth(crate::http::server::HttpAuth::Oauth(
                isolated_oauth_manager(vec!["tools:read".to_string()]),
            )),
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
            .with_auth(crate::http::server::HttpAuth::Oauth(
                isolated_oauth_manager(vec!["tools:read".to_string()]),
            )),
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
            .with_auth(crate::http::server::HttpAuth::Oauth(
                isolated_oauth_manager(vec![
                    "tools:read".to_string(),
                    "offline_access".to_string(),
                ]),
            )),
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
        assert_eq!(legacy.status(), StatusCode::NOT_FOUND);
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
        let redirect =
            crate::downstream_oauth::test_owner::approve_consent(&manager, &consent.consent_id)
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
            .with_auth(crate::http::server::HttpAuth::Oauth(
                isolated_oauth_manager(vec![
                    "tools:read".to_string(),
                    "offline_access".to_string(),
                ]),
            )),
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
    async fn dcr_oauth_lifecycle_acceptance_matrix() {
        let state_path = std::env::temp_dir().join(format!(
            "plug-oauth-lifecycle-matrix-{}.json",
            uuid::Uuid::new_v4()
        ));
        let oauth_config = crate::downstream_oauth::DownstreamOauthConfig {
            public_base_url: "https://plug.example.com".to_string(),
            oauth_scopes: vec!["tools:read".to_string()],
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
}
