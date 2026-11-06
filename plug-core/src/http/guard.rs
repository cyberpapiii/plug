use super::error::HttpError;
use super::identity::*;
use super::oauth::protected_resource_metadata_url;
use super::server::*;
use crate::downstream_oauth::{AccessTokenValidation, resource_scopes};
use axum::extract::Request;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

/// Check if a request's bearer token is valid against the expected token.
pub(super) fn check_bearer_token(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|token| crate::auth::verify_auth_token(token, expected))
}

/// Settle who the caller is under the listener's [`HttpAuth`] and record it
/// on the request as an [`AuthStatus`], or refuse the request.
pub(super) async fn validate_bearer_auth(
    State(state): State<Arc<HttpState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, HttpError> {
    let auth_status = match &state.auth {
        HttpAuth::Open => AuthStatus::NoAuthRequired,
        HttpAuth::Bearer(expected) => {
            if !check_bearer_token(req.headers(), expected) {
                tracing::warn!("bearer auth failed from downstream client");
                return Err(HttpError::Unauthorized);
            }
            AuthStatus::Authenticated(None)
        }
        HttpAuth::Oauth(manager) => {
            let token = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "));
            let claims = match token {
                Some(token) => match manager
                    .validate_access_token_for(token, &manager.resource())
                    .await
                {
                    AccessTokenValidation::Valid(claims) => Some(claims),
                    AccessTokenValidation::Invalid => None,
                },
                None => None,
            };
            let Some(claims) = claims else {
                let advertised_scopes = resource_scopes(&manager.config.oauth_scopes);
                return Err(HttpError::UnauthorizedWithMetadata {
                    metadata_url: protected_resource_metadata_url(manager),
                    scope: (!advertised_scopes.is_empty()).then(|| advertised_scopes.join(" ")),
                });
            };
            AuthStatus::Authenticated(Some(claims))
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
pub(super) async fn validate_origin(
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

pub(super) fn validate_modern_host(
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

pub(super) fn validate_modern_origin(
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

/// Extract the host from an Origin header value.
///
/// Origin format is `scheme://host[:port]`. We parse manually to avoid
/// pulling in the `url` crate dependency for this single use case.
pub(super) fn extract_origin_host(origin: &str) -> Option<&str> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_support::*;

    use axum::body::Body;
    use http::Request as HttpRequest;

    use tower::ServiceExt;

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
    async fn oauth_mode_requires_access_token_for_mcp_requests() {
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
}
