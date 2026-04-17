#![forbid(unsafe_code)]

//! Integration tests for plug-core.
//!
//! These tests exercise the ProxyHandler, client detection, and config
//! loading at the unit level without spawning child processes.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Form, Query, State};
use axum::http::Request as HttpRequest;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use futures::FutureExt;
use oauth2::{AccessToken, RefreshToken, TokenResponse, basic::BasicTokenType};
use plug_core::client_detect::detect_client;
use plug_core::config::{Config, ServerConfig, TransportType, validate_config};
use plug_core::engine::Engine;
use plug_core::http::server::{HttpState, build_router};
use plug_core::http::session::SessionManager;
use plug_core::oauth;
use plug_core::proxy::ProxyHandler;
use plug_core::server::ServerManager;
use plug_core::session::SessionStore;
use plug_core::types::ClientType;
use rmcp::ErrorData as McpError;
use rmcp::ServiceExt as _;
use rmcp::handler::client::ClientHandler;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, CreateElicitationRequestParams,
    CreateElicitationResult, CreateMessageRequestParams, CreateMessageResult, ElicitationAction,
    ElicitationCapability, FormElicitationCapability, Implementation, SamplingCapability,
    ReadResourceRequestParams, ResourceContents, SamplingMessage, UrlElicitationCapability,
};
use rmcp::service::RequestContext;
use rmcp::transport::auth::CredentialStore;
use rmcp::transport::auth::{StoredCredentials, VendorExtraTokenFields};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const HTTP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";
const HTTP_PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";
const HTTP_PROTOCOL_VERSION: &str = "2025-11-25";

fn oauth_integration_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn oauth_test_credentials(access: &str, refresh: &str) -> StoredCredentials {
    let mut token = oauth2::StandardTokenResponse::<VendorExtraTokenFields, BasicTokenType>::new(
        AccessToken::new(access.to_string()),
        BasicTokenType::Bearer,
        VendorExtraTokenFields::default(),
    );
    token.set_refresh_token(Some(RefreshToken::new(refresh.to_string())));
    token.set_expires_in(Some(&Duration::from_secs(3600)));

    StoredCredentials {
        client_id: "test-client".to_string(),
        token_response: Some(token),
        granted_scopes: vec![],
        token_received_at: Some(0),
    }
}

fn oauth_state_file_path(server_name: &str, state: &str) -> std::path::PathBuf {
    let safe_server =
        plug_core::config::sanitize_server_name_for_path(server_name).expect("valid server name");
    let safe_state: String = state
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    oauth::tokens_dir().join(format!("{safe_server}_state_{safe_state}.json"))
}

#[derive(Clone)]
struct MockOAuthProviderState {
    base_url: String,
    shared: Arc<Mutex<MockOAuthProviderShared>>,
}

struct MockOAuthProviderShared {
    metadata_requests: usize,
    authorize_requests: usize,
    token_grants: Vec<String>,
    pkce_verified: bool,
    current_access_token: String,
    current_refresh_token: String,
    pending_codes: HashMap<String, PendingAuthorizationCode>,
    mcp_auth_headers: Vec<String>,
    mcp_methods: Vec<String>,
}

struct PendingAuthorizationCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
}

#[derive(Debug)]
struct MockOAuthSnapshot {
    metadata_requests: usize,
    authorize_requests: usize,
    token_grants: Vec<String>,
    pkce_verified: bool,
    mcp_auth_headers: Vec<String>,
}

struct MockOAuthProvider {
    base_url: String,
    shared: Arc<Mutex<MockOAuthProviderShared>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockOAuthProvider {
    async fn start() -> Self {
        let shared = Arc::new(Mutex::new(MockOAuthProviderShared {
            metadata_requests: 0,
            authorize_requests: 0,
            token_grants: Vec::new(),
            pkce_verified: false,
            current_access_token: "access-token-1".to_string(),
            current_refresh_token: "refresh-token-1".to_string(),
            pending_codes: HashMap::new(),
            mcp_auth_headers: Vec::new(),
            mcp_methods: Vec::new(),
        }));

        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock oauth provider");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("provider local addr")
        );

        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(mock_oauth_metadata_handler),
            )
            .route(
                "/authorize",
                axum::routing::get(mock_oauth_authorize_handler),
            )
            .route("/token", axum::routing::post(mock_oauth_token_handler))
            .route("/mcp", axum::routing::post(mock_oauth_mcp_handler))
            .with_state(MockOAuthProviderState {
                base_url: base_url.clone(),
                shared: Arc::clone(&shared),
            });

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock oauth provider");
        });

        Self {
            base_url,
            shared,
            handle,
        }
    }

    fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url)
    }

    async fn snapshot(&self) -> MockOAuthSnapshot {
        let state = self.shared.lock().await;
        MockOAuthSnapshot {
            metadata_requests: state.metadata_requests,
            authorize_requests: state.authorize_requests,
            token_grants: state.token_grants.clone(),
            pkce_verified: state.pkce_verified,
            mcp_auth_headers: state.mcp_auth_headers.clone(),
        }
    }
}

struct MockStatelessOauthProvider {
    base_url: String,
    shared: Arc<Mutex<MockOAuthProviderShared>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockStatelessOauthProvider {
    async fn start() -> Self {
        let shared = Arc::new(Mutex::new(MockOAuthProviderShared {
            metadata_requests: 0,
            authorize_requests: 0,
            token_grants: Vec::new(),
            pkce_verified: false,
            current_access_token: "access-token-1".to_string(),
            current_refresh_token: "refresh-token-1".to_string(),
            pending_codes: HashMap::new(),
            mcp_auth_headers: Vec::new(),
            mcp_methods: Vec::new(),
        }));

        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock stateless oauth provider");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("provider local addr")
        );

        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(mock_oauth_metadata_handler),
            )
            .route(
                "/authorize",
                axum::routing::get(mock_oauth_authorize_handler),
            )
            .route("/token", axum::routing::post(mock_oauth_token_handler))
            .route("/mcp", axum::routing::post(mock_stateless_oauth_mcp_handler))
            .with_state(MockOAuthProviderState {
                base_url: base_url.clone(),
                shared: Arc::clone(&shared),
            });

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock stateless oauth provider");
        });

        Self {
            base_url,
            shared,
            handle,
        }
    }

    fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url)
    }
}

impl Drop for MockStatelessOauthProvider {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

struct MockToolListFailureOauthProvider {
    base_url: String,
    _shared: Arc<Mutex<MockOAuthProviderShared>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockToolListFailureOauthProvider {
    async fn start() -> Self {
        let shared = Arc::new(Mutex::new(MockOAuthProviderShared {
            metadata_requests: 0,
            authorize_requests: 0,
            token_grants: Vec::new(),
            pkce_verified: false,
            current_access_token: "access-token-1".to_string(),
            current_refresh_token: "refresh-token-1".to_string(),
            pending_codes: HashMap::new(),
            mcp_auth_headers: Vec::new(),
            mcp_methods: Vec::new(),
        }));

        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock failure oauth provider");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("provider local addr")
        );

        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(mock_oauth_metadata_handler),
            )
            .route(
                "/authorize",
                axum::routing::get(mock_oauth_authorize_handler),
            )
            .route("/token", axum::routing::post(mock_oauth_token_handler))
            .route("/mcp", axum::routing::post(mock_tool_list_failure_oauth_mcp_handler))
            .with_state(MockOAuthProviderState {
                base_url: base_url.clone(),
                shared: Arc::clone(&shared),
            });

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock failure oauth provider");
        });

        Self {
            base_url,
            _shared: shared,
            handle,
        }
    }

    fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url)
    }
}

impl Drop for MockToolListFailureOauthProvider {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

struct MockInitializedNotificationFailureOauthProvider {
    base_url: String,
    shared: Arc<Mutex<MockOAuthProviderShared>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockInitializedNotificationFailureOauthProvider {
    async fn start() -> Self {
        let shared = Arc::new(Mutex::new(MockOAuthProviderShared {
            metadata_requests: 0,
            authorize_requests: 0,
            token_grants: Vec::new(),
            pkce_verified: false,
            current_access_token: "access-token-1".to_string(),
            current_refresh_token: "refresh-token-1".to_string(),
            pending_codes: HashMap::new(),
            mcp_auth_headers: Vec::new(),
            mcp_methods: Vec::new(),
        }));

        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock initialized-notification failure provider");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("provider local addr")
        );

        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(mock_oauth_metadata_handler),
            )
            .route(
                "/authorize",
                axum::routing::get(mock_oauth_authorize_handler),
            )
            .route("/token", axum::routing::post(mock_oauth_token_handler))
            .route(
                "/mcp",
                axum::routing::post(mock_initialized_notification_failure_oauth_mcp_handler),
            )
            .with_state(MockOAuthProviderState {
                base_url: base_url.clone(),
                shared: Arc::clone(&shared),
            });

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock initialized-notification failure provider");
        });

        Self {
            base_url,
            shared,
            handle,
        }
    }

    fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url)
    }
}

impl Drop for MockInitializedNotificationFailureOauthProvider {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

struct MockInitializedNotificationAuthFailureOauthProvider {
    base_url: String,
    shared: Arc<Mutex<MockOAuthProviderShared>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockInitializedNotificationAuthFailureOauthProvider {
    async fn start() -> Self {
        let shared = Arc::new(Mutex::new(MockOAuthProviderShared {
            metadata_requests: 0,
            authorize_requests: 0,
            token_grants: Vec::new(),
            pkce_verified: false,
            current_access_token: "access-token-1".to_string(),
            current_refresh_token: "refresh-token-1".to_string(),
            pending_codes: HashMap::new(),
            mcp_auth_headers: Vec::new(),
            mcp_methods: Vec::new(),
        }));

        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock initialized-notification auth failure provider");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("provider local addr")
        );

        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(mock_oauth_metadata_handler),
            )
            .route(
                "/authorize",
                axum::routing::get(mock_oauth_authorize_handler),
            )
            .route("/token", axum::routing::post(mock_oauth_token_handler))
            .route(
                "/mcp",
                axum::routing::post(mock_initialized_notification_auth_failure_oauth_mcp_handler),
            )
            .with_state(MockOAuthProviderState {
                base_url: base_url.clone(),
                shared: Arc::clone(&shared),
            });

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock initialized-notification auth failure provider");
        });

        Self {
            base_url,
            shared,
            handle,
        }
    }

    fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url)
    }
}

impl Drop for MockInitializedNotificationAuthFailureOauthProvider {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

struct MockInitializedNotificationServerErrorOauthProvider {
    base_url: String,
    shared: Arc<Mutex<MockOAuthProviderShared>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockInitializedNotificationServerErrorOauthProvider {
    async fn start() -> Self {
        let shared = Arc::new(Mutex::new(MockOAuthProviderShared {
            metadata_requests: 0,
            authorize_requests: 0,
            token_grants: Vec::new(),
            pkce_verified: false,
            current_access_token: "access-token-1".to_string(),
            current_refresh_token: "refresh-token-1".to_string(),
            pending_codes: HashMap::new(),
            mcp_auth_headers: Vec::new(),
            mcp_methods: Vec::new(),
        }));

        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock initialized-notification server error provider");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("provider local addr")
        );

        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(mock_oauth_metadata_handler),
            )
            .route(
                "/authorize",
                axum::routing::get(mock_oauth_authorize_handler),
            )
            .route("/token", axum::routing::post(mock_oauth_token_handler))
            .route(
                "/mcp",
                axum::routing::post(mock_initialized_notification_server_error_oauth_mcp_handler),
            )
            .with_state(MockOAuthProviderState {
                base_url: base_url.clone(),
                shared: Arc::clone(&shared),
            });

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock initialized-notification server error provider");
        });

        Self {
            base_url,
            shared,
            handle,
        }
    }

    fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url)
    }
}

impl Drop for MockInitializedNotificationServerErrorOauthProvider {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl Drop for MockOAuthProvider {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn mock_oauth_metadata_handler(
    State(state): State<MockOAuthProviderState>,
) -> impl IntoResponse {
    let mut shared = state.shared.lock().await;
    shared.metadata_requests += 1;

    axum::Json(serde_json::json!({
        "issuer": state.base_url,
        "authorization_endpoint": format!("{}/authorize", state.base_url),
        "token_endpoint": format!("{}/token", state.base_url)
    }))
}

async fn mock_oauth_authorize_handler(
    State(state): State<MockOAuthProviderState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let client_id = match params.get("client_id") {
        Some(value) => value.clone(),
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    let redirect_uri = match params.get("redirect_uri") {
        Some(value) => value.clone(),
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    let state_param = match params.get("state") {
        Some(value) => value.clone(),
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    let code_challenge = match params.get("code_challenge") {
        Some(value) => value.clone(),
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    if params.get("code_challenge_method").map(String::as_str) != Some("S256")
        || params.get("response_type").map(String::as_str) != Some("code")
    {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let code = format!("code-{}", state_param);
    state.shared.lock().await.pending_codes.insert(
        code.clone(),
        PendingAuthorizationCode {
            client_id,
            redirect_uri: redirect_uri.clone(),
            code_challenge,
        },
    );
    state.shared.lock().await.authorize_requests += 1;

    Redirect::to(&format!("{redirect_uri}?code={code}&state={state_param}")).into_response()
}

async fn mock_oauth_token_handler(
    State(state): State<MockOAuthProviderState>,
    Form(params): Form<HashMap<String, String>>,
) -> Response {
    let grant_type = match params.get("grant_type").map(String::as_str) {
        Some(value) => value,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    let mut shared = state.shared.lock().await;
    shared.token_grants.push(grant_type.to_string());

    match grant_type {
        "authorization_code" => {
            let code = match params.get("code") {
                Some(value) => value,
                None => return StatusCode::BAD_REQUEST.into_response(),
            };
            let verifier = match params.get("code_verifier") {
                Some(value) => value,
                None => return StatusCode::BAD_REQUEST.into_response(),
            };
            let client_id = match params.get("client_id") {
                Some(value) => value,
                None => return StatusCode::BAD_REQUEST.into_response(),
            };
            let redirect_uri = match params.get("redirect_uri") {
                Some(value) => value,
                None => return StatusCode::BAD_REQUEST.into_response(),
            };
            let pending = match shared.pending_codes.remove(code) {
                Some(value) => value,
                None => return StatusCode::BAD_REQUEST.into_response(),
            };

            let verifier = oauth2::PkceCodeVerifier::new(verifier.clone());
            let challenge = oauth2::PkceCodeChallenge::from_code_verifier_sha256(&verifier);
            if client_id != &pending.client_id
                || redirect_uri != &pending.redirect_uri
                || challenge.as_str() != pending.code_challenge
            {
                return StatusCode::BAD_REQUEST.into_response();
            }

            shared.pkce_verified = true;
            shared.current_access_token = "access-token-1".to_string();
            shared.current_refresh_token = "refresh-token-1".to_string();

            axum::Json(serde_json::json!({
                "access_token": "access-token-1",
                "refresh_token": "refresh-token-1",
                "token_type": "Bearer",
                "expires_in": 3600,
                "scope": "read"
            }))
            .into_response()
        }
        "refresh_token" => {
            if params.get("refresh_token").map(String::as_str)
                != Some(shared.current_refresh_token.as_str())
            {
                return StatusCode::UNAUTHORIZED.into_response();
            }

            shared.current_access_token = "access-token-2".to_string();
            shared.current_refresh_token = "refresh-token-2".to_string();

            axum::Json(serde_json::json!({
                "access_token": "access-token-2",
                "refresh_token": "refresh-token-2",
                "token_type": "Bearer",
                "expires_in": 3600,
                "scope": "read"
            }))
            .into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn mock_oauth_mcp_handler(
    State(state): State<MockOAuthProviderState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());

    let mut shared = state.shared.lock().await;
    if let Some(ref header) = auth_header {
        shared.mcp_auth_headers.push(header.clone());
    }

    let expected = format!("Bearer {}", shared.current_access_token);
    if auth_header.as_deref() != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    drop(shared);

    let json_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let method = json_body
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let mut shared = state.shared.lock().await;
    shared.mcp_methods.push(method.to_string());
    drop(shared);

    let session_headers = [
        (
            axum::http::HeaderName::from_static("mcp-session-id"),
            "test-session",
        ),
        (axum::http::header::CONTENT_TYPE, "application/json"),
    ];

    if method == "initialize" {
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": json_body.get("id"),
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {
                    "tools": { "listChanged": false }
                },
                "serverInfo": {
                    "name": "mock-http-server",
                    "version": "0.1.0"
                }
            }
        });
        return (StatusCode::OK, session_headers, resp.to_string()).into_response();
    }

    if method == "notifications/initialized" {
        return (StatusCode::ACCEPTED, session_headers, String::new()).into_response();
    }

    if method == "tools/list" {
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": json_body.get("id"),
            "result": { "tools": [] }
        });
        return (StatusCode::OK, session_headers, resp.to_string()).into_response();
    }

    (StatusCode::BAD_REQUEST, session_headers, String::new()).into_response()
}

async fn mock_stateless_oauth_mcp_handler(
    State(state): State<MockOAuthProviderState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());

    let mut shared = state.shared.lock().await;
    if let Some(ref header) = auth_header {
        shared.mcp_auth_headers.push(header.clone());
    }

    let expected = format!("Bearer {}", shared.current_access_token);
    if auth_header.as_deref() != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    drop(shared);

    let json_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let method = json_body
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let mut shared = state.shared.lock().await;
    shared.mcp_methods.push(method.to_string());
    drop(shared);

    let sse_headers = [(axum::http::header::CONTENT_TYPE, "text/event-stream")];

    let payload = match method {
        "initialize" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": json_body.get("id"),
            "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {
                    "tools": { "listChanged": true }
                },
                "serverInfo": {
                    "name": "mock-stateless-http-server",
                    "version": "1.0.0"
                }
            }
        }),
        "notifications/initialized" => {
            return (StatusCode::ACCEPTED, sse_headers, String::new()).into_response();
        }
        "tools/list" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": json_body.get("id"),
            "result": {
                "tools": [{
                    "name": "search_meetings",
                    "description": "Search meetings",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                }]
            }
        }),
        _ => return (StatusCode::BAD_REQUEST, sse_headers, String::new()).into_response(),
    };

    let event_stream = format!("event: message\ndata: {payload}\n\n");
    (StatusCode::OK, sse_headers, event_stream).into_response()
}

async fn mock_tool_list_failure_oauth_mcp_handler(
    State(state): State<MockOAuthProviderState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());

    let mut shared = state.shared.lock().await;
    if let Some(ref header) = auth_header {
        shared.mcp_auth_headers.push(header.clone());
    }

    let expected = format!("Bearer {}", shared.current_access_token);
    if auth_header.as_deref() != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    drop(shared);

    let json_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let method = json_body
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let mut shared = state.shared.lock().await;
    shared.mcp_methods.push(method.to_string());
    drop(shared);

    let session_headers = [
        (
            axum::http::HeaderName::from_static("mcp-session-id"),
            "test-session",
        ),
        (axum::http::header::CONTENT_TYPE, "application/json"),
    ];

    match method {
        "initialize" => {
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {
                        "tools": { "listChanged": false }
                    },
                    "serverInfo": {
                        "name": "mock-tool-list-failure-server",
                        "version": "1.0.0"
                    }
                }
            });
            (StatusCode::OK, session_headers, resp.to_string()).into_response()
        }
        "notifications/initialized" => {
            (StatusCode::ACCEPTED, session_headers, String::new()).into_response()
        }
        "tools/list" => (
            StatusCode::INTERNAL_SERVER_ERROR,
            session_headers,
            "backend temporarily unavailable".to_string(),
        )
            .into_response(),
        _ => (StatusCode::BAD_REQUEST, session_headers, String::new()).into_response(),
    }
}

async fn mock_initialized_notification_failure_oauth_mcp_handler(
    State(state): State<MockOAuthProviderState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());

    let mut shared = state.shared.lock().await;
    if let Some(ref header) = auth_header {
        shared.mcp_auth_headers.push(header.clone());
    }

    let expected = format!("Bearer {}", shared.current_access_token);
    if auth_header.as_deref() != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    drop(shared);

    let json_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let method = json_body
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let mut shared = state.shared.lock().await;
    shared.mcp_methods.push(method.to_string());
    drop(shared);

    let sse_headers = [(axum::http::header::CONTENT_TYPE, "text/event-stream")];

    match method {
        "initialize" => {
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {
                        "tools": { "listChanged": true }
                    },
                    "serverInfo": {
                        "name": "mock-initialized-notification-failure-server",
                        "version": "1.0.0"
                    }
                }
            });
            let event_stream = format!("event: message\ndata: {payload}\n\n");
            (StatusCode::OK, sse_headers, event_stream).into_response()
        }
        "notifications/initialized" => StatusCode::BAD_REQUEST.into_response(),
        "tools/list" => {
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": {
                    "tools": [{
                        "name": "search_meetings",
                        "description": "Search meetings",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    }]
                }
            });
            let event_stream = format!("event: message\ndata: {payload}\n\n");
            (StatusCode::OK, sse_headers, event_stream).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, sse_headers, String::new()).into_response(),
    }
}

async fn mock_initialized_notification_auth_failure_oauth_mcp_handler(
    State(state): State<MockOAuthProviderState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());

    let mut shared = state.shared.lock().await;
    if let Some(ref header) = auth_header {
        shared.mcp_auth_headers.push(header.clone());
    }

    let expected = format!("Bearer {}", shared.current_access_token);
    if auth_header.as_deref() != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    drop(shared);

    let json_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let method = json_body
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");

    let mut shared = state.shared.lock().await;
    shared.mcp_methods.push(method.to_string());
    drop(shared);

    let sse_headers = [(axum::http::header::CONTENT_TYPE, "text/event-stream")];

    match method {
        "initialize" => {
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {
                        "tools": { "listChanged": true }
                    },
                    "serverInfo": {
                        "name": "mock-initialized-notification-auth-failure-server",
                        "version": "1.0.0"
                    }
                }
            });
            let event_stream = format!("event: message\ndata: {payload}\n\n");
            (StatusCode::OK, sse_headers, event_stream).into_response()
        }
        "notifications/initialized" => StatusCode::UNAUTHORIZED.into_response(),
        "tools/list" => {
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": {
                    "tools": [{
                        "name": "search_meetings",
                        "description": "Search meetings",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    }]
                }
            });
            let event_stream = format!("event: message\ndata: {payload}\n\n");
            (StatusCode::OK, sse_headers, event_stream).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, sse_headers, String::new()).into_response(),
    }
}

async fn mock_initialized_notification_server_error_oauth_mcp_handler(
    State(state): State<MockOAuthProviderState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());

    let mut shared = state.shared.lock().await;
    if let Some(ref header) = auth_header {
        shared.mcp_auth_headers.push(header.clone());
    }

    let expected = format!("Bearer {}", shared.current_access_token);
    if auth_header.as_deref() != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    drop(shared);

    let json_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let method = json_body
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");

    let mut shared = state.shared.lock().await;
    shared.mcp_methods.push(method.to_string());
    drop(shared);

    let sse_headers = [(axum::http::header::CONTENT_TYPE, "text/event-stream")];

    match method {
        "initialize" => {
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {
                        "tools": { "listChanged": true }
                    },
                    "serverInfo": {
                        "name": "mock-initialized-notification-server-error-server",
                        "version": "1.0.0"
                    }
                }
            });
            let event_stream = format!("event: message\ndata: {payload}\n\n");
            (StatusCode::OK, sse_headers, event_stream).into_response()
        }
        "notifications/initialized" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        "tools/list" => {
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": {
                    "tools": [{
                        "name": "search_meetings",
                        "description": "Search meetings",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    }]
                }
            });
            let event_stream = format!("event: message\ndata: {payload}\n\n");
            (StatusCode::OK, sse_headers, event_stream).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, sse_headers, String::new()).into_response(),
    }
}

#[derive(Clone)]
struct TestClient;

impl ClientHandler for TestClient {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::default()
    }
}

fn mock_server_config(tools: &str) -> ServerConfig {
    ServerConfig {
        command: Some("cargo".to_string()),
        args: vec![
            "run".to_string(),
            "--quiet".to_string(),
            "-p".to_string(),
            "plug-test-harness".to_string(),
            "--bin".to_string(),
            "mock-mcp-server".to_string(),
            "--".to_string(),
            "--tools".to_string(),
            tools.to_string(),
        ],
        env: HashMap::new(),
        enabled: true,
        transport: TransportType::Stdio,
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
        tool_renames: HashMap::new(),
        tool_groups: Vec::new(),
    }
}

async fn reserve_unused_local_port() -> u16 {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind ephemeral listener");
    listener.local_addr().expect("ephemeral local addr").port()
}

async fn collect_sse_events(body: Body, max_events: usize) -> Vec<String> {
    let mut events = Vec::new();
    let mut stream = body.into_data_stream();
    use futures::StreamExt;

    let timeout = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(chunk)) = stream.next().await {
            let text = String::from_utf8_lossy(&chunk).to_string();
            for part in text.split("\n\n") {
                let trimmed = part.trim();
                if !trimmed.is_empty() {
                    events.push(trimmed.to_string());
                }
            }
            if events.len() >= max_events {
                break;
            }
        }
    });

    let _ = timeout.await;
    events
}

// ---------------------------------------------------------------------------
// ProxyHandler: list_tools with no upstream servers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_proxy_handler_refresh_tools_empty() {
    let sm = Arc::new(ServerManager::new());
    let handler = ProxyHandler::new(
        sm,
        plug_core::proxy::RouterConfig {
            prefix_delimiter: "__".to_string(),
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            meta_tool_mode: false,
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        },
    );
    handler.refresh_tools().await;

    // Verify the handler still works (get_info returns valid info)
    let info = handler.get_info();
    assert!(info.capabilities.tools.is_none());
}

// ---------------------------------------------------------------------------
// ProxyHandler: get_info returns correct server info
// ---------------------------------------------------------------------------

#[test]
fn test_proxy_handler_get_info() {
    let sm = Arc::new(ServerManager::new());
    let handler = ProxyHandler::new(
        sm,
        plug_core::proxy::RouterConfig {
            prefix_delimiter: "__".to_string(),
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            meta_tool_mode: false,
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        },
    );
    let info = handler.get_info();

    assert_eq!(info.server_info.name, "plug");
    assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    assert!(info.capabilities.tools.is_none());
}

// ---------------------------------------------------------------------------
// ProxyHandler: routing table is empty with no servers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_server_manager_tools_empty() {
    let sm = Arc::new(ServerManager::new());
    let tools = sm.get_tools().await;
    assert!(
        tools.is_empty(),
        "expected no tools from empty ServerManager"
    );
}

// ---------------------------------------------------------------------------
// ProxyHandler: resources and prompts capabilities
// ---------------------------------------------------------------------------

#[test]
fn test_resources_capability_present() {
    // The ProxyHandler advertises resources capability (returns empty list, not error).
    let sm = Arc::new(ServerManager::new());
    let handler = ProxyHandler::new(
        sm,
        plug_core::proxy::RouterConfig {
            prefix_delimiter: "__".to_string(),
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            meta_tool_mode: false,
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        },
    );
    let info = handler.get_info();

    assert!(
        info.capabilities.resources.is_none(),
        "resources capability should be omitted until upstream support exists"
    );
}

#[test]
fn test_prompts_not_advertised() {
    // The ProxyHandler does not advertise prompts capability in server info,
    // but the trait default returns Ok(empty) so it won't error if called.
    let sm = Arc::new(ServerManager::new());
    let handler = ProxyHandler::new(
        sm,
        plug_core::proxy::RouterConfig {
            prefix_delimiter: "__".to_string(),
            priority_tools: Vec::new(),
            disabled_tools: Vec::new(),
            tool_description_max_chars: None,
            tool_search_threshold: 50,
            meta_tool_mode: false,
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        },
    );
    let info = handler.get_info();

    // prompts is None in capabilities (default), which means list_prompts
    // falls back to the trait default returning an empty list.
    assert!(
        info.capabilities.prompts.is_none(),
        "prompts should not be explicitly advertised"
    );
}

// ---------------------------------------------------------------------------
// Client detection: exact matches
// ---------------------------------------------------------------------------

#[test]
fn test_client_detection_exact_matches() {
    assert_eq!(detect_client("claude-code"), ClientType::ClaudeCode);
    assert_eq!(detect_client("claude-ai"), ClientType::ClaudeDesktop);
    assert_eq!(detect_client("cursor-vscode"), ClientType::Cursor);
    assert_eq!(detect_client("windsurf-client"), ClientType::Windsurf);
    assert_eq!(
        detect_client("Visual-Studio-Code"),
        ClientType::VSCodeCopilot
    );
    assert_eq!(
        detect_client("gemini-cli-mcp-client"),
        ClientType::GeminiCli
    );
    assert_eq!(detect_client("opencode"), ClientType::OpenCode);
    assert_eq!(detect_client("Zed"), ClientType::Zed);
}

// ---------------------------------------------------------------------------
// Client detection: fuzzy fallback
// ---------------------------------------------------------------------------

#[test]
fn test_client_detection_fuzzy() {
    assert_eq!(detect_client("Claude Code v2"), ClientType::ClaudeCode);
    assert_eq!(detect_client("claude-desktop"), ClientType::ClaudeDesktop);
    assert_eq!(detect_client("cursor-next"), ClientType::Cursor);
    assert_eq!(detect_client("codeium-editor"), ClientType::Windsurf);
    assert_eq!(detect_client("github-copilot"), ClientType::VSCodeCopilot);
    assert_eq!(detect_client("codex-cli"), ClientType::CodexCli);
}

// ---------------------------------------------------------------------------
// Client detection: unknown
// ---------------------------------------------------------------------------

#[test]
fn test_client_detection_unknown() {
    assert_eq!(detect_client("some-random-client"), ClientType::Unknown);
    assert_eq!(detect_client(""), ClientType::Unknown);
}

// ---------------------------------------------------------------------------
// Config loading from TOML string
// ---------------------------------------------------------------------------

#[test]
fn test_config_loading_defaults() {
    let cfg = Config::default();
    assert_eq!(cfg.http.bind_address, "127.0.0.1");
    assert_eq!(cfg.http.port, 3282);
    assert_eq!(cfg.log_level, "info");
    assert_eq!(cfg.prefix_delimiter, "__");
    assert!(cfg.enable_prefix);
    assert_eq!(cfg.startup_concurrency, 3);
    assert!(cfg.servers.is_empty());
}

#[test]
fn test_config_loading_from_toml() {
    use figment::Figment;
    use figment::providers::{Format, Serialized, Toml};

    let toml_str = r#"
        log_level = "debug"
        prefix_delimiter = "::"

        [http]
        port = 9090

        [servers.myserver]
        command = "node"
        args = ["server.js"]
        timeout_secs = 15
    "#;

    let cfg: Config = Figment::new()
        .merge(Serialized::defaults(Config::default()))
        .merge(Toml::string(toml_str))
        .extract()
        .expect("failed to parse TOML");

    assert_eq!(cfg.http.port, 9090);
    assert_eq!(cfg.log_level, "debug");
    assert_eq!(cfg.prefix_delimiter, "::");

    let srv = cfg.servers.get("myserver").expect("server missing");
    assert_eq!(srv.command.as_deref(), Some("node"));
    assert_eq!(srv.args, vec!["server.js"]);
    assert_eq!(srv.timeout_secs, 15);
    assert!(srv.enabled);
}

// ---------------------------------------------------------------------------
// Config validation
// ---------------------------------------------------------------------------

#[test]
fn test_config_validation_valid() {
    let mut cfg = Config::default();
    cfg.servers.insert(
        "valid".to_string(),
        ServerConfig {
            command: Some("node".to_string()),
            args: vec![],
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
        },
    );
    let errors = validate_config(&cfg);
    assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
}

#[test]
fn test_config_validation_catches_missing_command() {
    let mut cfg = Config::default();
    cfg.servers.insert(
        "bad".to_string(),
        ServerConfig {
            command: None,
            args: vec![],
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
        },
    );
    let errors = validate_config(&cfg);
    assert!(
        errors.iter().any(|e| e.contains("command")),
        "expected command error, got: {errors:?}"
    );
}

// ---------------------------------------------------------------------------
// Mock server binary path
// ---------------------------------------------------------------------------

#[test]
fn test_mock_server_path_is_reasonable() {
    let path = plug_test_harness::mock_server_path();
    assert!(
        path.ends_with("mock-mcp-server"),
        "unexpected mock server path: {path:?}"
    );
}

#[tokio::test]
async fn test_stdio_timeout_reconnects_cleanly() {
    let temp = std::env::temp_dir().join(format!("plug-timeout-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).expect("create temp dir");
    let marker = temp.join("first-run.marker");
    let script = temp.join("mock-wrapper.sh");

    std::fs::write(
        &script,
        "#!/bin/sh\nif [ ! -f \"$1\" ]; then\n  touch \"$1\"\n  exec cargo run --quiet -p plug-test-harness --bin mock-mcp-server -- --tools echo --delay-ms 1500\nelse\n  exec cargo run --quiet -p plug-test-harness --bin mock-mcp-server -- --tools echo --delay-ms 0\nfi\n",
    )
    .expect("write wrapper");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }

    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        ServerConfig {
            command: Some(script.display().to_string()),
            args: vec![marker.display().to_string()],
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 10,
            call_timeout_secs: 1,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
        },
    );

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let first = engine.tool_router().call_tool("Mock__echo", None).await;
    assert!(first.is_err(), "first call should time out");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let second = engine
        .tool_router()
        .call_tool(
            "Mock__echo",
            Some(
                serde_json::json!({"input": "ok"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await;
    assert!(second.is_ok(), "second call should succeed after reconnect");

    engine.shutdown().await;
    let _ = std::fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn test_stdio_crash_restart_recovers_cleanly() {
    let temp = std::env::temp_dir().join(format!("plug-crash-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).expect("create temp dir");
    let marker = temp.join("first-run.marker");
    let script = temp.join("mock-wrapper.sh");

    std::fs::write(
        &script,
        "#!/bin/sh\nif [ ! -f \"$1\" ]; then\n  touch \"$1\"\n  exec cargo run --quiet -p plug-test-harness --bin mock-mcp-server -- --tools echo --fail-mode crash\nelse\n  exec cargo run --quiet -p plug-test-harness --bin mock-mcp-server -- --tools echo --delay-ms 0\nfi\n",
    )
    .expect("write wrapper");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }

    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        ServerConfig {
            command: Some(script.display().to_string()),
            args: vec![marker.display().to_string()],
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 10,
            call_timeout_secs: 5,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
        },
    );

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let result = engine
        .tool_router()
        .call_tool(
            "Mock__echo",
            Some(
                serde_json::json!({"input": "recover"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await;
    assert!(
        result.is_ok(),
        "tool call should recover after upstream restart: {result:?}"
    );
    let rendered = format!("{:?}", result.unwrap());
    assert!(
        rendered.contains("recover"),
        "unexpected result: {rendered}"
    );

    engine.shutdown().await;
    let _ = std::fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn test_stdio_end_to_end_proxy_path() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("echo,greet"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let proxy_handler = ProxyHandler::from_router(engine.tool_router().clone());
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let server = proxy_handler
            .serve(server_transport)
            .await
            .expect("start stdio proxy server");
        let _ = server.waiting().await;
    });

    let client = TestClient
        .serve(client_transport)
        .await
        .expect("connect stdio client");

    let tools = client.peer().list_all_tools().await.expect("list tools");
    let names = tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"Mock__echo".to_string()),
        "tools: {names:?}"
    );
    assert!(
        names.contains(&"Mock__greet".to_string()),
        "tools: {names:?}"
    );

    let result = client
        .call_tool(
            CallToolRequestParams::new("Mock__echo").with_arguments(
                serde_json::json!({"input": "hello"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("call tool");
    let rendered = format!("{result:?}");
    assert!(
        rendered.contains("Called echo with"),
        "unexpected stdio result: {rendered}"
    );
    assert!(
        rendered.contains("hello"),
        "unexpected stdio result: {rendered}"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn test_http_end_to_end_proxy_path_with_sse() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("echo"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let state = Arc::new(HttpState {
        router: engine.tool_router().clone(),
        sessions: Arc::new(SessionManager::new(1800, 100)) as Arc<dyn SessionStore>,
        cancel: CancellationToken::new(),
        auth_mode: plug_core::config::DownstreamAuthMode::Auto,
        downstream_oauth: None,
        sse_channel_capacity: 32,
        allowed_origins: Vec::new(),
        notification_task_started: std::sync::atomic::AtomicBool::new(false),
        auth_token: None,
        roots_capable_sessions: dashmap::DashMap::new(),
        pending_client_requests: dashmap::DashMap::new(),
        reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
        client_capabilities: dashmap::DashMap::new(),
    });
    let app = build_router(state.clone());

    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "http-e2e", "version": "1.0" }
        }
    });
    let initialize_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&initialize_body).unwrap()))
        .unwrap();
    let initialize_resp = app.clone().oneshot(initialize_req).await.unwrap();
    let session_id = initialize_resp
        .headers()
        .get(HTTP_SESSION_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let sse_req = HttpRequest::builder()
        .method("GET")
        .uri("/mcp")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header("accept", "text/event-stream")
        .body(Body::empty())
        .unwrap();
    let sse_resp = app.clone().oneshot(sse_req).await.unwrap();
    let events = collect_sse_events(sse_resp.into_body(), 1).await;
    assert!(
        events.iter().any(|event| event.contains("id: 0")),
        "expected priming SSE event, got {events:?}"
    );

    let list_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    let list_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
        .body(Body::from(serde_json::to_vec(&list_body).unwrap()))
        .unwrap();
    let list_resp = app.clone().oneshot(list_req).await.unwrap();
    let list_bytes = axum::body::to_bytes(list_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let list_json: serde_json::Value = serde_json::from_slice(&list_bytes).unwrap();
    let tool_names = list_json["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(
        tool_names.contains(&"Mock__echo".to_string()),
        "tool names: {tool_names:?}"
    );

    let call_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "Mock__echo",
            "arguments": { "input": "http" }
        }
    });
    let call_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
        .body(Body::from(serde_json::to_vec(&call_body).unwrap()))
        .unwrap();
    let call_resp = app.oneshot(call_req).await.unwrap();
    let call_bytes = axum::body::to_bytes(call_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let call_json: serde_json::Value = serde_json::from_slice(&call_bytes).unwrap();
    let response_text = call_json["result"]["content"][0]["text"].as_str().unwrap();
    assert!(response_text.contains("Called echo with {\"input\":\"http\"}"));

    engine.shutdown().await;
}

#[tokio::test]
async fn test_stdio_structured_content_passes_through_end_to_end() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("structured"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let proxy_handler = ProxyHandler::from_router(engine.tool_router().clone());
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let server = proxy_handler
            .serve(server_transport)
            .await
            .expect("start stdio proxy server");
        let _ = server.waiting().await;
    });

    let client = TestClient
        .serve(client_transport)
        .await
        .expect("connect stdio client");

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__structured"))
        .await
        .expect("call structured tool");
    assert_eq!(
        result.structured_content,
        Some(serde_json::json!({
            "tool": "structured",
            "ok": true,
            "count": 2
        }))
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn test_stdio_resource_link_passes_through_end_to_end() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("resource_link"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let proxy_handler = ProxyHandler::from_router(engine.tool_router().clone());
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let server = proxy_handler
            .serve(server_transport)
            .await
            .expect("start stdio proxy server");
        let _ = server.waiting().await;
    });

    let client = TestClient
        .serve(client_transport)
        .await
        .expect("connect stdio client");

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__resource_link"))
        .await
        .expect("call resource_link tool");
    let resource = result
        .content
        .first()
        .and_then(|content| content.raw.as_resource_link())
        .expect("resource_link content");
    assert_eq!(resource.uri, "file:///tmp/mock-resource.txt");
    assert_eq!(resource.name, "mock-resource.txt");
    assert_eq!(resource.mime_type.as_deref(), Some("text/plain"));

    engine.shutdown().await;
}

#[tokio::test]
async fn test_stdio_oversized_tool_result_spills_to_artifact_link() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("artifact_text"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let proxy_handler = ProxyHandler::from_router(engine.tool_router().clone());
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let server = proxy_handler
            .serve(server_transport)
            .await
            .expect("start stdio proxy server");
        let _ = server.waiting().await;
    });

    let client = TestClient
        .serve(client_transport)
        .await
        .expect("connect stdio client");

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__artifact_text"))
        .await
        .expect("call oversized tool");

    let resource = result
        .content
        .iter()
        .find_map(|content| content.raw.as_resource_link())
        .expect("artifact resource_link content");
    assert!(
        resource.uri.starts_with("plug://artifact/"),
        "expected plug artifact URI, got {}",
        resource.uri
    );
    assert_eq!(result.is_error, Some(false));

    engine.shutdown().await;
}

#[tokio::test]
async fn test_stdio_artifact_manifest_is_readable() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("artifact_text"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let proxy_handler = ProxyHandler::from_router(engine.tool_router().clone());
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let server = proxy_handler
            .serve(server_transport)
            .await
            .expect("start stdio proxy server");
        let _ = server.waiting().await;
    });

    let client = TestClient
        .serve(client_transport)
        .await
        .expect("connect stdio client");

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__artifact_text"))
        .await
        .expect("call oversized tool");
    let manifest_uri = result
        .content
        .iter()
        .find_map(|content| content.raw.as_resource_link())
        .map(|resource| resource.uri.clone())
        .expect("artifact resource_link content");

    let manifest = client
        .peer()
        .read_resource(ReadResourceRequestParams::new(manifest_uri))
        .await
        .expect("read artifact manifest");
    let first = manifest.contents.first().expect("manifest contents");
    match first {
        ResourceContents::TextResourceContents { text, .. } => {
            assert!(
                text.contains("artifact_text"),
                "expected manifest to mention source tool, got: {text}"
            );
        }
        other => panic!("expected text manifest contents, got {other:?}"),
    }

    engine.shutdown().await;
}

#[tokio::test]
async fn test_stdio_artifact_chunk_is_readable() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("artifact_text"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let proxy_handler = ProxyHandler::from_router(engine.tool_router().clone());
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let server = proxy_handler
            .serve(server_transport)
            .await
            .expect("start stdio proxy server");
        let _ = server.waiting().await;
    });

    let client = TestClient
        .serve(client_transport)
        .await
        .expect("connect stdio client");

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__artifact_text"))
        .await
        .expect("call oversized tool");
    let manifest_uri = result
        .content
        .iter()
        .find_map(|content| content.raw.as_resource_link())
        .map(|resource| resource.uri.clone())
        .expect("artifact resource_link content");
    let chunk_uri = manifest_uri.replace("/manifest", "/chunk/0");

    let chunk = client
        .peer()
        .read_resource(ReadResourceRequestParams::new(chunk_uri))
        .await
        .expect("read artifact chunk");
    let first = chunk.contents.first().expect("chunk contents");
    match first {
        ResourceContents::TextResourceContents { text, .. } => {
            assert!(!text.is_empty(), "expected non-empty chunk text");
        }
        other => panic!("expected text chunk contents, got {other:?}"),
    }

    engine.shutdown().await;
}

#[tokio::test]
async fn test_stdio_attachment_like_tool_result_spills_to_artifact_link() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("attachment_blob"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let proxy_handler = ProxyHandler::from_router(engine.tool_router().clone());
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let server = proxy_handler
            .serve(server_transport)
            .await
            .expect("start stdio proxy server");
        let _ = server.waiting().await;
    });

    let client = TestClient
        .serve(client_transport)
        .await
        .expect("connect stdio client");

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__attachment_blob"))
        .await
        .expect("call attachment-like tool");

    let resource = result
        .content
        .iter()
        .find_map(|content| content.raw.as_resource_link())
        .expect("artifact resource_link content");
    assert!(resource.uri.starts_with("plug://artifact/"));
    assert_eq!(result.is_error, Some(false));

    engine.shutdown().await;
}

#[tokio::test]
async fn test_http_structured_content_passes_through_end_to_end() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("structured"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let state = Arc::new(HttpState {
        router: engine.tool_router().clone(),
        sessions: Arc::new(SessionManager::new(1800, 100)) as Arc<dyn SessionStore>,
        cancel: CancellationToken::new(),
        auth_mode: plug_core::config::DownstreamAuthMode::Auto,
        downstream_oauth: None,
        sse_channel_capacity: 32,
        allowed_origins: Vec::new(),
        notification_task_started: std::sync::atomic::AtomicBool::new(false),
        auth_token: None,
        roots_capable_sessions: dashmap::DashMap::new(),
        pending_client_requests: dashmap::DashMap::new(),
        reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
        client_capabilities: dashmap::DashMap::new(),
    });
    let app = build_router(state.clone());

    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "http-structured", "version": "1.0" }
        }
    });
    let initialize_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&initialize_body).unwrap()))
        .unwrap();
    let initialize_resp = app.clone().oneshot(initialize_req).await.unwrap();
    let session_id = initialize_resp
        .headers()
        .get(HTTP_SESSION_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let call_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "Mock__structured"
        }
    });
    let call_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
        .body(Body::from(serde_json::to_vec(&call_body).unwrap()))
        .unwrap();
    let call_resp = app.oneshot(call_req).await.unwrap();
    let call_bytes = axum::body::to_bytes(call_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let call_json: serde_json::Value = serde_json::from_slice(&call_bytes).unwrap();
    assert_eq!(
        call_json["result"]["structuredContent"],
        serde_json::json!({
            "tool": "structured",
            "ok": true,
            "count": 2
        })
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn test_http_resource_link_passes_through_end_to_end() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("resource_link"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let state = Arc::new(HttpState {
        router: engine.tool_router().clone(),
        sessions: Arc::new(SessionManager::new(1800, 100)) as Arc<dyn SessionStore>,
        cancel: CancellationToken::new(),
        auth_mode: plug_core::config::DownstreamAuthMode::Auto,
        downstream_oauth: None,
        sse_channel_capacity: 32,
        allowed_origins: Vec::new(),
        notification_task_started: std::sync::atomic::AtomicBool::new(false),
        auth_token: None,
        roots_capable_sessions: dashmap::DashMap::new(),
        pending_client_requests: dashmap::DashMap::new(),
        reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
        client_capabilities: dashmap::DashMap::new(),
    });
    let app = build_router(state.clone());

    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "http-resource-link", "version": "1.0" }
        }
    });
    let initialize_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&initialize_body).unwrap()))
        .unwrap();
    let initialize_resp = app.clone().oneshot(initialize_req).await.unwrap();
    let session_id = initialize_resp
        .headers()
        .get(HTTP_SESSION_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let call_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "Mock__resource_link"
        }
    });
    let call_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
        .body(Body::from(serde_json::to_vec(&call_body).unwrap()))
        .unwrap();
    let call_resp = app.oneshot(call_req).await.unwrap();
    let call_bytes = axum::body::to_bytes(call_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let call_json: serde_json::Value = serde_json::from_slice(&call_bytes).unwrap();
    assert_eq!(call_json["result"]["content"][0]["type"], "resource_link");
    assert_eq!(
        call_json["result"]["content"][0]["uri"],
        "file:///tmp/mock-resource.txt"
    );
    assert_eq!(
        call_json["result"]["content"][0]["name"],
        "mock-resource.txt"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn test_http_oversized_tool_result_spills_to_artifact_link() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("artifact_text"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let state = Arc::new(HttpState {
        router: engine.tool_router().clone(),
        sessions: Arc::new(SessionManager::new(1800, 100)) as Arc<dyn SessionStore>,
        cancel: CancellationToken::new(),
        auth_mode: plug_core::config::DownstreamAuthMode::Auto,
        downstream_oauth: None,
        sse_channel_capacity: 32,
        allowed_origins: Vec::new(),
        notification_task_started: std::sync::atomic::AtomicBool::new(false),
        auth_token: None,
        roots_capable_sessions: dashmap::DashMap::new(),
        pending_client_requests: dashmap::DashMap::new(),
        reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
        client_capabilities: dashmap::DashMap::new(),
    });
    let app = build_router(state.clone());

    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "http-artifact", "version": "1.0" }
        }
    });
    let initialize_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&initialize_body).unwrap()))
        .unwrap();
    let initialize_resp = app.clone().oneshot(initialize_req).await.unwrap();
    let session_id = initialize_resp
        .headers()
        .get(HTTP_SESSION_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let call_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "Mock__artifact_text"
        }
    });
    let call_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
        .body(Body::from(serde_json::to_vec(&call_body).unwrap()))
        .unwrap();
    let call_resp = app.oneshot(call_req).await.unwrap();
    let call_bytes = axum::body::to_bytes(call_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let call_json: serde_json::Value = serde_json::from_slice(&call_bytes).unwrap();
    assert_eq!(call_json["result"]["isError"], false);
    assert_eq!(call_json["result"]["content"][1]["type"], "resource_link");
    assert!(
        call_json["result"]["content"][1]["uri"]
            .as_str()
            .expect("artifact uri")
            .starts_with("plug://artifact/")
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn test_multi_client_shared_engine_isolation() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("echo"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    async fn connect_client(
        router: Arc<plug_core::proxy::ToolRouter>,
    ) -> rmcp::service::RunningService<rmcp::RoleClient, TestClient> {
        let proxy_handler = ProxyHandler::from_router(router);
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy_handler
                .serve(server_transport)
                .await
                .expect("start proxy server");
            let _ = server.waiting().await;
        });
        TestClient
            .serve(client_transport)
            .await
            .expect("connect client")
    }

    let client_a = connect_client(engine.tool_router().clone()).await;
    let client_b = connect_client(engine.tool_router().clone()).await;

    let call_a = tokio::spawn(async move {
        client_a
            .call_tool(
                CallToolRequestParams::new("Mock__echo").with_arguments(
                    serde_json::json!({"input": "from-a"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("client a call")
    });
    let call_b = tokio::spawn(async move {
        client_b
            .call_tool(
                CallToolRequestParams::new("Mock__echo").with_arguments(
                    serde_json::json!({"input": "from-b"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("client b call")
    });

    let result_a = call_a.await.unwrap();
    let result_b = call_b.await.unwrap();
    let text_a = format!("{result_a:?}");
    let text_b = format!("{result_b:?}");
    assert!(
        text_a.contains("from-a"),
        "client a got wrong result: {text_a}"
    );
    assert!(
        text_b.contains("from-b"),
        "client b got wrong result: {text_b}"
    );

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Confidence test: rmcp injects MCP-Protocol-Version on upstream HTTP requests
// ---------------------------------------------------------------------------
//
// This test does NOT validate plug code — it confirms that rmcp's
// StreamableHttpClientTransport automatically injects the
// MCP-Protocol-Version header after initialization, which plug relies on
// through its single upstream HTTP code path in server/mod.rs.

#[tokio::test]
async fn test_upstream_http_sends_protocol_version_header() {
    /// Captured method name + optional MCP-Protocol-Version + Authorization header value.
    type CapturedHeaders = Vec<(String, Option<String>, Option<String>)>;

    #[derive(Clone)]
    struct MockState {
        captured: Arc<Mutex<CapturedHeaders>>,
    }

    async fn mock_mcp_handler(
        axum::extract::State(state): axum::extract::State<MockState>,
        headers: axum::http::HeaderMap,
        body: axum::body::Bytes,
    ) -> axum::response::Response {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        let protocol_version = headers
            .get("mcp-protocol-version")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let authorization = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let json_body: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };

        let method = json_body
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();

        state
            .captured
            .lock()
            .await
            .push((method.clone(), protocol_version, authorization));

        let session_headers = [
            (
                axum::http::HeaderName::from_static("mcp-session-id"),
                "test-session",
            ),
            (axum::http::header::CONTENT_TYPE, "application/json"),
        ];

        if method == "initialize" {
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {
                        "tools": { "listChanged": false }
                    },
                    "serverInfo": {
                        "name": "mock-http-server",
                        "version": "0.1.0"
                    }
                }
            });
            return (StatusCode::OK, session_headers, resp.to_string()).into_response();
        }

        if method == "notifications/initialized" {
            return (StatusCode::ACCEPTED, session_headers, String::new()).into_response();
        }

        if method == "tools/list" {
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": { "tools": [] }
            });
            return (StatusCode::OK, session_headers, resp.to_string()).into_response();
        }

        // Default: return empty success
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": json_body.get("id"),
            "result": {}
        });
        (StatusCode::OK, session_headers, resp.to_string()).into_response()
    }

    let mock_state = MockState {
        captured: Arc::new(Mutex::new(Vec::new())),
    };

    let app = axum::Router::new()
        .route("/mcp", axum::routing::post(mock_mcp_handler))
        .with_state(mock_state.clone());

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind mock server");
    let port = listener.local_addr().unwrap().port();

    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    // Give the mock server a moment to start.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let server_name = format!("mock-http-oauth-{}", std::process::id());
    let store = oauth::get_or_create_store(&server_name);
    store.clear().await.expect("clear OAuth store before test");
    plug_core::tls::ensure_rustls_provider_installed();
    store
        .save(oauth_test_credentials(
            "oauth-access-token",
            "oauth-refresh-token",
        ))
        .await
        .expect("seed OAuth credentials");

    // Connect through plug's upstream HTTP path (ServerManager::start_server).
    let sm = Arc::new(ServerManager::new());
    let config = ServerConfig {
        command: None,
        args: Vec::new(),
        env: HashMap::new(),
        enabled: true,
        transport: TransportType::Http,
        url: Some(format!("http://127.0.0.1:{port}/mcp")),
        auth_token: None,
        auth: Some("oauth".to_string()),
        oauth_client_id: Some("test-client".to_string()),
        oauth_scopes: None,
        timeout_secs: 10,
        call_timeout_secs: 5,
        max_concurrent: 4,
        health_check_interval_secs: 60,
        circuit_breaker_enabled: false,
        enrichment: false,
        tool_renames: HashMap::new(),
        tool_groups: Vec::new(),
    };

    let upstream = sm
        .start_server(&server_name, &config)
        .await
        .expect("connect to mock HTTP upstream");

    // start_server does: initialize → notifications/initialized → tools/list.
    // After it returns, all three requests have been made.

    let captured = mock_state.captured.lock().await;

    // initialize: should NOT have MCP-Protocol-Version (version unknown yet)
    let init = captured.iter().find(|(m, _, _)| m == "initialize");
    assert!(init.is_some(), "should have captured initialize request");
    assert_eq!(
        init.unwrap().1,
        None,
        "initialize must not send MCP-Protocol-Version (version not yet negotiated)"
    );
    assert_eq!(
        init.unwrap().2.as_deref(),
        Some("Bearer oauth-access-token"),
        "initialize should send a single Bearer auth header"
    );

    // notifications/initialized: SHOULD have MCP-Protocol-Version
    let initialized = captured
        .iter()
        .find(|(m, _, _)| m == "notifications/initialized");
    assert!(
        initialized.is_some(),
        "should have captured notifications/initialized"
    );
    assert_eq!(
        initialized.unwrap().1,
        Some("2025-11-25".to_string()),
        "notifications/initialized must include MCP-Protocol-Version from server's InitializeResult"
    );

    // tools/list: SHOULD have MCP-Protocol-Version
    let tools_list = captured.iter().find(|(m, _, _)| m == "tools/list");
    assert!(
        tools_list.is_some(),
        "should have captured tools/list request"
    );
    assert_eq!(
        tools_list.unwrap().1,
        Some("2025-11-25".to_string()),
        "tools/list must include MCP-Protocol-Version"
    );

    drop(upstream);
    store.clear().await.expect("clear OAuth store after test");
    server_handle.abort();
}

#[tokio::test]
async fn test_oauth_auth_code_exchange_persists_credentials() {
    let _guard = oauth_integration_test_lock().lock().await;
    let provider = MockOAuthProvider::start().await;
    let server_name = format!("oauth-code-{}", std::process::id());
    let store = oauth::get_or_create_store(&server_name);
    store.clear().await.expect("clear OAuth store before test");
    plug_core::tls::ensure_rustls_provider_installed();
    let result = AssertUnwindSafe(async {
        use rmcp::transport::auth::{AuthorizationManager, OAuthClientConfig};

        let mut auth_manager = AuthorizationManager::new(provider.mcp_url().as_str())
            .await
            .expect("create auth manager");
        auth_manager.set_credential_store(plug_core::oauth::CompositeCredentialStore::new(
            server_name.clone(),
        ));
        auth_manager.set_state_store(plug_core::oauth::CompositeStateStore::new(
            server_name.clone(),
        ));
        let metadata = auth_manager
            .discover_metadata()
            .await
            .expect("discover metadata");
        auth_manager.set_metadata(metadata);
        auth_manager
            .configure_client(OAuthClientConfig {
                client_id: "test-client".to_string(),
                client_secret: None,
                scopes: vec!["read".to_string()],
                redirect_uri: "http://localhost:0/callback".to_string(),
            })
            .expect("configure oauth client");

        let authorize_url = auth_manager
            .get_authorization_url(&["read"])
            .await
            .expect("authorization url");
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client");
        let response = client
            .get(authorize_url)
            .send()
            .await
            .expect("authorize request");
        let redirect = reqwest::Url::parse(
            response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .expect("redirect header"),
        )
        .expect("parse redirect url");
        let code = redirect
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .expect("authorization code");
        let state = redirect
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .expect("csrf state");
        let state_path = oauth_state_file_path(&server_name, &state);
        assert!(
            state_path.exists(),
            "state file should be persisted before token exchange"
        );

        auth_manager
            .exchange_code_for_token(&code, &state)
            .await
            .expect("exchange code");

        assert!(
            !state_path.exists(),
            "state file should be removed after token exchange"
        );

        let stored = store
            .load()
            .await
            .expect("load credentials")
            .expect("stored credentials");
        let token = stored.token_response.expect("token response");
        assert_eq!(token.access_token().secret(), "access-token-1");
        assert_eq!(
            token.refresh_token().expect("refresh token").secret(),
            "refresh-token-1"
        );

        let snapshot = provider.snapshot().await;
        assert_eq!(snapshot.metadata_requests, 1);
        assert_eq!(snapshot.authorize_requests, 1);
        assert_eq!(
            snapshot.token_grants,
            vec!["authorization_code".to_string()]
        );
        assert!(snapshot.pkce_verified);
    })
    .catch_unwind()
    .await;

    store.clear().await.expect("clear OAuth store after test");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test]
async fn test_oauth_refresh_persists_credentials_and_reconnects_with_fresh_token() {
    let _guard = oauth_integration_test_lock().lock().await;
    let provider = MockOAuthProvider::start().await;
    let server_name = format!("oauth-refresh-{}", std::process::id());
    let store = oauth::get_or_create_store(&server_name);
    store.clear().await.expect("clear OAuth store before test");
    plug_core::tls::ensure_rustls_provider_installed();
    store
        .save(oauth_test_credentials("access-token-1", "refresh-token-1"))
        .await
        .expect("seed oauth credentials");
    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let mut config = Config::default();
        config.servers.insert(
            server_name.clone(),
            ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some(provider.mcp_url()),
                auth_token: None,
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                timeout_secs: 10,
                call_timeout_secs: 5,
                max_concurrent: 4,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: false,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );

        let started_engine = Arc::new(Engine::new(config));
        started_engine.start().await.expect("engine start");
        engine = Some(Arc::clone(&started_engine));

        let startup_snapshot = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = provider.snapshot().await;
                if !snapshot.mcp_auth_headers.is_empty() {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("startup should reach the OAuth MCP endpoint");
        assert!(
            startup_snapshot
                .mcp_auth_headers
                .iter()
                .any(|header| header == "Bearer access-token-1"),
            "startup should use the seeded access token"
        );

        let refresh =
            oauth::refresh_access_token(&server_name, &provider.mcp_url(), Some("test-client"))
                .await;
        assert!(
            matches!(refresh, plug_core::oauth::RefreshResult::Refreshed),
            "expected refresh success, got {refresh:?}"
        );

        started_engine
            .reconnect_server(&server_name)
            .await
            .expect("reconnect with refreshed token");

        let stored = store
            .load()
            .await
            .expect("load refreshed credentials")
            .expect("stored credentials");
        let token = stored.token_response.expect("token response");
        assert_eq!(token.access_token().secret(), "access-token-2");
        assert_eq!(
            token.refresh_token().expect("refresh token").secret(),
            "refresh-token-2"
        );

        let snapshot = provider.snapshot().await;
        assert!(
            snapshot
                .token_grants
                .iter()
                .any(|grant| grant == "refresh_token"),
            "refresh flow should use refresh_token grant"
        );
        assert!(
            snapshot
                .mcp_auth_headers
                .iter()
                .any(|header| header == "Bearer access-token-2"),
            "reconnect should use the refreshed access token"
        );
    })
    .catch_unwind()
    .await;

    if let Some(engine) = engine {
        tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
            .await
            .expect("engine shutdown timed out");
    }
    store.clear().await.expect("clear OAuth store after test");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test]
async fn test_engine_mixed_auth_fleet_reports_distinct_server_states() {
    let provider = MockOAuthProvider::start().await;
    let oauth_healthy = format!("oauth-healthy-{}", std::process::id());
    let oauth_auth_required = format!("oauth-required-{}", std::process::id());
    let healthy_store = oauth::get_or_create_store(&oauth_healthy);
    let required_store = oauth::get_or_create_store(&oauth_auth_required);
    healthy_store
        .clear()
        .await
        .expect("clear healthy OAuth store before test");
    required_store
        .clear()
        .await
        .expect("clear required OAuth store before test");
    healthy_store
        .save(oauth_test_credentials("access-token-1", "refresh-token-1"))
        .await
        .expect("seed healthy oauth credentials");

    let failed_port = reserve_unused_local_port().await;
    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let mut config = Config::default();
        config
            .servers
            .insert("stdio".to_string(), mock_server_config("echo"));
        config.servers.insert(
            oauth_healthy.clone(),
            ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some(provider.mcp_url()),
                auth_token: None,
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                timeout_secs: 10,
                call_timeout_secs: 5,
                max_concurrent: 4,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: false,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );
        config.servers.insert(
            oauth_auth_required.clone(),
            ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some(provider.mcp_url()),
                auth_token: None,
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                timeout_secs: 10,
                call_timeout_secs: 5,
                max_concurrent: 4,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: false,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );
        config.servers.insert(
            "failed".to_string(),
            ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some(format!("http://127.0.0.1:{failed_port}/mcp")),
                auth_token: None,
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 1,
                call_timeout_secs: 1,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: false,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );

        let started_engine = Arc::new(Engine::new(config));
        started_engine.start().await.expect("engine start");
        engine = Some(Arc::clone(&started_engine));

        let statuses = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let statuses = started_engine.server_statuses();
                let status_map: HashMap<String, plug_core::types::ServerStatus> = statuses
                    .iter()
                    .cloned()
                    .map(|status| (status.server_id.clone(), status))
                    .collect();
                let healthy_ready = status_map
                    .get(&oauth_healthy)
                    .is_some_and(|s| s.auth_status == "oauth" && s.health == plug_core::types::ServerHealth::Healthy);
                let required_ready = status_map
                    .get(&oauth_auth_required)
                    .is_some_and(|s| s.auth_status == "auth-required" && s.health == plug_core::types::ServerHealth::AuthRequired);
                let stdio_ready = status_map
                    .get("stdio")
                    .is_some_and(|s| s.health == plug_core::types::ServerHealth::Healthy && s.tool_count == 1);
                if healthy_ready && required_ready && stdio_ready {
                    break statuses;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("engine status snapshot should converge");
        let status_map: HashMap<String, plug_core::types::ServerStatus> = statuses
            .into_iter()
            .map(|status| (status.server_id.clone(), status))
            .collect();

        assert_eq!(
            status_map.get("stdio").map(|s| s.health),
            Some(plug_core::types::ServerHealth::Healthy)
        );
        assert_eq!(
            status_map.get("stdio").map(|s| s.tool_count),
            Some(1),
            "stdio mock should contribute its echo tool"
        );
        assert_eq!(
            status_map
                .get(&oauth_healthy)
                .map(|s| s.auth_status.as_str()),
            Some("oauth")
        );
        assert_eq!(
            status_map.get(&oauth_healthy).map(|s| s.health),
            Some(plug_core::types::ServerHealth::Healthy)
        );
        assert_eq!(
            status_map
                .get(&oauth_auth_required)
                .map(|s| s.auth_status.as_str()),
            Some("auth-required")
        );
        assert_eq!(
            status_map.get(&oauth_auth_required).map(|s| s.health),
            Some(plug_core::types::ServerHealth::AuthRequired)
        );
        assert_eq!(
            status_map.get("failed").map(|s| s.health),
            Some(plug_core::types::ServerHealth::Failed)
        );
    })
    .catch_unwind()
    .await;

    if let Some(engine) = engine {
        tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
            .await
            .expect("engine shutdown timed out");
    }
    healthy_store
        .clear()
        .await
        .expect("clear healthy OAuth store after test");
    required_store
        .clear()
        .await
        .expect("clear required OAuth store after test");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test]
async fn test_oauth_stateless_http_server_with_valid_credentials_starts_healthy() {
    let _guard = oauth_integration_test_lock().lock().await;
    let provider = MockStatelessOauthProvider::start().await;
    let server_name = format!("oauth-stateless-{}", std::process::id());
    let store = oauth::get_or_create_store(&server_name);
    store
        .clear()
        .await
        .expect("clear OAuth store before test");
    store
        .save(oauth_test_credentials("access-token-1", "refresh-token-1"))
        .await
        .expect("seed oauth credentials");

    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let mut config = Config::default();
        config.servers.insert(
            server_name.clone(),
            ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some(provider.mcp_url()),
                auth_token: None,
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                timeout_secs: 10,
                call_timeout_secs: 5,
                max_concurrent: 4,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: false,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );

        let started_engine = Arc::new(Engine::new(config));
        started_engine.start().await.expect("engine start");
        engine = Some(Arc::clone(&started_engine));

        let statuses = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let statuses = started_engine.server_statuses();
                if statuses.iter().any(|status| status.server_id == server_name) {
                    break statuses;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("engine status snapshot should converge");

        let status = statuses
            .into_iter()
            .find(|status| status.server_id == server_name)
            .expect("server status present");
        assert_eq!(status.health, plug_core::types::ServerHealth::Healthy);
        assert_eq!(status.auth_status, "oauth");
        assert_eq!(status.tool_count, 1, "stateless upstream should expose tools");

        let snapshot = provider.shared.lock().await;
        assert!(
            snapshot
                .mcp_auth_headers
                .iter()
                .all(|header| header == "Bearer access-token-1"),
            "expected upstream requests to use the saved bearer token once"
        );
    })
    .catch_unwind()
    .await;

    if let Some(engine) = engine {
        tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
            .await
            .expect("engine shutdown timed out");
    }
    store.clear().await.expect("clear OAuth store after test");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test]
async fn test_oauth_startup_failure_with_valid_credentials_is_not_auth_required() {
    let _guard = oauth_integration_test_lock().lock().await;
    let provider = MockToolListFailureOauthProvider::start().await;
    let server_name = format!("oauth-startup-failure-{}", std::process::id());
    let store = oauth::get_or_create_store(&server_name);
    store
        .clear()
        .await
        .expect("clear OAuth store before test");
    store
        .save(oauth_test_credentials("access-token-1", "refresh-token-1"))
        .await
        .expect("seed oauth credentials");

    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let mut config = Config::default();
        config.servers.insert(
            server_name.clone(),
            ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some(provider.mcp_url()),
                auth_token: None,
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                timeout_secs: 10,
                call_timeout_secs: 5,
                max_concurrent: 4,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: false,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );

        let started_engine = Arc::new(Engine::new(config));
        started_engine.start().await.expect("engine start");
        engine = Some(Arc::clone(&started_engine));

        let status = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = started_engine
                    .server_statuses()
                    .into_iter()
                    .find(|status| status.server_id == server_name)
                {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("engine status snapshot should converge");

        assert_eq!(
            status.health,
            plug_core::types::ServerHealth::Failed,
            "valid saved credentials plus a non-auth startup failure should not be classified as re-auth required"
        );
        assert_eq!(status.auth_status, "oauth");
        assert_eq!(status.tool_count, 0);
    })
    .catch_unwind()
    .await;

    if let Some(engine) = engine {
        tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
            .await
            .expect("engine shutdown timed out");
    }
    store.clear().await.expect("clear OAuth store after test");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test]
async fn test_oauth_server_can_start_when_initialized_notification_is_rejected() {
    let _guard = oauth_integration_test_lock().lock().await;
    let provider = MockInitializedNotificationFailureOauthProvider::start().await;
    let server_name = format!(
        "oauth-init-notif-failure-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let store = oauth::get_or_create_store(&server_name);
    store
        .clear()
        .await
        .expect("clear OAuth store before test");
    store
        .save(oauth_test_credentials("access-token-1", "refresh-token-1"))
        .await
        .expect("seed oauth credentials");

    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let mut config = Config::default();
        config.servers.insert(
            server_name.clone(),
            ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some(provider.mcp_url()),
                auth_token: None,
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                timeout_secs: 10,
                call_timeout_secs: 5,
                max_concurrent: 4,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: false,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );

        let started_engine = Arc::new(Engine::new(config));
        started_engine.start().await.expect("engine start");
        engine = Some(Arc::clone(&started_engine));

        let status = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = started_engine
                    .server_statuses()
                    .into_iter()
                    .find(|status| status.server_id == server_name)
                {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("engine status snapshot should converge");

        assert_eq!(status.health, plug_core::types::ServerHealth::Healthy);
        assert_eq!(status.auth_status, "oauth");
        assert_eq!(status.tool_count, 1);

        let snapshot = provider.shared.lock().await;
        assert!(
            snapshot
                .mcp_auth_headers
                .iter()
                .all(|header| header == "Bearer access-token-1"),
            "expected startup requests to continue using the valid bearer token"
        );
        assert_eq!(
            snapshot.mcp_methods,
            vec![
                "initialize".to_string(),
                "notifications/initialized".to_string(),
                "tools/list".to_string(),
            ],
            "expected startup to exercise initialize, the initialized notification, then tools/list"
        );
    })
    .catch_unwind()
    .await;

    if let Some(engine) = engine {
        tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
            .await
            .expect("engine shutdown timed out");
    }
    store.clear().await.expect("clear OAuth store after test");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test]
async fn test_oauth_server_does_not_start_when_initialized_notification_is_auth_rejected() {
    let _guard = oauth_integration_test_lock().lock().await;
    let provider = MockInitializedNotificationAuthFailureOauthProvider::start().await;
    let server_name = format!(
        "oauth-init-notif-auth-failure-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let store = oauth::get_or_create_store(&server_name);
    store
        .clear()
        .await
        .expect("clear OAuth store before test");
    store
        .save(oauth_test_credentials("access-token-1", "refresh-token-1"))
        .await
        .expect("seed oauth credentials");

    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let mut config = Config::default();
        config.servers.insert(
            server_name.clone(),
            ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some(provider.mcp_url()),
                auth_token: None,
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                timeout_secs: 10,
                call_timeout_secs: 5,
                max_concurrent: 4,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: false,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );

        let started_engine = Arc::new(Engine::new(config));
        started_engine.start().await.expect("engine start");
        engine = Some(Arc::clone(&started_engine));

        let status = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = started_engine
                    .server_statuses()
                    .into_iter()
                    .find(|status| status.server_id == server_name)
                {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("engine status snapshot should converge");

        assert_eq!(status.health, plug_core::types::ServerHealth::Failed);
        assert_eq!(status.auth_status, "oauth");
        assert_eq!(status.tool_count, 0);

        let snapshot = provider.shared.lock().await;
        assert_eq!(
            snapshot.mcp_methods,
            vec![
                "initialize".to_string(),
                "notifications/initialized".to_string(),
            ],
            "auth rejection on initialized notification should abort before tools/list"
        );
    })
    .catch_unwind()
    .await;

    if let Some(engine) = engine {
        tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
            .await
            .expect("engine shutdown timed out");
    }
    store.clear().await.expect("clear OAuth store after test");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test]
async fn test_oauth_server_does_not_start_when_initialized_notification_returns_server_error() {
    let _guard = oauth_integration_test_lock().lock().await;
    let provider = MockInitializedNotificationServerErrorOauthProvider::start().await;
    let server_name = format!(
        "oauth-init-notif-server-error-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let store = oauth::get_or_create_store(&server_name);
    store
        .clear()
        .await
        .expect("clear OAuth store before test");
    store
        .save(oauth_test_credentials("access-token-1", "refresh-token-1"))
        .await
        .expect("seed oauth credentials");

    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let mut config = Config::default();
        config.servers.insert(
            server_name.clone(),
            ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Http,
                url: Some(provider.mcp_url()),
                auth_token: None,
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                timeout_secs: 10,
                call_timeout_secs: 5,
                max_concurrent: 4,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: false,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );

        let started_engine = Arc::new(Engine::new(config));
        started_engine.start().await.expect("engine start");
        engine = Some(Arc::clone(&started_engine));

        let status = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = started_engine
                    .server_statuses()
                    .into_iter()
                    .find(|status| status.server_id == server_name)
                {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("engine status snapshot should converge");

        assert_eq!(status.health, plug_core::types::ServerHealth::Failed);
        assert_eq!(status.auth_status, "oauth");
        assert_eq!(status.tool_count, 0);

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let methods = {
                    let snapshot = provider.shared.lock().await;
                    snapshot.mcp_methods.clone()
                };
                if methods
                    == vec![
                        "initialize".to_string(),
                        "notifications/initialized".to_string(),
                    ]
                {
                    break methods;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("initialized notification request sequence should converge");

        let snapshot = provider.shared.lock().await;
        assert_eq!(
            snapshot.mcp_methods,
            vec![
                "initialize".to_string(),
                "notifications/initialized".to_string(),
            ],
            "server error on initialized notification should abort before tools/list"
        );
    })
    .catch_unwind()
    .await;

    if let Some(engine) = engine {
        tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
            .await
            .expect("engine shutdown timed out");
    }
    store.clear().await.expect("clear OAuth store after test");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test]
async fn test_downstream_oauth_protected_discovery_card_end_to_end() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server_config("echo"));

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let oauth_config = plug_core::downstream_oauth::DownstreamOauthConfig {
        public_base_url: "https://plug.example.com".to_string(),
        oauth_client_id: Some("client-123".to_string()),
        oauth_client_secret: None,
        oauth_scopes: vec!["tools:read".to_string()],
    };
    let manager = plug_core::downstream_oauth::DownstreamOauthManager::new(oauth_config);
    let app = build_router(Arc::new(HttpState {
        router: engine.tool_router().clone(),
        sessions: Arc::new(SessionManager::new(1800, 100)) as Arc<dyn SessionStore>,
        cancel: CancellationToken::new(),
        auth_mode: plug_core::config::DownstreamAuthMode::Oauth,
        downstream_oauth: Some(manager),
        sse_channel_capacity: 32,
        allowed_origins: Vec::new(),
        notification_task_started: std::sync::atomic::AtomicBool::new(false),
        auth_token: None,
        roots_capable_sessions: dashmap::DashMap::new(),
        pending_client_requests: dashmap::DashMap::new(),
        reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
        client_capabilities: dashmap::DashMap::new(),
    }));

    let unauth_req = HttpRequest::builder()
        .method("GET")
        .uri("/.well-known/mcp.json")
        .body(Body::empty())
        .unwrap();
    let unauth_resp = app.clone().oneshot(unauth_req).await.unwrap();
    assert_eq!(unauth_resp.status(), StatusCode::OK);
    let unauth_body = axum::body::to_bytes(unauth_resp.into_body(), 10_000)
        .await
        .unwrap();
    let unauth_card: serde_json::Value = serde_json::from_slice(&unauth_body).unwrap();
    assert!(unauth_card.get("servers").is_none());
    assert!(unauth_card.get("tools").is_none());
    assert_eq!(unauth_card["auth_required"], true);

    let verifier = oauth2::PkceCodeVerifier::new("v".repeat(43));
    let challenge = oauth2::PkceCodeChallenge::from_code_verifier_sha256(&verifier);
    let authorize_req = HttpRequest::builder()
        .method("GET")
        .uri(format!(
            "/oauth/authorize?response_type=code&client_id=client-123&redirect_uri=http://localhost/callback&state=test-state&code_challenge={}&code_challenge_method=S256",
            challenge.as_str()
        ))
        .body(Body::empty())
        .unwrap();
    let authorize_resp = app.clone().oneshot(authorize_req).await.unwrap();
    assert!(
        authorize_resp.status().is_redirection(),
        "unexpected authorize status {}",
        authorize_resp.status()
    );
    let redirect = authorize_resp
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("redirect location");
    let redirect = reqwest::Url::parse(redirect).expect("parse redirect URL");
    let code = redirect
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .expect("authorization code");

    let token_req = HttpRequest::builder()
        .method("POST")
        .uri("/oauth/token")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!(
            "grant_type=authorization_code&code={code}&client_id=client-123&redirect_uri=http://localhost/callback&code_verifier={}",
            verifier.secret()
        )))
        .unwrap();
    let token_resp = app.clone().oneshot(token_req).await.unwrap();
    assert_eq!(token_resp.status(), StatusCode::OK);
    let token_body = axum::body::to_bytes(token_resp.into_body(), 10_000)
        .await
        .unwrap();
    let token_json: serde_json::Value = serde_json::from_slice(&token_body).unwrap();
    let access_token = token_json["access_token"]
        .as_str()
        .expect("downstream access token");

    let auth_req = HttpRequest::builder()
        .method("GET")
        .uri("/.well-known/mcp.json")
        .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
        .body(Body::empty())
        .unwrap();
    let auth_resp = app.clone().oneshot(auth_req).await.unwrap();
    assert_eq!(auth_resp.status(), StatusCode::OK);
    let auth_body = axum::body::to_bytes(auth_resp.into_body(), 10_000)
        .await
        .unwrap();
    let auth_card: serde_json::Value = serde_json::from_slice(&auth_body).unwrap();
    assert!(auth_card.get("servers").is_some());
    assert!(auth_card.get("tools").is_some());

    let mcp_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
        .body(Body::from(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": HTTP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "oauth-discovery-e2e", "version": "1.0" }
                }
            })
            .to_string(),
        ))
        .unwrap();
    let mcp_resp = app.oneshot(mcp_req).await.unwrap();
    assert_eq!(mcp_resp.status(), StatusCode::OK);

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Reverse-request test infrastructure
// ---------------------------------------------------------------------------

/// A test client that advertises elicitation + sampling capabilities
/// and handles reverse requests from upstream servers.
#[derive(Clone)]
struct ReverseRequestTestClient;

#[allow(clippy::manual_async_fn)]
impl ClientHandler for ReverseRequestTestClient {
    fn get_info(&self) -> ClientInfo {
        let mut caps = ClientCapabilities::default();
        caps.sampling = Some(SamplingCapability::default());
        caps.elicitation = Some(ElicitationCapability {
            form: Some(FormElicitationCapability::default()),
            url: Some(UrlElicitationCapability {}),
        });
        ClientInfo::new(caps, Implementation::new("reverse-test-client", "1.0"))
    }

    fn create_elicitation(
        &self,
        _request: CreateElicitationRequestParams,
        _context: RequestContext<rmcp::RoleClient>,
    ) -> impl Future<Output = Result<CreateElicitationResult, McpError>> + Send + '_ {
        async {
            Ok(CreateElicitationResult::new(ElicitationAction::Accept)
                .with_content(serde_json::json!({"answer": "test-elicitation-response"})))
        }
    }

    fn create_message(
        &self,
        _request: CreateMessageRequestParams,
        _context: RequestContext<rmcp::RoleClient>,
    ) -> impl Future<Output = Result<CreateMessageResult, McpError>> + Send + '_ {
        async {
            Ok(CreateMessageResult::new(
                SamplingMessage::assistant_text("test-sampling-response"),
                "mock-model".to_string(),
            ))
        }
    }
}

fn mock_server_config_with_reverse_request(tools: &str, reverse_request: &str) -> ServerConfig {
    ServerConfig {
        command: Some("cargo".to_string()),
        args: vec![
            "run".to_string(),
            "--quiet".to_string(),
            "-p".to_string(),
            "plug-test-harness".to_string(),
            "--bin".to_string(),
            "mock-mcp-server".to_string(),
            "--".to_string(),
            "--tools".to_string(),
            tools.to_string(),
            "--reverse-request".to_string(),
            reverse_request.to_string(),
        ],
        env: HashMap::new(),
        enabled: true,
        transport: TransportType::Stdio,
        url: None,
        auth_token: None,
        auth: None,
        oauth_client_id: None,
        oauth_scopes: None,
        timeout_secs: 30,
        call_timeout_secs: 30,
        max_concurrent: 4,
        health_check_interval_secs: 60,
        circuit_breaker_enabled: true,
        enrichment: false,
        tool_renames: HashMap::new(),
        tool_groups: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Stdio: elicitation reverse-request round trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stdio_elicitation_reverse_request_round_trip() {
    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        mock_server_config_with_reverse_request("echo", "elicitation"),
    );

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let proxy_handler = ProxyHandler::from_router(engine.tool_router().clone());
    let (server_transport, client_transport) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        let server = proxy_handler
            .serve(server_transport)
            .await
            .expect("start stdio proxy server");
        let _ = server.waiting().await;
    });

    let client = ReverseRequestTestClient
        .serve(client_transport)
        .await
        .expect("connect stdio client");

    // Verify tools are available
    let tools = client.peer().list_all_tools().await.expect("list tools");
    let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    assert!(
        names.contains(&"Mock__echo".to_string()),
        "tools: {names:?}"
    );

    // Call a tool — the mock server will send an elicitation reverse request
    let result = client
        .call_tool(
            CallToolRequestParams::new("Mock__echo").with_arguments(
                serde_json::json!({"input": "test"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("call tool with elicitation reverse request");

    let rendered = format!("{result:?}");
    assert!(
        rendered.contains("reverse=elicitation:Accept"),
        "expected elicitation:Accept in result, got: {rendered}"
    );

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// Stdio: sampling reverse-request round trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stdio_sampling_reverse_request_round_trip() {
    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        mock_server_config_with_reverse_request("echo", "sampling"),
    );

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let proxy_handler = ProxyHandler::from_router(engine.tool_router().clone());
    let (server_transport, client_transport) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        let server = proxy_handler
            .serve(server_transport)
            .await
            .expect("start stdio proxy server");
        let _ = server.waiting().await;
    });

    let client = ReverseRequestTestClient
        .serve(client_transport)
        .await
        .expect("connect stdio client");

    let tools = client.peer().list_all_tools().await.expect("list tools");
    assert!(
        tools.iter().any(|t| t.name == "Mock__echo"),
        "mock tool not found"
    );

    let result = client
        .call_tool(
            CallToolRequestParams::new("Mock__echo").with_arguments(
                serde_json::json!({"input": "test"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("call tool with sampling reverse request");

    let rendered = format!("{result:?}");
    assert!(
        rendered.contains("reverse=sampling:model=mock-model"),
        "expected sampling:model=mock-model in result, got: {rendered}"
    );

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// HTTP: elicitation reverse-request round trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_elicitation_reverse_request_round_trip() {
    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        mock_server_config_with_reverse_request("echo", "elicitation"),
    );

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let state = Arc::new(HttpState {
        router: engine.tool_router().clone(),
        sessions: Arc::new(SessionManager::new(1800, 100)) as Arc<dyn SessionStore>,
        cancel: CancellationToken::new(),
        auth_mode: plug_core::config::DownstreamAuthMode::Auto,
        downstream_oauth: None,
        sse_channel_capacity: 32,
        allowed_origins: Vec::new(),
        notification_task_started: std::sync::atomic::AtomicBool::new(false),
        auth_token: None,
        roots_capable_sessions: dashmap::DashMap::new(),
        pending_client_requests: dashmap::DashMap::new(),
        reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
        client_capabilities: dashmap::DashMap::new(),
    });
    let app = build_router(state.clone());

    // 1. Initialize with elicitation + sampling capabilities
    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {
                "sampling": {},
                "elicitation": {
                    "form": {},
                    "url": {}
                }
            },
            "clientInfo": { "name": "reverse-test-http", "version": "1.0" }
        }
    });
    let initialize_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&initialize_body).unwrap()))
        .unwrap();
    let initialize_resp = app.clone().oneshot(initialize_req).await.unwrap();
    let session_id = initialize_resp
        .headers()
        .get(HTTP_SESSION_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // 2. Send initialized notification (triggers bridge registration)
    let initialized_body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let initialized_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
        .body(Body::from(serde_json::to_vec(&initialized_body).unwrap()))
        .unwrap();
    let _ = app.clone().oneshot(initialized_req).await.unwrap();

    // 3. Open SSE stream
    let sse_req = HttpRequest::builder()
        .method("GET")
        .uri("/mcp")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header("accept", "text/event-stream")
        .body(Body::empty())
        .unwrap();
    let sse_resp = app.clone().oneshot(sse_req).await.unwrap();

    // 4. Spawn tools/call POST in background task
    let app_clone = app.clone();
    let session_id_clone = session_id.clone();
    let call_handle = tokio::spawn(async move {
        // Small delay to let SSE stream establish
        tokio::time::sleep(Duration::from_millis(100)).await;

        let call_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "Mock__echo",
                "arguments": { "input": "http-elicitation-test" }
            }
        });
        let call_req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header(HTTP_SESSION_ID_HEADER, &session_id_clone)
            .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
            .body(Body::from(serde_json::to_vec(&call_body).unwrap()))
            .unwrap();
        let call_resp = app_clone.oneshot(call_req).await.unwrap();
        let call_bytes = axum::body::to_bytes(call_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&call_bytes).unwrap()
    });

    // 5. Read SSE stream for elicitation request
    let mut stream = sse_resp.into_body().into_data_stream();
    use futures::StreamExt;

    let mut elicitation_request_id: Option<serde_json::Value> = None;
    let sse_timeout = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(Ok(chunk)) = stream.next().await {
            let text = String::from_utf8_lossy(&chunk).to_string();
            for line in text.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                        if json.get("method").and_then(|m| m.as_str()) == Some("elicitation/create")
                        {
                            elicitation_request_id = json.get("id").cloned();
                            return;
                        }
                    }
                }
            }
        }
    })
    .await;
    assert!(
        sse_timeout.is_ok(),
        "timed out waiting for elicitation request on SSE stream"
    );
    let request_id = elicitation_request_id.expect("elicitation request id should be captured");

    // 6. POST elicitation response
    let elicitation_response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {
            "action": "accept",
            "content": {"answer": "test-http-elicitation-response"}
        }
    });
    let resp_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
        .body(Body::from(
            serde_json::to_vec(&elicitation_response).unwrap(),
        ))
        .unwrap();
    let _ = app.clone().oneshot(resp_req).await.unwrap();

    // 7. Await tools/call completion
    let call_result = tokio::time::timeout(Duration::from_secs(30), call_handle)
        .await
        .expect("tools/call timed out")
        .expect("tools/call task panicked");

    let response_text = call_result["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        response_text.contains("reverse=elicitation:Accept"),
        "expected elicitation:Accept in HTTP response, got: {response_text}"
    );

    engine.shutdown().await;
}

// ---------------------------------------------------------------------------
// HTTP: sampling reverse-request round trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_http_sampling_reverse_request_round_trip() {
    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        mock_server_config_with_reverse_request("echo", "sampling"),
    );

    let engine = Arc::new(Engine::new(config));
    engine.start().await.expect("engine start");

    let state = Arc::new(HttpState {
        router: engine.tool_router().clone(),
        sessions: Arc::new(SessionManager::new(1800, 100)) as Arc<dyn SessionStore>,
        cancel: CancellationToken::new(),
        auth_mode: plug_core::config::DownstreamAuthMode::Auto,
        downstream_oauth: None,
        sse_channel_capacity: 32,
        allowed_origins: Vec::new(),
        notification_task_started: std::sync::atomic::AtomicBool::new(false),
        auth_token: None,
        roots_capable_sessions: dashmap::DashMap::new(),
        pending_client_requests: dashmap::DashMap::new(),
        reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
        client_capabilities: dashmap::DashMap::new(),
    });
    let app = build_router(state.clone());

    // 1. Initialize with sampling capability
    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {
                "sampling": {},
                "elicitation": {
                    "form": {},
                    "url": {}
                }
            },
            "clientInfo": { "name": "reverse-test-http-sampling", "version": "1.0" }
        }
    });
    let initialize_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&initialize_body).unwrap()))
        .unwrap();
    let initialize_resp = app.clone().oneshot(initialize_req).await.unwrap();
    let session_id = initialize_resp
        .headers()
        .get(HTTP_SESSION_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // 2. Send initialized notification (triggers bridge registration)
    let initialized_body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let initialized_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
        .body(Body::from(serde_json::to_vec(&initialized_body).unwrap()))
        .unwrap();
    let _ = app.clone().oneshot(initialized_req).await.unwrap();

    // 3. Open SSE stream
    let sse_req = HttpRequest::builder()
        .method("GET")
        .uri("/mcp")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header("accept", "text/event-stream")
        .body(Body::empty())
        .unwrap();
    let sse_resp = app.clone().oneshot(sse_req).await.unwrap();

    // 4. Spawn tools/call POST in background task
    let app_clone = app.clone();
    let session_id_clone = session_id.clone();
    let call_handle = tokio::spawn(async move {
        // Small delay to let SSE stream establish
        tokio::time::sleep(Duration::from_millis(100)).await;

        let call_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "Mock__echo",
                "arguments": { "input": "http-sampling-test" }
            }
        });
        let call_req = HttpRequest::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header(HTTP_SESSION_ID_HEADER, &session_id_clone)
            .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
            .body(Body::from(serde_json::to_vec(&call_body).unwrap()))
            .unwrap();
        let call_resp = app_clone.oneshot(call_req).await.unwrap();
        let call_bytes = axum::body::to_bytes(call_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&call_bytes).unwrap()
    });

    // 5. Read SSE stream for sampling request
    let mut stream = sse_resp.into_body().into_data_stream();
    use futures::StreamExt;

    let mut sampling_request_id: Option<serde_json::Value> = None;
    let sse_timeout = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(Ok(chunk)) = stream.next().await {
            let text = String::from_utf8_lossy(&chunk).to_string();
            for line in text.lines() {
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                        if json.get("method").and_then(|m| m.as_str())
                            == Some("sampling/createMessage")
                        {
                            sampling_request_id = json.get("id").cloned();
                            return;
                        }
                    }
                }
            }
        }
    })
    .await;
    assert!(
        sse_timeout.is_ok(),
        "timed out waiting for sampling request on SSE stream"
    );
    let request_id = sampling_request_id.expect("sampling request id should be captured");

    // 6. POST sampling response
    let sampling_response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {
            "role": "assistant",
            "content": { "type": "text", "text": "test-http-sampling-response" },
            "model": "mock-model"
        }
    });
    let resp_req = HttpRequest::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header(HTTP_SESSION_ID_HEADER, &session_id)
        .header(HTTP_PROTOCOL_VERSION_HEADER, HTTP_PROTOCOL_VERSION)
        .body(Body::from(serde_json::to_vec(&sampling_response).unwrap()))
        .unwrap();
    let _ = app.clone().oneshot(resp_req).await.unwrap();

    // 7. Await tools/call completion
    let call_result = tokio::time::timeout(Duration::from_secs(30), call_handle)
        .await
        .expect("tools/call timed out")
        .expect("tools/call task panicked");

    let response_text = call_result["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        response_text.contains("reverse=sampling:model=mock-model"),
        "expected sampling:model=mock-model in HTTP response, got: {response_text}"
    );

    engine.shutdown().await;
}
