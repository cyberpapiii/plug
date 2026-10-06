use dialoguer::console::style;
use plug_core::export::{ExportTarget, ExportTransport};
use std::path::{Path, PathBuf};

use crate::ui::{cli_prompt_theme, print_banner, print_info_line, print_warning_line};

#[derive(Debug, Clone)]
pub(crate) struct LinkedClientConfig {
    pub(crate) transport: ExportTransport,
    pub(crate) endpoint: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) args: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlugLinkDisposition {
    Canonical,
    RecognizedLegacy,
    UnknownCommand,
    Http,
    Missing,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientRepairItem {
    pub target: String,
    pub path: PathBuf,
    pub disposition: PlugLinkDisposition,
    pub changed: bool,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientRepairReport {
    pub canonical_command: PathBuf,
    pub items: Vec<ClientRepairItem>,
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
    pub(crate) lazy_tool_mode: String,
    pub(crate) lazy_tool_mode_origin: String,
    pub(crate) lazy_tool_mode_reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LiveSessionView {
    pub(crate) transport: String,
    pub(crate) client_id: Option<String>,
    pub(crate) session_id: String,
    pub(crate) client_type: String,
    pub(crate) client_info: Option<String>,
    /// The program that started a local connector, read from the process
    /// table rather than from what the client says about itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) host: Option<plug_core::ipc::ClientHost>,
    /// What `plug clients rename` takes to name this client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) key: Option<String>,
    /// The name the owner gave this client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// Where the owner says this client runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) place: Option<String>,
    /// The name a remote client signed in under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) grant_name: Option<String>,
    /// The key this session's requests carry, which blocks are stored under.
    /// It is `key` for every session but a remote one with no grant.
    #[serde(skip)]
    pub(crate) access_key: Option<String>,
    pub(crate) connected_secs: u64,
    pub(crate) last_activity_secs: Option<u64>,
}

impl LiveSessionView {
    /// What to call the session: the name the owner gave it, else the client
    /// Plug recognised, else the client its link was written for, else the
    /// name a remote client signed in under, else the program that started
    /// it.
    pub(crate) fn label(&self) -> &str {
        if let Some(name) = &self.name {
            return name;
        }
        if self.client_type != "Unknown" {
            return &self.client_type;
        }
        let linked = self.key.as_deref().and_then(|key| {
            all_client_targets()
                .iter()
                .find(|(_, target)| *target == key)
                .map(|(name, _)| *name)
        });
        match (linked, &self.grant_name, &self.host) {
            (Some(name), _, _) => name,
            (None, Some(name), _) => name,
            (None, None, Some(host)) => &host.name,
            (None, None, None) => &self.client_type,
        }
    }
}

#[derive(clap::Subcommand)]
pub(crate) enum ClientCommands {
    /// Give a client a name of your choosing ("" goes back to Plug's name)
    Rename {
        /// The client: its key from `plug clients -v`, or the name it shows
        /// under while connected
        client: String,
        /// The new name
        name: String,
    },
    /// Say where a client runs, such as "Work laptop" ("" takes it back)
    Place {
        /// The client: its key from `plug clients -v`, or the name it shows
        /// under while connected
        client: String,
        /// Where it runs, in your own words
        place: String,
    },
    /// Keep a client from a server or from single tools
    Block {
        /// The client: its key from `plug clients -v`, or the name it shows
        /// under while connected
        client: String,
        /// A server to keep it from, by its name in `plug servers`
        #[arg(long = "server", value_name = "SERVER")]
        servers: Vec<String>,
        /// A tool to keep it from, by the name in `plug tools`; `*` matches
        /// any run of characters
        #[arg(long = "tool", value_name = "TOOL")]
        tools: Vec<String>,
    },
    /// Let a client back in to a server or a tool
    Unblock {
        /// The client: its key from `plug clients -v`, or the name it shows
        /// under while connected
        client: String,
        /// A server to let it back in to
        #[arg(long = "server", value_name = "SERVER")]
        servers: Vec<String>,
        /// A tool to let it back in to, written as it was blocked
        #[arg(long = "tool", value_name = "TOOL")]
        tools: Vec<String>,
    },
}

/// Work out which client `plug clients rename` means.
///
/// A connected client can be picked by the name it shows under or by the
/// start of a session id. Anything else has to be a key, since a name only
/// means something while its client is connected.
pub(crate) fn resolve_client_key(
    wanted: &str,
    sessions: &[LiveSessionView],
) -> anyhow::Result<String> {
    resolve_client_key_by(wanted, sessions, |session| session.key.clone())
}

/// Work out which client `plug clients block` means: the same search, but
/// the answer is the key the client's requests carry.
pub(crate) fn resolve_client_access_key(
    wanted: &str,
    sessions: &[LiveSessionView],
) -> anyhow::Result<String> {
    resolve_client_key_by(wanted, sessions, |session| session.access_key.clone())
}

fn resolve_client_key_by(
    wanted: &str,
    sessions: &[LiveSessionView],
    key_of: impl Fn(&LiveSessionView) -> Option<String>,
) -> anyhow::Result<String> {
    let mut keys = sessions
        .iter()
        .filter(|session| {
            session.label().eq_ignore_ascii_case(wanted)
                || session.key.as_deref() == Some(wanted)
                || session.access_key.as_deref() == Some(wanted)
                || session.session_id.starts_with(wanted)
        })
        .map(key_of)
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    match keys.as_slice() {
        [Some(key)] => Ok(key.clone()),
        [None] => anyhow::bail!(
            "Plug cannot tell `{wanted}` apart from other clients, so it has nothing to store a name under"
        ),
        [] if wanted.contains(':') || wanted.parse::<ExportTarget>().is_ok() => {
            Ok(wanted.to_string())
        }
        [] => anyhow::bail!(
            "no connected client is called `{wanted}`; run `plug clients -v` to see each client's key"
        ),
        _ => anyhow::bail!(
            "more than one connected client matches `{wanted}`; use a key from `plug clients -v`"
        ),
    }
}

pub(crate) async fn cmd_client_rename(
    config_path: Option<&PathBuf>,
    client: String,
    name: String,
) -> anyhow::Result<()> {
    let (live, _, _) = crate::runtime::fetch_live_sessions(config_path).await;
    // Names already given are loaded too, so a client can be picked by one.
    let config = plug_core::config::load_config(config_path).ok();
    let key = resolve_client_key(
        &client,
        &live_session_views(&live, config.as_ref(), &downstream_grants().await),
    )?;
    let name = name.trim().to_string();
    crate::commands::servers::apply_server_mutation(
        config_path,
        plug_core::operator::OperatorMutation::RenameClient {
            key: key.clone(),
            name: name.clone(),
        },
    )
    .await?;
    if name.is_empty() {
        print_info_line(format!("{key} goes by the name Plug works out again."));
    } else {
        print_info_line(format!("{key} is now called {name}."));
    }
    Ok(())
}

pub(crate) async fn cmd_client_place(
    config_path: Option<&PathBuf>,
    client: String,
    place: String,
) -> anyhow::Result<()> {
    let (live, _, _) = crate::runtime::fetch_live_sessions(config_path).await;
    let config = plug_core::config::load_config(config_path).ok();
    let key = resolve_client_key(
        &client,
        &live_session_views(&live, config.as_ref(), &downstream_grants().await),
    )?;
    let place = place.trim().to_string();
    crate::commands::servers::apply_server_mutation(
        config_path,
        plug_core::operator::OperatorMutation::SetClientPlace {
            key: key.clone(),
            place: place.clone(),
        },
    )
    .await?;
    if place.is_empty() {
        print_info_line(format!("{key} no longer says where it runs."));
    } else {
        print_info_line(format!("{key} runs on {place}."));
    }
    Ok(())
}

/// `plug clients block` and `unblock`: one mutation per server or tool named,
/// then what the client is kept from now.
pub(crate) async fn cmd_client_block(
    config_path: Option<&PathBuf>,
    client: String,
    servers: Vec<String>,
    tools: Vec<String>,
    blocked: bool,
) -> anyhow::Result<()> {
    use plug_core::operator::ClientBlockKind;

    if servers.is_empty() && tools.is_empty() {
        anyhow::bail!("say what with --server <name> or --tool <name>");
    }
    let (live, _, _) = crate::runtime::fetch_live_sessions(config_path).await;
    let config = plug_core::config::load_config(config_path).ok();
    let key = resolve_client_access_key(
        &client,
        &live_session_views(&live, config.as_ref(), &downstream_grants().await),
    )?;
    let targets = servers
        .into_iter()
        .map(|server| (ClientBlockKind::Server, server))
        .chain(tools.into_iter().map(|tool| (ClientBlockKind::Tool, tool)));
    for (kind, target) in targets {
        crate::commands::servers::apply_server_mutation(
            config_path,
            plug_core::operator::OperatorMutation::SetClientBlock {
                key: key.clone(),
                kind,
                target,
                blocked,
            },
        )
        .await?;
    }

    let settings = plug_core::config::load_config(config_path)
        .ok()
        .and_then(|config| config.clients.get(&key).cloned())
        .unwrap_or_default();
    print_info_line(client_blocks_line(&key, &settings));
    if !key.starts_with("oauth:") {
        print_info_line(
            style("This keeps the tool list tidy. It is not a security boundary: only a remote client's grant is verified.").dim(),
        );
    }
    Ok(())
}

/// One line saying what a client is kept from.
pub(crate) fn client_blocks_line(
    key: &str,
    settings: &plug_core::config::ClientSettings,
) -> String {
    let mut parts = Vec::new();
    if !settings.blocked_servers.is_empty() {
        parts.push(format!("servers {}", settings.blocked_servers.join(", ")));
    }
    if !settings.blocked_tools.is_empty() {
        let tools: Vec<String> = settings
            .blocked_tools
            .iter()
            .map(ToString::to_string)
            .collect();
        parts.push(format!("tools {}", tools.join(", ")));
    }
    if parts.is_empty() {
        format!("{key} is kept from nothing.")
    } else {
        format!("{key} is kept from {}.", parts.join("; "))
    }
}

pub(crate) fn all_client_targets() -> &'static [(&'static str, &'static str)] {
    &[
        ("Claude Desktop", "claude-desktop"),
        ("Claude Code", "claude-code"),
        ("Cursor", "cursor"),
        ("VS Code Copilot", "vscode"),
        ("GitHub Copilot CLI", "copilot-cli"),
        ("Devin", "devin"),
        ("Gemini CLI", "gemini-cli"),
        ("Codex CLI", "codex-cli"),
        // Grok Bot is not here: it reaches Plug over the public internet
        // only, so there is no file on this Mac to link.
        ("Grok Build", "grok-build"),
        ("OpenCode", "opencode"),
        ("Zed", "zed"),
        ("Cline (VS Code)", "cline"),
        ("Cline CLI", "cline-cli"),
        ("Factory", "factory"),
        ("Nanobot", "nanobot"),
        ("JetBrains Junie", "junie"),
        ("Kilo Code", "kilo"),
        ("Pi", "pi"),
        ("Warp", "warp"),
        ("Kiro", "kiro"),
        ("Kimi Code", "kimi-code"),
        ("Qwen Code", "qwen-code"),
        ("Google Antigravity", "antigravity"),
        ("Goose", "goose"),
        ("Hermes Agent", "hermes"),
        ("Amp", "amp"),
        ("OpenClaw", "openclaw"),
        ("LM Studio", "lm-studio"),
        ("Muse Code", "muse-code"),
    ]
}

pub(crate) fn client_display_name(target: &str) -> &str {
    all_client_targets()
        .iter()
        .find(|(_, candidate)| *candidate == target)
        .map(|(name, _)| *name)
        .unwrap_or(target)
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

pub(crate) fn linked_client_config_from_content(
    path: &Path,
    target_enum: plug_core::export::ExportTarget,
    content: &str,
) -> Option<LinkedClientConfig> {
    let ext = path.extension().and_then(|e| e.to_str());

    match ext {
        Some("toml") => {
            let value = toml::from_str::<toml::Value>(content).ok()?;
            let table = value.get("mcp_servers")?.get("plug")?;
            if let Some(url) = table.get("url").and_then(|value| value.as_str()) {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Http,
                    endpoint: Some(url.to_string()),
                    command: None,
                    args: None,
                })
            } else if table.get("command").is_some() {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Stdio,
                    endpoint: None,
                    command: table
                        .get("command")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned),
                    args: table.get("args").and_then(toml_string_args),
                })
            } else {
                None
            }
        }
        Some("yaml") | Some("yml") => {
            let value = serde_norway::from_str::<serde_norway::Value>(content).ok()?;
            let plug = value
                .get(yaml_servers_key(target_enum))?
                .as_mapping()?
                .iter()
                .find(|(name, _)| {
                    name.as_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case("plug"))
                })
                .map(|(_, entry)| entry)?;
            if let Some(uri) = plug
                .get("uri")
                .or_else(|| plug.get("url"))
                .and_then(|value| value.as_str())
            {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Http,
                    endpoint: Some(uri.to_string()),
                    command: None,
                    args: None,
                })
            } else if plug.get("command").is_some() {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Stdio,
                    endpoint: None,
                    command: plug
                        .get("command")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned),
                    args: plug.get("args").and_then(yaml_string_args),
                })
            } else {
                None
            }
        }
        _ => {
            let json = serde_json::from_str::<serde_json::Value>(&content).ok()?;
            let plug = match target_enum {
                ExportTarget::Nanobot => json.get("tools")?.get("mcpServers")?.get("plug")?,
                ExportTarget::VSCodeCopilot => json.get("servers")?.get("plug")?,
                ExportTarget::Amp => json.get("amp.mcpServers")?.get("plug")?,
                ExportTarget::OpenClaw => json.get("mcp")?.get("servers")?.get("plug")?,
                ExportTarget::MuseCode => json.get("mcp_servers")?.get("plug")?,
                ExportTarget::OpenCode | ExportTarget::Kilo => json.get("mcp")?.get("plug")?,
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
                    command: None,
                    args: None,
                })
            } else if let Some(mut words) = plug.get("command").and_then(json_string_args) {
                // OpenCode: the command and its arguments are one list.
                let args = words.split_off(words.len().min(1));
                Some(LinkedClientConfig {
                    transport: ExportTransport::Stdio,
                    endpoint: None,
                    command: words.pop(),
                    args: Some(args),
                })
            } else if plug.get("command").is_some() {
                Some(LinkedClientConfig {
                    transport: ExportTransport::Stdio,
                    endpoint: None,
                    command: plug
                        .get("command")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned),
                    args: plug.get("args").and_then(json_string_args),
                })
            } else {
                None
            }
        }
    }
}

fn toml_string_args(value: &toml::Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect()
}

/// The top-level key a YAML client keeps its servers under.
fn yaml_servers_key(target: ExportTarget) -> &'static str {
    match target {
        ExportTarget::Hermes => "mcp_servers",
        _ => "extensions",
    }
}

/// The lines of the Plug entry under `key`: the `plug:` line and one past the
/// entry's last line. Hermes Agent links written by hand are often `Plug:`.
fn yaml_plug_block(lines: &[String], key: &str) -> Option<(usize, usize)> {
    let heading = format!("{key}:");
    let parent = lines.iter().position(|line| line.trim_end() == heading)?;
    let section_end = lines[parent + 1..]
        .iter()
        .position(|line| !line.trim().is_empty() && indentation(line) == 0)
        .map(|offset| parent + 1 + offset)
        .unwrap_or(lines.len());
    let plug = (parent + 1..section_end)
        .find(|&index| lines[index].trim().eq_ignore_ascii_case("plug:"))?;
    let plug_indent = indentation(&lines[plug]);
    let end = (plug + 1..section_end)
        .find(|&index| !lines[index].trim().is_empty() && indentation(&lines[index]) <= plug_indent)
        .unwrap_or(section_end);
    Some((plug, end))
}

/// Put `snippet`'s Plug entry into a YAML file as text, so comments, key
/// order, and every other line of the file stay exactly as they were.
///
/// `snippet` is a heading line, `<key>:`, followed by the entry indented two
/// spaces.
fn link_yaml_text(existing: &str, snippet: &str) -> anyhow::Result<String> {
    let (heading, entry) = snippet
        .split_once('\n')
        .ok_or_else(|| anyhow::anyhow!("empty YAML entry"))?;
    let key = heading.trim_end().trim_end_matches(':');
    let mut lines = unlink_yaml(existing, key)
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    match lines.iter().position(|line| line.trim_end() == heading) {
        Some(parent) => {
            // Match the indentation the file's other servers already use.
            let indent = lines[parent + 1..]
                .iter()
                .find(|line| !line.trim().is_empty())
                .map(|line| indentation(line))
                .filter(|indent| *indent > 0)
                .unwrap_or(2);
            let pad = " ".repeat(indent.saturating_sub(2));
            let entry = entry.lines().map(|line| format!("{pad}{line}"));
            lines.splice(parent + 1..parent + 1, entry);
        }
        None if lines
            .iter()
            .any(|line| indentation(line) == 0 && line.starts_with(heading)) =>
        {
            anyhow::bail!("`{heading}` is written on one line; add Plug to it by hand");
        }
        None => lines.extend(snippet.lines().map(str::to_string)),
    }
    let updated = lines.join("\n") + "\n";
    serde_norway::from_str::<serde_norway::Value>(&updated)
        .map_err(|error| anyhow::anyhow!("the file would not be valid YAML: {error}"))?;
    Ok(updated)
}

fn yaml_string_args(value: &serde_norway::Value) -> Option<Vec<String>> {
    value
        .as_sequence()?
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect()
}

fn json_string_args(value: &serde_json::Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect()
}

/// Classify a stdio `plug` entry without executing its configured command.
///
/// Repair is intentionally limited to paths that can only reasonably be a
/// Plug installation. A bare `plug` command or an arbitrary executable might
/// be another program, so it remains untouched.
pub fn classify_plug_client_command(
    command: &str,
    args: &[String],
    canonical: &Path,
) -> PlugLinkDisposition {
    if !plug_core::export::is_connect_args(args) {
        return PlugLinkDisposition::UnknownCommand;
    }

    let command_path = Path::new(command);
    if paths_equivalent(command_path, canonical) {
        return PlugLinkDisposition::Canonical;
    }

    let identified_path = command_path
        .canonicalize()
        .unwrap_or_else(|_| command_path.into());
    if is_recognized_legacy_plug_path(&identified_path) {
        PlugLinkDisposition::RecognizedLegacy
    } else {
        PlugLinkDisposition::UnknownCommand
    }
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn is_recognized_legacy_plug_path(path: &Path) -> bool {
    if crate::service::is_recognized_legacy_program(path) {
        return true;
    }

    let path_components = path.components().collect::<Vec<_>>();
    let has_suffix = |suffix: &[&str]| {
        path_components.len() >= suffix.len()
            && path_components[path_components.len() - suffix.len()..]
                .iter()
                .map(|component| component.as_os_str())
                .eq(suffix.iter().map(std::ffi::OsStr::new))
    };

    let home = dirs::home_dir();
    let is_home_legacy = home
        .as_ref()
        .is_some_and(|home| path == home.join(".local/bin/plug"));
    let is_old_app = path == Path::new("/Applications/Plug.app/Contents/Resources/plug")
        || home
            .as_ref()
            .is_some_and(|home| path == home.join("Applications/Plug.app/Contents/Resources/plug"));

    is_home_legacy
        || is_old_app
        || has_suffix(&["target", "debug", "plug"])
        || has_suffix(&["target", "release", "plug"])
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
                home.join(".vscode/extensions/saoudrizwan.claude-dev"),
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
    client_type.target_slug()
}

pub(crate) fn lazy_tool_policy_for_target(
    config: Option<&plug_core::config::Config>,
    target: &str,
) -> plug_core::types::ResolvedLazyToolPolicy {
    if let Some(config) = config {
        plug_core::config::resolve_lazy_tool_policy_for_target(config, target)
    } else {
        let config = plug_core::config::Config::default();
        plug_core::config::resolve_lazy_tool_policy_for_target(&config, target)
    }
}

fn lazy_tool_policy_summary(config: &plug_core::config::Config, target: &str) -> String {
    let policy = plug_core::config::resolve_lazy_tool_policy_for_target(config, target);
    format!(
        "{} lazy tools: {} ({}) - {}",
        client_display_name(target),
        policy.mode.label(),
        policy.origin.label(),
        policy.reason
    )
}

fn print_lazy_tool_policy(config: &plug_core::config::Config, target: &str) {
    print_info_line(style(lazy_tool_policy_summary(config, target)).dim());
}

pub(crate) fn client_views(
    live: &[plug_core::ipc::IpcLiveSessionInfo],
    config: Option<&plug_core::config::Config>,
) -> Vec<ClientView> {
    let mut live_counts: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    let mut live_transports: std::collections::HashMap<
        &'static str,
        std::collections::BTreeSet<&'static str>,
    > = std::collections::HashMap::new();
    for session in live {
        if let Some(target) = client_target_from_type(session.client_type) {
            *live_counts.entry(target).or_insert(0) += 1;
            let transport = match session.transport {
                plug_core::ipc::LiveSessionTransport::DaemonProxy => "daemon_proxy",
                plug_core::ipc::LiveSessionTransport::Http => "http",
                plug_core::ipc::LiveSessionTransport::Sse => "sse",
            };
            live_transports.entry(target).or_default().insert(transport);
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
                .map(|transports| {
                    transports
                        .iter()
                        .map(|value| (*value).to_string())
                        .collect()
                })
                .unwrap_or_default();
            let lazy_policy = lazy_tool_policy_for_target(config, target);
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
                lazy_tool_mode: lazy_policy.mode.label().to_string(),
                lazy_tool_mode_origin: lazy_policy.origin.label().to_string(),
                lazy_tool_mode_reason: lazy_policy.reason,
            }
        })
        .collect::<Vec<_>>();
    views.sort_by(|a, b| a.name.cmp(&b.name));
    views
}

/// The remote clients that signed in, as the daemon lists them. Empty when
/// the daemon cannot be asked: the names are a nicety, not a need.
pub(crate) async fn downstream_grants() -> Vec<plug_core::downstream_oauth::RegisteredClientSummary>
{
    let Ok(auth_token) = crate::daemon::read_auth_token() else {
        return Vec::new();
    };
    match crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::OperatorSnapshot { auth_token })
        .await
    {
        Ok(plug_core::ipc::IpcResponse::OperatorSnapshot { snapshot }) => {
            snapshot.downstream_clients
        }
        _ => Vec::new(),
    }
}

pub(crate) fn live_session_views(
    live: &[plug_core::ipc::IpcLiveSessionInfo],
    config: Option<&plug_core::config::Config>,
    grants: &[plug_core::downstream_oauth::RegisteredClientSummary],
) -> Vec<LiveSessionView> {
    let mut views = live
        .iter()
        .map(|session| (session, session.client_key()))
        .map(|(session, key)| LiveSessionView {
            access_key: session.access_key(),
            name: key
                .as_ref()
                .and_then(|key| config?.clients.get(key)?.name.clone()),
            place: key
                .as_ref()
                .and_then(|key| config?.clients.get(key)?.place.clone()),
            grant_name: key.as_ref().and_then(|key| {
                grants
                    .iter()
                    .find(|grant| plug_core::ipc::grant_client_key(&grant.client_id) == *key)
                    .map(|grant| grant.client_name.trim().to_string())
                    .filter(|name| !name.is_empty())
            }),
            key,
            transport: match session.transport {
                plug_core::ipc::LiveSessionTransport::DaemonProxy => "daemon_proxy".to_string(),
                plug_core::ipc::LiveSessionTransport::Http => "http".to_string(),
                plug_core::ipc::LiveSessionTransport::Sse => "sse".to_string(),
            },
            client_id: session.client_id.clone(),
            session_id: session.session_id.clone(),
            client_type: session.client_type.to_string(),
            client_info: session.client_info.clone(),
            host: session.host.clone(),
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
    Some(http_export_url(&config))
}

/// The address a client is given to reach Plug over HTTP: the public one when
/// there is one, else this Mac's own.
pub(crate) fn http_export_url(config: &plug_core::config::Config) -> String {
    let base = config
        .http
        .public_base_url
        .clone()
        .unwrap_or_else(|| localhost_export_base(config));
    let trimmed = base.trim_end_matches('/');
    format!("{trimmed}/mcp")
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
    let config_for_policy = plug_core::config::load_config(config_path).unwrap_or_default();

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
            print_lazy_tool_policy(&config_for_policy, target);
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
            print_lazy_tool_policy(&config_for_policy, target);
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
            print_lazy_tool_policy(&config_for_policy, target);
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
        let canonical_command = if matches!(transport, ExportTransport::Stdio) {
            Some(
                crate::install::canonical_client_command()?
                    .to_string_lossy()
                    .to_string(),
            )
        } else {
            None
        };
        let (snippet, is_toml, is_yaml) = match (format, transport) {
            (0, ExportTransport::Stdio) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcpServers":{"plug":{"command":canonical_command,"args":["connect"]}}})).unwrap(),
                false,
                false,
            ),
            (0, ExportTransport::Http) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcpServers":{"plug":{"url":configured_http_url}}})).unwrap(),
                false,
                false,
            ),
            (1, ExportTransport::Stdio) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcp":{"servers":{"plug":{"command":canonical_command,"args":["connect"]}}}})).unwrap(),
                false,
                false,
            ),
            (1, ExportTransport::Http) => (
                serde_json::to_string_pretty(&serde_json::json!({"mcp":{"servers":{"plug":{"url":configured_http_url}}}})).unwrap(),
                false,
                false,
            ),
            (2, ExportTransport::Stdio) => (
                format!(
                    "\n[mcp_servers.plug]\ncommand = {}\nargs = [\"connect\"]\n",
                    toml_string_literal(canonical_command.as_deref().expect("stdio command"))
                ),
                true,
                false,
            ),
            (2, ExportTransport::Http) => (
                format!("\n[mcp_servers.plug]\nurl = \"{configured_http_url}\"\n"),
                true,
                false,
            ),
            (3, ExportTransport::Stdio) => (
                format!(
                    "\nextensions:\n  plug:\n    type: stdio\n    command: {}\n    args: [\"connect\"]\n    enabled: true\n",
                    yaml_string_scalar(canonical_command.as_deref().expect("stdio command"))
                ),
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
            let mut un = unlink_yaml(&existing, "extensions");
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
        Some("yaml") | Some("yml") => unlink_yaml(&existing, yaml_servers_key(target_enum)),
        _ => unmerge_json_config(&existing)?,
    };
    std::fs::write(&path, unlinked)?;
    Ok(())
}

pub(crate) fn is_linked(target: &str, project: bool) -> bool {
    linked_client_transport(target, project).is_some()
}

/// A client's JSON settings as a value. An empty file is an empty object.
///
/// A file that is not plain JSON, such as one with comments, is refused:
/// writing it back would drop everything Plug could not read.
fn read_client_json(existing: &str) -> anyhow::Result<serde_json::Value> {
    if existing.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(existing).map_err(|error| {
        anyhow::anyhow!(
            "this client's settings file is not plain JSON ({error}), so Plug left it alone; \
             run `plug export <client>` and add the entry by hand"
        )
    })
}

fn unmerge_json_config(existing: &str) -> anyhow::Result<String> {
    let mut json = read_client_json(existing)?;
    if let Some(obj) = json.as_object_mut() {
        for key in [
            "mcpServers",
            "context_servers",
            "servers",
            "amp.mcpServers",
            "mcp_servers",
        ] {
            if let Some(inner) = obj.get_mut(key).and_then(|v| v.as_object_mut()) {
                inner.remove("plug");
            }
        }
        if let Some(mcp) = obj.get_mut("mcp").and_then(|v| v.as_object_mut()) {
            // OpenCode keeps the entry here; OpenClaw one level down.
            mcp.remove("plug");
            if let Some(srv) = mcp.get_mut("servers").and_then(|v| v.as_object_mut()) {
                srv.remove("plug");
            }
        }
        if let Some(tools) = obj.get_mut("tools").and_then(|v| v.as_object_mut())
            && let Some(srv) = tools.get_mut("mcpServers").and_then(|v| v.as_object_mut())
        {
            srv.remove("plug");
        }
    }
    Ok(serde_json::to_string_pretty(&json)?)
}

fn merge_json_config(existing: &str, snippet: &str) -> anyhow::Result<String> {
    let mut existing_json = read_client_json(existing)?;
    let snippet_json: serde_json::Value = serde_json::from_str(snippet)?;
    if let (Some(e_obj), Some(s_obj)) = (existing_json.as_object_mut(), snippet_json.as_object()) {
        for (k, v) in s_obj {
            if k == "mcp" || k == "tools" {
                if let (Some(e_inner), Some(s_inner)) = (
                    e_obj.get_mut(k).and_then(|v| v.as_object_mut()),
                    v.as_object(),
                ) {
                    for (ik, iv) in s_inner {
                        // Plug's own entry is replaced whole, so a command
                        // does not stay behind beside a URL.
                        if ik == "plug" {
                            e_inner.insert(ik.clone(), iv.clone());
                        } else if let (Some(e_deep), Some(s_deep)) = (
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
    // A file that cannot be read as YAML is left as it is: writing a fresh
    // one over it would throw away whatever the person had there.
    let mut existing_yml: serde_norway::Value = if existing.trim().is_empty() {
        serde_norway::Value::Mapping(serde_norway::Mapping::new())
    } else {
        serde_norway::from_str(existing)
            .ok()
            .filter(serde_norway::Value::is_mapping)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the client's config file is not valid YAML, so Plug left it alone; fix or move the file and try again"
                )
            })?
    };
    let snippet_yml: serde_norway::Value = serde_norway::from_str(snippet)?;

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

    Ok(serde_norway::to_string(&existing_yml)?)
}

#[derive(Debug, Clone)]
pub(crate) struct ClientContentRepair {
    pub(crate) disposition: PlugLinkDisposition,
    pub(crate) updated: Option<String>,
    pub(crate) message: String,
}

/// Repair only a stdio entry whose command has already been proven to be a
/// known Plug installation. The targeted field updates preserve neighbouring
/// MCP servers and unknown configuration fields.
pub(crate) fn repair_client_content(
    target: ExportTarget,
    path: &Path,
    content: &str,
    canonical: &Path,
    _http_url: &str,
) -> anyhow::Result<ClientContentRepair> {
    let Some(linked) = linked_client_config_from_content(path, target, content) else {
        return Ok(ClientContentRepair {
            disposition: PlugLinkDisposition::Missing,
            updated: None,
            message: "No Plug entry found; left unchanged.".to_string(),
        });
    };

    if matches!(linked.transport, ExportTransport::Http) {
        return Ok(ClientContentRepair {
            disposition: PlugLinkDisposition::Http,
            updated: None,
            message: "HTTP Plug entry does not need a command repair.".to_string(),
        });
    }

    let disposition = linked
        .command
        .as_deref()
        .zip(linked.args.as_deref())
        .map(|(command, args)| classify_plug_client_command(command, args, canonical))
        .unwrap_or(PlugLinkDisposition::UnknownCommand);
    match disposition {
        PlugLinkDisposition::Canonical => Ok(ClientContentRepair {
            disposition,
            updated: None,
            message: "Already uses the canonical Plug command.".to_string(),
        }),
        PlugLinkDisposition::UnknownCommand => Ok(ClientContentRepair {
            disposition,
            updated: None,
            message: "Plug command is not recognized; left unchanged.".to_string(),
        }),
        PlugLinkDisposition::RecognizedLegacy => Ok(ClientContentRepair {
            disposition,
            updated: Some(replace_stdio_command(path, target, content, canonical)?),
            message: "Repaired a recognized legacy Plug command.".to_string(),
        }),
        PlugLinkDisposition::Http | PlugLinkDisposition::Missing => unreachable!(),
    }
}

fn replace_stdio_command(
    path: &Path,
    target: ExportTarget,
    content: &str,
    canonical: &Path,
) -> anyhow::Result<String> {
    let command = canonical.to_string_lossy().to_string();
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("toml") => replace_toml_stdio_command(content, &command),
        Some("yaml") | Some("yml") => {
            replace_yaml_stdio_command(content, yaml_servers_key(target), &command)
        }
        _ => replace_json_stdio_command(target, content, command),
    }
}

fn toml_string_literal(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

fn yaml_string_scalar(value: &str) -> String {
    serde_norway::to_string(&serde_norway::Value::from(value))
        .expect("serializing a YAML string cannot fail")
        .trim()
        .to_string()
}

fn replace_toml_stdio_command(content: &str, command: &str) -> anyhow::Result<String> {
    let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| line.trim() == "[mcp_servers.plug]")
        .ok_or_else(|| anyhow::anyhow!("missing Codex Plug entry"))?
        + 1;
    let end = lines[start..]
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .map(|offset| start + offset)
        .unwrap_or(lines.len());
    replace_assignments(
        &mut lines,
        start,
        end,
        &[
            ("command", toml_string_literal(command)),
            ("args", "[\"connect\"]".to_string()),
        ],
        AssignmentSyntax::Toml,
    )?;
    Ok(join_lines(content, lines))
}

fn replace_yaml_stdio_command(content: &str, key: &str, command: &str) -> anyhow::Result<String> {
    let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    let (plug, end) = yaml_plug_block(&lines, key)
        .ok_or_else(|| anyhow::anyhow!("missing Plug entry under {key}"))?;
    replace_assignments(
        &mut lines,
        plug + 1,
        end,
        &[
            ("command", yaml_string_scalar(command)),
            ("args", "[\"connect\"]".to_string()),
        ],
        AssignmentSyntax::Yaml,
    )?;
    Ok(join_lines(content, lines))
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[derive(Clone, Copy)]
enum AssignmentSyntax {
    Toml,
    Yaml,
}

fn replace_assignments(
    lines: &mut Vec<String>,
    start: usize,
    end: usize,
    replacements: &[(&str, String)],
    syntax: AssignmentSyntax,
) -> anyhow::Result<()> {
    let mut section_end = end;
    for (key, value) in replacements {
        let mut found = false;
        let mut index = start;
        while index < section_end {
            let line = &lines[index];
            let trimmed = line.trim_start();
            let Some(after_key) = trimmed.strip_prefix(key) else {
                index += 1;
                continue;
            };
            if !after_key.trim_start().starts_with('=') && !after_key.trim_start().starts_with(':')
            {
                index += 1;
                continue;
            }
            let separator = if after_key.trim_start().starts_with('=') {
                '='
            } else {
                ':'
            };
            let separator_index = line.find(separator).expect("assignment separator");
            let value_span_end =
                assignment_value_span_end(lines, index, section_end, separator_index, syntax);
            let comment = inline_comment(&line[separator_index + 1..]).to_string();
            lines[index] = format!("{} {}{}", &line[..=separator_index], value, comment);
            if value_span_end > index + 1 {
                let removed = value_span_end - index - 1;
                lines.drain(index + 1..value_span_end);
                section_end -= removed;
            }
            found = true;
            break;
        }
        if !found {
            anyhow::bail!("missing Plug {key} entry");
        }
    }
    Ok(())
}

fn assignment_value_span_end(
    lines: &[String],
    start: usize,
    end: usize,
    separator_index: usize,
    syntax: AssignmentSyntax,
) -> usize {
    match syntax {
        AssignmentSyntax::Toml => toml_value_span_end(lines, start, end, separator_index),
        AssignmentSyntax::Yaml => yaml_value_span_end(lines, start, end, separator_index),
    }
}

fn toml_value_span_end(
    lines: &[String],
    start: usize,
    end: usize,
    separator_index: usize,
) -> usize {
    if !lines[start][separator_index + 1..]
        .trim_start()
        .starts_with('[')
    {
        return start + 1;
    }

    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (line_index, line) in lines.iter().enumerate().take(end).skip(start) {
        let value = if line_index == start {
            &line[separator_index + 1..]
        } else {
            line.as_str()
        };
        for character in value.chars() {
            match character {
                '\\' if quoted => escaped = !escaped,
                '"' if !escaped => quoted = !quoted,
                '#' if !quoted => break,
                '[' if !quoted => depth += 1,
                ']' if !quoted && depth > 0 => depth -= 1,
                _ => escaped = false,
            }
        }
        if depth == 0 {
            return line_index + 1;
        }
    }
    start + 1
}

fn yaml_value_span_end(
    lines: &[String],
    start: usize,
    end: usize,
    separator_index: usize,
) -> usize {
    let value = &lines[start][separator_index + 1..];
    let comment = inline_comment(value);
    let value_without_comment = if comment.is_empty() {
        value
    } else {
        &value[..value.len() - comment.len()]
    }
    .trim();
    if !value_without_comment.is_empty() {
        return start + 1;
    }

    let assignment_indent = indentation(&lines[start]);
    let mut value_end = start + 1;
    while value_end < end {
        let line = &lines[value_end];
        if line.trim().is_empty() {
            value_end += 1;
            continue;
        }
        if indentation(line) <= assignment_indent {
            break;
        }
        value_end += 1;
    }
    value_end
}

fn inline_comment(value: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        match character {
            '\\' if quoted => escaped = !escaped,
            '"' if !escaped => quoted = !quoted,
            '#' if !quoted => {
                let comment_start = value[..index]
                    .char_indices()
                    .rev()
                    .find(|(_, character)| !character.is_whitespace())
                    .map(|(index, character)| index + character.len_utf8())
                    .unwrap_or(0);
                return &value[comment_start..];
            }
            _ => escaped = false,
        }
    }
    ""
}

fn join_lines(original: &str, lines: Vec<String>) -> String {
    let mut result = lines.join("\n");
    if original.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn replace_json_stdio_command(
    target: ExportTarget,
    content: &str,
    command: String,
) -> anyhow::Result<String> {
    let mut value = serde_json::from_str::<serde_json::Value>(content)?;
    let plug = match target {
        ExportTarget::Nanobot => value
            .get_mut("tools")
            .and_then(|tools| tools.get_mut("mcpServers"))
            .and_then(|servers| servers.get_mut("plug")),
        ExportTarget::VSCodeCopilot => value
            .get_mut("servers")
            .and_then(|servers| servers.get_mut("plug")),
        ExportTarget::Amp => value
            .get_mut("amp.mcpServers")
            .and_then(|servers| servers.get_mut("plug")),
        ExportTarget::OpenClaw => value
            .get_mut("mcp")
            .and_then(|mcp| mcp.get_mut("servers"))
            .and_then(|servers| servers.get_mut("plug")),
        ExportTarget::MuseCode => value
            .get_mut("mcp_servers")
            .and_then(|servers| servers.get_mut("plug")),
        ExportTarget::OpenCode | ExportTarget::Kilo => {
            let plug = value
                .get_mut("mcp")
                .and_then(|servers| servers.get_mut("plug"))
                .and_then(serde_json::Value::as_object_mut)
                .ok_or_else(|| anyhow::anyhow!("missing JSON Plug entry"))?;
            plug.insert(
                "command".to_string(),
                serde_json::json!([command, "connect", "--client", target.target_name()]),
            );
            return Ok(serde_json::to_string_pretty(&value)?);
        }
        _ => {
            if value
                .get("mcpServers")
                .and_then(|servers| servers.get("plug"))
                .is_some()
            {
                value
                    .get_mut("mcpServers")
                    .and_then(|servers| servers.get_mut("plug"))
            } else {
                value
                    .get_mut("context_servers")
                    .and_then(|servers| servers.get_mut("plug"))
            }
        }
    }
    .and_then(serde_json::Value::as_object_mut)
    .ok_or_else(|| anyhow::anyhow!("missing JSON Plug entry"))?;
    plug.insert("command".to_string(), serde_json::Value::String(command));
    plug.insert(
        "args".to_string(),
        serde_json::json!(["connect", "--client", target.target_name()]),
    );
    Ok(serde_json::to_string_pretty(&value)?)
}

/// Muse Code reads no settings file without `schema_version`, so a file Plug
/// starts gets the one its documentation names. A version already there is
/// the person's and stays.
fn with_muse_schema(merged: &str) -> anyhow::Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(merged)?;
    if let Some(settings) = value.as_object_mut()
        && !settings.contains_key("schema_version")
    {
        settings.insert("schema_version".to_string(), serde_json::json!(1));
    }
    Ok(serde_json::to_string_pretty(&value)?)
}

/// Remove the Plug entry under `key`, and nothing else.
pub(crate) fn unlink_yaml(existing: &str, key: &str) -> String {
    let mut lines = existing.lines().map(str::to_string).collect::<Vec<_>>();
    let Some((plug, mut end)) = yaml_plug_block(&lines, key) else {
        return existing.to_string();
    };
    // Blank lines after the entry belong to whatever follows it.
    while end > plug + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines.drain(plug..end);
    join_lines(existing, lines)
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

    let command = crate::install::canonical_client_command()?
        .to_string_lossy()
        .to_string();

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
            // Hermes Agent's file is long, commented, and the person's own;
            // parsing and re-emitting it would rewrite every line.
            Some("yaml") | Some("yml") if target_enum == ExportTarget::Hermes => {
                link_yaml_text(&existing, &snippet)?
            }
            Some("yaml") | Some("yml") => merge_yaml_config(&existing, &snippet)?,
            _ if target_enum == ExportTarget::MuseCode => {
                with_muse_schema(&merge_json_config(&existing, &snippet)?)?
            }
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
        let cert_path = path.parent().unwrap().join("cert.pem");
        let key_path = path.parent().unwrap().join("key.pem");
        std::fs::write(&cert_path, "test-cert").unwrap();
        std::fs::write(&key_path, "test-key").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(
            &path,
            format!(
                "[http]\nbind_address = \"0.0.0.0\"\nauth_mode = \"bearer\"\nport = 4444\ntls_cert_path = \"{}\"\ntls_key_path = \"{}\"\n",
                cert_path.display(),
                key_path.display()
            ),
        )
        .unwrap();

        assert_eq!(
            configured_http_export_url(Some(&path)).as_deref(),
            Some("https://localhost:4444/mcp")
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
    fn vscode_and_copilot_cli_links_survive_a_merge_and_come_back_out() {
        use plug_core::export::{ExportOptions, ExportTarget, ExportTransport, export_config};
        for (target, existing, other) in [
            (
                ExportTarget::VSCodeCopilot,
                r#"{"servers":{"other":{"command":"x"}}}"#,
                "/servers/other",
            ),
            (
                ExportTarget::CopilotCli,
                r#"{"mcpServers":{"other":{"type":"local","command":"x","tools":["*"]}}}"#,
                "/mcpServers/other",
            ),
        ] {
            let snippet = export_config(&ExportOptions {
                target,
                transport: ExportTransport::Stdio,
                port: 3282,
                http_url: None,
                command: "plug".to_string(),
            });
            let merged = merge_json_config(existing, &snippet).expect("merge");
            let linked = linked_client_config_from_content(
                std::path::Path::new("mcp.json"),
                target,
                &merged,
            )
            .expect("linked config");
            assert!(matches!(linked.transport, ExportTransport::Stdio));
            assert_eq!(linked.command.as_deref(), Some("plug"));

            let unlinked = unmerge_json_config(&merged).expect("unmerge");
            assert!(
                linked_client_config_from_content(
                    std::path::Path::new("mcp.json"),
                    target,
                    &unlinked
                )
                .is_none()
            );
            let value: serde_json::Value = serde_json::from_str(&unlinked).unwrap();
            assert!(value.pointer(other).is_some(), "{other} must survive");
        }
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
    fn hermes_link_changes_only_the_plug_entry() {
        use plug_core::export::{ExportOptions, ExportTarget, ExportTransport, export_config};
        let path = std::path::Path::new("config.yaml");
        let existing = "# my notes\nmodel:\n  plug: not a server\nmcp_servers:\n    other:\n        command: other # keep\n\nplatforms:\n  webhook:\n    enabled: true\n";
        let snippet = export_config(&ExportOptions {
            target: ExportTarget::Hermes,
            transport: ExportTransport::Stdio,
            port: 3282,
            http_url: None,
            command: "/Applications/Plug.app/Contents/Resources/plug".to_string(),
        });

        let linked = link_yaml_text(existing, &snippet).expect("link");
        let config =
            linked_client_config_from_content(path, ExportTarget::Hermes, &linked).expect("linked");
        assert_eq!(config.transport, ExportTransport::Stdio);
        assert_eq!(
            config.args.as_deref(),
            Some(
                &[
                    "connect".to_string(),
                    "--client".to_string(),
                    "hermes".to_string()
                ][..]
            )
        );
        // Linking twice leaves one entry, and unlinking gives the file back.
        assert_eq!(link_yaml_text(&linked, &snippet).expect("relink"), linked);
        assert_eq!(unlink_yaml(&linked, "mcp_servers"), existing);

        // A file with no servers yet gains the section at its end.
        let fresh = link_yaml_text("model:\n  default: x\n", &snippet).expect("link");
        assert!(fresh.starts_with("model:\n  default: x\nmcp_servers:\n  plug:\n"));
        assert!(link_yaml_text("mcp_servers: {}\n", &snippet).is_err());
    }

    #[test]
    fn hermes_link_written_by_hand_is_read_and_replaced() {
        use plug_core::export::{ExportOptions, ExportTarget, ExportTransport, export_config};
        let path = std::path::Path::new("config.yaml");
        let existing = "mcp_servers:\n  Plug:\n    command: /Users/rob/.local/bin/plug\n    args:\n      - connect\n    env: {}\nknown:\n  cli:\n    - spotify\n";
        let config = linked_client_config_from_content(path, ExportTarget::Hermes, existing)
            .expect("linked");
        assert_eq!(config.args.as_deref(), Some(&["connect".to_string()][..]));

        let snippet = export_config(&ExportOptions {
            target: ExportTarget::Hermes,
            transport: ExportTransport::Http,
            port: 3282,
            http_url: Some("https://plug.example.com/mcp".to_string()),
            command: "plug".to_string(),
        });
        let linked = link_yaml_text(existing, &snippet).expect("link");
        assert_eq!(
            linked,
            "mcp_servers:\n  plug:\n    url: https://plug.example.com/mcp\nknown:\n  cli:\n    - spotify\n"
        );
        let config =
            linked_client_config_from_content(path, ExportTarget::Hermes, &linked).expect("linked");
        assert_eq!(
            config.endpoint.as_deref(),
            Some("https://plug.example.com/mcp")
        );
    }

    #[test]
    fn opencode_links_under_mcp_and_moves_from_a_command_to_a_url() {
        use plug_core::export::{ExportOptions, export_config};
        let options = |transport| ExportOptions {
            target: ExportTarget::OpenCode,
            transport,
            port: 3282,
            http_url: Some("https://plug.example.com/mcp".to_string()),
            command: "/usr/local/bin/plug".to_string(),
        };
        let path = std::path::Path::new("opencode.json");
        let existing = r#"{"permission":"allow","mcp":{"other":{"type":"local","command":["x"]}}}"#;

        let local = export_config(&options(ExportTransport::Stdio));
        let merged = merge_json_config(existing, &local).expect("merge");
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert!(
            value.get("mcpServers").is_none(),
            "OpenCode refuses the key"
        );
        assert_eq!(
            value["mcp"]["plug"]["command"],
            serde_json::json!(["/usr/local/bin/plug", "connect", "--client", "opencode"])
        );
        let linked = linked_client_config_from_content(path, ExportTarget::OpenCode, &merged)
            .expect("linked");
        assert!(matches!(linked.transport, ExportTransport::Stdio));
        assert_eq!(linked.command.as_deref(), Some("/usr/local/bin/plug"));
        assert_eq!(
            linked.args.as_deref(),
            Some(
                &[
                    "connect".to_string(),
                    "--client".to_string(),
                    "opencode".to_string()
                ][..]
            )
        );

        let moved =
            replace_json_stdio_command(ExportTarget::OpenCode, &merged, "/new/plug".to_string())
                .expect("repair");
        let moved: serde_json::Value = serde_json::from_str(&moved).unwrap();
        assert_eq!(moved["mcp"]["plug"]["command"][0], "/new/plug");

        let remote = export_config(&options(ExportTransport::Http));
        let merged = merge_json_config(&merged, &remote).expect("merge");
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["mcp"]["plug"]["type"], "remote");
        assert!(value["mcp"]["plug"].get("command").is_none());
        assert!(value["mcp"].get("other").is_some());
        let linked = linked_client_config_from_content(path, ExportTarget::OpenCode, &merged)
            .expect("linked");
        assert!(matches!(linked.transport, ExportTransport::Http));

        let unlinked = unmerge_json_config(&merged).expect("unmerge");
        let value: serde_json::Value = serde_json::from_str(&unlinked).unwrap();
        assert!(value["mcp"].get("plug").is_none());
        assert!(value["mcp"].get("other").is_some());
    }

    #[test]
    fn claude_code_over_http_names_its_type() {
        use plug_core::export::{ExportOptions, export_config};
        let snippet = export_config(&ExportOptions {
            target: ExportTarget::ClaudeCode,
            transport: ExportTransport::Http,
            port: 3282,
            http_url: None,
            command: "plug".to_string(),
        });
        let value: serde_json::Value = serde_json::from_str(&snippet).unwrap();
        assert_eq!(value["mcpServers"]["plug"]["type"], "http");
    }

    #[test]
    fn the_four_newer_clients_link_where_each_keeps_its_servers() {
        use plug_core::export::{ExportOptions, export_config};
        for (target, existing, pointer) in [
            (
                ExportTarget::Amp,
                r#"{"amp.showCosts":true,"amp.mcpServers":{"other":{"command":"x"}}}"#,
                "/amp.mcpServers",
            ),
            (
                ExportTarget::OpenClaw,
                r#"{"gateway":{"port":1},"mcp":{"servers":{"other":{"command":"x"}}}}"#,
                "/mcp/servers",
            ),
            (
                ExportTarget::LmStudio,
                r#"{"mcpServers":{"other":{"command":"x"}}}"#,
                "/mcpServers",
            ),
            (
                ExportTarget::MuseCode,
                r#"{"schema_version":1,"mcp_servers":{"other":{"transport":"stdio","command":"x"}}}"#,
                "/mcp_servers",
            ),
        ] {
            let snippet = export_config(&ExportOptions {
                target,
                transport: ExportTransport::Stdio,
                port: 3282,
                http_url: None,
                command: "/usr/local/bin/plug".to_string(),
            });
            let merged = merge_json_config(existing, &snippet).expect("merge");
            let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
            let servers = value.pointer(pointer).expect("servers");
            assert!(
                servers.get("other").is_some(),
                "{target:?} keeps other servers"
            );
            assert!(servers.get("plug").is_some(), "{target:?} gains plug");

            let path = std::path::Path::new("settings.json");
            let linked = linked_client_config_from_content(path, target, &merged).expect("linked");
            assert!(matches!(linked.transport, ExportTransport::Stdio));
            assert_eq!(linked.command.as_deref(), Some("/usr/local/bin/plug"));

            let moved = replace_json_stdio_command(target, &merged, "/new/plug".to_string())
                .expect("repair");
            let moved: serde_json::Value = serde_json::from_str(&moved).unwrap();
            assert_eq!(
                moved.pointer(pointer).unwrap()["plug"]["command"],
                serde_json::json!("/new/plug")
            );

            let unlinked = unmerge_json_config(&merged).expect("unmerge");
            let value: serde_json::Value = serde_json::from_str(&unlinked).unwrap();
            let servers = value.pointer(pointer).expect("servers");
            assert!(servers.get("plug").is_none(), "{target:?} loses plug");
            assert!(servers.get("other").is_some(), "{target:?} still has other");
        }
    }

    #[test]
    fn a_settings_file_with_comments_is_left_alone() {
        let commented = "{\n  // my servers\n  \"amp.mcpServers\": {}\n}\n";
        let snippet = r#"{"amp.mcpServers":{"plug":{"command":"plug"}}}"#;
        assert!(merge_json_config(commented, snippet).is_err());
        assert!(unmerge_json_config(commented).is_err());
        assert!(merge_json_config("", snippet).is_ok());
    }

    #[test]
    fn a_muse_code_file_plug_starts_names_its_schema() {
        let started = with_muse_schema(r#"{"mcp_servers":{"plug":{}}}"#).unwrap();
        let value: serde_json::Value = serde_json::from_str(&started).unwrap();
        assert_eq!(value["schema_version"], serde_json::json!(1));

        let kept = with_muse_schema(r#"{"schema_version":2,"mcp_servers":{}}"#).unwrap();
        let value: serde_json::Value = serde_json::from_str(&kept).unwrap();
        assert_eq!(value["schema_version"], serde_json::json!(2));
    }

    #[test]
    fn unlink_yaml_keeps_the_servers_after_plug() {
        let existing = "extensions:\n  plug:\n    type: sse\n    uri: https://plug.example.com/mcp\n    enabled: true\n  other:\n    type: stdio\n    command: other\n";
        assert_eq!(
            unlink_yaml(existing, "extensions"),
            "extensions:\n  other:\n    type: stdio\n    command: other\n"
        );
    }

    #[test]
    fn a_config_file_that_is_not_yaml_is_left_alone() {
        let snippet = "extensions:\n  plug:\n    type: stdio\n";
        for broken in ["extensions: [unclosed\n", "- a\n- list\n", "just text"] {
            let error = merge_yaml_config(broken, snippet).unwrap_err();
            assert!(error.to_string().contains("left it alone"), "{error}");
        }
        // No file yet, or an empty one, is a fresh start.
        assert!(merge_yaml_config("", snippet).unwrap().contains("plug"));
        assert!(merge_yaml_config("\n", snippet).unwrap().contains("plug"));
    }

    #[test]
    fn merge_yaml_config_preserves_existing_goose_extensions() {
        let existing = r#"
extensions:
  github:
    type: stdio
    command: npx
    args:
    - "@modelcontextprotocol/server-github"
    enabled: true
"#;
        let snippet = plug_core::export::export_config(&ExportOptions {
            target: plug_core::export::ExportTarget::Goose,
            transport: plug_core::export::ExportTransport::Http,
            port: 3282,
            http_url: Some("https://plug.example.com/mcp".to_string()),
            command: "plug".to_string(),
        });

        let merged = merge_yaml_config(existing, &snippet).expect("merge YAML");
        let parsed = serde_norway::from_str::<serde_norway::Value>(&merged).expect("valid YAML");
        let extensions = parsed
            .get("extensions")
            .and_then(|value| value.as_mapping())
            .expect("extensions mapping");

        assert!(extensions.get("github").is_some());
        assert_eq!(
            extensions
                .get("plug")
                .and_then(|value| value.get("uri"))
                .and_then(|value| value.as_str()),
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
    fn classifies_only_canonical_or_recognized_legacy_connect_commands() {
        let canonical = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let connect = vec!["connect".to_string()];
        let home = dirs::home_dir().expect("test home directory");

        assert_eq!(
            classify_plug_client_command(canonical.to_str().unwrap(), &connect, canonical),
            PlugLinkDisposition::Canonical
        );
        for legacy in [
            home.join(".cargo/bin/plug").to_string_lossy().into_owned(),
            "/opt/homebrew/bin/plug".to_string(),
            home.join("Applications/Plug.app/Contents/Resources/plug")
                .to_string_lossy()
                .into_owned(),
            "/Users/rob/src/plug/target/release/plug".to_string(),
        ] {
            assert_eq!(
                classify_plug_client_command(&legacy, &connect, canonical),
                PlugLinkDisposition::RecognizedLegacy,
                "{legacy} should be a recognized legacy Plug command"
            );
        }
        assert_eq!(
            classify_plug_client_command("/usr/local/bin/not-plug", &connect, canonical),
            PlugLinkDisposition::UnknownCommand
        );
        assert_eq!(
            classify_plug_client_command(
                "/Users/rob/project/target/release/plug",
                &["serve".to_string()],
                canonical,
            ),
            PlugLinkDisposition::UnknownCommand
        );

        // A link that says which client it serves is the same link.
        let named = ["connect", "--client", "cursor"].map(str::to_string);
        assert_eq!(
            classify_plug_client_command(canonical.to_str().unwrap(), &named, canonical),
            PlugLinkDisposition::Canonical
        );
        assert_eq!(
            classify_plug_client_command("/opt/homebrew/bin/plug", &named, canonical),
            PlugLinkDisposition::RecognizedLegacy
        );
        for other in [
            vec!["connect", "--client"],
            vec!["connect", "--config", "/tmp/other.toml"],
            vec!["connect", "--client", "cursor", "--verbose"],
        ] {
            let other: Vec<String> = other.into_iter().map(str::to_string).collect();
            assert_eq!(
                classify_plug_client_command(canonical.to_str().unwrap(), &other, canonical),
                PlugLinkDisposition::UnknownCommand,
                "{other:?} is not an argument list plug link writes"
            );
        }
    }

    #[test]
    fn rejects_broad_legacy_path_shapes_and_escapes_custom_stdio_paths() {
        let canonical = std::path::Path::new("/Applications/Plug.app/Contents/Resources/plug");
        let connect = vec!["connect".to_string()];
        for command in [
            "/tmp/homebrew/bin/plug",
            "/tmp/opt/homebrew/bin/plug",
            "/tmp/usr/local/bin/plug",
            "/Applications/Other Plug.app/Contents/Resources/plug",
            "/Users/rob/project/target/custom/plug",
            "/Users/rob/target/release/plug/not-a-plug/plug",
        ] {
            assert_eq!(
                classify_plug_client_command(command, &connect, canonical),
                PlugLinkDisposition::UnknownCommand,
                "{command} must not be adopted"
            );
        }

        let special = "/Applications/Plug #1.app/Contents/Resources/plug";
        let toml = toml_string_literal(special);
        assert_eq!(
            toml::from_str::<toml::Value>(&format!("command = {toml}")).unwrap()["command"]
                .as_str(),
            Some(special)
        );
        let yaml = yaml_string_scalar(special);
        assert_eq!(
            serde_norway::from_str::<serde_norway::Value>(&format!("command: {yaml}\n"))
                .unwrap()
                .get("command")
                .and_then(|value| value.as_str()),
            Some(special)
        );
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

    #[test]
    fn client_inventory_names_current_products() {
        let clients = all_client_targets();
        assert!(clients.contains(&("Devin", "devin")));
        assert!(clients.contains(&("Grok Build", "grok-build")));
        assert!(clients.contains(&("GitHub Copilot CLI", "copilot-cli")));
        for client in [
            ("Pi", "pi"),
            ("Warp", "warp"),
            ("Kiro", "kiro"),
            ("Kimi Code", "kimi-code"),
            ("Qwen Code", "qwen-code"),
        ] {
            assert!(clients.contains(&client));
        }
        assert!(!clients.iter().any(|(name, _)| name.contains("Windsurf")));
        assert!(!clients.iter().any(|(_, target)| *target == "roocode"));
        // Every registry target parses, so `plug link` accepts each row.
        for (_, target) in clients {
            assert!(
                target.parse::<plug_core::export::ExportTarget>().is_ok(),
                "{target} is not an export target"
            );
        }
    }
}
