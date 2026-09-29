use dialoguer::Select;
use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::clients::{
    all_client_targets, client_display_name, client_views, cmd_link, cmd_unlink, live_session_views,
};
use crate::runtime::{
    LiveClientSupport, daemon_running, fetch_live_sessions, live_inventory_metadata,
};
use crate::ui::{
    can_prompt_interactively, cli_prompt_theme, print_banner, print_heading, print_info_line,
    print_label_value, print_success_line, print_warning_line,
};

fn live_inventory_scope_text(scope: plug_core::ipc::LiveSessionInventoryScope) -> &'static str {
    match scope {
        plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly => {
            "Live session inventory currently reflects daemon proxy clients only; downstream HTTP sessions are not yet surfaced here."
        }
        plug_core::ipc::LiveSessionInventoryScope::HttpOnly => {
            "Live session inventory currently reflects downstream HTTP sessions only; daemon proxy sessions are not available."
        }
        plug_core::ipc::LiveSessionInventoryScope::TransportComplete => {
            "Live session inventory includes both daemon proxy and downstream HTTP sessions."
        }
        plug_core::ipc::LiveSessionInventoryScope::Unavailable => {
            "Live session inventory is unavailable from both daemon proxy and downstream HTTP sources."
        }
    }
}

fn live_inventory_scope_label(scope: plug_core::ipc::LiveSessionInventoryScope) -> &'static str {
    match scope {
        plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly => "daemon-proxy-only",
        plug_core::ipc::LiveSessionInventoryScope::HttpOnly => "http-only",
        plug_core::ipc::LiveSessionInventoryScope::TransportComplete => "transport-complete",
        plug_core::ipc::LiveSessionInventoryScope::Unavailable => "unavailable",
    }
}

fn live_inventory_summary(inventory: &crate::runtime::LiveInventoryMetadata) -> String {
    let scope = live_inventory_scope_label(inventory.scope);
    if inventory.availability.partial {
        format!(
            "{scope} (missing: {})",
            inventory.availability.unavailable_sources.join(", ")
        )
    } else {
        scope.to_string()
    }
}

fn configured_client_state_text(client: &crate::commands::clients::ClientView) -> String {
    let mut states = Vec::new();
    if client.detected {
        states.push("detected".to_string());
    }
    if client.live {
        let transport_summary = if client.live_transports.is_empty() {
            "unknown".to_string()
        } else {
            client
                .live_transports
                .iter()
                .map(|transport| match transport.as_str() {
                    "daemon_proxy" => "local",
                    other => other,
                })
                .collect::<Vec<_>>()
                .join("+")
        };
        let sessions = match client.live_sessions {
            1 => "1 live session".to_string(),
            count => format!("{count} live sessions"),
        };
        states.push(format!("{sessions} ({transport_summary})"));
    }

    if states.is_empty() {
        "configured only".to_string()
    } else {
        states.join(", ")
    }
}

fn configured_client_link_text(client: &crate::commands::clients::ClientView) -> Option<String> {
    let transport = client.linked_transport.as_deref()?;
    let mut detail = format!("linked via {transport}");
    if let Some(endpoint) = client.linked_endpoint.as_deref() {
        detail.push_str(" -> ");
        detail.push_str(endpoint);
    }
    Some(detail)
}

fn configured_client_lazy_tool_text(client: &crate::commands::clients::ClientView) -> String {
    format!(
        "lazy tools: {} ({}) - {}",
        client.lazy_tool_mode, client.lazy_tool_mode_origin, client.lazy_tool_mode_reason
    )
}

fn client_list_json(
    clients: &[crate::commands::clients::ClientView],
    live_sessions: &[crate::commands::clients::LiveSessionView],
    inventory: &crate::runtime::LiveInventoryMetadata,
    live_client_support: LiveClientSupport,
    live_inventory_scope: plug_core::ipc::LiveSessionInventoryScope,
    daemon_error: Option<&str>,
    config_error: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "clients": clients,
        "live_sessions": live_sessions,
        "live_session_count": inventory.session_count,
        "live_session_transports": inventory.session_transports,
        "live_client_support": live_client_support,
        "live_inventory_scope": live_inventory_scope,
        "inventory_partial": inventory.availability.partial,
        "inventory_unavailable_sources": inventory.availability.unavailable_sources,
        "http_sessions_included": inventory.http_sessions_included,
        "daemon_error": daemon_error,
        "config_error": config_error,
        "lazy_tool_policy_scope": "configured",
        "lazy_tool_live_policy_note": "clients[].lazy_tool_* fields reflect config/default resolution; live sessions keep their daemon-start policy until restart after lazy_tools edits",
        "restart_required_for_lazy_tool_config_changes": inventory.session_count > 0,
    })
}

fn prompt_lazy_tool_mode(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let (path, mut config) = crate::commands::config::load_editable_config(config_path)?;
    let targets = all_client_targets();
    let labels = targets
        .iter()
        .map(|(display, target)| {
            let policy = plug_core::config::resolve_lazy_tool_policy_for_target(&config, target);
            format!(
                "{:<20} {} ({})",
                display,
                policy.mode.label(),
                policy.origin.label()
            )
        })
        .collect::<Vec<_>>();

    let target_selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Choose client")
        .items(&labels)
        .default(0)
        .interact_opt()?;
    let Some(target_index) = target_selection else {
        return Ok(());
    };
    let target = targets[target_index].1;

    let mode_options = ["auto", "standard", "native", "bridge"];
    let current = config
        .lazy_tools
        .clients
        .get(target)
        .copied()
        .unwrap_or(plug_core::types::LazyToolSetting::Auto);
    let default = match current {
        plug_core::types::LazyToolSetting::Auto => 0,
        plug_core::types::LazyToolSetting::Standard => 1,
        plug_core::types::LazyToolSetting::Native => 2,
        plug_core::types::LazyToolSetting::Bridge => 3,
    };

    let mode_selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Lazy tool mode")
        .items(mode_options)
        .default(default)
        .interact_opt()?;
    let Some(mode_index) = mode_selection else {
        return Ok(());
    };

    match mode_index {
        0 => {
            config.lazy_tools.clients.remove(target);
        }
        1 => {
            config.lazy_tools.clients.insert(
                target.to_string(),
                plug_core::types::LazyToolSetting::Standard,
            );
        }
        2 => {
            config.lazy_tools.clients.insert(
                target.to_string(),
                plug_core::types::LazyToolSetting::Native,
            );
        }
        3 => {
            config.lazy_tools.clients.insert(
                target.to_string(),
                plug_core::types::LazyToolSetting::Bridge,
            );
        }
        _ => unreachable!(),
    }

    crate::commands::config::save_config(&path, &config)?;
    let policy = plug_core::config::resolve_lazy_tool_policy_for_target(&config, target);
    print_success_line(format!(
        "{} lazy tools: {} ({})",
        client_display_name(target),
        policy.mode.label(),
        policy.origin.label()
    ));
    print_warning_line(
        "Restart the plug daemon before relying on this lazy tool mode in live client sessions.",
    );
    Ok(())
}

fn prompt_client_actions(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<bool> {
    let options = [
        "Done",
        "Link clients",
        "Unlink clients",
        "Configure lazy tools",
    ];
    let selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Choose action")
        .items(options)
        .default(0)
        .interact_opt()?;

    match selection {
        Some(1) => {
            cmd_link(None, Vec::new(), false, false, None)?;
            Ok(true)
        }
        Some(2) => {
            cmd_unlink(Vec::new(), false, false)?;
            Ok(true)
        }
        Some(3) => {
            prompt_lazy_tool_mode(config_path)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Live sessions for one client, so `plug clients` shows `Claude Code  12`
/// instead of twelve near-identical rows.
#[derive(Debug, PartialEq, Eq)]
struct LiveSessionGroup<'a> {
    client: &'a str,
    sessions: usize,
    transports: Vec<&'a str>,
    longest_connected_secs: u64,
}

/// Busiest client first, then by name.
fn live_session_groups(
    sessions: &[crate::commands::clients::LiveSessionView],
) -> Vec<LiveSessionGroup<'_>> {
    let mut groups: Vec<LiveSessionGroup<'_>> = Vec::new();
    for session in sessions {
        let transport = match session.transport.as_str() {
            "daemon_proxy" => "local",
            other => other,
        };
        match groups
            .iter_mut()
            .find(|group| group.client == session.client_type)
        {
            Some(group) => {
                group.sessions += 1;
                if !group.transports.contains(&transport) {
                    group.transports.push(transport);
                }
                group.longest_connected_secs =
                    group.longest_connected_secs.max(session.connected_secs);
            }
            None => groups.push(LiveSessionGroup {
                client: &session.client_type,
                sessions: 1,
                transports: vec![transport],
                longest_connected_secs: session.connected_secs,
            }),
        }
    }
    groups.sort_by(|a, b| b.sessions.cmp(&a.sessions).then(a.client.cmp(b.client)));
    groups
}

fn print_live_session_rows(sessions: &[crate::commands::clients::LiveSessionView]) {
    println!(
        "  {:<18} {:<14} {:<12} {:<10} {:<10}",
        style("SESSION").dim(),
        style("CLIENT").dim(),
        style("TRANSPORT").dim(),
        style("CONNECTED").dim(),
        style("IDLE").dim()
    );
    println!(
        "  {}",
        style("--------------------------------------------------------------------------").dim()
    );
    for session in sessions {
        let idle = session
            .last_activity_secs
            .map(crate::ui::format_duration)
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  {:<18} {:<14} {:<12} {:<10} {:<10}",
            &session.session_id[..session.session_id.len().min(18)],
            session.client_type,
            session.transport,
            crate::ui::format_duration(session.connected_secs),
            idle,
        );
    }
}

/// One line for every client Plug knows about but has not linked, noting the
/// ones that are installed or connected anyway.
fn unlinked_clients_text(clients: &[crate::commands::clients::ClientView]) -> Option<String> {
    let names = clients
        .iter()
        .filter(|client| !client.linked)
        .map(|client| {
            if client.live {
                format!("{} (live)", client.name)
            } else if client.detected {
                format!("{} (detected)", client.name)
            } else {
                client.name.clone()
            }
        })
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(", "))
}

pub(crate) async fn cmd_client_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
    verbose: bool,
) -> anyhow::Result<()> {
    let interactive = matches!(output, OutputFormat::Text) && can_prompt_interactively();
    let mut started = false;

    loop {
        let daemon_error = if daemon_running().await {
            None
        } else if crate::runtime::daemon_starting() {
            Some("daemon is starting".to_string())
        } else {
            Some("daemon not running".to_string())
        };
        let (live, live_inventory_scope, live_client_support) =
            fetch_live_sessions(config_path).await;
        let inventory = live_inventory_metadata(&live, live_inventory_scope);
        let config_result = plug_core::config::load_config(config_path);
        let config_error = config_result.as_ref().err().map(|error| error.to_string());
        let config = config_result.ok();
        let clients = client_views(&live, config.as_ref());
        let live_sessions = live_session_views(&live);

        if matches!(output, OutputFormat::Json) {
            println!(
                "{}",
                serde_json::to_string_pretty(&client_list_json(
                    &clients,
                    &live_sessions,
                    &inventory,
                    live_client_support,
                    live_inventory_scope,
                    daemon_error.as_deref(),
                    config_error.as_deref(),
                ))?
            );
            return Ok(());
        }

        print_banner("◆", "Clients", "Linked, detected, and live AI clients");
        if started {
            println!();
        }
        if matches!(
            live_client_support,
            LiveClientSupport::DaemonRestartRequired
        ) {
            print_warning_line(
                "Live client inspection requires restarting the background daemon after this upgrade.",
            );
            println!();
        } else if let Some(error) = &daemon_error {
            print_warning_line(format!(
                "Live client inspection unavailable: {error}. Showing linked and detected clients from config only."
            ));
            println!();
        }
        if let Some(error) = &config_error {
            print_warning_line(format!(
                "Config validation failed: {error}. Lazy tool modes below fall back to defaults until config is fixed."
            ));
            println!();
        }
        let linked_count = clients.iter().filter(|client| client.linked).count();
        let detected_count = clients.iter().filter(|client| client.detected).count();
        print_heading("Summary");
        print_label_value("Linked", style(linked_count).green().bold());
        print_label_value("Detected", style(detected_count).cyan().bold());
        match (&daemon_error, &live_client_support) {
            (Some(_), _) => {
                print_label_value("Live", style("unavailable").yellow().bold());
            }
            (None, LiveClientSupport::Supported) => {
                print_label_value("Live", style(inventory.session_count).bold());
            }
            (None, LiveClientSupport::DaemonRestartRequired) => {
                print_label_value("Live", style("restart required").yellow().bold());
            }
        }
        if daemon_error.is_none() && matches!(live_client_support, LiveClientSupport::Supported) {
            print_label_value("Live Inventory", live_inventory_summary(&inventory));
            print_info_line(live_inventory_scope_text(live_inventory_scope));
            print_label_value("Live Transports", inventory.session_transports.summary());
            if inventory.session_count > 0 {
                print_info_line(
                    "Lazy tool modes below are configured values; live sessions keep their daemon-start policy until restart after lazy_tools edits.",
                );
            }
        }
        println!();
        print_heading("Live Sessions");
        if live_sessions.is_empty() {
            print_info_line("No live downstream sessions observed.");
        } else {
            println!(
                "  {:<24} {:<10} {:<14} {}",
                style("CLIENT").dim(),
                style("SESSIONS").dim(),
                style("TRANSPORT").dim(),
                style("LONGEST").dim()
            );
            println!(
                "  {}",
                style("----------------------------------------------------------------").dim()
            );
            for group in live_session_groups(&live_sessions) {
                println!(
                    "  {:<24} {:<10} {:<14} {}",
                    group.client,
                    group.sessions,
                    group.transports.join("+"),
                    crate::ui::format_duration(group.longest_connected_secs),
                );
            }
            if verbose {
                println!();
                print_live_session_rows(&live_sessions);
            } else {
                print_info_line(style("Run `plug clients -v` to list each session.").dim());
            }
        }

        println!();
        print_heading("Configured Clients");
        let linked_clients = clients.iter().filter(|client| client.linked);
        if linked_count == 0 {
            print_info_line("No clients linked yet. Run `plug link` to add one.");
        } else {
            println!("  {:<24} {}", style("CLIENT").dim(), style("STATE").dim());
            println!(
                "  {}",
                style("----------------------------------------------------------------").dim()
            );
        }
        for client in linked_clients {
            println!(
                "  {:<24} {}",
                client.name,
                style(configured_client_state_text(client)).dim()
            );
            if let Some(link_text) = configured_client_link_text(client) {
                print_info_line(style(link_text).dim());
            }
            print_info_line(style(configured_client_lazy_tool_text(client)).dim());
        }
        if let Some(unlinked) = unlinked_clients_text(&clients) {
            println!();
            print_label_value("Not linked", style(unlinked).dim());
        }

        if !interactive {
            break;
        }
        println!();
        if !prompt_client_actions(config_path)? {
            break;
        }
        println!();
        started = false;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        LiveSessionGroup, client_list_json, configured_client_lazy_tool_text,
        configured_client_link_text, configured_client_state_text, live_inventory_scope_label,
        live_inventory_scope_text, live_inventory_summary, live_session_groups,
        unlinked_clients_text,
    };
    use crate::commands::clients::{ClientView, LiveSessionView};
    use crate::runtime::{
        LiveInventoryAvailability, LiveInventoryMetadata, LiveSessionTransportCounts,
    };

    #[test]
    fn client_list_json_includes_inventory_contract_fields() {
        let clients = vec![ClientView {
            name: "Claude".to_string(),
            target: "claude-desktop".to_string(),
            linked: true,
            linked_transport: Some("http".to_string()),
            linked_endpoint: Some("https://plug.example.com/mcp".to_string()),
            detected: true,
            live: true,
            live_sessions: 1,
            live_transports: vec!["http".to_string()],
            lazy_tool_mode: "bridge".to_string(),
            lazy_tool_mode_origin: "auto_default".to_string(),
            lazy_tool_mode_reason: "client benefits from plug-owned search bridge discovery"
                .to_string(),
        }];
        let live_sessions = vec![LiveSessionView {
            session_id: "session-1".to_string(),
            client_type: "claude_desktop".to_string(),
            transport: "http".to_string(),
            client_id: None,
            client_info: Some("Claude Desktop".to_string()),
            connected_secs: 12,
            last_activity_secs: Some(3),
        }];
        let inventory = LiveInventoryMetadata {
            session_count: 1,
            session_transports: LiveSessionTransportCounts {
                daemon_proxy: 0,
                http: 1,
                sse: 0,
            },
            scope: plug_core::ipc::LiveSessionInventoryScope::HttpOnly,
            availability: LiveInventoryAvailability {
                partial: true,
                unavailable_sources: vec!["daemon_proxy"],
            },
            http_sessions_included: true,
        };

        let json = client_list_json(
            &clients,
            &live_sessions,
            &inventory,
            crate::runtime::LiveClientSupport::Supported,
            plug_core::ipc::LiveSessionInventoryScope::HttpOnly,
            Some("daemon unavailable"),
            Some("invalid lazy config"),
        );

        assert_eq!(json["live_session_count"], 1);
        assert_eq!(json["live_session_transports"]["http"], 1);
        assert_eq!(json["live_inventory_scope"], "http_only");
        assert_eq!(json["inventory_partial"], true);
        assert_eq!(json["inventory_unavailable_sources"][0], "daemon_proxy");
        assert_eq!(json["http_sessions_included"], true);
        assert_eq!(json["daemon_error"], "daemon unavailable");
        assert_eq!(json["config_error"], "invalid lazy config");
        assert_eq!(json["lazy_tool_policy_scope"], "configured");
        assert_eq!(json["restart_required_for_lazy_tool_config_changes"], true);
    }

    #[test]
    fn live_inventory_scope_label_uses_stable_short_states() {
        assert_eq!(
            live_inventory_scope_label(plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly),
            "daemon-proxy-only"
        );
        assert_eq!(
            live_inventory_scope_label(plug_core::ipc::LiveSessionInventoryScope::HttpOnly),
            "http-only"
        );
        assert_eq!(
            live_inventory_scope_label(
                plug_core::ipc::LiveSessionInventoryScope::TransportComplete
            ),
            "transport-complete"
        );
        assert_eq!(
            live_inventory_scope_label(plug_core::ipc::LiveSessionInventoryScope::Unavailable),
            "unavailable"
        );
    }

    #[test]
    fn live_inventory_scope_text_mentions_daemon_and_http_gap() {
        let text =
            live_inventory_scope_text(plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly);
        assert!(text.contains("daemon proxy clients"));
        assert!(text.contains("HTTP sessions"));
    }

    #[test]
    fn live_inventory_scope_text_covers_http_only_and_unavailable() {
        let http_only =
            live_inventory_scope_text(plug_core::ipc::LiveSessionInventoryScope::HttpOnly);
        assert!(http_only.contains("HTTP sessions only"));

        let unavailable =
            live_inventory_scope_text(plug_core::ipc::LiveSessionInventoryScope::Unavailable);
        assert!(unavailable.contains("unavailable"));
    }

    #[test]
    fn configured_client_state_text_prefers_compact_presence_summary() {
        let client = ClientView {
            name: "Codex CLI".to_string(),
            target: "codex-cli".to_string(),
            linked: true,
            linked_transport: Some("stdio".to_string()),
            linked_endpoint: None,
            detected: true,
            live: true,
            live_sessions: 2,
            live_transports: vec!["daemon_proxy".to_string()],
            lazy_tool_mode: "native".to_string(),
            lazy_tool_mode_origin: "auto_default".to_string(),
            lazy_tool_mode_reason: "client has its own native lazy/deferred tool discovery"
                .to_string(),
        };
        assert_eq!(
            configured_client_state_text(&client),
            "detected, 2 live sessions (local)"
        );
    }

    #[test]
    fn configured_client_link_text_includes_endpoint_when_present() {
        let client = ClientView {
            name: "Claude Code".to_string(),
            target: "claude-code".to_string(),
            linked: true,
            linked_transport: Some("http".to_string()),
            linked_endpoint: Some("https://plug.example.com/mcp".to_string()),
            detected: false,
            live: false,
            live_sessions: 0,
            live_transports: Vec::new(),
            lazy_tool_mode: "native".to_string(),
            lazy_tool_mode_origin: "auto_default".to_string(),
            lazy_tool_mode_reason: "client has its own native lazy/deferred tool discovery"
                .to_string(),
        };
        assert_eq!(
            configured_client_link_text(&client).as_deref(),
            Some("linked via http -> https://plug.example.com/mcp")
        );
    }

    #[test]
    fn configured_client_lazy_tool_text_explains_mode_origin_and_reason() {
        let client = ClientView {
            name: "OpenCode".to_string(),
            target: "opencode".to_string(),
            linked: true,
            linked_transport: Some("stdio".to_string()),
            linked_endpoint: None,
            detected: true,
            live: false,
            live_sessions: 0,
            live_transports: Vec::new(),
            lazy_tool_mode: "bridge".to_string(),
            lazy_tool_mode_origin: "client_override".to_string(),
            lazy_tool_mode_reason: "configured for this client target".to_string(),
        };

        assert_eq!(
            configured_client_lazy_tool_text(&client),
            "lazy tools: bridge (client_override) - configured for this client target"
        );
    }

    #[test]
    fn live_inventory_summary_collapses_scope_and_availability() {
        let inventory = LiveInventoryMetadata {
            session_count: 0,
            session_transports: LiveSessionTransportCounts::default(),
            scope: plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly,
            availability: LiveInventoryAvailability {
                partial: true,
                unavailable_sources: vec!["http"],
            },
            http_sessions_included: false,
        };

        assert_eq!(
            live_inventory_summary(&inventory),
            "daemon-proxy-only (missing: http)"
        );
    }

    fn session(client_type: &str, transport: &str, connected_secs: u64) -> LiveSessionView {
        LiveSessionView {
            session_id: format!("{client_type}-{connected_secs}"),
            client_type: client_type.to_string(),
            transport: transport.to_string(),
            client_id: None,
            client_info: None,
            connected_secs,
            last_activity_secs: None,
        }
    }

    fn client(name: &str, linked: bool, detected: bool, live: bool) -> ClientView {
        ClientView {
            name: name.to_string(),
            target: name.to_lowercase(),
            linked,
            linked_transport: None,
            linked_endpoint: None,
            detected,
            live,
            live_sessions: usize::from(live),
            live_transports: Vec::new(),
            lazy_tool_mode: "native".to_string(),
            lazy_tool_mode_origin: "auto_default".to_string(),
            lazy_tool_mode_reason: String::new(),
        }
    }

    #[test]
    fn live_sessions_group_by_client_busiest_first() {
        let sessions = vec![
            session("Claude Code", "daemon_proxy", 60),
            session("Codex CLI", "daemon_proxy", 30),
            session("Codex CLI", "daemon_proxy", 7_200),
            session("Codex CLI", "http", 10),
            session("Cursor", "http", 5),
        ];
        assert_eq!(
            live_session_groups(&sessions),
            vec![
                LiveSessionGroup {
                    client: "Codex CLI",
                    sessions: 3,
                    transports: vec!["local", "http"],
                    longest_connected_secs: 7_200,
                },
                LiveSessionGroup {
                    client: "Claude Code",
                    sessions: 1,
                    transports: vec!["local"],
                    longest_connected_secs: 60,
                },
                LiveSessionGroup {
                    client: "Cursor",
                    sessions: 1,
                    transports: vec!["http"],
                    longest_connected_secs: 5,
                },
            ]
        );
    }

    #[test]
    fn unlinked_clients_collapse_into_one_line() {
        let clients = vec![
            client("Claude Code", true, true, true),
            client("Claude Desktop", false, true, false),
            client("Goose", false, false, true),
            client("Zed", false, false, false),
        ];
        assert_eq!(
            unlinked_clients_text(&clients).as_deref(),
            Some("Claude Desktop (detected), Goose (live), Zed")
        );
        assert_eq!(unlinked_clients_text(&clients[..1]), None);
    }
}
