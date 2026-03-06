use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::clients::linked_client_targets;
use crate::runtime::{LiveClientSupport, ensure_daemon_with_feedback, fetch_live_clients};
use crate::ui::{
    print_banner, print_heading, print_label_value, print_next_action, print_warning_line,
    status_label, status_marker,
};

pub(crate) async fn cmd_overview(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let config_path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    let config_exists = config_path.exists();
    let linked_clients = linked_client_targets();
    let (live_clients, live_client_support) = fetch_live_clients().await;
    let live_client_count = live_clients.len();

    if matches!(output, OutputFormat::Json) {
        let daemon_running = crate::daemon::connect_to_daemon().await.is_some();
        let config = if config_exists {
            plug_core::config::load_config(Some(&config_path)).ok()
        } else {
            None
        };
        let server_count = config.as_ref().map(|c| c.servers.len()).unwrap_or(0);
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "command": "overview",
                "config_exists": config_exists,
                "config_path": config_path,
                "daemon_running": daemon_running,
                "server_count": server_count,
                "linked_clients": linked_clients,
                "live_client_count": live_client_count,
                "live_client_support": live_client_support,
                "next_actions": if !config_exists {
                    vec!["plug setup"]
                } else if linked_clients.is_empty() {
                    vec!["plug link", "plug status"]
                } else if daemon_running {
                    vec!["plug status", "plug doctor"]
                } else {
                    vec!["plug status", "plug doctor", "plug repair"]
                }
            }))?
        );
        return Ok(());
    }

    print_banner("◆", "plug", "MCP multiplexer");

    if !config_exists {
        print_heading("Overview");
        print_label_value("Config", style("not found").yellow().bold());
        print_label_value("Path", style(config_path.display()).dim());
        println!();
        print_heading("Next");
        print_next_action(1, "plug setup", "Create config and link clients");
        print_next_action(2, "plug status", "Check runtime health once configured");
        return Ok(());
    }

    let config = plug_core::config::load_config(Some(&config_path))?;
    let daemon_running = crate::daemon::connect_to_daemon().await.is_some();

    print_heading("Overview");
    print_label_value("Path", style(config_path.display()).dim());
    print_label_value("Servers", style(config.servers.len()).bold());
    print_label_value("Clients", style(linked_clients.len()).bold());
    match live_client_support {
        LiveClientSupport::Supported => {
            print_label_value("Live", style(live_client_count).bold());
        }
        LiveClientSupport::DaemonRestartRequired => {
            print_label_value("Live", style("restart required").yellow().bold());
        }
    }
    print_label_value(
        "Service",
        if daemon_running {
            style("running").green().bold()
        } else {
            style("stopped").yellow().bold()
        },
    );

    if !linked_clients.is_empty() {
        print_label_value("Linked", linked_clients.join(", "));
    }

    if matches!(
        live_client_support,
        LiveClientSupport::DaemonRestartRequired
    ) {
        println!();
        print_warning_line(
            "Live client inspection requires restarting the background daemon after this upgrade.",
        );
    }

    println!();
    print_heading("Next");
    if linked_clients.is_empty() {
        print_next_action(1, "plug link", "Link plug to your AI clients");
        print_next_action(2, "plug status", "Check runtime health");
    } else if daemon_running {
        print_next_action(1, "plug status", "Inspect runtime health");
        print_next_action(2, "plug doctor", "Diagnose configuration issues");
    } else {
        print_next_action(1, "plug status", "Start and inspect the service");
        print_next_action(2, "plug repair", "Refresh linked client configs");
    }

    Ok(())
}

pub(crate) async fn cmd_status(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let started =
        ensure_daemon_with_feedback(config_path, matches!(output, OutputFormat::Text)).await?;

    if let Ok(plug_core::ipc::IpcResponse::Status {
        servers,
        clients,
        uptime_secs,
    }) = crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await
    {
        if matches!(output, OutputFormat::Text) {
            print_banner("◆", "Runtime", "Live service health");
            if started {
                println!();
            }
            print_heading("Service");
            print_label_value("Status", style("running").green().bold());
            print_label_value("Uptime", style(format!("{uptime_secs}s")).bold());
            print_label_value("Clients", style(clients.to_string()).bold());
            println!();
            if servers.is_empty() {
                print_heading("Servers");
                println!("  No servers configured.");
            } else {
                print_heading("Servers");
                println!(
                    "  {:<2} {:<18} {:<12} {:>5}",
                    style("").dim(),
                    style("SERVER").dim(),
                    style("STATUS").dim(),
                    style("TOOLS").dim()
                );
                println!(
                    "  {}",
                    style("------------------------------------------------").dim()
                );
                for s in &servers {
                    if s.server_id == "__plug_internal__" {
                        continue;
                    }
                    println!(
                        "  {} {:<18} {:<12} {:>5}",
                        status_marker(&s.health),
                        s.server_id,
                        status_label(&s.health),
                        s.tool_count
                    );
                }
            }
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({ "uptime": uptime_secs, "clients": clients, "servers": servers })
                )?
            );
        }
        return Ok(());
    }

    let config = plug_core::config::load_config(config_path)?;
    if matches!(output, OutputFormat::Text) {
        print_banner(
            "◆",
            "Runtime unavailable",
            "Service is not currently reachable",
        );
        println!();
        print_heading("Configured servers");
        let mut names: Vec<_> = config.servers.keys().collect();
        names.sort();
        for n in names {
            println!(
                "  {} {:<18} {}",
                style("·").dim(),
                n,
                style("not running").dim()
            );
        }
    }
    Ok(())
}
