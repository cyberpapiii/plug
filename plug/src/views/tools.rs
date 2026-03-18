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
    let daemon_available = daemon_running().await;

    loop {
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
                    if daemon_available {
                        println!(
                            "No live tools found. Check {} and {} for runtime or auth failures.",
                            style("plug status").cyan(),
                            style("plug auth status").cyan()
                        );
                    } else {
                        println!(
                            "Runtime is unavailable. Start the shared service with {} or inspect config with {}.",
                            style("plug start").cyan(),
                            style("plug servers").cyan()
                        );
                    }
                    return Ok(());
                }
                let term_width = terminal_width();
                let available_width = term_width.saturating_sub(40);
                let disabled_count = load_editable_config(config_path)
                    .map(|(_, config)| config.disabled_tools.len())
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
