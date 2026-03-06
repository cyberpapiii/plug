use dialoguer::console::style;
use dialoguer::{MultiSelect, Select};

use crate::commands::config::{load_editable_config, save_config};
use crate::runtime::ensure_daemon_with_feedback;
use crate::ui::{
    cli_prompt_theme, print_banner, print_info_line, print_label_value, print_success_line,
};
use crate::{OutputFormat, ToolCommands};

pub(crate) async fn cmd_tool_command(
    config_path: Option<&std::path::PathBuf>,
    command: Option<ToolCommands>,
    output: &OutputFormat,
    verbose: u8,
) -> anyhow::Result<()> {
    match command {
        None => crate::views::tools::cmd_tool_list(config_path, output, verbose, None).await,
        Some(ToolCommands::Disabled) => cmd_tool_disabled(config_path, output),
        Some(ToolCommands::Disable { server, patterns }) => {
            cmd_tool_disable(config_path, tool_patterns_for_server(server, patterns)?).await
        }
        Some(ToolCommands::Enable { server, patterns }) => {
            cmd_tool_enable(config_path, tool_patterns_for_server(server, patterns)?)
        }
    }
}

pub(crate) fn tool_patterns_for_server(
    server: Option<String>,
    mut patterns: Vec<String>,
) -> anyhow::Result<Vec<String>> {
    if let Some(server) = server {
        if !patterns.is_empty() {
            anyhow::bail!("pass either patterns or `--server`, not both");
        }
        patterns.push(format!("{server}__*"));
    }
    Ok(patterns)
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
            cmd_tool_enable(config_path, Vec::new())?;
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
    let (path, mut config) = load_editable_config(config_path)?;

    if patterns.is_empty() {
        let _ = ensure_daemon_with_feedback(config_path, true).await?;
        let mut all_tools: Vec<String> = if let Ok(plug_core::ipc::IpcResponse::Tools { tools }) =
            crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::ListTools).await
        {
            tools
                .into_iter()
                .filter(|tool| tool.server_id != "__plug_internal__")
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

    let mut added = Vec::new();
    for pattern in patterns {
        if !config
            .disabled_tools
            .iter()
            .any(|existing| existing == &pattern)
        {
            config.disabled_tools.push(pattern.clone());
            added.push(pattern);
        }
    }
    config.disabled_tools.sort();
    save_config(&path, &config)?;

    if added.is_empty() {
        print_info_line("No new disabled tool patterns were added.");
    } else {
        print_success_line(format!("Disabled {} tool pattern(s).", added.len()));
    }
    Ok(())
}

pub(crate) fn cmd_tool_enable(
    config_path: Option<&std::path::PathBuf>,
    mut patterns: Vec<String>,
) -> anyhow::Result<()> {
    let (path, mut config) = load_editable_config(config_path)?;
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
    config
        .disabled_tools
        .retain(|existing| !patterns.iter().any(|pattern| pattern == existing));
    save_config(&path, &config)?;

    let removed = before.saturating_sub(config.disabled_tools.len());
    if removed == 0 {
        print_info_line("No matching disabled tool patterns were found.");
    } else {
        print_success_line(format!("Re-enabled {} tool pattern(s).", removed));
    }
    Ok(())
}
