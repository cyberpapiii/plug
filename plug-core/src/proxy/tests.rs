use super::*;
use rmcp::handler::server::ServerHandler;
use rmcp::service::{RequestContext, RoleServer};

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
fn http_call_contexts_are_untrusted_until_an_adapter_opts_in() {
    // HTTP is the internet-reachable transport. A context that reaches policy
    // without an adapter having declared how the caller authenticated must be
    // able to do nothing, not everything.
    for context in [
        DownstreamCallContext::http(
            "bare-session",
            RequestId::Number(1),
            crate::types::ClientType::Unknown,
        ),
        DownstreamCallContext::http("bare-session", RequestId::Number(2), ClientType::Unknown),
        DownstreamCallContext::http("bare-session", RequestId::Number(3), ClientType::Unknown)
            .with_trace_id("0123456789abcdef0123456789abcdef"),
    ] {
        assert!(
            !context.local_trust,
            "HTTP contexts must not inherit local trust from the constructor"
        );
        assert!(
            context
                .authorize(crate::protocol::MethodFamily::ToolsList)
                .is_err(),
            "an unopted HTTP context must be denied, not allowed"
        );
    }

    // The two explicit opt-ins still work, so no legitimate caller regresses.
    let bearer = DownstreamCallContext::http(
        "oauth-session",
        RequestId::Number(4),
        crate::types::ClientType::Unknown,
    )
    .with_authorization(
        crate::types::PrincipalId::configured_credential("test-bearer", 0),
        ["tools:read".to_string()],
    );
    assert!(!bearer.local_trust);
    assert!(
        bearer
            .authorize(crate::protocol::MethodFamily::ToolsList)
            .is_ok()
    );
    assert!(
        bearer
            .authorize(crate::protocol::MethodFamily::PromptsGet)
            .is_err(),
        "an authorized context is still bounded by its scopes"
    );

    let loopback = DownstreamCallContext::http(
        "loopback-session",
        RequestId::Number(5),
        crate::types::ClientType::Unknown,
    )
    .with_local_principal(crate::types::PrincipalId::configured_credential(
        "downstream-http-loopback",
        0,
    ));
    assert!(loopback.local_trust);
    assert!(
        loopback
            .authorize(crate::protocol::MethodFamily::PromptsGet)
            .is_ok(),
        "a loopback listener that requires no auth keeps full local access"
    );

    // IPC is local by construction and must stay trusted.
    let context = DownstreamCallContext::ipc(
        "ipc-client",
        RequestId::Number(7),
        ClientType::Unknown,
        None,
    );
    assert!(context.local_trust, "local transports keep local trust");
    assert!(
        context
            .authorize(crate::protocol::MethodFamily::ToolsCall)
            .is_ok()
    );
}

#[test]
fn downstream_context_preserves_supplied_http_trace_id() {
    let context = DownstreamCallContext::http(
        "session-a",
        RequestId::from(NumberOrString::Number(1)),
        ClientType::ClaudeCode,
    )
    .with_trace_id(Arc::<str>::from("4bf92f3577b34da6a3ce929d0e0e4736"));

    assert_eq!(
        context.trace_id.as_ref(),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );
}

#[test]
fn client_metadata_never_changes_principal_identity() {
    let request = RequestId::from(NumberOrString::Number(1));
    let original = DownstreamCallContext::ipc(
        "process-instance",
        request.clone(),
        ClientType::Unknown,
        None,
    );
    let changed =
        DownstreamCallContext::ipc("process-instance", request, ClientType::ClaudeCode, None)
            .with_client_metadata("forged-client", "999");
    assert_eq!(original.principal, changed.principal);
    assert_ne!(original.client_metadata, changed.client_metadata);
}

pub(super) fn router_with_git_commit_tool() -> ToolRouter {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());
    let mut routes = HashMap::new();
    routes.insert(
        "git__commit".to_string(),
        ("git".to_string(), "commit".to_string()),
    );
    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            tools_all: Arc::new(vec![Tool::new(
                Cow::Borrowed("git__commit"),
                Cow::Borrowed("Create a git commit"),
                Arc::new(serde_json::Map::new()),
            )]),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
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
        }
        .with_indexes(),
    ));
    router
}

#[test]
fn a_listed_tool_gives_its_own_name_and_says_when_it_is_not_read_only() {
    let router = router_with_git_commit_tool();
    // The merged name carries Plug's prefix; a watch is written without it.
    assert_eq!(
        router.own_tool("git__commit"),
        Some(("commit".to_string(), false))
    );
    assert_eq!(router.own_tool("git__missing"), None);
}

#[test]
fn a_lost_call_is_sent_again_only_on_the_servers_word() {
    use rmcp::model::ToolAnnotations;
    let resend = |declared: Option<ToolAnnotations>, inferred: Option<ToolAnnotations>| {
        let mut snapshot = (**router_with_git_commit_tool().cache.load()).clone();
        snapshot.tool_risk_inventory.insert(
            "git__commit".to_string(),
            ToolRisk {
                upstream_declared: declared,
                plug_inferred: inferred.clone(),
                effective: inferred,
            },
        );
        snapshot.may_resend("git__commit")
    };
    let mut read_only = ToolAnnotations::default();
    read_only.read_only_hint = Some(true);
    let mut idempotent = ToolAnnotations::default();
    idempotent.idempotent_hint = Some(true);
    assert!(resend(Some(read_only.clone()), None));
    assert!(resend(Some(idempotent.clone()), None));
    assert!(!resend(Some(ToolAnnotations::default()), None));
    // Plug's guess from the tool's name is not the server's word.
    assert!(!resend(None, Some(read_only)));
    assert!(!resend(None, Some(idempotent)));
}

#[test]
fn a_watch_takes_the_servers_word_over_the_tools_name() {
    use rmcp::model::ToolAnnotations;
    let hint = |read_only| {
        let mut annotations = ToolAnnotations::default();
        annotations.read_only_hint = Some(read_only);
        annotations
    };
    let with = |declared: Option<bool>, effective: Option<bool>| {
        let router = router_with_git_commit_tool();
        let mut snapshot = (**router.cache.load()).clone();
        snapshot.tool_risk_inventory.insert(
            "git__commit".to_string(),
            ToolRisk {
                upstream_declared: declared.map(hint),
                plug_inferred: None,
                effective: effective.map(hint),
            },
        );
        router.cache.store(Arc::new(snapshot));
        router
            .watched_tool("git", "commit")
            .map(|(_, read_only)| read_only)
    };
    // The name reads as harmless, the server says the tool writes.
    assert_eq!(with(Some(false), Some(true)), Some(false));
    assert_eq!(with(Some(true), Some(true)), Some(true));
    // A server that says nothing has not said the tool is safe, whatever
    // the name suggests. The owner can still allow the watch by hand.
    assert_eq!(with(None, Some(true)), Some(false));
    assert_eq!(with(None, None), Some(false));
}

#[tokio::test(start_paused = true)]
async fn schedule_tool_list_changed_refresh_debounces_bursts() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(sm, RouterConfig::default()));
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
    let router = Arc::new(ToolRouter::new(sm, RouterConfig::default()));
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
async fn tool_router_list_tools_returns_arc() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());
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

    let d = Tool::new(
        Cow::Borrowed("aaa__important_tool"),
        Cow::Borrowed("desc"),
        Arc::new(serde_json::Map::new()),
    );
    assert_eq!(priority_sort(&d, &a, &priority), std::cmp::Ordering::Less);
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
    let router = ToolRouter::new(sm, RouterConfig::default());

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

    let tools_devin = Arc::new(tools.iter().take(100).cloned().collect::<Vec<_>>());
    let tools_copilot = Arc::new(tools.iter().take(128).cloned().collect::<Vec<_>>());
    let tools_all = Arc::new(tools);

    router.cache.store(Arc::new(
        RouterSnapshot {
            routes: HashMap::new(),
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),
            tools_all,
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin,
            tools_copilot,
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        }
        .with_indexes(),
    ));

    assert_eq!(router.list_tools_for_client(ClientType::Devin).len(), 100);
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
fn list_tools_for_client_ignores_empty_filtered_views_when_filtering_disabled() {
    // Pins the invariant the refresh_tools() filtered-view gate depends on:
    // when `tool_filter_enabled` is false, `refresh_tools()` no longer
    // populates `tools_devin` / `tools_copilot` (they're left empty).
    // `list_tools_for_client_session` must still return the FULL catalog for
    // Devin/Copilot in that case — it has to return `tools_all` early
    // before ever reading those two fields.
    let sm = Arc::new(ServerManager::new());
    let config = RouterConfig {
        tool_filter_enabled: false,
        ..RouterConfig::default()
    };
    let router = ToolRouter::new(sm, config);

    let tools: Vec<Tool> = (0..150)
        .map(|i| {
            Tool::new(
                Cow::Owned(format!("tool_{i:03}")),
                Cow::Owned(format!("Tool {i}")),
                Arc::new(serde_json::Map::new()),
            )
        })
        .collect();

    // Simulate what refresh_tools() now produces when filtering is
    // disabled: tools_devin/tools_copilot stay empty.
    router.cache.store(Arc::new(
        RouterSnapshot {
            routes: HashMap::new(),
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),
            tools_all: Arc::new(tools),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        }
        .with_indexes(),
    ));

    assert_eq!(
        router
            .list_tools_for_client_session(ClientType::Devin, None, None)
            .len(),
        150,
        "Devin must still see the full catalog, not the empty pre-cached view"
    );
    assert_eq!(
        router
            .list_tools_for_client_session(ClientType::VSCodeCopilot, None, None)
            .len(),
        150,
        "Copilot must still see the full catalog, not the empty pre-cached view"
    );
}

fn store_plain_tools_snapshot(router: &ToolRouter, tools: Vec<Tool>) {
    let routes = tools
        .iter()
        .map(|tool| {
            let name = tool.name.to_string();
            let (server, original) = name.split_once("__").unwrap_or(("fixture", &name));
            (name.clone(), (server.to_string(), original.to_string()))
        })
        .collect();
    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),
            tools_all: Arc::new(tools),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        }
        .with_indexes(),
    ));
}

#[test]
fn standard_client_sees_full_catalog_when_filtering_disabled_and_unknown_is_lazy() {
    // A client whose own lazy policy is Standard must get the full catalog
    // when filtering is disabled, even if the global policy (which the
    // Unknown client resolves to) hides tools behind the search bridge.
    let sm = Arc::new(ServerManager::new());
    let mut lazy_tools = LazyToolsConfig {
        mode: crate::types::LazyToolSetting::Bridge,
        ..LazyToolsConfig::default()
    };
    lazy_tools.clients.insert(
        "claude-code".to_string(),
        crate::types::LazyToolSetting::Standard,
    );
    let config = RouterConfig {
        tool_filter_enabled: false,
        lazy_tools,
        ..RouterConfig::default()
    };
    let router = ToolRouter::new(sm, config);
    let tools: Vec<Tool> = (0..3)
        .map(|i| {
            Tool::new(
                Cow::Owned(format!("fixture__tool_{i}")),
                Cow::Owned(format!("Tool {i}")),
                Arc::new(serde_json::Map::new()),
            )
        })
        .collect();
    store_plain_tools_snapshot(&router, tools);

    let names = router
        .list_tools_for_client_session(ClientType::ClaudeCode, None, None)
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["fixture__tool_0", "fixture__tool_1", "fixture__tool_2"]
    );
    // The bridge client itself still gets the bridge surface.
    let bridge_names = router
        .list_tools_for_client_session(ClientType::Unknown, None, None)
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(bridge_names, expected_meta_tool_names());
}

#[test]
fn search_tools_returns_matches() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

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

    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            tools_all: Arc::new(tools),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
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
        }
        .with_indexes(),
    ));

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
fn search_index_scores_match_direct_scoring_and_follow_the_snapshot() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());
    let mut titled = Tool::new(
        Cow::Borrowed("Git_Hub__Create-PR"),
        Cow::Borrowed("Open a Pull Request on GitHub"),
        Arc::new(serde_json::Map::new()),
    );
    titled.title = Some("Create Pull Request".to_string());
    let tools = vec![
        titled,
        Tool::new(
            Cow::Borrowed("slack__send"),
            Cow::Borrowed("Send a message on Slack"),
            Arc::new(serde_json::Map::new()),
        ),
    ];
    store_plain_tools_snapshot(&router, tools.clone());

    let snapshot = router.cache.load_full();
    let index = router.search_index_for(&snapshot);
    assert!(
        Arc::ptr_eq(&index, &router.search_index_for(&snapshot)),
        "the same snapshot must reuse its index"
    );
    let indexed = index
        .entries
        .iter()
        .map(|entry| entry.tool_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(indexed, vec!["Git_Hub__Create-PR", "slack__send"]);

    for query in [
        "pull request",
        "git hub",
        "send slack message",
        "create",
        "x",
    ] {
        let tokens = tokenize_search_query(query);
        let phrase = tokens.join(" ");
        for (entry, tool) in index.entries.iter().zip(&tools) {
            assert_eq!(
                score_normalized_match(&entry.text, &phrase, &tokens),
                score_tool_match(tool, &entry.server_id, &phrase, &tokens),
                "query {query:?} tool {}",
                entry.tool_name
            );
        }
    }

    store_plain_tools_snapshot(
        &router,
        vec![Tool::new(
            Cow::Borrowed("jira__file"),
            Cow::Borrowed("File a ticket"),
            Arc::new(serde_json::Map::new()),
        )],
    );
    let next = router.search_index_for(&router.cache.load_full());
    assert!(
        !Arc::ptr_eq(&index, &next),
        "a new snapshot rebuilds the index"
    );
    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("ticket"));
    let text = format!(
        "{:?}",
        router.handle_search_tools(Some(args), None).unwrap()
    );
    assert!(text.contains("jira__file"));
}

#[test]
fn meta_tool_mode_lists_only_meta_tools() {
    let sm = Arc::new(ServerManager::new());
    let config = RouterConfig {
        meta_tool_mode: true,
        ..Default::default()
    };
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

    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            tools_all: Arc::new(tools),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
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
        }
        .with_indexes(),
    ));

    let visible_tools = router.list_tools_for_client(ClientType::ClaudeCode);
    let names = visible_tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, expected_legacy_meta_tool_names());

    let full_tools = router.list_all_tools_with_risk();
    assert_eq!(full_tools.len(), 1);
    assert_eq!(full_tools[0].0, "git");
    assert_eq!(full_tools[0].1.name.as_ref(), "git__commit");
}

#[test]
fn client_lazy_bridge_policy_lists_meta_tools_for_opencode() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

    router.cache.store(Arc::new(
        RouterSnapshot {
            routes: HashMap::new(),
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),
            tools_all: Arc::new(vec![Tool::new(
                Cow::Borrowed("git__commit"),
                Cow::Borrowed("Create a git commit"),
                Arc::new(serde_json::Map::new()),
            )]),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
            resources_all: Arc::new(Vec::new()),
            resource_templates_all: Arc::new(Vec::new()),
            prompts_all: Arc::new(Vec::new()),
            resource_routes: HashMap::new(),
            prompt_routes: HashMap::new(),
            tool_definition_fingerprints: HashMap::new(),
            tool_risk_inventory: HashMap::new(),
        }
        .with_indexes(),
    ));

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
    let router = ToolRouter::new(sm, RouterConfig::default());

    let mut routes = HashMap::new();
    routes.insert(
        "git__commit".to_string(),
        ("git".to_string(), "commit".to_string()),
    );
    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            tools_all: Arc::new(vec![Tool::new(
                Cow::Borrowed("git__commit"),
                Cow::Borrowed("Create a git commit"),
                Arc::new(serde_json::Map::new()),
            )]),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
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
        }
        .with_indexes(),
    ));

    let session_key = DownstreamTransport::Ipc.lazy_session_key("client-a");
    assert_eq!(
        router
            .list_tools_for_client_session(ClientType::OpenCode, Some(&session_key), None)
            .len(),
        expected_meta_tool_names().len()
    );

    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("commit"));
    let downstream =
        DownstreamCallContext::ipc("client-a", RequestId::Number(1), ClientType::OpenCode, None);
    router
        .handle_search_tools(Some(args), Some(&downstream))
        .expect("search and load tool");

    let visible =
        router.list_tools_for_client_session(ClientType::OpenCode, Some(&session_key), None);
    let names = visible
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    assert!(names.contains(&"git__commit".to_string()));
}

#[test]
fn bridge_search_keeps_session_working_set_bounded() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

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
    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            tools_all: Arc::new(tools),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
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
        }
        .with_indexes(),
    ));

    let downstream =
        DownstreamCallContext::ipc("client-a", RequestId::Number(1), ClientType::OpenCode, None);
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

    let session_key = DownstreamTransport::Ipc.lazy_session_key("client-a");
    let visible =
        router.list_tools_for_client_session(ClientType::OpenCode, Some(&session_key), None);
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
    let router = ToolRouter::new(sm, RouterConfig::default());

    let mut routes = HashMap::new();
    routes.insert(
        "git__commit".to_string(),
        ("git".to_string(), "commit".to_string()),
    );
    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            tools_all: Arc::new(vec![Tool::new(
                Cow::Borrowed("git__commit"),
                Cow::Borrowed("Create a git commit"),
                Arc::new(serde_json::Map::new()),
            )]),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
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
        }
        .with_indexes(),
    ));

    let mut notifications = router.subscribe_notifications();
    let downstream =
        DownstreamCallContext::ipc("client-a", RequestId::Number(1), ClientType::OpenCode, None);
    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("commit"));
    router
        .handle_search_tools(Some(args), Some(&downstream))
        .expect("search and load tool");
    assert_eq!(
        notifications.try_recv().expect("search notification"),
        ProtocolNotification::ToolListChangedFor {
            target: NotificationTarget::Ipc {
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
    let router = ToolRouter::new(sm, RouterConfig::default());

    let mut routes = HashMap::new();
    routes.insert(
        "git__commit".to_string(),
        ("git".to_string(), "commit".to_string()),
    );
    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            tools_all: Arc::new(vec![Tool::new(
                Cow::Borrowed("git__commit"),
                Cow::Borrowed("Create a git commit"),
                Arc::new(serde_json::Map::new()),
            )]),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
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
        }
        .with_indexes(),
    ));

    let session_key = DownstreamTransport::Ipc.lazy_session_key("client-a");
    router.list_tools_for_client_session(ClientType::OpenCode, Some(&session_key), None);

    let err = router
        .call_tool_with_context(
            "git__commit",
            None,
            None,
            Some(DownstreamCallContext::ipc(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
                None,
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
            Some(DownstreamCallContext::ipc(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
                None,
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
            None,
            Some(DownstreamCallContext::ipc(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
                None,
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
            None,
            Some(DownstreamCallContext::ipc(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
                None,
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
            Some(DownstreamCallContext::ipc(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
                None,
            )),
        )
        .await
        .expect_err("invoke wrapper should not exist in the bridge surface");

    assert!(err.to_string().contains("plug__invoke_tool"));
}

#[tokio::test]
async fn disabled_bridge_search_tool_is_not_visible_or_callable() {
    let sm = Arc::new(ServerManager::new());
    let config = RouterConfig::from(&Config {
        disabled_tools: vec!["plug__search_tools".to_string()],
        ..Default::default()
    });
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
            Some(DownstreamCallContext::ipc(
                "client-a",
                RequestId::Number(1),
                ClientType::OpenCode,
                None,
            )),
        )
        .await
        .expect_err("disabled search meta-tool should reject");
    assert!(err.to_string().contains("plug__search_tools"));
}

#[test]
fn synthesized_capabilities_include_tasks_when_tools_exist() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

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
    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            tools_all: Arc::new(tools),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
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
        }
        .with_indexes(),
    ));

    let caps = router.synthesized_capabilities();
    assert!(crate::protocol::legacy_tasks_capability(&caps));
}

#[test]
fn synthesized_capabilities_suppress_tasks_for_bridge_clients() {
    let router = router_with_git_commit_tool();

    let caps = router.synthesized_capabilities_for_client(ClientType::OpenCode);

    assert!(caps.tools.is_some());
    assert!(!crate::protocol::legacy_tasks_capability(&caps));
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

#[test]
fn only_a_lost_connection_is_a_lost_session_and_only_a_refused_one_proves_the_call_never_arrived() {
    use rmcp::service::ServiceError;
    let send_failure = |message: &str| {
        ServiceError::TransportSend(rmcp::transport::DynamicTransportError::from_parts(
            "test",
            std::any::TypeId::of::<()>(),
            message.to_string().into(),
        ))
    };
    let cases = [
        (
            send_failure("HTTP 404: Session not found"),
            Some(LostSession::NeverArrived),
        ),
        (
            send_failure("Connection refused (os error 61)"),
            Some(LostSession::NeverArrived),
        ),
        (
            send_failure("connection reset by peer"),
            Some(LostSession::MayHaveArrived),
        ),
        (
            send_failure("Broken pipe"),
            Some(LostSession::MayHaveArrived),
        ),
        (
            ServiceError::TransportClosed,
            Some(LostSession::MayHaveArrived),
        ),
        (send_failure("invalid header value"), None),
        (
            ServiceError::McpError(McpError::internal_error("tool failed", None)),
            None,
        ),
        (
            ServiceError::Timeout {
                timeout: Duration::from_secs(30),
            },
            None,
        ),
        (ServiceError::UnexpectedResponse, None),
    ];
    for (error, expected) in cases {
        assert_eq!(LostSession::of(&error), expected, "{error}");
    }
}

#[test]
fn case_insensitive_route_lookup() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

    let mut routes = HashMap::new();
    routes.insert(
        "Slack__search_messages".to_string(),
        (
            "slack".to_string(),
            "conversations_search_messages".to_string(),
        ),
    );

    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            tools_all: Arc::new(Vec::new()),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
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
        }
        .with_indexes(),
    ));

    let snapshot = router.cache.load();
    assert!(snapshot.routes.contains_key("Slack__search_messages"));
    let found = snapshot
        .resolve_route("slack__search_messages")
        .expect("case-insensitive route lookup");
    assert_eq!(found.0, "slack");
    assert_eq!(found.1, "conversations_search_messages");
}

#[tokio::test(start_paused = true)]
async fn call_tool_times_out_waiting_for_semaphore() {
    let server_manager = Arc::new(ServerManager::new());
    let router = ToolRouter::new(server_manager.clone(), RouterConfig::default());

    let upstream =
        connect_subscribable_upstream("busy-server", SubscribableUpstreamState::new(&[])).await;
    server_manager.replace_server("busy-server", upstream).await;

    server_manager.apply_live_settings(
        "busy-server",
        &crate::config::ServerConfig {
            max_concurrent: 0,
            ..Default::default()
        },
    );

    let mut routes = HashMap::new();
    routes.insert(
        "Busy__tool".to_string(),
        ("busy-server".to_string(), "tool".to_string()),
    );
    router.cache.store(Arc::new(
        RouterSnapshot {
            routes,
            tools_all: Arc::new(Vec::new()),
            meta_tools_all: Arc::new(build_meta_tools()),
            tools_devin: Arc::new(Vec::new()),
            tools_copilot: Arc::new(Vec::new()),
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
        }
        .with_indexes(),
    ));

    let call = router.call_tool("Busy__tool", None);
    tokio::pin!(call);

    tokio::time::advance(Duration::from_secs(31)).await;

    let err = call.await.unwrap_err();
    assert!(
        err.message.contains("server overloaded"),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn queued_tool_call_resolves_reconnected_upstream_after_permit() {
    let server_manager = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(
        Arc::clone(&server_manager),
        RouterConfig::default(),
    ));
    let old_state = SubscribableUpstreamState::new(&[]);
    let old_upstream = connect_subscribable_upstream("queued-server", Arc::clone(&old_state)).await;
    server_manager
        .replace_server("queued-server", old_upstream)
        .await;

    server_manager.apply_live_settings(
        "queued-server",
        &crate::config::ServerConfig {
            max_concurrent: 0,
            ..Default::default()
        },
    );
    let semaphore = server_manager
        .call_slots("queued-server")
        .expect("queued-server has slots");
    publish_tool_route_snapshot(&router, "Queued__tool", "queued-server");

    let call_router = Arc::clone(&router);
    let call = tokio::spawn(async move {
        call_router
            .call_tool_with_context(
                "Queued__tool",
                None,
                None,
                Some(DownstreamCallContext::ipc(
                    "queued-client",
                    RequestId::Number(1),
                    ClientType::Unknown,
                    None,
                )),
            )
            .await
    });
    while router.active_call_count() == 0 {
        tokio::task::yield_now().await;
    }

    let replacement_state = SubscribableUpstreamState::new(&[]);
    let replacement =
        connect_subscribable_upstream("queued-server", Arc::clone(&replacement_state)).await;
    server_manager
        .replace_server("queued-server", replacement)
        .await;
    semaphore.add_permits(1);

    call.await
        .expect("queued call task")
        .expect("queued call should use replacement upstream");
    assert_eq!(
        old_state.tool_calls.load(Ordering::SeqCst),
        0,
        "the retired upstream must not receive the queued call"
    );
    assert_eq!(
        replacement_state.tool_calls.load(Ordering::SeqCst),
        1,
        "the live replacement must receive the queued call"
    );
}

#[test]
fn list_tools_page_for_client_uses_cursor_pagination() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());
    let tools: Vec<Tool> = (0..750)
        .map(|index| {
            Tool::new(
                Cow::Owned(format!("tool_{index}")),
                Cow::Borrowed("desc"),
                Arc::new(serde_json::Map::new()),
            )
        })
        .collect();
    router.cache.store(Arc::new(
        RouterSnapshot {
            routes: HashMap::new(),
            routes_lower: HashMap::new(),
            tools_by_name: HashMap::new(),
            tools_by_name_lower: HashMap::new(),
            tools_devin: Arc::new(tools.iter().take(100).cloned().collect()),
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
        }
        .with_indexes(),
    ));

    let first = router.list_tools_page_for_client(ClientType::Unknown, Some(Default::default()));
    assert_eq!(first.tools.len(), 500);
    assert_eq!(first.next_cursor.as_deref(), Some("500"));

    let mut second_request = PaginatedRequestParams::default();
    second_request.cursor = first.next_cursor;
    let second = router.list_tools_page_for_client(ClientType::Unknown, Some(second_request));
    assert_eq!(second.tools.len(), 250);
    assert!(second.next_cursor.is_none());
}

#[test]
fn paginated_result_returns_mid_cursor_page_from_borrowed_slice() {
    // `paginated_result` now takes `&[T]` instead of an owned `Vec<T>`; this
    // pins that a mid-cursor page sliced from a borrowed input still returns
    // the same items and next_cursor as the old owned-Vec implementation.
    let items: Vec<i32> = (0..750).collect();

    let mut request = PaginatedRequestParams::default();
    request.cursor = Some("500".to_string());

    let (page, next_cursor) = paginated_result(&items, Some(request), |page, next_cursor| {
        (page, next_cursor)
    });

    assert_eq!(page, items[500..750].to_vec());
    assert!(next_cursor.is_none());
}

#[tokio::test]
async fn route_upstream_progress_publishes_targeted_notification() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());
    let mut rx = router.subscribe_notifications();
    let progress_token = ProgressToken(NumberOrString::String(Arc::from("progress-1")));

    router
        .register_active_call(
            42,
            ActiveCallRecord {
                downstream: DownstreamCallContext::ipc(
                    Arc::from("client-1"),
                    RequestId::from(NumberOrString::Number(1)),
                    crate::types::ClientType::Unknown,
                    None,
                ),
                upstream_server_id: "upstream".to_string(),
                upstream_request_id: None,
                downstream_progress_token: Some(progress_token.clone()),
                upstream_progress_token: Some(progress_token.clone()),
                pending_cancel_reason: None,
            },
        )
        .expect("register active call");

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
                NotificationTarget::Ipc {
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
    let config = RouterConfig::default();
    let router = ToolRouter::new(sm, config);

    // No upstreams → no resources capability at all
    let caps = router.synthesized_capabilities();
    assert!(caps.resources.is_none());
}

#[test]
fn resource_subscription_registry_lifecycle() {
    let sm = Arc::new(ServerManager::new());
    let config = RouterConfig::default();
    let router = ToolRouter::new(sm, config);

    let target = NotificationTarget::Ipc {
        client_id: Arc::from("test-client"),
    };

    // Registry starts empty
    assert_eq!(router.resource_subscriptions.len(), 0);

    // Insert directly (bypassing upstream check for unit test)
    router
        .resource_subscriptions
        .insert_active_for_test("file:///test", target.clone());
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
    let router = ToolRouter::new(sm, RouterConfig::default());
    let caps = router.synthesized_capabilities();
    assert!(caps.completions.is_none());
}

#[test]
fn complete_request_params_serde_roundtrip() {
    let params = CompleteRequestParams::new(
        Reference::for_prompt("test-prompt"),
        ArgumentInfo::new("arg1", "partial"),
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
    let router = ToolRouter::new(sm, RouterConfig::default());
    // Default level is Warning, so send a warning-level message
    let mut rx = router.subscribe_logging();

    router.route_upstream_logging_message(
        "github",
        LoggingMessageNotificationParam::new(
            LoggingLevel::Warning,
            serde_json::json!("something happened"),
        )
        .with_logger("default"),
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
    let router = ToolRouter::new(sm, RouterConfig::default());
    let mut rx = router.subscribe_logging();

    // Default level is Warning — debug should be filtered
    router.route_upstream_logging_message(
        "github",
        LoggingMessageNotificationParam::new(LoggingLevel::Debug, serde_json::json!("debug noise")),
    );

    assert!(rx.try_recv().is_err(), "debug message should be filtered");
}

#[test]
fn set_client_log_level_changes_effective_level() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

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
    let router = ToolRouter::new(sm, RouterConfig::default());
    let mut rx = router.subscribe_logging();

    // Lower threshold to Debug
    router.set_client_log_level("client-a", LoggingLevel::Debug);

    // Now debug messages should pass through
    router.route_upstream_logging_message(
        "server1",
        LoggingMessageNotificationParam::new(LoggingLevel::Debug, serde_json::json!("debug info")),
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
    let router = ToolRouter::new(sm, RouterConfig::default());
    let mut control_rx = router.subscribe_notifications();
    let mut logging_rx = router.subscribe_logging();

    // Send a logging message
    router.route_upstream_logging_message(
        "server1",
        LoggingMessageNotificationParam::new(LoggingLevel::Warning, serde_json::json!("log msg")),
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
    let router = ToolRouter::new(sm, RouterConfig::default());

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
    let router = ToolRouter::new(sm, RouterConfig::default());

    let result = router.list_roots_union();
    assert!(result.roots.is_empty());
}

#[test]
fn set_roots_for_target_returns_true_on_first_insert() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

    let target = NotificationTarget::Ipc {
        client_id: Arc::from("client-1"),
    };
    let roots = vec![make_root("file:///project-a", Some("Project A"))];

    assert!(router.set_roots_for_target(target, roots));
}

#[test]
fn set_roots_for_target_returns_false_when_unchanged() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

    let target = NotificationTarget::Ipc {
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
    let router = ToolRouter::new(sm, RouterConfig::default());

    let target = NotificationTarget::Ipc {
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
    let router = ToolRouter::new(sm, RouterConfig::default());

    let target = NotificationTarget::Ipc {
        client_id: Arc::from("client-1"),
    };
    let roots = vec![make_root("file:///project-a", None)];

    router.set_roots_for_target(target.clone(), roots);
    assert!(router.clear_roots_for_target(&target));
}

#[test]
fn clear_roots_for_target_returns_false_when_not_existed() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

    let target = NotificationTarget::Ipc {
        client_id: Arc::from("client-nonexistent"),
    };
    assert!(!router.clear_roots_for_target(&target));
}

#[test]
fn list_roots_union_deduplicates_by_uri() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

    // Client 1 has roots A and B
    let target1 = NotificationTarget::Ipc {
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
    let router = ToolRouter::new(sm, RouterConfig::default());

    let target = NotificationTarget::Ipc {
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
    let router = ToolRouter::new(sm, RouterConfig::default());

    let target1 = NotificationTarget::Ipc {
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
    let router = ToolRouter::new(sm, RouterConfig::default());

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
    let router = ToolRouter::new(sm, RouterConfig::default());

    router
        .register_active_call(
            1,
            ActiveCallRecord {
                downstream: DownstreamCallContext::ipc(
                    Arc::from("client-a"),
                    RequestId::from(NumberOrString::Number(1)),
                    crate::types::ClientType::Unknown,
                    None,
                ),
                upstream_server_id: "s1".to_string(),
                upstream_request_id: Some(RequestId::from(NumberOrString::Number(101))),
                downstream_progress_token: None,
                upstream_progress_token: None,
                pending_cancel_reason: None,
            },
        )
        .expect("register first active call");
    router
        .register_active_call(
            2,
            ActiveCallRecord {
                downstream: DownstreamCallContext::http(
                    Arc::from("session-b"),
                    RequestId::from(NumberOrString::Number(2)),
                    crate::types::ClientType::Unknown,
                ),
                upstream_server_id: "s1".to_string(),
                upstream_request_id: Some(RequestId::from(NumberOrString::Number(202))),
                downstream_progress_token: None,
                upstream_progress_token: None,
                pending_cancel_reason: None,
            },
        )
        .expect("register second active call");

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
    let router = Arc::new(ToolRouter::new(sm, RouterConfig::default()));
    let mut rx = router.subscribe_notifications();

    router
        .register_active_call(
            1,
            ActiveCallRecord {
                downstream: DownstreamCallContext::ipc(
                    Arc::from("client-a"),
                    RequestId::from(NumberOrString::Number(1)),
                    crate::types::ClientType::Unknown,
                    None,
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
        )
        .expect("register active call");

    router.route_upstream_progress(
        "s1",
        ProgressNotificationParam::new(
            ProgressToken(NumberOrString::String(Arc::from("upstream-token"))),
            0.5,
        )
        .with_total(1.0),
    );

    let notification = rx.try_recv().expect("progress notification");
    match notification {
        ProtocolNotification::Progress { target, params } => {
            assert_eq!(
                target,
                NotificationTarget::Ipc {
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
fn test_anonymous_upstream_cancellation_preserves_active_call() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

    router
        .register_active_call(
            1,
            ActiveCallRecord {
                downstream: DownstreamCallContext::ipc(
                    Arc::from("client-a"),
                    RequestId::from(NumberOrString::Number(1)),
                    crate::types::ClientType::Unknown,
                    None,
                ),
                upstream_server_id: "s1".to_string(),
                upstream_request_id: Some(RequestId::from(NumberOrString::Number(101))),
                downstream_progress_token: None,
                upstream_progress_token: None,
                pending_cancel_reason: None,
            },
        )
        .expect("register active call");

    router.route_upstream_cancelled(
        "s1",
        CancelledNotificationParam::new(None, Some("session closed".to_string())),
    );

    assert_eq!(router.active_call_count(), 1);
    assert!(router.active_calls.contains_key(&1));
}

#[test]
fn test_attach_upstream_request_id_preserves_pending_cancel_reason() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

    router
        .register_active_call(
            1,
            ActiveCallRecord {
                downstream: DownstreamCallContext::ipc(
                    Arc::from("client-a"),
                    RequestId::from(NumberOrString::Number(1)),
                    crate::types::ClientType::Unknown,
                    None,
                ),
                upstream_server_id: "s1".to_string(),
                upstream_request_id: None,
                downstream_progress_token: None,
                upstream_progress_token: None,
                pending_cancel_reason: Some(Some("cancelled".to_string())),
            },
        )
        .expect("register active call");

    let progress_token = ProgressToken(NumberOrString::Number(43));
    router.attach_upstream_request_id(
        1,
        "s1",
        RequestId::from(NumberOrString::Number(42)),
        Some(progress_token.clone()),
    );
    let record = router.active_calls.get(&1).expect("active call").clone();
    assert_eq!(
        record.upstream_request_id,
        Some(RequestId::from(NumberOrString::Number(42)))
    );
    assert!(record.pending_cancel_reason.is_none());
    assert_eq!(record.upstream_progress_token, Some(progress_token.clone()));
    assert_eq!(
        router
            .upstream_progress_lookup
            .get(&UpstreamProgressKey {
                server_id: "s1".to_string(),
                progress_token,
            })
            .map(|entry| *entry),
        Some(1)
    );
}

#[test]
fn test_register_active_call_uses_upstream_progress_token_for_lookup() {
    let sm = Arc::new(ServerManager::new());
    let router = ToolRouter::new(sm, RouterConfig::default());

    router
        .register_active_call(
            1,
            ActiveCallRecord {
                downstream: DownstreamCallContext::ipc(
                    Arc::from("client-a"),
                    RequestId::from(NumberOrString::Number(1)),
                    crate::types::ClientType::Unknown,
                    None,
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
        )
        .expect("register active call");

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
        DownstreamCallContext::ipc(
            Arc::from("client-a"),
            RequestId::from(NumberOrString::Number(1)),
            crate::types::ClientType::Unknown,
            None
        )
        .notification_target(),
        NotificationTarget::Ipc {
            client_id: Arc::from("client-a"),
        },
    );
}

#[test]
fn mrtr_pair_policy_activates_only_native_modern_rounds() {
    use crate::protocol::ProtocolEra::{Legacy, Modern};

    assert!(mrtr_pair_supported(Modern, Modern, false, false));
    assert!(mrtr_pair_supported(Modern, Modern, true, false));
    assert!(mrtr_pair_supported(Modern, Legacy, false, false));
    assert!(mrtr_pair_supported(Legacy, Legacy, false, false));

    // No mixed-era continuation, no task+MRTR, and no legacy call into an
    // upstream that may return an unrepresentable InputRequired result.
    assert!(!mrtr_pair_supported(Modern, Legacy, true, false));
    assert!(!mrtr_pair_supported(Legacy, Modern, true, false));
    assert!(!mrtr_pair_supported(Legacy, Modern, false, false));
    assert!(!mrtr_pair_supported(Modern, Modern, false, true));
}

#[test]
fn admitted_extension_policy_is_identical_for_all_protocol_pair_rows() {
    let source = MetaObject::from(
        serde_json::json!({
            "example.test/typed": {
                "boolean": true,
                "number": 7,
                "array": [null, "value"]
            },
            "traceparent": "00-0af7651916cd43dd8448eb211c80319c-00f067aa0ba902b7-01"
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    for downstream in [
        crate::protocol::ProtocolEra::Legacy,
        crate::protocol::ProtocolEra::Modern,
    ] {
        for upstream in [
            crate::protocol::ProtocolEra::Legacy,
            crate::protocol::ProtocolEra::Modern,
        ] {
            let admitted = crate::types::ExtensionEnvelope::from_peer_meta(Some(&source))
                .unwrap()
                .into_meta()
                .unwrap();
            assert_eq!(admitted, source, "pair {downstream:?} -> {upstream:?}");
        }
    }
}

// ─── refresh_tools ↔ subscription race tests ────────────────────────────────
//
// These drive `refresh_tools` itself against in-process duplex-connected
// upstreams whose `unsubscribe` handlers can be parked on per-URI gates,
// letting a test deterministically place a racing downstream subscribe or
// unsubscribe inside a refresh pass's prune/rebind window.

use rmcp::ServiceExt as _;

use crate::server::{UpstreamClientHandler, UpstreamServer};
use crate::types::ServerHealth;

/// Async gate: `wait()` parks until `open()`. Same shape as the registry
/// unit tests' Gate, but per-URI on the upstream side.
struct SubGate {
    notify: tokio::sync::Notify,
    open: AtomicBool,
}

impl SubGate {
    fn new_closed() -> Arc<Self> {
        Arc::new(Self {
            notify: tokio::sync::Notify::new(),
            open: AtomicBool::new(false),
        })
    }

    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            let notified = self.notify.notified();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

/// Shared state backing a `SubscribableUpstreamHandler`: the resource list
/// it serves (mutable between refreshes to flip routes) and per-URI
/// subscribe/unsubscribe call logs plus gates.
struct SubscribableUpstreamState {
    resources: std::sync::Mutex<Vec<String>>,
    list_calls: std::sync::atomic::AtomicUsize,
    list_gate: std::sync::Mutex<Option<Arc<SubGate>>>,
    list_entered: tokio::sync::Notify,
    subscribe_log: std::sync::Mutex<Vec<String>>,
    unsubscribe_log: std::sync::Mutex<Vec<String>>,
    subscribe_gates: std::sync::Mutex<HashMap<String, Arc<SubGate>>>,
    unsubscribe_gates: std::sync::Mutex<HashMap<String, Arc<SubGate>>>,
    subscribe_errors: std::sync::Mutex<HashSet<String>>,
    subscribe_entered: tokio::sync::Notify,
    unsubscribe_entered: tokio::sync::Notify,
    tool_calls: std::sync::atomic::AtomicUsize,
    tool_gate: std::sync::Mutex<Option<Arc<SubGate>>>,
    tool_entered: tokio::sync::Notify,
    tool_requires_input: std::sync::atomic::AtomicBool,
    tool_growing_input: std::sync::atomic::AtomicBool,
}

impl SubscribableUpstreamState {
    fn new(resources: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            resources: std::sync::Mutex::new(resources.iter().map(|uri| uri.to_string()).collect()),
            list_calls: std::sync::atomic::AtomicUsize::new(0),
            list_gate: std::sync::Mutex::new(None),
            list_entered: tokio::sync::Notify::new(),
            subscribe_log: std::sync::Mutex::new(Vec::new()),
            unsubscribe_log: std::sync::Mutex::new(Vec::new()),
            subscribe_gates: std::sync::Mutex::new(HashMap::new()),
            unsubscribe_gates: std::sync::Mutex::new(HashMap::new()),
            subscribe_errors: std::sync::Mutex::new(HashSet::new()),
            subscribe_entered: tokio::sync::Notify::new(),
            unsubscribe_entered: tokio::sync::Notify::new(),
            tool_calls: std::sync::atomic::AtomicUsize::new(0),
            tool_gate: std::sync::Mutex::new(None),
            tool_entered: tokio::sync::Notify::new(),
            tool_requires_input: std::sync::atomic::AtomicBool::new(false),
            tool_growing_input: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn set_resources(&self, resources: &[&str]) {
        *self.resources.lock().unwrap() = resources.iter().map(|uri| uri.to_string()).collect();
    }

    fn close_list_gate(&self) -> Arc<SubGate> {
        let gate = SubGate::new_closed();
        *self.list_gate.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    fn clear_list_gate(&self) {
        self.list_gate.lock().unwrap().take();
    }

    fn list_count(&self) -> usize {
        self.list_calls.load(Ordering::SeqCst)
    }

    /// All subsequent `resources/subscribe` calls for `uri` park until the
    /// returned gate is opened.
    fn close_subscribe_gate(&self, uri: &str) -> Arc<SubGate> {
        let gate = SubGate::new_closed();
        self.subscribe_gates
            .lock()
            .unwrap()
            .insert(uri.to_string(), Arc::clone(&gate));
        gate
    }

    /// All subsequent `resources/unsubscribe` calls for `uri` park until the
    /// returned gate is opened.
    fn close_unsubscribe_gate(&self, uri: &str) -> Arc<SubGate> {
        let gate = SubGate::new_closed();
        self.unsubscribe_gates
            .lock()
            .unwrap()
            .insert(uri.to_string(), Arc::clone(&gate));
        gate
    }

    /// All subsequent `resources/subscribe` calls for `uri` fail (after
    /// still being logged/counted) until `clear_subscribe_failure`.
    fn fail_subscribe(&self, uri: &str) {
        self.subscribe_errors
            .lock()
            .unwrap()
            .insert(uri.to_string());
    }

    fn clear_subscribe_failure(&self, uri: &str) {
        self.subscribe_errors.lock().unwrap().remove(uri);
    }

    fn subscribe_count(&self, uri: &str) -> usize {
        self.subscribe_log
            .lock()
            .unwrap()
            .iter()
            .filter(|logged| logged.as_str() == uri)
            .count()
    }

    fn unsubscribe_count(&self, uri: &str) -> usize {
        self.unsubscribe_log
            .lock()
            .unwrap()
            .iter()
            .filter(|logged| logged.as_str() == uri)
            .count()
    }
}

struct SubscribableUpstreamHandler {
    state: Arc<SubscribableUpstreamState>,
}

impl ServerHandler for SubscribableUpstreamHandler {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        let mut resources = rmcp::model::ResourcesCapability::default();
        resources.subscribe = Some(true);
        resources.list_changed = Some(false);
        capabilities.resources = Some(resources);
        ServerInfo::new(capabilities)
    }

    fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        self.state.list_calls.fetch_add(1, Ordering::SeqCst);
        let uris = self.state.resources.lock().unwrap().clone();
        let gate = self.state.list_gate.lock().unwrap().clone();
        self.state.list_entered.notify_waiters();
        async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
            Ok(ListResourcesResult::with_all_items(
                uris.iter()
                    .map(|uri| Resource::new(uri.as_str(), uri.as_str()))
                    .collect(),
            ))
        }
    }

    fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        let state = Arc::clone(&self.state);
        async move {
            state
                .subscribe_log
                .lock()
                .unwrap()
                .push(request.uri.clone());
            let gate = state
                .subscribe_gates
                .lock()
                .unwrap()
                .get(&request.uri)
                .cloned();
            state.subscribe_entered.notify_waiters();
            if let Some(gate) = gate {
                gate.wait().await;
            }
            if state
                .subscribe_errors
                .lock()
                .unwrap()
                .contains(&request.uri)
            {
                return Err(McpError::internal_error(
                    "test-injected subscribe failure".to_string(),
                    None,
                ));
            }
            Ok(())
        }
    }

    fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), McpError>> + Send + '_ {
        let state = Arc::clone(&self.state);
        async move {
            state
                .unsubscribe_log
                .lock()
                .unwrap()
                .push(request.uri.clone());
            let gate = state
                .unsubscribe_gates
                .lock()
                .unwrap()
                .get(&request.uri)
                .cloned();
            state.unsubscribe_entered.notify_waiters();
            if let Some(gate) = gate {
                gate.wait().await;
            }
            Ok(())
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
        let state = Arc::clone(&self.state);
        async move {
            let invocation = state
                .tool_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            state.tool_entered.notify_one();
            let gate = state.tool_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.wait().await;
            }
            if state
                .tool_requires_input
                .load(std::sync::atomic::Ordering::SeqCst)
                && (request.input_responses.is_none()
                    || (state
                        .tool_growing_input
                        .load(std::sync::atomic::Ordering::SeqCst)
                        && invocation == 1))
            {
                let mut requests = InputRequests::from_iter([(
                    "answer".to_string(),
                    InputRequest::Elicitation(ElicitRequest::new(
                        ElicitRequestParams::UrlElicitationParams {
                            meta: None,
                            message: "Continue".to_string(),
                            url: "https://example.test/continue".to_string(),
                            elicitation_id: "answer".to_string(),
                        },
                    )),
                )]);
                if invocation == 1 {
                    requests.insert(
                        "details".to_string(),
                        InputRequest::Elicitation(ElicitRequest::new(
                            ElicitRequestParams::UrlElicitationParams {
                                meta: None,
                                message: "Provide more detail".repeat(64),
                                url: "https://example.test/details".to_string(),
                                elicitation_id: "details".to_string(),
                            },
                        )),
                    );
                }
                return Ok(rmcp::model::InputRequiredResult::new(
                    Some(requests),
                    Some(format!("raw-upstream-state-{invocation}")),
                )
                .into());
            }
            Ok(CallToolResult::success(Vec::new()).into())
        }
    }
}

/// Build a real, duplex-connected `UpstreamServer` backed by the given
/// state, mirroring the pattern in `server::tests`.
async fn connect_subscribable_upstream(
    name: &str,
    state: Arc<SubscribableUpstreamState>,
) -> UpstreamServer {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let server = SubscribableUpstreamHandler { state }
            .serve(server_transport)
            .await
            .expect("start subscribable upstream test server");
        let _ = server.waiting().await;
    });

    let tools = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
    let handler = Arc::new(UpstreamClientHandler::new_for_tests(
        Arc::from(name.to_string()),
        Arc::clone(&tools),
        crate::server::no_events(),
    ));
    let client = handler
        .serve(client_transport)
        .await
        .expect("connect subscribable upstream test client");

    let mut capabilities = ServerCapabilities::default();
    let mut resources = rmcp::model::ResourcesCapability::default();
    resources.subscribe = Some(true);
    resources.list_changed = Some(false);
    capabilities.resources = Some(resources);

    UpstreamServer {
        name: name.to_string(),
        config: subscribable_test_server_config(),
        client,
        tools,
        capabilities,
        upstream: None,
        protocol_era: crate::protocol::ProtocolEra::Legacy,
        selected_protocol_version: crate::protocol::SUPPORTED_PROTOCOL_VERSION.to_string(),
        protocol_gate_state: 0,
        connection: crate::server::ConnectionGeneration::new(),
        health: ServerHealth::Healthy,
        child_pid: None,
        stderr_tail: None,
    }
}

async fn connect_modern_tool_upstream(
    name: &str,
    state: Arc<SubscribableUpstreamState>,
) -> UpstreamServer {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let server = SubscribableUpstreamHandler { state }
            .serve(server_transport)
            .await
            .expect("start modern tool upstream test server");
        let _ = server.waiting().await;
    });
    let tools = Arc::new(ArcSwap::from_pointee(Vec::<Tool>::new()));
    let handler = Arc::new(UpstreamClientHandler::new_for_tests_with_protocol(
        Arc::from(name.to_string()),
        Arc::clone(&tools),
        crate::server::no_events(),
        rmcp::model::ProtocolVersion::V_2026_07_28,
    ));
    let client = handler
        .serve(client_transport)
        .await
        .expect("connect modern tool upstream test client");
    UpstreamServer {
        name: name.to_string(),
        config: subscribable_test_server_config(),
        client,
        tools,
        capabilities: ServerCapabilities::default(),
        upstream: None,
        protocol_era: crate::protocol::ProtocolEra::Modern,
        selected_protocol_version: crate::protocol::MODERN_PROTOCOL_VERSION.to_string(),
        protocol_gate_state: 0,
        connection: crate::server::ConnectionGeneration::new(),
        health: ServerHealth::Healthy,
        child_pid: None,
        stderr_tail: None,
    }
}

fn subscribable_test_server_config() -> crate::config::ServerConfig {
    crate::config::ServerConfig {
        command: Some("fake".to_string()),
        call_timeout_secs: 30,
        circuit_breaker_enabled: false,
        ..Default::default()
    }
}

fn sub_target(id: &str) -> NotificationTarget {
    NotificationTarget::Ipc {
        client_id: Arc::from(id),
    }
}

#[tokio::test]
async fn overlapping_refreshes_share_one_trailing_pass_and_publish_fresh_catalog() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
    let state = SubscribableUpstreamState::new(&["memory://old"]);
    sm.replace_server(
        "mock",
        connect_subscribable_upstream("mock", Arc::clone(&state)).await,
    )
    .await;
    router.refresh_tools().await;
    assert_eq!(state.list_count(), 1);

    let gate = state.close_list_gate();
    let entered = state.list_entered.notified();
    let leader_router = Arc::clone(&router);
    let leader = tokio::spawn(async move { leader_router.refresh_tools().await });
    entered.await;

    let followers_router = Arc::clone(&router);
    let followers = tokio::spawn(async move {
        futures::future::join_all((0..8).map(|_| followers_router.refresh_tools())).await;
    });
    for _ in 0..100 {
        if router.refresh_request_count.load(Ordering::SeqCst) == 10 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(router.refresh_request_count.load(Ordering::SeqCst), 10);
    assert!(
        router
            .refresh_coordinator
            .state
            .lock()
            .unwrap()
            .trailing_requested,
        "overlapping callers must request one trailing refresh"
    );

    state.set_resources(&["memory://new"]);
    state.clear_list_gate();
    gate.open();
    leader.await.unwrap();
    followers.await.unwrap();

    assert_eq!(router.refresh_request_count.load(Ordering::SeqCst), 10);
    assert_eq!(router.refresh_pass_count.load(Ordering::SeqCst), 3);
    assert_eq!(state.list_count(), 3, "initial + active + trailing");
    assert!(
        router
            .list_resources()
            .iter()
            .any(|resource| resource.uri == "memory://new"),
        "trailing pass must publish catalog state newer than active pass"
    );
    assert!(
        !router
            .list_resources()
            .iter()
            .any(|resource| resource.uri == "memory://old")
    );
}

#[tokio::test]
async fn cancelled_refresh_leader_does_not_wedge_waiting_refresh() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
    let state = SubscribableUpstreamState::new(&["memory://old"]);
    sm.replace_server(
        "mock",
        connect_subscribable_upstream("mock", Arc::clone(&state)).await,
    )
    .await;
    router.refresh_tools().await;

    let gate = state.close_list_gate();
    let entered = state.list_entered.notified();
    let leader_router = Arc::clone(&router);
    let leader = tokio::spawn(async move { leader_router.refresh_tools().await });
    entered.await;

    let waiter_router = Arc::clone(&router);
    let waiter = tokio::spawn(async move { waiter_router.refresh_tools().await });
    for _ in 0..100 {
        if router
            .refresh_coordinator
            .state
            .lock()
            .unwrap()
            .trailing_requested
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    leader.abort();
    assert!(leader.await.unwrap_err().is_cancelled());

    state.set_resources(&["memory://recovered"]);
    state.clear_list_gate();
    gate.open();
    tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("waiting refresh must recover leadership after cancellation")
        .unwrap();
    assert!(
        router
            .list_resources()
            .iter()
            .any(|resource| resource.uri == "memory://recovered")
    );
}

/// Publish a snapshot whose `resource_routes` maps exactly `uri -> server`,
/// simulating a `refresh_tools` publish landing without running a pass (so
/// none of the pass's own reconciliation — classify, rebind, sweep — runs).
fn publish_route_snapshot(router: &ToolRouter, uri: &str, server_id: &str) {
    router.replace_snapshot(RouterSnapshot {
        routes: HashMap::new(),
        routes_lower: HashMap::new(),
        tools_by_name: HashMap::new(),
        tools_by_name_lower: HashMap::new(),
        tools_all: Arc::new(Vec::new()),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_devin: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(Vec::new()),
        resource_templates_all: Arc::new(Vec::new()),
        prompts_all: Arc::new(Vec::new()),
        resource_routes: HashMap::from([(uri.to_string(), server_id.to_string())]),
        prompt_routes: HashMap::new(),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    });
}

fn publish_tool_route_snapshot(router: &ToolRouter, tool: &str, server_id: &str) {
    publish_tool_route_snapshot_with_original(router, tool, server_id, "tool");
}

fn publish_tool_route_snapshot_with_original(
    router: &ToolRouter,
    tool: &str,
    server_id: &str,
    original_name: &str,
) {
    router.replace_snapshot(RouterSnapshot {
        routes: HashMap::from([(
            tool.to_string(),
            (server_id.to_string(), original_name.to_string()),
        )]),
        tools_all: Arc::new(Vec::new()),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_devin: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
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
}

fn publish_tool_route_snapshot_with_metadata(
    router: &ToolRouter,
    tool: &str,
    server_id: &str,
    description: &'static str,
) {
    router.replace_snapshot(RouterSnapshot {
        routes: HashMap::from([(
            tool.to_string(),
            (server_id.to_string(), "tool".to_string()),
        )]),
        tools_all: Arc::new(vec![Tool::new(
            Cow::Owned(tool.to_string()),
            Cow::Borrowed(description),
            Arc::new(serde_json::Map::new()),
        )]),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_devin: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
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
}

#[tokio::test]
async fn native_continuation_rejects_route_replacement_before_second_invocation() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
    let state = SubscribableUpstreamState::new(&[]);
    let mut first = connect_subscribable_upstream("modern", Arc::clone(&state)).await;
    first.protocol_era = crate::protocol::ProtocolEra::Modern;
    sm.replace_server("modern", first).await;
    publish_tool_route_snapshot(&router, "Modern__tool", "modern");

    let principal = crate::types::PrincipalId::daemon_ipc(uuid::Uuid::from_u128(99));
    let mut params = CallToolRequestParams::new("Modern__tool");
    let input_requests = InputRequests::from_iter([(
        "answer".to_string(),
        InputRequest::Elicitation(ElicitRequest::new(
            ElicitRequestParams::UrlElicitationParams {
                meta: None,
                message: "Continue".to_string(),
                url: "https://example.test/continue".to_string(),
                elicitation_id: "answer".to_string(),
            },
        )),
    )]);
    let old = sm.get_upstream("modern").unwrap();
    let (route, _) = router.tool_route_identity("Modern__tool").unwrap();
    let binding = continuations::ContinuationBinding::new(
        &principal,
        continuations::canonical_tool_request_digest(&params),
        &format!("{}\0{}", route.server_id, route.original_name),
        route.route_generation,
        continuations::ContinuationKind::NativeToolRound,
    );
    let token = router
        .continuation_registry
        .insert(
            &principal,
            binding,
            128,
            NativeContinuation {
                upstream_request_state: Some("raw-upstream-state".to_string()),
                input_requests,
                route,
                upstream: Arc::clone(&old),
                round: 1,
                chain_expires_at: std::time::Instant::now() + MRTR_CHAIN_TTL,
            },
        )
        .unwrap();
    params.request_state = Some(token);
    params.input_responses = Some(InputResponses::from_iter([(
        "answer".to_string(),
        serde_json::json!({"action": "accept"}),
    )]));

    let mut replacement = connect_subscribable_upstream("modern", state).await;
    replacement.protocol_era = crate::protocol::ProtocolEra::Modern;
    sm.replace_server("modern", replacement).await;

    let context = DownstreamCallContext::ipc(
        "modern-client",
        RequestId::from(NumberOrString::Number(2)),
        ClientType::Unknown,
        None,
    )
    .with_local_principal(principal)
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::MODERN_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(true);
    let error = router
        .call_tool_round_with_context(params, None, Some(context))
        .await
        .expect_err("replacement route must invalidate the continuation");
    assert_eq!(error.code, ErrorCode(-32004));
    assert!(
        Arc::strong_count(&old) >= 2,
        "registry retains the exact old Arc"
    );
}

#[tokio::test]
async fn native_continuation_rejects_same_upstream_arc_tool_remap_before_invocation() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
    let state = SubscribableUpstreamState::new(&[]);
    let upstream = connect_modern_tool_upstream("modern", Arc::clone(&state)).await;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot_with_original(&router, "Modern__tool", "modern", "old_tool");

    let principal = crate::types::PrincipalId::daemon_ipc(uuid::Uuid::from_u128(100));
    let mut params = CallToolRequestParams::new("Modern__tool");
    let input_requests = InputRequests::from_iter([(
        "answer".to_string(),
        InputRequest::Elicitation(ElicitRequest::new(
            ElicitRequestParams::UrlElicitationParams {
                meta: None,
                message: "Continue".to_string(),
                url: "https://example.test/continue".to_string(),
                elicitation_id: "answer".to_string(),
            },
        )),
    )]);
    let upstream = sm.get_upstream("modern").unwrap();
    let (route, _) = router.tool_route_identity("Modern__tool").unwrap();
    let binding = continuations::ContinuationBinding::new(
        &principal,
        continuations::canonical_tool_request_digest(&params),
        &format!("{}\0{}", route.server_id, route.original_name),
        route.route_generation,
        continuations::ContinuationKind::NativeToolRound,
    );
    let token = router
        .continuation_registry
        .insert(
            &principal,
            binding,
            128,
            NativeContinuation {
                upstream_request_state: Some("raw-upstream-state".to_string()),
                input_requests,
                route,
                upstream,
                round: 1,
                chain_expires_at: std::time::Instant::now() + MRTR_CHAIN_TTL,
            },
        )
        .unwrap();
    params.request_state = Some(token);
    params.input_responses = Some(InputResponses::from_iter([(
        "answer".to_string(),
        serde_json::json!({"action": "accept"}),
    )]));

    // The server Arc is unchanged; only the exposed-name mapping moved.
    publish_tool_route_snapshot_with_original(&router, "Modern__tool", "modern", "new_tool");
    let context = DownstreamCallContext::ipc(
        "modern-client",
        RequestId::from(NumberOrString::Number(3)),
        ClientType::Unknown,
        None,
    )
    .with_local_principal(principal)
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::MODERN_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(true);
    let error = router
        .call_tool_round_with_context(params, None, Some(context))
        .await
        .expect_err("same-Arc tool remap must invalidate the continuation");
    assert_eq!(error.code, ErrorCode(-32004));
    assert_eq!(
        state.tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn full_continuation_count_and_byte_capacity_prevents_upstream_invocation() {
    let sm = Arc::new(ServerManager::new());
    let quotas = crate::protocol::AdmissionQuotas::new(crate::protocol::AdmissionQuotaConfig {
        continuations: 1,
        continuation_bytes: MRTR_MAX_BYTES,
        per_principal_divisor: 1,
        ..Default::default()
    });
    let router = Arc::new(ToolRouter::new_with_quotas(
        Arc::clone(&sm),
        RouterConfig::default(),
        quotas,
    ));
    let state = SubscribableUpstreamState::new(&[]);
    let upstream = connect_modern_tool_upstream("modern", Arc::clone(&state)).await;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot(&router, "Modern__tool", "modern");

    let principal = crate::types::PrincipalId::daemon_ipc(uuid::Uuid::from_u128(101));
    let _full = router
        .continuation_registry
        .reserve(&principal, MRTR_MAX_BYTES)
        .unwrap();
    let context = DownstreamCallContext::ipc(
        "modern-client",
        RequestId::from(NumberOrString::Number(4)),
        ClientType::Unknown,
        None,
    )
    .with_local_principal(principal)
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::MODERN_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(true);
    let error = router
        .call_tool_round_with_context(
            CallToolRequestParams::new("Modern__tool"),
            None,
            Some(context),
        )
        .await
        .expect_err("capacity must be reserved before the upstream call");
    assert_eq!(error.code, ErrorCode(-32007));
    assert_eq!(
        state.tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn default_oauth_grant_allows_plain_tool_call_to_modern_upstream() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
    let state = SubscribableUpstreamState::new(&[]);
    let upstream = connect_modern_tool_upstream("modern", Arc::clone(&state)).await;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot(&router, "Modern__tool", "modern");

    let principal =
        crate::types::PrincipalId::downstream_oauth("https://issuer.example", "client", "plug");
    let context = DownstreamCallContext::http(
        "modern-session",
        RequestId::from(NumberOrString::Number(9)),
        ClientType::Unknown,
    )
    .with_authorization(
        principal,
        crate::protocol::DEFAULT_DOWNSTREAM_OAUTH_SCOPES.map(str::to_string),
    )
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::MODERN_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(true);

    let response = router
        .call_tool_round_with_context(
            CallToolRequestParams::new("Modern__tool"),
            None,
            Some(context),
        )
        .await
        .expect("the full default grant must admit a plain modern tool call");

    assert!(matches!(response, CallToolResponse::Complete(_)));
    assert_eq!(
        state.tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(router.continuation_registry.len(), 0);
}

#[tokio::test]
async fn native_modern_upstream_completes_a_real_two_round_tool_call() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
    let state = SubscribableUpstreamState::new(&[]);
    state
        .tool_requires_input
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let upstream = connect_modern_tool_upstream("modern", Arc::clone(&state)).await;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot(&router, "Modern__tool", "modern");

    let principal = crate::types::PrincipalId::daemon_ipc(uuid::Uuid::from_u128(102));
    let context = DownstreamCallContext::ipc(
        "modern-client",
        RequestId::from(NumberOrString::Number(5)),
        ClientType::Unknown,
        None,
    )
    .with_local_principal(principal.clone())
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::MODERN_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(true);
    let first = router
        .call_tool_round_with_context(
            CallToolRequestParams::new("Modern__tool"),
            None,
            Some(context.clone()),
        )
        .await
        .unwrap();
    let CallToolResponse::InputRequired(first) = first else {
        panic!("first round must request input")
    };
    let plug_state = first.request_state.expect("Plug continuation state");
    assert_ne!(plug_state, "raw-upstream-state");

    // A catalog refresh that allocates a new snapshot but preserves the exact
    // tool route and upstream instance must not strand the parked round.
    publish_tool_route_snapshot_with_metadata(
        &router,
        "Modern__tool",
        "modern",
        "metadata-only refresh",
    );
    assert_eq!(router.continuation_registry.len(), 1);

    let mut second = CallToolRequestParams::new("Modern__tool");
    second.request_state = Some(plug_state);
    second.input_responses = Some(InputResponses::from_iter([(
        "answer".to_string(),
        serde_json::json!({"action": "accept"}),
    )]));
    let result = router
        .call_tool_round_with_context(second, None, Some(context))
        .await
        .unwrap();
    assert!(matches!(result, CallToolResponse::Complete(_)));
    assert_eq!(
        state.tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
}

#[tokio::test]
async fn continuation_keeps_max_byte_reservation_across_larger_second_input_required() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
    let state = SubscribableUpstreamState::new(&[]);
    state
        .tool_requires_input
        .store(true, std::sync::atomic::Ordering::SeqCst);
    state
        .tool_growing_input
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let upstream = connect_modern_tool_upstream("modern", Arc::clone(&state)).await;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot(&router, "Modern__tool", "modern");

    let principal = crate::types::PrincipalId::daemon_ipc(uuid::Uuid::from_u128(103));
    let context = DownstreamCallContext::ipc(
        "modern-client",
        RequestId::from(NumberOrString::Number(8)),
        ClientType::Unknown,
        None,
    )
    .with_local_principal(principal)
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::MODERN_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(true);

    let first = router
        .call_tool_round_with_context(
            CallToolRequestParams::new("Modern__tool"),
            None,
            Some(context.clone()),
        )
        .await
        .unwrap();
    let CallToolResponse::InputRequired(first) = first else {
        panic!("first round must request input")
    };
    let mut second = CallToolRequestParams::new("Modern__tool");
    second.request_state = first.request_state;
    second.input_responses = Some(InputResponses::from_iter([(
        "answer".to_string(),
        serde_json::json!({"action": "accept"}),
    )]));
    let second = router
        .call_tool_round_with_context(second, None, Some(context.clone()))
        .await
        .unwrap();
    let CallToolResponse::InputRequired(second) = second else {
        panic!("second round must return a larger input request set")
    };
    assert_eq!(second.input_requests.as_ref().unwrap().len(), 2);

    let mut third = CallToolRequestParams::new("Modern__tool");
    third.request_state = second.request_state;
    third.input_responses = Some(InputResponses::from_iter([
        (
            "answer".to_string(),
            serde_json::json!({"action": "accept"}),
        ),
        (
            "details".to_string(),
            serde_json::json!({"action": "accept"}),
        ),
    ]));
    assert!(matches!(
        router
            .call_tool_round_with_context(third, None, Some(context))
            .await
            .unwrap(),
        CallToolResponse::Complete(_)
    ));
    assert_eq!(
        state.tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        3
    );
}

#[tokio::test]
async fn oauth_revocation_while_upstream_response_is_paused_cannot_park_state() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
    let state = SubscribableUpstreamState::new(&[]);
    state
        .tool_requires_input
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let gate = SubGate::new_closed();
    *state.tool_gate.lock().unwrap() = Some(Arc::clone(&gate));
    let upstream = connect_modern_tool_upstream("modern", Arc::clone(&state)).await;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot(&router, "Modern__tool", "modern");

    let principal =
        crate::types::PrincipalId::downstream_oauth("https://issuer.example", "client", "plug");
    let lifecycle = crate::downstream_oauth::PrincipalLifecycleLease::active_for_tests();
    let context = DownstreamCallContext::http(
        "modern-session",
        RequestId::from(NumberOrString::Number(6)),
        ClientType::Unknown,
    )
    .with_authorization(
        principal.clone(),
        [
            "tools:read".to_string(),
            "continuations:complete".to_string(),
        ],
    )
    .with_principal_lifecycle(lifecycle.clone())
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::MODERN_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(true);
    let request_router = Arc::clone(&router);
    let request = tokio::spawn(async move {
        request_router
            .call_tool_round_with_context(
                CallToolRequestParams::new("Modern__tool"),
                None,
                Some(context),
            )
            .await
    });
    state.tool_entered.notified().await;
    lifecycle.deactivate_for_tests();
    router
        .cleanup_tasks_for_owner(&crate::tasks::TaskOwner::new(principal.owner_key()))
        .await;
    gate.open();

    let error = request
        .await
        .unwrap()
        .expect_err("revoked lifecycle must reject the paused response");
    assert_eq!(error.code, ErrorCode(-32001));
    assert_eq!(router.continuation_registry.len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_refresh_after_final_precheck_cannot_publish_parked_state() {
    let sm = Arc::new(ServerManager::new());
    let quotas = crate::protocol::AdmissionQuotas::new(crate::protocol::AdmissionQuotaConfig {
        continuations: 1,
        continuation_bytes: MRTR_MAX_BYTES,
        per_principal_divisor: 1,
        ..Default::default()
    });
    let router = Arc::new(ToolRouter::new_with_quotas(
        Arc::clone(&sm),
        RouterConfig::default(),
        quotas,
    ));
    let state = SubscribableUpstreamState::new(&[]);
    state
        .tool_requires_input
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let upstream = connect_modern_tool_upstream("modern", state).await;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot_with_original(&router, "Modern__tool", "modern", "old_tool");
    let gate = Arc::new(ContinuationInsertGate {
        entered: Arc::new(std::sync::Barrier::new(2)),
        release: Arc::new(std::sync::Barrier::new(2)),
    });
    router.set_continuation_insert_gate(Some(Arc::clone(&gate)));
    let principal = crate::types::PrincipalId::daemon_ipc(uuid::Uuid::from_u128(105));
    let context = DownstreamCallContext::ipc(
        "modern-client",
        RequestId::from(NumberOrString::Number(10)),
        ClientType::Unknown,
        None,
    )
    .with_local_principal(principal.clone())
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::MODERN_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(true);
    let request_router = Arc::clone(&router);
    let request = tokio::spawn(async move {
        request_router
            .call_tool_round_with_context(
                CallToolRequestParams::new("Modern__tool"),
                None,
                Some(context),
            )
            .await
    });
    gate.entered.wait();
    publish_tool_route_snapshot_with_original(&router, "Modern__tool", "modern", "new_tool");
    gate.release.wait();

    let error = request
        .await
        .unwrap()
        .expect_err("old-generation reservation must not publish after refresh");
    assert_eq!(error.code, ErrorCode(-32004));
    assert_eq!(router.continuation_registry.len(), 0);
    assert!(
        router
            .continuation_registry
            .reserve(&principal, MRTR_MAX_BYTES)
            .is_ok(),
        "failed publication must release both quota leases"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipc_principal_cleanup_after_final_precheck_cannot_publish_parked_state() {
    let sm = Arc::new(ServerManager::new());
    let quotas = crate::protocol::AdmissionQuotas::new(crate::protocol::AdmissionQuotaConfig {
        continuations: 1,
        continuation_bytes: MRTR_MAX_BYTES,
        per_principal_divisor: 1,
        ..Default::default()
    });
    let router = Arc::new(ToolRouter::new_with_quotas(
        Arc::clone(&sm),
        RouterConfig::default(),
        quotas,
    ));
    let state = SubscribableUpstreamState::new(&[]);
    state
        .tool_requires_input
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let upstream = connect_modern_tool_upstream("modern", state).await;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot(&router, "Modern__tool", "modern");
    let gate = Arc::new(ContinuationInsertGate {
        entered: Arc::new(std::sync::Barrier::new(2)),
        release: Arc::new(std::sync::Barrier::new(2)),
    });
    router.set_continuation_insert_gate(Some(Arc::clone(&gate)));
    let principal = crate::downstream_identity::ipc_registry_principal("ipc-client-race");
    let context = DownstreamCallContext::ipc(
        "modern-client",
        RequestId::from(NumberOrString::Number(11)),
        ClientType::Unknown,
        None,
    )
    .with_local_principal(principal.clone())
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::MODERN_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(true);
    let request_router = Arc::clone(&router);
    let request = tokio::spawn(async move {
        request_router
            .call_tool_round_with_context(
                CallToolRequestParams::new("Modern__tool"),
                None,
                Some(context),
            )
            .await
    });
    gate.entered.wait();
    router.revoke_continuations_for_principal(&principal);
    gate.release.wait();

    let error = request
        .await
        .unwrap()
        .expect_err("revoked owner generation must not publish after IPC cleanup");
    assert_eq!(error.code, ErrorCode(-32004));
    assert_eq!(router.continuation_registry.len(), 0);
    assert!(
        router
            .continuation_registry
            .reserve(&principal, MRTR_MAX_BYTES)
            .is_ok(),
        "failed publication must release both quota leases"
    );
}

#[tokio::test]
async fn legacy_task_to_modern_upstream_is_rejected_before_task_or_tool_effect() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));
    let state = SubscribableUpstreamState::new(&[]);
    let mut upstream = connect_subscribable_upstream("modern", Arc::clone(&state)).await;
    upstream.protocol_era = crate::protocol::ProtocolEra::Modern;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot(&router, "Modern__tool", "modern");
    let downstream = DownstreamCallContext::ipc(
        "test-client",
        RequestId::Number(1),
        ClientType::Unknown,
        None,
    );

    let error = router
        .ensure_tool_round_supported("Modern__tool", &downstream, false, true)
        .expect_err("legacy task cannot target a modern upstream");
    assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);
    assert_eq!(
        state.tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn invoke_meta_tool_applies_pair_suppression_to_resolved_target() {
    let sm = Arc::new(ServerManager::new());
    let config = RouterConfig {
        meta_tool_mode: true,
        ..Default::default()
    };
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), config));
    let state = SubscribableUpstreamState::new(&[]);
    let mut upstream = connect_subscribable_upstream("modern", Arc::clone(&state)).await;
    upstream.protocol_era = crate::protocol::ProtocolEra::Modern;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot(&router, "Modern__tool", "modern");
    let context = DownstreamCallContext::ipc(
        "legacy-client",
        RequestId::from(NumberOrString::Number(7)),
        ClientType::Unknown,
        None,
    );
    let arguments = serde_json::Map::from_iter([
        ("tool_name".to_string(), serde_json::json!("Modern__tool")),
        ("arguments".to_string(), serde_json::json!({})),
    ]);

    let error = router
        .call_tool_with_context("plug__invoke_tool", Some(arguments), None, Some(context))
        .await
        .expect_err("meta indirection must not bypass protocol-pair policy");
    assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);
    assert_eq!(
        state.tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn invoke_meta_tool_suppresses_modern_to_modern_input_required_before_invocation() {
    let sm = Arc::new(ServerManager::new());
    let config = RouterConfig {
        meta_tool_mode: true,
        ..Default::default()
    };
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), config));
    let state = SubscribableUpstreamState::new(&[]);
    state
        .tool_requires_input
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let upstream = connect_modern_tool_upstream("modern", Arc::clone(&state)).await;
    sm.replace_server("modern", upstream).await;
    publish_tool_route_snapshot(&router, "Modern__tool", "modern");
    let context = DownstreamCallContext::ipc(
        "modern-client",
        RequestId::from(NumberOrString::Number(9)),
        ClientType::Unknown,
        None,
    )
    .with_local_principal(crate::types::PrincipalId::daemon_ipc(
        uuid::Uuid::from_u128(104),
    ))
    .with_protocol(
        crate::protocol::ProtocolEra::Modern,
        crate::protocol::MODERN_PROTOCOL_VERSION,
    )
    .with_modern_direction_enabled(true);
    let arguments = serde_json::Map::from_iter([
        ("tool_name".to_string(), serde_json::json!("Modern__tool")),
        ("arguments".to_string(), serde_json::json!({})),
    ]);

    let error = router
        .call_tool_with_context("plug__invoke_tool", Some(arguments), None, Some(context))
        .await
        .expect_err("meta-tool cannot represent a modern InputRequired response");
    assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);
    assert_eq!(
        state.tool_calls.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

/// Race manifestation 1 (rebind side): a last-member unsubscribe lands
/// while a `refresh_tools` pass that will rebind the URI is still inside
/// its (pre-publish) prune phase, so the rebind reaches a zero-member
/// entry whose drain is still in flight. Without the empty-entry guard,
/// rebind revived the entry onto the new owner: a zero-member Active entry
/// holding a live new-owner subscription nothing would ever drain.
#[tokio::test]
async fn refresh_rebind_with_racing_last_unsubscribe_leaves_no_orphan() {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));

    let old_state = SubscribableUpstreamState::new(&["memory://x", "memory://y"]);
    let new_state = SubscribableUpstreamState::new(&[]);
    sm.replace_server(
        "old",
        connect_subscribable_upstream("old", Arc::clone(&old_state)).await,
    )
    .await;
    sm.replace_server(
        "new",
        connect_subscribable_upstream("new", Arc::clone(&new_state)).await,
    )
    .await;

    router.refresh_tools().await;
    assert_eq!(
        router.cache.load().resource_routes.get("memory://x"),
        Some(&"old".to_string())
    );

    router
        .subscribe_resource("memory://x", sub_target("a"), None)
        .await
        .unwrap();
    router
        .subscribe_resource("memory://y", sub_target("c"), None)
        .await
        .unwrap();
    assert_eq!(old_state.subscribe_count("memory://x"), 1);

    // Flip the routes: x migrates old -> new (Rebind), y vanishes (Prune).
    old_state.set_resources(&[]);
    new_state.set_resources(&["memory://x"]);
    let gate_y = old_state.close_unsubscribe_gate("memory://y");
    let gate_x = old_state.close_unsubscribe_gate("memory://x");

    // Start the refresh; its prune(y) drain parks inside the old server's
    // unsubscribe handler, holding the pass in its pre-publish window.
    let entered_y = old_state.unsubscribe_entered.notified();
    let refresh_router = Arc::clone(&router);
    let refresh = tokio::spawn(async move { refresh_router.refresh_tools().await });
    entered_y.await;

    // The last member of x unsubscribes inside the window. The entry
    // empties and its drain is gated; when the refresh proceeds to
    // rebind(x), the entry is still present with zero members.
    let entered_x = old_state.unsubscribe_entered.notified();
    router
        .unsubscribe_resource("memory://x", &sub_target("a"))
        .await
        .unwrap();

    gate_y.open();
    // Whichever drain generation performs the upstream call (the member's
    // gated drain, or the guard's drain queued behind it) enters here.
    entered_x.await;
    gate_x.open();
    refresh.await.unwrap();

    // Let any superseded drain finish its no-op before asserting.
    for _ in 0..50 {
        if router.resource_subscriptions.is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert!(
        router
            .resource_subscriptions
            .members_snapshot("memory://x")
            .is_none(),
        "no zero-member entry may survive the rebind"
    );
    assert_eq!(router.resource_subscriptions.len(), 0);
    assert_eq!(
        new_state.subscribe_count("memory://x"),
        0,
        "the new owner must never be subscribed for an emptied entry"
    );
    assert_eq!(new_state.unsubscribe_count("memory://x"), 0);
    assert_eq!(old_state.subscribe_count("memory://x"), 1);
    assert!(
        old_state.unsubscribe_count("memory://x") >= 1,
        "the old owner must end unsubscribed"
    );
    assert_eq!(old_state.unsubscribe_count("memory://y"), 1);
}

/// Shared setup for the prune-side race (manifestation 3): a downstream
/// subscribe lands while a `refresh_tools` pass is draining the URI's
/// prune (pre-publish), resolves the OLD route, and supersedes the drain;
/// the published snapshot then has no route for the URI. Returns with the
/// racing subscriber tracked on the old owner and the route gone.
async fn setup_subscribe_racing_prune() -> (
    Arc<ServerManager>,
    Arc<ToolRouter>,
    Arc<SubscribableUpstreamState>,
) {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));

    let old_state = SubscribableUpstreamState::new(&["memory://x"]);
    sm.replace_server(
        "old",
        connect_subscribable_upstream("old", Arc::clone(&old_state)).await,
    )
    .await;

    router.refresh_tools().await;
    router
        .subscribe_resource("memory://x", sub_target("a"), None)
        .await
        .unwrap();

    // The route vanishes; the refresh's prune drain parks inside the old
    // server's unsubscribe handler, pre-publish.
    old_state.set_resources(&[]);
    let gate_x = old_state.close_unsubscribe_gate("memory://x");
    let entered_x = old_state.unsubscribe_entered.notified();
    let refresh_router = Arc::clone(&router);
    let refresh = tokio::spawn(async move { refresh_router.refresh_tools().await });
    entered_x.await;

    // B subscribes in the window: the wrapper resolves the still-published
    // OLD snapshot, and the registry replaces the Draining entry with a
    // fresh generation queued behind the parked drain.
    let subscribe_router = Arc::clone(&router);
    let b = tokio::spawn(async move {
        subscribe_router
            .subscribe_resource("memory://x", sub_target("b"), None)
            .await
    });
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert!(!b.is_finished(), "B must queue behind the in-flight drain");

    gate_x.open();
    refresh.await.unwrap();
    assert!(b.await.unwrap().is_ok(), "racing subscriber must get Ok");

    // The racing subscriber is correctly tracked on the old owner while the
    // published snapshot has no route for the URI.
    assert!(
        !router
            .cache
            .load()
            .resource_routes
            .contains_key("memory://x")
    );
    assert_eq!(
        router
            .resource_subscriptions
            .members_snapshot("memory://x")
            .unwrap(),
        HashSet::from([sub_target("b")])
    );
    assert_eq!(old_state.subscribe_count("memory://x"), 2);
    assert_eq!(old_state.unsubscribe_count("memory://x"), 1);

    (sm, router, old_state)
}

/// Prune-side race, downstream-unsubscribe ending: B's later unsubscribe
/// used to fail "resource not found" (route gone) without touching the
/// registry. It must now drain via the recorded owner.
#[tokio::test]
async fn racing_subscriber_unsubscribes_cleanly_after_route_vanishes() {
    let (_sm, router, old_state) = setup_subscribe_racing_prune().await;

    router
        .unsubscribe_resource("memory://x", &sub_target("b"))
        .await
        .expect("unsubscribe of a tracked routeless entry must succeed");

    for _ in 0..50 {
        if router.resource_subscriptions.is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(router.resource_subscriptions.len(), 0);
    assert_eq!(
        old_state.unsubscribe_count("memory://x"),
        2,
        "the drain must unsubscribe the recorded old owner"
    );

    // With neither an entry nor a route left, the historical error stands.
    let err = router
        .unsubscribe_resource("memory://x", &sub_target("b"))
        .await
        .expect_err("no entry and no route must still be an error");
    assert!(err.message.contains("resource not found"));
}

/// Prune-side race, next-refresh ending: the following refresh pass
/// classifies the surviving routeless entry as a prune with no old server
/// id (upstream fallback `None`). It used to remove the entry with no
/// upstream unsubscribe ever sent; the recorded owner must drain it.
#[tokio::test]
async fn next_refresh_prunes_routeless_entry_via_recorded_owner() {
    let (_sm, router, old_state) = setup_subscribe_racing_prune().await;

    router.refresh_tools().await;

    assert_eq!(router.resource_subscriptions.len(), 0);
    assert_eq!(
        old_state.unsubscribe_count("memory://x"),
        2,
        "the prune-with-no-route drain must unsubscribe the recorded old owner"
    );
}

/// Shared setup for the first-subscribe-during-refresh skew: routes are
/// flipping x old -> new in a refresh whose prune of y is parked on a gate
/// (holding the pass pre-publish). Returns with the refresh spawned and
/// parked inside the old server's `unsubscribe(y)` handler, after classify
/// ran — so an entry created for x from here on is invisible to the pass.
async fn setup_refresh_parked_pre_publish() -> (
    Arc<ServerManager>,
    Arc<ToolRouter>,
    Arc<SubscribableUpstreamState>,
    Arc<SubscribableUpstreamState>,
    Arc<SubGate>,
    tokio::task::JoinHandle<()>,
) {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));

    let old_state = SubscribableUpstreamState::new(&["memory://x", "memory://y"]);
    let new_state = SubscribableUpstreamState::new(&[]);
    sm.replace_server(
        "old",
        connect_subscribable_upstream("old", Arc::clone(&old_state)).await,
    )
    .await;
    sm.replace_server(
        "new",
        connect_subscribable_upstream("new", Arc::clone(&new_state)).await,
    )
    .await;

    router.refresh_tools().await;
    assert_eq!(
        router.cache.load().resource_routes.get("memory://x"),
        Some(&"old".to_string())
    );

    // Give the refresh a second URI to prune so the pass can be parked
    // pre-publish. No subscription for x exists yet.
    router
        .subscribe_resource("memory://y", sub_target("c"), None)
        .await
        .unwrap();

    // Flip the routes: x migrates old -> new, y vanishes (Prune).
    old_state.set_resources(&[]);
    new_state.set_resources(&["memory://x"]);
    let gate_y = old_state.close_unsubscribe_gate("memory://y");

    // Start the refresh; classify runs (seeing only y), then the prune(y)
    // drain parks inside the old server's unsubscribe handler.
    let entered_y = old_state.unsubscribe_entered.notified();
    let refresh_router = Arc::clone(&router);
    let refresh = tokio::spawn(async move { refresh_router.refresh_tools().await });
    entered_y.await;

    (sm, router, old_state, new_state, gate_y, refresh)
}

/// Defect arm 1 (classify owner/route drift): a FIRST subscriber for x
/// lands entirely inside the parked pass's pre-publish window, resolves
/// the OLD snapshot, and binds to the old owner. The pass classified
/// before the entry existed, so pure old-vs-new route diffing would
/// compare new==new forever after — and with refreshes being purely
/// event-driven, "the next refresh heals it" is a guarantee that may
/// never fire. The TRIGGERING pass's own post-publish sweep must heal the
/// skew before `refresh_tools` returns: rebound onto "new", old owner
/// unsubscribed, members (and their notification fan-out) intact, no
/// zero-member entry left anywhere. No second refresh is invoked.
#[tokio::test]
async fn first_subscribe_during_refresh_window_heals_in_same_refresh() {
    let (_sm, router, old_state, new_state, gate_y, refresh) =
        setup_refresh_parked_pre_publish().await;

    // First subscriber for x arrives inside the window and completes fully
    // pre-publish: it binds to the old owner (the still-published route).
    router
        .subscribe_resource("memory://x", sub_target("a"), None)
        .await
        .unwrap();
    assert_eq!(old_state.subscribe_count("memory://x"), 1);
    assert_eq!(new_state.subscribe_count("memory://x"), 0);

    // Drain the entry's post-confirm hook task against the still-published
    // OLD snapshot (owner == route, so it no-ops) before releasing the
    // pass, so the heal below is attributable to the pass's own sweep.
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }

    gate_y.open();
    refresh.await.unwrap();

    // The pass classified before the entry existed, but its post-publish
    // sweep re-classified the recorded owner ("old") against the published
    // snapshot ("new") and healed the skew before refresh_tools returned.
    assert_eq!(
        router.cache.load().resource_routes.get("memory://x"),
        Some(&"new".to_string())
    );
    assert_eq!(
        new_state.subscribe_count("memory://x"),
        1,
        "the entry must be rebound onto the new owner"
    );
    assert_eq!(
        old_state.unsubscribe_count("memory://x"),
        1,
        "the old owner must end unsubscribed"
    );
    assert_eq!(old_state.subscribe_count("memory://x"), 1);
    assert_eq!(
        router
            .resource_subscriptions
            .members_snapshot("memory://x")
            .unwrap(),
        HashSet::from([sub_target("a")]),
        "members must survive the heal"
    );
    assert_eq!(
        router.resource_subscriptions.len(),
        1,
        "no zero-member entry may survive (y pruned, x healed)"
    );

    // Notification-relevant invariant: an upstream resources/updated for x
    // still fans out to the surviving member.
    let mut rx = router.subscribe_notifications();
    router.route_upstream_resource_updated(ResourceUpdatedNotificationParam::new("memory://x"));
    match rx.try_recv() {
        Ok(ProtocolNotification::ResourceUpdated { target, params }) => {
            assert_eq!(target, sub_target("a"));
            assert_eq!(params.uri, "memory://x");
        }
        other => panic!("expected ResourceUpdated for the healed member, got: {other:?}"),
    }
}

/// Defect arm 2 (post-subscribe self-check): the first subscriber for x
/// resolves the OLD snapshot pre-publish, but its upstream subscribe call
/// is still parked on the old server when the refresh publishes the new
/// snapshot (x -> "new"). When the call lands, the entry's owner is
/// confirmed as "old" while the current route says "new" — the running
/// pass never saw the entry, so nothing else reconciles it in this test.
/// `subscribe_resource`'s one-shot self-check must re-load the snapshot
/// and migrate the subscription to "new" before returning.
#[tokio::test]
async fn first_subscribe_straddling_publish_rebinds_via_self_check() {
    let (_sm, router, old_state, new_state, gate_y, refresh) =
        setup_refresh_parked_pre_publish().await;

    // The first subscriber's upstream subscribe(x) parks on the old server.
    let gate_sub_x = old_state.close_subscribe_gate("memory://x");
    let entered_sub_x = old_state.subscribe_entered.notified();
    let subscribe_router = Arc::clone(&router);
    let a = tokio::spawn(async move {
        subscribe_router
            .subscribe_resource("memory://x", sub_target("a"), None)
            .await
    });
    entered_sub_x.await;
    assert_eq!(old_state.subscribe_count("memory://x"), 1);

    // Publish happens while the subscribe is still in flight.
    gate_y.open();
    refresh.await.unwrap();
    assert_eq!(
        router.cache.load().resource_routes.get("memory://x"),
        Some(&"new".to_string())
    );
    assert!(!a.is_finished(), "subscribe must still be parked upstream");

    // The subscribe lands on "old" post-publish; the self-check must see
    // the moved route and rebind to "new" before returning Ok.
    gate_sub_x.open();
    a.await.unwrap().unwrap();

    assert_eq!(
        new_state.subscribe_count("memory://x"),
        1,
        "the self-check must resubscribe on the current owner"
    );
    assert_eq!(
        old_state.unsubscribe_count("memory://x"),
        1,
        "the old owner must end unsubscribed"
    );
    assert_eq!(old_state.subscribe_count("memory://x"), 1);
    assert_eq!(
        router
            .resource_subscriptions
            .members_snapshot("memory://x")
            .unwrap(),
        HashSet::from([sub_target("a")]),
        "members must survive the self-check rebind"
    );
    assert_eq!(router.resource_subscriptions.len(), 1);
    assert_eq!(new_state.unsubscribe_count("memory://x"), 0);
}

/// Routeless-entry-gains-route-elsewhere variant: the racing-subscriber
/// setup leaves an entry recorded on "old" with no route at all. When a
/// later refresh routes the URI to a DIFFERENT server ("c"), classify's
/// old None arm used to do nothing (it only pruned when the new snapshot
/// also lacked the URI) — owner-vs-route mismatch unreconciled forever.
/// The recorded owner must now drive a rebind onto "c".
#[tokio::test]
async fn routeless_entry_gaining_route_elsewhere_is_rebound() {
    let (sm, router, old_state) = setup_subscribe_racing_prune().await;

    let c_state = SubscribableUpstreamState::new(&["memory://x"]);
    sm.replace_server(
        "c",
        connect_subscribable_upstream("c", Arc::clone(&c_state)).await,
    )
    .await;

    router.refresh_tools().await;

    assert_eq!(
        router.cache.load().resource_routes.get("memory://x"),
        Some(&"c".to_string())
    );
    assert_eq!(
        c_state.subscribe_count("memory://x"),
        1,
        "the routeless entry must be rebound onto the server now owning the route"
    );
    assert_eq!(
        old_state.unsubscribe_count("memory://x"),
        2,
        "the recorded old owner must be unsubscribed by the rebind"
    );
    assert_eq!(
        router
            .resource_subscriptions
            .members_snapshot("memory://x")
            .unwrap(),
        HashSet::from([sub_target("b")]),
        "members must survive the rebind"
    );
    assert_eq!(router.resource_subscriptions.len(), 1);
}

/// Shared setup for the post-confirm-hook and self-check heal tests: two
/// subscribable upstreams, routes published with x -> "old".
async fn setup_two_owner_router() -> (
    Arc<ServerManager>,
    Arc<ToolRouter>,
    Arc<SubscribableUpstreamState>,
    Arc<SubscribableUpstreamState>,
) {
    let sm = Arc::new(ServerManager::new());
    let router = Arc::new(ToolRouter::new(Arc::clone(&sm), RouterConfig::default()));

    let old_state = SubscribableUpstreamState::new(&["memory://x"]);
    let new_state = SubscribableUpstreamState::new(&[]);
    sm.replace_server(
        "old",
        connect_subscribable_upstream("old", Arc::clone(&old_state)).await,
    )
    .await;
    sm.replace_server(
        "new",
        connect_subscribable_upstream("new", Arc::clone(&new_state)).await,
    )
    .await;

    router.refresh_tools().await;
    assert_eq!(
        router.cache.load().resource_routes.get("memory://x"),
        Some(&"old".to_string())
    );

    (sm, router, old_state, new_state)
}

/// Defect arm (caller cancellation): the heal must not ride the caller's
/// cancellable request future. A subscribe whose upstream confirmation is
/// still parked when a refresh publish moves the URI — and whose
/// downstream caller is then aborted — has no self-check left to run. The
/// registry's post-confirm hook fires from the detached transition task
/// and must heal the skew anyway: new owner subscribed, old owner
/// unsubscribed, recorded owner updated to the published route.
#[tokio::test]
async fn caller_cancelled_subscribe_still_heals_via_post_confirm_hook() {
    let (_sm, router, old_state, new_state) = setup_two_owner_router().await;

    // The subscriber's upstream subscribe(x) parks on the old server.
    let gate_sub_x = old_state.close_subscribe_gate("memory://x");
    let entered_sub_x = old_state.subscribe_entered.notified();
    let subscribe_router = Arc::clone(&router);
    let a = tokio::spawn(async move {
        subscribe_router
            .subscribe_resource("memory://x", sub_target("a"), None)
            .await
    });
    entered_sub_x.await;
    assert_eq!(old_state.subscribe_count("memory://x"), 1);

    // A refresh publish lands mid-subscribe: x now routes to "new".
    publish_route_snapshot(&router, "memory://x", "new");

    // The downstream caller disconnects; its request future is dropped
    // before the transition confirms, so no post-subscribe self-check will
    // ever run for this call.
    a.abort();
    let _ = a.await;

    // The detached transition confirms against the stale owner. The
    // post-confirm hook must spawn the heal with no caller left alive.
    gate_sub_x.open();

    for _ in 0..500 {
        if new_state.subscribe_count("memory://x") == 1
            && old_state.unsubscribe_count("memory://x") == 1
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        new_state.subscribe_count("memory://x"),
        1,
        "the hook must resubscribe on the published owner"
    );
    assert_eq!(
        old_state.unsubscribe_count("memory://x"),
        1,
        "the stale owner must end unsubscribed"
    );
    assert_eq!(old_state.subscribe_count("memory://x"), 1);
    assert_eq!(new_state.unsubscribe_count("memory://x"), 0);
    assert_eq!(
        router.resource_subscriptions.owner_snapshot("memory://x"),
        Some(Some("new".to_string())),
        "recorded owner must be the published route"
    );
    assert_eq!(
        router
            .resource_subscriptions
            .members_snapshot("memory://x")
            .unwrap(),
        HashSet::from([sub_target("a")]),
        "the (disconnected but never unsubscribed) member survives the heal"
    );
}

/// Defect arm (retry doesn't heal): a retry/second subscriber against an
/// already-skewed entry resolves the NEW route, so a self-check comparing
/// the caller-resolved server against the published route sees nothing
/// wrong. The self-check must compare the entry's RECORDED owner instead
/// and heal the skew before returning Ok.
#[tokio::test]
async fn retry_subscribe_heals_stale_recorded_owner() {
    let (_sm, router, old_state, new_state) = setup_two_owner_router().await;

    router
        .subscribe_resource("memory://x", sub_target("a"), None)
        .await
        .unwrap();
    assert_eq!(old_state.subscribe_count("memory://x"), 1);
    assert_eq!(
        router.resource_subscriptions.owner_snapshot("memory://x"),
        Some(Some("old".to_string()))
    );

    // A refresh publish moved x to "new" while nothing reconciled the
    // entry (simulated skew: recorded owner "old", published route "new").
    publish_route_snapshot(&router, "memory://x", "new");

    // A second subscriber piggybacks on the Active entry (no upstream
    // transition, so no post-confirm hook fires). Its self-check must
    // detect owner != route via the RECORDED owner and heal before Ok.
    router
        .subscribe_resource("memory://x", sub_target("b"), None)
        .await
        .expect("retry against a skewed entry must succeed after healing");

    assert_eq!(
        new_state.subscribe_count("memory://x"),
        1,
        "the heal must resubscribe on the published owner"
    );
    assert_eq!(
        old_state.unsubscribe_count("memory://x"),
        1,
        "the stale owner must end unsubscribed"
    );
    assert_eq!(old_state.subscribe_count("memory://x"), 1);
    assert_eq!(
        router.resource_subscriptions.owner_snapshot("memory://x"),
        Some(Some("new".to_string()))
    );
    assert_eq!(
        router
            .resource_subscriptions
            .members_snapshot("memory://x")
            .unwrap(),
        HashSet::from([sub_target("a"), sub_target("b")]),
        "both members must survive the heal"
    );
}

/// Defect arm (silent success on failed migration): the self-check heal
/// unsubscribes the stale owner and, when the new owner's subscribe fails,
/// the transition prunes the entry — the old contract returned Ok with no
/// live subscription anywhere. The caller must now get an error, and a
/// fresh subscribe afterwards must succeed cleanly against the new owner.
#[tokio::test]
async fn failed_heal_migration_propagates_error_to_subscriber() {
    let (_sm, router, old_state, new_state) = setup_two_owner_router().await;

    router
        .subscribe_resource("memory://x", sub_target("a"), None)
        .await
        .unwrap();

    // Skew the entry (recorded owner "old", published route "new") and
    // make the new owner reject the migration's subscribe.
    publish_route_snapshot(&router, "memory://x", "new");
    new_state.fail_subscribe("memory://x");

    let err = router
        .subscribe_resource("memory://x", sub_target("b"), None)
        .await
        .expect_err("a failed migration must not be reported as success");
    assert!(
        err.message.contains("retry subscribe"),
        "error must tell the client to retry, got: {}",
        err.message
    );
    assert_eq!(old_state.unsubscribe_count("memory://x"), 1);
    assert_eq!(
        new_state.subscribe_count("memory://x"),
        1,
        "the failed migration attempt was made"
    );
    assert!(
        !router.resource_subscriptions.has_entry("memory://x"),
        "the failed migration must prune the entry"
    );

    // A retry after the failure heals cleanly against the current owner.
    new_state.clear_subscribe_failure("memory://x");
    router
        .subscribe_resource("memory://x", sub_target("b"), None)
        .await
        .expect("a fresh subscribe after the failed migration must succeed");
    assert_eq!(new_state.subscribe_count("memory://x"), 2);
    assert_eq!(
        router.resource_subscriptions.owner_snapshot("memory://x"),
        Some(Some("new".to_string()))
    );
    assert_eq!(
        router
            .resource_subscriptions
            .members_snapshot("memory://x")
            .unwrap(),
        HashSet::from([sub_target("b")])
    );
}

#[test]
fn dropping_router_releases_subscription_registry() {
    let server_manager = Arc::new(ServerManager::new());
    let router = ToolRouter::new(server_manager, RouterConfig::default());
    let registry = Arc::downgrade(&router.resource_subscriptions);

    drop(router);

    assert!(
        registry.upgrade().is_none(),
        "the registry-owned post-confirm hook must not retain the registry itself"
    );
}

// ── Per-client blocks ───────────────────────────────────────────────────────

fn plain_tool(name: &'static str) -> Tool {
    Tool::new(
        Cow::Borrowed(name),
        Cow::Borrowed("fixture tool"),
        Arc::new(serde_json::Map::new()),
    )
}

/// Three tools on two servers. `cursor` is kept from the whole `git` server;
/// `pi` is kept from every Slack tool by pattern.
fn router_with_client_blocks(config: RouterConfig) -> ToolRouter {
    let router = ToolRouter::new(Arc::new(ServerManager::new()), config);
    store_plain_tools_snapshot(
        &router,
        vec![
            plain_tool("git__commit"),
            plain_tool("git__push"),
            plain_tool("slack__post"),
        ],
    );
    let mut clients = std::collections::BTreeMap::new();
    clients.insert(
        "cursor".to_string(),
        crate::config::ClientSettings {
            blocked_servers: vec!["git".to_string()],
            ..Default::default()
        },
    );
    clients.insert(
        "pi".to_string(),
        crate::config::ClientSettings {
            blocked_tools: vec!["Slack__*".into()],
            ..Default::default()
        },
    );
    router.set_client_access(&clients);
    router
}

fn context_for(client_key: Option<&str>) -> DownstreamCallContext {
    DownstreamCallContext::ipc("client-a", RequestId::Number(1), ClientType::Unknown, None)
        .with_client_key(client_key)
}

fn tool_names(tools: &[Tool]) -> Vec<String> {
    let mut names: Vec<String> = tools.iter().map(|tool| tool.name.to_string()).collect();
    names.sort();
    names
}

fn result_text(result: &CallToolResult) -> String {
    serde_json::to_string(result).expect("serialize tool result")
}

#[test]
fn a_client_with_an_allow_list_gets_only_what_is_on_it() {
    let config = RouterConfig {
        tool_filter_enabled: false,
        ..Default::default()
    };
    let router = router_with_client_blocks(config);
    let allowed_tool = |server: &str, tool: &str| crate::config::AllowedTool {
        server: server.to_string(),
        tool: tool.to_string(),
    };
    let mut clients = std::collections::BTreeMap::new();
    clients.insert(
        "radar".to_string(),
        crate::config::ClientSettings {
            allowed_servers: vec!["slack".to_string()],
            ..Default::default()
        },
    );
    clients.insert(
        "bot".to_string(),
        crate::config::ClientSettings {
            allowed_tools: vec![allowed_tool("git", "commit")],
            ..Default::default()
        },
    );
    // A block takes away what the list lets in.
    clients.insert(
        "both".to_string(),
        crate::config::ClientSettings {
            allowed_servers: vec!["git".to_string()],
            blocked_tools: vec!["git__push".into()],
            ..Default::default()
        },
    );
    router.set_client_access(&clients);
    let listed = |client_key: &str| {
        tool_names(&router.list_tools_for_client_session(
            ClientType::ClaudeCode,
            None,
            Some(client_key),
        ))
    };

    assert_eq!(listed("radar"), ["slack__post"]);
    assert_eq!(listed("bot"), ["git__commit"]);
    assert_eq!(listed("both"), ["git__commit"]);
    assert_eq!(listed("other"), ["git__commit", "git__push", "slack__post"]);

    assert!(router.client_may_use_server(Some("radar"), "slack"));
    assert!(!router.client_may_use_server(Some("radar"), "git"));
    // A server added later is not on the list.
    assert!(!router.client_may_use_server(Some("radar"), "notion"));
    assert!(router.client_is_kept_from_a_server(Some("radar")));

    // Single tools of a server are those tools, not its resources or
    // prompts.
    assert!(router.client_may_use_server(Some("bot"), "git"));
    assert!(!router.client_may_use_all_of_server(Some("bot"), "git"));
    assert!(router.client_may_use_all_of_server(Some("radar"), "slack"));
    assert!(router.client_may_use_all_of_server(Some("other"), "git"));

    // A result is read by the same rule, and one whose tool was not
    // recorded is not on a list of single tools.
    assert!(router.client_may_read_result_of(Some("bot"), Some("git"), Some("commit"), "x"));
    assert!(!router.client_may_read_result_of(Some("bot"), Some("git"), Some("push"), "x"));
    assert!(!router.client_may_read_result_of(Some("bot"), Some("git"), None, "x"));
    assert!(!router.client_may_read_result_of(Some("bot"), None, None, "x"));
    assert!(router.client_may_read_result_of(Some("radar"), Some("slack"), None, "x"));
}

#[test]
fn a_blocked_client_is_not_listed_what_it_is_kept_from() {
    let config = RouterConfig {
        tool_filter_enabled: false,
        ..Default::default()
    };
    let router = router_with_client_blocks(config);
    let listed = |client_key: Option<&str>| {
        tool_names(&router.list_tools_for_client_session(ClientType::ClaudeCode, None, client_key))
    };

    assert_eq!(listed(Some("cursor")), ["slack__post"]);
    assert_eq!(listed(Some("pi")), ["git__commit", "git__push"]);
    // No entry, and no key at all, both get everything.
    for unblocked in [Some("claude-code"), None] {
        assert_eq!(
            listed(unblocked),
            ["git__commit", "git__push", "slack__post"]
        );
    }

    let page = router.list_tools_page_for_client_session(
        ClientType::ClaudeCode,
        None,
        Some("cursor"),
        None,
    );
    assert_eq!(tool_names(&page.tools), ["slack__post"]);

    assert!(
        router
            .get_tool_definition("git__commit", Some("cursor"))
            .is_none()
    );
    assert!(
        router
            .get_tool_definition("git__commit", Some("pi"))
            .is_some()
    );
}

#[tokio::test]
async fn a_blocked_tool_answers_a_call_as_a_tool_that_does_not_exist() {
    let config = RouterConfig {
        tool_filter_enabled: false,
        ..Default::default()
    };
    let router = Arc::new(router_with_client_blocks(config));
    let call = |tool: &'static str, client_key: Option<&'static str>| {
        let router = Arc::clone(&router);
        async move {
            router
                .call_tool_with_context(tool, None, None, Some(context_for(client_key)))
                .await
                .expect_err("no upstream is connected, so every call fails")
                .to_string()
        }
    };

    let missing = call("git__nope", Some("cursor")).await;
    // The casing a model drifts into resolves to the same route, so it is
    // blocked the same way.
    for blocked in ["git__commit", "GIT__Commit"] {
        assert_eq!(
            call(blocked, Some("cursor")).await,
            missing.replace("git__nope", blocked),
            "a blocked call must be indistinguishable from an unknown tool"
        );
    }
    assert_eq!(
        call("slack__post", Some("pi")).await,
        missing.replace("git__nope", "slack__post")
    );

    // The same tools get past the block for a client that is not kept from
    // them: the call fails further on, for want of an upstream.
    assert_ne!(
        call("git__commit", Some("pi")).await,
        missing.replace("git__nope", "git__commit")
    );
    assert_ne!(
        call("slack__post", Some("cursor")).await,
        missing.replace("git__nope", "slack__post")
    );

    let context = context_for(Some("cursor"));
    assert!(
        router
            .ensure_tool_round_supported("git__commit", &context, false, false)
            .is_err()
    );

    let task = router
        .enqueue_tool_task(
            "git__commit",
            None,
            None,
            TaskOwner::new(Arc::<str>::from("stdio:client-a")),
            None,
            Some(context_for(Some("cursor"))),
        )
        .await
        .expect_err("a task-wrapped call is a call");
    assert_eq!(
        task.to_string(),
        missing.replace("git__nope", "git__commit")
    );
}

#[tokio::test]
async fn the_meta_tools_do_not_reach_around_a_block() {
    let config = RouterConfig {
        meta_tool_mode: true,
        ..Default::default()
    };
    let router = router_with_client_blocks(config);
    let cursor = context_for(Some("cursor"));
    let pi = context_for(Some("pi"));

    let search = |context: &DownstreamCallContext, query: &str| {
        let mut args = serde_json::Map::new();
        args.insert("query".to_string(), serde_json::json!(query));
        result_text(
            &router
                .handle_search_tools(Some(args), Some(context))
                .expect("search"),
        )
    };
    assert!(!search(&cursor, "commit").contains("git__commit"));
    assert!(search(&pi, "commit").contains("git__commit"));
    assert!(!search(&pi, "post").contains("slack__post"));

    let listed = |context: &DownstreamCallContext| {
        result_text(
            &router
                .handle_list_tools(None, Some(context))
                .expect("list tools"),
        )
    };
    assert!(!listed(&cursor).contains("git__"));
    assert!(listed(&cursor).contains("slack__post"));
    assert!(!listed(&pi).contains("slack__"));

    let mut args = serde_json::Map::new();
    args.insert("tool_name".to_string(), serde_json::json!("git__commit"));
    let invoke = |context: DownstreamCallContext| {
        let args = args.clone();
        let router = &router;
        async move {
            router
                .call_tool_with_context("plug__invoke_tool", Some(args), None, Some(context))
                .await
                .expect_err("no upstream is connected")
                .to_string()
        }
    };
    let blocked = invoke(cursor.clone()).await;
    let allowed = invoke(pi.clone()).await;
    assert!(blocked.contains("git__commit"), "{blocked}");
    assert_ne!(
        blocked, allowed,
        "the wrapper must not carry a blocked call"
    );

    // A meta tool is a tool: its name can be blocked like any other.
    let mut clients = std::collections::BTreeMap::new();
    clients.insert(
        "cursor".to_string(),
        crate::config::ClientSettings {
            blocked_tools: vec!["plug__invoke_tool".into()],
            ..Default::default()
        },
    );
    router.set_client_access(&clients);
    let names = tool_names(&router.list_tools_for_client_session(
        ClientType::Unknown,
        None,
        Some("cursor"),
    ));
    assert!(
        !names.contains(&"plug__invoke_tool".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"plug__search_tools".to_string()),
        "{names:?}"
    );
}

#[test]
fn an_allow_list_leaves_a_client_the_tool_it_searches_with() {
    let config = RouterConfig {
        meta_tool_mode: true,
        ..Default::default()
    };
    let router = router_with_client_blocks(config);
    let mut clients = std::collections::BTreeMap::new();
    clients.insert(
        "radar".to_string(),
        crate::config::ClientSettings {
            allowed_servers: vec!["slack".to_string()],
            ..Default::default()
        },
    );
    router.set_client_access(&clients);

    let names =
        tool_names(&router.list_tools_for_client_session(ClientType::Unknown, None, Some("radar")));
    assert!(
        names.contains(&"plug__search_tools".to_string()),
        "{names:?}"
    );
    assert!(router.client_may_use_tool(Some("radar"), "plug__search_tools"));
    // What it finds is still only what is on the list.
    assert!(!router.client_may_use_tool(Some("radar"), "git__commit"));
}

#[test]
fn clients_hear_about_a_block_changing_and_about_nothing_else() {
    let router = ToolRouter::new(Arc::new(ServerManager::new()), RouterConfig::default());
    let mut notifications = router.subscribe_notifications();
    let mut clients = std::collections::BTreeMap::new();

    // A rename is not a change in what anyone can reach.
    clients.insert(
        "cursor".to_string(),
        crate::config::ClientSettings {
            name: Some("Work Cursor".to_string()),
            ..Default::default()
        },
    );
    router.set_client_access(&clients);
    assert!(notifications.try_recv().is_err());

    // A tool block changes the tool list and nothing else.
    clients.get_mut("cursor").unwrap().blocked_tools = vec!["git__push".into()];
    router.set_client_access(&clients);
    assert_eq!(
        notifications.try_recv().expect("tool block added"),
        ProtocolNotification::ToolListChanged
    );
    assert!(notifications.try_recv().is_err());

    // A server block also moves that server's resources and prompts.
    let mut whole_server_change = |clients: &std::collections::BTreeMap<_, _>, what: &str| {
        router.set_client_access(clients);
        for expected in [
            ProtocolNotification::ToolListChanged,
            ProtocolNotification::ResourceListChanged,
            ProtocolNotification::PromptListChanged,
        ] {
            assert_eq!(notifications.try_recv().expect(what), expected);
        }
        router.set_client_access(clients);
        assert!(notifications.try_recv().is_err(), "same blocks, no news");
    };
    clients.get_mut("cursor").unwrap().blocked_servers = vec!["git".to_string()];
    whole_server_change(&clients, "server block added");
    clients.clear();
    whole_server_change(&clients, "blocks removed");
    assert!(router.client_may_use_tool(Some("cursor"), "git__commit"));
}

#[test]
fn a_block_on_a_tool_covers_a_watch_of_it_and_survives_a_rename() {
    let router = router_with_git_commit_tool();
    let blocked = |tools: &[&str]| {
        let mut clients = std::collections::BTreeMap::new();
        clients.insert(
            "oauth:watcher".to_string(),
            crate::config::ClientSettings {
                blocked_tools: tools.iter().map(|tool| (*tool).into()).collect(),
                ..Default::default()
            },
        );
        router.set_client_access(&clients);
    };
    let key = Some("oauth:watcher");

    assert!(router.client_may_watch(key, "git", "commit"));
    // An event carries the tool's result, so a block on the tool covers it.
    blocked(&["git__commit"]);
    assert!(!router.client_may_watch(key, "git", "commit"));
    assert!(router.client_may_watch(None, "git", "commit"), "Plug's own");

    // The name the tool has once a second server shares its prefix, and the
    // name it goes back to: a block under either holds under both.
    blocked(&["gitGit__commit"]);
    assert!(!router.client_may_use_tool(key, "git__commit"));
    assert!(!router.client_may_watch(key, "git", "commit"));
    blocked(&["other__commit"]);
    assert!(router.client_may_use_tool(key, "git__commit"));

    // A result too large to send is kept as an artifact; the block on the
    // tool covers that too.
    blocked(&["git__commit"]);
    let store = &router.artifact_store;
    store.insert_for_test("kept", Some(("git", "commit")), "git__commit");
    // A result of a tool that is no longer listed, by any name.
    store.insert_for_test("gone", Some(("git", "retired")), "git__retired");
    store.insert_for_test("unknown", None, "task_result:1");
    let read = |id: &str, key| {
        let uri = format!("plug://artifact/{id}/manifest");
        futures::executor::block_on(router.read_resource(&uri, key)).is_ok()
    };
    assert!(!read("kept", key));
    assert!(read("kept", Some("oauth:someone-else")));
    assert!(read("kept", None));
    assert!(read("gone", key), "only git__commit is blocked");
    // Where a result came from is not known, so a client with any block
    // does not get it.
    assert!(!read("unknown", key));
    assert!(read("unknown", Some("oauth:someone-else")));

    // A tool block alone keeps the client from no server.
    assert!(!router.client_is_kept_from_a_server(key));
    let mut clients = std::collections::BTreeMap::new();
    clients.insert(
        "oauth:watcher".to_string(),
        crate::config::ClientSettings {
            blocked_servers: vec!["git".to_string()],
            ..Default::default()
        },
    );
    router.set_client_access(&clients);
    assert!(router.client_is_kept_from_a_server(key));
    // Kept from the server, so from every result it ever produced.
    assert!(!read("kept", key));
    assert!(!read("gone", key));

    // Stored by server and tool, a block does not go by the listed name at
    // all: it holds for a result kept while the tool had another name, and
    // for one kept before the tool was recorded with it.
    let by_server = |tool: &str| {
        let mut clients = std::collections::BTreeMap::new();
        clients.insert(
            "oauth:watcher".to_string(),
            crate::config::ClientSettings {
                blocked_tools: vec![crate::config::ToolBlock::Of {
                    server: "git".to_string(),
                    tool: tool.to_string(),
                }],
                ..Default::default()
            },
        );
        router.set_client_access(&clients);
    };
    by_server("commit");
    assert!(!router.client_may_use_tool(key, "git__commit"));
    assert!(!router.client_may_watch(key, "git", "commit"));
    store.insert_for_test("renamed", Some(("git", "commit")), "git__save");
    store.insert_for_test("old", Some(("git", "")), "git__anything");
    assert!(!read("kept", key));
    assert!(!read("renamed", key), "the same tool under an old name");
    assert!(read("gone", key), "another tool of the server");
    assert!(!read("old", key), "which tool is not known");
    by_server("COM*");
    assert!(!router.client_may_use_tool(key, "git__commit"), "a pattern");
    by_server("push");
    assert!(router.client_may_use_tool(key, "git__commit"));
    assert!(read("renamed", key));
    assert_eq!(
        router.tool_behind("GIT__commit"),
        Some(("git".to_string(), "commit".to_string()))
    );
    assert_eq!(router.tool_behind("git__nothing"), None);
    assert_eq!(
        router.listed_name("git", "commit").as_deref(),
        Some("git__commit")
    );

    // A rule by listed name holds for the server's own name for a tool too,
    // so renaming the tool does not take it out from under the rule.
    blocked(&["git__com*"]);
    assert!(!read("renamed", key));
    assert!(!router.client_is_kept_from_a_server(None));
}

/// Two servers, each with a resource, a resource template, and a prompt.
/// `cursor` is kept from `git`.
fn router_with_blocked_resources_and_prompts() -> Arc<ToolRouter> {
    let router = ToolRouter::new(Arc::new(ServerManager::new()), RouterConfig::default());
    router.replace_snapshot(RouterSnapshot {
        routes: HashMap::new(),
        routes_lower: HashMap::new(),
        tools_by_name: HashMap::new(),
        tools_by_name_lower: HashMap::new(),
        tools_all: Arc::new(Vec::new()),
        meta_tools_all: Arc::new(build_meta_tools()),
        tools_devin: Arc::new(Vec::new()),
        tools_copilot: Arc::new(Vec::new()),
        resources_all: Arc::new(vec![
            Resource::new("git://log", "git__log"),
            Resource::new("slack://inbox", "slack__inbox"),
        ]),
        resource_templates_all: Arc::new(vec![
            ResourceTemplate::new("git://commit/{id}", "git__commit"),
            ResourceTemplate::new("slack://channel/{id}", "slack__channel"),
        ]),
        prompts_all: Arc::new(vec![
            Prompt::new("git__review", Some("Review a change"), None),
            Prompt::new("slack__digest", Some("Digest a channel"), None),
        ]),
        resource_routes: HashMap::from([
            ("git://log".to_string(), "git".to_string()),
            ("git://commit/{id}".to_string(), "git".to_string()),
            ("slack://inbox".to_string(), "slack".to_string()),
            ("slack://channel/{id}".to_string(), "slack".to_string()),
        ]),
        prompt_routes: HashMap::from([
            (
                "git__review".to_string(),
                ("git".to_string(), "review".to_string()),
            ),
            (
                "slack__digest".to_string(),
                ("slack".to_string(), "digest".to_string()),
            ),
        ]),
        tool_definition_fingerprints: HashMap::new(),
        tool_risk_inventory: HashMap::new(),
    });
    let mut clients = std::collections::BTreeMap::new();
    clients.insert(
        "cursor".to_string(),
        crate::config::ClientSettings {
            blocked_servers: vec!["git".to_string()],
            ..Default::default()
        },
    );
    router.set_client_access(&clients);
    Arc::new(router)
}

#[test]
fn a_blocked_client_is_not_listed_the_resources_and_prompts_of_that_server() {
    let router = router_with_blocked_resources_and_prompts();

    let blocked = Some("cursor");
    let resources = router.list_resources_page(None, blocked).resources;
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].uri, "slack://inbox");
    let templates = router
        .list_resource_templates_page(None, blocked)
        .resource_templates;
    assert_eq!(templates.len(), 1);
    assert_eq!(templates[0].uri_template, "slack://channel/{id}");
    let prompts = router.list_prompts_page(None, blocked).prompts;
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].name, "slack__digest");

    // No entry, and no key at all, both get everything.
    for unblocked in [Some("claude-code"), None] {
        assert_eq!(
            router.list_resources_page(None, unblocked).resources.len(),
            2
        );
        assert_eq!(
            router
                .list_resource_templates_page(None, unblocked)
                .resource_templates
                .len(),
            2
        );
        assert_eq!(router.list_prompts_page(None, unblocked).prompts.len(), 2);
    }
}

#[tokio::test]
async fn a_blocked_resource_or_prompt_answers_as_one_that_does_not_exist() {
    let router = router_with_blocked_resources_and_prompts();
    let blocked = Some("cursor");
    let complete = |reference: Reference| {
        CompleteRequestParams::new(reference, ArgumentInfo::new("arg", "par"))
    };

    // An exact resource, and one a template expands to.
    for (uri, missing) in [
        ("git://log", "git://nope"),
        ("git://commit/abc", "nope://commit/abc"),
    ] {
        let unknown = router
            .read_resource(missing, blocked)
            .await
            .expect_err("no such resource")
            .to_string();
        let kept = router
            .read_resource(uri, blocked)
            .await
            .expect_err("kept from the server")
            .to_string();
        assert_eq!(kept, unknown.replace(missing, uri));
        // Anyone else gets as far as the server, which is not connected here.
        let allowed = router
            .read_resource(uri, None)
            .await
            .expect_err("no upstream is connected")
            .to_string();
        assert_ne!(allowed, kept);

        let kept = router
            .complete_request(complete(Reference::for_resource(uri)), blocked)
            .await
            .expect_err("kept from the server")
            .to_string();
        assert_eq!(kept, unknown.replace(missing, uri));
    }

    let unknown = router
        .get_prompt("git__nope", None, blocked)
        .await
        .expect_err("no such prompt")
        .to_string();
    let kept = router
        .get_prompt("git__review", None, blocked)
        .await
        .expect_err("kept from the server")
        .to_string();
    assert_eq!(kept, unknown.replace("git__nope", "git__review"));
    let kept = router
        .complete_request(complete(Reference::for_prompt("git__review")), blocked)
        .await
        .expect_err("kept from the server")
        .to_string();
    assert_eq!(kept, unknown.replace("git__nope", "git__review"));
    assert_ne!(
        router
            .get_prompt("git__review", None, None)
            .await
            .expect_err("no upstream is connected")
            .to_string(),
        kept
    );

    let unknown = router
        .subscribe_resource("git://nope", sub_target("a"), blocked)
        .await
        .expect_err("no such resource")
        .to_string();
    let kept = router
        .subscribe_resource("git://log", sub_target("a"), blocked)
        .await
        .expect_err("kept from the server")
        .to_string();
    assert_eq!(kept, unknown.replace("git://nope", "git://log"));
    assert_eq!(router.active_subscription_count(), 0);
}

#[test]
fn a_client_kept_from_a_server_stops_hearing_its_resource_updates() {
    let router = router_with_blocked_resources_and_prompts();
    let cursor = NotificationTarget::Ipc {
        client_id: Arc::from("cursor-session"),
    };
    let zed = NotificationTarget::Ipc {
        client_id: Arc::from("zed-session"),
    };
    // Both subscribed before `cursor` was kept from `git`.
    for (target, key) in [(&cursor, "cursor"), (&zed, "zed")] {
        router
            .resource_subscriptions
            .insert_active_for_test("git://log", target.clone());
        router
            .subscriber_client_keys
            .insert(target.clone(), key.to_string());
    }
    let mut notifications = router.subscribe_notifications();
    let mut heard = |router: &ToolRouter| {
        router.route_upstream_resource_updated(ResourceUpdatedNotificationParam::new("git://log"));
        let mut targets = Vec::new();
        while let Ok(notification) = notifications.try_recv() {
            if let ProtocolNotification::ResourceUpdated { target, .. } = notification {
                targets.push(target);
            }
        }
        targets
    };

    assert_eq!(heard(&router), vec![zed.clone()]);

    // Lifting the block brings the updates back without a new subscribe.
    router.set_client_access(&std::collections::BTreeMap::new());
    let both = heard(&router);
    assert_eq!(both.len(), 2);
    assert!(both.contains(&cursor) && both.contains(&zed));
}
