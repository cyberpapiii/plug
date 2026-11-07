use dialoguer::console::style;

use crate::ipc_client::DaemonQueryAvailability;
use crate::ui::{
    print_banner, print_heading, print_info_line, print_label_value, print_reason, status_label,
    status_marker, summarize_server_auth, summarize_server_target, summarize_server_transport,
};

/// `plug servers` as JSON while the daemon answers.
pub(crate) fn live_servers_json(
    servers: Vec<plug_core::types::ServerStatus>,
    config: Option<&plug_core::config::Config>,
    availability: DaemonQueryAvailability,
) -> serde_json::Value {
    let servers = servers
        .into_iter()
        .map(|server| {
            let server_cfg = config.and_then(|cfg| cfg.servers.get(&server.server_id));
            serde_json::json!({
                "server_id": server.server_id,
                "health": server.health,
                "tool_count": server.tool_count,
                "auth_status": server.auth_status,
                "upstream": server.upstream,
                "selected_protocol_era": server.selected_protocol_era,
                "selected_protocol_version": server.selected_protocol_version,
                "error": server.error,
                "source": server_cfg.map(plug_core::ipc::IpcServerSourceInfo::from_config),
                "trust": plug_core::ipc::IpcTrustInfo::for_server(&server.server_id, server_cfg),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "runtime_available": true,
        "status_source": super::status_source(availability),
        "servers": servers,
    })
}

/// `plug servers` as text while the daemon answers.
pub(crate) fn print_live_servers(
    servers: &[plug_core::types::ServerStatus],
    config: Option<&plug_core::config::Config>,
) {
    if servers.is_empty() {
        print_banner("◆", "Servers", "No servers yet");
        println!();
        print_info_line("Use `Add server` below to add your first one.");
    } else {
        let mut healthy = 0usize;
        let mut degraded = 0usize;
        let mut failed = 0usize;
        let mut auth_required = 0usize;
        for server in servers {
            if server.server_id == plug_core::tool_naming::INTERNAL_SERVER_ID {
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
                "{} on",
                servers
                    .iter()
                    .filter(|server| server.server_id != plug_core::tool_naming::INTERNAL_SERVER_ID)
                    .count()
            ),
        );
        print_heading("Summary");
        print_label_value("Running", style(healthy).green().bold());
        print_label_value("Unsteady", style(degraded).yellow().bold());
        print_label_value("Down", style(failed).red().bold());
        print_label_value("Sign-in needed", style(auth_required).yellow().bold());
        println!();
        print_heading("Servers");
        for s in servers {
            if s.server_id == plug_core::tool_naming::INTERNAL_SERVER_ID {
                continue;
            }
            let server_cfg = config
                .as_ref()
                .and_then(|cfg| cfg.servers.get(&s.server_id));
            let place = crate::ui::server_place(server_cfg);
            let target = summarize_server_target(server_cfg, 28);
            let protocol = protocol_label(
                s.selected_protocol_era,
                s.selected_protocol_version.as_deref(),
            );
            println!(
                "  {} {:<18} {:<14} {:<16} {:<28} ({} tools; {})",
                status_marker(&s.health),
                style(&s.server_id).bold(),
                status_label(&s.health),
                place,
                target,
                s.tool_count,
                protocol
            );
            print_reason(s);
        }
        let auth_required_servers = servers
            .iter()
            .filter(|s| s.server_id != plug_core::tool_naming::INTERNAL_SERVER_ID)
            .filter(|s| matches!(s.health, plug_core::types::ServerHealth::AuthRequired))
            .map(|s| s.server_id.clone())
            .collect::<Vec<_>>();
        let failed_servers = servers
            .iter()
            .filter(|s| s.server_id != plug_core::tool_naming::INTERNAL_SERVER_ID)
            .filter(|s| matches!(s.health, plug_core::types::ServerHealth::Failed))
            .map(|s| s.server_id.clone())
            .collect::<Vec<_>>();
        let degraded_servers = servers
            .iter()
            .filter(|s| s.server_id != plug_core::tool_naming::INTERNAL_SERVER_ID)
            .filter(|s| matches!(s.health, plug_core::types::ServerHealth::Degraded))
            .map(|s| s.server_id.clone())
            .collect::<Vec<_>>();
        if !auth_required_servers.is_empty()
            || !failed_servers.is_empty()
            || !degraded_servers.is_empty()
        {
            println!();
            print_heading("Needs you");
            if !auth_required_servers.is_empty() {
                for name in &auth_required_servers {
                    print_info_line(format!(
                        "{name} needs a sign-in. Run `plug auth login --server {name}`."
                    ));
                }
            }
            if !failed_servers.is_empty() {
                for name in &failed_servers {
                    print_info_line(format!(
                        "{name} is down. The reason is under its name; Plug keeps trying."
                    ));
                }
            }
            if !degraded_servers.is_empty() {
                for name in &degraded_servers {
                    print_info_line(format!(
                        "{name} is unsteady: some calls fail. Run `plug doctor`."
                    ));
                }
            }
        }
    }
}

/// `plug servers` as text from the config alone.
pub(crate) fn print_configured_servers(
    config: &plug_core::config::Config,
    availability: DaemonQueryAvailability,
    daemon_starting: bool,
) {
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
            } else if daemon_starting {
                "daemon is starting"
            } else {
                "daemon not running"
            }
        ),
    );
    print_heading("Servers");
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
}

/// `plug servers` as JSON from the config alone.
pub(crate) fn configured_servers_json(
    config: &plug_core::config::Config,
    availability: DaemonQueryAvailability,
) -> serde_json::Value {
    let server_inventory = config
        .servers
        .iter()
        .map(|(server_id, server)| {
            serde_json::json!({
                "server_id": server_id,
                "config": server,
                "source": plug_core::ipc::IpcServerSourceInfo::from_config(server),
                "trust": plug_core::ipc::IpcTrustInfo::for_server(server_id, Some(server)),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "runtime_available": false,
        "status_source": super::status_source(availability),
        "daemon_running": availability.daemon_reachable(),
        "servers": config.servers,
        "server_inventory": server_inventory,
    })
}

/// The MCP version a server negotiated. The 2025 era is the normal state, so
/// only the newer stateless era earns a word in front of the version.
fn protocol_label(era: Option<plug_core::protocol::ProtocolEra>, version: Option<&str>) -> String {
    match (era, version) {
        (Some(plug_core::protocol::ProtocolEra::Modern), Some(version)) => {
            format!("modern {version}")
        }
        (Some(plug_core::protocol::ProtocolEra::Legacy), Some(version)) => version.to_string(),
        _ => "not connected".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::protocol_label;
    use plug_core::protocol::ProtocolEra;

    #[test]
    fn protocol_label_names_only_the_modern_era() {
        assert_eq!(
            protocol_label(Some(ProtocolEra::Legacy), Some("2025-11-25")),
            "2025-11-25"
        );
        assert_eq!(
            protocol_label(Some(ProtocolEra::Modern), Some("2026-07-28")),
            "modern 2026-07-28"
        );
        assert_eq!(protocol_label(None, None), "not connected");
    }
}
