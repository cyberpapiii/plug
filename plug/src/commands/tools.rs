use dialoguer::console::style;
use dialoguer::{MultiSelect, Select};

use crate::commands::config::load_editable_config;
use crate::runtime::ensure_daemon_with_feedback;
use crate::ui::{
    cli_prompt_theme, print_banner, print_info_line, print_label_value, print_success_line,
};
use crate::{OutputFormat, ToolCommands};

pub(crate) async fn cmd_tool_command(
    config_path: Option<&std::path::PathBuf>,
    command: ToolCommands,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    match command {
        ToolCommands::Disabled => cmd_tool_disabled(config_path, output),
        ToolCommands::Disable { server, patterns } => {
            let patterns = tool_patterns_for_server(config_path, server, patterns)?;
            cmd_tool_disable(config_path, patterns).await
        }
        ToolCommands::Enable { server, patterns } => {
            let patterns = tool_patterns_for_server(config_path, server, patterns)?;
            cmd_tool_enable(config_path, patterns).await
        }
    }
}

/// The patterns to change: those given, or every tool of `server`, named
/// with the configured delimiter.
fn tool_patterns_for_server(
    config_path: Option<&std::path::PathBuf>,
    server: Option<String>,
    mut patterns: Vec<String>,
) -> anyhow::Result<Vec<String>> {
    if let Some(server) = server {
        if !patterns.is_empty() {
            anyhow::bail!("pass either patterns or `--server`, not both");
        }
        let (_, config) = load_editable_config(config_path)?;
        patterns.push(format!("{server}{}*", config.prefix_delimiter));
    }
    Ok(patterns)
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_server_pattern_uses_the_configured_delimiter() {
        let config = plug_core::config::Config {
            prefix_delimiter: "::".to_string(),
            ..Default::default()
        };
        let path = config_file("delimiter", &config);
        let patterns = tool_patterns_for_server(Some(&path), Some("slack".to_string()), Vec::new())
            .expect("patterns");
        assert_eq!(patterns, ["slack::*"]);
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
