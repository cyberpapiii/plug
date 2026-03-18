use dialoguer::Select;
use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::clients::{client_views, cmd_link, cmd_unlink, live_session_views};
use crate::runtime::{
    LiveClientSupport, daemon_running, fetch_live_sessions, live_inventory_metadata,
};
use crate::ui::{
    can_prompt_interactively, cli_prompt_theme, print_banner, print_heading, print_info_line,
    print_label_value, print_warning_line,
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
            client.live_transports.join("+")
        };
        states.push(format!(
            "live via {transport_summary} ({})",
            client.live_sessions
        ));
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

fn client_list_json(
    clients: &[crate::commands::clients::ClientView],
    live_sessions: &[crate::commands::clients::LiveSessionView],
    inventory: &crate::runtime::LiveInventoryMetadata,
    live_client_support: LiveClientSupport,
    live_inventory_scope: plug_core::ipc::LiveSessionInventoryScope,
    daemon_error: Option<&str>,
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
    })
}

fn prompt_client_actions() -> anyhow::Result<bool> {
    let options = ["Done", "Link clients", "Unlink clients"];
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
        _ => Ok(false),
    }
}

pub(crate) async fn cmd_client_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let interactive = matches!(output, OutputFormat::Text) && can_prompt_interactively();
    let mut started = false;

    loop {
        let daemon_error = if daemon_running().await {
            None
        } else {
            Some("daemon not running".to_string())
        };
        let (live, live_inventory_scope, live_client_support) =
            fetch_live_sessions(config_path).await;
        let inventory = live_inventory_metadata(&live, live_inventory_scope);
        let clients = client_views(&live);
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
            print_label_value(
                "Live Transports",
                format!(
                    "daemon_proxy={} http={} sse={}",
                    inventory.session_transports.daemon_proxy,
                    inventory.session_transports.http,
                    inventory.session_transports.sse
                ),
            );
        }
        println!();
        print_heading("Live Sessions");
        if live_sessions.is_empty() {
            print_info_line("No live downstream sessions observed.");
        } else {
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
                style("--------------------------------------------------------------------------")
                    .dim()
            );
            for session in &live_sessions {
                let idle = session
                    .last_activity_secs
                    .map(|seconds| format!("{seconds}s"))
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "  {:<18} {:<14} {:<12} {:<10} {:<10}",
                    &session.session_id[..session.session_id.len().min(18)],
                    session.client_type,
                    session.transport,
                    format!("{}s", session.connected_secs),
                    idle,
                );
            }
        }

        println!();
        print_heading("Configured Clients");
        println!(
            "  {:<24} {:<10} {}",
            style("CLIENT").dim(),
            style("LINKED").dim(),
            style("STATE").dim()
        );
        println!(
            "  {}",
            style("----------------------------------------------------------------").dim()
        );
        for client in &clients {
            let linked = if client.linked {
                style("yes").green().bold()
            } else {
                style("no").dim()
            };
            println!(
                "  {:<24} {:<10} {}",
                client.name,
                linked,
                style(configured_client_state_text(client)).dim()
            );
            if let Some(link_text) = configured_client_link_text(client) {
                print_info_line(style(link_text).dim());
            }
        }

        if !interactive {
            break;
        }
        println!();
        if !prompt_client_actions()? {
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
        client_list_json, configured_client_link_text, configured_client_state_text,
        live_inventory_scope_label, live_inventory_scope_text, live_inventory_summary,
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
        );

        assert_eq!(json["live_session_count"], 1);
        assert_eq!(json["live_session_transports"]["http"], 1);
        assert_eq!(json["live_inventory_scope"], "http_only");
        assert_eq!(json["inventory_partial"], true);
        assert_eq!(json["inventory_unavailable_sources"][0], "daemon_proxy");
        assert_eq!(json["http_sessions_included"], true);
        assert_eq!(json["daemon_error"], "daemon unavailable");
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
        };
        assert_eq!(
            configured_client_state_text(&client),
            "detected, live via daemon_proxy (2)"
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
        };
        assert_eq!(
            configured_client_link_text(&client).as_deref(),
            Some("linked via http -> https://plug.example.com/mcp")
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
}
