use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::clients::{linked_client_targets, linked_client_transport};
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
    let transport_counts = config.servers.values().fold((0usize, 0usize, 0usize), |mut acc, server| {
        match server.transport {
            plug_core::config::TransportType::Stdio => acc.0 += 1,
            plug_core::config::TransportType::Http => acc.1 += 1,
            plug_core::config::TransportType::Sse => acc.2 += 1,
        }
        acc
    });
    let downstream_auth_mode = config.http.auth_mode.label();
    let downstream_auth_summary = match config.http.auth_mode {
        plug_core::config::DownstreamAuthMode::Auto => {
            if plug_core::config::http_bind_is_loopback(&config.http.bind_address) {
                "auto (loopback => no auth)"
            } else {
                "auto (non-loopback => bearer)"
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
    print_label_value(
        "Downstream Auth",
        if downstream_auth_mode == "auto" {
            style(downstream_auth_summary).bold()
        } else {
            style(downstream_auth_summary).yellow().bold()
        },
    );
    if let Some(public_base_url) = &config.http.public_base_url {
        print_label_value("Public URL", public_base_url);
    }

    if !linked_clients.is_empty() {
        let linked_descriptions = linked_clients
            .iter()
            .map(|target| {
                let transport = linked_client_transport(target, false)
                    .map(|transport| match transport {
                        plug_core::export::ExportTransport::Stdio => "stdio",
                        plug_core::export::ExportTransport::Http => "http",
                    })
                    .unwrap_or("unknown");
                format!("{target} ({transport})")
            })
            .collect::<Vec<_>>();
        print_label_value("Linked", linked_descriptions.join(", "));
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
    show_token: bool,
) -> anyhow::Result<()> {
    let started =
        ensure_daemon_with_feedback(config_path, matches!(output, OutputFormat::Text)).await?;

    // Load config to check HTTP auth status
    let config = plug_core::config::load_config(config_path).ok();
    let downstream_auth_info = config.as_ref().map(|c| {
        let mode = c.http.auth_mode.label();
        let summary = match c.http.auth_mode {
            plug_core::config::DownstreamAuthMode::Auto => {
                if plug_core::config::http_bind_is_loopback(&c.http.bind_address) {
                    "loopback => no auth".to_string()
                } else {
                    "non-loopback => bearer".to_string()
                }
            }
            plug_core::config::DownstreamAuthMode::None => "explicit none".to_string(),
            plug_core::config::DownstreamAuthMode::Bearer => "explicit bearer".to_string(),
            plug_core::config::DownstreamAuthMode::Oauth => {
                "authorization-code + PKCE".to_string()
            }
        };
        (mode.to_string(), summary, c.http.public_base_url.clone())
    });
    let http_auth_info = config.as_ref().and_then(|c| {
        let expects_bearer = match c.http.auth_mode {
            plug_core::config::DownstreamAuthMode::Bearer => true,
            plug_core::config::DownstreamAuthMode::Auto => {
                !plug_core::config::http_bind_is_loopback(&c.http.bind_address)
            }
            plug_core::config::DownstreamAuthMode::None
            | plug_core::config::DownstreamAuthMode::Oauth => false,
        };
        if !expects_bearer {
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
            if let Some((mode, summary, public_base_url)) = &downstream_auth_info {
                print_label_value(
                    "Downstream Auth",
                    if mode == "auto" {
                        style(format!("{mode} ({summary})")).bold()
                    } else {
                        style(format!("{mode} ({summary})")).yellow().bold()
                    },
                );
                if let Some(public_base_url) = public_base_url {
                    print_label_value("Public URL", public_base_url);
                }
            }

            // Show HTTP auth status
            if let Some((token_exists, token)) = &http_auth_info {
                if *token_exists {
                    if let Some(t) = token {
                        print_label_value(
                            "HTTP Auth",
                            style(format!("enabled | Token: {t}")).green().bold(),
                        );
                    } else {
                        print_label_value(
                            "HTTP Auth",
                            style("enabled (use --show-token to reveal)").green().bold(),
                        );
                    }
                } else {
                    print_label_value("HTTP Auth", style("NOT CONFIGURED").red().bold());
                }
            }

            println!();
            if servers.is_empty() {
                print_heading("Servers");
                println!("  No servers configured.");
            } else {
                let config_by_server = config
                    .as_ref()
                    .map(|cfg| &cfg.servers);
                print_heading("Servers");
                println!(
                    "  {:<2} {:<18} {:<12} {:<8} {:<6} {:>5}",
                    style("").dim(),
                    style("SERVER").dim(),
                    style("STATUS").dim(),
                    style("UPSTREAM").dim(),
                    style("AUTH").dim(),
                    style("TOOLS").dim()
                );
                println!(
                    "  {}",
                    style("----------------------------------------------------------------").dim()
                );
                for s in &servers {
                    if s.server_id == "__plug_internal__" {
                        continue;
                    }
                    let server_cfg = config_by_server.and_then(|servers| servers.get(&s.server_id));
                    let transport = server_cfg
                        .map(|cfg| match cfg.transport {
                            plug_core::config::TransportType::Stdio => "stdio",
                            plug_core::config::TransportType::Http => "http",
                            plug_core::config::TransportType::Sse => "sse",
                        })
                        .unwrap_or("unknown");
                    let auth = server_cfg
                        .map(|cfg| match (cfg.auth.as_deref(), cfg.auth_token.is_some()) {
                            (Some("oauth"), _) => "oauth",
                            (_, true) => "bearer",
                            _ => "none",
                        })
                        .unwrap_or("unknown");
                    println!(
                        "  {} {:<18} {:<12} {:<8} {:<6} {:>5}",
                        status_marker(&s.health),
                        s.server_id,
                        status_label(&s.health),
                        transport,
                        auth,
                        s.tool_count
                    );
                }
            }
        } else {
            let mut json_obj = serde_json::json!({ "uptime": uptime_secs, "clients": clients, "servers": servers });
            if let Some((mode, summary, public_base_url)) = &downstream_auth_info {
                json_obj["downstream_auth"] = serde_json::json!({
                    "mode": mode,
                    "summary": summary,
                    "public_base_url": public_base_url,
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
    } else {
        let servers = config
            .servers
            .keys()
            .cloned()
            .map(|server_id| plug_core::types::ServerStatus {
                server_id,
                health: plug_core::types::ServerHealth::Failed,
                tool_count: 0,
                auth_status: "none".to_string(),
                last_seen: None,
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "uptime": 0,
                "clients": 0,
                "servers": servers,
                "daemon_running": false
            }))?
        );
    }
    Ok(())
}
