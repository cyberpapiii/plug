use std::collections::{BTreeMap, BTreeSet};

use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::config::load_editable_config;
use crate::commands::tools::prompt_tool_actions;
use crate::runtime::daemon_running;
use crate::ui::{
    can_prompt_interactively, print_banner, print_heading, print_label_value, terminal_width,
};

type ToolInventoryGroup = Vec<(String, String, Option<String>, Option<String>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolInventoryEmptyState {
    NoConfiguredServers,
    RuntimeUnavailable,
    AllServersUnavailable,
    EmptyMergedSet,
}

fn classify_empty_tool_inventory(
    daemon_available: bool,
    configured_server_count: usize,
    runtime_servers: &[plug_core::types::ServerStatus],
) -> ToolInventoryEmptyState {
    if configured_server_count == 0 {
        return ToolInventoryEmptyState::NoConfiguredServers;
    }
    if !daemon_available {
        return ToolInventoryEmptyState::RuntimeUnavailable;
    }
    if !runtime_servers.is_empty()
        && runtime_servers
            .iter()
            .all(|server| !matches!(server.health, plug_core::types::ServerHealth::Healthy))
    {
        return ToolInventoryEmptyState::AllServersUnavailable;
    }
    ToolInventoryEmptyState::EmptyMergedSet
}

pub(crate) async fn cmd_tool_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
    _verbose: u8,
    started: Option<bool>,
) -> anyhow::Result<()> {
    let interactive = matches!(output, OutputFormat::Text) && can_prompt_interactively();
    let mut started = match started {
        Some(started) => started,
        None => false,
    };

    loop {
        let daemon_available = daemon_running().await;
        let config = load_editable_config(config_path).ok().map(|(_, config)| config);
        let configured_server_count = config.as_ref().map(|config| config.servers.len()).unwrap_or(0);
        let runtime_servers = if daemon_available {
            match crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::Status).await {
                Ok(plug_core::ipc::IpcResponse::Status { servers, .. }) => servers
                    .into_iter()
                    .filter(|server| server.server_id != "__plug_internal__")
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let mut all_tools: Vec<plug_core::ipc::IpcToolInfo> = Vec::new();
        if daemon_available
            && let Ok(plug_core::ipc::IpcResponse::Tools { tools }) =
            crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::ListTools).await
        {
            for t in tools {
                if t.server_id == "__plug_internal__" {
                    continue;
                }
                all_tools.push(t);
            }
        }

        let mut tools_by_prefix: BTreeMap<String, ToolInventoryGroup> = BTreeMap::new();
        for t in &all_tools {
            let (prefix, tool_name) = if let Some(idx) = t.name.find("__") {
                (t.name[..idx].to_string(), t.name[idx + 2..].to_string())
            } else {
                (t.server_id.clone(), t.name.clone())
            };
            tools_by_prefix.entry(prefix).or_default().push((
                tool_name,
                t.server_id.clone(),
                t.title.clone(),
                t.description.clone(),
            ));
        }

        let unique_servers: BTreeSet<&str> =
            all_tools.iter().map(|t| t.server_id.as_str()).collect();

        match output {
            OutputFormat::Json => {
                let json_groups: BTreeMap<String, Vec<serde_json::Value>> = tools_by_prefix
                    .iter()
                    .map(|(prefix, tools)| {
                        let entries: Vec<serde_json::Value> = tools
                            .iter()
                            .map(|(name, server_id, title, desc)| {
                                serde_json::json!({
                                    "name": name,
                                    "server_id": server_id,
                                    "title": title,
                                    "description": desc,
                                })
                            })
                            .collect();
                        (prefix.clone(), entries)
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "runtime_available": daemon_available,
                        "status_source": if daemon_available { "live_daemon" } else { "runtime_unavailable" },
                        "tool_count": all_tools.len(),
                        "server_count": unique_servers.len(),
                        "groups": json_groups,
                    }))?
                );
                return Ok(());
            }
            OutputFormat::Text => {
                if tools_by_prefix.is_empty() {
                    match classify_empty_tool_inventory(
                        daemon_available,
                        configured_server_count,
                        &runtime_servers,
                    ) {
                        ToolInventoryEmptyState::NoConfiguredServers => {
                            println!(
                                "No servers are configured. Use {} or {} to add upstreams.",
                                style("plug setup").cyan(),
                                style("plug servers").cyan()
                            );
                        }
                        ToolInventoryEmptyState::RuntimeUnavailable => {
                            println!(
                                "Runtime is unavailable. Start the shared service with {} or inspect config with {}.",
                                style("plug start").cyan(),
                                style("plug servers").cyan()
                            );
                        }
                        ToolInventoryEmptyState::AllServersUnavailable => {
                            println!(
                                "All configured servers are currently unavailable or auth-required. Check {} and {} for details.",
                                style("plug status").cyan(),
                                style("plug auth status").cyan()
                            );
                        }
                        ToolInventoryEmptyState::EmptyMergedSet => {
                            println!(
                                "No tools are currently exposed even though the runtime is available. Check {} for enabled servers and {} for hidden or disabled tools.",
                                style("plug servers").cyan(),
                                style("plug config check").cyan()
                            );
                        }
                    }
                    return Ok(());
                }
                let term_width = terminal_width();
                let available_width = term_width.saturating_sub(40);
                let disabled_count = config
                    .as_ref()
                    .map(|config| config.disabled_tools.len())
                    .unwrap_or(0);
                print_banner(
                    "◆",
                    "Tools",
                    &format!(
                        "{} tools across {} server(s)",
                        all_tools.len(),
                        unique_servers.len()
                    ),
                );
                if started {
                    println!();
                }
                print_heading("Summary");
                print_label_value("Tools", style(all_tools.len()).bold());
                print_label_value("Servers", style(unique_servers.len()).bold());
                print_label_value("Disabled", style(disabled_count).yellow().bold());
                println!();
                print_heading("Inventory");
                for (prefix, mut tools) in tools_by_prefix {
                    tools.sort_by(|a, b| a.0.cmp(&b.0));
                    let server_id = &tools[0].1;
                    let annotation = if server_id != &prefix {
                        format!(" {}", style(format!("[{}]", server_id)).dim())
                    } else {
                        String::new()
                    };
                    println!(
                        "{} {} {}{}",
                        style("▸").cyan().bold(),
                        style(&prefix).bold(),
                        style(format!("{} tools", tools.len())).dim(),
                        annotation
                    );
                    for (name, _server_id, title, desc) in &tools {
                        let name_styled = style(format!("  │ {:<28}", name)).cyan();
                        let display_text = title.as_deref().or(desc.as_deref());
                        if let Some(text) = display_text {
                            let cleaned = text.replace('\n', " ").replace('\r', "");
                            let short = if cleaned.len() > available_width {
                                format!("{}...", &cleaned[..available_width.max(0)])
                            } else {
                                cleaned
                            };
                            println!("{}  {}", name_styled, style(short).dim());
                        } else {
                            println!("{}", name_styled);
                        }
                    }
                    println!();
                }
            }
        }

        if !interactive {
            break;
        }
        println!();
        if !prompt_tool_actions(config_path).await? {
            break;
        }
        println!();
        started = false;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_status(server_id: &str, health: plug_core::types::ServerHealth) -> plug_core::types::ServerStatus {
        plug_core::types::ServerStatus {
            server_id: server_id.to_string(),
            health,
            auth_status: "oauth".to_string(),
            tool_count: 0,
            last_seen: None,
        }
    }

    #[test]
    fn classify_empty_tool_inventory_distinguishes_major_empty_states() {
        assert_eq!(
            classify_empty_tool_inventory(false, 0, &[]),
            ToolInventoryEmptyState::NoConfiguredServers
        );
        assert_eq!(
            classify_empty_tool_inventory(false, 2, &[]),
            ToolInventoryEmptyState::RuntimeUnavailable
        );
        assert_eq!(
            classify_empty_tool_inventory(
                true,
                2,
                &[
                    server_status("a", plug_core::types::ServerHealth::AuthRequired),
                    server_status("b", plug_core::types::ServerHealth::Failed),
                ],
            ),
            ToolInventoryEmptyState::AllServersUnavailable
        );
        assert_eq!(
            classify_empty_tool_inventory(
                true,
                1,
                &[server_status("a", plug_core::types::ServerHealth::Healthy)],
            ),
            ToolInventoryEmptyState::EmptyMergedSet
        );
    }
}
