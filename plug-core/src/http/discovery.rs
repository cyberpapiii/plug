use super::server::{HttpAuth, HttpState};
use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use serde_json::json;
use std::sync::Arc;

/// The server card, at its current address and the one older clients read.
/// Any page may read it, so it sits outside the Origin check.
pub(super) fn discovery_router(state: Arc<HttpState>) -> Router {
    Router::new()
        .route("/.well-known/mcp.json", get(get_server_card))
        .route("/.well-known/mcp-server-card", get(get_server_card))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
}

pub(super) async fn get_server_card(State(state): State<Arc<HttpState>>) -> impl IntoResponse {
    let mut remote = json!({
        "type": "streamable-http",
        "url": "/mcp",
        "supportedProtocolVersions": crate::protocol::supported_downstream_protocol_versions(
            state.router.modern_downstream_enabled()
        ),
    });
    if !matches!(state.auth, HttpAuth::Open) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::test_support::*;

    use axum::body::Body;
    use http::Request as HttpRequest;

    use tower::ServiceExt;

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

    /// The card lists the revisions `server/discover` lists.
    #[tokio::test]
    async fn server_card_lists_the_modern_revision_once_it_is_served() {
        let state = test_state();
        state.router.set_modern_downstream_enabled(true);
        let req = HttpRequest::builder()
            .method("GET")
            .uri("/.well-known/mcp.json")
            .body(Body::empty())
            .unwrap();

        let resp = build_router(state).oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["remotes"][0]["supportedProtocolVersions"],
            serde_json::json!([
                SUPPORTED_PROTOCOL_VERSION,
                crate::protocol::MODERN_PROTOCOL_VERSION
            ])
        );
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
            .with_auth(crate::http::server::HttpAuth::Oauth(
                isolated_oauth_manager(vec!["tools:read".to_string()]),
            )),
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
