#![forbid(unsafe_code)]

//! Integration tests for plug-core.
//!
//! These tests exercise the ProxyHandler, client detection, and config
//! loading at the unit level without spawning child processes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use plug_core::client_detect::detect_client;
use plug_core::config::{Config, ServerConfig, TransportType, validate_config};
use plug_core::engine::Engine;
use plug_core::proxy::ProxyHandler;
use plug_core::server::ServerManager;
use plug_core::types::ClientType;
use rmcp::handler::server::ServerHandler;

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
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        },
    );
    handler.refresh_tools().await;

    // Verify the handler still works (get_info returns valid info)
    let info = handler.get_info();
    assert!(info.capabilities.tools.is_some());
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
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        },
    );
    let info = handler.get_info();

    assert_eq!(info.server_info.name, "plug");
    assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    assert!(info.capabilities.tools.is_some());
    assert_eq!(
        info.capabilities.tools.as_ref().unwrap().list_changed,
        Some(true)
    );
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
            tool_filter_enabled: true,
            enrichment_servers: std::collections::HashSet::new(),
        },
    );
    let info = handler.get_info();

    assert!(
        info.capabilities.resources.is_some(),
        "resources capability should be advertised"
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
            Some(serde_json::json!({"input": "ok"}).as_object().unwrap().clone()),
        )
        .await;
    assert!(second.is_ok(), "second call should succeed after reconnect");

    engine.shutdown().await;
    let _ = std::fs::remove_dir_all(&temp);
}
