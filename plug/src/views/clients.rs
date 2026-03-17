use dialoguer::Select;
use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::clients::{client_views, cmd_link, cmd_unlink};
use crate::runtime::{LiveClientSupport, ensure_daemon_with_feedback, fetch_live_clients};
use crate::ui::{
    can_prompt_interactively, cli_prompt_theme, print_banner, print_heading, print_label_value,
    print_warning_line,
};

fn prompt_client_actions() -> anyhow::Result<bool> {
    let options = ["Done", "Link clients", "Unlink clients"];
    let selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Choose action")
        .items(options)
        .default(0)
        .interact_opt()?;

    match selection {
        Some(1) => {
            cmd_link(Vec::new(), false, false)?;
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
    let mut started =
        ensure_daemon_with_feedback(config_path, matches!(output, OutputFormat::Text)).await?;

    loop {
        let (live, live_client_support) = fetch_live_clients().await;
        let clients = client_views(&live);

        if matches!(output, OutputFormat::Json) {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "clients": clients,
                    "live_client_support": live_client_support,
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
        }
        let linked_count = clients.iter().filter(|client| client.linked).count();
        let detected_count = clients.iter().filter(|client| client.detected).count();
        let live_count = clients.iter().filter(|client| client.live).count();
        print_heading("Summary");
        print_label_value("Linked", style(linked_count).green().bold());
        print_label_value("Detected", style(detected_count).cyan().bold());
        match live_client_support {
            LiveClientSupport::Supported => {
                print_label_value("Live", style(live_count).bold());
            }
            LiveClientSupport::DaemonRestartRequired => {
                print_label_value("Live", style("restart required").yellow().bold());
            }
        }
        println!();
        print_heading("Inventory");
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
