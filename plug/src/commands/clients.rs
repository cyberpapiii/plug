use dialoguer::console::style;
use plug_core::export::{ExportTarget, ExportTransport};

use crate::ui::{cli_prompt_theme, print_banner, print_warning_line};

#[derive(Debug, Clone)]
pub(crate) struct LinkedClientConfig {
    pub(crate) transport: ExportTransport,
    pub(crate) endpoint: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ClientView {
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) linked: bool,
    pub(crate) linked_transport: Option<String>,
    pub(crate) linked_endpoint: Option<String>,
    pub(crate) detected: bool,
    pub(crate) live: bool,
    pub(crate) live_sessions: usize,
    pub(crate) live_transports: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LiveSessionView {
    pub(crate) transport: String,
    pub(crate) client_id: Option<String>,
    pub(crate) session_id: String,
    pub(crate) client_type: String,
    pub(crate) client_info: Option<String>,
    pub(crate) connected_secs: u64,
    pub(crate) last_activity_secs: Option<u64>,
}

pub(crate) fn all_client_targets() -> &'static [(&'static str, &'static str)] {
    &[
        ("Claude Desktop", "claude-desktop"),
        ("Claude Code", "claude-code"),
        ("Cursor", "cursor"),
        ("VS Code Copilot", "vscode"),
        ("Windsurf", "windsurf"),
        ("Gemini CLI", "gemini-cli"),
        ("Codex CLI", "codex-cli"),
        ("OpenCode", "opencode"),
        ("Zed", "zed"),
        ("Cline (VS Code)", "cline"),
        ("Cline CLI", "cline-cli"),
        ("RooCode", "roocode"),
        ("Factory", "factory"),
        ("Nanobot", "nanobot"),
        ("JetBrains Junie", "junie"),
        ("Kilo Code", "kilo"),
        ("Google Antigravity", "antigravity"),
        ("Goose", "goose"),
    ]
}

pub(crate) fn linked_client_targets() -> Vec<String> {
    all_client_targets()
        .iter()
        .filter(|(_, target)| linked_client_config(target, false).is_some())
        .map(|(_, target)| (*target).to_string())
        .collect()
}

pub(crate) fn linked_client_transport(
    target: &str,
    project: bool,
) -> Option<plug_core::export::ExportTransport> {
    linked_client_config(target, project).map(|config| config.transport)
}

pub(crate) fn linked_client_config(target: &str, project: bool) -> Option<LinkedClientConfig> {
    let target_enum: ExportTarget = target.parse().ok()?;
    let path = plug_core::export::default_config_path(target_enum, project)?;
    if !path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    linked_client_config_from_content(&path, target_enum, &content)
}

fn linked_client_config_from_content(
    path: &std::path::Path,
    target_enum: plug_core::export::ExportTarget,
    content: &str,
) -> Option<LinkedClientConfig> {
    let ext = path.extension().and_then(|e| e.to_str());

    match ext {
        Some("toml") => {
            let value = content.parse::<toml::Value>().ok()?;
            let table = value.get("mcp_servers")?.get("plug")?;
            if let Some(url) = table.get("url").and_then(|value| value.as_str()) {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Http,
                    endpoint: Some(url.to_string()),
                })
            } else if table.get("command").is_some() {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Stdio,
                    endpoint: None,
                })
            } else {
                None
            }
        }
        Some("yaml") | Some("yml") => {
            let value = serde_yml::from_str::<serde_yml::Value>(&content).ok()?;
            let plug = value.get("extensions")?.get("plug")?;
            if let Some(uri) = plug.get("uri").and_then(|value| value.as_str()) {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Http,
                    endpoint: Some(uri.to_string()),
                })
            } else if plug.get("command").is_some() {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Stdio,
                    endpoint: None,
                })
            } else {
                None
            }
        }
        _ => {
            let json = serde_json::from_str::<serde_json::Value>(&content).ok()?;
            let plug = match target_enum {
                ExportTarget::Nanobot => json.get("tools")?.get("mcpServers")?.get("plug")?,
                ExportTarget::VSCodeCopilot => json.get("mcp")?.get("servers")?.get("plug")?,
                _ => json
                    .get("mcpServers")
                    .and_then(|s| s.get("plug"))
                    .or_else(|| json.get("context_servers").and_then(|s| s.get("plug")))?,
            };
            if let Some(url) = plug
                .get("url")
                .and_then(|value| value.as_str())
                .or_else(|| plug.get("uri").and_then(|value| value.as_str()))
            {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Http,
                    endpoint: Some(url.to_string()),
                })
            } else if plug.get("command").is_some() {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Stdio,
                    endpoint: None,
                })
            } else {
                None
            }
        }
    }
}

pub(crate) fn is_detected(target: &str) -> bool {
    if let Ok(t) = target.parse::<plug_core::export::ExportTarget>() {
        if let Some(path) = plug_core::export::default_config_path(t, false) {
            let config_exists = path.exists();
            let parent_exists = path.parent().is_some_and(|parent| {
                parent.exists()
                    && !parent.to_string_lossy().ends_with(".config")
                    && parent != dirs::home_dir().unwrap_or_default()
            });
            is_detected_from_signals(t, config_exists, parent_exists)
        } else {
            false
        }
    } else {
        false
    }
}

fn is_detected_from_signals(
    target: plug_core::export::ExportTarget,
    config_exists: bool,
    parent_exists: bool,
) -> bool {
    is_detected_from_signals_with_markers(
        target,
        config_exists,
        parent_exists,
        vscode_app_installed(),
        cline_vscode_marker_exists(),
        cline_cli_marker_exists(),
    )
}

fn is_detected_from_signals_with_markers(
    target: plug_core::export::ExportTarget,
    config_exists: bool,
    parent_exists: bool,
    vscode_installed: bool,
    cline_vscode_marker: bool,
    cline_cli_marker: bool,
) -> bool {
    match target {
        plug_core::export::ExportTarget::VSCodeCopilot => vscode_installed,
        plug_core::export::ExportTarget::Cline => vscode_installed && cline_vscode_marker,
        plug_core::export::ExportTarget::ClineCli => cline_cli_marker,
        _ => config_exists || parent_exists,
    }
}

fn vscode_app_installed() -> bool {
    known_app_paths(&["Visual Studio Code.app"])
        .into_iter()
        .any(|path| path.exists())
}

fn cline_vscode_marker_exists() -> bool {
    dirs::home_dir()
        .map(|home| {
            [
                home.join(
                    "Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev",
                ),
                home.join(
                    ".vscode/extensions/saoudrizwan.claude-dev",
                ),
            ]
            .into_iter()
            .any(|path| path.exists())
        })
        .unwrap_or(false)
}

fn cline_cli_marker_exists() -> bool {
    dirs::home_dir()
        .map(|home| home.join(".cline").exists())
        .unwrap_or(false)
}

fn known_app_paths(app_names: &[&str]) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    for app in app_names {
        paths.push(std::path::PathBuf::from("/Applications").join(app));
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join("Applications").join(app));
        }
    }
    paths
}

fn client_target_from_type(client_type: plug_core::types::ClientType) -> Option<&'static str> {
    match client_type {
        plug_core::types::ClientType::ClaudeDesktop => Some("claude-desktop"),
        plug_core::types::ClientType::ClaudeCode => Some("claude-code"),
        plug_core::types::ClientType::Cursor => Some("cursor"),
        plug_core::types::ClientType::Windsurf => Some("windsurf"),
        plug_core::types::ClientType::VSCodeCopilot => Some("vscode"),
        plug_core::types::ClientType::GeminiCli => Some("gemini-cli"),
        plug_core::types::ClientType::CodexCli => Some("codex-cli"),
        plug_core::types::ClientType::OpenCode => Some("opencode"),
        plug_core::types::ClientType::Zed => Some("zed"),
        plug_core::types::ClientType::Unknown => None,
    }
}

pub(crate) fn client_views(live: &[plug_core::ipc::IpcLiveSessionInfo]) -> Vec<ClientView> {
    let mut live_counts: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    let mut live_transports: std::collections::HashMap<&'static str, std::collections::BTreeSet<&'static str>> =
        std::collections::HashMap::new();
    for session in live {
        if let Some(target) = client_target_from_type(session.client_type) {
            *live_counts.entry(target).or_insert(0) += 1;
            let transport = match session.transport {
                plug_core::ipc::LiveSessionTransport::DaemonProxy => "daemon_proxy",
                plug_core::ipc::LiveSessionTransport::Http => "http",
                plug_core::ipc::LiveSessionTransport::Sse => "sse",
            };
            live_transports
                .entry(target)
                .or_default()
                .insert(transport);
        }
    }

    let mut views = all_client_targets()
        .iter()
        .map(|(name, target)| {
            let linked_config = linked_client_config(target, false);
            let linked_transport = linked_config.as_ref().map(|config| match config.transport {
                plug_core::export::ExportTransport::Stdio => "stdio".to_string(),
                plug_core::export::ExportTransport::Http => "http".to_string(),
            });
            let linked_endpoint = linked_config.and_then(|config| config.endpoint);
            let linked = linked_transport.is_some();
            let detected = is_detected(target);
            let live_sessions = *live_counts.get(target).unwrap_or(&0);
            let live_transports = live_transports
                .get(target)
                .map(|transports| transports.iter().map(|value| (*value).to_string()).collect())
                .unwrap_or_default();
            ClientView {
                name: (*name).to_string(),
                target: (*target).to_string(),
                linked,
                linked_transport,
                linked_endpoint,
                detected,
                live: live_sessions > 0,
                live_sessions,
                live_transports,
            }
        })
        .collect::<Vec<_>>();
    views.sort_by(|a, b| a.name.cmp(&b.name));
    views
}

pub(crate) fn live_session_views(
    live: &[plug_core::ipc::IpcLiveSessionInfo],
) -> Vec<LiveSessionView> {
    let mut views = live
        .iter()
        .map(|session| LiveSessionView {
            transport: match session.transport {
                plug_core::ipc::LiveSessionTransport::DaemonProxy => "daemon_proxy".to_string(),
                plug_core::ipc::LiveSessionTransport::Http => "http".to_string(),
                plug_core::ipc::LiveSessionTransport::Sse => "sse".to_string(),
            },
            client_id: session.client_id.clone(),
            session_id: session.session_id.clone(),
            client_type: session.client_type.to_string(),
            client_info: session.client_info.clone(),
            connected_secs: session.connected_secs,
            last_activity_secs: session.last_activity_secs,
        })
        .collect::<Vec<_>>();
    views.sort_by(|a, b| {
        a.transport
            .cmp(&b.transport)
            .then(a.client_type.cmp(&b.client_type))
            .then(a.session_id.cmp(&b.session_id))
    });
    views
}

pub(crate) fn detected_or_linked_clients() -> Vec<(&'static str, &'static str, bool)> {
    let mut items = Vec::new();
    for (display, target) in all_client_targets() {
        let linked = is_linked(target, false);
        let installed = is_detected(target);
        if linked || installed {
            items.push((*display, *target, linked));
        }
    }
    items
}

fn localhost_export_base(config: &plug_core::config::Config) -> String {
    let scheme = if config.http.tls_cert_path.is_some() && config.http.tls_key_path.is_some() {
        "https"
    } else {
        "http"
    };

    let host = match config.http.bind_address.as_str() {
        "0.0.0.0" | "::" | "[::]" => "localhost",
        bind if plug_core::config::http_bind_is_loopback(bind) => "localhost",
        bind => bind,
    };

    format!("{scheme}://{host}:{}", config.http.port)
}

pub(crate) fn configured_http_export_url(
    config_path: Option<&std::path::PathBuf>,
) -> Option<String> {
    let config = plug_core::config::load_config(config_path).ok()?;
    let base = config
        .http
        .public_base_url
        .clone()
        .unwrap_or_else(|| localhost_export_base(&config));
    let trimmed = base.trim_end_matches('/');
    Some(format!("{trimmed}/mcp"))
}

fn requested_link_transport(
    transport: Option<ExportTransport>,
    yes: bool,
) -> Option<ExportTransport> {
    transport.or(if yes {
        Some(ExportTransport::Stdio)
    } else {
        None
    })
}

fn prompt_link_transport(
    configured_http_url: &str,
    requested_transport: Option<ExportTransport>,
    prompt_label: &str,
    default_http: bool,
) -> anyhow::Result<ExportTransport> {
    use dialoguer::Select;

    if let Some(requested) = requested_transport {
        return Ok(requested);
    }

    let selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt(prompt_label)
        .items([
            "stdio via `plug connect`",
            &format!("HTTP via `{configured_http_url}`"),
        ])
        .default(if default_http { 1 } else { 0 })
        .interact()?;
    Ok(if selection == 1 {
        ExportTransport::Http
    } else {
        ExportTransport::Stdio
    })
}

pub(crate) fn cmd_link(
    config_path: Option<&std::path::PathBuf>,
    targets: Vec<String>,
    all: bool,
    yes: bool,
    transport: Option<ExportTransport>,
) -> anyhow::Result<()> {
    use dialoguer::{Confirm, Input, MultiSelect, Select};
    use plug_core::export::ExportTransport;

    let configured_http_url = configured_http_export_url(config_path)
        .unwrap_or_else(|| "http://localhost:3282/mcp".to_string());
    let requested_transport = requested_link_transport(transport, yes);

    if !targets.is_empty() {
        let transport = prompt_link_transport(
            configured_http_url.as_str(),
            requested_transport,
            "How should selected clients connect to plug?",
            false,
        )?;
        for target in &targets {
            execute_export(
                target,
                matches!(transport, ExportTransport::Http),
                configured_http_url.as_str(),
                true,
                false,
            )?;
        }
        return Ok(());
    }

    print_banner(
        "◆",
        "Link clients",
        "Choose which AI clients should point at plug",
    );

    if all {
        let detected = detected_or_linked_clients();
        if detected.is_empty() {
            anyhow::bail!(
                "no detected clients found; pass explicit targets or run `plug link` interactively"
            );
        }
        let transport = prompt_link_transport(
            configured_http_url.as_str(),
            requested_transport,
            "How should selected clients connect to plug?",
            false,
        )?;
        for target in detected.iter().map(|(_, target, _)| *target) {
            execute_export(
                target,
                matches!(transport, ExportTransport::Http),
                configured_http_url.as_str(),
                true,
                false,
            )?;
        }
        return Ok(());
    }

    let mut items = detected_or_linked_clients()
        .into_iter()
        .map(|(display, target, linked)| {
            let label = if linked {
                format!("{display}  {}", style("[linked]").green().dim())
            } else {
                format!("{display}  {}", style("[detected]").cyan().dim())
            };
            (label, target, display, linked)
        })
        .collect::<Vec<_>>();

    if items.is_empty() {
        print_warning_line("No clients detected.");
        if yes {
            println!(
                "Pass explicit targets like `plug link claude-code cursor` or run `plug link` interactively."
            );
            return Ok(());
        }
        if Confirm::with_theme(&cli_prompt_theme())
            .with_prompt("Show all supported clients?")
            .default(true)
            .interact()?
        {
            for (display, target) in all_client_targets() {
                items.push((
                    display.to_string(),
                    *target,
                    *display,
                    is_linked(target, false),
                ));
            }
        } else {
            return Ok(());
        }
    } else if !yes
        && Confirm::with_theme(&cli_prompt_theme())
            .with_prompt("Show all supported clients?")
            .default(false)
            .interact()?
    {
        items.clear();
        for (display, target) in all_client_targets() {
            let linked = is_linked(target, false);
            let label = if linked {
                format!("{display}  {}", style("[linked]").green().dim())
            } else {
                display.to_string()
            };
            items.push((label, *target, *display, linked));
        }
    }

    let selections = if yes {
        (0..items.len()).collect::<Vec<_>>()
    } else {
        let labels: Vec<_> = items.iter().map(|(l, ..)| l.clone()).collect();
        let defaults: Vec<_> = items.iter().map(|(.., linked)| *linked).collect();
        MultiSelect::with_theme(&cli_prompt_theme())
            .with_prompt("Space to toggle [Linked], Enter to apply")
            .items(&labels)
            .defaults(&defaults)
            .interact()?
    };

    let selected_new_targets = items
        .iter()
        .enumerate()
        .filter(|(idx, (_, _, _, was_linked))| selections.contains(idx) && !was_linked)
        .map(|(_, (_, target, display, _))| (*target, *display))
        .collect::<Vec<_>>();

    let selected_transport = if selected_new_targets.is_empty() {
        None
    } else if let Some(requested) = requested_transport {
        Some(requested)
    } else {
        None
    };

    for (idx, (_, target, _display, was_linked)) in items.iter().enumerate() {
        let is_selected = selections.contains(&idx);
        if is_selected && !was_linked {
            let transport = selected_transport.unwrap_or(prompt_link_transport(
                configured_http_url.as_str(),
                None,
                &format!("How should {} connect to plug?", _display),
                false,
            )?);
            execute_export(
                target,
                matches!(transport, ExportTransport::Http),
                configured_http_url.as_str(),
                true,
                false,
            )?;
        } else if !is_selected && *was_linked {
            execute_unlink(target, false)?;
        }
    }

    if yes {
        return Ok(());
    }

    println!();
    if Confirm::with_theme(&cli_prompt_theme())
        .with_prompt("Configure custom client?")
        .default(false)
        .interact()?
    {
        let path_str: String = Input::with_theme(&cli_prompt_theme())
            .with_prompt("Config path")
            .interact_text()?;
        let path = if let Some(stripped) = path_str.strip_prefix("~/") {
            dirs::home_dir().unwrap().join(stripped)
        } else {
            std::path::PathBuf::from(path_str)
        };
        let format = Select::with_theme(&cli_prompt_theme())
            .with_prompt("Format")
            .items(["JSON", "JSON (VS Code style)", "TOML", "YAML"])
            .default(0)
            .interact()?;
        let transport = prompt_link_transport(
            configured_http_url.as_str(),
            requested_transport,
            "How should this client connect to plug?",
            false,
        )?;
        let (snippet, is_toml, is_yaml) = match (format, transport) {
            (0, ExportTransport::Stdio) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcpServers":{"plug":{"command":"plug","args":["connect"]}}})).unwrap(),
                false,
                false,
            ),
            (0, ExportTransport::Http) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcpServers":{"plug":{"url":configured_http_url}}})).unwrap(),
                false,
                false,
            ),
            (1, ExportTransport::Stdio) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcp":{"servers":{"plug":{"command":"plug","args":["connect"]}}}})).unwrap(),
                false,
                false,
            ),
            (1, ExportTransport::Http) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcp":{"servers":{"plug":{"url":configured_http_url}}}})).unwrap(),
                false,
                false,
            ),
            (2, ExportTransport::Stdio) => (
                "\n[mcp_servers.plug]\ncommand = \"plug\"\nargs = [\"connect\"]\n".to_string(),
                true,
                false,
            ),
            (2, ExportTransport::Http) => (
                format!("\n[mcp_servers.plug]\nurl = \"{configured_http_url}\"\n"),
                true,
                false,
            ),
            (3, ExportTransport::Stdio) => (
                "\nextensions:\n  plug:\n    type: stdio\n    command: plug\n    args: [\"connect\"]\n    enabled: true\n".to_string(),
                false,
                true,
            ),
            (3, ExportTransport::Http) => (
                format!(
                    "\nextensions:\n  plug:\n    type: sse\n    uri: {configured_http_url}\n    enabled: true\n"
                ),
                false,
                true,
            ),
            _ => unreachable!(),
        };
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let existing = if path.exists() {
            std::fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let updated = if is_toml {
            let mut un = plug_core::import::unlink_toml(&existing);
            if !un.ends_with('\n') {
                un.push('\n');
            }
            un.push_str(&snippet);
            un
        } else if is_yaml {
            let mut un = unlink_yaml(&existing);
            if !un.ends_with('\n') {
                un.push('\n');
            }
            un.push_str(&snippet);
            un
        } else {
            merge_json_config(&existing, &snippet)?
        };
        std::fs::write(&path, updated)?;
    }
    Ok(())
}

pub(crate) fn cmd_unlink(targets: Vec<String>, all: bool, yes: bool) -> anyhow::Result<()> {
    use dialoguer::{Confirm, MultiSelect};

    if !targets.is_empty() {
        for target in &targets {
            execute_unlink(target, false)?;
        }
        return Ok(());
    }

    let items = all_client_targets()
        .iter()
        .filter(|(_, target)| is_linked(target, false))
        .map(|(display, target)| (display.to_string(), *target))
        .collect::<Vec<_>>();

    if items.is_empty() {
        print_warning_line("No linked clients found.");
        return Ok(());
    }

    print_banner(
        "◆",
        "Unlink clients",
        "Remove plug from selected AI client configs",
    );

    if all || yes {
        for (_, target) in &items {
            execute_unlink(target, false)?;
        }
        return Ok(());
    }

    if !Confirm::with_theme(&cli_prompt_theme())
        .with_prompt("Choose which linked clients to remove?")
        .default(true)
        .interact()?
    {
        return Ok(());
    }

    let labels = items
        .iter()
        .map(|(display, _)| display.clone())
        .collect::<Vec<_>>();
    let selections = MultiSelect::with_theme(&cli_prompt_theme())
        .with_prompt("Space to toggle, Enter to unlink")
        .items(&labels)
        .defaults(&vec![true; labels.len()])
        .interact()?;

    for index in selections {
        execute_unlink(items[index].1, false)?;
    }

    Ok(())
}

pub(crate) fn execute_unlink(target: &str, project: bool) -> anyhow::Result<()> {
    let target_enum: plug_core::export::ExportTarget =
        target.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let path = plug_core::export::default_config_path(target_enum, project)
        .ok_or_else(|| anyhow::anyhow!("no path"))?;
    if !path.exists() {
        return Ok(());
    }
    let existing = std::fs::read_to_string(&path)?;
    let ext = path.extension().and_then(|e| e.to_str());
    let unlinked = match ext {
        Some("toml") => plug_core::import::unlink_toml(&existing),
        Some("yaml") | Some("yml") => unlink_yaml(&existing),
        _ => unmerge_json_config(&existing)?,
    };
    std::fs::write(&path, unlinked)?;
    Ok(())
}

pub(crate) fn is_linked(target: &str, project: bool) -> bool {
    linked_client_transport(target, project).is_some()
}

fn unmerge_json_config(existing: &str) -> anyhow::Result<String> {
    let mut json: serde_json::Value =
        serde_json::from_str(existing).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = json.as_object_mut() {
        for key in ["mcpServers", "context_servers"] {
            if let Some(inner) = obj.get_mut(key).and_then(|v| v.as_object_mut()) {
                inner.remove("plug");
            }
        }
        if let Some(mcp) = obj.get_mut("mcp").and_then(|v| v.as_object_mut()) {
            if let Some(srv) = mcp.get_mut("servers").and_then(|v| v.as_object_mut()) {
                srv.remove("plug");
            }
        }
        if let Some(tools) = obj.get_mut("tools").and_then(|v| v.as_object_mut()) {
            if let Some(srv) = tools.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
                srv.remove("plug");
            }
        }
    }
    Ok(serde_json::to_string_pretty(&json)?)
}

fn merge_json_config(existing: &str, snippet: &str) -> anyhow::Result<String> {
    let mut existing_json: serde_json::Value =
        serde_json::from_str(existing).unwrap_or_else(|_| serde_json::json!({}));
    let snippet_json: serde_json::Value = serde_json::from_str(snippet)?;
    if let (Some(e_obj), Some(s_obj)) = (existing_json.as_object_mut(), snippet_json.as_object()) {
        for (k, v) in s_obj {
            if k == "mcp" || k == "tools" {
                if let (Some(e_inner), Some(s_inner)) = (
                    e_obj.get_mut(k).and_then(|v| v.as_object_mut()),
                    v.as_object(),
                ) {
                    for (ik, iv) in s_inner {
                        if let (Some(e_deep), Some(s_deep)) = (
                            e_inner.get_mut(ik).and_then(|v| v.as_object_mut()),
                            iv.as_object(),
                        ) {
                            for (dk, dv) in s_deep {
                                e_deep.insert(dk.clone(), dv.clone());
                            }
                        } else {
                            e_inner.insert(ik.clone(), iv.clone());
                        }
                    }
                } else {
                    e_obj.insert(k.clone(), v.clone());
                }
            } else if let (Some(e_inner), Some(s_inner)) = (
                e_obj.get_mut(k).and_then(|v| v.as_object_mut()),
                v.as_object(),
            ) {
                for (ik, iv) in s_inner {
                    e_inner.insert(ik.clone(), iv.clone());
                }
            } else {
                e_obj.insert(k.clone(), v.clone());
            }
        }
    }
    Ok(serde_json::to_string_pretty(&existing_json)?)
}

fn merge_yaml_config(existing: &str, snippet: &str) -> anyhow::Result<String> {
    let mut existing_yml: serde_yml::Value = serde_yml::from_str(existing)
        .unwrap_or_else(|_| serde_yml::Value::Mapping(serde_yml::Mapping::new()));
    let snippet_yml: serde_yml::Value = serde_yml::from_str(snippet)?;

    if let (Some(e_map), Some(s_map)) = (existing_yml.as_mapping_mut(), snippet_yml.as_mapping()) {
        for (k, v) in s_map {
            if let (Some(e_inner), Some(s_inner)) = (
                e_map.get_mut(k).and_then(|v| v.as_mapping_mut()),
                v.as_mapping(),
            ) {
                for (ik, iv) in s_inner {
                    e_inner.insert(ik.clone(), iv.clone());
                }
            } else {
                e_map.insert(k.clone(), v.clone());
            }
        }
    }

    Ok(serde_yml::to_string(&existing_yml)?)
}

pub(crate) fn unlink_yaml(existing: &str) -> String {
    let mut output = Vec::new();
    let mut skipping = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == "plug:"
            || (skipping
                && (trimmed.starts_with("type:")
                    || trimmed.starts_with("command:")
                    || trimmed.starts_with("args:")
                    || trimmed.starts_with("enabled:")
                    || trimmed.starts_with("- ")))
        {
            skipping = true;
            continue;
        }
        if skipping && !line.starts_with(' ') && !trimmed.is_empty() {
            skipping = false;
        }
        if !skipping {
            output.push(line);
        }
    }
    output.join("\n")
}

pub(crate) fn execute_export(
    target: &str,
    http: bool,
    http_url: &str,
    write: bool,
    project: bool,
) -> anyhow::Result<()> {
    use plug_core::export::{ExportOptions, ExportTarget, ExportTransport};
    let target_enum: ExportTarget = target.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let transport = if http {
        ExportTransport::Http
    } else {
        ExportTransport::Stdio
    };

    let command = std::env::current_exe()?.to_string_lossy().to_string();

    let options = ExportOptions {
        target: target_enum,
        transport,
        port: 3282,
        http_url: if http {
            Some(http_url.to_string())
        } else {
            None
        },
        command,
    };
    let snippet = plug_core::export::export_config(&options);
    if write {
        let path = plug_core::export::default_config_path(target_enum, project)
            .ok_or_else(|| anyhow::anyhow!("no path"))?;
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let existing = if path.exists() {
            std::fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let ext = path.extension().and_then(|e| e.to_str());
        let updated = match ext {
            Some("toml") => {
                let mut un = plug_core::import::unlink_toml(&existing);
                if !un.ends_with('\n') {
                    un.push('\n');
                }
                un.push_str(&snippet);
                un
            }
            Some("yaml") | Some("yml") => merge_yaml_config(&existing, &snippet)?,
            _ => merge_json_config(&existing, &snippet)?,
        };
        std::fs::write(&path, updated)?;
    } else {
        println!("{snippet}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plug_core::export::ExportOptions;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("plug-{name}-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn configured_http_export_url_uses_public_base_url_when_present() {
        let path = temp_config_path("public");
        std::fs::write(
            &path,
            r#"[http]
public_base_url = "https://plug.example.com/base"
port = 4444
"#,
        )
        .unwrap();

        assert_eq!(
            configured_http_export_url(Some(&path)).as_deref(),
            Some("https://plug.example.com/base/mcp")
        );
    }

    #[test]
    fn configured_http_export_url_uses_localhost_for_wildcard_bind() {
        let path = temp_config_path("wildcard");
        std::fs::write(
            &path,
            r#"[http]
bind_address = "0.0.0.0"
port = 4444
"#,
        )
        .unwrap();

        assert_eq!(
            configured_http_export_url(Some(&path)).as_deref(),
            Some("http://localhost:4444/mcp")
        );
    }

    #[test]
    fn linked_client_config_reads_json_http_url() {
        let path = std::path::Path::new("config.json");
        let content = r#"{"mcpServers":{"plug":{"url":"https://plug.example.com/mcp"}}}"#;
        let linked = linked_client_config_from_content(
            path,
            plug_core::export::ExportTarget::Cursor,
            content,
        )
        .expect("linked config");
        assert_eq!(linked.transport, plug_core::export::ExportTransport::Http);
        assert_eq!(
            linked.endpoint.as_deref(),
            Some("https://plug.example.com/mcp")
        );
    }

    #[test]
    fn linked_client_config_reads_yaml_http_uri() {
        let path = std::path::Path::new("config.yaml");
        let content = "extensions:\n  plug:\n    type: sse\n    uri: https://plug.example.com/mcp\n    enabled: true\n";
        let linked = linked_client_config_from_content(
            path,
            plug_core::export::ExportTarget::Goose,
            content,
        )
        .expect("linked config");
        assert_eq!(linked.transport, plug_core::export::ExportTransport::Http);
        assert_eq!(
            linked.endpoint.as_deref(),
            Some("https://plug.example.com/mcp")
        );
    }

    #[test]
    fn linked_client_config_reads_toml_http_url() {
        let path = std::path::Path::new("config.toml");
        let content =
            "[mcp_servers.plug]\ntransport = \"http\"\nurl = \"https://plug.example.com/mcp\"\n";
        let linked = linked_client_config_from_content(
            path,
            plug_core::export::ExportTarget::CodexCli,
            content,
        )
        .expect("linked config");
        assert_eq!(linked.transport, plug_core::export::ExportTransport::Http);
        assert_eq!(
            linked.endpoint.as_deref(),
            Some("https://plug.example.com/mcp")
        );
    }

    #[test]
    fn export_and_parse_round_trip_cursor_http_endpoint() {
        let output = plug_core::export::export_config(&ExportOptions {
            target: plug_core::export::ExportTarget::Cursor,
            transport: plug_core::export::ExportTransport::Http,
            port: 3282,
            http_url: Some("https://plug.example.com/mcp".to_string()),
            command: "plug".to_string(),
        });
        let linked = linked_client_config_from_content(
            std::path::Path::new("config.json"),
            plug_core::export::ExportTarget::Cursor,
            &output,
        )
        .expect("linked config");
        assert_eq!(linked.transport, plug_core::export::ExportTransport::Http);
        assert_eq!(
            linked.endpoint.as_deref(),
            Some("https://plug.example.com/mcp")
        );
    }

    #[test]
    fn export_and_parse_round_trip_codex_http_endpoint() {
        let output = plug_core::export::export_config(&ExportOptions {
            target: plug_core::export::ExportTarget::CodexCli,
            transport: plug_core::export::ExportTransport::Http,
            port: 3282,
            http_url: Some("https://plug.example.com/mcp".to_string()),
            command: "plug".to_string(),
        });
        let linked = linked_client_config_from_content(
            std::path::Path::new("config.toml"),
            plug_core::export::ExportTarget::CodexCli,
            &output,
        )
        .expect("linked config");
        assert_eq!(linked.transport, plug_core::export::ExportTransport::Http);
        assert_eq!(
            linked.endpoint.as_deref(),
            Some("https://plug.example.com/mcp")
        );
    }

    #[test]
    fn export_and_parse_round_trip_goose_http_endpoint() {
        let output = plug_core::export::export_config(&ExportOptions {
            target: plug_core::export::ExportTarget::Goose,
            transport: plug_core::export::ExportTransport::Http,
            port: 3282,
            http_url: Some("https://plug.example.com/mcp".to_string()),
            command: "plug".to_string(),
        });
        let linked = linked_client_config_from_content(
            std::path::Path::new("config.yaml"),
            plug_core::export::ExportTarget::Goose,
            &output,
        )
        .expect("linked config");
        assert_eq!(linked.transport, plug_core::export::ExportTransport::Http);
        assert_eq!(
            linked.endpoint.as_deref(),
            Some("https://plug.example.com/mcp")
        );
    }

    #[test]
    fn requested_link_transport_prefers_explicit_http_even_with_yes() {
        assert_eq!(
            requested_link_transport(Some(ExportTransport::Http), true),
            Some(ExportTransport::Http)
        );
    }

    #[test]
    fn requested_link_transport_defaults_yes_to_stdio() {
        assert_eq!(
            requested_link_transport(None, true),
            Some(ExportTransport::Stdio)
        );
        assert_eq!(requested_link_transport(None, false), None);
    }

    #[test]
    fn requested_link_transport_defaults_explicit_targets_to_prompt() {
        assert_eq!(requested_link_transport(None, false), None);
    }

    #[test]
    fn detection_requires_real_vscode_install_markers() {
        assert!(!is_detected_from_signals_with_markers(
            plug_core::export::ExportTarget::VSCodeCopilot,
            true,
            true,
            false,
            false,
            false,
        ));
    }

    #[test]
    fn detection_requires_real_cline_markers() {
        assert!(!is_detected_from_signals_with_markers(
            plug_core::export::ExportTarget::Cline,
            true,
            true,
            false,
            true,
            false,
        ));
        assert!(!is_detected_from_signals_with_markers(
            plug_core::export::ExportTarget::ClineCli,
            true,
            true,
            false,
            false,
            false,
        ));
    }

    #[test]
    fn detection_keeps_parent_fallback_for_other_clients() {
        assert!(is_detected_from_signals_with_markers(
            plug_core::export::ExportTarget::Cursor,
            false,
            true,
            false,
            false,
            false,
        ));
    }
}
