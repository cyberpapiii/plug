#![forbid(unsafe_code)]
// Preserve integration coverage for MCP 2025-11-25 features that RMCP 3.1
// deprecates only in anticipation of future SEP-2577.
#![allow(deprecated)]

//! Integration tests for plug-core: engines run against mock MCP servers,
//! spawned as `mock-mcp-server` processes or served in-process by axum, and
//! driven through the dispatcher, the HTTP server and the OAuth flows.

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
use plug_core::config::{
    Config, ServerConfig, TransportType, UpstreamProtocolMode, validate_config,
};
use plug_core::engine::Engine;
use plug_core::http::server::{HttpState, build_router};
use plug_core::oauth;
use plug_core::server::ServerManager;
use plug_core::test_helpers::{collect_sse_events, mock_server};
use rmcp::ErrorData as McpError;
use rmcp::model::{
    CallToolRequestParams, CreateMessageRequestParams, CreateMessageResult, ElicitRequestParams,
    ElicitResult, ElicitationAction, ReadResourceRequestParams, ResourceContents, SamplingMessage,
};
use rmcp::transport::auth::CredentialStore;
use rmcp::transport::auth::{StoredCredentials, VendorExtraTokenFields};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const HTTP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";
const HTTP_PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";
const HTTP_PROTOCOL_VERSION: &str = plug_core::protocol::SUPPORTED_PROTOCOL_VERSION;

/// OAuth tests share one process-wide credential backend and test clock, so
/// they run one at a time.
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

    let received_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs();
    StoredCredentials::new(
        "test-client".to_string(),
        Some(token),
        vec![],
        Some(received_at),
    )
}

async fn save_bound_oauth_test_credentials(
    store: &oauth::CompositeCredentialStore,
    issuer: &str,
    resource: &str,
    access: &str,
    refresh: &str,
) {
    store
        .bind_verified_authority(&oauth::VerifiedOAuthAuthority {
            issuer: reqwest::Url::parse(issuer)
                .expect("valid OAuth test issuer")
                .to_string(),
            resource: reqwest::Url::parse(resource)
                .expect("valid OAuth test resource")
                .to_string(),
        })
        .expect("bind OAuth test authority");
    store
        .save(oauth_test_credentials(access, refresh))
        .await
        .expect("seed bound OAuth credentials");
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
    plug_core::paths::tokens_dir().join(format!("{safe_server}_state_{safe_state}.json"))
}

#[derive(Clone)]
struct MockOAuthProviderState {
    base_url: String,
    shared: Arc<Mutex<MockOAuthProviderShared>>,
    behavior: McpBehavior,
}

/// How the mock's `/mcp` endpoint answers once a request carries the current
/// bearer token.
#[derive(Clone, Copy)]
enum McpBehavior {
    /// A session server answering in JSON, with no tools.
    Session,
    /// A session server whose `tools/list` fails with a 500.
    ToolListFails,
    /// A stateless server answering in SSE with one tool, which answers
    /// `notifications/initialized` with `initialized`.
    Stateless {
        protocol_version: &'static str,
        initialized: StatusCode,
    },
}

impl McpBehavior {
    const STATELESS: Self = Self::Stateless {
        protocol_version: "2025-06-18",
        initialized: StatusCode::ACCEPTED,
    };

    fn stateless_answering_initialized_with(status: StatusCode) -> Self {
        Self::Stateless {
            protocol_version: "2025-11-25",
            initialized: status,
        }
    }
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
    mcp_methods: Vec<String>,
}

/// An OAuth authorization server and the MCP server it protects, on one
/// loopback address.
struct MockOAuthProvider {
    base_url: String,
    shared: Arc<Mutex<MockOAuthProviderShared>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockOAuthProvider {
    async fn start() -> Self {
        Self::start_with(McpBehavior::Session).await
    }

    async fn start_with(behavior: McpBehavior) -> Self {
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
                behavior,
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
            mcp_methods: state.mcp_methods.clone(),
        }
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
    state
        .shared
        .lock()
        .await
        .mcp_methods
        .push(method.to_string());
    let id = json_body.get("id");

    match state.behavior {
        McpBehavior::Session | McpBehavior::ToolListFails => {
            let session_headers = [
                (
                    axum::http::HeaderName::from_static("mcp-session-id"),
                    "test-session",
                ),
                (axum::http::header::CONTENT_TYPE, "application/json"),
            ];
            let tools_fail = matches!(state.behavior, McpBehavior::ToolListFails);
            let payload = match method {
                "initialize" => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "mock-http-server", "version": "0.1.0" }
                    }
                }),
                "notifications/initialized" => {
                    return (StatusCode::ACCEPTED, session_headers, String::new()).into_response();
                }
                "tools/list" if tools_fail => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        session_headers,
                        "backend temporarily unavailable".to_string(),
                    )
                        .into_response();
                }
                "tools/list" => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "tools": [] }
                }),
                _ => {
                    return (StatusCode::BAD_REQUEST, session_headers, String::new())
                        .into_response();
                }
            };
            (StatusCode::OK, session_headers, payload.to_string()).into_response()
        }
        McpBehavior::Stateless {
            protocol_version,
            initialized,
        } => {
            let sse_headers = [(axum::http::header::CONTENT_TYPE, "text/event-stream")];
            let payload = match method {
                "initialize" => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": protocol_version,
                        "capabilities": { "tools": { "listChanged": true } },
                        "serverInfo": { "name": "mock-stateless-http-server", "version": "1.0.0" }
                    }
                }),
                "notifications/initialized" if initialized == StatusCode::ACCEPTED => {
                    return (StatusCode::ACCEPTED, sse_headers, String::new()).into_response();
                }
                "notifications/initialized" => return initialized.into_response(),
                "tools/list" => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": [{
                            "name": "search_meetings",
                            "description": "Search meetings",
                            "inputSchema": { "type": "object", "properties": {} }
                        }]
                    }
                }),
                _ => {
                    return (StatusCode::BAD_REQUEST, sse_headers, String::new()).into_response();
                }
            };
            let event_stream = format!("event: message\ndata: {payload}\n\n");
            (StatusCode::OK, sse_headers, event_stream).into_response()
        }
    }
}

/// A `plug connect` session, asking the dispatcher the way the daemon does.
struct Downstream {
    router: Arc<plug_core::proxy::ToolRouter>,
    session_id: Arc<str>,
    next_request: std::sync::atomic::AtomicI64,
}

impl Downstream {
    fn connect(router: Arc<plug_core::proxy::ToolRouter>) -> Self {
        Self {
            router,
            session_id: Arc::from(uuid::Uuid::new_v4().to_string()),
            next_request: std::sync::atomic::AtomicI64::new(1),
        }
    }

    /// Connect a client that answers Plug's reverse requests with `bridge`.
    fn connect_with_bridge(
        router: Arc<plug_core::proxy::ToolRouter>,
        bridge: Arc<dyn plug_core::proxy::DownstreamBridge>,
    ) -> Self {
        let downstream = Self::connect(router);
        downstream.router.register_downstream_bridge(
            plug_core::notifications::NotificationTarget::Ipc {
                client_id: Arc::clone(&downstream.session_id),
            },
            bridge,
        );
        downstream
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<T, McpError> {
        let request_id = self
            .next_request
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let ctx = plug_core::dispatch::IpcContext::new(
            Arc::clone(&self.session_id),
            rmcp::model::RequestId::Number(request_id),
        );
        let value = plug_core::dispatch::handle(&self.router, &ctx, method, params).await?;
        Ok(serde_json::from_value(value).expect("result has the method's shape"))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
    ) -> Result<rmcp::model::CallToolResult, McpError> {
        self.request("tools/call", Some(serde_json::to_value(params).unwrap()))
            .await
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
    ) -> Result<rmcp::model::ReadResourceResult, McpError> {
        self.request(
            "resources/read",
            Some(serde_json::to_value(params).unwrap()),
        )
        .await
    }

    async fn list_all_tools(&self) -> Result<Vec<rmcp::model::Tool>, McpError> {
        let mut tools = Vec::new();
        let mut cursor = None;
        loop {
            let page: rmcp::model::ListToolsResult = self
                .request("tools/list", Some(serde_json::json!({ "cursor": cursor })))
                .await?;
            tools.extend(page.tools);
            cursor = page.next_cursor;
            if cursor.is_none() {
                return Ok(tools);
            }
        }
    }

    async fn list_all_resources(&self) -> Result<Vec<rmcp::model::Resource>, McpError> {
        let page: rmcp::model::ListResourcesResult = self.request("resources/list", None).await?;
        Ok(page.resources)
    }
}

fn lifecycle_server_config(
    fixture_mode: &str,
    configured_mode: UpstreamProtocolMode,
    request_log_file: &std::path::Path,
) -> ServerConfig {
    let mut config = mock_server("echo");
    config.args = vec![
        "--lifecycle".to_string(),
        fixture_mode.to_string(),
        "--request-log-file".to_string(),
        request_log_file.to_string_lossy().into_owned(),
    ];
    config.protocol_mode = configured_mode;
    config
}

fn unique_lifecycle_log(label: &str) -> std::path::PathBuf {
    plug_core::paths::test::scratch(label).join("requests.log")
}

fn lifecycle_sequence(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("lifecycle request log")
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn upstream_lifecycle_modes_negotiate_independently_with_live_truth() {
    // The global gate is the first safety boundary: even a server configured
    // modern uses initialize while the gate is off.
    let gated_log = unique_lifecycle_log("gated");
    let mut gated_config = Config::default();
    gated_config.servers.insert(
        "gated".to_string(),
        lifecycle_server_config("legacy-only", UpstreamProtocolMode::Modern, &gated_log),
    );
    let gated_engine = Engine::new(gated_config);
    gated_engine.start().await;
    let gated_status = gated_engine
        .server_statuses()
        .into_iter()
        .find(|status| status.server_id == "gated")
        .expect("gated status");
    assert_eq!(
        gated_status.selected_protocol_era,
        Some(plug_core::protocol::ProtocolEra::Legacy)
    );
    let gated_sequence = lifecycle_sequence(&gated_log);
    assert_eq!(
        gated_sequence.first().map(String::as_str),
        Some("initialize")
    );
    assert!(
        !gated_sequence
            .iter()
            .any(|method| method == "server/discover")
    );
    gated_engine.shutdown().await;

    // Auto uses RMCP's exact method-not-found fallback on the same transport.
    let auto_log = unique_lifecycle_log("auto");
    let mut auto_config = Config {
        modern_upstream_enabled: true,
        ..Config::default()
    };
    auto_config.servers.insert(
        "auto".to_string(),
        lifecycle_server_config("legacy-only", UpstreamProtocolMode::Auto, &auto_log),
    );
    let auto_engine = Engine::new(auto_config);
    auto_engine.start().await;
    let auto_sequence = lifecycle_sequence(&auto_log);
    assert_eq!(
        &auto_sequence[..4],
        &[
            "server/discover",
            "initialize",
            "notifications/initialized",
            "tools/list",
        ]
    );
    let auto_status = auto_engine
        .server_statuses()
        .into_iter()
        .find(|status| status.server_id == "auto")
        .expect("auto status");
    assert_eq!(
        auto_status.selected_protocol_version.as_deref(),
        Some("2025-11-25")
    );
    auto_engine.shutdown().await;

    // Forced modern selects discovery, records live truth, and routes an
    // ordinary tool call through the existing shared router.
    let modern_log = unique_lifecycle_log("modern");
    let mut modern_config = Config {
        modern_upstream_enabled: true,
        ..Config::default()
    };
    modern_config.servers.insert(
        "modern".to_string(),
        lifecycle_server_config("modern-only", UpstreamProtocolMode::Modern, &modern_log),
    );
    let modern_engine = Engine::new(modern_config);
    modern_engine.start().await;
    let modern_status = modern_engine
        .server_statuses()
        .into_iter()
        .find(|status| status.server_id == "modern")
        .expect("modern status");
    assert_eq!(
        modern_status.selected_protocol_era,
        Some(plug_core::protocol::ProtocolEra::Modern)
    );
    assert_eq!(
        modern_status.selected_protocol_version.as_deref(),
        Some("2026-07-28")
    );
    let routed_tool = modern_engine
        .tool_router()
        .list_tools_for_client_session(plug_core::types::ClientType::Unknown, None, None)
        .iter()
        .find(|tool| tool.name.ends_with("__echo"))
        .expect("modern tool routed")
        .clone();
    let projected_page = modern_engine
        .tool_router()
        .list_tools_page_for_client_session(plug_core::types::ClientType::Unknown, None, None, None)
        .expect("first page");
    assert_eq!(projected_page.ttl_ms, Some(0));
    assert_eq!(
        projected_page.cache_scope,
        Some(rmcp::model::CacheScope::Private)
    );
    let routed_meta = routed_tool.meta.as_ref().expect("admitted U7 metadata");
    assert_eq!(
        routed_meta.get("example.test/typed"),
        Some(&serde_json::json!({
            "boolean": true,
            "number": 7,
            "array": [null, "value"]
        })),
        "admitted extension values must retain JSON type fidelity"
    );
    assert_eq!(
        routed_meta.get("io.modelcontextprotocol/ui"),
        Some(&serde_json::json!({"resourceUri":"ui://plug/fixture"})),
        "Apps/UI descriptor metadata must survive the routed catalog"
    );
    assert!(routed_tool.output_schema.is_some());
    let tool_name = routed_tool.name.to_string();
    let result = modern_engine
        .tool_router()
        .call_tool(&tool_name, None)
        .await
        .expect("ordinary modern upstream call translates");
    assert!(result.content.iter().any(|content| {
        content
            .as_text()
            .is_some_and(|text| text.text == "lifecycle fixture echo")
    }));
    assert_eq!(
        result
            .meta
            .as_ref()
            .and_then(|meta| meta.get("example.test/result")),
        Some(&serde_json::json!({"boolean": true, "number": 7})),
        "admitted result metadata must survive before artifact handling"
    );
    let modern_sequence = lifecycle_sequence(&modern_log);
    assert_eq!(
        modern_sequence.first().map(String::as_str),
        Some("server/discover")
    );
    assert!(!modern_sequence.iter().any(|method| method == "initialize"));
    modern_engine.shutdown().await;
}

#[tokio::test]
async fn forced_modern_rejects_a_legacy_only_upstream_without_fallback() {
    let log = unique_lifecycle_log("forced-modern-rejects-legacy");
    let manager = ServerManager::new();
    manager.set_modern_upstream_enabled(true);
    let config = lifecycle_server_config("legacy-only", UpstreamProtocolMode::Modern, &log);

    let error = match manager.start_server("legacy-only", &config).await {
        Ok(_) => panic!("forced modern must reject method-not-found"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("Method not found") || error.to_string().contains("-32601"),
        "unexpected forced-modern error: {error:#}"
    );
    assert_eq!(lifecycle_sequence(&log), vec!["server/discover"]);
}

#[tokio::test]
async fn legacy_task_child_process_stdio_preserves_full_lifecycle_under_rmcp3() {
    let mut server = mock_server("echo");
    server.args = vec!["--legacy-tasks".to_string()];
    let mut config = Config::default();
    config.servers.insert("legacy-task".to_string(), server);

    let engine = Engine::new(config);
    engine.start().await;
    let router = engine.tool_router();
    let tool_name = router
        .list_tools_for_client_session(plug_core::types::ClientType::Unknown, None, None)
        .iter()
        .find(|tool| tool.name.ends_with("__echo"))
        .expect("legacy task child tool routed")
        .name
        .to_string();
    let owner = plug_core::tasks::TaskOwner::new(Arc::<str>::from("stdio:legacy-task-e2e"));

    let created = router
        .enqueue_tool_task(
            &tool_name,
            Some(
                serde_json::json!({"input":"round-trip"})
                    .as_object()
                    .expect("arguments object")
                    .clone(),
            ),
            None,
            owner.clone(),
            None,
            None,
        )
        .await
        .expect("create native legacy task");
    assert_eq!(created.task.task_id, "task_1");

    let info = router
        .get_task_info_for_owner(&owner, &created.task.task_id)
        .await
        .expect("get native legacy task");
    assert_eq!(
        info.task.status,
        plug_core::legacy_tasks::TaskStatus::Completed
    );

    let payload = router
        .get_task_result_for_owner(&owner, &created.task.task_id)
        .await
        .expect("get native legacy task result");
    assert!(payload.0.to_string().contains("child-task round-trip"));

    let second = router
        .enqueue_tool_task(&tool_name, None, None, owner.clone(), None, None)
        .await
        .expect("create cancellable legacy task");
    let cancelled = router
        .cancel_task_for_owner(&owner, &second.task.task_id)
        .await
        .expect("cancel native legacy task");
    assert_eq!(
        cancelled.task.status,
        plug_core::legacy_tasks::TaskStatus::Cancelled
    );

    engine.shutdown().await;
}

async fn reserve_unused_local_port() -> u16 {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind ephemeral listener");
    listener.local_addr().expect("ephemeral local addr").port()
}

// A connected upstream that stalls on resource listing must be skipped after
// its per-server call_timeout_secs, not allowed to block the whole refresh.
#[tokio::test]
async fn test_refresh_tools_skips_upstream_that_stalls_on_resource_listing() {
    let mut config = Config::default();
    let mut server = mock_server("echo");
    // Advertise resources and hang on list_resources (fail-mode timeout).
    server.args.push("--resources".to_string());
    server.args.push("--fail-mode".to_string());
    server.args.push("timeout".to_string());
    server.call_timeout_secs = 1;
    config.servers.insert("mock".to_string(), server);

    let engine = Engine::new(config);
    engine.start().await;

    // Without the per-server listing timeout this hangs forever; with it the
    // stalled upstream is skipped after ~1s and the refresh completes.
    tokio::time::timeout(
        Duration::from_secs(10),
        engine.tool_router().refresh_tools(),
    )
    .await
    .expect("refresh_tools must not block on an upstream that stalls on resource listing");

    // The upstream stalls on its FIRST listing (timeout arm of ListingOutcome::
    // Unavailable, no last-known-good cached). It is routable but serving nothing,
    // so it must report `degraded` — not `healthy` (it is failing) and not `absent`
    // (it is still connected/routable).
    let availability = engine
        .server_statuses()
        .into_iter()
        .find(|s| s.server_id == "mock")
        .map(|s| s.availability)
        .expect("mock server status present");
    assert_eq!(availability, plug_core::types::Availability::Degraded);

    engine.shutdown().await;
}

// The three live catalog families (resources, resource templates, prompts)
// must be fetched concurrently, not serially: a refresh's upstream latency
// should be ~max(family delays), not ~sum(family delays). One upstream
// delays all three list responses by 600ms; serial fetching would take
// >=1800ms while concurrent fetching takes ~600ms plus overhead. The
// 900ms margin (assert < 1500ms) is deliberate anti-flake headroom.
#[tokio::test]
async fn catalog_families_fetch_concurrently() {
    let mut config = Config::default();
    let mut server = mock_server("echo");
    server.args.push("--resources".to_string());
    server.args.push("--resource-templates".to_string());
    server.args.push("--prompts".to_string());
    server.args.push("--list-delay-ms".to_string());
    server.args.push("600".to_string());
    config.servers.insert("mock".to_string(), server);

    let engine = Engine::new(config);
    engine.start().await;

    let started = std::time::Instant::now();
    engine.tool_router().refresh_tools().await;
    let elapsed = started.elapsed();
    eprintln!("catalog_families_fetch_concurrently: elapsed={elapsed:?}");

    assert!(
        elapsed < Duration::from_millis(1500),
        "refresh_tools took {elapsed:?}; expected ~600ms (concurrent family fetch), \
         not ~1800ms (serial family fetch)"
    );

    engine.shutdown().await;
}

// A transient listing failure must NOT prune an active resource subscription.
// The degraded-vs-absent regression: the
// upstream stays routable, its last-known-good catalog is carried forward, the
// subscription survives, and the server reports `availability = degraded`. A
// genuine fresh removal (empty success) must still prune — proving the fix did
// not simply disable pruning.
#[tokio::test]
async fn degraded_listing_carries_last_known_good_and_keeps_subscription() {
    use plug_core::notifications::NotificationTarget;
    use plug_core::types::Availability;

    let tmp = plug_core::paths::test::scratch("degraded");
    // Neither flag exists initially → the mock lists its resource normally.
    let fail_flag = tmp.join("fail.flag");
    let empty_flag = tmp.join("empty.flag");

    let mut config = Config::default();
    let mut server = mock_server("echo");
    server.args.push("--resources".to_string());
    server.args.push("--list-fail-flag-file".to_string());
    server.args.push(fail_flag.to_string_lossy().to_string());
    server.args.push("--list-empty-flag-file".to_string());
    server.args.push(empty_flag.to_string_lossy().to_string());
    server.call_timeout_secs = 2;
    config.servers.insert("mock".to_string(), server);

    let engine = Engine::new(config);
    engine.start().await;
    let router = engine.tool_router();
    let uri = "file:///tmp/mock-resource.txt";

    let availability_of = |engine: &Engine| {
        engine
            .server_statuses()
            .into_iter()
            .find(|s| s.server_id == "mock")
            .map(|s| s.availability)
            .expect("mock server status present")
    };

    // ── Healthy: live listing succeeds, resource enters catalog, cache primed ──
    router.refresh_tools().await;
    assert!(
        router
            .list_resources_for_client(None)
            .iter()
            .any(|r| r.uri == uri),
        "resource should be present after a healthy listing"
    );
    let target = NotificationTarget::Ipc {
        client_id: Arc::from("c1"),
    };
    // Subscribe a notification receiver BEFORE subscribing so we capture the
    // resources/updated the mock emits on subscribe — proving the subscription
    // delivers end-to-end, not just that a registry entry exists.
    let mut notifications = router.subscribe_notifications();
    router
        .subscribe_resource(uri, target.clone(), None)
        .await
        .expect("subscribe to mock resource");
    assert_eq!(router.active_subscription_count(), 1);
    assert_eq!(availability_of(&engine), Availability::Healthy);

    let delivered = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match notifications.recv().await {
                Ok(plug_core::notifications::ProtocolNotification::ResourceUpdated {
                    target: t,
                    params,
                    ..
                }) if params.uri == uri && t == target => break true,
                Ok(_) => continue,
                Err(_) => break false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        delivered,
        "an active subscription must deliver resources/updated to its subscriber"
    );

    // ── Degraded: live listing errors → carry last-known-good, keep subscription ──
    std::fs::write(&fail_flag, b"1").expect("set fail flag");
    router.refresh_tools().await;
    assert!(
        router
            .list_resources_for_client(None)
            .iter()
            .any(|r| r.uri == uri),
        "degraded listing must carry the last-known-good resource forward"
    );
    assert_eq!(
        router.active_subscription_count(),
        1,
        "degraded listing must NOT prune the active subscription"
    );
    assert_eq!(availability_of(&engine), Availability::Degraded);

    // ── Recovery: live listing succeeds again → healthy, subscription intact ──
    std::fs::remove_file(&fail_flag).expect("clear fail flag");
    router.refresh_tools().await;
    assert_eq!(router.active_subscription_count(), 1);
    assert_eq!(availability_of(&engine), Availability::Healthy);

    // ── Control: a genuine fresh removal (empty success) still prunes ──
    std::fs::write(&empty_flag, b"1").expect("set empty flag");
    router.refresh_tools().await;
    assert!(
        !router
            .list_resources_for_client(None)
            .iter()
            .any(|r| r.uri == uri),
        "a genuine fresh-empty listing must drop the resource"
    );
    assert_eq!(
        router.active_subscription_count(),
        0,
        "a genuine removal must still prune the subscription"
    );

    engine.shutdown().await;
}

/// A wrapper script that runs the mock with `first_args` the first time and
/// with `--tools echo --delay-ms 0` after that, keeping its marker in `temp`.
fn write_mock_wrapper(temp: &std::path::Path, first_args: &str) -> std::path::PathBuf {
    let script = temp.join("mock-wrapper.sh");
    let mock = plug_test_harness::mock_server_bin();
    let mock = mock.display();
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ ! -f \"$1\" ]; then\n  touch \"$1\"\n  exec '{mock}' {first_args}\nelse\n  exec '{mock}' --tools echo --delay-ms 0\nfi\n"
        ),
    )
    .expect("write wrapper");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("make wrapper executable");
    script
}

#[tokio::test]
async fn test_stdio_timeout_reconnects_cleanly() {
    let temp = plug_core::paths::test::scratch("timeout-test");
    let marker = temp.join("first-run.marker");
    let script = write_mock_wrapper(&temp, "--tools echo --delay-ms 1500");

    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        ServerConfig {
            command: Some(script.display().to_string()),
            args: vec![
                marker.display().to_string(),
                plug_test_harness::mock_server_bin().display().to_string(),
            ],
            timeout_secs: 10,
            call_timeout_secs: 1,
            ..Default::default()
        },
    );

    let engine = Engine::new(config);
    engine.start().await;

    let first = engine.tool_router().call_tool("Mock__echo", None).await;
    assert!(first.is_err(), "first call should time out");

    let second = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let result = engine
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
            if result.is_ok() {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("reconnect should complete within timeout");
    assert!(second.is_ok(), "second call should succeed after reconnect");

    engine.shutdown().await;
}

#[tokio::test]
async fn test_stdio_crash_restart_recovers_cleanly() {
    let temp = plug_core::paths::test::scratch("crash-test");
    let marker = temp.join("first-run.marker");
    let script = write_mock_wrapper(&temp, "--tools echo --fail-mode crash");

    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        ServerConfig {
            command: Some(script.display().to_string()),
            args: vec![
                marker.display().to_string(),
                plug_test_harness::mock_server_bin().display().to_string(),
            ],
            timeout_secs: 10,
            call_timeout_secs: 5,
            ..Default::default()
        },
    );

    let engine = Engine::new(config);
    engine.start().await;

    let call = || async {
        engine
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
            .await
    };

    // The server dies with the call in hand. The tool may have run, so the
    // call is not sent a second time: the caller is told the outcome is
    // unknown.
    let dropped = call()
        .await
        .expect_err("a call the server died on is not re-sent");
    assert!(
        dropped.message.contains("may or may not have completed"),
        "unexpected error: {dropped:?}"
    );

    // The server is brought back without anyone asking, and the next call
    // works.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let rendered = loop {
        match call().await {
            Ok(result) => break format!("{result:?}"),
            Err(error) if tokio::time::Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(error) => panic!("server did not come back: {error:?}"),
        }
    };
    assert!(
        rendered.contains("recover"),
        "unexpected result: {rendered}"
    );

    engine.shutdown().await;
}

// HTTP upstream crash-restart supervision. `MockRestartableHttpUpstream`
// binds port 0 once, captures the port, and re-binds that same port for the
// restart. If another process takes the port between drop and re-bind, the
// whole scenario retries (bounded).

/// Per-incarnation state for the restartable mock HTTP upstream.
///
/// Sessions are scoped to one incarnation: a restarted incarnation starts
/// with an empty session set, so requests carrying a stale session id get
/// HTTP 404 — exactly what a real streamable HTTP server does after losing
/// its session state in a crash.
#[derive(Clone)]
struct MockRestartableHttpUpstreamState {
    sessions: Arc<Mutex<std::collections::HashSet<String>>>,
    session_counter: Arc<std::sync::atomic::AtomicU64>,
}

struct MockRestartableHttpUpstream {
    addr: SocketAddr,
    shutdown: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl MockRestartableHttpUpstream {
    /// Bind `addr` and serve a session-aware mock MCP upstream on `/mcp`.
    /// Pass port 0 to let the OS choose; pass a captured `addr` to restart
    /// a previous incarnation on the same port.
    async fn start(addr: SocketAddr) -> Result<Self, std::io::Error> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;

        let state = MockRestartableHttpUpstreamState {
            sessions: Arc::new(Mutex::new(std::collections::HashSet::new())),
            session_counter: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        };
        let app = axum::Router::new()
            .route(
                "/mcp",
                axum::routing::post(mock_restartable_http_upstream_handler),
            )
            .with_state(state);

        let shutdown = CancellationToken::new();
        let shutdown_signal = shutdown.clone();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { shutdown_signal.cancelled().await })
                .await
                .expect("serve mock restartable http upstream");
        });

        Ok(Self {
            addr,
            shutdown,
            handle,
        })
    }

    /// Gracefully stop this incarnation and wait until the listener socket is
    /// released, so the port can be re-bound by the next incarnation.
    async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.handle)
            .await
            .expect("mock restartable http upstream shutdown timed out")
            .expect("mock restartable http upstream task panicked");
    }
}

async fn mock_restartable_http_upstream_handler(
    State(state): State<MockRestartableHttpUpstreamState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let json_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let method = json_body
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");

    let json_headers = |session_id: String| {
        [
            (
                axum::http::HeaderName::from_static("mcp-session-id"),
                session_id,
            ),
            (
                axum::http::header::CONTENT_TYPE,
                "application/json".to_string(),
            ),
        ]
    };

    if method == "initialize" {
        let session_id = format!(
            "sess-{}",
            state
                .session_counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        state.sessions.lock().await.insert(session_id.clone());
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": json_body.get("id"),
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {
                    "tools": { "listChanged": false }
                },
                "serverInfo": {
                    "name": "mock-restartable-http-upstream",
                    "version": "1.0.0"
                }
            }
        });
        return (StatusCode::OK, json_headers(session_id), resp.to_string()).into_response();
    }

    // Every non-initialize message must carry a session id this incarnation
    // minted; stale/absent sessions get 404 like a real restarted server.
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());
    let session_known = match &session_id {
        Some(id) => state.sessions.lock().await.contains(id),
        None => false,
    };
    let Some(session_id) = session_id.filter(|_| session_known) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    match method {
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": {
                    "tools": [{
                        "name": "echo",
                        "description": "Echo the input back",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "input": { "type": "string" }
                            }
                        }
                    }]
                }
            });
            (StatusCode::OK, json_headers(session_id), resp.to_string()).into_response()
        }
        "tools/call" => {
            let arguments = json_body
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_body.get("id"),
                "result": {
                    "content": [{
                        "type": "text",
                        "text": format!("Called echo with {arguments}")
                    }],
                    "isError": false
                }
            });
            (StatusCode::OK, json_headers(session_id), resp.to_string()).into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

enum HttpCrashRestartOutcome {
    Completed,
    PortStolen,
}

/// One full crash-restart scenario. Returns `PortStolen` if the captured
/// port was grabbed by another process between drop and re-bind (the caller
/// retries the whole setup a bounded number of times).
async fn run_http_upstream_crash_restart_scenario(
    recovery_deadline: Duration,
    recovery_poll: Duration,
) -> HttpCrashRestartOutcome {
    let mock = MockRestartableHttpUpstream::start(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind mock restartable http upstream");
    let addr = mock.addr;

    let mut config = Config::default();
    config.servers.insert(
        "httpcrash".to_string(),
        ServerConfig {
            transport: TransportType::Http,
            url: Some(format!("http://{addr}/mcp")),
            timeout_secs: 10,
            call_timeout_secs: 5,
            max_concurrent: 4,
            circuit_breaker_enabled: false,
            ..Default::default()
        },
    );

    let engine = Engine::new(config);
    engine.start().await;

    // Phase 1: baseline call succeeds against the live upstream.
    let result = engine
        .tool_router()
        .call_tool(
            "Httpcrash__echo",
            Some(
                serde_json::json!({"input": "before-crash"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await;
    assert!(
        result.is_ok(),
        "baseline tools/call should succeed: {result:?}"
    );
    let rendered = format!("{:?}", result.unwrap());
    assert!(
        rendered.contains("before-crash"),
        "unexpected baseline result: {rendered}"
    );

    // Phase 2: kill the upstream (graceful shutdown releases the port and
    // severs keep-alive connections). A call must now fail.
    mock.stop().await;
    let result = engine
        .tool_router()
        .call_tool(
            "Httpcrash__echo",
            Some(
                serde_json::json!({"input": "while-dead"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await;
    assert!(
        result.is_err(),
        "tools/call should fail while the upstream is down: {result:?}"
    );

    // Phase 3: restart the upstream on the SAME port.
    let mock = match MockRestartableHttpUpstream::start(addr).await {
        Ok(mock) => mock,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            engine.shutdown().await;
            return HttpCrashRestartOutcome::PortStolen;
        }
        Err(error) => panic!("re-bind mock upstream on {addr}: {error}"),
    };

    // Phase 4: poll until calls flow again. The restarted incarnation 404s
    // the stale session, which the client stack recovers from.
    let deadline = tokio::time::Instant::now() + recovery_deadline;
    let recovered = loop {
        let result = engine
            .tool_router()
            .call_tool(
                "Httpcrash__echo",
                Some(
                    serde_json::json!({"input": "after-restart"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await;
        match result {
            Ok(value) => break value,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(recovery_poll).await;
            }
            Err(error) => {
                panic!("tools/call did not recover within {recovery_deadline:?}: {error:?}")
            }
        }
    };
    let rendered = format!("{recovered:?}");
    assert!(
        rendered.contains("after-restart"),
        "unexpected post-restart result: {rendered}"
    );

    // Phase 5: deterministically exercise the engine reconnect path
    // (one fresh connect, commit and `refresh_tools`) against the restarted
    // upstream — the same entry point
    // health-monitor proactive recovery and call-path reactive recovery use.
    assert_eq!(
        engine
            .reconnect_server("httpcrash")
            .await
            .expect("explicit reconnect against restarted upstream"),
        plug_core::health::Reconnect::Reconnected
    );
    let result = engine
        .tool_router()
        .call_tool(
            "Httpcrash__echo",
            Some(
                serde_json::json!({"input": "after-reconnect"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await;
    assert!(
        result.is_ok(),
        "tools/call should succeed after explicit reconnect: {result:?}"
    );
    let rendered = format!("{:?}", result.unwrap());
    assert!(
        rendered.contains("after-reconnect"),
        "unexpected post-reconnect result: {rendered}"
    );

    engine.shutdown().await;
    mock.stop().await;
    HttpCrashRestartOutcome::Completed
}

#[tokio::test]
async fn test_http_upstream_crash_restart_recovers_cleanly() {
    // Poll bounds as named consts: if monitor or
    // reconnect cadences change in config defaults, adjust these to match.
    const HTTP_CRASH_RECOVERY_DEADLINE: Duration = Duration::from_secs(15);
    const HTTP_CRASH_RECOVERY_POLL: Duration = Duration::from_millis(250);
    const HTTP_CRASH_SETUP_ATTEMPTS: usize = 3;

    for attempt in 1..=HTTP_CRASH_SETUP_ATTEMPTS {
        match run_http_upstream_crash_restart_scenario(
            HTTP_CRASH_RECOVERY_DEADLINE,
            HTTP_CRASH_RECOVERY_POLL,
        )
        .await
        {
            HttpCrashRestartOutcome::Completed => return,
            HttpCrashRestartOutcome::PortStolen => {
                eprintln!(
                    "mock upstream port stolen between drop and re-bind \
                     (attempt {attempt}/{HTTP_CRASH_SETUP_ATTEMPTS}); retrying full setup"
                );
            }
        }
    }
    panic!(
        "mock upstream port was stolen on {HTTP_CRASH_SETUP_ATTEMPTS} consecutive setup attempts"
    );
}

#[tokio::test]
async fn test_end_to_end_proxy_path() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server("echo,greet"));

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect(engine.tool_router().clone());

    let tools = client.list_all_tools().await.expect("list tools");
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

    let statuses = engine.server_statuses();
    let mock = statuses
        .iter()
        .find(|status| status.server_id == "mock")
        .expect("mock server status");
    let metrics = mock.metrics.as_ref().expect("metrics present on status");
    assert!(
        metrics.call_count >= 1,
        "successful tool call should increment call_count, got {metrics:?}"
    );
    assert_eq!(
        metrics.error_count, 0,
        "echo success must not count as error: {metrics:?}"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn test_http_end_to_end_proxy_path_with_sse() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server("echo"));

    let engine = Engine::new(config);
    engine.start().await;

    let state = Arc::new(HttpState::new(
        engine.tool_router().clone(),
        Arc::new(plug_core::session::SessionStore::new(1800, 100)),
        CancellationToken::new(),
    ));
    let app = build_router(state.clone());

    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": HTTP_PROTOCOL_VERSION,
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

/// MCP Apps (SEP-1865) is pass-through for Plug: the server links a tool to a
/// `ui://` resource and the client renders it. Plug has to say it supports the
/// extension, or the server leaves the link out, and then carry the link, the
/// resource, and the resource's security metadata through untouched.
#[tokio::test]
async fn an_mcp_app_reaches_the_client_whole() {
    let mut config = Config::default();
    let mut mock = mock_server("widget");
    mock.args.push("--resources".to_string());
    config.servers.insert("mock".to_string(), mock);

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect(engine.tool_router().clone());

    let tools = client.list_all_tools().await.expect("list tools");
    let widget = tools
        .iter()
        .find(|tool| tool.name == "Mock__widget")
        .expect("widget tool");
    assert_eq!(
        widget.meta.as_ref().and_then(|meta| meta.get("ui")),
        Some(&serde_json::json!({
            "resourceUri": "ui://mock/widget.html",
            "visibility": ["model", "app"]
        })),
        "the server only links the tool to its UI for a client that supports MCP Apps"
    );

    let resources = client.list_all_resources().await.expect("list resources");
    let page = resources
        .iter()
        .find(|resource| resource.uri == "ui://mock/widget.html")
        .expect("ui resource is listed under its own URI");
    assert_eq!(page.mime_type.as_deref(), Some("text/html;profile=mcp-app"));

    let read = client
        .read_resource(ReadResourceRequestParams::new("ui://mock/widget.html"))
        .await
        .expect("read ui resource");
    let rmcp::model::ResourceContents::TextResourceContents {
        mime_type, meta, ..
    } = &read.contents[0]
    else {
        panic!("ui resource is text");
    };
    assert_eq!(mime_type.as_deref(), Some("text/html;profile=mcp-app"));
    assert_eq!(
        meta.as_ref().and_then(|meta| meta.get("ui")),
        Some(&serde_json::json!({
            "csp": {"connectDomains": ["https://api.example.test"]},
            "prefersBorder": true
        })),
        "the client builds its sandbox from this"
    );

    // The page calls the tool by the name its server gave it.
    let called = engine.tool_router().call_tool("widget", None).await;
    assert!(called.is_ok(), "an app's own tool call routes: {called:?}");

    engine.shutdown().await;
}

#[tokio::test]
async fn a_tool_turned_off_for_everyone_is_neither_listed_nor_callable() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server("echo,greet"));
    config.disabled_tools = vec!["Mock__greet".to_string()];

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect(engine.tool_router().clone());

    let tools = client.list_all_tools().await.expect("list tools");
    let names = tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert!(names.contains(&"Mock__echo".to_string()), "{names:?}");
    assert!(!names.contains(&"Mock__greet".to_string()), "{names:?}");

    // Hiding a tool is not enough: a client that already knows the name must
    // not reach the server with it.
    let refused = client
        .call_tool(CallToolRequestParams::new("Mock__greet"))
        .await;
    assert!(
        refused.is_err() || refused.is_ok_and(|result| result.is_error == Some(true)),
        "a call to a tool turned off for everyone went through"
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn test_structured_content_passes_through_end_to_end() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server("structured"));

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect(engine.tool_router().clone());

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
async fn test_resource_link_passes_through_end_to_end() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server("resource_link"));

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect(engine.tool_router().clone());

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__resource_link"))
        .await
        .expect("call resource_link tool");
    let resource = result
        .content
        .first()
        .and_then(|content| content.as_resource_link())
        .expect("resource_link content");
    assert_eq!(resource.uri, "file:///tmp/mock-resource.txt");
    assert_eq!(resource.name, "mock-resource.txt");
    assert_eq!(resource.mime_type.as_deref(), Some("text/plain"));

    engine.shutdown().await;
}

#[tokio::test]
async fn test_oversized_tool_result_spills_to_artifact_link() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server("artifact_text"));

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect(engine.tool_router().clone());

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__artifact_text"))
        .await
        .expect("call oversized tool");

    let resource = result
        .content
        .iter()
        .find_map(|content| content.as_resource_link())
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
async fn test_artifact_manifest_is_readable() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server("artifact_text"));

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect(engine.tool_router().clone());

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__artifact_text"))
        .await
        .expect("call oversized tool");
    let manifest_uri = result
        .content
        .iter()
        .find_map(|content| content.as_resource_link())
        .map(|resource| resource.uri.clone())
        .expect("artifact resource_link content");

    let manifest = client
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
async fn test_artifact_chunk_is_readable() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server("artifact_text"));

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect(engine.tool_router().clone());

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__artifact_text"))
        .await
        .expect("call oversized tool");
    let manifest_uri = result
        .content
        .iter()
        .find_map(|content| content.as_resource_link())
        .map(|resource| resource.uri.clone())
        .expect("artifact resource_link content");
    let chunk_uri = manifest_uri.replace("/manifest", "/chunk/0");

    let chunk = client
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
async fn test_attachment_like_tool_result_spills_to_artifact_link() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server("attachment_blob"));

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect(engine.tool_router().clone());

    let result = client
        .call_tool(CallToolRequestParams::new("Mock__attachment_blob"))
        .await
        .expect("call attachment-like tool");

    let resource = result
        .content
        .iter()
        .find_map(|content| content.as_resource_link())
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
        .insert("mock".to_string(), mock_server("structured"));

    let engine = Engine::new(config);
    engine.start().await;

    let state = Arc::new(HttpState::new(
        engine.tool_router().clone(),
        Arc::new(plug_core::session::SessionStore::new(1800, 100)),
        CancellationToken::new(),
    ));
    let app = build_router(state.clone());

    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": HTTP_PROTOCOL_VERSION,
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
        .insert("mock".to_string(), mock_server("resource_link"));

    let engine = Engine::new(config);
    engine.start().await;

    let state = Arc::new(HttpState::new(
        engine.tool_router().clone(),
        Arc::new(plug_core::session::SessionStore::new(1800, 100)),
        CancellationToken::new(),
    ));
    let app = build_router(state.clone());

    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": HTTP_PROTOCOL_VERSION,
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
        .insert("mock".to_string(), mock_server("artifact_text"));

    let engine = Engine::new(config);
    engine.start().await;

    let state = Arc::new(HttpState::new(
        engine.tool_router().clone(),
        Arc::new(plug_core::session::SessionStore::new(1800, 100)),
        CancellationToken::new(),
    ));
    let app = build_router(state.clone());

    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": HTTP_PROTOCOL_VERSION,
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
        .insert("mock".to_string(), mock_server("echo"));

    let engine = Engine::new(config);
    engine.start().await;

    let client_a = Downstream::connect(engine.tool_router().clone());
    let client_b = Downstream::connect(engine.tool_router().clone());

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

// rmcp's StreamableHttpClientTransport adds the MCP-Protocol-Version header
// after initialization, which Plug's upstream HTTP client relies on; this test
// also checks Plug sends a single Bearer header from its OAuth store.

#[tokio::test]
async fn test_upstream_http_sends_protocol_version_header() {
    let _guard = oauth_integration_test_lock().lock().await;
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

    let server_name = format!("mock-http-oauth-{}", std::process::id());
    let store = oauth::get_or_create_store(&server_name);
    store.clear().await.expect("clear OAuth store before test");
    let resource_url = format!("http://127.0.0.1:{port}/mcp");
    save_bound_oauth_test_credentials(
        &store,
        &resource_url,
        &resource_url,
        "oauth-access-token",
        "oauth-refresh-token",
    )
    .await;

    // Connect through plug's upstream HTTP path (ServerManager::start_server).
    let sm = Arc::new(ServerManager::new());
    let config = ServerConfig {
        transport: TransportType::Http,
        url: Some(resource_url),
        auth: Some("oauth".to_string()),
        oauth_client_id: Some("test-client".to_string()),
        timeout_secs: 10,
        call_timeout_secs: 5,
        max_concurrent: 4,
        circuit_breaker_enabled: false,
        ..Default::default()
    };

    let upstream = sm
        .start_server(&server_name, &config)
        .await
        .expect("connect to mock HTTP upstream");

    // start_server does: initialize → notifications/initialized → tools/list.
    // After it returns, all three requests have been made.

    let captured = mock_state.captured.lock().await;

    // initialize: should NOT have MCP-Protocol-Version (version unknown yet)
    let init = captured
        .iter()
        .find(|(m, _, _)| m == "initialize")
        .expect("should have captured initialize request");
    assert_eq!(
        init.1, None,
        "initialize must not send MCP-Protocol-Version (version not yet negotiated)"
    );
    assert_eq!(
        init.2.as_deref(),
        Some("Bearer oauth-access-token"),
        "initialize should send a single Bearer auth header"
    );

    let initialized = captured
        .iter()
        .find(|(m, _, _)| m == "notifications/initialized")
        .expect("should have captured notifications/initialized");
    assert_eq!(
        initialized.1,
        Some("2025-11-25".to_string()),
        "notifications/initialized must include MCP-Protocol-Version from server's InitializeResult"
    );

    let tools_list = captured
        .iter()
        .find(|(m, _, _)| m == "tools/list")
        .expect("should have captured tools/list request");
    assert_eq!(
        tools_list.1,
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
    let result = AssertUnwindSafe(async {
        use rmcp::transport::auth::OAuthClientConfig;

        let mut auth_manager =
            plug_core::upstream_http::authorization_manager(provider.mcp_url().as_str())
                .await
                .expect("create auth manager");
        auth_manager.set_credential_store(plug_core::oauth::CompositeCredentialStore::new(
            server_name.clone(),
        ));
        auth_manager.set_state_store(plug_core::oauth::CompositeStateStore::new(
            server_name.clone(),
        ));
        let metadata = auth_manager
            .resolve_metadata()
            .await
            .expect("discover metadata");
        auth_manager.set_metadata(metadata.metadata);
        auth_manager
            .configure_client(
                OAuthClientConfig::new("test-client".to_string(), "http://localhost:0/callback")
                    .with_scopes(vec!["read".to_string()]),
            )
            .expect("configure oauth client");

        let authorize_url = auth_manager
            .get_authorization_url(&["read"])
            .await
            .expect("authorization url");
        let client = plug_core::tls::http_client_builder()
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
    save_bound_oauth_test_credentials(
        &store,
        &provider.base_url,
        &provider.mcp_url(),
        "access-token-1",
        "refresh-token-1",
    )
    .await;
    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let mut config = Config::default();
        config
            .servers
            .insert(server_name.clone(), oauth_http_server(provider.mcp_url()));

        let started_engine = Engine::new(config);
        started_engine.start().await;
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

        assert_eq!(
            started_engine
                .reconnect_server(&server_name)
                .await
                .expect("reconnect with refreshed token"),
            plug_core::health::Reconnect::Reconnected
        );

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
    let _guard = oauth_integration_test_lock().lock().await;
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
    save_bound_oauth_test_credentials(
        &healthy_store,
        &provider.base_url,
        &provider.mcp_url(),
        "access-token-1",
        "refresh-token-1",
    )
    .await;

    let failed_port = reserve_unused_local_port().await;
    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let mut config = Config::default();
        config
            .servers
            .insert("stdio".to_string(), mock_server("echo"));
        config
            .servers
            .insert(oauth_healthy.clone(), oauth_http_server(provider.mcp_url()));
        config.servers.insert(
            oauth_auth_required.clone(),
            oauth_http_server(provider.mcp_url()),
        );
        config.servers.insert(
            "failed".to_string(),
            ServerConfig {
                transport: TransportType::Http,
                url: Some(format!("http://127.0.0.1:{failed_port}/mcp")),
                timeout_secs: 1,
                call_timeout_secs: 1,
                circuit_breaker_enabled: false,
                ..Default::default()
            },
        );

        let started_engine = Engine::new(config);
        started_engine.start().await;
        engine = Some(Arc::clone(&started_engine));

        let statuses = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let statuses = started_engine.server_statuses();
                let status_map: HashMap<String, plug_core::types::ServerStatus> = statuses
                    .iter()
                    .cloned()
                    .map(|status| (status.server_id.clone(), status))
                    .collect();
                let healthy_ready = status_map.get(&oauth_healthy).is_some_and(|s| {
                    s.auth_status == "oauth" && s.health == plug_core::types::ServerHealth::Healthy
                });
                let required_ready = status_map.get(&oauth_auth_required).is_some_and(|s| {
                    s.auth_status == "auth-required"
                        && s.health == plug_core::types::ServerHealth::AuthRequired
                });
                let stdio_ready = status_map.get("stdio").is_some_and(|s| {
                    s.health == plug_core::types::ServerHealth::Healthy && s.tool_count == 1
                });
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

/// An HTTP server signed in with OAuth, with short timeouts and no breaker.
fn oauth_http_server(url: String) -> ServerConfig {
    ServerConfig {
        transport: TransportType::Http,
        url: Some(url),
        auth: Some("oauth".to_string()),
        oauth_client_id: Some("test-client".to_string()),
        oauth_scopes: Some(vec!["read".to_string()]),
        timeout_secs: 10,
        call_timeout_secs: 5,
        max_concurrent: 4,
        circuit_breaker_enabled: false,
        ..Default::default()
    }
}

/// Start an engine with one OAuth server at `provider` whose token was saved
/// just now, wait for the server's first status, and hand it to `check`.
/// Afterwards, assert the fresh token was never refreshed.
async fn start_with_fresh_oauth_token<F, Fut>(provider: &MockOAuthProvider, label: &str, check: F)
where
    F: FnOnce(plug_core::types::ServerStatus) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let _guard = oauth_integration_test_lock().lock().await;
    let server_name = format!("{label}-{}", uuid::Uuid::new_v4().simple());
    let store = oauth::get_or_create_store(&server_name);
    store.clear().await.expect("clear OAuth store before test");
    save_bound_oauth_test_credentials(
        &store,
        &provider.base_url,
        &provider.mcp_url(),
        "access-token-1",
        "refresh-token-1",
    )
    .await;

    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let (received_at, expires_in) = store.cached_expiry().expect("saved token expiry");
        assert!(
            !oauth::time_until_refresh_window(received_at, expires_in).is_zero(),
            "the saved token must not be due for refresh"
        );

        let mut config = Config::default();
        config
            .servers
            .insert(server_name.clone(), oauth_http_server(provider.mcp_url()));
        let started_engine = Engine::new(config);
        started_engine.start().await;
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
        assert_eq!(status.auth_status, "oauth");

        check(status).await;

        assert_eq!(
            provider.snapshot().await.token_grants,
            Vec::<String>::new(),
            "a fresh token must not be refreshed"
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

/// Wait until the methods `provider` has seen are exactly `expected`.
async fn wait_for_mcp_methods(provider: &MockOAuthProvider, expected: &[&str]) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if provider.snapshot().await.mcp_methods == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("expected upstream methods {expected:?}");
    });
}

#[tokio::test]
async fn test_oauth_stateless_http_server_with_valid_credentials_starts_healthy() {
    let provider = &MockOAuthProvider::start_with(McpBehavior::STATELESS).await;
    start_with_fresh_oauth_token(provider, "oauth-stateless", |status| async move {
        assert_eq!(status.health, plug_core::types::ServerHealth::Healthy);
        assert_eq!(
            status.tool_count, 1,
            "stateless upstream should expose tools"
        );
        assert!(
            provider
                .snapshot()
                .await
                .mcp_auth_headers
                .iter()
                .all(|header| header == "Bearer access-token-1"),
            "expected upstream requests to use the saved bearer token"
        );
    })
    .await;
}

#[tokio::test]
async fn test_oauth_startup_failure_with_valid_credentials_is_not_auth_required() {
    let provider = &MockOAuthProvider::start_with(McpBehavior::ToolListFails).await;
    start_with_fresh_oauth_token(provider, "oauth-startup-failure", |status| async move {
        assert_eq!(
            status.health,
            plug_core::types::ServerHealth::Failed,
            "valid saved credentials plus a non-auth startup failure should not be classified as re-auth required"
        );
        assert_eq!(status.tool_count, 0);
    })
    .await;
}

#[tokio::test]
async fn test_oauth_server_can_start_when_initialized_notification_is_rejected() {
    let provider = &MockOAuthProvider::start_with(
        McpBehavior::stateless_answering_initialized_with(StatusCode::BAD_REQUEST),
    )
    .await;
    start_with_fresh_oauth_token(provider, "oauth-init-notif-failure", |status| async move {
        assert_eq!(status.health, plug_core::types::ServerHealth::Healthy);
        assert_eq!(status.tool_count, 1);
        let snapshot = provider.snapshot().await;
        assert!(
            snapshot
                .mcp_auth_headers
                .iter()
                .all(|header| header == "Bearer access-token-1"),
            "expected startup requests to continue using the valid bearer token"
        );
        assert_eq!(
            snapshot.mcp_methods,
            [
                "initialize",
                "notifications/initialized",
                "tools/list",
                "tasks/list"
            ],
            "expected startup to exercise initialize, the initialized notification, tools/list, then the bounded legacy-task capability probe"
        );
    })
    .await;
}

#[tokio::test]
async fn test_oauth_server_does_not_start_when_initialized_notification_is_auth_rejected() {
    let provider = &MockOAuthProvider::start_with(
        McpBehavior::stateless_answering_initialized_with(StatusCode::UNAUTHORIZED),
    )
    .await;
    start_with_fresh_oauth_token(
        provider,
        "oauth-init-notif-auth-failure",
        |status| async move {
            assert_eq!(status.health, plug_core::types::ServerHealth::Failed);
            assert_eq!(status.tool_count, 0);
            wait_for_mcp_methods(provider, &["initialize", "notifications/initialized"]).await;
        },
    )
    .await;
}

#[tokio::test]
async fn test_oauth_server_does_not_start_when_initialized_notification_returns_server_error() {
    let provider = &MockOAuthProvider::start_with(
        McpBehavior::stateless_answering_initialized_with(StatusCode::INTERNAL_SERVER_ERROR),
    )
    .await;
    start_with_fresh_oauth_token(
        provider,
        "oauth-init-notif-server-error",
        |status| async move {
            assert_eq!(status.health, plug_core::types::ServerHealth::Failed);
            assert_eq!(status.tool_count, 0);
            wait_for_mcp_methods(provider, &["initialize", "notifications/initialized"]).await;
        },
    )
    .await;
}

#[tokio::test]

async fn test_downstream_oauth_protected_discovery_card_end_to_end() {
    let mut config = Config::default();
    config
        .servers
        .insert("mock".to_string(), mock_server("echo"));

    let engine = Engine::new(config);
    engine.start().await;

    let oauth_config = plug_core::downstream_oauth::DownstreamOauthConfig {
        public_base_url: "https://plug.example.com".to_string(),
        oauth_scopes: vec!["tools:read".to_string()],
    };
    let manager = plug_core::downstream_oauth::DownstreamOauthManager::new_with_state_path(
        oauth_config,
        plug_core::paths::test::scratch("oauth-e2e").join("state.json"),
    )
    .expect("isolated OAuth manager");
    let approval_manager = manager.clone();
    let app = build_router(Arc::new(
        HttpState::new(
            engine.tool_router().clone(),
            Arc::new(plug_core::session::SessionStore::new(1800, 100)),
            CancellationToken::new(),
        )
        .with_auth(plug_core::http::server::HttpAuth::Oauth(manager)),
    ));

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
    assert_eq!(unauth_card["name"], "io.github.cyberpapiii/plug");
    assert_eq!(
        unauth_card["remotes"][0]["headers"][0]["name"],
        "Authorization"
    );

    let resource_metadata_req = HttpRequest::builder()
        .method("GET")
        .uri("/.well-known/oauth-protected-resource")
        .body(Body::empty())
        .unwrap();
    let resource_metadata_resp = app.clone().oneshot(resource_metadata_req).await.unwrap();
    assert_eq!(resource_metadata_resp.status(), StatusCode::OK);
    let resource_metadata_body = axum::body::to_bytes(resource_metadata_resp.into_body(), 10_000)
        .await
        .unwrap();
    let resource_metadata: serde_json::Value =
        serde_json::from_slice(&resource_metadata_body).unwrap();
    assert_eq!(
        resource_metadata["resource"],
        "https://plug.example.com/mcp"
    );
    assert_eq!(
        resource_metadata["authorization_servers"],
        serde_json::json!(["https://plug.example.com"])
    );

    let verifier = oauth2::PkceCodeVerifier::new("v".repeat(43));
    let challenge = oauth2::PkceCodeChallenge::from_code_verifier_sha256(&verifier);
    let registration_req = HttpRequest::builder()
        .method("POST")
        .uri("/oauth/register")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"client_name":"Local DCR fixture","redirect_uris":["http://localhost/callback"],"token_endpoint_auth_method":"none"}"#,
        ))
        .unwrap();
    let registration_resp = app.clone().oneshot(registration_req).await.unwrap();
    assert_eq!(registration_resp.status(), StatusCode::CREATED);
    let registration_body = axum::body::to_bytes(registration_resp.into_body(), 10_000)
        .await
        .unwrap();
    let registration: serde_json::Value = serde_json::from_slice(&registration_body).unwrap();
    let client_id = registration["client_id"]
        .as_str()
        .expect("registered client ID");
    let authorize_req = HttpRequest::builder()
        .method("GET")
        .uri(format!(
            "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri=http://localhost/callback&state=test-state&code_challenge={}&code_challenge_method=S256&scope=tools%3Aread&resource=https%3A%2F%2Fplug.example.com%2Fmcp",
            challenge.as_str(),
        ))
        .body(Body::empty())
        .unwrap();
    let authorize_resp = app.clone().oneshot(authorize_req).await.unwrap();
    assert_eq!(authorize_resp.status(), StatusCode::OK);
    let consent_body = axum::body::to_bytes(authorize_resp.into_body(), 20_000)
        .await
        .unwrap();
    let consent_html = String::from_utf8(consent_body.to_vec()).unwrap();
    let consent_id = consent_html
        .split("data-consent-id=\"")
        .nth(1)
        .and_then(|value| value.split('\"').next())
        .expect("consent ID");
    // Public WebAuthn ceremony coverage lives in server router tests. This
    // integration test continues from a manager-level grant to focus on the
    // protected discovery and MCP-token boundary it owns.
    let redirect =
        plug_core::downstream_oauth::test_owner::approve_consent(&approval_manager, consent_id)
            .await
            .expect("approve integration-test consent");
    let redirect = reqwest::Url::parse(&redirect.location).expect("parse redirect URL");
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
            "grant_type=authorization_code&code={code}&client_id={client_id}&redirect_uri=http://localhost/callback&code_verifier={}&resource=https%3A%2F%2Fplug.example.com%2Fmcp",
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
    assert!(auth_card.get("servers").is_none());
    assert!(auth_card.get("tools").is_none());
    assert_eq!(auth_card["name"], unauth_card["name"]);

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

#[derive(Clone)]
/// A downstream client that accepts every elicitation and answers every
/// sampling request, as Plug's bridge to it sees it.
struct ReverseRequestTestClient;

impl plug_core::proxy::DownstreamBridge for ReverseRequestTestClient {
    fn create_elicitation(
        &self,
        _request: ElicitRequestParams,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<ElicitResult, McpError>> + Send + '_>> {
        Box::pin(async {
            Ok(ElicitResult::new(ElicitationAction::Accept)
                .with_content(serde_json::json!({"answer": "test-elicitation-response"})))
        })
    }

    fn create_message(
        &self,
        _request: CreateMessageRequestParams,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<CreateMessageResult, McpError>> + Send + '_>>
    {
        Box::pin(async {
            Ok(CreateMessageResult::new(
                SamplingMessage::assistant_text("test-sampling-response"),
                "mock-model".to_string(),
            ))
        })
    }
}

fn mock_server_config_with_reverse_request(tools: &str, reverse_request: &str) -> ServerConfig {
    let mut config = mock_server(tools);
    config
        .args
        .extend(["--reverse-request".to_string(), reverse_request.to_string()]);
    config.call_timeout_secs = 30;
    config
}

#[tokio::test]
async fn test_elicitation_reverse_request_round_trip() {
    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        mock_server_config_with_reverse_request("echo", "elicitation"),
    );

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect_with_bridge(
        engine.tool_router().clone(),
        Arc::new(ReverseRequestTestClient),
    );

    // Verify tools are available
    let tools = client.list_all_tools().await.expect("list tools");
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

#[tokio::test]
async fn test_sampling_reverse_request_round_trip() {
    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        mock_server_config_with_reverse_request("echo", "sampling"),
    );

    let engine = Engine::new(config);
    engine.start().await;

    let client = Downstream::connect_with_bridge(
        engine.tool_router().clone(),
        Arc::new(ReverseRequestTestClient),
    );

    let tools = client.list_all_tools().await.expect("list tools");
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

#[tokio::test]
async fn test_http_elicitation_reverse_request_round_trip() {
    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        mock_server_config_with_reverse_request("echo", "elicitation"),
    );

    let engine = Engine::new(config);
    engine.start().await;

    let state = Arc::new(HttpState::new(
        engine.tool_router().clone(),
        Arc::new(plug_core::session::SessionStore::new(1800, 100)),
        CancellationToken::new(),
    ));
    let app = build_router(state.clone());

    // 1. Initialize with elicitation + sampling capabilities
    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": HTTP_PROTOCOL_VERSION,
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
                if let Some(data) = line.strip_prefix("data: ")
                    && let Ok(json) = serde_json::from_str::<serde_json::Value>(data)
                    && json.get("method").and_then(|m| m.as_str()) == Some("elicitation/create")
                {
                    elicitation_request_id = json.get("id").cloned();
                    return;
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

#[tokio::test]
async fn test_http_sampling_reverse_request_round_trip() {
    let mut config = Config::default();
    config.servers.insert(
        "mock".to_string(),
        mock_server_config_with_reverse_request("echo", "sampling"),
    );

    let engine = Engine::new(config);
    engine.start().await;

    let state = Arc::new(HttpState::new(
        engine.tool_router().clone(),
        Arc::new(plug_core::session::SessionStore::new(1800, 100)),
        CancellationToken::new(),
    ));
    let app = build_router(state.clone());

    // 1. Initialize with sampling capability
    let initialize_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": HTTP_PROTOCOL_VERSION,
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
                if let Some(data) = line.strip_prefix("data: ")
                    && let Ok(json) = serde_json::from_str::<serde_json::Value>(data)
                    && json.get("method").and_then(|m| m.as_str()) == Some("sampling/createMessage")
                {
                    sampling_request_id = json.get("id").cloned();
                    return;
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

// OAuth refresh under load. This test needs two things real wall time cannot give it. The background
// refresh loop in `engine.rs` polls on a 30-second cadence, so observing two of
// its cycles costs a minute of real time; and token expiry is measured against
// `SystemTime`, which `tokio::time::pause()` does not touch, so virtual time
// alone leaves a token that never comes due.
//
// So the test runs on a paused Tokio clock and drives `oauth::advance_test_clock`
// in step with it, keeping the OAuth wall clock and the virtual clock in sync.
// The per-request timeouts in the server config are set far above the refresh
// cadence on purpose: under a paused clock Tokio jumps straight to the earliest
// pending deadline whenever the runtime is idle, so a short call timeout would
// fire the instant a loopback request had to wait for a byte.

#[derive(Default)]
struct RefreshUnderLoadShared {
    /// Every access token this provider has ever issued. A real authorization
    /// server does not invalidate an access token when a refresh mints the next
    /// one — the old token stays good until its own expiry. The other mocks in
    /// this file are stricter than that, which would turn every refresh into a
    /// guaranteed 401 window and make this test assert a property no real
    /// provider requires.
    issued_access_tokens: std::collections::HashSet<String>,
    current_access_token: String,
    current_refresh_token: String,
    refresh_grants: usize,
    unauthorized_calls: usize,
    tool_calls: usize,
}

#[derive(Clone)]
struct RefreshUnderLoadState {
    base_url: String,
    shared: Arc<Mutex<RefreshUnderLoadShared>>,
}

struct MockRefreshUnderLoadProvider {
    base_url: String,
    shared: Arc<Mutex<RefreshUnderLoadShared>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockRefreshUnderLoadProvider {
    async fn start() -> Self {
        let shared = Arc::new(Mutex::new(RefreshUnderLoadShared {
            issued_access_tokens: std::collections::HashSet::from([
                "load-access-token-1".to_string()
            ]),
            current_access_token: "load-access-token-1".to_string(),
            current_refresh_token: "load-refresh-token-1".to_string(),
            ..Default::default()
        }));

        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind refresh-under-load provider");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("provider local addr")
        );

        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(refresh_under_load_metadata_handler),
            )
            .route(
                "/token",
                axum::routing::post(refresh_under_load_token_handler),
            )
            .route("/mcp", axum::routing::post(refresh_under_load_mcp_handler))
            .with_state(RefreshUnderLoadState {
                base_url: base_url.clone(),
                shared: Arc::clone(&shared),
            });

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve refresh-under-load provider");
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

async fn refresh_under_load_metadata_handler(
    State(state): State<RefreshUnderLoadState>,
) -> impl IntoResponse {
    axum::Json(serde_json::json!({
        "issuer": state.base_url,
        "authorization_endpoint": format!("{}/authorize", state.base_url),
        "token_endpoint": format!("{}/token", state.base_url)
    }))
}

async fn refresh_under_load_token_handler(
    State(state): State<RefreshUnderLoadState>,
    Form(params): Form<HashMap<String, String>>,
) -> Response {
    if params.get("grant_type").map(String::as_str) != Some("refresh_token") {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let mut shared = state.shared.lock().await;
    if params.get("refresh_token").map(String::as_str)
        != Some(shared.current_refresh_token.as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    shared.refresh_grants += 1;
    let generation = shared.refresh_grants + 1;
    let access = format!("load-access-token-{generation}");
    let refresh = format!("load-refresh-token-{generation}");
    shared.issued_access_tokens.insert(access.clone());
    shared.current_access_token = access.clone();
    shared.current_refresh_token = refresh.clone();

    axum::Json(serde_json::json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "Bearer",
        "expires_in": 3600,
        "scope": "read"
    }))
    .into_response()
}

async fn refresh_under_load_mcp_handler(
    State(state): State<RefreshUnderLoadState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_string);

    let json_body: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let method = json_body
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");

    {
        let mut shared = state.shared.lock().await;
        let accepted = bearer
            .as_deref()
            .is_some_and(|token| shared.issued_access_tokens.contains(token));
        if !accepted {
            shared.unauthorized_calls += 1;
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if method == "tools/call" {
            shared.tool_calls += 1;
        }
    }

    let session_headers = [
        (
            axum::http::HeaderName::from_static("mcp-session-id"),
            "refresh-under-load-session",
        ),
        (axum::http::header::CONTENT_TYPE, "application/json"),
    ];

    let payload = match method {
        "initialize" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": json_body.get("id"),
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "refresh-under-load", "version": "0.1.0" }
            }
        }),
        "notifications/initialized" => {
            return (StatusCode::ACCEPTED, session_headers, String::new()).into_response();
        }
        "tools/list" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": json_body.get("id"),
            "result": { "tools": [{
                "name": "echo",
                "description": "echo",
                "inputSchema": { "type": "object", "properties": {} }
            }] }
        }),
        "tools/call" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": json_body.get("id"),
            "result": { "content": [{ "type": "text", "text": "ok" }], "isError": false }
        }),
        _ => return (StatusCode::BAD_REQUEST, session_headers, String::new()).into_response(),
    };

    (StatusCode::OK, session_headers, payload.to_string()).into_response()
}

#[tokio::test]
async fn test_oauth_refresh_under_load_no_auth_errors() {
    let _guard = oauth_integration_test_lock().lock().await;
    let provider = MockRefreshUnderLoadProvider::start().await;
    let server_name = format!("oauth-refresh-load-{}", std::process::id());
    let store = oauth::get_or_create_store(&server_name);
    store.clear().await.expect("clear OAuth store before test");
    save_bound_oauth_test_credentials(
        &store,
        &provider.base_url,
        &provider.mcp_url(),
        "load-access-token-1",
        "load-refresh-token-1",
    )
    .await;

    let mut engine: Option<Arc<Engine>> = None;
    let result = AssertUnwindSafe(async {
        let mut config = Config::default();
        config.servers.insert(
            server_name.clone(),
            ServerConfig {
                transport: TransportType::Http,
                url: Some(provider.mcp_url()),
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                // Far above the 30s refresh cadence so a paused-clock jump to
                // the refresh deadline never trips a request timeout.
                timeout_secs: 3600,
                call_timeout_secs: 3600,
                max_concurrent: 4,
                health_check_interval_secs: 3600,
                circuit_breaker_enabled: false,
                ..Default::default()
            },
        );

        let started_engine = Engine::new(config);
        started_engine.start().await;
        engine = Some(Arc::clone(&started_engine));

        let tool_name = started_engine
            .tool_router()
            .list_tools_for_client_session(plug_core::types::ClientType::Unknown, None, None)
            .iter()
            .find(|tool| tool.name.ends_with("__echo"))
            .expect("upstream tool routed")
            .name
            .to_string();

        // Startup runs on the real clock — dialing the upstream involves real
        // loopback I/O, and a paused clock would jump to the first pending
        // deadline the moment the runtime went idle waiting for a byte. Only the
        // refresh cadence needs to be virtualized, so pause after the engine is
        // up and every connect-path timer has already fired.
        tokio::time::pause();

        // Keeps the OAuth wall clock in step with Tokio's virtual clock, so a
        // token minted at virtual T comes due at virtual T + its refresh window
        // exactly as it would in real time.
        let clock = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                oauth::advance_test_clock(1);
            }
        });

        let mut call_failures: Vec<String> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4 * 3600);
        loop {
            let outcome = started_engine
                .tool_router()
                .call_tool(&tool_name, None)
                .await;
            if let Err(error) = outcome {
                call_failures.push(error.to_string());
            }
            let grants = provider.shared.lock().await.refresh_grants;
            if grants >= 2 || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        clock.abort();

        let shared = provider.shared.lock().await;
        assert!(
            shared.refresh_grants >= 2,
            "expected at least two background refresh cycles, saw {}",
            shared.refresh_grants
        );
        assert!(
            shared.tool_calls > 0,
            "the load loop must have reached the upstream at least once"
        );
        assert_eq!(
            shared.unauthorized_calls, 0,
            "no call may be rejected as unauthorized while refreshes run"
        );
        assert!(
            call_failures.is_empty(),
            "tool calls must not fail across refresh cycles: {call_failures:?}"
        );
    })
    .catch_unwind()
    .await;

    if let Some(engine) = engine {
        tokio::time::timeout(Duration::from_secs(60), engine.shutdown())
            .await
            .expect("engine shutdown timed out");
    }
    store.clear().await.expect("clear OAuth store after test");
    oauth::reset_test_clock();
    provider.handle.abort();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

/// An HTTP API named by an OpenAPI document is a server like any other: its
/// operations are listed as tools, a call makes the request with the server's
/// bearer token, and an HTTP failure comes back as a tool error.
#[tokio::test]
async fn an_openapi_document_becomes_a_server() {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind api fixture");
    let address = listener.local_addr().expect("fixture address");

    async fn get_pet(
        axum::extract::Path(id): axum::extract::Path<String>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Response {
        if id == "missing" {
            return (StatusCode::NOT_FOUND, "no such pet").into_response();
        }
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        axum::Json(serde_json::json!({
            "id": id,
            "verbose": query.get("verbose"),
            "authorization": authorization,
            "key": headers.get("x-api-key").and_then(|value| value.to_str().ok()),
        }))
        .into_response()
    }
    async fn create_pet(axum::Json(body): axum::Json<serde_json::Value>) -> Response {
        (StatusCode::CREATED, axum::Json(body)).into_response()
    }
    let app = axum::Router::new()
        .route("/v1/pets/{id}", axum::routing::get(get_pet))
        .route("/v1/pets", axum::routing::post(create_pet));
    let fixture = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let directory = tempfile::tempdir().expect("tempdir");
    let spec_path = directory.path().join("pets.json");
    std::fs::write(
        &spec_path,
        serde_json::json!({
            "openapi": "3.0.3",
            "info": { "title": "Pets", "version": "1.0.0" },
            "servers": [{ "url": format!("http://{address}/v1") }],
            "paths": {
                "/pets/{id}": {
                    "get": {
                        "operationId": "getPet",
                        "parameters": [
                            { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "verbose", "in": "query", "schema": { "type": "boolean" } }
                        ]
                    }
                },
                "/pets": {
                    "post": {
                        "operationId": "createPet",
                        "requestBody": {
                            "required": true,
                            "content": { "application/json": { "schema": { "type": "object" } } }
                        }
                    }
                }
            }
        })
        .to_string(),
    )
    .expect("write document");

    let mut api = ServerConfig {
        transport: TransportType::OpenApi,
        spec: Some(spec_path.to_string_lossy().into_owned()),
        auth_token: Some("api-token".to_string().into()),
        ..Default::default()
    };
    let mut config = Config::default();
    config.servers.insert("pets".to_string(), api.clone());
    assert!(validate_config(&config).is_empty());

    let engine = Engine::new(config);
    engine.start().await;
    let router = engine.tool_router();

    let text = |result: &rmcp::model::CallToolResult| -> String {
        result.content[0]
            .as_text()
            .map(|text| text.text.clone())
            .expect("text content")
    };

    let arguments = serde_json::json!({ "id": "rex", "verbose": true });
    let found = router
        .call_tool("getPet", arguments.as_object().cloned())
        .await
        .expect("call getPet");
    assert_ne!(found.is_error, Some(true));
    let body: serde_json::Value = serde_json::from_str(&text(&found)).expect("json body");
    assert_eq!(body["id"], "rex");
    assert_eq!(body["verbose"], "true");
    assert_eq!(body["authorization"], "Bearer api-token");

    let arguments = serde_json::json!({ "body": { "name": "Rex" } });
    let created = router
        .call_tool("createPet", arguments.as_object().cloned())
        .await
        .expect("call createPet");
    assert_ne!(created.is_error, Some(true));
    let body: serde_json::Value = serde_json::from_str(&text(&created)).expect("json body");
    assert_eq!(body["name"], "Rex");

    let arguments = serde_json::json!({ "id": "missing" });
    let missing = router
        .call_tool("getPet", arguments.as_object().cloned())
        .await
        .expect("call getPet for a missing pet");
    assert_eq!(missing.is_error, Some(true));
    assert!(text(&missing).contains("404"), "{}", text(&missing));

    engine.shutdown().await;

    // The same API, told that its key belongs in a header of its own.
    api.token_in = Some("header:X-API-Key".to_string());
    let mut config = Config::default();
    config.servers.insert("pets".to_string(), api);
    assert!(validate_config(&config).is_empty());
    let engine = Engine::new(config);
    engine.start().await;
    let arguments = serde_json::json!({ "id": "rex" });
    let found = engine
        .tool_router()
        .call_tool("getPet", arguments.as_object().cloned())
        .await
        .expect("call getPet with a key");
    let body: serde_json::Value = serde_json::from_str(&text(&found)).expect("json body");
    assert_eq!(body["key"], "api-token");
    assert_eq!(body["authorization"], "");

    engine.shutdown().await;
    fixture.abort();
}
