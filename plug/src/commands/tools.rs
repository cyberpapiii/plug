use dialoguer::console::style;
use dialoguer::{MultiSelect, Select};

use crate::commands::config::load_editable_config;
use crate::runtime::ensure_daemon_with_feedback;
use crate::ui::{
    can_prompt_interactively, cli_prompt_theme, print_banner, print_info_line, print_label_value,
    print_success_line,
};
use crate::views::tools::{
    ToolInventoryGroup, print_empty_tool_inventory, print_tool_inventory, tools_json,
};
use crate::{OutputFormat, ToolCommands};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) async fn cmd_tool_command(
    config_path: Option<&std::path::PathBuf>,
    command: ToolCommands,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    match command {
        ToolCommands::Disabled => cmd_tool_disabled(config_path, output),
        ToolCommands::Disable { server, patterns } => {
            let patterns = tool_patterns_for_server(config_path, server, patterns, false).await?;
            cmd_tool_disable(config_path, patterns).await
        }
        ToolCommands::Enable { server, patterns } => {
            let patterns = tool_patterns_for_server(config_path, server, patterns, true).await?;
            cmd_tool_enable(config_path, patterns).await
        }
    }
}

/// The patterns to change: those given, or every tool of `server`.
///
/// The daemon knows which server each tool it lists came from, so the live
/// config gets patterns from that. An offline `--config` file has no tool
/// list; it gets a wildcard for each prefix the server's tools are named
/// under. Turning a server back on also takes off each of its tools that
/// was turned off by name.
async fn tool_patterns_for_server(
    config_path: Option<&std::path::PathBuf>,
    server: Option<String>,
    patterns: Vec<String>,
    enabling: bool,
) -> anyhow::Result<Vec<String>> {
    let Some(server) = server else {
        return Ok(patterns);
    };
    if !patterns.is_empty() {
        anyhow::bail!("pass either patterns or `--server`, not both");
    }
    let (_, config) = load_editable_config(config_path)?;
    let Some(server_config) = config.servers.get(&server) else {
        anyhow::bail!("unknown server `{server}`");
    };
    let mut patterns = if config_path.is_some() {
        offline_server_patterns(&config, &server, server_config)?
    } else {
        ensure_daemon_with_feedback(None, false).await?;
        let tools = match crate::ipc_client::request(plug_core::ipc::IpcRequest::ListTools).await? {
            plug_core::ipc::IpcResponse::Tools { tools } => tools,
            plug_core::ipc::IpcResponse::Error { message, .. } => anyhow::bail!(message),
            other => anyhow::bail!("unexpected daemon response: {other:?}"),
        };
        live_server_patterns(&tools, &server, &config.prefix_delimiter)
    };
    if patterns.is_empty() {
        anyhow::bail!("`{server}` lists no tools right now; name the tools instead");
    }
    if enabling {
        let named: Vec<String> = config
            .disabled_tools
            .iter()
            .filter(|entry| {
                !entry.contains('*') && plug_core::tool_patterns::is_disabled_tool(&patterns, entry)
            })
            .cloned()
            .collect();
        patterns.extend(named);
        patterns.dedup();
        // A tool cannot be turned on while a wildcard still covers it.
        patterns.sort_by_key(|pattern| !pattern.contains('*'));
    }
    Ok(patterns)
}

/// `server`'s listed tools as patterns: a wildcard for a prefix only its
/// tools are listed under, so a tool it adds later is covered too, and the
/// tool's own name under a prefix another server's tools share.
fn live_server_patterns(
    tools: &[plug_core::ipc::IpcToolInfo],
    server: &str,
    delimiter: &str,
) -> Vec<String> {
    // Patterns match without regard to case.
    let prefix_of = |name: &str| {
        name.split_once(delimiter)
            .map(|(prefix, _)| prefix.to_ascii_lowercase())
    };
    let shared: std::collections::HashSet<String> = tools
        .iter()
        .filter(|tool| tool.server_id != server)
        .filter_map(|tool| prefix_of(&tool.name))
        .collect();
    let mut patterns: Vec<String> = tools
        .iter()
        .filter(|tool| tool.server_id == server)
        .map(|tool| match tool.name.split_once(delimiter) {
            Some((prefix, _)) if !shared.contains(&prefix.to_ascii_lowercase()) => {
                format!("{prefix}{delimiter}*")
            }
            _ => tool.name.clone(),
        })
        .collect();
    patterns.sort();
    patterns.dedup();
    patterns
}

fn offline_server_patterns(
    config: &plug_core::config::Config,
    server: &str,
    server_config: &plug_core::config::ServerConfig,
) -> anyhow::Result<Vec<String>> {
    if !config.enable_prefix {
        anyhow::bail!("tool names carry no server prefix in this config; name the tools instead");
    }
    Ok(
        plug_core::tool_naming::server_prefixes(server, server_config, None)
            .into_iter()
            .map(|prefix| format!("{prefix}{}*", config.prefix_delimiter))
            .collect(),
    )
}

/// Turn each pattern on or off through the mutation every config edit
/// takes, returning the disabled patterns afterwards.
async fn set_patterns_enabled(
    config_path: Option<&std::path::PathBuf>,
    patterns: &[String],
    enabled: bool,
) -> anyhow::Result<Vec<String>> {
    let mut disabled = Vec::new();
    for pattern in patterns {
        let result = crate::commands::servers::apply_server_mutation(
            config_path,
            plug_core::operator::OperatorMutation::SetToolEnabled {
                tool: pattern.clone(),
                enabled,
            },
        )
        .await?;
        disabled = result.disabled_tools.unwrap_or_default();
    }
    Ok(disabled)
}

pub(crate) async fn prompt_tool_actions(
    config_path: Option<&std::path::PathBuf>,
) -> anyhow::Result<bool> {
    let options = ["Done", "Disable tools", "Enable tools", "Show disabled"];
    let selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Choose action")
        .items(options)
        .default(0)
        .interact_opt()?;

    match selection {
        Some(1) => {
            cmd_tool_disable(config_path, Vec::new()).await?;
            Ok(true)
        }
        Some(2) => {
            cmd_tool_enable(config_path, Vec::new()).await?;
            Ok(true)
        }
        Some(3) => {
            cmd_tool_disabled(config_path, &OutputFormat::Text)?;
            Ok(false)
        }
        _ => Ok(false),
    }
}

pub(crate) fn cmd_tool_disabled(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let (path, config) = load_editable_config(config_path)?;

    if matches!(output, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": path,
                "disabled_tools": config.disabled_tools,
            }))?
        );
        return Ok(());
    }

    print_banner(
        "◆",
        "Disabled tools",
        "Configured exact names and wildcard patterns",
    );
    print_label_value("Path", style(path.display()).dim());
    if config.disabled_tools.is_empty() {
        println!();
        print_info_line("No disabled tool patterns configured.");
        return Ok(());
    }
    println!();
    for pattern in config.disabled_tools {
        println!("  {} {}", style("·").dim(), pattern);
    }
    Ok(())
}

pub(crate) async fn cmd_tool_disable(
    config_path: Option<&std::path::PathBuf>,
    mut patterns: Vec<String>,
) -> anyhow::Result<()> {
    let (_, config) = load_editable_config(config_path)?;

    if patterns.is_empty() {
        ensure_daemon_with_feedback(config_path, true).await?;
        let mut all_tools: Vec<String> = if let Ok(plug_core::ipc::IpcResponse::Tools { tools }) =
            crate::ipc_client::request(plug_core::ipc::IpcRequest::ListTools).await
        {
            tools
                .into_iter()
                .filter(|tool| tool.server_id != plug_core::tool_naming::INTERNAL_SERVER_ID)
                .map(|tool| tool.name)
                .collect()
        } else {
            Vec::new()
        };
        all_tools.sort();
        all_tools.dedup();
        if all_tools.is_empty() {
            anyhow::bail!("no live tools available to disable");
        }

        let selections = MultiSelect::with_theme(&cli_prompt_theme())
            .with_prompt("Select tools to disable")
            .items(&all_tools)
            .interact()?;
        patterns = selections
            .into_iter()
            .map(|index| all_tools[index].clone())
            .collect();
        if patterns.is_empty() {
            return Ok(());
        }
    }

    let before = config.disabled_tools;
    let after = set_patterns_enabled(config_path, &patterns, false).await?;
    let added = after
        .iter()
        .filter(|pattern| !before.contains(pattern))
        .count();
    if added == 0 {
        print_info_line("No new disabled tool patterns were added.");
    } else {
        print_success_line(format!("Disabled {added} tool pattern(s)."));
    }
    Ok(())
}

pub(crate) async fn cmd_tool_enable(
    config_path: Option<&std::path::PathBuf>,
    mut patterns: Vec<String>,
) -> anyhow::Result<()> {
    let (_, config) = load_editable_config(config_path)?;
    if config.disabled_tools.is_empty() {
        print_info_line("No disabled tool patterns configured.");
        return Ok(());
    }

    if patterns.is_empty() {
        let selections = MultiSelect::with_theme(&cli_prompt_theme())
            .with_prompt("Select disabled patterns to re-enable")
            .items(&config.disabled_tools)
            .defaults(&vec![false; config.disabled_tools.len()])
            .interact()?;
        patterns = selections
            .into_iter()
            .map(|index| config.disabled_tools[index].clone())
            .collect();
        if patterns.is_empty() {
            return Ok(());
        }
    }

    let before = config.disabled_tools.len();
    let after = set_patterns_enabled(config_path, &patterns, true).await?;
    let removed = before.saturating_sub(after.len());
    if removed == 0 {
        print_info_line("No matching disabled tool patterns were found.");
    } else {
        print_success_line(format!("Re-enabled {removed} tool pattern(s)."));
    }
    Ok(())
}

pub(crate) async fn cmd_tool_list(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
    verbose: u8,
    server: Option<&str>,
) -> anyhow::Result<()> {
    let interactive = matches!(output, OutputFormat::Text) && can_prompt_interactively();

    loop {
        let (status_availability, runtime_status) = crate::ipc_client::query(
            plug_core::ipc::IpcRequest::Status,
            |response| match response {
                plug_core::ipc::IpcResponse::Status { servers, .. } => Some(servers),
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
        let (tools_availability, tools_result) =
            crate::ipc_client::query(plug_core::ipc::IpcRequest::ListTools, |response| {
                match response {
                    plug_core::ipc::IpcResponse::Tools { tools } => Some(
                        tools
                            .into_iter()
                            .filter(|tool| {
                                tool.server_id != plug_core::tool_naming::INTERNAL_SERVER_ID
                            })
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                }
            })
            .await;
        let all_tools = tools_result.unwrap_or_default();
        let inventory_availability = if runtime_available {
            tools_availability
        } else {
            status_availability
        };
        let inventory_available = inventory_availability.runtime_available();

        let delimiter = config.as_ref().map_or_else(
            || plug_core::config::Config::default().prefix_delimiter,
            |config| config.prefix_delimiter.clone(),
        );
        let mut tools_by_prefix: BTreeMap<String, ToolInventoryGroup> = BTreeMap::new();
        for t in &all_tools {
            let (prefix, tool_name) =
                if let Some((prefix, tool_name)) = t.name.split_once(delimiter.as_str()) {
                    (prefix.to_string(), tool_name.to_string())
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
                println!(
                    "{}",
                    serde_json::to_string_pretty(&tools_json(
                        &tools_by_prefix,
                        inventory_availability,
                        tool_count,
                        server_count,
                    ))?
                );
                return Ok(());
            }
            OutputFormat::Text => {
                if tools_by_prefix.is_empty() {
                    print_empty_tool_inventory(
                        inventory_available,
                        inventory_availability.daemon_reachable(),
                        configured_server_count,
                        &runtime_servers,
                    );
                    return Ok(());
                }
                let disabled_count = config
                    .as_ref()
                    .map(|config| config.disabled_tools.len())
                    .unwrap_or(0);
                print_tool_inventory(
                    tools_by_prefix,
                    tool_count,
                    server_count,
                    disabled_count,
                    server.is_none(),
                    verbose,
                );
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
    }
    Ok(())
}

/// `plug tools workspace` matches the server id; `plug tools gmail` matches
/// one of the tool groups a server exposes under its own prefix.
fn group_matches(prefix: &str, server_id: &str, filter: &str) -> bool {
    server_id.eq_ignore_ascii_case(filter) || prefix.eq_ignore_ascii_case(filter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_filter_matches_server_or_group_ignoring_case() {
        assert!(group_matches("Gmail", "workspace", "workspace"));
        assert!(group_matches("Gmail", "workspace", "gmail"));
        assert!(!group_matches("Gmail", "workspace", "slack"));
    }

    fn config_file(label: &str, config: &plug_core::config::Config) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plug-tools-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("config.toml");
        plug_core::operator::persist_config_atomic(&path, config).expect("write config");
        path
    }

    fn tool(name: &str, server_id: &str) -> plug_core::ipc::IpcToolInfo {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "server_id": server_id,
            "description": null,
            "title": null,
        }))
        .expect("tool info")
    }

    /// A server's tools are the ones the router routes to it, whatever
    /// prefix they are listed under: a group's, or one with a second
    /// account's name added. A prefix another server's tools also carry is
    /// not taken whole.
    #[test]
    fn a_servers_tools_are_the_ones_routed_to_it() {
        let tools = [
            tool("GmailWorkspace__search_messages", "workspace"),
            tool("GmailWorkspace__send_message", "workspace"),
            tool("GoogleDrive__list_items", "workspace"),
            tool("Workspace__render_page_to_image", "workspace"),
            tool("GmailGoogleWork__search_messages", "google-work"),
            tool("GoogleDrive__empty_trash", "drive-extras"),
            tool("Slack__search_messages", "slack"),
            tool("SlackWork__search_messages", "slack-work"),
        ];
        assert_eq!(
            live_server_patterns(&tools, "workspace", "__"),
            [
                "GmailWorkspace__*",
                "GoogleDrive__list_items",
                "Workspace__*",
            ]
        );
        assert_eq!(live_server_patterns(&tools, "slack", "__"), ["Slack__*"]);
        assert!(live_server_patterns(&tools, "github", "__").is_empty());
    }

    /// Offline there is no tool list, so a server's patterns are the
    /// prefixes the router names its tools under, group rules included,
    /// with the configured delimiter.
    #[tokio::test]
    async fn offline_server_patterns_follow_the_routers_naming() {
        let mut config = plug_core::config::Config {
            prefix_delimiter: "::".to_string(),
            ..Default::default()
        };
        for name in ["workspace", "slack"] {
            config.servers.insert(
                name.to_string(),
                plug_core::config::ServerConfig {
                    command: Some("true".to_string()),
                    ..Default::default()
                },
            );
        }
        let path = config_file("delimiter", &config);
        let patterns = |server: &str| {
            tool_patterns_for_server(Some(&path), Some(server.to_string()), Vec::new(), false)
        };

        assert_eq!(patterns("slack").await.expect("slack"), ["Slack::*"]);
        let workspace = patterns("workspace").await.expect("workspace");
        for expected in ["Gmail::*", "GoogleDrive::*", "Workspace::*"] {
            assert!(
                workspace.iter().any(|pattern| pattern == expected),
                "{workspace:?}"
            );
        }
        let unknown = patterns("github").await.expect_err("unknown server");
        assert!(unknown.to_string().contains("github"), "{unknown}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Turning a server off and on again leaves the other servers' tools as
    /// they were, and takes off its tools that were off by name too.
    #[tokio::test]
    async fn a_server_goes_off_and_on_whole() {
        let mut config = plug_core::config::Config {
            disabled_tools: vec!["Gmail__send_message".to_string(), "Slack__post".to_string()],
            ..Default::default()
        };
        config.servers.insert(
            "workspace".to_string(),
            plug_core::config::ServerConfig {
                command: Some("true".to_string()),
                ..Default::default()
            },
        );
        let path = config_file("whole", &config);
        let server = || Some("workspace".to_string());

        let off = ToolCommands::Disable {
            server: server(),
            patterns: Vec::new(),
        };
        cmd_tool_command(Some(&path), off, &OutputFormat::Text)
            .await
            .expect("off");
        let (_, written) = load_editable_config(Some(&path)).expect("reload");
        assert!(written.disabled_tools.contains(&"Gmail__*".to_string()));

        let on = ToolCommands::Enable {
            server: server(),
            patterns: Vec::new(),
        };
        cmd_tool_command(Some(&path), on, &OutputFormat::Text)
            .await
            .expect("on");
        let (_, written) = load_editable_config(Some(&path)).expect("reload");
        assert_eq!(written.disabled_tools, ["Slack__post"]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// With `--config`, the edit lands in that file through the operator
    /// mutation, and turning the tool back on refuses while a wider pattern
    /// still covers it.
    #[tokio::test]
    async fn tool_edits_on_an_offline_config_go_through_the_mutation() {
        let config = plug_core::config::Config {
            disabled_tools: vec!["slack__*".to_string()],
            ..Default::default()
        };
        let path = config_file("offline", &config);

        cmd_tool_disable(Some(&path), vec!["github__create_issue".to_string()])
            .await
            .expect("disable");
        let (_, written) = load_editable_config(Some(&path)).expect("reload");
        assert_eq!(written.disabled_tools, ["github__create_issue", "slack__*"]);

        let error = cmd_tool_enable(Some(&path), vec!["slack__send".to_string()])
            .await
            .expect_err("a wider pattern keeps the tool off");
        assert!(error.to_string().contains("slack__*"), "{error}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
