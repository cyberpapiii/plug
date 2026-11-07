use std::collections::BTreeMap;

use dialoguer::console::style;

use crate::ipc_client::DaemonQueryAvailability;
use crate::ui::{
    print_banner, print_heading, print_info_line, print_label_value, print_wrapped_rows,
    terminal_width,
};

pub(crate) type ToolInventoryGroup = Vec<(
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

/// `plug tools` as JSON.
pub(crate) fn tools_json(
    tools_by_prefix: &BTreeMap<String, ToolInventoryGroup>,
    inventory_availability: DaemonQueryAvailability,
    tool_count: usize,
    server_count: usize,
) -> serde_json::Value {
    let json_groups: BTreeMap<String, Vec<serde_json::Value>> = tools_by_prefix
        .iter()
        .map(|(prefix, tools)| {
            let entries: Vec<serde_json::Value> = tools
                .iter()
                .map(
                    |(name, server_id, title, desc, icons, risk, source, upstream, trust)| {
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
    serde_json::json!({
        "runtime_available": inventory_availability.runtime_available(),
        "status_source": inventory_availability.status_source(),
        "tool_count": tool_count,
        "server_count": server_count,
        "groups": json_groups,
    })
}

/// Why `plug tools` has nothing to list, and what to run about it.
pub(crate) fn print_empty_tool_inventory(
    inventory_available: bool,
    daemon_reachable: bool,
    configured_server_count: usize,
    runtime_servers: &[plug_core::types::ServerStatus],
) {
    match classify_empty_tool_inventory(
        inventory_available,
        daemon_reachable,
        configured_server_count,
        runtime_servers,
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
}

/// `plug tools` as text. `whole_inventory` is false when one server or
/// group was asked for.
pub(crate) fn print_tool_inventory(
    tools_by_prefix: BTreeMap<String, ToolInventoryGroup>,
    tool_count: usize,
    server_count: usize,
    disabled_count: usize,
    whole_inventory: bool,
    verbose: u8,
) {
    let term_width = terminal_width();
    let available_width = term_width.saturating_sub(40);
    print_banner(
        "◆",
        "Tools",
        &format!("{tool_count} tools across {server_count} server(s)"),
    );
    print_heading("Summary");
    print_label_value("Tools", style(tool_count).bold());
    print_label_value("Servers", style(server_count).bold());
    // `disabled_tools` applies to every server, so it only belongs
    // in the whole inventory.
    if whole_inventory && disabled_count > 0 {
        print_label_value("Disabled", style(disabled_count).yellow().bold());
    }
    println!();
    if whole_inventory && verbose == 0 {
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
            for (name, _server_id, title, desc, _icons, _risk, _source, _upstream, _trust) in &tools
            {
                let name_styled = style(format!("  │ {:<28}", name)).cyan();
                let display_text = title.as_deref().or(desc.as_deref());
                if let Some(text) = display_text {
                    let cleaned = text.replace('\n', " ").replace('\r', "");
                    let short = crate::ui::truncate_tail(&cleaned, available_width);
                    println!("{}  {}", name_styled, style(short).dim());
                } else {
                    println!("{}", name_styled);
                }
            }
            println!();
        }
    }
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
}
