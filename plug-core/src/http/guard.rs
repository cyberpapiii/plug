use super::error::HttpError;
use super::identity::AuthStatus;
use super::oauth::protected_resource_metadata_url;
use super::server::{HttpAuth, HttpState};
use crate::downstream_oauth::{AccessTokenValidation, resource_scopes};
use axum::extract::Request;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

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

/// Refuse a browser page that is not allowed to drive `/mcp`, against DNS
/// rebinding. A caller that proved who it is skips this: the credential, not
/// the page, is what is trusted. The modern era checks Origin for every
/// caller in [`validate_modern_request`].
pub(super) async fn validate_origin(
    State(state): State<Arc<HttpState>>,
    req: Request,
    next: Next,
) -> Result<Response, HttpError> {
    if !matches!(
        req.extensions().get::<AuthStatus>(),
        Some(AuthStatus::Authenticated(_))
    ) {
        check_origin(req.headers(), &state)?;
    }
    Ok(next.run(req).await)
}

/// A modern request names a Host this listener answers to, and comes from no
/// page or an allowed one.
pub(super) fn validate_modern_request(
    headers: &HeaderMap,
    state: &HttpState,
) -> Result<(), HttpError> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            HttpError::BadRequest("modern requests require a valid Host header".into())
        })?;
    let host = host_of_authority(host)
        .ok_or_else(|| HttpError::BadRequest("modern request Host is malformed".into()))?;
    let answers_to = crate::config::is_loopback_host(&host)
        || state.public_host.as_deref() == Some(host.as_str())
        || state
            .allowed_origins
            .iter()
            .any(|origin| host_of_url(origin).as_deref() == Some(host.as_str()));
    if !answers_to {
        return Err(HttpError::BadRequest(
            "modern request Host is not allowed".into(),
        ));
    }
    check_origin(headers, state)
}

/// No Origin is a client that is not a browser. Otherwise the page must be
/// one the config allows or one served from this Mac. `null` is what a
/// sandboxed or local-file page sends, so it is never allowed.
fn check_origin(headers: &HeaderMap, state: &HttpState) -> Result<(), HttpError> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let origin = origin.to_str().map_err(|_| HttpError::InvalidOrigin)?;
    let allowed = origin != "null"
        && (state
            .allowed_origins
            .iter()
            .any(|allowed| allowed.as_ref() == origin)
            || host_of_url(origin).is_some_and(|host| crate::config::is_loopback_host(&host)));
    if allowed {
        Ok(())
    } else {
        Err(HttpError::InvalidOrigin)
    }
}

/// The host of a URL such as an Origin, lowercased, with an IPv6 address in
/// brackets.
pub(super) fn host_of_url(value: &str) -> Option<String> {
    url::Url::parse(value)
        .ok()?
        .host_str()
        .filter(|host| !host.is_empty())
        .map(str::to_string)
}

/// The host of a Host header value, `name[:port]`.
fn host_of_authority(authority: &str) -> Option<String> {
    if authority.is_empty() || authority.contains(['/', '?', '#', '@', '\\']) {
        return None;
    }
    host_of_url(&format!("http://{authority}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_support::*;

    use axum::body::Body;
    use http::Request as HttpRequest;

    use tower::ServiceExt;

    fn headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
        }
        headers
    }

    #[tokio::test]
    async fn a_modern_request_must_name_a_host_this_listener_answers_to() {
        let state = test_state();
        for host in [
            "localhost:3282",
            "LOCALHOST",
            "127.0.0.1:3282",
            "[::1]:3282",
        ] {
            assert!(
                validate_modern_request(&headers(host, None), &state).is_ok(),
                "host {host}"
            );
        }
        for host in [
            "localhost.evil.example:3282",
            "192.0.2.1:3282",
            "::1",
            "localhost#.evil.example",
            "evil.example@localhost",
        ] {
            assert!(
                validate_modern_request(&headers(host, None), &state).is_err(),
                "host {host} must not become loopback"
            );
        }
    }

    #[tokio::test]
    async fn the_public_host_is_answered_without_trusting_pages_served_from_it() {
        let sm = Arc::new(crate::server::ServerManager::new());
        let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
        let state = HttpState::new(
            router,
            Arc::new(crate::session::SessionStore::new(1800, 100)),
            CancellationToken::new(),
        )
        .with_public_base_url(Some("https://Plug-Tunnel.example.com/"));
        let host = "plug-tunnel.example.com";
        assert!(validate_modern_request(&headers(host, None), &state).is_ok());
        for origin in [
            "https://unrelated.example.com",
            "https://plug-tunnel.example.com",
            "null",
        ] {
            assert!(
                validate_modern_request(&headers(host, Some(origin)), &state).is_err(),
                "{origin}"
            );
        }
        assert!(
            validate_modern_request(&headers(host, Some("http://localhost:3000")), &state).is_ok()
        );
        assert!(validate_modern_request(&headers("other.example.com", None), &state).is_err());
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
