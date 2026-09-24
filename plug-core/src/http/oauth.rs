//! Downstream OAuth authorization-server and owner-passkey HTTP handlers.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{FormRejection, JsonRejection, QueryRejection};
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::server::HttpState;
use crate::downstream_oauth::{
    AuthorizationRequest, ClientRegistrationRequest, DownstreamOauthError, PublicKeyCredential,
    RegisterPublicKeyCredential, resource_scopes,
};

#[derive(Debug, serde::Deserialize)]
pub(super) struct OAuthAuthorizeParams {
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
pub(super) struct OAuthConsentChallengeRequest {
    consent_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub(super) enum OAuthConsentDecision {
    Approve {
        ceremony_id: String,
        credential: PublicKeyCredential,
    },
    Deny {
        consent_id: String,
        csrf_token: String,
    },
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct OAuthOwnerEnrollmentChallengeRequest {
    bootstrap: String,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct OAuthOwnerEnrollmentCompleteRequest {
    ceremony_id: String,
    credential: RegisterPublicKeyCredential,
}

pub(super) async fn get_oauth_authorization_server_metadata(
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

pub(super) async fn get_oauth_protected_resource_metadata(
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

pub(super) async fn oauth_register(
    State(state): State<Arc<HttpState>>,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
    request: Result<Json<ClientRegistrationRequest>, JsonRejection>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
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

pub(super) async fn oauth_authorize(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    params: Result<Query<OAuthAuthorizeParams>, QueryRejection>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
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

pub(super) async fn oauth_consent_javascript(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !public_browser_request_allowed(manager, &headers, false) {
        return oauth_forbidden_response();
    }
    super::oauth_ui::javascript_asset(super::oauth_ui::CONSENT_JAVASCRIPT)
}

pub(super) async fn oauth_enroll_javascript(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !public_browser_request_allowed(manager, &headers, false) {
        return oauth_forbidden_response();
    }
    super::oauth_ui::javascript_asset(super::oauth_ui::ENROLL_JAVASCRIPT)
}

pub(super) async fn oauth_owner_enroll(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !public_browser_request_allowed(manager, &headers, false) {
        return oauth_forbidden_response();
    }
    super::oauth_ui::enrollment_page()
}

pub(super) async fn oauth_consent_challenge(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    request: Result<Json<OAuthConsentChallengeRequest>, JsonRejection>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !public_browser_request_allowed(manager, &headers, true) {
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

pub(super) async fn oauth_consent_decision(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    decision: Result<Json<OAuthConsentDecision>, JsonRejection>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !public_browser_request_allowed(manager, &headers, true) {
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

pub(super) async fn oauth_owner_enroll_challenge(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    request: Result<Json<OAuthOwnerEnrollmentChallengeRequest>, JsonRejection>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !public_browser_request_allowed(manager, &headers, true) {
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

pub(super) async fn oauth_owner_enroll_complete(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    request: Result<Json<OAuthOwnerEnrollmentCompleteRequest>, JsonRejection>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !public_browser_request_allowed(manager, &headers, true) {
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

pub(super) async fn oauth_token(
    State(state): State<Arc<HttpState>>,
    params: Result<Form<HashMap<String, String>>, FormRejection>,
) -> Response {
    let Some(manager) = &state.downstream_oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Form(params) = match params {
        Ok(params) => params,
        Err(_) => return oauth_error_response(&DownstreamOauthError::InvalidAuthorizationRequest),
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
            oauth_credential_response(body)
        }
        Err(error) => oauth_error_response(&error),
    }
}

pub(super) fn oauth_credential_response<T: serde::Serialize>(payload: T) -> Response {
    let mut response = oauth_json_response(StatusCode::OK, payload);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

pub(super) fn oauth_error_response(error: &DownstreamOauthError) -> Response {
    let (status, code, description) = oauth_public_error(error);
    oauth_json_response(
        status,
        json!({
            "error": code,
            "error_description": description,
        }),
    )
}

pub(super) fn oauth_validated_callback_error_response(
    error: &DownstreamOauthError,
) -> Option<Response> {
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

pub(super) fn oauth_json_response<T: serde::Serialize>(status: StatusCode, payload: T) -> Response {
    let mut response = (status, Json(payload)).into_response();
    super::oauth_ui::apply_oauth_json_security_headers(&mut response);
    response
}

pub(super) fn oauth_public_error(
    error: &DownstreamOauthError,
) -> (StatusCode, &'static str, &'static str) {
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
            "Plug already has five owner passkeys. Remove one locally before enrolling another.",
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

pub(super) fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(html_media_range_is_acceptable)
}

pub(super) fn html_media_range_is_acceptable(value: &str) -> bool {
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

pub(super) fn oauth_authorization_error_response(error: &DownstreamOauthError) -> Response {
    let (status, code, description) = oauth_public_error(error);
    super::oauth_ui::authorization_error_page(status, code, description)
}

pub(super) fn oauth_forbidden_response() -> Response {
    let mut response = StatusCode::FORBIDDEN.into_response();
    super::oauth_ui::apply_oauth_json_security_headers(&mut response);
    response
}

pub(super) fn public_browser_request_allowed(
    manager: &crate::downstream_oauth::DownstreamOauthManager,
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
pub(super) fn registration_rate_key(peer: Option<std::net::IpAddr>, headers: &HeaderMap) -> String {
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

pub(super) fn oauth_authorization_error_redirect(
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

pub(super) fn protected_resource_metadata_url(base_url: &str) -> String {
    format!(
        "{}/.well-known/oauth-protected-resource",
        base_url.trim_end_matches('/')
    )
}
