//! Builders the HTTP tests share.

#![allow(unused_imports)]

pub(super) use crate::downstream_identity::HttpCredential;
pub(super) use crate::downstream_oauth::{AccessTokenClaims, resource_scopes};
pub(super) use crate::events::{EVENT_SCOPE, EventSource};
pub(super) use crate::http::discovery::*;
pub(super) use crate::http::error::HttpError;
pub(super) use crate::http::events::*;
pub(super) use crate::http::fanout::*;
pub(super) use crate::http::guard::*;
pub(super) use crate::http::identity::*;
pub(super) use crate::http::response::*;
pub(super) use crate::http::reverse::*;
pub(super) use crate::http::server::*;
pub(super) use crate::http::session_binding::*;
pub(super) use crate::http::sse::sse_stream_with_heartbeat;
pub(super) use crate::mcp_http_headers::{
    HEADER_MISMATCH_CODE, HeaderMismatch, inject_trace_context, validate_mirrored_headers,
    validate_required_mirrored_headers,
};
pub(super) use crate::notifications::{NotificationTarget, ProtocolNotification};
pub(super) use crate::protocol::SUPPORTED_PROTOCOL_VERSION;
pub(super) use crate::proxy::{DownstreamBridge, DownstreamCallContext, ToolRouter};
pub(super) use crate::session::{SessionEnd, SessionStore, SseMessage, SseReplayKey};
pub(super) use axum::Json;
pub(super) use axum::body::Bytes;
pub(super) use axum::extract::State;
pub(super) use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
pub(super) use axum::middleware::{self, Next};
pub(super) use axum::response::{IntoResponse, Response};
pub(super) use axum::routing::{get, post};
pub(super) use axum::{Router, extract::DefaultBodyLimit, extract::Request};
pub(super) use dashmap::DashMap;
pub(super) use rmcp::ErrorData as McpError;
pub(super) use rmcp::model::*;
pub(super) use serde_json::json;
pub(super) use std::convert::Infallible;
pub(super) use std::future::Future;
pub(super) use std::pin::Pin;
pub(super) use std::sync::Arc;
pub(super) use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
pub(super) use std::time::Duration;
pub(super) use tokio::sync::mpsc;
pub(super) use tokio::sync::oneshot;
pub(super) use tokio_util::sync::CancellationToken;
pub(super) use tower_http::limit::RequestBodyLimitLayer;

use crate::downstream_oauth::owner::tests::BrowserAuthenticator;
use crate::downstream_oauth::{
    AuthorizationRequest, ClientRegistrationRequest, DownstreamOauthError,
};
use crate::proxy::RouterSnapshot;
use crate::test_helpers::collect_sse_events;
use axum::body::Body;
use http::Request as HttpRequest;
use std::collections::HashMap;
use tower::ServiceExt;

pub(crate) fn isolated_oauth_manager(
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
        },
        path,
    )
    .expect("isolated OAuth manager")
}

pub(crate) fn oauth_test_state() -> Arc<HttpState> {
    oauth_test_state_with_manager(isolated_oauth_manager(vec!["tools:read".to_string()]))
}

pub(crate) fn oauth_test_state_with_manager(
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
        .with_auth(crate::http::server::HttpAuth::Oauth(manager)),
    )
}

pub(crate) async fn oauth_test_client(
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

pub(crate) async fn oauth_test_consent(
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

pub(crate) fn oauth_authorize_request(client_id: &str) -> HttpRequest<Body> {
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

pub(crate) async fn enroll_test_owner(
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

pub(crate) fn oauth_tools_list_request(access_token: &str, session_id: &str) -> HttpRequest<Body> {
    legacy_oauth_session_request(access_token, session_id, 2, "tools/list")
}

pub(crate) fn test_state_with_router_config(
    router_config: crate::proxy::RouterConfig,
) -> Arc<HttpState> {
    let sm = Arc::new(crate::server::ServerManager::new());
    let router = Arc::new(ToolRouter::new(sm, router_config));
    Arc::new(HttpState::new(
        router,
        Arc::new(crate::session::SessionStore::new(1800, 100)),
        CancellationToken::new(),
    ))
}

pub(crate) fn test_state() -> Arc<HttpState> {
    test_state_with_router_config(crate::proxy::RouterConfig::default())
}

pub(crate) fn enable_test_task_surface(router: &ToolRouter) {
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

pub(crate) fn modern_request(method: &str, mut params: serde_json::Value) -> HttpRequest<Body> {
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

pub(crate) async fn issue_test_oauth_token(
    manager: &crate::downstream_oauth::DownstreamOauthManager,
    scope: &str,
) -> String {
    issue_test_oauth_grant(manager, scope).await.1.access_token
}

/// Same flow as `issue_test_oauth_token`, but keeps the client id and the
/// refresh token, which rotation and reuse-detection tests need.
pub(crate) async fn issue_test_oauth_grant(
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

pub(crate) fn legacy_oauth_session_request(
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

pub(crate) async fn legacy_oauth_session(app: &axum::Router, access_token: &str) -> String {
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

pub(crate) fn test_state_with_auth(token: &str) -> Arc<HttpState> {
    let sm = Arc::new(crate::server::ServerManager::new());
    let router = Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()));
    Arc::new(
        HttpState::new(
            router,
            Arc::new(crate::session::SessionStore::new(1800, 100)),
            CancellationToken::new(),
        )
        .with_auth(crate::http::server::HttpAuth::Bearer(Arc::from(token))),
    )
}

pub(crate) fn bare_router() -> Arc<ToolRouter> {
    let sm = Arc::new(crate::server::ServerManager::new());
    Arc::new(ToolRouter::new(sm, crate::proxy::RouterConfig::default()))
}

/// Bind `session_id` the way `initialize` would, under `grant`'s client when
/// one is given, with every broadcast admitted.
pub(crate) fn bind_for_tests(sessions: &SessionStore, session_id: &str, grant: Option<&str>) {
    sessions
        .bind(
            session_id,
            crate::session::SessionBinding {
                grant: grant.map(|client_id| crate::session::GrantBinding {
                    client_id: client_id.to_string(),
                    still_allowed: Arc::new(|| true),
                }),
                broadcast_audience: crate::session::BroadcastAudience::unrestricted(),
            },
        )
        .expect("bind test session");
}
