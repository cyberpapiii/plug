use dialoguer::Select;
use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::clients::{client_views, cmd_link, cmd_unlink, live_session_views};
use crate::runtime::{LiveClientSupport, ensure_daemon_with_feedback, fetch_live_sessions};
use crate::ui::{
    can_prompt_interactively, cli_prompt_theme, print_banner, print_heading, print_info_line,
    print_label_value, print_warning_line,
};

fn live_inventory_scope_text(scope: plug_core::ipc::LiveSessionInventoryScope) -> &'static str {
    match scope {
        plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly => {
            "Live session inventory currently reflects daemon proxy clients only; downstream HTTP sessions are not yet surfaced here."
        }
        plug_core::ipc::LiveSessionInventoryScope::TransportComplete => {
            "Live session inventory includes both daemon proxy and downstream HTTP sessions."
        }
    }
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
    let mut daemon_error = None;
    let mut started = match ensure_daemon_with_feedback(
        config_path,
        matches!(output, OutputFormat::Text),
    )
    .await
    {
        Ok(started) => started,
        Err(error) => {
            daemon_error = Some(error.to_string());
            false
        }
    };

    loop {
        let (live, live_inventory_scope, live_client_support) = fetch_live_sessions().await;
        let clients = client_views(&live);
        let live_sessions = live_session_views(&live);

        if matches!(output, OutputFormat::Json) {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "clients": clients,
                    "live_sessions": live_sessions,
                    "live_client_support": live_client_support,
                    "live_inventory_scope": live_inventory_scope,
                    "http_sessions_included": matches!(
                        live_inventory_scope,
                        plug_core::ipc::LiveSessionInventoryScope::TransportComplete
                    ),
                    "daemon_error": daemon_error,
                }))?
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
        let live_count = live_sessions.len();
        let daemon_proxy_count = live_sessions
            .iter()
            .filter(|session| session.transport == "daemon_proxy")
            .count();
        let http_count = live_sessions
            .iter()
            .filter(|session| session.transport == "http")
            .count();
        print_heading("Summary");
        print_label_value("Linked", style(linked_count).green().bold());
        print_label_value("Detected", style(detected_count).cyan().bold());
        match (&daemon_error, &live_client_support) {
            (Some(_), _) => {
                print_label_value("Live", style("unavailable").yellow().bold());
            }
            (None, LiveClientSupport::Supported) => {
                print_label_value("Live", style(live_count).bold());
            }
            (None, LiveClientSupport::DaemonRestartRequired) => {
                print_label_value("Live", style("restart required").yellow().bold());
            }
        }
        if daemon_error.is_none() && matches!(live_client_support, LiveClientSupport::Supported) {
            print_info_line(live_inventory_scope_text(live_inventory_scope));
            print_label_value(
                "Live Transports",
                format!("daemon_proxy={} http={}", daemon_proxy_count, http_count),
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
            "  {:<24} {:<10} {:<10} {:<28} {:<10} {:<6}",
            style("CLIENT").dim(),
            style("LINKED").dim(),
            style("MODE").dim(),
            style("ENDPOINT").dim(),
            style("DETECTED").dim(),
            style("LIVE").dim()
        );
        println!(
            "  {}",
            style("------------------------------------------------------------------------------------------------").dim()
        );
        for client in &clients {
            let linked = if client.linked {
                style("yes").green().bold()
            } else {
                style("no").dim()
            };
            let linked_transport = client
                .linked_transport
                .as_deref()
                .map(|transport| style(transport).yellow().bold().to_string())
                .unwrap_or_else(|| style("-").dim().to_string());
            let endpoint = client
                .linked_endpoint
                .as_deref()
                .map(|endpoint| {
                    let mut value = endpoint.to_string();
                    if value.len() > 28 {
                        value.truncate(25);
                        value.push_str("...");
                    }
                    style(value).dim().to_string()
                })
                .unwrap_or_else(|| style("-").dim().to_string());
            let detected = if client.detected {
                style("yes").cyan()
            } else {
                style("no").dim()
            };
            let live_label = if client.live {
                style(format!("yes ({})", client.live_sessions))
                    .green()
                    .bold()
                    .to_string()
            } else {
                style("no").dim().to_string()
            };
            println!(
                "  {:<24} {:<10} {:<10} {:<28} {:<10} {:<6}",
                client.name, linked, linked_transport, endpoint, detected, live_label
            );
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
    use super::live_inventory_scope_text;

    #[test]
    fn live_inventory_scope_text_mentions_daemon_and_http_gap() {
        let text =
            live_inventory_scope_text(plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly);
        assert!(text.contains("daemon proxy clients"));
        assert!(text.contains("HTTP sessions"));
    }
}
