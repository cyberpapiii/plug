use dialoguer::Select;
use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::servers::{
    cmd_server_add, cmd_server_edit, cmd_server_remove, cmd_server_set_enabled,
};
use crate::runtime::ensure_daemon_with_feedback;
use crate::ui::{
    can_prompt_interactively, cli_prompt_theme, print_banner, print_heading, print_info_line,
    print_label_value, status_label, status_marker,
};

async fn prompt_server_actions(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<bool> {
    let options = [
        "Done",
        "Add server",
        "Edit server",
        "Remove server",
        "Enable server",
        "Disable server",
    ];
    let selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Choose action")
        .items(options)
        .default(0)
        .interact_opt()?;

    match selection {
        Some(1) => {
            cmd_server_add(config_path, None, None, None, Vec::new(), None, false)?;
            Ok(true)
        }
        Some(2) => {
            cmd_server_edit(config_path, None, output).await?;
            Ok(true)
        }
        Some(3) => {
            cmd_server_remove(config_path, None, false)?;
            Ok(true)
        }
        Some(4) => {
            cmd_server_set_enabled(config_path, None, true)?;
            Ok(true)
        }
        Some(5) => {
            cmd_server_set_enabled(config_path, None, false)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) async fn cmd_server_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let interactive = matches!(output, OutputFormat::Text) && can_prompt_interactively();
    let mut started =
        ensure_daemon_with_feedback(config_path, matches!(output, OutputFormat::Text)).await?;

    loop {
        if let Ok(plug_core::ipc::IpcResponse::Status { servers, .. }) =
            crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await
        {
            match output {
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&servers)?);
                    return Ok(());
                }
                OutputFormat::Text => {
                    if servers.is_empty() {
                        print_banner("◆", "Servers", "No servers configured");
                        println!();
                        print_info_line("Use `Add server` below to create your first upstream.");
                    } else {
                        let mut healthy = 0usize;
                        let mut degraded = 0usize;
                        let mut failed = 0usize;
                        for server in &servers {
                            if server.server_id == "__plug_internal__" {
                                continue;
                            }
                            match server.health {
                                plug_core::types::ServerHealth::Healthy => healthy += 1,
                                plug_core::types::ServerHealth::Degraded => degraded += 1,
                                plug_core::types::ServerHealth::Failed => failed += 1,
                                plug_core::types::ServerHealth::AuthRequired => failed += 1,
                            }
                        }
                        print_banner(
                            "◆",
                            "Servers",
                            &format!(
                                "{} server(s) active",
                                servers
                                    .iter()
                                    .filter(|server| server.server_id != "__plug_internal__")
                                    .count()
                            ),
                        );
                        if started {
                            println!();
                        }
                        print_heading("Summary");
                        print_label_value("Healthy", style(healthy).green().bold());
                        print_label_value("Degraded", style(degraded).yellow().bold());
                        print_label_value("Failed", style(failed).red().bold());
                        println!();
                        print_heading("Inventory");
                        for s in servers {
                            if s.server_id == "__plug_internal__" {
                                continue;
                            }
                            println!(
                                "  {} {:<18} {} ({} tools)",
                                status_marker(&s.health),
                                style(&s.server_id).bold(),
                                status_label(&s.health),
                                s.tool_count
                            );
                        }
                    }
                }
            }
        } else {
            let config = plug_core::config::load_config(config_path)?;
            if matches!(output, OutputFormat::Text) {
                let mut names: Vec<_> = config.servers.keys().collect();
                names.sort();
                print_banner(
                    "◆",
                    "Servers",
                    &format!("{} server(s) configured (daemon not running)", names.len()),
                );
                print_heading("Inventory");
                for name in names {
                    let enabled = config
                        .servers
                        .get(name)
                        .map(|server| server.enabled)
                        .unwrap_or(true);
                    println!(
                        "  {} {:<18} {}",
                        if enabled {
                            style("·").dim()
                        } else {
                            style("!").yellow().bold()
                        },
                        style(name).bold(),
                        if enabled {
                            style("configured").dim()
                        } else {
                            style("disabled").yellow()
                        }
                    );
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&config.servers)?);
                return Ok(());
            }
        }

        if !interactive {
            break;
        }
        println!();
        if !prompt_server_actions(config_path, output).await? {
            break;
        }
        println!();
        started = false;
    }
    Ok(())
}
