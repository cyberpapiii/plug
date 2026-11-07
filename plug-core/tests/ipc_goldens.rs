//! The IPC wire as the app sees it, pinned in `testdata/ipc`.
//!
//! `requests.json` holds every request Plug.app sends and `responses.json`
//! every reply it reads, by the name the app's own golden tests use;
//! `chunked_response.json` holds the frames of a reply too large for one;
//! `cli_requests.json` what only the command line sends.
//! Both sides are checked against the same files: here, that the files are
//! what the daemon's types write and read; in
//! `PlugApp/PlugIPC/Tests/PlugIPCTests/GoldenPayloadTests.swift`, that the
//! app writes the same requests and reads the same replies.
//!
//! A change to the wire fails this test. When the change is meant, run it
//! with `PLUG_REGENERATE_GOLDENS=1` to rewrite the files, and change the app
//! to match.

use std::collections::BTreeMap;
use std::path::PathBuf;

use plug_core::activity::{ActivityEvent, ActivityOutcome};
use plug_core::config::{ServerConfig, StdioSandboxConfig, ToolGroupRule, TransportType};
use plug_core::downstream_identity::{ClientHost, SHARED_REMOTE_CLIENT_KEY};
use plug_core::downstream_oauth::{ClientSource, RegisteredClientSummary};
use plug_core::events::{EventStatus, WatchConfig, WatchHealth};
use plug_core::ipc::{
    ClientBlocks, ClientName, ClientPlace, DaemonOwnershipMode, IpcAuthServerInfo,
    IpcLiveSessionInfo, IpcRequest, IpcResponse, IpcToolInfo, IpcToolRiskInfo, IpcTrustInfo,
    LiveSessionTransport, OffTool, OperatorHandshake, OperatorRequest, OperatorSnapshot,
};
use plug_core::operator::{
    ClientBlockKind, OperatorMutation, OperatorMutationResult, OperatorServerSummary,
};
use plug_core::types::{ClientType, ServerHealth, ServerStatus};

const TOKEN: &str = "golden-auth-token";

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../testdata/ipc")
}

fn operator(request: OperatorRequest) -> IpcRequest {
    IpcRequest::Operator {
        auth_token: TOKEN.to_string().into(),
        request,
    }
}

fn mutate(mutation: OperatorMutation) -> IpcRequest {
    operator(OperatorRequest::Mutate {
        mutation: Box::new(mutation),
        secret_store: None,
    })
}

fn block(kind: ClientBlockKind, target: &str, blocked: bool) -> IpcRequest {
    mutate(OperatorMutation::SetClientBlock {
        key: "claude-code".to_string(),
        kind,
        target: target.to_string(),
        blocked,
    })
}

/// A server with every field the app's editor carries set to something
/// other than its default.
fn stdio_server() -> ServerConfig {
    ServerConfig {
        command: Some("uvx".to_string()),
        args: vec!["workspace-mcp".to_string(), "--single-user".to_string()],
        env: [(
            "WORKSPACE_TOKEN".to_string(),
            "$WORKSPACE_TOKEN".to_string(),
        )]
        .into(),
        enabled: false,
        timeout_secs: 45,
        call_timeout_secs: 600,
        max_concurrent: 2,
        health_check_interval_secs: 120,
        circuit_breaker_enabled: false,
        tool_renames: [("search".to_string(), "find".to_string())].into(),
        tool_groups: vec![ToolGroupRule {
            prefix: "Gmail".to_string(),
            contains: vec!["gmail".to_string()],
            strip: vec!["gmail".to_string()],
        }],
        sandbox: Some(StdioSandboxConfig {
            enabled: true,
            allow_network: true,
            allow_read: vec!["/Users/me/notes".into()],
            allow_write: Vec::new(),
            profile_path: None,
        }),
        ..Default::default()
    }
}

fn remote_server() -> ServerConfig {
    ServerConfig {
        transport: TransportType::Sse,
        url: Some("https://example.com/sse".to_string()),
        auth: Some("oauth".to_string()),
        oauth_client_id: Some("plug".to_string()),
        oauth_scopes: Some(vec!["read".to_string()]),
        ..Default::default()
    }
}

fn api_server() -> ServerConfig {
    ServerConfig {
        transport: TransportType::OpenApi,
        max_concurrent: 4,
        auth_token: Some(plug_core::config::REDACTED_SECRET.to_string().into()),
        spec: Some("https://example.com/openapi.json".to_string()),
        operations: vec!["listPets".to_string()],
        token_in: Some("header:X-Api-Key".to_string()),
        ..Default::default()
    }
}

/// A server with every field set to something other than its default. The
/// literal names every field, so a field added to `ServerConfig` does not
/// compile here until it is given a value, and then shows in
/// `server_config_every_field.json`, which the app's tests require its own
/// `ServerConfig` to model.
fn every_field_server() -> ServerConfig {
    ServerConfig {
        command: Some("uvx".to_string()),
        args: vec!["workspace-mcp".to_string()],
        env: [("TOKEN".to_string(), "$TOKEN".to_string())].into(),
        enabled: false,
        transport: TransportType::Http,
        protocol_mode: plug_core::config::UpstreamProtocolMode::Auto,
        url: Some("https://example.com/mcp".to_string()),
        auth_token: Some(plug_core::config::REDACTED_SECRET.to_string().into()),
        auth: Some("oauth".to_string()),
        oauth_client_id: Some("plug".to_string()),
        oauth_scopes: Some(vec!["read".to_string()]),
        timeout_secs: 45,
        call_timeout_secs: 600,
        max_concurrent: 2,
        health_check_interval_secs: 120,
        circuit_breaker_enabled: false,
        tool_renames: [("search".to_string(), "find".to_string())].into(),
        tool_groups: vec![ToolGroupRule {
            prefix: "Gmail".to_string(),
            contains: vec!["gmail".to_string()],
            strip: vec!["gmail".to_string()],
        }],
        sandbox: Some(StdioSandboxConfig {
            enabled: true,
            allow_network: true,
            allow_read: vec!["/Users/me/notes".into()],
            allow_write: vec!["/tmp".into()],
            profile_path: Some("/Users/me/plug.sb".into()),
        }),
        spec: Some("https://example.com/openapi.json".to_string()),
        operations: vec!["listPets".to_string()],
        token_in: Some("header:X-Api-Key".to_string()),
    }
}

fn requests() -> BTreeMap<&'static str, IpcRequest> {
    BTreeMap::from([
        (
            "handshake",
            IpcRequest::OperatorHandshake {
                client_version: "0.9.0".to_string(),
            },
        ),
        ("status", IpcRequest::Status),
        ("list_tools", IpcRequest::ListTools),
        ("snapshot", operator(OperatorRequest::Snapshot)),
        (
            "activity",
            operator(OperatorRequest::Activity {
                after_sequence: 41,
                limit: 200,
                failures_only: false,
            }),
        ),
        (
            "get_server_config",
            operator(OperatorRequest::GetServerConfig {
                name: "workspace".to_string(),
            }),
        ),
        (
            "validate_server",
            operator(OperatorRequest::ValidateServer {
                name: "workspace".to_string(),
                server: Box::new(stdio_server()),
            }),
        ),
        (
            "describe_api",
            operator(OperatorRequest::DescribeApi {
                spec: "https://example.com/openapi.json".to_string(),
            }),
        ),
        (
            "add_server",
            operator(OperatorRequest::Mutate {
                mutation: Box::new(OperatorMutation::AddServer {
                    name: "workspace".to_string(),
                    server: stdio_server(),
                }),
                secret_store: Some("file".to_string()),
            }),
        ),
        (
            "add_remote_server",
            mutate(OperatorMutation::AddServer {
                name: "notes".to_string(),
                server: remote_server(),
            }),
        ),
        (
            "update_server",
            mutate(OperatorMutation::UpdateServer {
                name: "pets".to_string(),
                server: api_server(),
            }),
        ),
        (
            "remove_server",
            mutate(OperatorMutation::RemoveServer {
                name: "workspace".to_string(),
            }),
        ),
        (
            "import_servers",
            mutate(OperatorMutation::ImportServers {
                servers: vec![plug_core::import::ImportedServer {
                    name: "github".to_string(),
                    source_target: "cursor".to_string(),
                    server: ServerConfig {
                        command: Some("npx".to_string()),
                        args: vec!["-y".to_string(), "github-mcp".to_string()],
                        ..Default::default()
                    },
                }],
            }),
        ),
        (
            "add_account",
            mutate(OperatorMutation::AddAccount {
                server: "workspace".to_string(),
                account: "work".to_string(),
            }),
        ),
        (
            "set_server_enabled",
            mutate(OperatorMutation::SetServerEnabled {
                name: "workspace".to_string(),
                enabled: false,
            }),
        ),
        (
            "set_tool_enabled",
            mutate(OperatorMutation::SetToolEnabled {
                tool: "Gmail__send".to_string(),
                enabled: true,
            }),
        ),
        (
            "rename_client",
            mutate(OperatorMutation::RenameClient {
                key: "oauth:abc".to_string(),
                name: "Work laptop".to_string(),
            }),
        ),
        (
            "move_client_settings",
            mutate(OperatorMutation::MoveClientSettings {
                from: "oauth:old".to_string(),
                to: "oauth:new".to_string(),
            }),
        ),
        (
            "set_client_place",
            mutate(OperatorMutation::SetClientPlace {
                key: "oauth:abc".to_string(),
                place: "Studio Mac".to_string(),
            }),
        ),
        (
            "block_server",
            block(ClientBlockKind::Server, "slack", true),
        ),
        (
            "block_tool",
            block(ClientBlockKind::Tool, "Gmail__send", false),
        ),
        (
            "allow_server",
            block(ClientBlockKind::AllowedServer, "github", true),
        ),
        (
            "allow_tool",
            block(ClientBlockKind::AllowedTool, "GitHub__search", false),
        ),
        ("allow_list", block(ClientBlockKind::AllowList, "*", true)),
        (
            "add_watch",
            mutate(OperatorMutation::AddWatch {
                watch: WatchConfig {
                    name: "inbox".to_string(),
                    server: "workspace".to_string(),
                    tool: "search_gmail_messages".to_string(),
                    arguments: serde_json::json!({"query": "is:unread", "maxResults": 5})
                        .as_object()
                        .cloned()
                        .unwrap(),
                    every_secs: 60,
                    allow_writes: true,
                },
            }),
        ),
        (
            "remove_watch",
            mutate(OperatorMutation::RemoveWatch {
                event: "workspace.inbox".to_string(),
            }),
        ),
        (
            "restart_server",
            operator(OperatorRequest::RestartServer {
                server_id: "workspace".to_string(),
            }),
        ),
        ("reload", operator(OperatorRequest::Reload)),
        (
            "revoke_client",
            operator(OperatorRequest::RevokeDownstreamClient {
                client_id: "abc".to_string(),
            }),
        ),
        ("shutdown", operator(OperatorRequest::Shutdown)),
    ])
}

fn status(name: &str, health: ServerHealth, tool_count: usize) -> ServerStatus {
    // From JSON, so a field added to ServerStatus takes its serde default
    // here instead of breaking the fixture.
    let mut status = serde_json::json!({
        "server_id": name,
        "health": health,
        "tool_count": tool_count,
        "upstream": {
            "name": name,
            "version": "1.0.0",
            "website_url": "https://example.com",
        },
    });
    if health == ServerHealth::Failed {
        status["error"] = serde_json::json!("failed to spawn `npx`: not found");
    }
    serde_json::from_value(status).expect("server status")
}

fn every_health() -> [(&'static str, ServerHealth); 4] {
    // A new ServerHealth fails to compile here until the fixture shows it.
    let _every = |health: ServerHealth| match health {
        ServerHealth::Healthy
        | ServerHealth::Degraded
        | ServerHealth::Failed
        | ServerHealth::AuthRequired => {}
    };
    [
        ("healthy", ServerHealth::Healthy),
        ("degraded", ServerHealth::Degraded),
        ("failed", ServerHealth::Failed),
        ("auth-required", ServerHealth::AuthRequired),
    ]
}

fn every_watch_state() -> [WatchHealth; 6] {
    let _every = |state: WatchHealth| match state {
        WatchHealth::Waiting
        | WatchHealth::Watching
        | WatchHealth::ToolMissing
        | WatchHealth::NotReadOnly
        | WatchHealth::CallFailed
        | WatchHealth::TooLarge => {}
    };
    [
        WatchHealth::Waiting,
        WatchHealth::Watching,
        WatchHealth::ToolMissing,
        WatchHealth::NotReadOnly,
        WatchHealth::CallFailed,
        WatchHealth::TooLarge,
    ]
}

fn snapshot() -> OperatorSnapshot {
    OperatorSnapshot {
        runtime_version: "0.9.0".to_string(),
        uptime_secs: 4_000,
        tool_catalog_revision: 7,
        ownership: DaemonOwnershipMode::AppManaged,
        configured_servers: every_health()
            .iter()
            .map(|(name, health)| OperatorServerSummary {
                name: (*name).to_string(),
                enabled: *health != ServerHealth::Degraded,
                transport: TransportType::Stdio,
                oauth: *health == ServerHealth::AuthRequired,
            })
            .chain([OperatorServerSummary::from_config(
                "notes".to_string(),
                &remote_server(),
            )])
            .collect(),
        servers: every_health()
            .iter()
            .enumerate()
            .map(|(index, (name, health))| status(name, *health, index * 4))
            .collect(),
        live_sessions: vec![
            IpcLiveSessionInfo {
                transport: LiveSessionTransport::DaemonProxy,
                client_id: Some("client-1".to_string()),
                session_id: "session-1".to_string(),
                client_type: ClientType::ClaudeCode,
                client_info: Some("claude-code 2.0".to_string()),
                adapter_version: Some("0.9.0".to_string()),
                host: Some(ClientHost {
                    name: "Claude Code".to_string(),
                    executable: "/opt/homebrew/bin/claude".to_string(),
                    app: None,
                    script: None,
                }),
                grant: None,
                link_target: Some("claude-code".to_string()),
                client_key: Some("claude-code".to_string()),
                access_key: Some("claude-code".to_string()),
                target: Some("claude-code".to_string()),
                connected_secs: 3_700,
                last_activity_secs: Some(2),
            },
            IpcLiveSessionInfo {
                transport: LiveSessionTransport::Http,
                client_id: None,
                session_id: "session-2".to_string(),
                client_type: ClientType::Unknown,
                client_info: Some("ChatGPT".to_string()),
                adapter_version: None,
                host: None,
                grant: Some("abc".to_string()),
                link_target: None,
                client_key: Some("oauth:abc".to_string()),
                access_key: Some("oauth:abc".to_string()),
                target: None,
                connected_secs: 61,
                last_activity_secs: None,
            },
        ],
        upstream_auth: vec![IpcAuthServerInfo {
            name: "auth-required".to_string(),
            url: Some("https://example.com/mcp".to_string()),
            authenticated: false,
            health: ServerHealth::AuthRequired,
            scopes: Some(vec!["read".to_string()]),
            token_expires_in_secs: Some(7_300),
            warnings: vec!["token expires soon".to_string()],
        }],
        downstream_clients: vec![RegisteredClientSummary {
            client_id: "abc".to_string(),
            client_name: "ChatGPT".to_string(),
            redirect_uris: vec!["https://chatgpt.com/callback".to_string()],
            source: ClientSource::DynamicRegistration,
            created_at: 1_700_000_000,
            last_used_at: Some(1_700_000_500),
            expires_at: 1_800_000_000,
            signed_in: true,
            client_key: "oauth:abc".to_string(),
            target: None,
        }],
        client_names: vec![ClientName {
            key: "oauth:abc".to_string(),
            name: "Work laptop".to_string(),
        }],
        client_places: vec![ClientPlace {
            key: "oauth:abc".to_string(),
            place: "Studio Mac".to_string(),
        }],
        client_blocks: vec![
            ClientBlocks {
                key: "claude-code".to_string(),
                servers: vec!["degraded".to_string()],
                tools: vec!["healthy__send".to_string(), "failed__*".to_string()],
                only_allowed: false,
                allowed_servers: Vec::new(),
                allowed_tools: Vec::new(),
                off_servers: vec!["degraded".to_string()],
                off_tools: vec![
                    OffTool {
                        name: "healthy__send".to_string(),
                        rule: None,
                    },
                    OffTool {
                        name: "failed__read".to_string(),
                        rule: Some("failed__*".to_string()),
                    },
                ],
            },
            ClientBlocks {
                key: "oauth:abc".to_string(),
                servers: Vec::new(),
                tools: Vec::new(),
                only_allowed: true,
                allowed_servers: vec!["healthy".to_string()],
                allowed_tools: vec!["failed__read".to_string()],
                off_servers: vec!["degraded".to_string(), "notes".to_string()],
                off_tools: vec![OffTool {
                    name: "failed__write".to_string(),
                    rule: None,
                }],
            },
        ],
        events: every_watch_state()
            .into_iter()
            .enumerate()
            .map(|(index, state)| EventStatus {
                name: format!("healthy.watch{index}"),
                server: "healthy".to_string(),
                tool: Some("read".to_string()),
                every_secs: Some(300),
                state,
                last_checked: Some(1_700_000_100 + index as u64),
                last_changed: (index % 2 == 0).then_some(1_700_000_000),
                subscribers: index,
            })
            .chain([EventStatus {
                name: "slack.message".to_string(),
                server: "slack".to_string(),
                tool: None,
                every_secs: None,
                state: WatchHealth::Watching,
                last_checked: None,
                last_changed: None,
                subscribers: 1,
            }])
            .collect(),
        client_address: Some("https://plug.example.com/mcp".to_string()),
        // Not the default, so the app is seen reading it.
        prefix_delimiter: ".".to_string(),
    }
}

fn tool(name: &str, server: &str) -> IpcToolInfo {
    IpcToolInfo {
        name: name.to_string(),
        server_id: server.to_string(),
        description: Some("Send a message.".to_string()),
        title: Some("Send".to_string()),
        disabled: false,
        disabled_by_pattern: None,
        icons: None,
        own_name: Some("send".to_string()),
        read_only: false,
        risk: IpcToolRiskInfo::default(),
        source: None,
        upstream: None,
        trust: IpcTrustInfo::default(),
    }
}

fn tools() -> IpcResponse {
    IpcResponse::Tools {
        tools: vec![
            tool("healthy__send", "healthy"),
            IpcToolInfo {
                disabled: true,
                disabled_by_pattern: Some("failed__*".to_string()),
                read_only: true,
                description: None,
                title: None,
                own_name: Some("read".to_string()),
                ..tool("failed__read", "failed")
            },
        ],
    }
}

fn reload_report() -> plug_core::reload::ReloadReport {
    plug_core::reload::ReloadReport {
        added: vec!["notes".to_string()],
        removed: vec!["old".to_string()],
        changed: vec!["workspace".to_string()],
        unchanged: vec!["healthy".to_string()],
        settings_changed: true,
        restart_required: vec!["http.port (restart required)".to_string()],
        errors: vec!["pets: spec unreadable".to_string()],
    }
}

/// Requests only the command line sends, so the app's golden tests leave
/// them out.
fn cli_requests() -> BTreeMap<&'static str, IpcRequest> {
    BTreeMap::from([(
        "set_lazy_tools",
        mutate(OperatorMutation::SetLazyTools {
            client: "claude-code".to_string(),
            setting: plug_core::types::LazyToolSetting::Bridge,
        }),
    )])
}

fn responses() -> BTreeMap<&'static str, IpcResponse> {
    BTreeMap::from([
        (
            "handshake",
            IpcResponse::OperatorHandshake {
                handshake: OperatorHandshake::new(
                    "0.9.0".to_string(),
                    Some("/Applications/Plug.app/Contents/Resources/plug".into()),
                    DaemonOwnershipMode::AppManaged,
                ),
            },
        ),
        (
            "snapshot",
            IpcResponse::OperatorSnapshot {
                snapshot: Box::new(snapshot()),
            },
        ),
        (
            "status",
            IpcResponse::Status {
                servers: vec![status("healthy", ServerHealth::Healthy, 3)],
                clients: 2,
                uptime_secs: 4_000,
                runtime_version: "0.9.0".to_string(),
                resource_subscriptions: 1,
            },
        ),
        (
            "server_config",
            IpcResponse::ServerConfig {
                name: "workspace".to_string(),
                server: Box::new(stdio_server()),
            },
        ),
        (
            "api_server_config",
            IpcResponse::ServerConfig {
                name: "pets".to_string(),
                server: Box::new(api_server()),
            },
        ),
        (
            "remote_server_config",
            IpcResponse::ServerConfig {
                name: "notes".to_string(),
                server: Box::new(remote_server()),
            },
        ),
        (
            "activity",
            IpcResponse::ActivitySnapshot {
                events: vec![
                    ActivityEvent {
                        sequence: 42,
                        occurred_at_ms: 1_700_000_000_000,
                        client: Some("client-1".to_string()),
                        server: Some("healthy".to_string()),
                        method: "tools/call".to_string(),
                        tool: Some("healthy__send".to_string()),
                        client_type: Some("claude-code".to_string()),
                        client_label: Some("claude-code 2.0".to_string()),
                        client_key: Some("claude-code".to_string()),
                        target: Some("claude-code".to_string()),
                        remote: false,
                        latency_ms: 120,
                        outcome: ActivityOutcome::Success,
                        reason: None,
                    },
                    ActivityEvent {
                        sequence: 43,
                        occurred_at_ms: 1_700_000_001_000,
                        client: None,
                        server: Some("failed".to_string()),
                        method: "tools/call".to_string(),
                        tool: Some("failed__read".to_string()),
                        client_type: None,
                        client_label: None,
                        client_key: Some("oauth:abc".to_string()),
                        target: None,
                        remote: true,
                        latency_ms: 30_000,
                        outcome: ActivityOutcome::Error,
                        reason: Some("upstream timed out".to_string()),
                    },
                    ActivityEvent {
                        sequence: 44,
                        occurred_at_ms: 1_700_000_002_000,
                        client: None,
                        server: None,
                        method: "tools/list".to_string(),
                        tool: None,
                        client_type: None,
                        client_label: None,
                        client_key: Some(SHARED_REMOTE_CLIENT_KEY.to_string()),
                        target: None,
                        remote: true,
                        latency_ms: 3,
                        outcome: ActivityOutcome::Cancelled,
                        reason: None,
                    },
                ],
            },
        ),
        ("tools", tools()),
        (
            "server_validated",
            IpcResponse::ServerValidated {
                server: OperatorServerSummary::from_config(
                    "workspace".to_string(),
                    &stdio_server(),
                ),
            },
        ),
        (
            "api_described",
            IpcResponse::ApiDescribed {
                api: serde_json::from_value(serde_json::json!({
                    "title": "Pets",
                    "operations": [
                        {"name": "listPets", "method": "GET", "path": "/pets", "summary": "List pets", "tag": "pets"},
                        {"name": "addPet", "method": "POST", "path": "/pets", "summary": "", "tag": null},
                    ],
                }))
                .expect("api summary"),
            },
        ),
        (
            "mutation",
            IpcResponse::OperatorMutation {
                result: OperatorMutationResult {
                    server: Some(OperatorServerSummary::from_config(
                        "workspace".to_string(),
                        &stdio_server(),
                    )),
                    disabled_tools: Some(vec!["failed__*".to_string()]),
                    imported: None,
                },
                reload: reload_report(),
            },
        ),
        (
            "imported",
            IpcResponse::OperatorMutation {
                result: OperatorMutationResult {
                    server: None,
                    disabled_tools: None,
                    imported: Some(vec![OperatorServerSummary::from_config(
                        "github-cursor".to_string(),
                        &ServerConfig::default(),
                    )]),
                },
                reload: reload_report(),
            },
        ),
        (
            "revoked",
            IpcResponse::DownstreamClientRevoked {
                client_id: "abc".to_string(),
            },
        ),
        (
            "reloaded",
            IpcResponse::Reloaded {
                report: reload_report(),
            },
        ),
        ("ok", IpcResponse::Ok),
        (
            "error",
            IpcResponse::Error {
                code: "OPERATOR_MUTATION_FAILED".to_string(),
                message: "unknown server `gone`".to_string(),
            },
        ),
    ])
}

/// The frames of the `tools` reply sent in chunks of 64 bytes. The daemon
/// chunks only a reply larger than one frame, in much larger chunks, but the
/// envelopes are the same.
fn chunked_response() -> Vec<plug_core::ipc::DaemonToProxyMessage> {
    let payload = serde_json::to_vec(&tools()).expect("tools serialize");
    plug_core::ipc::response_chunks(&payload, 64).collect()
}

fn pretty(value: &impl serde::Serialize) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(value).expect("golden serializes")
    )
}

/// Compare `contents` with the golden file `name`, or write it there when
/// regenerating.
fn check(name: &str, contents: String) {
    let path = dir().join(name);
    if std::env::var_os("PLUG_REGENERATE_GOLDENS").is_some() {
        std::fs::write(&path, contents).unwrap_or_else(|e| panic!("write {name}: {e}"));
        return;
    }
    let golden =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read golden {name}: {e}"));
    assert!(
        golden == contents,
        "testdata/ipc/{name} is not what the daemon's types write. If the wire is meant to \
         change, rerun with PLUG_REGENERATE_GOLDENS=1 and change the app to match."
    );
}

#[test]
fn the_goldens_are_what_the_daemon_writes() {
    check("requests.json", pretty(&requests()));
    check("responses.json", pretty(&responses()));
    check("cli_requests.json", pretty(&cli_requests()));
    check("chunked_response.json", pretty(&chunked_response()));
    // What the daemon fills in for a field a server leaves out, which is how
    // the app sends a setting the person did not set.
    check(
        "server_config_default.json",
        pretty(&ServerConfig::default()),
    );
    check(
        "server_config_every_field.json",
        pretty(&every_field_server()),
    );

    let mut files: Vec<String> = std::fs::read_dir(dir())
        .expect("testdata/ipc")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "chunked_response.json",
            "cli_requests.json",
            "requests.json",
            "responses.json",
            "server_config_default.json",
            "server_config_every_field.json"
        ],
        "testdata/ipc holds a file nothing checks"
    );
}

/// The daemon reads every request the app and the command line send as
/// what it was written from.
#[test]
fn the_daemon_reads_every_request_golden() {
    let golden = |name: &str| -> BTreeMap<String, serde_json::Value> {
        serde_json::from_str(&std::fs::read_to_string(dir().join(name)).unwrap()).unwrap()
    };
    for (name, value) in golden("requests.json")
        .into_iter()
        .chain(golden("cli_requests.json"))
    {
        let request: IpcRequest = serde_json::from_value(value.clone())
            .unwrap_or_else(|e| panic!("request {name} does not parse: {e}"));
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            value,
            "request {name}"
        );
    }
}

/// Requests the app never sends: `plug connect` and the command line send
/// these, so `requests.json` has no golden for them.
const NOT_SENT_BY_THE_APP: &[&str] = &[
    "AuthStatus",
    "CancelMcpRequest",
    "Capabilities",
    "Deregister",
    "ListLiveSessions",
    "McpRequest",
    "ModernDownstreamGate",
    "Ping",
    "Register",
    "RestoreResourceSubscriptions",
    "UpdateCapabilities",
    "UpdateRoots",
    "UpdateSession",
];

/// Every variant of `T` by its wire name, as serde lists them when it meets
/// one it does not know: "unknown variant `x`, expected one of `A`, `B`".
fn wire_names<T: serde::de::DeserializeOwned>(unknown: serde_json::Value) -> Vec<String> {
    let error = serde_json::from_value::<T>(unknown)
        .err()
        .expect("an unknown variant is refused")
        .to_string();
    let (_, expected) = error
        .split_once("expected")
        .unwrap_or_else(|| panic!("serde names the variants it expected: {error}"));
    expected
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// A request, operator request, change or block kind added to the wire
/// without a golden fails here: in `requests.json` when the app sends it, in
/// `cli_requests.json` when only the command line does.
#[test]
fn every_request_has_a_golden() {
    let golden: BTreeMap<String, serde_json::Value> = ["requests.json", "cli_requests.json"]
        .into_iter()
        .flat_map(|file| {
            serde_json::from_str::<BTreeMap<String, serde_json::Value>>(
                &std::fs::read_to_string(dir().join(file)).unwrap(),
            )
            .unwrap()
        })
        .collect();
    let shown = |pointer: &str| -> std::collections::BTreeSet<String> {
        golden
            .values()
            .filter_map(|request| request.pointer(pointer)?.as_str().map(str::to_string))
            .collect()
    };
    let unknown_tag = serde_json::json!({"type": "\u{0}"});

    let requests = wire_names::<IpcRequest>(unknown_tag.clone());
    for name in NOT_SENT_BY_THE_APP {
        assert!(
            requests.iter().any(|request| request == name),
            "{name} is not a request"
        );
    }
    let sent: std::collections::BTreeSet<String> = requests
        .into_iter()
        .filter(|request| !NOT_SENT_BY_THE_APP.contains(&request.as_str()))
        .collect();
    assert_eq!(shown("/type"), sent, "IpcRequest");
    assert_eq!(
        shown("/request/type"),
        wire_names::<OperatorRequest>(unknown_tag.clone())
            .into_iter()
            .collect(),
        "OperatorRequest"
    );
    assert_eq!(
        shown("/request/mutation/type"),
        wire_names::<OperatorMutation>(unknown_tag)
            .into_iter()
            .collect(),
        "OperatorMutation"
    );
    assert_eq!(
        shown("/request/mutation/kind"),
        wire_names::<ClientBlockKind>(serde_json::json!("\u{0}"))
            .into_iter()
            .collect(),
        "ClientBlockKind"
    );
}

/// Every field of the every-field server is written, and none with its
/// default, so the app's test of it covers every setting.
#[test]
fn the_every_field_server_sets_every_field() {
    let every = serde_json::to_value(every_field_server()).unwrap();
    let default = serde_json::to_value(ServerConfig::default()).unwrap();
    let every = every.as_object().unwrap();
    for (key, value) in default.as_object().unwrap() {
        assert_ne!(every.get(key), Some(value), "{key} is left at its default");
    }
    for key in ["spec", "operations", "token_in"] {
        assert!(every.contains_key(key), "{key} is not written");
    }
}

/// What a client reads off the frames, reassembled, is the reply.
#[test]
fn the_chunked_golden_reassembles_into_the_reply() {
    let mut decoder = plug_core::ipc::DaemonFrameDecoder::default();
    let frames = chunked_response();
    assert!(frames.len() > 2, "the fixture shows several chunks");
    let mut reply = None;
    for frame in &frames {
        reply = decoder
            .decode(&serde_json::to_vec(frame).unwrap())
            .expect("chunk decodes");
    }
    match reply {
        Some(plug_core::ipc::DaemonFrame::Reply {
            response: IpcResponse::Tools { tools },
            ..
        }) => assert_eq!(tools.len(), 2),
        other => panic!("expected the tools reply, got {other:?}"),
    }
}
