use std::collections::HashMap;

use dialoguer::{Confirm, Input, Select};

use crate::commands::config::{load_editable_config, save_config};
use crate::ui::{cli_prompt_theme, print_info_line, print_success_line};
use crate::{OutputFormat, ServerCommands};

pub(crate) fn parse_transport(
    value: Option<String>,
    url: &Option<String>,
) -> anyhow::Result<plug_core::config::TransportType> {
    match value.as_deref() {
        Some("stdio") | None if url.is_none() => Ok(plug_core::config::TransportType::Stdio),
        Some("http") => Ok(plug_core::config::TransportType::Http),
        Some("sse") => Ok(plug_core::config::TransportType::Sse),
        None => Ok(plug_core::config::TransportType::Http),
        Some(other) => {
            anyhow::bail!("unsupported transport `{other}`; use `stdio`, `http`, or `sse`")
        }
    }
}

pub(crate) async fn cmd_server_command(
    config_path: Option<&std::path::PathBuf>,
    command: ServerCommands,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    match command {
        ServerCommands::Add {
            name,
            command,
            url,
            args,
            transport,
            disabled,
        } => cmd_server_add(config_path, name, command, url, args, transport, disabled),
        ServerCommands::Remove { name, yes } => cmd_server_remove(config_path, name, yes),
        ServerCommands::Edit { name } => cmd_server_edit(config_path, name, output).await,
        ServerCommands::Enable { name } => cmd_server_set_enabled(config_path, name, true),
        ServerCommands::Disable { name } => cmd_server_set_enabled(config_path, name, false),
    }
}

pub(crate) fn cmd_server_add(
    config_path: Option<&std::path::PathBuf>,
    name: Option<String>,
    command: Option<String>,
    url: Option<String>,
    args: Vec<String>,
    transport: Option<String>,
    disabled: bool,
) -> anyhow::Result<()> {
    let (path, mut config) = load_editable_config(config_path)?;
    let name = match name {
        Some(name) => name,
        None => Input::with_theme(&cli_prompt_theme())
            .with_prompt("Server name")
            .interact_text()?,
    };

    if config.servers.contains_key(&name) {
        anyhow::bail!("server `{name}` already exists");
    }

    let provided_transport = transport.clone();
    let non_interactive =
        provided_transport.is_some() || command.is_some() || url.is_some() || !args.is_empty();
    let transport = match transport {
        Some(value) => parse_transport(Some(value), &url)?,
        None if command.is_some() => plug_core::config::TransportType::Stdio,
        None if url.is_some() => plug_core::config::TransportType::Http,
        None => match Select::with_theme(&cli_prompt_theme())
            .with_prompt("Transport")
            .items(["stdio", "http", "sse"])
            .default(0)
            .interact()?
        {
            0 => plug_core::config::TransportType::Stdio,
            1 => plug_core::config::TransportType::Http,
            _ => plug_core::config::TransportType::Sse,
        },
    };

    let server = match transport {
        plug_core::config::TransportType::Stdio => {
            let command = match command {
                Some(command) => command,
                None => Input::with_theme(&cli_prompt_theme())
                    .with_prompt("Command")
                    .interact_text()?,
            };
            let args = if args.is_empty() {
                let value: String = Input::with_theme(&cli_prompt_theme())
                    .with_prompt("Args (space-separated, optional)")
                    .allow_empty(true)
                    .interact_text()?;
                if value.trim().is_empty() {
                    Vec::new()
                } else {
                    value
                        .split_whitespace()
                        .map(|part| part.to_string())
                        .collect()
                }
            } else {
                args
            };
            plug_core::config::ServerConfig {
                command: Some(command),
                args,
                env: HashMap::new(),
                enabled: !disabled,
                transport,
                url: None,
                auth_token: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            }
        }
        plug_core::config::TransportType::Http | plug_core::config::TransportType::Sse => {
            let url = match url {
                Some(url) => url,
                None => Input::with_theme(&cli_prompt_theme())
                    .with_prompt("URL")
                    .interact_text()?,
            };
            let enabled = if disabled {
                false
            } else if non_interactive {
                true
            } else {
                Confirm::with_theme(&cli_prompt_theme())
                    .with_prompt("Enable immediately?")
                    .default(true)
                    .interact()?
            };
            plug_core::config::ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled,
                transport,
                url: Some(url),
                auth_token: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            }
        }
    };

    config.servers.insert(name.clone(), server);
    save_config(&path, &config)?;
    print_success_line(format!("Added server `{name}`."));
    Ok(())
}

pub(crate) fn cmd_server_remove(
    config_path: Option<&std::path::PathBuf>,
    name: Option<String>,
    yes: bool,
) -> anyhow::Result<()> {
    let (path, mut config) = load_editable_config(config_path)?;
    if config.servers.is_empty() {
        print_info_line("No configured servers to remove.");
        return Ok(());
    }

    let name = match name {
        Some(name) => name,
        None => {
            let mut names = config.servers.keys().cloned().collect::<Vec<_>>();
            names.sort();
            let index = Select::with_theme(&cli_prompt_theme())
                .with_prompt("Select a server to remove")
                .items(&names)
                .default(0)
                .interact()?;
            names[index].clone()
        }
    };

    if !config.servers.contains_key(&name) {
        anyhow::bail!("unknown server `{name}`");
    }

    if !yes
        && !Confirm::with_theme(&cli_prompt_theme())
            .with_prompt(format!("Remove server `{name}`?"))
            .default(false)
            .interact()?
    {
        return Ok(());
    }

    config.servers.remove(&name);
    save_config(&path, &config)?;
    print_success_line(format!("Removed server `{name}`."));
    Ok(())
}

pub(crate) async fn cmd_server_edit(
    config_path: Option<&std::path::PathBuf>,
    name: Option<String>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let (path, mut config) = load_editable_config(config_path)?;
    if config.servers.is_empty() {
        print_info_line("No configured servers to edit.");
        return Ok(());
    }

    let name = match name {
        Some(name) => name,
        None => {
            let mut names = config.servers.keys().cloned().collect::<Vec<_>>();
            names.sort();
            let index = Select::with_theme(&cli_prompt_theme())
                .with_prompt("Select a server to edit")
                .items(&names)
                .default(0)
                .interact()?;
            names[index].clone()
        }
    };

    let server = config
        .servers
        .get_mut(&name)
        .ok_or_else(|| anyhow::anyhow!("unknown server `{name}`"))?;

    if matches!(output, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(server)?);
        return Ok(());
    }

    let enabled = Confirm::with_theme(&cli_prompt_theme())
        .with_prompt("Enabled?")
        .default(server.enabled)
        .interact()?;
    server.enabled = enabled;

    match server.transport {
        plug_core::config::TransportType::Stdio => {
            let command: String = Input::with_theme(&cli_prompt_theme())
                .with_prompt("Command")
                .with_initial_text(server.command.clone().unwrap_or_default())
                .interact_text()?;
            let args: String = Input::with_theme(&cli_prompt_theme())
                .with_prompt("Args (space-separated)")
                .with_initial_text(server.args.join(" "))
                .allow_empty(true)
                .interact_text()?;
            server.command = Some(command);
            server.args = if args.trim().is_empty() {
                Vec::new()
            } else {
                args.split_whitespace()
                    .map(|part| part.to_string())
                    .collect()
            };
        }
        plug_core::config::TransportType::Http | plug_core::config::TransportType::Sse => {
            let url: String = Input::with_theme(&cli_prompt_theme())
                .with_prompt("URL")
                .with_initial_text(server.url.clone().unwrap_or_default())
                .interact_text()?;
            server.url = Some(url);
        }
    }

    save_config(&path, &config)?;
    print_success_line(format!("Updated server `{name}`."));
    Ok(())
}

pub(crate) fn cmd_server_set_enabled(
    config_path: Option<&std::path::PathBuf>,
    name: Option<String>,
    enabled: bool,
) -> anyhow::Result<()> {
    let (path, mut config) = load_editable_config(config_path)?;
    if config.servers.is_empty() {
        print_info_line("No configured servers found.");
        return Ok(());
    }

    let name = match name {
        Some(name) => name,
        None => {
            let mut names = config.servers.keys().cloned().collect::<Vec<_>>();
            names.sort();
            let index = Select::with_theme(&cli_prompt_theme())
                .with_prompt(if enabled {
                    "Select a server to enable"
                } else {
                    "Select a server to disable"
                })
                .items(&names)
                .default(0)
                .interact()?;
            names[index].clone()
        }
    };

    let server = config
        .servers
        .get_mut(&name)
        .ok_or_else(|| anyhow::anyhow!("unknown server `{name}`"))?;
    server.enabled = enabled;
    save_config(&path, &config)?;
    if enabled {
        print_success_line(format!("Enabled server `{name}`."));
    } else {
        print_success_line(format!("Disabled server `{name}`."));
    }
    Ok(())
}
