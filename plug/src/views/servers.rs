use dialoguer::Select;
use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::servers::{
    cmd_server_add, cmd_server_edit, cmd_server_remove, cmd_server_set_enabled,
};
use crate::runtime::daemon_query;
use crate::ui::{
    can_prompt_interactively, cli_prompt_theme, print_banner, print_heading, print_info_line,
    print_label_value, status_label, status_marker, summarize_server_auth, summarize_server_target,
    summarize_server_transport,
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
            cmd_server_add(
                config_path,
                None,
                None,
                None,
                Vec::new(),
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                false,
            )?;
            Ok(true)
        }
        Some(2) => {
            cmd_server_edit(
                config_path,
                None,
                None,
                None,
                None,
                Vec::new(),
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                output,
            )
            .await?;
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
    let mut started = false;

    loop {
        let (availability, live_status) = daemon_query(
            &plug_core::ipc::IpcRequest::Status,
            |response| match response {
                plug_core::ipc::IpcResponse::Status { servers, .. } => Some(servers),
                _ => None,
            },
        )
        .await;
        if let Some(servers) = live_status {
            match output {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "runtime_available": true,
                            "status_source": "live_daemon",
                            "servers": servers,
                        }))?
                    );
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
                        let mut auth_required = 0usize;
                        let config = plug_core::config::load_config(config_path).ok();
                        for server in &servers {
                            if server.server_id == "__plug_internal__" {
                                continue;
                            }
                            match server.health {
                                plug_core::types::ServerHealth::Healthy => healthy += 1,
                                plug_core::types::ServerHealth::Degraded => degraded += 1,
                                plug_core::types::ServerHealth::Failed => failed += 1,
                                plug_core::types::ServerHealth::AuthRequired => auth_required += 1,
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
                        print_label_value("Auth Required", style(auth_required).yellow().bold());
                        println!();
                        print_heading("Inventory");
                        for s in &servers {
                            if s.server_id == "__plug_internal__" {
                                continue;
                            }
                            let server_cfg = config
                                .as_ref()
                                .and_then(|cfg| cfg.servers.get(&s.server_id));
                            let transport = summarize_server_transport(server_cfg);
                            let auth = summarize_server_auth(server_cfg);
                            let target = summarize_server_target(server_cfg, 28);
                            println!(
                                "  {} {:<18} {:<12} {:<8} {:<6} {:<28} ({} tools)",
                                status_marker(&s.health),
                                style(&s.server_id).bold(),
                                status_label(&s.health),
                                transport,
                                auth,
                                target,
                                s.tool_count
                            );
                        }
                        let auth_required_servers = servers
                            .iter()
                            .filter(|s| s.server_id != "__plug_internal__")
                            .filter(|s| {
                                matches!(s.health, plug_core::types::ServerHealth::AuthRequired)
                            })
                            .map(|s| s.server_id.clone())
                            .collect::<Vec<_>>();
                        let failed_servers = servers
                            .iter()
                            .filter(|s| s.server_id != "__plug_internal__")
                            .filter(|s| matches!(s.health, plug_core::types::ServerHealth::Failed))
                            .map(|s| s.server_id.clone())
                            .collect::<Vec<_>>();
                        let degraded_servers = servers
                            .iter()
                            .filter(|s| s.server_id != "__plug_internal__")
                            .filter(|s| {
                                matches!(s.health, plug_core::types::ServerHealth::Degraded)
                            })
                            .map(|s| s.server_id.clone())
                            .collect::<Vec<_>>();
                        if !auth_required_servers.is_empty()
                            || !failed_servers.is_empty()
                            || !degraded_servers.is_empty()
                        {
                            println!();
                            print_heading("Recovery");
                            if !auth_required_servers.is_empty() {
                                print_label_value(
                                    "Auth",
                                    format!(
                                        "{} need re-auth — run `plug auth status` or `plug auth login --server <name>`",
                                        auth_required_servers.join(", ")
                                    ),
                                );
                            }
                            if !failed_servers.is_empty() {
                                print_label_value(
                                    "Failed",
                                    format!(
                                        "{} failed — run `plug doctor` to inspect connectivity and runtime context",
                                        failed_servers.join(", ")
                                    ),
                                );
                            }
                            if !degraded_servers.is_empty() {
                                print_label_value(
                                    "Degraded",
                                    format!(
                                        "{} are degraded — compare `plug status` and `plug doctor` for runtime/auth details",
                                        degraded_servers.join(", ")
                                    ),
                                );
                            }
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
                    &format!(
                        "{} server(s) configured ({})",
                        names.len(),
                        if availability.daemon_reachable() {
                            "runtime inspection failed"
                        } else {
                            "daemon not running"
                        }
                    ),
                );
                print_heading("Inventory");
                println!(
                    "  {:<18} {:<8} {:<6} {:<40} {}",
                    style("SERVER").dim(),
                    style("UPSTREAM").dim(),
                    style("AUTH").dim(),
                    style("TARGET").dim(),
                    style("STATE").dim()
                );
                println!(
                    "  {}",
                    style("---------------------------------------------------------------------------------------").dim()
                );
                for name in names {
                    let server = config.servers.get(name);
                    let enabled = server.map(|server| server.enabled).unwrap_or(true);
                    let target = summarize_server_target(server, 40);
                    println!(
                        "  {} {:<18} {:<8} {:<6} {:<40} {}",
                        if enabled {
                            style("·").dim()
                        } else {
                            style("!").yellow().bold()
                        },
                        style(name).bold(),
                        summarize_server_transport(server),
                        summarize_server_auth(server),
                        style(target).dim(),
                        if enabled {
                            style("configured").dim()
                        } else {
                            style("disabled").yellow()
                        }
                    );
                }
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "runtime_available": false,
                        "status_source": if availability.daemon_reachable() {
                            "ipc_unavailable"
                        } else {
                            "config_only"
                        },
                        "daemon_running": availability.daemon_reachable(),
                        "servers": config.servers,
                    }))?
                );
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
