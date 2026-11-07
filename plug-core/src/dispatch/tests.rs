use std::collections::HashMap;

use rmcp::model::{CallToolRequestParams, InitializeResult, NumberOrString};

use super::*;
use crate::proxy::{RouterConfig, RouterSnapshot, build_meta_tools};
use crate::server::ServerManager;

fn router() -> Arc<ToolRouter> {
    Arc::new(ToolRouter::new(
        Arc::new(ServerManager::new()),
        RouterConfig::default(),
    ))
}

fn caller(protocol_version: &str) -> IpcContext {
    let mut ctx = IpcContext::new("session-1", RequestId::Number(1));
    ctx.registry_client_id = Some(Arc::from("client-1"));
    ctx.protocol_version = Arc::from(protocol_version);
    ctx.modern_enabled = true;
    ctx
}

#[test]
fn an_ipc_caller_keeps_its_protocol_client_and_principal() {
    let mut ctx = caller(crate::protocol::MODERN_PROTOCOL_VERSION);
    ctx.client_metadata = Some(crate::protocol::ClientMetadata {
        name: Arc::from("modern-client"),
        version: Arc::from("2.0"),
    });
    let call = ctx.downstream_call_context();

    assert_eq!(call.request_id, RequestId::Number(1));
    assert_eq!(call.protocol_era, ProtocolEra::Modern);
    assert!(call.modern_direction_enabled);
    assert_eq!(call.protocol_version.as_ref(), "2026-07-28");
    assert_eq!(
        call.principal(),
        Some(&crate::downstream_identity::ipc_registry_principal(
            "client-1"
        ))
    );
    let metadata = call.client_metadata.expect("client metadata");
    assert_eq!(metadata.name.as_ref(), "modern-client");
    assert_eq!(metadata.version.as_ref(), "2.0");
}

#[tokio::test]
async fn a_modern_request_is_refused_while_the_gate_is_shut() {
    let router = router();
    let error = handle(
        &router,
        &caller(crate::protocol::MODERN_PROTOCOL_VERSION),
        "tools/list",
        None,
    )
    .await
    .expect_err("gate is shut");
    assert_eq!(error.code, ErrorCode::UNSUPPORTED_PROTOCOL_VERSION);
}

#[tokio::test]
async fn each_era_answers_only_its_own_methods() {
    let router = router();
    router.set_modern_downstream_enabled(true);
    let legacy = caller(crate::protocol::SUPPORTED_PROTOCOL_VERSION);
    let modern = caller(crate::protocol::MODERN_PROTOCOL_VERSION);

    for method in [
        "initialize",
        "ping",
        "resources/subscribe",
        "resources/unsubscribe",
        "logging/setLevel",
    ] {
        let error = handle(&router, &modern, method, Some(serde_json::json!({})))
            .await
            .expect_err(method);
        assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND, "{method}");
    }
    let error = handle(&router, &legacy, "server/discover", None)
        .await
        .expect_err("discovery is modern");
    assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);
    for ctx in [&legacy, &modern] {
        let error = handle(&router, ctx, "subscriptions/listen", None)
            .await
            .expect_err("unknown method");
        assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);
    }

    assert_eq!(
        handle(&router, &legacy, "ping", None).await.unwrap(),
        serde_json::json!({})
    );
    let discovered = handle(&router, &modern, "server/discover", None)
        .await
        .unwrap();
    assert_eq!(
        discovered["supportedVersions"],
        serde_json::json!(["2025-11-25", "2026-07-28"])
    );
}

// A result proxied from a `2025-11-25` upstream carries no `resultType`;
// `2026-07-28` makes it mandatory and a strict modern client rejects the
// response without it. A legacy peer must never see it, nor the modern
// catalog cache directives.
#[tokio::test]
async fn results_speak_the_callers_era() {
    let router = router();
    router.set_modern_downstream_enabled(true);

    let legacy = handle(
        &router,
        &caller(crate::protocol::SUPPORTED_PROTOCOL_VERSION),
        "tools/list",
        None,
    )
    .await
    .unwrap();
    for key in ["resultType", "ttlMs", "cacheScope"] {
        assert!(legacy.get(key).is_none(), "legacy result carries {key}");
    }

    let modern = handle(
        &router,
        &caller(crate::protocol::MODERN_PROTOCOL_VERSION),
        "prompts/list",
        None,
    )
    .await
    .unwrap();
    assert_eq!(modern["resultType"], "complete");
}

#[tokio::test]
async fn malformed_params_are_invalid_params() {
    let router = router();
    let legacy = caller(crate::protocol::SUPPORTED_PROTOCOL_VERSION);
    for (method, params) in [
        ("resources/read", None),
        ("prompts/get", Some(serde_json::json!({ "name": 7 }))),
        ("tools/call", None),
        (
            "logging/setLevel",
            Some(serde_json::json!({ "level": "loud" })),
        ),
        ("tasks/get", Some(serde_json::json!({}))),
    ] {
        let error = handle(&router, &legacy, method, params)
            .await
            .expect_err(method);
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS, "{method}");
    }
}

#[test]
fn the_modern_projection_advertises_only_what_the_modern_era_serves() {
    let mut source = ServerCapabilities::builder()
        .enable_tools()
        .enable_resources()
        .enable_resources_subscribe()
        .enable_logging()
        .build();
    source.experimental = Some(
        [(
            crate::protocol::LEGACY_TASKS_CAPABILITY_KEY.to_string(),
            serde_json::Map::new(),
        )]
        .into_iter()
        .collect(),
    );

    let legacy = caller(crate::protocol::SUPPORTED_PROTOCOL_VERSION).downstream_call_context();
    assert_eq!(project_capabilities(source.clone(), &legacy, true), source);

    let modern = caller(crate::protocol::MODERN_PROTOCOL_VERSION).downstream_call_context();
    let projected = project_capabilities(source.clone(), &modern, false);
    assert!(projected.tools.is_some());
    assert!(projected.logging.is_none());
    assert!(projected.experimental.is_none());
    assert!(projected.extensions.is_none());
    assert_eq!(projected.resources.as_ref().and_then(|r| r.subscribe), None);
    assert!(
        project_capabilities(source, &modern, true)
            .extensions
            .is_some_and(|extensions| extensions.contains_key(rmcp::model::TASKS_EXTENSION_ID))
    );
}

#[tokio::test]
async fn initialize_answers_with_plugs_identity() {
    let router = Arc::new(ToolRouter::new(
        Arc::new(ServerManager::new()),
        RouterConfig::default(),
    ));
    let value = handle(
        &router,
        &local_caller(),
        "initialize",
        Some(serde_json::json!({})),
    )
    .await
    .expect("initialize");
    let info: InitializeResult = serde_json::from_value(value).expect("initialize result");

    assert_eq!(info.server_info.name, "plug");
    assert_eq!(info.server_info.title.as_deref(), Some("Plug"));
    assert_eq!(
        info.server_info.description.as_deref(),
        Some("MCP multiplexer")
    );
    assert_eq!(
        info.server_info.website_url.as_deref(),
        Some("https://github.com/cyberpapiii/plug")
    );
    let icons = info.server_info.icons.as_ref().expect("plug icons");
    assert_plug_icons_sequence(icons);
    assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        info.protocol_version.as_str(),
        crate::protocol::SUPPORTED_PROTOCOL_VERSION
    );
    assert!(info.capabilities.tools.is_none());
    assert!(info.capabilities.resources.is_none());
}

// ── dispatch: tools/call ─────────────────────────────────────────────────

fn local_caller() -> IpcContext {
    let mut ctx = IpcContext::new("test-client", RequestId::from(NumberOrString::Number(1)));
    ctx.registry_client_id = Some(Arc::from("test-owner"));
    ctx
}

async fn dispatch_call(
    router: &Arc<ToolRouter>,
    ctx: &dyn DownstreamContext,
    params: CallToolRequestParams,
) -> Result<serde_json::Value, McpError> {
    handle(
        router,
        ctx,
        "tools/call",
        Some(serde_json::to_value(params).expect("params serialize")),
    )
    .await
}

/// OAuth-backed transport context whose tool scope is intentionally narrower
/// than its task scope. Task-wrapped tools/call must pass both checks.
struct ScopedTaskDownstream {
    scopes: std::collections::BTreeSet<String>,
}

impl DownstreamContext for ScopedTaskDownstream {
    fn downstream_call_context(&self) -> DownstreamCallContext {
        DownstreamCallContext::http(
            "session-scoped",
            RequestId::from(NumberOrString::Number(1)),
            ClientType::Unknown,
        )
        .with_trace_id(Arc::<str>::from("00000000000000000000000000000001"))
        .with_authorization(
            crate::types::PrincipalId::downstream_oauth(
                "https://issuer.example",
                "client-a",
                "https://plug.example/mcp",
            ),
            self.scopes.clone(),
        )
    }

    fn session_bound(&self) -> bool {
        false
    }

    fn task_owner(&self) -> Result<TaskOwner, McpError> {
        Ok(TaskOwner::new(
            crate::types::PrincipalId::downstream_oauth(
                "https://issuer.example",
                "client-a",
                "https://plug.example/mcp",
            )
            .owner_key(),
        ))
    }
}

/// Build a router with a single route to a server that has no registered
/// upstream. The sync path then fails with `ServerUnavailable`, while the task
/// path falls through to a local passthrough task and succeeds — making the two
/// branches observably distinct.
fn router_with_unrouted_single_route() -> Arc<ToolRouter> {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(sm, RouterConfig::default()));
    let mut routes = HashMap::new();
    routes.insert(
        "Mock__tool".to_string(),
        ("mock-server".to_string(), "tool".to_string()),
    );
    router.replace_snapshot(RouterSnapshot {
        routes,
        tools_all: Arc::new(Vec::new()),
        meta_tools_all: Arc::new(build_meta_tools()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        routes_lower: HashMap::new(),
        tools_by_name: HashMap::new(),
        tools_by_name_lower: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    });
    router
}

#[tokio::test]
async fn dispatch_tools_call_empty_name_returns_tool_not_found() {
    let router = router_with_unrouted_single_route();
    let ctx = local_caller();
    let params = CallToolRequestParams::new("");

    let err = dispatch_call(&router, &ctx, params)
        .await
        .expect_err("empty tool name must error");

    // An unknown/empty tool name routes to ToolNotFound, METHOD_NOT_FOUND.
    assert_eq!(err.code, ErrorCode::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn dispatch_tools_call_rejects_hostile_extensions_before_route_effects() {
    let router = router_with_unrouted_single_route();
    let ctx = local_caller();

    for (key, value) in [
        ("plug.dev/legacy-task-support", serde_json::json!(true)),
        (
            "attacker.example/authorization",
            serde_json::json!("Bearer stolen"),
        ),
        ("attacker.example/value", serde_json::json!("Bearer stolen")),
    ] {
        let mut params = CallToolRequestParams::new("Mock__tool");
        params
            .meta
            .get_or_insert_with(Default::default)
            .insert(key.to_string(), value);
        let error = dispatch_call(&router, &ctx, params)
            .await
            .expect_err("hostile metadata must fail before route lookup");
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS, "hostile key: {key}");
        assert!(
            !error.message.to_lowercase().contains("unavailable"),
            "a route effect occurred before extension admission for {key}"
        );
    }
}

#[tokio::test]
async fn dispatch_tools_call_rejects_oversized_request_state_before_route_effects() {
    let router = router_with_unrouted_single_route();
    let ctx = local_caller();
    let mut params = CallToolRequestParams::new("Mock__tool");
    params.request_state = Some("x".repeat(4 * 1024 + 1));

    let error = dispatch_call(&router, &ctx, params)
        .await
        .expect_err("oversized requestState must fail before route lookup");
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert!(!error.message.to_lowercase().contains("unavailable"));
}

#[tokio::test]
async fn hostile_task_extension_is_rejected_before_durable_record_creation() {
    let router = router_with_unrouted_single_route();
    let ctx = local_caller();
    let mut params = CallToolRequestParams::new("Mock__tool");
    let meta = params.meta.get_or_insert_with(Default::default);
    meta.insert(
        crate::protocol::LEGACY_TASK_REQUEST_KEY.to_string(),
        serde_json::json!({}),
    );
    meta.insert(
        "attacker.example/principal".to_string(),
        serde_json::json!("someone-else"),
    );

    let error = dispatch_call(&router, &ctx, params)
        .await
        .expect_err("hostile task metadata must fail before durable creation");
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(
        router
            .task_count_for_owner(&DownstreamContext::task_owner(&ctx).expect("owner"))
            .await,
        0
    );
}

#[tokio::test]
async fn dispatch_tools_call_task_param_with_task_support_creates_task() {
    let router = router_with_unrouted_single_route();
    let ctx = local_caller();
    let mut params = CallToolRequestParams::new("Mock__tool");
    params.meta.get_or_insert_with(Default::default).insert(
        crate::protocol::LEGACY_TASK_REQUEST_KEY.to_string(),
        serde_json::json!({}),
    );

    let outcome = dispatch_call(&router, &ctx, params)
        .await
        .expect("task-augmented call should enqueue a local passthrough task");

    assert!(
        outcome.get("task").is_some(),
        "expected a created task, got {outcome:?}"
    );
    let activity = router.activity_snapshot(&crate::activity::ActivityFilter::default());
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].method, "tools/call");
    assert_eq!(
        activity[0].outcome,
        crate::activity::ActivityOutcome::Success
    );
    assert!(!activity[0].remote, "a connector call comes from this Mac");
}

#[tokio::test]
async fn dispatch_tools_call_task_param_requires_task_scope_before_creating_record() {
    let router = router_with_unrouted_single_route();
    let ctx = ScopedTaskDownstream {
        scopes: std::collections::BTreeSet::from(["tools:read".to_string()]),
    };
    let owner = DownstreamContext::task_owner(&ctx).expect("stable principal owner");
    let mut params = CallToolRequestParams::new("Mock__tool");
    params.meta.get_or_insert_with(Default::default).insert(
        crate::protocol::LEGACY_TASK_REQUEST_KEY.to_string(),
        serde_json::json!({}),
    );

    let err = dispatch_call(&router, &ctx, params)
        .await
        .expect_err("tools scope alone must not authorize durable task creation");

    assert_eq!(err.code, ErrorCode(-32005));
    assert_eq!(router.task_count_for_owner(&owner).await, 0);
    let activity = router.activity_snapshot(&crate::activity::ActivityFilter::default());
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].outcome, crate::activity::ActivityOutcome::Error);
    assert!(activity[0].remote, "an HTTP call comes over the network");
}

/// An upstream whose call runs until the client cancels it. rmcp cancels a
/// handler's `context.ct` when `notifications/cancelled` names its request.
struct HangingUpstream {
    entered: Arc<tokio::sync::Notify>,
}

impl rmcp::handler::server::ServerHandler for HangingUpstream {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(rmcp::model::ServerCapabilities::default())
    }

    fn call_tool(
        &self,
        _request: CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::CallToolResponse, McpError>> + Send + '_
    {
        let entered = Arc::clone(&self.entered);
        async move {
            entered.notify_one();
            context.ct.cancelled().await;
            Err(McpError::internal_error("call cancelled by client", None))
        }
    }
}

#[tokio::test]
async fn a_call_the_client_cancels_while_it_runs_is_recorded_as_cancelled() {
    use rmcp::ServiceExt as _;

    let router = router_with_unrouted_single_route();
    let entered = Arc::new(tokio::sync::Notify::new());
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let upstream = HangingUpstream {
        entered: Arc::clone(&entered),
    };
    tokio::spawn(async move {
        let server = upstream
            .serve(server_transport)
            .await
            .expect("start hanging upstream");
        let _ = server.waiting().await;
    });
    let tools = Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new()));
    let client = Arc::new(crate::server::UpstreamClientHandler::new_for_tests(
        Arc::from("mock-server"),
        Arc::clone(&tools),
        crate::server::no_events(),
    ))
    .serve(client_transport)
    .await
    .expect("connect hanging upstream");
    router
        .server_manager()
        .replace_server(
            "mock-server",
            crate::server::UpstreamServer::for_tests("mock-server".to_string(), client, tools),
        )
        .await;

    let ctx = local_caller();
    let call = {
        let router = Arc::clone(&router);
        let ctx = local_caller();
        tokio::spawn(async move {
            dispatch_call(&router, &ctx, CallToolRequestParams::new("Mock__tool")).await
        })
    };
    entered.notified().await;
    router.cancel_downstream_request(&ctx.downstream_call_context(), Some("stop".to_string()));

    tokio::time::timeout(std::time::Duration::from_secs(10), call)
        .await
        .expect("the cancelled call ends")
        .expect("call task")
        .expect_err("a cancelled call has no result");
    let activity = router.activity_snapshot(&crate::activity::ActivityFilter::default());
    assert_eq!(activity.len(), 1);
    assert_eq!(
        activity[0].outcome,
        crate::activity::ActivityOutcome::Cancelled
    );
}

/// A tool that answers with `isError: true` failed, though the call itself
/// went through.
#[tokio::test]
async fn a_tool_error_result_is_recorded_as_an_error() {
    let mut config = crate::config::Config::default();
    config.servers.insert(
        "mock".into(),
        crate::test_helpers::mock_server("tool_error"),
    );
    let engine = crate::engine::Engine::new(config);
    engine.start().await;
    let router = engine.tool_router();
    let tool = router
        .list_tools_for_client(ClientType::Unknown)
        .iter()
        .map(|tool| tool.name.to_string())
        .find(|name| name.ends_with("tool_error"))
        .expect("tool_error is listed");

    let result = dispatch_call(router, &local_caller(), CallToolRequestParams::new(tool))
        .await
        .expect("the call goes through");

    assert_eq!(result["isError"], true);
    let activity = router.activity_snapshot(&crate::activity::ActivityFilter::default());
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].outcome, crate::activity::ActivityOutcome::Error);
    engine.shutdown().await;
}

fn assert_plug_icons_sequence(icons: &[rmcp::model::Icon]) {
    let expected_sizes = ["16x16", "32x32", "64x64", "128x128", "256x256", "512x512"];
    assert_eq!(icons.len(), expected_sizes.len() + 1);

    for (icon, expected_size) in icons.iter().zip(expected_sizes) {
        assert!(icon.src.starts_with("data:image/png;base64,"));
        assert_eq!(icon.mime_type.as_deref(), Some("image/png"));
        assert_eq!(
            icon.sizes
                .as_ref()
                .and_then(|sizes| sizes.first())
                .map(String::as_str),
            Some(expected_size)
        );
    }

    let svg = icons.last().expect("svg fallback icon");
    assert!(svg.src.starts_with("data:image/svg+xml;base64,"));
    assert_eq!(svg.mime_type.as_deref(), Some("image/svg+xml"));
    assert_eq!(svg.sizes.as_deref(), Some(&["any".to_string()][..]));
}
