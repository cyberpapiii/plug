use dialoguer::console::style;
use plug_core::clients::LinkTransport;

use crate::OutputFormat;
use crate::commands::clients::{link_transport, linked_clients, own_http};
use crate::ipc_client::{self, daemon_running};
use crate::ui::{
    print_banner, print_heading, print_info_line, print_label_value, print_next_action,
    print_reason, print_warning_line, status_label, status_marker, summarize_server_auth,
    summarize_server_target, summarize_server_transport,
};
use crate::views::live::{self, fetch_live_sessions};

/// Each linked client's slug and the transport its link uses.
fn linked_topology(config: Option<&plug_core::config::Config>) -> Vec<(String, LinkTransport)> {
    linked_clients(&own_http(config))
        .into_iter()
        .map(|(target, entry)| (target.slug().to_string(), link_transport(&entry)))
        .collect()
}

fn linked_client_transport_summary(linked_clients: &[(String, LinkTransport)]) -> String {
    let count = |kind: LinkTransport| {
        linked_clients
            .iter()
            .filter(|(_, transport)| *transport == kind)
            .count()
    };
    format!(
        "stdio={} http={}",
        count(LinkTransport::Stdio),
        count(LinkTransport::Http)
    )
}

fn owner_approval_state(
    config: &plug_core::config::Config,
) -> Option<plug_core::downstream_oauth::OwnerEnrollmentStatus> {
    plug_core::downstream_oauth::DownstreamOauthConfig::from_http_config(&config.http)
        .map(|oauth| plug_core::downstream_oauth::inspect_owner_enrollment(&oauth))
}

fn owner_approval_summary(
    status: Option<plug_core::downstream_oauth::OwnerEnrollmentStatus>,
) -> Option<String> {
    match status {
        None => None,
        Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::NotEnrolled) => {
            Some("setup required".to_string())
        }
        Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::Enrolled { credential_count }) => {
            Some(format!("enrolled ({credential_count} credentials)"))
        }
        Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::UnsafePermissions) => {
            Some("unsafe state permissions".to_string())
        }
        Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::StateUnavailable) => {
            Some("state unavailable".to_string())
        }
    }
}

fn owner_approval_json(
    status: Option<plug_core::downstream_oauth::OwnerEnrollmentStatus>,
) -> serde_json::Value {
    match status {
        None => serde_json::Value::Null,
        Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::NotEnrolled) => {
            serde_json::json!({"status": "setup_required", "credential_count": 0})
        }
        Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::Enrolled { credential_count }) => {
            serde_json::json!({
                "status": "enrolled",
                "credential_count": credential_count,
            })
        }
        Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::UnsafePermissions) => {
            serde_json::json!({"status": "unsafe_state_permissions", "credential_count": null})
        }
        Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::StateUnavailable) => {
            serde_json::json!({"status": "state_unavailable", "credential_count": null})
        }
    }
}

/// What to run next, each with why. Text and JSON both show this list.
fn overview_next_actions(
    config_exists: bool,
    linked_clients_empty: bool,
    daemon_running: bool,
    owner_approval: Option<plug_core::downstream_oauth::OwnerEnrollmentStatus>,
) -> Vec<(&'static str, &'static str)> {
    use plug_core::downstream_oauth::OwnerEnrollmentStatus;
    const DOCTOR: (&str, &str) = ("plug doctor", "Diagnose configuration issues");
    if !config_exists {
        return vec![("plug setup", "Create config and link clients")];
    }
    match owner_approval {
        Some(OwnerEnrollmentStatus::NotEnrolled) => vec![
            (
                "plug auth owner enroll",
                "Finish owner setup before connecting",
            ),
            ("plug doctor", "Verify connection prerequisites"),
        ],
        Some(OwnerEnrollmentStatus::StateUnavailable) => vec![
            ("plug doctor", "Inspect owner state safely"),
            (
                "plug auth owner list",
                "Confirm whether owner passkeys can be read",
            ),
        ],
        Some(OwnerEnrollmentStatus::UnsafePermissions) => {
            vec![("plug doctor", "Inspect owner state safely")]
        }
        _ if linked_clients_empty => vec![
            ("plug link", "Link plug to your AI clients"),
            ("plug status", "Check runtime health"),
        ],
        _ if daemon_running => vec![("plug status", "Inspect runtime health"), DOCTOR],
        _ => vec![
            ("plug status", "Start and inspect the service"),
            DOCTOR,
            ("plug repair", "Refresh linked client configs"),
        ],
    }
}

fn print_next_actions(actions: &[(&str, &str)]) {
    print_heading("Next");
    for (index, (command, why)) in actions.iter().enumerate() {
        print_next_action(index + 1, command, why);
    }
}

#[allow(clippy::too_many_arguments)]
fn overview_json(
    config_exists: bool,
    config_path: &std::path::Path,
    daemon_running: bool,
    server_count: usize,
    linked_clients: &[String],
    inventory: &live::LiveInventory,
    owner_approval: Option<plug_core::downstream_oauth::OwnerEnrollmentStatus>,
    next_actions: &[(&str, &str)],
) -> serde_json::Value {
    serde_json::json!({
        "command": "overview",
        "config_exists": config_exists,
        "config_path": config_path,
        "daemon_running": daemon_running,
        "server_count": server_count,
        "linked_client_count": linked_clients.len(),
        "linked_clients": linked_clients,
        "live_session_count": inventory.session_count,
        "live_session_transports": inventory.session_transports,
        "owner_approval": owner_approval_json(owner_approval),
        "next_actions": next_actions.iter().map(|(command, _)| command).collect::<Vec<_>>(),
    })
}

#[allow(clippy::too_many_arguments)]
fn status_json(
    uptime_secs: u64,
    daemon_proxy_client_count: usize,
    inventory: &live::LiveInventory,
    servers: &[plug_core::types::ServerStatus],
    runtime_available: bool,
    status_source: &str,
    daemon_running: bool,
    resource_subscriptions: usize,
) -> serde_json::Value {
    serde_json::json!({
        "uptime": uptime_secs,
        "clients": daemon_proxy_client_count,
        "daemon_proxy_client_count": daemon_proxy_client_count,
        "live_session_count": inventory.session_count,
        "live_session_transports": inventory.session_transports,
        "servers": servers,
        "runtime_available": runtime_available,
        "status_source": status_source,
        "daemon_running": daemon_running,
        "resource_subscriptions": resource_subscriptions
    })
}

pub(crate) async fn cmd_overview(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let config_path = config_path
        .cloned()
        .unwrap_or_else(plug_core::paths::default_config_path);
    let config_exists = config_path.exists();
    let loaded = if config_exists {
        plug_core::config::load_config(Some(&config_path)).ok()
    } else {
        None
    };
    let linked = linked_topology(loaded.as_ref());
    let linked_clients: Vec<String> = linked.iter().map(|(target, _)| target.clone()).collect();
    let live_clients = fetch_live_sessions().await;
    let live_client_count = live_clients.len();
    let inventory = live::inventory(&live_clients);

    if matches!(output, OutputFormat::Json) {
        let daemon_running = crate::ipc_client::daemon_running().await;
        let config = loaded;
        let server_count = config.as_ref().map(|c| c.servers.len()).unwrap_or(0);
        let owner_approval = config.as_ref().and_then(owner_approval_state);
        let next_actions = overview_next_actions(
            config_exists,
            linked_clients.is_empty(),
            daemon_running,
            owner_approval,
        );
        println!(
            "{}",
            serde_json::to_string_pretty(&overview_json(
                config_exists,
                &config_path,
                daemon_running,
                server_count,
                &linked_clients,
                &inventory,
                owner_approval,
                &next_actions,
            ))?
        );
        return Ok(());
    }

    print_banner("◆", "plug", "MCP gateway");

    if !config_exists {
        print_heading("Overview");
        print_label_value("Config", style("not found").yellow().bold());
        print_label_value("Path", style(config_path.display()).dim());
        println!();
        print_next_actions(&overview_next_actions(false, true, false, None));
        return Ok(());
    }

    let config = plug_core::config::load_config(Some(&config_path))?;
    let daemon_running = crate::ipc_client::daemon_running().await;
    let transport_counts =
        config
            .servers
            .values()
            .fold((0usize, 0usize, 0usize), |mut acc, server| {
                match server.transport {
                    plug_core::config::TransportType::Stdio => acc.0 += 1,
                    plug_core::config::TransportType::Http
                    | plug_core::config::TransportType::OpenApi => acc.1 += 1,
                    plug_core::config::TransportType::Sse => acc.2 += 1,
                }
                acc
            });
    let downstream_auth_mode = config.http.auth_mode.label();
    let owner_approval = owner_approval_state(&config);
    let downstream_http_endpoint = config.http.mcp_url();
    let downstream_auth_summary = match config.http.auth_mode {
        plug_core::config::DownstreamAuthMode::Auto => {
            if config.http.requires_bearer() {
                "auto (reachable off this Mac => bearer)"
            } else {
                "auto (loopback => no auth)"
            }
        }
        plug_core::config::DownstreamAuthMode::None => "explicit none",
        plug_core::config::DownstreamAuthMode::Bearer => "explicit bearer",
        plug_core::config::DownstreamAuthMode::Oauth => "oauth (authorization-code + PKCE)",
    };

    print_heading("Overview");
    print_label_value("Path", style(config_path.display()).dim());
    print_label_value("Servers", style(config.servers.len()).bold());
    print_label_value(
        "Upstreams",
        format!(
            "stdio={} http={} sse={}",
            transport_counts.0, transport_counts.1, transport_counts.2
        ),
    );
    print_label_value("Linked Clients", style(linked_clients.len()).bold());
    print_label_value("Live", style(live_client_count).bold());
    print_label_value("Live Transports", inventory.session_transports.summary());
    print_label_value(
        "Service",
        if daemon_running {
            style("running").green().bold()
        } else if crate::ipc_client::daemon_starting() {
            style("starting").yellow().bold()
        } else {
            style("stopped").yellow().bold()
        },
    );
    print_label_value(
        "Downstream Auth",
        if downstream_auth_mode == "auto" {
            style(downstream_auth_summary).bold()
        } else {
            style(downstream_auth_summary).yellow().bold()
        },
    );
    print_label_value("HTTP Endpoint", downstream_http_endpoint);
    if let Some(public_base_url) = &config.http.public_base_url {
        print_label_value("Public URL", public_base_url);
    }
    if let Some(summary) = owner_approval_summary(owner_approval) {
        let value = match owner_approval {
            Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::Enrolled { .. }) => {
                style(summary).green().bold()
            }
            Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::NotEnrolled) => {
                style(summary).yellow().bold()
            }
            Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::StateUnavailable) => {
                style(summary).red().bold()
            }
            Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::UnsafePermissions) => {
                style(summary).red().bold()
            }
            None => unreachable!("summary exists only for downstream OAuth"),
        };
        print_label_value("Owner approval", value);
    }

    if !linked.is_empty() {
        let linked_descriptions = linked
            .iter()
            .map(|(target, transport)| format!("{target} ({transport})"))
            .collect::<Vec<_>>();
        print_label_value("Linked Topology", linked_client_transport_summary(&linked));
        print_label_value("Linked Clients", linked_descriptions.join(", "));
    }

    println!();
    print_next_actions(&overview_next_actions(
        true,
        linked_clients.is_empty(),
        daemon_running,
        owner_approval,
    ));

    Ok(())
}

pub(crate) async fn cmd_status(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
    show_token: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    let daemon_running = daemon_running().await;

    let config = plug_core::config::load_config(config_path).ok();
    let linked_clients = linked_topology(config.as_ref());
    let downstream_auth_info = config.as_ref().map(|c| {
        let mode = c.http.auth_mode.label();
        let summary = match c.http.auth_mode {
            plug_core::config::DownstreamAuthMode::Auto => {
                if c.http.requires_bearer() {
                    "reachable off this Mac => bearer".to_string()
                } else {
                    "loopback => no auth".to_string()
                }
            }
            plug_core::config::DownstreamAuthMode::None => "explicit none".to_string(),
            plug_core::config::DownstreamAuthMode::Bearer => "explicit bearer".to_string(),
            plug_core::config::DownstreamAuthMode::Oauth => "authorization-code + PKCE".to_string(),
        };
        let endpoint = c.http.mcp_url();
        (
            mode.to_string(),
            summary,
            c.http.public_base_url.clone(),
            endpoint,
        )
    });
    let http_auth_info = config.as_ref().and_then(|c| {
        if !c.http.requires_bearer() {
            return None;
        }

        let token_path = plug_core::auth::http_auth_token_path(c.http.port);
        let token = if show_token {
            std::fs::read_to_string(&token_path)
                .ok()
                .map(|t| t.trim().to_string())
        } else {
            None
        };
        Some((token_path.exists(), token))
    });

    let (status_availability, live_status) = ipc_client::query(
        plug_core::ipc::IpcRequest::Status,
        |response| match response {
            plug_core::ipc::IpcResponse::Status {
                servers,
                clients,
                uptime_secs,
                runtime_version,
                resource_subscriptions,
            } => Some((
                servers,
                clients,
                uptime_secs,
                runtime_version,
                resource_subscriptions,
            )),
            _ => None,
        },
    )
    .await;

    if let Some((servers, clients, uptime_secs, runtime_version, resource_subscriptions)) =
        live_status
    {
        let live_sessions = fetch_live_sessions().await;
        let inventory = live::inventory(&live_sessions);
        if matches!(output, OutputFormat::Text) {
            print_banner("◆", "Plug", "Plug is running");
            print_heading("Plug");
            print_label_value(
                "Status",
                style(format!(
                    "Running for {}",
                    crate::ui::format_duration(uptime_secs)
                ))
                .green()
                .bold(),
            );
            print_label_value(
                "Clients",
                format!(
                    "{} connected now, {} set up",
                    inventory.session_count,
                    linked_clients.len()
                ),
            );
            if let Some((_, _, Some(public_base_url), _)) = &downstream_auth_info {
                print_label_value("Remote address", public_base_url);
            }
            if verbose {
                println!();
                print_heading("Detail");
                print_label_value(
                    "Live Sessions",
                    style(inventory.session_count.to_string()).bold(),
                );
                print_label_value("Live Transports", inventory.session_transports.summary());
                // Resource subscriptions are held by the runtime on behalf of
                // downstream clients and are otherwise invisible from the CLI, so
                // there is no way to tell a client that subscribed and went quiet
                // from one that never subscribed at all.
                print_label_value(
                    "Subscriptions",
                    style(resource_subscriptions.to_string()).bold(),
                );
                if !linked_clients.is_empty() {
                    print_label_value("Linked Clients", style(linked_clients.len()).bold());
                    print_label_value(
                        "Linked Topology",
                        linked_client_transport_summary(&linked_clients),
                    );
                    let linked_summary = linked_clients
                        .iter()
                        .map(|(target, transport)| format!("{target} ({transport})"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    print_label_value("Linked Detail", linked_summary);
                }
                if let Some((mode, summary, public_base_url, endpoint)) = &downstream_auth_info {
                    print_label_value(
                        "Downstream Auth",
                        if mode == "auto" {
                            style(format!("{mode} ({summary})")).bold()
                        } else {
                            style(format!("{mode} ({summary})")).yellow().bold()
                        },
                    );
                    print_label_value("HTTP Endpoint", endpoint);
                    if let Some(public_base_url) = public_base_url {
                        print_label_value("Public URL", public_base_url);
                    }
                }

                if let Some((token_exists, token)) = &http_auth_info {
                    match (*token_exists, token) {
                        (true, Some(t)) => print_label_value(
                            "HTTP Auth",
                            style(format!("enabled | Token: {t}")).green().bold(),
                        ),
                        (true, None) => print_label_value(
                            "HTTP Auth",
                            style("enabled (use --show-token to reveal)").green().bold(),
                        ),
                        (false, _) => {
                            print_label_value("HTTP Auth", style("NOT CONFIGURED").red().bold())
                        }
                    }
                }
            }

            println!();
            if servers.is_empty() {
                print_heading("Servers");
                println!("  No servers configured.");
            } else {
                let config_by_server = config.as_ref().map(|cfg| &cfg.servers);
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
                print_heading("Servers");
                for s in &servers {
                    if s.server_id == plug_core::tool_naming::INTERNAL_SERVER_ID {
                        continue;
                    }
                    let server_cfg = config_by_server.and_then(|servers| servers.get(&s.server_id));
                    if verbose {
                        println!(
                            "  {} {:<18} {:<14} {:<8} {:<6} {:<28} {:>5}",
                            status_marker(&s.health),
                            s.server_id,
                            status_label(&s.health),
                            summarize_server_transport(server_cfg),
                            summarize_server_auth(server_cfg),
                            summarize_server_target(server_cfg, 28),
                            s.tool_count
                        );
                    } else {
                        println!(
                            "  {} {:<18} {:<14} {:<16} {:>4} tools",
                            status_marker(&s.health),
                            s.server_id,
                            status_label(&s.health),
                            crate::ui::server_place(server_cfg),
                            s.tool_count
                        );
                    }
                    print_reason(s);
                }

                if !auth_required_servers.is_empty() || !failed_servers.is_empty() {
                    println!();
                    print_heading("Needs you");
                    for name in &auth_required_servers {
                        print_info_line(format!(
                            "{name} needs a sign-in. Run `plug auth login --server {name}`."
                        ));
                    }
                    for name in &failed_servers {
                        print_info_line(format!(
                            "{name} is down. The reason is under its name; Plug keeps trying."
                        ));
                    }
                }
                if !verbose {
                    println!();
                    print_info_line("`plug status -v` adds connections and addresses.");
                }
            }
        } else {
            let mut json_obj = status_json(
                uptime_secs,
                clients,
                &inventory,
                &servers,
                true,
                crate::ipc_client::DaemonQueryAvailability::Available.status_source(),
                daemon_running,
                resource_subscriptions,
            );
            json_obj["runtime_version"] = serde_json::json!(runtime_version);
            if !linked_clients.is_empty() {
                json_obj["linked_clients"] = serde_json::json!(
                    linked_clients
                        .iter()
                        .map(|(target, transport)| serde_json::json!({
                            "target": target,
                            "transport": transport,
                        }))
                        .collect::<Vec<_>>()
                );
            }
            if let Some((mode, summary, public_base_url, endpoint)) = &downstream_auth_info {
                json_obj["downstream_auth"] = serde_json::json!({
                    "mode": mode,
                    "summary": summary,
                    "public_base_url": public_base_url,
                    "http_endpoint": endpoint,
                });
            }
            if let Some((token_exists, token)) = &http_auth_info {
                json_obj["http_auth"] = serde_json::json!({
                    "enabled": *token_exists,
                    "token": token,
                });
            }
            println!("{}", serde_json::to_string_pretty(&json_obj)?);
        }
        return Ok(());
    }

    let config = plug_core::config::load_config(config_path)?;
    if matches!(output, OutputFormat::Text) {
        print_banner("◆", "Plug", "Plug is not running");
        println!();
        if status_availability.daemon_reachable() {
            print_warning_line(
                "Plug answered but could not say how its servers are doing. Run `plug doctor`.",
            );
        } else {
            print_warning_line(
                "Open Plug.app or run `plug start`. The servers below come from the settings file.",
            );
        }
        println!();
        print_heading("Servers in the settings file");
        println!(
            "  {:<18} {:<8} {:<6} {:<28} {}",
            style("SERVER").dim(),
            style("UPSTREAM").dim(),
            style("AUTH").dim(),
            style("TARGET").dim(),
            style("STATE").dim()
        );
        println!(
            "  {}",
            style(
                "-------------------------------------------------------------------------------"
            )
            .dim()
        );
        let mut names: Vec<_> = config.servers.keys().collect();
        names.sort();
        for n in names {
            let server = config.servers.get(n);
            println!(
                "  {} {:<18} {:<8} {:<6} {:<28} {}",
                if server.map(|server| server.enabled).unwrap_or(true) {
                    style("·").dim()
                } else {
                    style("!").yellow().bold()
                },
                n,
                summarize_server_transport(server),
                summarize_server_auth(server),
                summarize_server_target(server, 28),
                if server.map(|server| server.enabled).unwrap_or(true) {
                    style("on").dim()
                } else {
                    style("off").yellow()
                }
            );
        }
    } else {
        let servers = config
            .servers
            .iter()
            .map(|(server_id, server)| {
                serde_json::json!({
                    "server_id": server_id,
                    "health": serde_json::Value::Null,
                    "tool_count": 0,
                    "auth_status": serde_json::Value::Null,
                    "configured": true,
                    "enabled": server.enabled,
                    "status_source": super::status_source(status_availability),
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "uptime": 0,
                "clients": 0,
                "runtime_available": false,
                "status_source": super::status_source(status_availability),
                "servers": servers,
                "daemon_running": status_availability.daemon_reachable()
            }))?
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{overview_json, overview_next_actions, owner_approval_summary, status_json};
    use crate::views::live::{LiveInventory, LiveSessionTransportCounts};

    #[test]
    fn overview_json_includes_inventory_contract_fields() {
        let inventory = LiveInventory {
            session_count: 2,
            session_transports: LiveSessionTransportCounts {
                daemon_proxy: 1,
                http: 1,
            },
        };
        let linked = vec!["claude".to_string()];
        let json = overview_json(
            true,
            std::path::Path::new("/tmp/config.toml"),
            true,
            3,
            &linked,
            &inventory,
            Some(
                plug_core::downstream_oauth::OwnerEnrollmentStatus::Enrolled {
                    credential_count: 2,
                },
            ),
            &[("plug status", "Inspect runtime health")],
        );
        assert_eq!(json["live_session_count"], 2);
        assert_eq!(json["live_session_transports"]["daemon_proxy"], 1);
        assert_eq!(json["owner_approval"]["status"], "enrolled");
        assert_eq!(json["owner_approval"]["credential_count"], 2);
    }

    #[test]
    fn owner_approval_summary_is_actionable_without_exposing_credentials() {
        assert_eq!(
            owner_approval_summary(Some(
                plug_core::downstream_oauth::OwnerEnrollmentStatus::Enrolled {
                    credential_count: 3,
                }
            )),
            Some("enrolled (3 credentials)".to_string())
        );
        assert_eq!(
            owner_approval_summary(Some(
                plug_core::downstream_oauth::OwnerEnrollmentStatus::NotEnrolled
            )),
            Some("setup required".to_string())
        );
        assert_eq!(owner_approval_summary(None), None);
    }

    #[test]
    fn owner_readiness_blocks_less_useful_overview_actions() {
        assert_eq!(
            overview_next_actions(
                true,
                false,
                true,
                Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::NotEnrolled),
            )
            .iter()
            .map(|(command, _)| *command)
            .collect::<Vec<_>>(),
            vec!["plug auth owner enroll", "plug doctor"]
        );
        assert_eq!(
            overview_next_actions(
                true,
                false,
                true,
                Some(plug_core::downstream_oauth::OwnerEnrollmentStatus::StateUnavailable),
            )
            .iter()
            .map(|(command, _)| *command)
            .collect::<Vec<_>>(),
            vec!["plug doctor", "plug auth owner list"]
        );
    }

    #[test]
    fn status_json_includes_inventory_contract_fields() {
        let inventory = LiveInventory {
            session_count: 1,
            session_transports: LiveSessionTransportCounts {
                daemon_proxy: 0,
                http: 1,
            },
        };
        let servers = vec![plug_core::types::ServerStatus {
            server_id: "notion".to_string(),
            health: plug_core::types::ServerHealth::AuthRequired,
            tool_count: 0,
            auth_status: "required".to_string(),
            upstream: None,
            metrics: None,
            availability: Default::default(),
            selected_protocol_era: None,
            selected_protocol_version: None,
            error: None,
            last_seen: None,
        }];
        let json = status_json(42, 3, &inventory, &servers, true, "live_daemon", true, 4);

        assert_eq!(json["clients"], 3);
        assert_eq!(json["resource_subscriptions"], 4);
        assert_eq!(json["daemon_proxy_client_count"], 3);
        assert_eq!(json["live_session_count"], 1);
        assert_eq!(json["live_session_transports"]["http"], 1);
        assert_eq!(json["runtime_available"], true);
        assert_eq!(json["status_source"], "live_daemon");
    }

    #[test]
    fn status_json_includes_per_upstream_metrics() {
        let inventory = LiveInventory {
            session_count: 0,
            session_transports: LiveSessionTransportCounts {
                daemon_proxy: 0,
                http: 0,
            },
        };
        let servers = vec![plug_core::types::ServerStatus {
            server_id: "imessage".to_string(),
            health: plug_core::types::ServerHealth::Healthy,
            tool_count: 12,
            auth_status: "none".to_string(),
            upstream: None,
            metrics: Some(plug_core::types::UpstreamMetricsSnapshot {
                call_count: 100,
                error_count: 7,
                last_latency_ms: 42,
                degraded_since_epoch_secs: Some(1_700_000_000),
                circuit_state: "open".to_string(),
                restart_count: 3,
                last_restart_epoch_secs: Some(1_700_000_500),
            }),
            availability: plug_core::types::Availability::Degraded,
            selected_protocol_era: None,
            selected_protocol_version: None,
            error: None,
            last_seen: None,
        }];
        let json = status_json(10, 1, &inventory, &servers, true, "live_daemon", true, 0);

        let m = &json["servers"][0]["metrics"];
        assert_eq!(m["call_count"], 100);
        assert_eq!(m["error_count"], 7);
        assert_eq!(m["last_latency_ms"], 42);
        assert_eq!(m["degraded_since_epoch_secs"], 1_700_000_000_u64);
        assert_eq!(m["circuit_state"], "open");
        // Supervision (item 2b) restart metrics ride the same per-upstream contract.
        assert_eq!(m["restart_count"], 3);
        assert_eq!(m["last_restart_epoch_secs"], 1_700_000_500_u64);
        // Availability rides the same per-server JSON contract.
        assert_eq!(json["servers"][0]["availability"], "degraded");
    }
}
