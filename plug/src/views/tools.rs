use std::collections::{BTreeMap, BTreeSet};

use dialoguer::console::style;

use crate::OutputFormat;
use crate::commands::config::load_editable_config;
use crate::commands::tools::prompt_tool_actions;
use crate::runtime::daemon_query;
use crate::ui::{
    can_prompt_interactively, print_banner, print_heading, print_info_line, print_label_value,
    print_wrapped_rows, terminal_width,
};

type ToolInventoryGroup = Vec<(
    String,
    String,
    Option<String>,
    Option<String>,
    Option<Vec<rmcp::model::Icon>>,
    plug_core::ipc::IpcToolRiskInfo,
    Option<plug_core::ipc::IpcServerSourceInfo>,
    Option<plug_core::types::UpstreamServerMetadata>,
    plug_core::ipc::IpcTrustInfo,
)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolInventoryEmptyState {
    NoConfiguredServers,
    RuntimeUnavailable,
    RuntimeInspectionFailed,
    AllServersUnavailable,
    EmptyMergedSet,
}

fn classify_empty_tool_inventory(
    runtime_available: bool,
    daemon_reachable: bool,
    configured_server_count: usize,
    runtime_servers: &[plug_core::types::ServerStatus],
) -> ToolInventoryEmptyState {
    if configured_server_count == 0 {
        return ToolInventoryEmptyState::NoConfiguredServers;
    }
    if !daemon_reachable {
        return ToolInventoryEmptyState::RuntimeUnavailable;
    }
    if !runtime_available {
        return ToolInventoryEmptyState::RuntimeInspectionFailed;
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

/// `plug tools workspace` matches the server id; `plug tools gmail` matches
/// one of the tool groups a server exposes under its own prefix.
fn group_matches(prefix: &str, server_id: &str, filter: &str) -> bool {
    server_id.eq_ignore_ascii_case(filter) || prefix.eq_ignore_ascii_case(filter)
}

/// One row per server for the default `plug tools` view.
#[derive(Debug, PartialEq, Eq)]
struct ServerToolSummary<'a> {
    server: &'a str,
    tools: usize,
    /// Tool groups the server exposes under a prefix other than its own id.
    groups: Vec<&'a str>,
}

fn server_tool_summaries(
    tools_by_prefix: &BTreeMap<String, ToolInventoryGroup>,
) -> Vec<ServerToolSummary<'_>> {
    let mut summaries: BTreeMap<&str, ServerToolSummary<'_>> = BTreeMap::new();
    for (prefix, tools) in tools_by_prefix {
        for tool in tools {
            let server = tool.1.as_str();
            let summary = summaries
                .entry(server)
                .or_insert_with(|| ServerToolSummary {
                    server,
                    tools: 0,
                    groups: Vec::new(),
                });
            summary.tools += 1;
            if !prefix.eq_ignore_ascii_case(server) && !summary.groups.contains(&prefix.as_str()) {
                summary.groups.push(prefix);
            }
        }
    }
    summaries.into_values().collect()
}

fn print_server_summaries(summaries: &[ServerToolSummary<'_>], width: usize) {
    print_heading("Servers");
    // Groups come from `tool_groups` in the config; most setups have none.
    let any_groups = summaries.iter().any(|summary| !summary.groups.is_empty());
    println!(
        "  {:<24} {:>5}  {}",
        style("SERVER").dim(),
        style("TOOLS").dim(),
        if any_groups {
            style("GROUPS").dim()
        } else {
            style("")
        }
    );
    for summary in summaries {
        let prefix_text = format!("  {:<24} {:>5}  ", summary.server, summary.tools);
        let prefix_display = format!(
            "  {:<24} {:>5}  ",
            style(summary.server).cyan(),
            summary.tools
        );
        print_wrapped_rows(
            &prefix_text,
            prefix_display,
            &summary.groups.join(", "),
            width,
            |line| style(line).dim(),
        );
    }
}

pub(crate) async fn cmd_tool_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
    verbose: u8,
    server: Option<&str>,
    started: Option<bool>,
) -> anyhow::Result<()> {
    let interactive = matches!(output, OutputFormat::Text) && can_prompt_interactively();
    let mut started = match started {
        Some(started) => started,
        None => false,
    };

    loop {
        let (status_availability, runtime_status) = daemon_query(
            &plug_core::ipc::IpcRequest::Status,
            |response| match response {
                plug_core::ipc::IpcResponse::Status { servers, .. } => Some(
                    servers
                        .into_iter()
                        .filter(|server| server.server_id != "__plug_internal__")
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            },
        )
        .await;
        let config = load_editable_config(config_path)
            .ok()
            .map(|(_, config)| config);
        let configured_server_count = config
            .as_ref()
            .map(|config| config.servers.len())
            .unwrap_or(0);
        let runtime_available = status_availability.runtime_available();
        let runtime_servers = runtime_status.unwrap_or_default();
        let (tools_availability, tools_result) = daemon_query(
            &plug_core::ipc::IpcRequest::ListTools,
            |response| match response {
                plug_core::ipc::IpcResponse::Tools { tools } => Some(
                    tools
                        .into_iter()
                        .filter(|tool| tool.server_id != "__plug_internal__")
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            },
        )
        .await;
        let all_tools = tools_result.unwrap_or_default();
        let inventory_availability = if runtime_available {
            tools_availability
        } else {
            status_availability
        };
        let inventory_available = inventory_availability.runtime_available();

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
                t.icons.clone(),
                t.risk.clone(),
                t.source.clone(),
                t.upstream.clone(),
                t.trust.clone(),
            ));
        }

        if let Some(filter) = server {
            tools_by_prefix.retain(|prefix, tools| group_matches(prefix, &tools[0].1, filter));
            if tools_by_prefix.is_empty() && inventory_available {
                anyhow::bail!(
                    "no server or tool group named `{filter}`; run `plug tools` to see them"
                );
            }
        }
        let tool_count: usize = tools_by_prefix.values().map(Vec::len).sum();
        let server_count = tools_by_prefix
            .values()
            .flat_map(|tools| tools.iter().map(|tool| tool.1.as_str()))
            .collect::<BTreeSet<_>>()
            .len();

        match output {
            OutputFormat::Json => {
                let json_groups: BTreeMap<String, Vec<serde_json::Value>> = tools_by_prefix
                    .iter()
                    .map(|(prefix, tools)| {
                        let entries: Vec<serde_json::Value> = tools
                            .iter()
                            .map(
                                |(
                                    name,
                                    server_id,
                                    title,
                                    desc,
                                    icons,
                                    risk,
                                    source,
                                    upstream,
                                    trust,
                                )| {
                                    serde_json::json!({
                                        "name": name,
                                        "server_id": server_id,
                                        "title": title,
                                        "description": desc,
                                        "icons": icons,
                                        "risk": risk,
                                        "source": source,
                                        "upstream": upstream,
                                        "trust": trust,
                                    })
                                },
                            )
                            .collect();
                        (prefix.clone(), entries)
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "runtime_available": inventory_available,
                        "status_source": inventory_availability.status_source(),
                        "tool_count": tool_count,
                        "server_count": server_count,
                        "groups": json_groups,
                    }))?
                );
                return Ok(());
            }
            OutputFormat::Text => {
                if tools_by_prefix.is_empty() {
                    match classify_empty_tool_inventory(
                        inventory_available,
                        inventory_availability.daemon_reachable(),
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
                        ToolInventoryEmptyState::RuntimeInspectionFailed => {
                            println!(
                                "Runtime inspection failed even though the daemon is reachable. Use {} or {} to diagnose the IPC/runtime failure.",
                                style("plug doctor").cyan(),
                                style("plug status").cyan()
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
                    &format!("{tool_count} tools across {server_count} server(s)"),
                );
                if started {
                    println!();
                }
                print_heading("Summary");
                print_label_value("Tools", style(tool_count).bold());
                print_label_value("Servers", style(server_count).bold());
                // `disabled_tools` applies to every server, so it only belongs
                // in the whole inventory.
                if server.is_none() && disabled_count > 0 {
                    print_label_value("Disabled", style(disabled_count).yellow().bold());
                }
                println!();
                if server.is_none() && verbose == 0 {
                    print_server_summaries(&server_tool_summaries(&tools_by_prefix), term_width);
                    println!();
                    print_info_line(
                        "Run `plug tools <server>` to list one server's tools, or `plug tools -v` to list them all.",
                    );
                } else {
                    print_heading("Inventory");
                    for (prefix, mut tools) in tools_by_prefix {
                        tools.sort_by(|a, b| a.0.cmp(&b.0));
                        let server_id = &tools[0].1;
                        let annotation = if !server_id.eq_ignore_ascii_case(&prefix) {
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
                        for (
                            name,
                            _server_id,
                            title,
                            desc,
                            _icons,
                            _risk,
                            _source,
                            _upstream,
                            _trust,
                        ) in &tools
                        {
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

    fn server_status(
        server_id: &str,
        health: plug_core::types::ServerHealth,
    ) -> plug_core::types::ServerStatus {
        plug_core::types::ServerStatus {
            server_id: server_id.to_string(),
            health,
            auth_status: "oauth".to_string(),
            tool_count: 0,
            upstream: None,
            metrics: None,
            availability: Default::default(),
            selected_protocol_era: None,
            selected_protocol_version: None,
            error: None,
            last_seen: None,
        }
    }

    #[test]
    fn classify_empty_tool_inventory_distinguishes_major_empty_states() {
        assert_eq!(
            classify_empty_tool_inventory(false, false, 0, &[]),
            ToolInventoryEmptyState::NoConfiguredServers
        );
        assert_eq!(
            classify_empty_tool_inventory(false, false, 2, &[]),
            ToolInventoryEmptyState::RuntimeUnavailable
        );
        assert_eq!(
            classify_empty_tool_inventory(
                true,
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
                true,
                1,
                &[server_status("a", plug_core::types::ServerHealth::Healthy)],
            ),
            ToolInventoryEmptyState::EmptyMergedSet
        );
        assert_eq!(
            classify_empty_tool_inventory(false, true, 1, &[]),
            ToolInventoryEmptyState::RuntimeInspectionFailed
        );
    }

    fn inventory(entries: &[(&str, &str)]) -> BTreeMap<String, ToolInventoryGroup> {
        let mut groups: BTreeMap<String, ToolInventoryGroup> = BTreeMap::new();
        for (index, (prefix, server)) in entries.iter().enumerate() {
            groups.entry(prefix.to_string()).or_default().push((
                format!("tool{index}"),
                server.to_string(),
                None,
                None,
                None,
                Default::default(),
                None,
                None,
                Default::default(),
            ));
        }
        groups
    }

    #[test]
    fn server_summaries_count_tools_per_server_and_name_foreign_groups() {
        let groups = inventory(&[
            ("Gmail", "workspace"),
            ("Gmail", "workspace"),
            ("GoogleDocs", "workspace"),
            ("Workspace", "workspace"),
            ("Agent-admin", "agent-admin"),
            ("exa", "exa"),
        ]);
        assert_eq!(
            server_tool_summaries(&groups),
            vec![
                ServerToolSummary {
                    server: "agent-admin",
                    tools: 1,
                    groups: vec![],
                },
                ServerToolSummary {
                    server: "exa",
                    tools: 1,
                    groups: vec![],
                },
                ServerToolSummary {
                    server: "workspace",
                    tools: 4,
                    groups: vec!["Gmail", "GoogleDocs"],
                },
            ]
        );
    }

    #[test]
    fn tool_filter_matches_server_or_group_ignoring_case() {
        assert!(group_matches("Gmail", "workspace", "workspace"));
        assert!(group_matches("Gmail", "workspace", "gmail"));
        assert!(!group_matches("Gmail", "workspace", "slack"));
    }
}
