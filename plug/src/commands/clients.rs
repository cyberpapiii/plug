use dialoguer::console::style;

use crate::ui::{cli_prompt_theme, print_banner, print_warning_line};

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ClientView {
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) linked: bool,
    pub(crate) detected: bool,
    pub(crate) live: bool,
    pub(crate) live_sessions: usize,
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
        .filter(|(_, target)| linked_client_transport(target, false).is_some())
        .map(|(_, target)| (*target).to_string())
        .collect()
}

pub(crate) fn linked_client_transport(
    target: &str,
    project: bool,
) -> Option<plug_core::export::ExportTransport> {
    use plug_core::export::{ExportTarget, ExportTransport};

    let target_enum: ExportTarget = target.parse().ok()?;
    let path = plug_core::export::default_config_path(target_enum, project)?;
    if !path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    let ext = path.extension().and_then(|e| e.to_str());

    match ext {
        Some("toml") => {
            let value = content.parse::<toml::Value>().ok()?;
            let table = value.get("mcp_servers")?.get("plug")?;
            if table.get("url").is_some() {
                Some(ExportTransport::Http)
            } else if table.get("command").is_some() {
                Some(ExportTransport::Stdio)
            } else {
                None
            }
        }
        Some("yaml") | Some("yml") => {
            let value = serde_yml::from_str::<serde_yml::Value>(&content).ok()?;
            let plug = value.get("extensions")?.get("plug")?;
            if plug.get("uri").is_some() {
                Some(ExportTransport::Http)
            } else if plug.get("command").is_some() {
                Some(ExportTransport::Stdio)
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
            if plug.get("url").is_some() || plug.get("uri").is_some() {
                Some(ExportTransport::Http)
            } else if plug.get("command").is_some() {
                Some(ExportTransport::Stdio)
            } else {
                None
            }
        }
    }
}

pub(crate) fn is_detected(target: &str) -> bool {
    if let Ok(t) = target.parse::<plug_core::export::ExportTarget>() {
        if let Some(path) = plug_core::export::default_config_path(t, false) {
            if path.exists() {
                true
            } else if let Some(parent) = path.parent() {
                parent.exists()
                    && !parent.to_string_lossy().ends_with(".config")
                    && parent != dirs::home_dir().unwrap_or_default()
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    }
}

fn client_target_from_info(client_info: Option<&str>) -> Option<&'static str> {
    let info = client_info?;
    match plug_core::client_detect::detect_client(info) {
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

pub(crate) fn client_views(live: &[plug_core::ipc::IpcClientInfo]) -> Vec<ClientView> {
    let mut live_counts: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    for session in live {
        if let Some(target) = client_target_from_info(session.client_info.as_deref()) {
            *live_counts.entry(target).or_insert(0) += 1;
        }
    }

    let mut views = all_client_targets()
        .iter()
        .map(|(name, target)| {
            let linked = is_linked(target, false);
            let detected = is_detected(target);
            let live_sessions = *live_counts.get(target).unwrap_or(&0);
            ClientView {
                name: (*name).to_string(),
                target: (*target).to_string(),
                linked,
                detected,
                live: live_sessions > 0,
                live_sessions,
            }
        })
        .collect::<Vec<_>>();
    views.sort_by(|a, b| a.name.cmp(&b.name));
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

pub(crate) fn cmd_link(targets: Vec<String>, all: bool, yes: bool) -> anyhow::Result<()> {
    use dialoguer::{Confirm, Input, MultiSelect, Select};
    use plug_core::export::ExportTransport;

    let prompt_transport = |default_http: bool| -> anyhow::Result<ExportTransport> {
        if yes {
            return Ok(ExportTransport::Stdio);
        }

        let selection = Select::with_theme(&cli_prompt_theme())
            .with_prompt("How should selected clients connect to plug?")
            .items([
                "stdio via `plug connect`",
                "HTTP via `http://localhost:3282/mcp`",
            ])
            .default(if default_http { 1 } else { 0 })
            .interact()?;
        Ok(if selection == 1 {
            ExportTransport::Http
        } else {
            ExportTransport::Stdio
        })
    };

    if !targets.is_empty() {
        let transport = prompt_transport(false)?;
        for target in &targets {
            execute_export(
                target,
                matches!(transport, ExportTransport::Http),
                3282,
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
        let transport = prompt_transport(false)?;
        for target in detected.iter().map(|(_, target, _)| *target) {
            execute_export(
                target,
                matches!(transport, ExportTransport::Http),
                3282,
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
        .map(|(_, (_, target, _, _))| *target)
        .collect::<Vec<_>>();

    let selected_transport = if selected_new_targets.is_empty() {
        None
    } else {
        Some(prompt_transport(false)?)
    };

    for (idx, (_, target, _display, was_linked)) in items.iter().enumerate() {
        let is_selected = selections.contains(&idx);
        if is_selected && !was_linked {
            execute_export(
                target,
                matches!(selected_transport, Some(ExportTransport::Http)),
                3282,
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
        let transport = prompt_transport(false)?;
        let (snippet, is_toml, is_yaml) = match (format, transport) {
            (0, ExportTransport::Stdio) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcpServers":{"plug":{"command":"plug","args":["connect"]}}})).unwrap(),
                false,
                false,
            ),
            (0, ExportTransport::Http) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcpServers":{"plug":{"url":"http://localhost:3282/mcp"}}})).unwrap(),
                false,
                false,
            ),
            (1, ExportTransport::Stdio) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcp":{"servers":{"plug":{"command":"plug","args":["connect"]}}}})).unwrap(),
                false,
                false,
            ),
            (1, ExportTransport::Http) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcp":{"servers":{"plug":{"url":"http://localhost:3282/mcp"}}}})).unwrap(),
                false,
                false,
            ),
            (2, ExportTransport::Stdio) => (
                "\n[mcp_servers.plug]\ncommand = \"plug\"\nargs = [\"connect\"]\n".to_string(),
                true,
                false,
            ),
            (2, ExportTransport::Http) => (
                "\n[mcp_servers.plug]\nurl = \"http://localhost:3282/mcp\"\n".to_string(),
                true,
                false,
            ),
            (3, ExportTransport::Stdio) => (
                "\nextensions:\n  plug:\n    type: stdio\n    command: plug\n    args: [\"connect\"]\n    enabled: true\n".to_string(),
                false,
                true,
            ),
            (3, ExportTransport::Http) => (
                "\nextensions:\n  plug:\n    type: sse\n    uri: http://localhost:3282/mcp\n    enabled: true\n".to_string(),
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
    port: u16,
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
        port,
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
