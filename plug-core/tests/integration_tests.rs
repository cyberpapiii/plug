#![forbid(unsafe_code)]

//! Integration tests for plug-core.
//!
//! These tests exercise the ProxyHandler, client detection, and config
//! loading at the unit level without spawning child processes.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request as HttpRequest;
use plug_core::client_detect::detect_client;
use plug_core::config::{Config, ServerConfig, TransportType, validate_config};
use plug_core::engine::Engine;
use plug_core::http::server::{HttpState, build_router};
use plug_core::http::session::SessionManager;
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
    SamplingMessage, UrlElicitationCapability,
};
use rmcp::service::RequestContext;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const HTTP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";
const HTTP_PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";
const HTTP_PROTOCOL_VERSION: &str = "2025-11-25";

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
        sse_channel_capacity: 32,
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
    /// Captured method name + optional MCP-Protocol-Version header value.
    type CapturedHeaders = Vec<(String, Option<String>)>;

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
            .push((method.clone(), protocol_version));

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
        auth: None,
        oauth_client_id: None,
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
        .start_server("mock-http", &config)
        .await
        .expect("connect to mock HTTP upstream");

    // start_server does: initialize → notifications/initialized → tools/list.
    // After it returns, all three requests have been made.

    let captured = mock_state.captured.lock().await;

    // initialize: should NOT have MCP-Protocol-Version (version unknown yet)
    let init = captured.iter().find(|(m, _)| m == "initialize");
    assert!(init.is_some(), "should have captured initialize request");
    assert_eq!(
        init.unwrap().1,
        None,
        "initialize must not send MCP-Protocol-Version (version not yet negotiated)"
    );

    // notifications/initialized: SHOULD have MCP-Protocol-Version
    let initialized = captured
        .iter()
        .find(|(m, _)| m == "notifications/initialized");
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
    let tools_list = captured.iter().find(|(m, _)| m == "tools/list");
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
    server_handle.abort();
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
        sse_channel_capacity: 32,
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
        sse_channel_capacity: 32,
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
