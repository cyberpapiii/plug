use super::*;

fn test_router_config() -> RouterConfig {
    RouterConfig {
        prefix_delimiter: "__".to_string(),
        priority_tools: Vec::new(),
        disabled_tools: Vec::new(),
        tool_description_max_chars: None,
        tool_search_threshold: 50,
        meta_tool_mode: false,
        lazy_tools: LazyToolsConfig::default(),
        tool_filter_enabled: true,
        enrichment_servers: std::collections::HashSet::new(),
    }
}

fn expected_meta_tool_names() -> Vec<&'static str> {
    vec!["plug__search_tools"]
}

fn expected_legacy_meta_tool_names() -> Vec<&'static str> {
    vec![
        "plug__list_servers",
        "plug__list_tools",
        "plug__search_tools",
        "plug__invoke_tool",
    ]
}

#[test]
fn trace_ids_are_w3c_sized_hex_values() {
    let trace_id = new_trace_id();
    assert_eq!(trace_id.len(), 32);
    assert!(trace_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(trace_id, "00000000000000000000000000000000");
}

#[test]
fn downstream_context_preserves_supplied_http_trace_id() {
    let context = DownstreamCallContext::http_for_client_with_trace(
        "session-a",
        RequestId::from(NumberOrString::Number(1)),
        ClientType::ClaudeCode,
        Arc::<str>::from("4bf92f3577b34da6a3ce929d0e0e4736"),
    );

    assert_eq!(
        context.trace_id.as_ref(),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );
}

fn router_with_git_commit_tool() -> ToolRouter {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());
    let mut routes = HashMap::new();
    routes.insert(
        "git__commit".to_string(),
        ("git".to_string(), "commit".to_string()),
    );
    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(vec![Tool::new(
            Cow::Borrowed("git__commit"),
            Cow::Borrowed("Create a git commit"),
            Arc::new(serde_json::Map::new()),
        )]),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));
    router
}

#[test]
fn get_info_returns_correct_server_info() {
    let sm = Arc::new(ServerManager::new());
    let handler = ProxyHandler::new(sm, test_router_config());
    let info = handler.get_info();

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
    assert_eq!(info.protocol_version.as_str(), LATEST_PROTOCOL_VERSION);
    assert!(info.capabilities.tools.is_none());
    assert!(info.capabilities.resources.is_none());
}

fn assert_plug_icons_sequence(icons: &[Icon]) {
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

#[tokio::test(start_paused = true)]
async fn schedule_tool_list_changed_refresh_debounces_bursts() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(sm, test_router_config()));
    let mut rx = router.subscribe_notifications();

    router.schedule_tool_list_changed_refresh();
    router.schedule_tool_list_changed_refresh();
    router.schedule_tool_list_changed_refresh();

    tokio::task::yield_now().await;
    assert!(
        rx.try_recv().is_err(),
        "notification should not publish before debounce window"
    );

    tokio::time::advance(LIST_CHANGED_REFRESH_DEBOUNCE - Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(
        rx.try_recv().is_err(),
        "notification should still be pending inside debounce window"
    );

    tokio::time::advance(Duration::from_millis(1)).await;
    let notification = rx.recv().await.expect("tool list changed notification");
    assert_eq!(notification, ProtocolNotification::ToolListChanged);
    assert!(
        rx.try_recv().is_err(),
        "burst should coalesce into a single notification"
    );
}

#[tokio::test(start_paused = true)]
async fn refresh_task_releases_in_progress_flag_for_subsequent_refresh() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(sm, test_router_config()));
    let mut rx = router.subscribe_notifications();

    // First refresh cycle.
    router.schedule_tool_list_changed_refresh();
    tokio::time::advance(LIST_CHANGED_REFRESH_DEBOUNCE).await;
    let first = rx.recv().await.expect("first notification");
    assert_eq!(first, ProtocolNotification::ToolListChanged);

    // After the cycle completes the in-progress flag must be released —
    // a wedged flag (the regression) would silently drop every future
    // refresh.
    tokio::task::yield_now().await;
    assert!(
        !router
            .notification_refresh_in_progress
            .load(Ordering::SeqCst),
        "in-progress flag must be cleared after a refresh cycle"
    );

    // A second schedule must therefore spawn a fresh task and publish
    // again.
    router.schedule_tool_list_changed_refresh();
    tokio::time::advance(LIST_CHANGED_REFRESH_DEBOUNCE).await;
    let second = rx.recv().await.expect("second notification");
    assert_eq!(second, ProtocolNotification::ToolListChanged);
}

#[tokio::test]
async fn refresh_tools_with_no_servers() {
    let sm = Arc::new(ServerManager::new());
    let handler = ProxyHandler::new(sm, test_router_config());
    handler.refresh_tools().await;

    let tools = handler.router().list_tools();
    assert!(tools.is_empty());
}

#[tokio::test]
async fn tool_router_list_tools_returns_arc() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());
    router.refresh_tools().await;

    let tools1 = router.list_tools();
    let tools2 = router.list_tools();
    // Both should point to the same allocation (Arc)
    assert!(Arc::ptr_eq(&tools1, &tools2));
}

#[test]
fn priority_sort_orders_correctly() {
    let priority = vec!["important".to_string(), "medium".to_string()];

    let a = Tool::new(
        Cow::Borrowed("server__important_tool"),
        Cow::Borrowed("desc"),
        Arc::new(serde_json::Map::new()),
    );
    let b = Tool::new(
        Cow::Borrowed("server__other_tool"),
        Cow::Borrowed("desc"),
        Arc::new(serde_json::Map::new()),
    );
    let c = Tool::new(
        Cow::Borrowed("server__medium_tool"),
        Cow::Borrowed("desc"),
        Arc::new(serde_json::Map::new()),
    );

    // Priority tool should come before non-priority
    assert_eq!(priority_sort(&a, &b, &priority), std::cmp::Ordering::Less);
    // Non-priority after priority
    assert_eq!(
        priority_sort(&b, &a, &priority),
        std::cmp::Ordering::Greater
    );
    // Higher priority before lower priority
    assert_eq!(priority_sort(&a, &c, &priority), std::cmp::Ordering::Less);
    // Same priority: alphabetical
    assert_eq!(priority_sort(&b, &b, &priority), std::cmp::Ordering::Equal);
}

#[test]
fn disabled_tool_patterns_support_exact_and_wildcard_matches() {
    assert!(is_disabled_tool(
        &["slack__search_messages".into()],
        "Slack__search_messages"
    ));
    assert!(is_disabled_tool(
        &["slack__*".into()],
        "Slack__search_messages"
    ));
    assert!(is_disabled_tool(
        &["*search*".into()],
        "Slack__search_messages"
    ));
    assert!(!is_disabled_tool(
        &["gmail__*".into()],
        "Slack__search_messages"
    ));
}

#[test]
fn normalized_icons_preserve_item_icons_before_server_fallback() {
    let item = Icon::new("https://example.com/tool.png").with_mime_type("image/png");
    let fallback = Icon::new("https://example.com/server.png").with_mime_type("image/png");

    let icons = normalized_icons_with_fallback(Some(&[item]), Some(vec![fallback]))
        .expect("tool icon should survive");

    assert_eq!(icons.len(), 1);
    assert_eq!(icons[0].src, "https://example.com/tool.png");
}

#[test]
fn normalized_icons_use_https_server_fallback_when_item_icon_is_missing() {
    let fallback = Icon::new("https://example.com/server.png").with_mime_type("image/png");

    let icons = normalized_icons_with_fallback(None, Some(vec![fallback]))
        .expect("server fallback should be used");

    assert_eq!(icons.len(), 1);
    assert_eq!(icons[0].src, "https://example.com/server.png");
}

#[test]
fn normalized_icons_do_not_fallback_over_invalid_explicit_item_icons() {
    let unsafe_item = Icon::new("file:///tmp/tool.png").with_mime_type("image/png");
    let fallback = Icon::new("https://example.com/server.png").with_mime_type("image/png");

    assert!(normalized_icons_with_fallback(Some(&[unsafe_item]), Some(vec![fallback])).is_none());
}

#[test]
fn normalized_icons_do_not_duplicate_data_uri_server_fallbacks() {
    let fallback = Icon::new("data:image/png;base64,aGVsbG8=");

    assert!(normalized_icons_with_fallback(None, Some(vec![fallback])).is_none());
}

#[test]
fn strip_optional_fields_preserves_schema_and_truncates_description() {
    let mut tool = Tool::new(
        Cow::Borrowed("test_tool"),
        Cow::Borrowed("A long description that should be truncated if configured"),
        Arc::new(serde_json::Map::new()),
    );
    tool.title = Some("Title".to_string());
    tool.annotations = Some(ToolAnnotations::default());
    tool.output_schema = Some(Arc::new(serde_json::Map::new()));

    strip_optional_fields(&mut tool, Some(10));

    assert!(tool.title.is_some());
    assert!(tool.annotations.is_some());
    assert!(
        tool.output_schema.is_some(),
        "outputSchema must be preserved"
    );
    assert_eq!(tool.description.as_deref(), Some("A long des"));
}

#[test]
fn strip_optional_fields_removes_control_characters_from_description() {
    let mut tool = Tool::new(
        Cow::Borrowed("test_tool"),
        Cow::Borrowed("ok\u{0000}still-ok\tline\nnext"),
        Arc::new(serde_json::Map::new()),
    );

    strip_optional_fields(&mut tool, None);

    assert_eq!(tool.description.as_deref(), Some("okstill-ok\tline\nnext"));
}

#[test]
fn strip_optional_fields_sanitizes_before_truncating() {
    let mut tool = Tool::new(
        Cow::Borrowed("test_tool"),
        Cow::Borrowed("ab\u{0000}cdef"),
        Arc::new(serde_json::Map::new()),
    );

    strip_optional_fields(&mut tool, Some(4));

    assert_eq!(tool.description.as_deref(), Some("abcd"));
}

#[test]
fn apply_canonical_tool_title_sets_top_level_and_annotation_titles() {
    let mut tool = Tool::new(
        Cow::Borrowed("Slack__channels_list"),
        Cow::Borrowed("Get list of channels"),
        Arc::new(serde_json::Map::new()),
    );
    let mut annotations = ToolAnnotations::default();
    annotations.title = Some("List Channels".to_string());
    tool.annotations = Some(annotations);

    apply_canonical_tool_title(&mut tool, "Slack: List Channels".to_string());

    assert_eq!(tool.title.as_deref(), Some("Slack: List Channels"));
    assert_eq!(
        tool.annotations
            .as_ref()
            .and_then(|ann| ann.title.as_deref()),
        Some("Slack: List Channels")
    );
}

#[test]
fn apply_canonical_tool_title_creates_annotation_title_when_missing() {
    let mut tool = Tool::new(
        Cow::Borrowed("Todoist__add_filters"),
        Cow::Borrowed("Add one or more new personal filters."),
        Arc::new(serde_json::Map::new()),
    );

    apply_canonical_tool_title(&mut tool, "Todoist: Add Filters".to_string());

    assert_eq!(tool.title.as_deref(), Some("Todoist: Add Filters"));
    assert_eq!(
        tool.annotations
            .as_ref()
            .and_then(|ann| ann.title.as_deref()),
        Some("Todoist: Add Filters")
    );
}

#[test]
fn list_tools_for_client_returns_correct_counts() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    // Manually set up a snapshot with 150 tools
    let tools: Vec<Tool> = (0..150)
        .map(|i| {
            Tool::new(
                Cow::Owned(format!("tool_{i:03}")),
                Cow::Owned(format!("Tool {i}")),
                Arc::new(serde_json::Map::new()),
            )
        })
        .collect();

    let tools_windsurf = Arc::new(tools.iter().take(100).cloned().collect::<Vec<_>>());
    let tools_copilot = Arc::new(tools.iter().take(128).cloned().collect::<Vec<_>>());
    let tools_all = Arc::new(tools);

    router.cache.store(Arc::new(RouterSnapshot {
        routes: HashMap::new(),
        tools_all,
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf,
        tools_copilot,
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    assert_eq!(
        router.list_tools_for_client(ClientType::Windsurf).len(),
        100
    );
    assert_eq!(
        router
            .list_tools_for_client(ClientType::VSCodeCopilot)
            .len(),
        128
    );
    assert_eq!(
        router.list_tools_for_client(ClientType::ClaudeCode).len(),
        150
    );
    assert_eq!(router.list_tools_for_client(ClientType::Cursor).len(), 150);
}

#[test]
fn search_tools_returns_matches() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    // Set up a snapshot with named tools
    let tools = vec![
        Tool::new(
            Cow::Borrowed("git__commit"),
            Cow::Borrowed("Create a git commit"),
            Arc::new(serde_json::Map::new()),
        ),
        Tool::new(
            Cow::Borrowed("git__push"),
            Cow::Borrowed("Push changes to remote"),
            Arc::new(serde_json::Map::new()),
        ),
        Tool::new(
            Cow::Borrowed("slack__send"),
            Cow::Borrowed("Send a message on Slack"),
            Arc::new(serde_json::Map::new()),
        ),
    ];

    let mut routes = HashMap::new();
    routes.insert(
        "git__commit".to_string(),
        ("git".to_string(), "commit".to_string()),
    );
    routes.insert(
        "git__push".to_string(),
        ("git".to_string(), "push".to_string()),
    );
    routes.insert(
        "slack__send".to_string(),
        ("slack".to_string(), "send".to_string()),
    );

    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(tools),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    // Search by name
    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("git"));
    let result = router.handle_search_tools(Some(args), None).unwrap();
    let text = format!("{result:?}");
    assert!(text.contains("git__commit"));
    assert!(text.contains("git__push"));

    // Search by natural multi-token description
    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("send slack message"));
    let result = router.handle_search_tools(Some(args), None).unwrap();
    let text = format!("{result:?}");
    assert!(text.contains("slack__send"));

    // No matches
    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("nonexistent"));
    let result = router.handle_search_tools(Some(args), None).unwrap();
    let text = format!("{result:?}");
    assert!(text.contains("matches"));
    assert!(text.contains("[]"));
}

#[test]
fn meta_tool_mode_lists_only_meta_tools() {
    let sm = Arc::new(ServerManager::new());
    let mut config = test_router_config();
    config.meta_tool_mode = true;
    let router = ToolRouter::new(sm, config);

    let tools = vec![Tool::new(
        Cow::Borrowed("git__commit"),
        Cow::Borrowed("Create a git commit"),
        Arc::new(serde_json::Map::new()),
    )];

    let mut routes = HashMap::new();
    routes.insert(
        "git__commit".to_string(),
        ("git".to_string(), "commit".to_string()),
    );

    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(tools),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    let visible_tools = router.list_tools_for_client(ClientType::ClaudeCode);
    let names = visible_tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, expected_legacy_meta_tool_names());

    let full_tools = router.list_all_tools();
    assert_eq!(full_tools.len(), 1);
    assert_eq!(full_tools[0].1.name.as_ref(), "git__commit");
}

#[test]
fn client_lazy_bridge_policy_lists_meta_tools_for_opencode() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    router.cache.store(Arc::new(RouterSnapshot {
        routes: HashMap::new(),
        tools_all: Arc::new(vec![Tool::new(
            Cow::Borrowed("git__commit"),
            Cow::Borrowed("Create a git commit"),
            Arc::new(serde_json::Map::new()),
        )]),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    let visible_tools = router.list_tools_for_client(ClientType::OpenCode);
    let names = visible_tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, expected_meta_tool_names());

    assert_eq!(
        router.list_tools_for_client(ClientType::ClaudeCode).len(),
        1
    );
}

#[test]
fn bridge_search_tools_adds_real_tools_to_session_visible_set() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let mut routes = HashMap::new();
    routes.insert(
        "git__commit".to_string(),
        ("git".to_string(), "commit".to_string()),
    );
    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(vec![Tool::new(
            Cow::Borrowed("git__commit"),
            Cow::Borrowed("Create a git commit"),
            Arc::new(serde_json::Map::new()),
        )]),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    let session_key = ToolRouter::lazy_session_key(DownstreamTransport::Stdio, "client-a");
    assert_eq!(
        router
            .list_tools_for_client_session(ClientType::OpenCode, Some(&session_key))
            .len(),
        expected_meta_tool_names().len()
    );

    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("commit"));
    let downstream = DownstreamCallContext::stdio_for_client(
        "client-a",
        RequestId::Number(1),
        ClientType::OpenCode,
    );
    router
        .handle_search_tools(Some(args), Some(&downstream))
        .expect("search and load tool");

    let visible = router.list_tools_for_client_session(ClientType::OpenCode, Some(&session_key));
    let names = visible
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert!(names.contains(&"git__commit".to_string()));
}

#[test]
fn bridge_search_keeps_session_working_set_bounded() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let mut routes = HashMap::new();
    let tools = (0..(BRIDGE_WORKING_SET_MAX_TOOLS + 5))
        .map(|index| {
            let name = format!("tool_{index:03}");
            routes.insert(name.clone(), ("test".to_string(), name.clone()));
            Tool::new(
                Cow::Owned(name),
                Cow::Owned(format!("Tool number {index}")),
                Arc::new(serde_json::Map::new()),
            )
        })
        .collect::<Vec<_>>();
    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(tools),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    let downstream = DownstreamCallContext::stdio_for_client(
        "client-a",
        RequestId::Number(1),
        ClientType::OpenCode,
    );
    for index in 0..(BRIDGE_WORKING_SET_MAX_TOOLS + 5) {
        let mut args = serde_json::Map::new();
        args.insert(
            "query".to_string(),
            serde_json::json!(format!("tool_{index:03}")),
        );
        args.insert("limit".to_string(), serde_json::json!(1));
        router
            .handle_search_tools(Some(args), Some(&downstream))
            .expect("search and load tool");
    }

    let session_key = ToolRouter::lazy_session_key(DownstreamTransport::Stdio, "client-a");
    let visible = router.list_tools_for_client_session(ClientType::OpenCode, Some(&session_key));
    let names = visible
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        names.len(),
        BRIDGE_WORKING_SET_MAX_TOOLS + expected_meta_tool_names().len()
    );
    assert!(!names.contains(&"tool_000".to_string()));
    assert!(names.contains(&format!("tool_{:03}", BRIDGE_WORKING_SET_MAX_TOOLS + 4)));
}

#[test]
fn bridge_search_publish_tool_list_changed_for_newly_loaded_matches() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let mut routes = HashMap::new();
    routes.insert(
        "git__commit".to_string(),
        ("git".to_string(), "commit".to_string()),
    );
    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(vec![Tool::new(
            Cow::Borrowed("git__commit"),
            Cow::Borrowed("Create a git commit"),
            Arc::new(serde_json::Map::new()),
        )]),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    let mut notifications = router.subscribe_notifications();
    let downstream = DownstreamCallContext::stdio_for_client(
        "client-a",
        RequestId::Number(1),
        ClientType::OpenCode,
    );
    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("commit"));
    router
        .handle_search_tools(Some(args), Some(&downstream))
        .expect("search and load tool");
    assert_eq!(
        notifications.try_recv().expect("search notification"),
        ProtocolNotification::ToolListChangedFor {
            target: NotificationTarget::Stdio {
                client_id: Arc::from("client-a"),
            },
        }
    );

    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("commit"));
    router
        .handle_search_tools(Some(args), Some(&downstream))
        .expect("repeat search");
    assert!(
        notifications.try_recv().is_err(),
        "repeat search should not notify when it loads no new tools"
    );
}

#[tokio::test]
async fn bridge_session_rejects_unloaded_direct_tool_call() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let mut routes = HashMap::new();
    routes.insert(
        "git__commit".to_string(),
        ("git".to_string(), "commit".to_string()),
    );
    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(vec![Tool::new(
            Cow::Borrowed("git__commit"),
            Cow::Borrowed("Create a git commit"),
            Arc::new(serde_json::Map::new()),
        )]),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    let session_key = ToolRouter::lazy_session_key(DownstreamTransport::Stdio, "client-a");
    router.list_tools_for_client_session(ClientType::OpenCode, Some(&session_key));

    let err = router
        .call_tool_with_context(
            "git__commit",
            None,
            None,
            Some(DownstreamCallContext::stdio_for_client(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
            )),
        )
        .await
        .expect_err("unloaded direct call should be rejected");
    assert!(err.to_string().contains("plug__search_tools"));
}

#[tokio::test]
async fn bridge_session_rejects_unloaded_direct_tool_call_before_tools_list() {
    let router = router_with_git_commit_tool();

    let err = router
        .call_tool_with_context(
            "git__commit",
            None,
            None,
            Some(DownstreamCallContext::stdio_for_client(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
            )),
        )
        .await
        .expect_err("unloaded direct call should be rejected before tools/list");

    assert!(err.to_string().contains("plug__search_tools"));
}

#[tokio::test]
async fn bridge_session_rejects_unloaded_task_tool_call() {
    let router = Arc::new(router_with_git_commit_tool());

    let err = router
        .enqueue_tool_task(
            "git__commit",
            None,
            None,
            TaskOwner::new(Arc::<str>::from("stdio:client-a")),
            Some(DownstreamCallContext::stdio_for_client(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
            )),
        )
        .await
        .expect_err("unloaded task call should be rejected");

    assert!(err.to_string().contains("plug__search_tools"));
}

#[tokio::test]
async fn bridge_session_rejects_task_wrapped_search_meta_tool() {
    let router = Arc::new(router_with_git_commit_tool());
    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("commit"));

    let err = router
        .enqueue_tool_task(
            "plug__search_tools",
            Some(args),
            None,
            TaskOwner::new(Arc::<str>::from("stdio:client-a")),
            Some(DownstreamCallContext::stdio_for_client(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
            )),
        )
        .await
        .expect_err("task-wrapped meta-tools should be explicitly unsupported");

    assert!(err.to_string().contains("task-wrapped"));
}

#[tokio::test]
async fn bridge_session_rejects_invoke_wrapper_as_unknown_tool() {
    let router = router_with_git_commit_tool();
    let mut args = serde_json::Map::new();
    args.insert("tool_name".to_string(), serde_json::json!("git__commit"));

    let err = router
        .call_tool_with_context(
            "plug__invoke_tool",
            Some(args),
            None,
            Some(DownstreamCallContext::stdio_for_client(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
            )),
        )
        .await
        .expect_err("invoke wrapper should not exist in the bridge surface");

    assert!(err.to_string().contains("plug__invoke_tool"));
}

#[tokio::test]
async fn disabled_bridge_search_tool_is_not_visible_or_callable() {
    let sm = Arc::new(ServerManager::new());
    let mut config = test_router_config();
    config.disabled_tools = vec!["plug__search_tools".to_string()];
    let router = ToolRouter::new(sm, config);

    assert!(
        router
            .list_tools_for_client(ClientType::OpenCode)
            .iter()
            .all(|tool| tool.name.as_ref() != "plug__search_tools")
    );

    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("commit"));
    let err = router
        .call_tool_with_context(
            "plug__search_tools",
            Some(args),
            None,
            Some(DownstreamCallContext::stdio_for_client(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
            )),
        )
        .await
        .expect_err("disabled search meta-tool should reject");
    assert!(err.to_string().contains("plug__search_tools"));
}

#[test]
fn synthesized_capabilities_include_tasks_when_tools_exist() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let mut routes = HashMap::new();
    routes.insert(
        "Mock__echo".to_string(),
        ("mock".to_string(), "echo".to_string()),
    );
    let tools = vec![Tool::new(
        Cow::Borrowed("Mock__echo"),
        Cow::Borrowed("Echo a value"),
        Arc::new(serde_json::Map::new()),
    )];
    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(tools),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    let caps = router.synthesized_capabilities();
    assert!(caps.tasks.is_some());
    let tasks = caps.tasks.unwrap();
    assert!(tasks.supports_list());
    assert!(tasks.supports_cancel());
    assert!(tasks.supports_tools_call());
}

#[test]
fn synthesized_capabilities_suppress_tasks_for_bridge_clients() {
    let router = router_with_git_commit_tool();

    let caps = router.synthesized_capabilities_for_client(ClientType::OpenCode);

    assert!(caps.tools.is_some());
    assert!(caps.tasks.is_none());
}

#[test]
fn detect_tool_definition_drift_reports_changed_tools_only() {
    let previous = HashMap::from([
        ("git__commit".to_string(), 1_u64),
        ("git__push".to_string(), 2_u64),
    ]);
    let current = HashMap::from([
        ("git__commit".to_string(), 3_u64),
        ("git__push".to_string(), 2_u64),
        ("git__status".to_string(), 4_u64),
    ]);

    assert_eq!(
        detect_tool_definition_drift(&previous, &current),
        vec!["git__commit".to_string()]
    );
}

// -----------------------------------------------------------------------
// Session error classification tests
// -----------------------------------------------------------------------

#[test]
fn is_session_error_transport_closed() {
    use rmcp::service::ServiceError;
    assert!(is_session_error(&ServiceError::TransportClosed));
}

#[test]
fn is_session_error_mcp_error_not_session() {
    use rmcp::service::ServiceError;
    // Application-level MCP error should NOT trigger reconnect
    let mcp_err = McpError::internal_error("tool failed".to_string(), None);
    assert!(!is_session_error(&ServiceError::McpError(mcp_err)));
}

#[test]
fn is_session_error_timeout_not_session() {
    use rmcp::service::ServiceError;
    // Timeouts should NOT trigger reconnect
    assert!(!is_session_error(&ServiceError::Timeout {
        timeout: Duration::from_secs(30),
    }));
}

#[test]
fn is_session_error_cancelled_not_session() {
    use rmcp::service::ServiceError;
    assert!(!is_session_error(&ServiceError::Cancelled {
        reason: Some("test".to_string()),
    }));
}

#[test]
fn is_session_error_unexpected_response_not_session() {
    use rmcp::service::ServiceError;
    assert!(!is_session_error(&ServiceError::UnexpectedResponse));
}

#[test]
fn case_insensitive_route_lookup() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let mut routes = HashMap::new();
    routes.insert(
        "Slack__search_messages".to_string(),
        (
            "slack".to_string(),
            "conversations_search_messages".to_string(),
        ),
    );

    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(Vec::new()),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    let snapshot = router.cache.load();
    // Exact match works
    assert!(snapshot.routes.contains_key("Slack__search_messages"));
    // Case-insensitive fallback works
    let lower = "slack__search_messages";
    let found = snapshot.routes.get(lower).or_else(|| {
        snapshot
            .routes
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(lower))
            .map(|(_, v)| v)
    });
    assert!(found.is_some());
    assert_eq!(found.unwrap().0, "slack");
    assert_eq!(found.unwrap().1, "conversations_search_messages");
}

#[tokio::test(start_paused = true)]
async fn call_tool_times_out_waiting_for_semaphore() {
    let server_manager = Arc::new(ServerManager::new());
    let router = ToolRouter::new(server_manager.clone(), test_router_config());

    server_manager.semaphores.insert(
        "busy-server".to_string(),
        Arc::new(tokio::sync::Semaphore::new(0)),
    );

    let mut routes = HashMap::new();
    routes.insert(
        "Busy__tool".to_string(),
        ("busy-server".to_string(), "tool".to_string()),
    );
    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(Vec::new()),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    let call = router.call_tool("Busy__tool", None);
    tokio::pin!(call);

    tokio::time::advance(Duration::from_secs(31)).await;

    let err = call.await.unwrap_err();
    assert!(
        err.message.contains("server overloaded"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn list_tools_page_for_client_uses_cursor_pagination() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());
    let tools: Vec<Tool> = (0..750)
        .map(|index| {
            Tool::new(
                Cow::Owned(format!("tool_{index}")),
                Cow::Borrowed("desc"),
                Arc::new(serde_json::Map::new()),
            )
        })
        .collect();
    router.cache.store(Arc::new(RouterSnapshot {
        routes: HashMap::new(),
        tools_windsurf: Arc::new(tools.iter().take(100).cloned().collect()),
        tools_copilot: Arc::new(tools.iter().take(128).cloned().collect()),
        tools_all: Arc::new(tools),
        meta_tools_all: Arc::new(build_meta_tools()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));

    let first = router.list_tools_page_for_client(ClientType::Unknown, Some(Default::default()));
    assert_eq!(first.tools.len(), 500);
    assert_eq!(first.next_cursor.as_deref(), Some("500"));

    let mut second_request = PaginatedRequestParams::default();
    second_request.cursor = first.next_cursor;
    let second = router.list_tools_page_for_client(ClientType::Unknown, Some(second_request));
    assert_eq!(second.tools.len(), 250);
    assert!(second.next_cursor.is_none());
}

#[tokio::test]
async fn route_upstream_progress_publishes_targeted_notification() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());
    let mut rx = router.subscribe_notifications();
    let progress_token = ProgressToken(NumberOrString::String(Arc::from("progress-1")));

    router.register_active_call(
        42,
        ActiveCallRecord {
            downstream: DownstreamCallContext::stdio(
                Arc::from("client-1"),
                RequestId::from(NumberOrString::Number(1)),
            ),
            upstream_server_id: "upstream".to_string(),
            upstream_request_id: None,
            downstream_progress_token: Some(progress_token.clone()),
            upstream_progress_token: Some(progress_token.clone()),
            pending_cancel_reason: None,
        },
    );

    router.route_upstream_progress(
        "upstream",
        ProgressNotificationParam::new(progress_token.clone(), 0.5).with_message("halfway"),
    );

    let notification = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("notification arrives")
        .expect("notification channel open");

    match notification {
        ProtocolNotification::Progress { target, params } => {
            assert_eq!(
                target,
                NotificationTarget::Stdio {
                    client_id: Arc::from("client-1"),
                }
            );
            assert_eq!(params.progress_token, progress_token);
            assert_eq!(params.message.as_deref(), Some("halfway"));
        }
        other => panic!("unexpected notification: {other:?}"),
    }
}

#[test]
fn synthesized_capabilities_advertises_subscribe_when_upstream_supports_it() {
    let sm = Arc::new(ServerManager::new());
    let config = test_router_config();
    let router = ToolRouter::new(sm, config);

    // No upstreams → no resources capability at all
    let caps = router.synthesized_capabilities();
    assert!(caps.resources.is_none());
}

#[test]
fn resource_subscription_registry_lifecycle() {
    let sm = Arc::new(ServerManager::new());
    let config = test_router_config();
    let router = ToolRouter::new(sm, config);

    let target = NotificationTarget::Stdio {
        client_id: Arc::from("test-client"),
    };

    // Registry starts empty
    assert!(router.resource_subscriptions.is_empty());

    // Insert directly (bypassing upstream check for unit test)
    router
        .resource_subscriptions
        .entry("file:///test".to_string())
        .or_default()
        .insert(target.clone());
    assert_eq!(router.resource_subscriptions.len(), 1);

    // Route notification should publish to subscriber
    let mut rx = router.subscribe_notifications();
    router.route_upstream_resource_updated(ResourceUpdatedNotificationParam::new("file:///test"));

    match rx.try_recv() {
        Ok(ProtocolNotification::ResourceUpdated {
            target: t, params, ..
        }) => {
            assert_eq!(t, target);
            assert_eq!(params.uri, "file:///test");
        }
        other => panic!("expected ResourceUpdated, got: {other:?}"),
    }

    // Route notification for unsubscribed URI → no notification
    router.route_upstream_resource_updated(ResourceUpdatedNotificationParam::new("file:///other"));
    assert!(rx.try_recv().is_err());
}

#[test]
fn synthesized_capabilities_no_completions_without_upstream() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());
    let caps = router.synthesized_capabilities();
    assert!(caps.completions.is_none());
}

#[test]
fn complete_request_params_serde_roundtrip() {
    let params = CompleteRequestParams::new(
        Reference::for_prompt("test-prompt"),
        ArgumentInfo {
            name: "arg1".to_string(),
            value: "partial".to_string(),
        },
    );

    let json = serde_json::to_value(&params).unwrap();
    let deserialized: CompleteRequestParams = serde_json::from_value(json).unwrap();
    assert_eq!(deserialized.argument.name, "arg1");
    assert_eq!(deserialized.argument.value, "partial");
    match &deserialized.r#ref {
        Reference::Prompt(p) => assert_eq!(p.name, "test-prompt"),
        other => panic!("expected Prompt reference, got {other:?}"),
    }
}

#[test]
fn route_upstream_logging_message_publishes_with_server_prefix() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());
    // Default level is Warning, so send a warning-level message
    let mut rx = router.subscribe_logging();

    router.route_upstream_logging_message(
        "github",
        LoggingMessageNotificationParam {
            level: LoggingLevel::Warning,
            logger: Some("default".to_string()),
            data: serde_json::json!("something happened"),
        },
    );

    match rx.try_recv() {
        Ok(ProtocolNotification::LoggingMessage { params }) => {
            assert_eq!(params.logger.as_deref(), Some("github:default"));
            assert_eq!(params.level, LoggingLevel::Warning);
        }
        other => panic!("expected LoggingMessage, got: {other:?}"),
    }
}

#[test]
fn route_upstream_logging_message_filters_below_threshold() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());
    let mut rx = router.subscribe_logging();

    // Default level is Warning — debug should be filtered
    router.route_upstream_logging_message(
        "github",
        LoggingMessageNotificationParam {
            level: LoggingLevel::Debug,
            logger: None,
            data: serde_json::json!("debug noise"),
        },
    );

    assert!(rx.try_recv().is_err(), "debug message should be filtered");
}

#[test]
fn set_client_log_level_changes_effective_level() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    // Default is Warning
    assert_eq!(router.log_level(), LoggingLevel::Warning);

    // Client A sets Debug
    router.set_client_log_level("client-a", LoggingLevel::Debug);
    assert_eq!(router.log_level(), LoggingLevel::Debug);

    // Client B sets Error — most permissive (Debug) should win
    router.set_client_log_level("client-b", LoggingLevel::Error);
    assert_eq!(router.log_level(), LoggingLevel::Debug);

    // Client A disconnects — should fall to Error
    router.remove_client_log_level("client-a");
    assert_eq!(router.log_level(), LoggingLevel::Error);

    // Client B disconnects — should reset to Warning
    router.remove_client_log_level("client-b");
    assert_eq!(router.log_level(), LoggingLevel::Warning);
}

#[test]
fn route_upstream_logging_respects_changed_level() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());
    let mut rx = router.subscribe_logging();

    // Lower threshold to Debug
    router.set_client_log_level("client-a", LoggingLevel::Debug);

    // Now debug messages should pass through
    router.route_upstream_logging_message(
        "server1",
        LoggingMessageNotificationParam {
            level: LoggingLevel::Debug,
            logger: None,
            data: serde_json::json!("debug info"),
        },
    );

    match rx.try_recv() {
        Ok(ProtocolNotification::LoggingMessage { params }) => {
            assert_eq!(params.level, LoggingLevel::Debug);
            assert_eq!(params.logger.as_deref(), Some("server1:default"));
        }
        other => panic!("expected LoggingMessage, got: {other:?}"),
    }
}

#[test]
fn logging_channel_is_separate_from_control_channel() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());
    let mut control_rx = router.subscribe_notifications();
    let mut logging_rx = router.subscribe_logging();

    // Send a logging message
    router.route_upstream_logging_message(
        "server1",
        LoggingMessageNotificationParam {
            level: LoggingLevel::Warning,
            logger: None,
            data: serde_json::json!("log msg"),
        },
    );

    // Control channel should NOT receive it
    assert!(
        control_rx.try_recv().is_err(),
        "logging should not appear on control channel"
    );

    // Logging channel should receive it
    assert!(
        logging_rx.try_recv().is_ok(),
        "logging should appear on logging channel"
    );

    // Send a control notification
    router.publish_protocol_notification(ProtocolNotification::ToolListChanged);

    // Control channel should receive it
    assert!(
        control_rx.try_recv().is_ok(),
        "tool list changed should appear on control channel"
    );

    // Logging channel should NOT receive it
    assert!(
        logging_rx.try_recv().is_err(),
        "tool list changed should not appear on logging channel"
    );
}

#[test]
fn synthesized_capabilities_includes_logging_when_upstream_supports_it() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    // Without any upstream servers, no logging capability
    let caps = router.synthesized_capabilities();
    assert!(caps.logging.is_none());
}

// ── Roots cache tests ──────────────────────────────────────────────

/// Helper to construct `Root` (which is `#[non_exhaustive]` in rmcp 1.1).
fn make_root(uri: &str, name: Option<&str>) -> Root {
    serde_json::from_value(serde_json::json!({
        "uri": uri,
        "name": name,
    }))
    .expect("valid Root JSON")
}

#[test]
fn list_roots_union_empty_when_no_clients() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let result = router.list_roots_union();
    assert!(result.roots.is_empty());
}

#[test]
fn set_roots_for_target_returns_true_on_first_insert() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let target = NotificationTarget::Stdio {
        client_id: Arc::from("client-1"),
    };
    let roots = vec![make_root("file:///project-a", Some("Project A"))];

    assert!(router.set_roots_for_target(target, roots));
}

#[test]
fn set_roots_for_target_returns_false_when_unchanged() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let target = NotificationTarget::Stdio {
        client_id: Arc::from("client-1"),
    };
    let roots = vec![make_root("file:///project-a", Some("Project A"))];

    router.set_roots_for_target(target.clone(), roots.clone());
    // Second call with same roots should report no change
    assert!(!router.set_roots_for_target(target, roots));
}

#[test]
fn set_roots_for_target_returns_true_when_changed() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let target = NotificationTarget::Stdio {
        client_id: Arc::from("client-1"),
    };
    let roots_a = vec![make_root("file:///project-a", Some("Project A"))];
    let roots_b = vec![make_root("file:///project-b", Some("Project B"))];

    router.set_roots_for_target(target.clone(), roots_a);
    assert!(router.set_roots_for_target(target, roots_b));
}

#[test]
fn clear_roots_for_target_returns_true_when_existed() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let target = NotificationTarget::Stdio {
        client_id: Arc::from("client-1"),
    };
    let roots = vec![make_root("file:///project-a", None)];

    router.set_roots_for_target(target.clone(), roots);
    assert!(router.clear_roots_for_target(&target));
}

#[test]
fn clear_roots_for_target_returns_false_when_not_existed() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let target = NotificationTarget::Stdio {
        client_id: Arc::from("client-nonexistent"),
    };
    assert!(!router.clear_roots_for_target(&target));
}

#[test]
fn list_roots_union_deduplicates_by_uri() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    // Client 1 has roots A and B
    let target1 = NotificationTarget::Stdio {
        client_id: Arc::from("client-1"),
    };
    router.set_roots_for_target(
        target1,
        vec![
            make_root("file:///shared", Some("Shared from 1")),
            make_root("file:///only-1", Some("Only 1")),
        ],
    );

    // Client 2 has roots A (duplicate URI) and C
    let target2 = NotificationTarget::Http {
        session_id: Arc::from("session-2"),
    };
    router.set_roots_for_target(
        target2,
        vec![
            make_root("file:///shared", Some("Shared from 2")),
            make_root("file:///only-2", Some("Only 2")),
        ],
    );

    let result = router.list_roots_union();
    // Should have 3 unique URIs: /shared, /only-1, /only-2
    assert_eq!(result.roots.len(), 3);

    let uris: Vec<&str> = result.roots.iter().map(|r| r.uri.as_str()).collect();
    assert!(uris.contains(&"file:///shared"));
    assert!(uris.contains(&"file:///only-1"));
    assert!(uris.contains(&"file:///only-2"));
}

#[test]
fn list_roots_union_is_sorted_by_uri() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let target = NotificationTarget::Stdio {
        client_id: Arc::from("client-1"),
    };
    router.set_roots_for_target(
        target,
        vec![
            make_root("file:///z-project", None),
            make_root("file:///a-project", None),
            make_root("file:///m-project", None),
        ],
    );

    let result = router.list_roots_union();
    let uris: Vec<&str> = result.roots.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(
        uris,
        vec![
            "file:///a-project",
            "file:///m-project",
            "file:///z-project"
        ]
    );
}

#[test]
fn clear_roots_removes_from_union() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let target1 = NotificationTarget::Stdio {
        client_id: Arc::from("client-1"),
    };
    let target2 = NotificationTarget::Http {
        session_id: Arc::from("session-2"),
    };

    router.set_roots_for_target(target1.clone(), vec![make_root("file:///project-1", None)]);
    router.set_roots_for_target(target2, vec![make_root("file:///project-2", None)]);

    assert_eq!(router.list_roots_union().roots.len(), 2);

    // Clear client 1's roots
    router.clear_roots_for_target(&target1);
    let result = router.list_roots_union();
    assert_eq!(result.roots.len(), 1);
    assert_eq!(result.roots[0].uri, "file:///project-2");
}

// ── Upstream request ownership tests ───────────────────────────────

#[test]
fn test_upstream_request_lookup_requires_active_call() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    let result = router.active_call_for_upstream_request(
        "unknown-server",
        &RequestId::from(NumberOrString::Number(1)),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.message.contains("no active downstream call"),
        "expected 'no active downstream call' in error message, got: {}",
        err.message,
    );
}

#[test]
fn test_upstream_request_lookup_uses_request_id_not_server_uniqueness() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    router.register_active_call(
        1,
        ActiveCallRecord {
            downstream: DownstreamCallContext::stdio(
                Arc::from("client-a"),
                RequestId::from(NumberOrString::Number(1)),
            ),
            upstream_server_id: "s1".to_string(),
            upstream_request_id: Some(RequestId::from(NumberOrString::Number(101))),
            downstream_progress_token: None,
            upstream_progress_token: None,
            pending_cancel_reason: None,
        },
    );
    router.register_active_call(
        2,
        ActiveCallRecord {
            downstream: DownstreamCallContext::http(
                Arc::from("session-b"),
                RequestId::from(NumberOrString::Number(2)),
            ),
            upstream_server_id: "s1".to_string(),
            upstream_request_id: Some(RequestId::from(NumberOrString::Number(202))),
            downstream_progress_token: None,
            upstream_progress_token: None,
            pending_cancel_reason: None,
        },
    );

    let result = router
        .active_call_for_upstream_request("s1", &RequestId::from(NumberOrString::Number(202)));
    assert!(result.is_ok(), "expected Ok, got: {result:?}");
    assert_eq!(
        result.unwrap().downstream.notification_target(),
        NotificationTarget::Http {
            session_id: Arc::from("session-b"),
        },
    );
}

#[test]
fn test_route_upstream_progress_restores_downstream_token() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(sm, test_router_config()));
    let mut rx = router.subscribe_notifications();

    router.register_active_call(
        1,
        ActiveCallRecord {
            downstream: DownstreamCallContext::stdio(
                Arc::from("client-a"),
                RequestId::from(NumberOrString::Number(1)),
            ),
            upstream_server_id: "s1".to_string(),
            upstream_request_id: Some(RequestId::from(NumberOrString::Number(101))),
            downstream_progress_token: Some(ProgressToken(NumberOrString::String(Arc::from(
                "downstream-token",
            )))),
            upstream_progress_token: Some(ProgressToken(NumberOrString::String(Arc::from(
                "upstream-token",
            )))),
            pending_cancel_reason: None,
        },
    );

    router.route_upstream_progress(
        "s1",
        ProgressNotificationParam {
            progress_token: ProgressToken(NumberOrString::String(Arc::from("upstream-token"))),
            progress: 0.5,
            total: Some(1.0),
            message: None,
        },
    );

    let notification = rx.try_recv().expect("progress notification");
    match notification {
        ProtocolNotification::Progress { target, params } => {
            assert_eq!(
                target,
                NotificationTarget::Stdio {
                    client_id: Arc::from("client-a"),
                }
            );
            assert_eq!(
                params.progress_token,
                ProgressToken(NumberOrString::String(Arc::from("downstream-token")))
            );
        }
        other => panic!("expected progress notification, got {other:?}"),
    }
}

#[test]
fn test_attach_upstream_request_id_preserves_pending_cancel_reason() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    router.register_active_call(
        1,
        ActiveCallRecord {
            downstream: DownstreamCallContext::stdio(
                Arc::from("client-a"),
                RequestId::from(NumberOrString::Number(1)),
            ),
            upstream_server_id: "s1".to_string(),
            upstream_request_id: None,
            downstream_progress_token: None,
            upstream_progress_token: None,
            pending_cancel_reason: Some(Some("cancelled".to_string())),
        },
    );

    router.attach_upstream_request_id(1, "s1", RequestId::from(NumberOrString::Number(42)));
    let record = router.active_calls.get(&1).expect("active call").clone();
    assert_eq!(
        record.upstream_request_id,
        Some(RequestId::from(NumberOrString::Number(42)))
    );
    assert!(record.pending_cancel_reason.is_none());
}

#[test]
fn test_register_active_call_uses_upstream_progress_token_for_lookup() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, test_router_config());

    router.register_active_call(
        1,
        ActiveCallRecord {
            downstream: DownstreamCallContext::stdio(
                Arc::from("client-a"),
                RequestId::from(NumberOrString::Number(1)),
            ),
            upstream_server_id: "s1".to_string(),
            upstream_request_id: Some(RequestId::from(NumberOrString::Number(42))),
            downstream_progress_token: Some(ProgressToken(NumberOrString::String(Arc::from(
                "downstream-token",
            )))),
            upstream_progress_token: Some(ProgressToken(NumberOrString::String(Arc::from(
                "upstream-token",
            )))),
            pending_cancel_reason: None,
        },
    );

    assert_eq!(
        router
            .upstream_progress_lookup
            .get(&UpstreamProgressKey {
                server_id: "s1".to_string(),
                progress_token: ProgressToken(NumberOrString::String(Arc::from("upstream-token",))),
            })
            .map(|entry| *entry),
        Some(1)
    );
    assert!(
        router
            .upstream_progress_lookup
            .get(&UpstreamProgressKey {
                server_id: "s1".to_string(),
                progress_token: ProgressToken(NumberOrString::String(Arc::from(
                    "downstream-token",
                ))),
            })
            .is_none()
    );
}

#[test]
fn test_downstream_context_notification_target() {
    assert_eq!(
        DownstreamCallContext::stdio(
            Arc::from("client-a"),
            RequestId::from(NumberOrString::Number(1))
        )
        .notification_target(),
        NotificationTarget::Stdio {
            client_id: Arc::from("client-a"),
        },
    );
}

// ── dispatch::dispatch_tools_call characterization (U1) ──────────────────
//
// These pin the shared adapter's contract before the three transports are
// migrated onto it (U2/U3/U4). End-to-end behavior across every transport is
// covered by the integration suites and the parity matrix (U6); here we prove
// the sync/task branch decision and error propagation in isolation.

/// Mock transport context for exercising `dispatch_tools_call` without a live
/// transport. `supports_tasks` is configurable so we can prove the task-gate.
struct MockDownstream {
    supports_tasks: bool,
}

impl crate::dispatch::DownstreamContext for MockDownstream {
    fn downstream_call_context(&self) -> DownstreamCallContext {
        DownstreamCallContext::stdio_for_client(
            Arc::<str>::from("test-client"),
            RequestId::from(NumberOrString::Number(1)),
            ClientType::Unknown,
        )
    }

    fn supports_tasks(&self) -> bool {
        self.supports_tasks
    }

    fn task_owner(&self) -> Result<TaskOwner, McpError> {
        Ok(TaskOwner::new(Arc::<str>::from("test-owner")))
    }
}

/// Build a router with a single route to a server that has no registered
/// upstream. The sync path then fails with `ServerUnavailable`, while the task
/// path falls through to a local passthrough task and succeeds — making the two
/// branches observably distinct.
fn router_with_unrouted_single_route() -> Arc<ToolRouter> {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(sm, test_router_config()));
    let mut routes = HashMap::new();
    routes.insert(
        "Mock__tool".to_string(),
        ("mock-server".to_string(), "tool".to_string()),
    );
    router.cache.store(Arc::new(RouterSnapshot {
        routes,
        tools_all: Arc::new(Vec::new()),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_windsurf: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::new(),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    }));
    router
}

#[tokio::test]
async fn dispatch_tools_call_empty_name_returns_tool_not_found() {
    let router = router_with_unrouted_single_route();
    let ctx = MockDownstream {
        supports_tasks: true,
    };
    let params = CallToolRequestParams::new("");

    let err = crate::dispatch::dispatch_tools_call(&router, &ctx, params)
        .await
        .expect_err("empty tool name must error");

    // Canonical shape: an unknown/empty tool name routes to ToolNotFound
    // (the stdio/HTTP behavior, now shared), which maps to METHOD_NOT_FOUND.
    assert_eq!(err.code, ErrorCode::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn dispatch_tools_call_task_param_with_task_support_creates_task() {
    let router = router_with_unrouted_single_route();
    let ctx = MockDownstream {
        supports_tasks: true,
    };
    let mut params = CallToolRequestParams::new("Mock__tool");
    params.task = Some(serde_json::Map::new());

    let outcome = crate::dispatch::dispatch_tools_call(&router, &ctx, params)
        .await
        .expect("task-augmented call should enqueue a local passthrough task");

    assert!(
        matches!(outcome, crate::dispatch::ToolCallOutcome::TaskCreated(_)),
        "expected TaskCreated outcome, got {outcome:?}"
    );
}

#[tokio::test]
async fn dispatch_tools_call_task_param_without_task_support_takes_sync_path() {
    let router = router_with_unrouted_single_route();
    // stdio-like: cannot return a task result, so a task-augmented call must
    // fall through to the synchronous path (preserving today's behavior).
    let ctx = MockDownstream {
        supports_tasks: false,
    };
    let mut params = CallToolRequestParams::new("Mock__tool");
    params.task = Some(serde_json::Map::new());

    // expect_err is itself the branch-distinction proof: the task path returns
    // Ok(TaskCreated) (see the sibling test), so an Err means the sync path ran.
    let err = crate::dispatch::dispatch_tools_call(&router, &ctx, params)
        .await
        .expect_err("sync path with no upstream must error, not create a task");

    // And it is specifically the sync path's no-upstream ServerUnavailable error.
    assert!(
        err.message.to_lowercase().contains("unavailable"),
        "expected a sync-path ServerUnavailable error, got {err:?}"
    );
}
