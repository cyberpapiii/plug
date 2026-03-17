use std::collections::HashMap;

use dialoguer::{Confirm, Input, Password, Select};

use crate::commands::config::{load_editable_config, save_config};
use crate::ui::{cli_prompt_theme, print_info_line, print_success_line};
use crate::{OutputFormat, ServerCommands};

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteAuthSelection {
    None,
    Bearer { token: String },
    Oauth {
        client_id: Option<String>,
        scopes: Option<Vec<String>>,
    },
}

fn parse_scope_list(value: &str) -> Option<Vec<String>> {
    let scopes = value
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if scopes.is_empty() { None } else { Some(scopes) }
}

fn current_remote_auth_selection(server: &plug_core::config::ServerConfig) -> RemoteAuthSelection {
    if server.auth.as_deref() == Some("oauth") {
        RemoteAuthSelection::Oauth {
            client_id: server.oauth_client_id.clone(),
            scopes: server.oauth_scopes.clone(),
        }
    } else if let Some(token) = &server.auth_token {
        RemoteAuthSelection::Bearer {
            token: token.as_str().to_string(),
        }
    } else {
        RemoteAuthSelection::None
    }
}

fn apply_remote_auth_selection(
    server: &mut plug_core::config::ServerConfig,
    selection: RemoteAuthSelection,
) {
    match selection {
        RemoteAuthSelection::None => {
            server.auth = None;
            server.auth_token = None;
            server.oauth_client_id = None;
            server.oauth_scopes = None;
        }
        RemoteAuthSelection::Bearer { token } => {
            server.auth = None;
            server.auth_token = Some(token.into());
            server.oauth_client_id = None;
            server.oauth_scopes = None;
        }
        RemoteAuthSelection::Oauth { client_id, scopes } => {
            server.auth = Some("oauth".to_string());
            server.auth_token = None;
            server.oauth_client_id = client_id;
            server.oauth_scopes = scopes;
        }
    }
}

fn prompt_remote_auth_selection(
    server_name: &str,
    current: Option<&plug_core::config::ServerConfig>,
) -> anyhow::Result<RemoteAuthSelection> {
    let existing = current.map(current_remote_auth_selection);
    let default = match existing.as_ref() {
        Some(RemoteAuthSelection::Bearer { .. }) => 1,
        Some(RemoteAuthSelection::Oauth { .. }) => 2,
        _ => 0,
    };
    let choice = Select::with_theme(&cli_prompt_theme())
        .with_prompt(format!("Auth for `{server_name}`"))
        .items([
            "none",
            "bearer token",
            "oauth (authorization-code + PKCE)",
        ])
        .default(default)
        .interact()?;

    match choice {
        0 => Ok(RemoteAuthSelection::None),
        1 => {
            let initial = match existing {
                Some(RemoteAuthSelection::Bearer { token }) => token,
                _ => String::new(),
            };
            let token = Password::with_theme(&cli_prompt_theme())
                .with_prompt(if initial.is_empty() {
                    "Bearer token"
                } else {
                    "Bearer token (leave blank to keep current)"
                })
                .allow_empty_password(true)
                .with_confirmation("Confirm bearer token", "Tokens did not match")
                .interact()?;
            let token = if token.is_empty() { initial } else { token };
            if token.trim().is_empty() {
                anyhow::bail!("bearer token cannot be empty")
            }
            Ok(RemoteAuthSelection::Bearer { token })
        }
        _ => {
            let (initial_client_id, initial_scopes) = match existing {
                Some(RemoteAuthSelection::Oauth { client_id, scopes }) => (
                    client_id.unwrap_or_default(),
                    scopes.unwrap_or_default().join(", "),
                ),
                _ => (String::new(), String::new()),
            };
            let client_id: String = Input::with_theme(&cli_prompt_theme())
                .with_prompt("Pre-registered OAuth client ID (optional)")
                .with_initial_text(initial_client_id)
                .allow_empty(true)
                .interact_text()?;
            let scopes: String = Input::with_theme(&cli_prompt_theme())
                .with_prompt("OAuth scopes (comma-separated, optional)")
                .with_initial_text(initial_scopes)
                .allow_empty(true)
                .interact_text()?;
            Ok(RemoteAuthSelection::Oauth {
                client_id: if client_id.trim().is_empty() {
                    None
                } else {
                    Some(client_id.trim().to_string())
                },
                scopes: parse_scope_list(&scopes),
            })
        }
    }
}

fn noninteractive_remote_auth_selection(
    auth: Option<String>,
    bearer_token: Option<String>,
    oauth_client_id: Option<String>,
    oauth_scopes: Option<Vec<String>>,
) -> anyhow::Result<Option<RemoteAuthSelection>> {
    let inferred = match (
        auth.as_deref().map(str::trim).filter(|value| !value.is_empty()),
        bearer_token.as_ref(),
        oauth_client_id.as_ref(),
        oauth_scopes.as_ref(),
    ) {
        (Some("none"), None, None, None) => Some(RemoteAuthSelection::None),
        (Some("bearer"), _, _, _) | (None, Some(_), None, None) => {
            let token = bearer_token
                .filter(|token| !token.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("`--bearer-token` is required when auth is bearer"))?;
            Some(RemoteAuthSelection::Bearer { token })
        }
        (Some("oauth"), _, _, _) | (None, None, Some(_), _) | (None, None, None, Some(_)) => {
            if bearer_token.is_some() {
                anyhow::bail!("`--bearer-token` cannot be combined with oauth auth options");
            }
            Some(RemoteAuthSelection::Oauth {
                client_id: oauth_client_id.filter(|value| !value.trim().is_empty()),
                scopes: oauth_scopes.filter(|scopes| !scopes.is_empty()),
            })
        }
        (Some(other), _, _, _) => {
            anyhow::bail!("unsupported auth `{other}`; use `none`, `bearer`, or `oauth`")
        }
        (None, None, None, None) => None,
        (None, Some(_), _, _) => {
            anyhow::bail!("bearer auth options cannot be combined with oauth auth options")
        }
    };

    Ok(inferred)
}

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
            auth,
            bearer_token,
            oauth_client_id,
            oauth_scopes,
            disabled,
        } => cmd_server_add(
            config_path,
            name,
            command,
            url,
            args,
            transport,
            auth,
            bearer_token,
            oauth_client_id,
            oauth_scopes,
            disabled,
        ),
        ServerCommands::Remove { name, yes } => cmd_server_remove(config_path, name, yes),
        ServerCommands::Edit {
            name,
            command,
            url,
            args,
            auth,
            bearer_token,
            oauth_client_id,
            oauth_scopes,
        } => {
            cmd_server_edit(
                config_path,
                name,
                command,
                url,
                args,
                auth,
                bearer_token,
                oauth_client_id,
                oauth_scopes,
                output,
            )
            .await
        }
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
    auth: Option<String>,
    bearer_token: Option<String>,
    oauth_client_id: Option<String>,
    oauth_scopes: Option<Vec<String>>,
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
        provided_transport.is_some()
            || command.is_some()
            || url.is_some()
            || !args.is_empty()
            || auth.is_some()
            || bearer_token.is_some()
            || oauth_client_id.is_some()
            || oauth_scopes.is_some();
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
            if auth.is_some()
                || bearer_token.is_some()
                || oauth_client_id.is_some()
                || oauth_scopes.is_some()
            {
                anyhow::bail!("remote auth flags only apply to HTTP or SSE upstream servers");
            }
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
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
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
            let mut server = plug_core::config::ServerConfig {
                command: None,
                args: Vec::new(),
                env: HashMap::new(),
                enabled,
                transport,
                url: Some(url),
                auth_token: None,
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            };
            if let Some(selection) = noninteractive_remote_auth_selection(
                auth,
                bearer_token,
                oauth_client_id,
                oauth_scopes,
            )? {
                apply_remote_auth_selection(&mut server, selection);
            } else if !non_interactive {
                let selection = prompt_remote_auth_selection(&name, None)?;
                apply_remote_auth_selection(&mut server, selection);
            }
            server
        }
    };

    config.servers.insert(name.clone(), server);
    save_config(&path, &config)?;
    print_success_line(format!("Added server `{name}`."));
    if config
        .servers
        .get(&name)
        .and_then(|server| server.auth.as_deref())
        == Some("oauth")
    {
        print_info_line(format!(
            "Run `plug auth login --server {name}` after saving if this upstream needs authorization."
        ));
    }
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
    command: Option<String>,
    url: Option<String>,
    args: Option<Vec<String>>,
    auth: Option<String>,
    bearer_token: Option<String>,
    oauth_client_id: Option<String>,
    oauth_scopes: Option<Vec<String>>,
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

    let non_interactive = command.is_some()
        || url.is_some()
        || args.is_some()
        || auth.is_some()
        || bearer_token.is_some()
        || oauth_client_id.is_some()
        || oauth_scopes.is_some();

    let oauth_enabled = {
        let server = config
            .servers
            .get_mut(&name)
            .ok_or_else(|| anyhow::anyhow!("unknown server `{name}`"))?;

        if matches!(output, OutputFormat::Json) {
            println!("{}", serde_json::to_string_pretty(server)?);
            return Ok(());
        }

        match server.transport {
            plug_core::config::TransportType::Stdio => {
                if auth.is_some()
                    || bearer_token.is_some()
                    || oauth_client_id.is_some()
                    || oauth_scopes.is_some()
                    || url.is_some()
                {
                    anyhow::bail!(
                        "remote auth and URL flags only apply to HTTP or SSE upstream servers"
                    );
                }
                if non_interactive {
                    if let Some(command) = command {
                        server.command = Some(command);
                    }
                    if let Some(args) = args {
                        server.args = args;
                    }
                } else {
                    let enabled = Confirm::with_theme(&cli_prompt_theme())
                        .with_prompt("Enabled?")
                        .default(server.enabled)
                        .interact()?;
                    server.enabled = enabled;

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
            }
            plug_core::config::TransportType::Http | plug_core::config::TransportType::Sse => {
                if non_interactive {
                    if let Some(url) = url {
                        server.url = Some(url);
                    }
                    if let Some(selection) = noninteractive_remote_auth_selection(
                        auth,
                        bearer_token,
                        oauth_client_id,
                        oauth_scopes,
                    )? {
                        apply_remote_auth_selection(server, selection);
                    }
                } else {
                    let enabled = Confirm::with_theme(&cli_prompt_theme())
                        .with_prompt("Enabled?")
                        .default(server.enabled)
                        .interact()?;
                    server.enabled = enabled;

                    let url: String = Input::with_theme(&cli_prompt_theme())
                        .with_prompt("URL")
                        .with_initial_text(server.url.clone().unwrap_or_default())
                        .interact_text()?;
                    server.url = Some(url);
                    let selection = prompt_remote_auth_selection(&name, Some(server))?;
                    apply_remote_auth_selection(server, selection);
                }
            }
        }

        server.auth.as_deref() == Some("oauth")
    };
    save_config(&path, &config)?;
    print_success_line(format!("Updated server `{name}`."));
    if oauth_enabled {
        print_info_line(format!(
            "Run `plug auth login --server {name}` after saving if this upstream needs fresh authorization."
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_server() -> plug_core::config::ServerConfig {
        plug_core::config::ServerConfig {
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            enabled: true,
            transport: plug_core::config::TransportType::Http,
            url: Some("https://example.com/mcp".to_string()),
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
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

    #[test]
    fn parse_scope_list_ignores_empty_entries() {
        assert_eq!(
            parse_scope_list("read, write, , profile"),
            Some(vec![
                "read".to_string(),
                "write".to_string(),
                "profile".to_string()
            ])
        );
        assert_eq!(parse_scope_list(" , "), None);
    }

    #[test]
    fn apply_remote_auth_selection_sets_bearer_fields() {
        let mut server = remote_server();
        apply_remote_auth_selection(
            &mut server,
            RemoteAuthSelection::Bearer {
                token: "secret".to_string(),
            },
        );
        assert_eq!(server.auth, None);
        assert_eq!(server.auth_token.as_ref().map(|s| s.as_str()), Some("secret"));
        assert_eq!(server.oauth_client_id, None);
        assert_eq!(server.oauth_scopes, None);
    }

    #[test]
    fn apply_remote_auth_selection_sets_oauth_fields() {
        let mut server = remote_server();
        apply_remote_auth_selection(
            &mut server,
            RemoteAuthSelection::Oauth {
                client_id: Some("client-123".to_string()),
                scopes: Some(vec!["read".to_string(), "write".to_string()]),
            },
        );
        assert_eq!(server.auth.as_deref(), Some("oauth"));
        assert!(server.auth_token.is_none());
        assert_eq!(server.oauth_client_id.as_deref(), Some("client-123"));
        assert_eq!(
            server.oauth_scopes,
            Some(vec!["read".to_string(), "write".to_string()])
        );
    }

    #[test]
    fn current_remote_auth_selection_prefers_oauth_state() {
        let mut server = remote_server();
        server.auth = Some("oauth".to_string());
        server.oauth_client_id = Some("client-123".to_string());
        server.oauth_scopes = Some(vec!["read".to_string()]);
        match current_remote_auth_selection(&server) {
            RemoteAuthSelection::Oauth { client_id, scopes } => {
                assert_eq!(client_id.as_deref(), Some("client-123"));
                assert_eq!(scopes, Some(vec!["read".to_string()]));
            }
            other => panic!("expected oauth selection, got {other:?}"),
        }
    }

    #[test]
    fn noninteractive_remote_auth_selection_infers_bearer() {
        let selection = noninteractive_remote_auth_selection(
            None,
            Some("secret".to_string()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            selection,
            Some(RemoteAuthSelection::Bearer {
                token: "secret".to_string()
            })
        );
    }

    #[test]
    fn noninteractive_remote_auth_selection_infers_oauth() {
        let selection = noninteractive_remote_auth_selection(
            None,
            None,
            Some("client-123".to_string()),
            Some(vec!["read".to_string(), "write".to_string()]),
        )
        .unwrap();
        assert_eq!(
            selection,
            Some(RemoteAuthSelection::Oauth {
                client_id: Some("client-123".to_string()),
                scopes: Some(vec!["read".to_string(), "write".to_string()])
            })
        );
    }

    #[test]
    fn noninteractive_remote_auth_selection_rejects_conflicting_flags() {
        let error = noninteractive_remote_auth_selection(
            Some("oauth".to_string()),
            Some("secret".to_string()),
            None,
            None,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("`--bearer-token` cannot be combined with oauth auth options")
        );
    }

    #[test]
    fn noninteractive_remote_auth_selection_none_clears_auth() {
        let selection =
            noninteractive_remote_auth_selection(Some("none".to_string()), None, None, None)
                .unwrap();
        assert_eq!(selection, Some(RemoteAuthSelection::None));
    }
}
