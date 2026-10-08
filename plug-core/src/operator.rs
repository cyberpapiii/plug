//! Daemon-owned operator mutations and atomic configuration persistence.

use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

use crate::config::{Config, ServerConfig, ToolBlock, TransportType, validate_config};
use crate::proxy::is_disabled_tool;

#[derive(Debug, Clone)]
pub enum OperatorMutation {
    AddServer {
        name: String,
        server: ServerConfig,
    },
    UpdateServer {
        name: String,
        server: ServerConfig,
    },
    RemoveServer {
        name: String,
    },
    /// Add a configured server again as `<server>-<account>`, for a second
    /// account. See `account_copy`.
    AddAccount {
        server: String,
        account: String,
    },
    SetServerEnabled {
        name: String,
        enabled: bool,
    },
    SetToolEnabled {
        tool: String,
        enabled: bool,
    },
    /// Give a client a name of the owner's choosing. An empty name removes it.
    RenameClient {
        key: String,
        name: String,
    },
    /// Say where a client runs. An empty place removes it.
    SetClientPlace {
        key: String,
        place: String,
    },
    /// Hand one client's settings to another, replacing what that one had.
    /// A client that signs in again arrives under a new key; this is how it
    /// keeps its name, its place and its server choices.
    MoveClientSettings {
        from: String,
        to: String,
    },
    /// Keep a client from a server or a tool, or let it back in.
    SetClientBlock {
        key: String,
        kind: ClientBlockKind,
        target: String,
        blocked: bool,
    },
    /// Nothing of its own: the file is saved with every tool block that can
    /// be stored by server and tool stored that way.
    PinToolBlocks,
    /// Watch a tool for change. See `crate::events`.
    AddWatch {
        watch: crate::events::WatchConfig,
    },
    /// Stop watching, by event name (`<server>.<name>`).
    RemoveWatch {
        event: String,
    },
}

/// What a per-client block names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientBlockKind {
    /// A whole upstream server, by its name in the config.
    Server,
    /// A tool, by the name Plug lists it under. It is stored by its server
    /// and that server's name for it. A name with `*` in it, or one Plug
    /// lists no tool under, is stored as a rule over listed names.
    Tool,
    /// A server on the client's allow list. `blocked` puts it on the list
    /// and its absence takes it off. A client with anything on its allow
    /// list gets only what the list names.
    AllowedServer,
    /// A tool on the client's allow list, by the name Plug lists it under,
    /// stored by its server and that server's name for it.
    AllowedTool,
    /// The allow list itself; the target is not read. `blocked` starts one
    /// holding every server the client gets now, and its absence ends it,
    /// keeping the client from the servers that were not on it. Either way
    /// the client goes on getting what it got: what changes is whether a
    /// server added later reaches it.
    AllowList,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorServerSummary {
    pub name: String,
    pub enabled: bool,
    pub transport: TransportType,
    pub oauth: bool,
}

impl OperatorServerSummary {
    pub fn from_config(name: String, server: &ServerConfig) -> Self {
        Self {
            name,
            enabled: server.enabled,
            transport: server.transport.clone(),
            oauth: server.auth.as_deref() == Some("oauth"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorMutationResult {
    pub server: Option<OperatorServerSummary>,
    /// Disabled-tool patterns after the mutation, for `SetToolEnabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_tools: Option<Vec<String>>,
}

impl OperatorMutationResult {
    fn server(summary: Option<OperatorServerSummary>) -> Self {
        Self {
            server: summary,
            disabled_tools: None,
        }
    }
}

#[allow(clippy::result_large_err)]
pub fn load_editable_config(path: &Path) -> Result<Config, figment::Error> {
    if !path.exists() {
        return Ok(Config::default());
    }
    Figment::new()
        .merge(Serialized::defaults(Config::default()))
        .merge(Toml::file(path))
        .extract()
}

pub fn apply_operator_mutation(
    path: &Path,
    mutation: OperatorMutation,
) -> anyhow::Result<(Config, OperatorMutationResult)> {
    apply_operator_mutation_knowing(path, mutation, &|_| None)
}

/// The server and the server's own name behind a name Plug lists a tool
/// under, when a tool is listed under it.
pub type ToolBehind<'a> = &'a dyn Fn(&str) -> Option<(String, String)>;

/// [`apply_operator_mutation`] by a caller that knows the tools: a tool block
/// is stored by server and tool, and blocks written by listed name before
/// that was possible are stored that way too.
pub fn apply_operator_mutation_knowing(
    path: &Path,
    mutation: OperatorMutation,
    tool_behind: ToolBehind<'_>,
) -> anyhow::Result<(Config, OperatorMutationResult)> {
    let mut config = load_editable_config(path)?;
    pin_tool_blocks(&mut config, tool_behind);
    let result = match mutation {
        OperatorMutation::AddServer { name, mut server } => {
            if config.servers.contains_key(&name) {
                anyhow::bail!("server `{name}` already exists");
            }
            // A new server has nothing stored behind a placeholder, so this only
            // drops one that came back from a redacted read of another server.
            server.restore_redacted_secrets(None);
            let summary = OperatorServerSummary::from_config(name.clone(), &server);
            config.servers.insert(name, server);
            OperatorMutationResult::server(Some(summary))
        }
        OperatorMutation::UpdateServer { name, mut server } => {
            let Some(stored) = config.servers.get(&name) else {
                anyhow::bail!("unknown server `{name}`");
            };
            // Clients read this server redacted, so untouched secrets arrive as
            // placeholders. Put the stored values back before they are written.
            server.restore_redacted_secrets(Some(stored));
            let summary = OperatorServerSummary::from_config(name.clone(), &server);
            config.servers.insert(name, server);
            OperatorMutationResult::server(Some(summary))
        }
        OperatorMutation::RemoveServer { name } => {
            if config.servers.remove(&name).is_none() {
                anyhow::bail!("unknown server `{name}`");
            }
            // Off every allow list too, so a server added later under the
            // same name is a new one and no client gets it unasked.
            for settings in config.clients.values_mut() {
                // A list this empties still holds: nothing, not everything.
                settings.only_allowed = settings.has_allow_list();
                settings.allowed_servers.retain(|server| *server != name);
                settings.allowed_tools.retain(|tool| tool.server != name);
            }
            OperatorMutationResult::server(None)
        }
        OperatorMutation::AddAccount { server, account } => {
            let (name, copy) = account_copy(&config, &server, &account)?;
            let summary = OperatorServerSummary::from_config(name.clone(), &copy);
            config.servers.insert(name, copy);
            OperatorMutationResult::server(Some(summary))
        }
        OperatorMutation::SetServerEnabled { name, enabled } => {
            let server = config
                .servers
                .get_mut(&name)
                .ok_or_else(|| anyhow::anyhow!("unknown server `{name}`"))?;
            server.enabled = enabled;
            OperatorMutationResult::server(Some(OperatorServerSummary::from_config(name, server)))
        }
        OperatorMutation::SetToolEnabled { tool, enabled } => {
            set_tool_enabled(&mut config, &tool, enabled)?;
            OperatorMutationResult {
                server: None,
                disabled_tools: Some(config.disabled_tools.clone()),
            }
        }
        OperatorMutation::RenameClient { key, name } => {
            label_client(&mut config, &key, &name, ClientLabel::Name)?;
            OperatorMutationResult::server(None)
        }
        OperatorMutation::SetClientPlace { key, place } => {
            label_client(&mut config, &key, &place, ClientLabel::Place)?;
            OperatorMutationResult::server(None)
        }
        OperatorMutation::MoveClientSettings { from, to } => {
            move_client_settings(&mut config, &from, &to)?;
            OperatorMutationResult::server(None)
        }
        OperatorMutation::SetClientBlock {
            key,
            kind,
            target,
            blocked,
        } => {
            set_client_block(&mut config, &key, kind, &target, blocked, tool_behind)?;
            OperatorMutationResult::server(None)
        }
        OperatorMutation::PinToolBlocks => OperatorMutationResult::server(None),
        OperatorMutation::AddWatch { watch } => {
            config.events.watch.push(watch);
            // Only the event rules: the rest of the file was already accepted.
            let errors: Vec<String> = crate::config::validate_config(&config)
                .into_iter()
                .filter(|error| error.starts_with("events.watch"))
                .collect();
            if !errors.is_empty() {
                anyhow::bail!(errors.join("; "));
            }
            OperatorMutationResult::server(None)
        }
        OperatorMutation::RemoveWatch { event } => {
            let before = config.events.watch.len();
            config
                .events
                .watch
                .retain(|watch| watch.event_name() != event);
            if config.events.watch.len() == before {
                anyhow::bail!("no watch named `{event}`");
            }
            OperatorMutationResult::server(None)
        }
    };
    persist_config_atomic(path, &config)?;
    Ok((config, result))
}

/// The longest account label. It becomes part of every tool name.
const MAX_ACCOUNT_LABEL: usize = 24;

/// A second account for a server is the same server under another name.
///
/// The copy keeps the command, URL, and settings, secrets included, because
/// only this side can read them. OAuth sign-ins are stored by server name, so
/// the copy starts signed out and signs in on its own. Tool groups keep their
/// shape with the account added, so `Gmail` becomes `GmailPersonal` and the
/// two accounts never share a tool name.
fn account_copy(
    config: &Config,
    server: &str,
    account: &str,
) -> anyhow::Result<(String, ServerConfig)> {
    let source = config
        .servers
        .get(server)
        .ok_or_else(|| anyhow::anyhow!("unknown server `{server}`"))?;
    let label = account.trim();
    let fits = !label.is_empty()
        && label.len() <= MAX_ACCOUNT_LABEL
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && label.starts_with(|c: char| c.is_ascii_lowercase());
    if !fits {
        anyhow::bail!(
            "an account name is lowercase letters and digits, starts with a letter, and is at most {MAX_ACCOUNT_LABEL} long"
        );
    }
    let name = format!("{server}-{label}");
    if config.servers.contains_key(&name) {
        anyhow::bail!("server `{name}` already exists");
    }

    let mut copy = source.clone();
    let mut groups = source.tool_groups.clone();
    if groups.is_empty() && server == "workspace" {
        groups = crate::tool_naming::default_workspace_rules();
    }
    let suffix = crate::tool_naming::format_server_prefix(label);
    for group in &mut groups {
        group.prefix.push_str(&suffix);
    }
    copy.tool_groups = groups;
    Ok((name, copy))
}

/// Turn one tool on or off by editing `disabled_tools`.
///
/// Disabling appends the exact merged tool name. Enabling drops every exact
/// entry for it, then fails if a surviving wildcard still covers the tool:
/// the config format cannot express "this pattern except one tool", so
/// silently widening the pattern would switch on tools nobody asked for.
fn set_tool_enabled(config: &mut Config, tool: &str, enabled: bool) -> anyhow::Result<()> {
    if tool.trim().is_empty() {
        anyhow::bail!("tool name is required");
    }
    if enabled {
        config
            .disabled_tools
            .retain(|pattern| !pattern.eq_ignore_ascii_case(tool));
        if let Some(pattern) = config
            .disabled_tools
            .iter()
            .find(|pattern| is_disabled_tool(std::slice::from_ref(*pattern), tool))
        {
            anyhow::bail!(
                "`{tool}` stays off because the pattern `{pattern}` covers it; remove that pattern to turn it back on"
            );
        }
    } else if !config
        .disabled_tools
        .iter()
        .any(|pattern| pattern.eq_ignore_ascii_case(tool))
    {
        config.disabled_tools.push(tool.to_string());
        config.disabled_tools.sort();
    }
    Ok(())
}

/// The longest name a client can be given. Long enough for any product name,
/// short enough to fit a row.
fn move_client_settings(config: &mut Config, from: &str, to: &str) -> anyhow::Result<()> {
    let (from, to) = (from.trim(), to.trim());
    if from.is_empty() || to.is_empty() {
        anyhow::bail!("client key is required");
    }
    if from == to {
        anyhow::bail!("a client cannot take over its own settings");
    }
    match config.clients.remove(from) {
        Some(settings) => {
            config.clients.insert(to.to_string(), settings);
        }
        // The old client had nothing of its own, so neither does the new one.
        None => {
            config.clients.remove(to);
        }
    }
    Ok(())
}

const CLIENT_NAME_MAX_CHARS: usize = 60;

/// Name a client, or with an empty name go back to the one Plug works out.
///
/// The key is not checked against connected clients: a name is set once and
/// has to hold while its client is closed.
/// The two things the owner types about a client.
#[derive(Clone, Copy)]
enum ClientLabel {
    Name,
    Place,
}

fn label_client(
    config: &mut Config,
    key: &str,
    text: &str,
    label: ClientLabel,
) -> anyhow::Result<()> {
    let key = key.trim();
    let text = text.trim();
    let what = match label {
        ClientLabel::Name => "name",
        ClientLabel::Place => "place",
    };
    if key.is_empty() {
        anyhow::bail!("client key is required");
    }
    if text.chars().count() > CLIENT_NAME_MAX_CHARS {
        anyhow::bail!("a client {what} can be at most {CLIENT_NAME_MAX_CHARS} characters");
    }
    if text.chars().any(char::is_control) {
        anyhow::bail!("a client {what} cannot contain control characters");
    }
    let slot = |settings: &mut crate::config::ClientSettings, value: Option<String>| match label {
        ClientLabel::Name => settings.name = value,
        ClientLabel::Place => settings.place = value,
    };
    if text.is_empty() {
        if let Some(settings) = config.clients.get_mut(key) {
            slot(settings, None);
            if settings.is_empty() {
                config.clients.remove(key);
            }
        }
    } else {
        slot(
            config.clients.entry(key.to_string()).or_default(),
            Some(text.to_string()),
        );
    }
    Ok(())
}

/// Keep a client from a server or a tool, or let it back in.
///
/// Like a name, a block is stored under a key that need not be connected. A
/// server has to be one in the config to be blocked, so a typo cannot sit
/// there looking like a block; anything can be unblocked, so a block on a
/// server since removed can still be cleared.
fn set_client_block(
    config: &mut Config,
    key: &str,
    kind: ClientBlockKind,
    target: &str,
    blocked: bool,
    tool_behind: ToolBehind<'_>,
) -> anyhow::Result<()> {
    let key = key.trim();
    let target = target.trim();
    if key.is_empty() {
        anyhow::bail!("client key is required");
    }
    if target.is_empty() || target.chars().any(char::is_control) {
        anyhow::bail!("name the server or tool to block");
    }
    if blocked
        && matches!(
            kind,
            ClientBlockKind::Server | ClientBlockKind::AllowedServer
        )
        && !config.servers.contains_key(target)
    {
        anyhow::bail!("no server is called `{target}`; run `plug servers` to see their names");
    }
    let settings = config.clients.entry(key.to_string()).or_default();
    // Taking the last entry off an allow list leaves an empty list, not
    // none: the client gets nothing, where no list would give it everything.
    let had_allow_list = settings.has_allow_list();
    match kind {
        ClientBlockKind::Server => {
            let list = &mut settings.blocked_servers;
            list.retain(|existing| existing != target);
            if blocked {
                list.push(target.to_string());
                list.sort();
            }
        }
        ClientBlockKind::Tool => {
            let named = ToolBlock::Named(target.to_string());
            let block = pinned(&named, tool_behind).unwrap_or_else(|| named.clone());
            // Either way of writing it goes, so unblocking a tool clears a
            // block written before it was stored by server.
            let list = &mut settings.blocked_tools;
            list.retain(|existing| *existing != block && *existing != named);
            if blocked {
                list.push(block);
                list.sort();
            }
        }
        ClientBlockKind::AllowedServer => {
            let list = &mut settings.allowed_servers;
            list.retain(|existing| existing != target);
            if blocked {
                list.push(target.to_string());
                list.sort();
                // On the list means the client gets it.
                settings
                    .blocked_servers
                    .retain(|existing| existing != target);
            } else {
                // Off the list is all of it.
                settings
                    .allowed_tools
                    .retain(|allowed| allowed.server != target);
                settings.only_allowed = had_allow_list;
            }
        }
        ClientBlockKind::AllowList => {
            let servers: Vec<String> = config.servers.keys().cloned().collect();
            if blocked {
                if !settings.has_allow_list() {
                    settings.allowed_servers = servers
                        .into_iter()
                        .filter(|server| !settings.blocked_servers.contains(server))
                        .collect();
                    settings.allowed_servers.sort();
                    settings.blocked_servers.clear();
                }
                settings.only_allowed = true;
            } else if settings.has_allow_list() {
                let allowed = std::mem::take(&mut settings.allowed_servers);
                settings.allowed_tools.clear();
                settings.only_allowed = false;
                settings.blocked_servers.extend(
                    servers
                        .into_iter()
                        .filter(|server| !allowed.contains(server)),
                );
                settings.blocked_servers.sort();
                settings.blocked_servers.dedup();
            }
        }
        ClientBlockKind::AllowedTool => {
            // An allow list is exact, so a tool goes on it only once Plug
            // knows which server's tool it is. One already on it can be
            // taken off by the same name or as `server/tool`.
            let found = tool_behind(target).or_else(|| {
                target
                    .split_once('/')
                    .map(|(server, tool)| (server.to_string(), tool.to_string()))
                    .filter(|_| !blocked)
            });
            let Some((server, tool)) = found else {
                anyhow::bail!(
                    "Plug lists no tool called `{target}`; run `plug tools -v` to see their names"
                );
            };
            let allowed = crate::config::AllowedTool { server, tool };
            let list = &mut settings.allowed_tools;
            list.retain(|existing| *existing != allowed);
            if blocked {
                list.push(allowed);
                list.sort();
            } else {
                settings.only_allowed = had_allow_list;
            }
        }
    }
    if settings.is_empty() {
        config.clients.remove(key);
    }
    Ok(())
}

/// `block` by server and tool, when it is a plain listed name and a tool is
/// listed under it. A rule with `*` in it stays a rule.
fn pinned(block: &ToolBlock, tool_behind: ToolBehind<'_>) -> Option<ToolBlock> {
    let ToolBlock::Named(name) = block else {
        return None;
    };
    if name.contains('*') {
        return None;
    }
    let (server, tool) = tool_behind(name)?;
    Some(ToolBlock::Of { server, tool })
}

/// Store by server and tool every block that was written by listed name and
/// names a tool Plug lists. One it lists no tool under is left as written,
/// and still holds by that name.
fn pin_tool_blocks(config: &mut Config, tool_behind: ToolBehind<'_>) {
    for settings in config.clients.values_mut() {
        let mut changed = false;
        for block in &mut settings.blocked_tools {
            if let Some(by_server) = pinned(block, tool_behind) {
                *block = by_server;
                changed = true;
            }
        }
        if changed {
            settings.blocked_tools.sort();
            settings.blocked_tools.dedup();
        }
    }
}

/// Atomically replace `path` with a pretty-printed `Config`.
///
/// This is the only persist path for operator mutations. Load goes through
/// figment into `Config`, then this writes `toml::to_string_pretty` of that
/// struct. Comments, unknown keys, and original key order cannot survive that
/// round-trip. `toml_edit` is not a workspace dependency; do not add a second
/// TOML stack here just to keep comments.
pub fn persist_config_atomic(path: &Path, config: &Config) -> anyhow::Result<()> {
    let errors = validate_config(config);
    if !errors.is_empty() {
        anyhow::bail!(errors.join("; "));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temp = PathBuf::from(format!(
        "{}.{}.tmp",
        path.display(),
        uuid::Uuid::new_v4().simple()
    ));
    let contents = toml::to_string_pretty(config)?;

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(&temp, contents)?;

    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_that_signs_in_again_takes_over_its_old_settings() {
        let mut config = Config::default();
        label_client(&mut config, "oauth:old", "GrokBot", ClientLabel::Name).unwrap();
        label_client(&mut config, "oauth:old", "Cloud", ClientLabel::Place).unwrap();
        label_client(&mut config, "oauth:new", "Cursor 2", ClientLabel::Name).unwrap();

        move_client_settings(&mut config, "oauth:old", "oauth:new").unwrap();

        assert!(!config.clients.contains_key("oauth:old"));
        let moved = &config.clients["oauth:new"];
        assert_eq!(moved.name.as_deref(), Some("GrokBot"));
        assert_eq!(moved.place.as_deref(), Some("Cloud"));
    }

    #[test]
    fn taking_over_a_client_with_no_settings_leaves_none() {
        let mut config = Config::default();
        label_client(&mut config, "oauth:new", "Cursor 2", ClientLabel::Name).unwrap();

        move_client_settings(&mut config, "oauth:old", "oauth:new").unwrap();

        assert!(config.clients.is_empty());
        assert!(move_client_settings(&mut config, "oauth:new", "oauth:new").is_err());
    }

    fn fixture_path() -> PathBuf {
        tempfile::tempdir().unwrap().keep().join("config.toml")
    }

    fn server_with_secrets() -> ServerConfig {
        toml::from_str(
            r#"
transport = "http"
url = "https://example.test/mcp"
auth_token = "bearer-abc"

[env]
API_KEY = "sk-live-123"
"#,
        )
        .unwrap()
    }

    fn rename(path: &Path, key: &str, name: &str) -> anyhow::Result<Config> {
        apply_operator_mutation(
            path,
            OperatorMutation::RenameClient {
                key: key.into(),
                name: name.into(),
            },
        )
        .map(|(config, _)| config)
    }

    #[test]
    fn where_a_client_runs_is_stored_beside_its_name_and_removed_alone() {
        let path = fixture_path();
        let key = "oauth:abc";
        let place = |text: &str| {
            apply_operator_mutation(
                &path,
                OperatorMutation::SetClientPlace {
                    key: key.into(),
                    place: text.into(),
                },
            )
            .map(|(config, _)| config)
        };

        rename(&path, key, "Claude Code").unwrap();
        let config = place("  Work laptop ").unwrap();
        assert_eq!(config.clients[key].place.as_deref(), Some("Work laptop"));
        assert_eq!(config.clients[key].name.as_deref(), Some("Claude Code"));

        let config = place("").unwrap();
        assert_eq!(config.clients[key].place, None);
        assert_eq!(config.clients[key].name.as_deref(), Some("Claude Code"));
        assert!(place("a\u{7}b").is_err());
    }

    #[test]
    fn a_client_name_is_stored_under_its_key_and_an_empty_name_removes_it() {
        let path = fixture_path();
        let key = "host:/Users/someone/.hermes/bin/python3";

        rename(&path, key, "  Hermes  ").unwrap();
        let stored = load_editable_config(&path).unwrap();
        assert_eq!(stored.clients[key].name.as_deref(), Some("Hermes"));

        let config = rename(&path, key, "").unwrap();
        assert!(config.clients.is_empty());
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .any(|line| line.starts_with("[clients")),
            "a config with no named client must not grow an empty table"
        );
    }

    #[test]
    fn a_client_name_that_would_break_a_row_is_refused() {
        let path = fixture_path();
        assert!(rename(&path, "cursor", &"x".repeat(61)).is_err());
        assert!(rename(&path, "cursor", "two\nlines").is_err());
        assert!(rename(&path, "  ", "Name").is_err());
        assert!(!path.exists(), "a refused rename must not write the config");
    }

    #[test]
    fn a_second_account_is_the_same_server_under_another_name() {
        let path = fixture_path();
        apply_operator_mutation(
            &path,
            OperatorMutation::AddServer {
                name: "workspace".into(),
                server: server_with_secrets(),
            },
        )
        .unwrap();

        let (config, result) = apply_operator_mutation(
            &path,
            OperatorMutation::AddAccount {
                server: "workspace".into(),
                account: "personal".into(),
            },
        )
        .unwrap();

        assert_eq!(result.server.unwrap().name, "workspace-personal");
        let copy = &config.servers["workspace-personal"];
        // The copy is made where the secrets can be read, so it keeps them.
        assert_eq!(
            copy.auth_token.as_ref().map(|token| token.as_str()),
            Some("bearer-abc")
        );
        assert_eq!(copy.env["API_KEY"], "sk-live-123");
        // The built-in Google groups carry over, named for the account.
        assert!(config.servers["workspace"].tool_groups.is_empty());
        assert!(copy.tool_groups.iter().any(|g| g.prefix == "GmailPersonal"));
        assert!(
            copy.tool_groups
                .iter()
                .all(|g| g.prefix.ends_with("Personal"))
        );
    }

    #[test]
    fn a_second_account_is_refused_when_it_cannot_be_named() {
        let path = fixture_path();
        apply_operator_mutation(
            &path,
            OperatorMutation::AddServer {
                name: "slack".into(),
                server: server_with_secrets(),
            },
        )
        .unwrap();
        let add = |server: &str, account: &str| {
            apply_operator_mutation(
                &path,
                OperatorMutation::AddAccount {
                    server: server.into(),
                    account: account.into(),
                },
            )
        };

        assert!(add("missing", "work").is_err());
        assert!(add("slack", "").is_err());
        assert!(add("slack", "Work Team").is_err());
        assert!(add("slack", "2nd").is_err());
        assert!(add("slack", "work").is_ok());
        assert!(add("slack", "work").is_err(), "the name is taken");
        let config = load_editable_config(&path).unwrap();
        assert_eq!(config.servers.len(), 2);
    }

    #[test]
    fn saving_a_redacted_read_back_keeps_the_stored_secrets() {
        let path = fixture_path();
        apply_operator_mutation(
            &path,
            OperatorMutation::AddServer {
                name: "figma".into(),
                server: server_with_secrets(),
            },
        )
        .unwrap();

        // What an operator client sees, edited the way an editor edits it.
        let mut edited = load_editable_config(&path).unwrap().servers["figma"].redacted();
        edited.url = Some("https://example.test/v2".to_string());

        let (config, _) = apply_operator_mutation(
            &path,
            OperatorMutation::UpdateServer {
                name: "figma".into(),
                server: edited,
            },
        )
        .unwrap();

        let saved = &config.servers["figma"];
        assert_eq!(
            saved.auth_token.as_ref().map(|token| token.as_str()),
            Some("bearer-abc")
        );
        assert_eq!(saved.env["API_KEY"], "sk-live-123");
        assert_eq!(saved.url.as_deref(), Some("https://example.test/v2"));
    }

    #[test]
    fn tool_toggle_round_trips_through_disabled_tools() {
        let path = fixture_path();
        let (config, result) = apply_operator_mutation(
            &path,
            OperatorMutation::SetToolEnabled {
                tool: "figma__get_file".into(),
                enabled: false,
            },
        )
        .unwrap();
        assert_eq!(config.disabled_tools, vec!["figma__get_file".to_string()]);
        assert_eq!(
            result.disabled_tools,
            Some(vec!["figma__get_file".to_string()])
        );

        let (config, _) = apply_operator_mutation(
            &path,
            OperatorMutation::SetToolEnabled {
                tool: "figma__get_file".into(),
                enabled: true,
            },
        )
        .unwrap();
        assert!(config.disabled_tools.is_empty());
    }

    #[test]
    fn disabling_a_tool_twice_does_not_duplicate_the_pattern() {
        let path = fixture_path();
        for _ in 0..2 {
            apply_operator_mutation(
                &path,
                OperatorMutation::SetToolEnabled {
                    tool: "figma__get_file".into(),
                    enabled: false,
                },
            )
            .unwrap();
        }
        let config = load_editable_config(&path).unwrap();
        assert_eq!(config.disabled_tools, vec!["figma__get_file".to_string()]);
    }

    #[test]
    fn enabling_one_tool_refuses_to_widen_a_covering_wildcard() {
        let path = fixture_path();
        let config = Config {
            disabled_tools: vec!["figma__*".to_string()],
            ..Default::default()
        };
        persist_config_atomic(&path, &config).unwrap();

        let error = apply_operator_mutation(
            &path,
            OperatorMutation::SetToolEnabled {
                tool: "figma__get_file".into(),
                enabled: true,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("figma__*"), "{error}");

        // The refusal must leave the pattern in place rather than half-applying.
        let config = load_editable_config(&path).unwrap();
        assert_eq!(config.disabled_tools, vec!["figma__*".to_string()]);
    }

    #[test]
    fn validate_server_does_not_write_and_mutation_is_atomic_owner_only() {
        let path = fixture_path();
        let server: ServerConfig = serde_json::from_value(serde_json::json!({
            "command": "echo"
        }))
        .unwrap();
        let (config, result) = apply_operator_mutation(
            &path,
            OperatorMutation::AddServer {
                name: "search".into(),
                server,
            },
        )
        .unwrap();
        assert!(config.servers.contains_key("search"));
        assert_eq!(result.server.unwrap().name, "search");
        assert!(!PathBuf::from(format!("{}.tmp", path.display())).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn mutation_persists_changed_server_and_tool() {
        let path = fixture_path();
        std::fs::write(
            &path,
            "[servers.search]\ncommand = \"echo\"\nenabled = true\n",
        )
        .unwrap();

        let (config, result) = apply_operator_mutation(
            &path,
            OperatorMutation::SetServerEnabled {
                name: "search".into(),
                enabled: false,
            },
        )
        .unwrap();
        assert!(!config.servers["search"].enabled);
        assert!(!result.server.as_ref().unwrap().enabled);

        let (config, result) = apply_operator_mutation(
            &path,
            OperatorMutation::SetToolEnabled {
                tool: "search__lookup".into(),
                enabled: false,
            },
        )
        .unwrap();
        assert_eq!(config.disabled_tools, vec!["search__lookup".to_string()]);
        assert_eq!(
            result.disabled_tools,
            Some(vec!["search__lookup".to_string()])
        );

        let reloaded = load_editable_config(&path).unwrap();
        assert!(!reloaded.servers["search"].enabled);
        assert_eq!(reloaded.disabled_tools, vec!["search__lookup".to_string()]);
    }

    #[test]
    fn persist_config_atomic_drops_toml_comments() {
        let path = fixture_path();
        std::fs::write(
            &path,
            concat!(
                "# keep this document comment\n",
                "[servers.search]\n",
                "# keep this server comment\n",
                "command = \"echo\" # command note\n",
                "enabled = true\n",
            ),
        )
        .unwrap();

        apply_operator_mutation(
            &path,
            OperatorMutation::SetServerEnabled {
                name: "search".into(),
                enabled: false,
            },
        )
        .unwrap();

        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(
            !rewritten.contains("# keep this document comment"),
            "{rewritten}"
        );
        assert!(
            !rewritten.contains("# keep this server comment"),
            "{rewritten}"
        );
        assert!(!rewritten.contains("# command note"), "{rewritten}");

        let reloaded = load_editable_config(&path).unwrap();
        assert!(!reloaded.servers["search"].enabled);
    }
    #[test]
    fn a_client_block_round_trips_and_leaves_no_empty_entry() {
        let path = fixture_path();
        std::fs::write(
            &path,
            "[servers.git]\ncommand = \"git-mcp\"\n\n[clients.cursor]\nname = \"Work Cursor\"\n",
        )
        .unwrap();
        let block = |key: &str, kind, target: &str, blocked| {
            apply_operator_mutation(
                &path,
                OperatorMutation::SetClientBlock {
                    key: key.to_string(),
                    kind,
                    target: target.to_string(),
                    blocked,
                },
            )
        };

        // A server has to exist to be blocked; a tool pattern is free text.
        assert!(block("pi", ClientBlockKind::Server, "gti", true).is_err());
        block("pi", ClientBlockKind::Server, "git", true).unwrap();
        block("pi", ClientBlockKind::Server, "git", true).unwrap();
        block("pi", ClientBlockKind::Tool, "slack__*", true).unwrap();
        let (config, _) = block("cursor", ClientBlockKind::Tool, "git__push", true).unwrap();
        assert_eq!(config.clients["pi"].blocked_servers, ["git"]);
        assert_eq!(config.clients["pi"].blocked_tools, ["slack__*".into()]);
        assert_eq!(
            config.clients["cursor"].name.as_deref(),
            Some("Work Cursor")
        );

        let reloaded = crate::config::load_config(Some(&path)).unwrap();
        assert_eq!(reloaded.clients, config.clients);

        block("pi", ClientBlockKind::Server, "git", false).unwrap();
        let (config, _) = block("pi", ClientBlockKind::Tool, "slack__*", false).unwrap();
        assert!(!config.clients.contains_key("pi"), "nothing left to keep");
        // Unblocking keeps the name, and unblocking what was never blocked
        // is not an error.
        let (config, _) = block("cursor", ClientBlockKind::Tool, "git__push", false).unwrap();
        assert_eq!(
            config.clients["cursor"].name.as_deref(),
            Some("Work Cursor")
        );
        block("nobody", ClientBlockKind::Server, "gone", false).unwrap();
    }

    #[test]
    fn an_allow_list_names_servers_and_tools_plug_knows() {
        use crate::config::AllowedTool;

        let path = fixture_path();
        std::fs::write(&path, "[servers.git]\ncommand = \"git-mcp\"\n").unwrap();
        let tool_behind = |listed: &str| {
            (listed == "Code__push").then(|| ("git".to_string(), "push".to_string()))
        };
        let allow = |kind, target: &str, allowed| {
            apply_operator_mutation_knowing(
                &path,
                OperatorMutation::SetClientBlock {
                    key: "radar".to_string(),
                    kind,
                    target: target.to_string(),
                    blocked: allowed,
                },
                &tool_behind,
            )
        };

        assert!(allow(ClientBlockKind::AllowedServer, "gti", true).is_err());
        // A tool Plug does not list cannot go on: the list is exact.
        assert!(allow(ClientBlockKind::AllowedTool, "Code__pull", true).is_err());
        allow(ClientBlockKind::AllowedServer, "git", true).unwrap();
        allow(ClientBlockKind::AllowedTool, "Code__push", true).unwrap();
        let (config, _) = allow(ClientBlockKind::AllowedTool, "Code__push", true).unwrap();
        let push = AllowedTool {
            server: "git".to_string(),
            tool: "push".to_string(),
        };
        assert_eq!(config.clients["radar"].allowed_servers, ["git"]);
        assert_eq!(config.clients["radar"].allowed_tools, [push]);
        assert_eq!(
            crate::config::load_config(Some(&path)).unwrap().clients,
            config.clients
        );

        // Off by the server's own name too, for a tool no longer listed.
        allow(ClientBlockKind::AllowedTool, "git/push", false).unwrap();
        let (config, _) = allow(ClientBlockKind::AllowedServer, "git", false).unwrap();
        // An emptied list still holds: nothing, not everything.
        assert!(config.clients["radar"].has_allow_list());
        assert!(config.clients["radar"].allowed_servers.is_empty());
        assert_eq!(
            crate::config::load_config(Some(&path)).unwrap().clients,
            config.clients
        );

        // Ending the list keeps the client from what was not on it, and
        // starting one again puts on it what the client gets.
        let (config, _) = allow(ClientBlockKind::AllowList, "*", false).unwrap();
        assert!(!config.clients["radar"].has_allow_list());
        assert_eq!(config.clients["radar"].blocked_servers, ["git"]);
        let (config, _) = allow(ClientBlockKind::AllowList, "*", true).unwrap();
        assert!(config.clients["radar"].has_allow_list());
        assert!(config.clients["radar"].blocked_servers.is_empty());
        assert!(config.clients["radar"].allowed_servers.is_empty());
        allow(ClientBlockKind::AllowedServer, "git", true).unwrap();
        let (config, _) = allow(ClientBlockKind::AllowList, "*", false).unwrap();
        assert!(
            !config.clients.contains_key("radar"),
            "nothing left to keep"
        );

        // Taking off what was never on starts no list.
        let (config, _) = allow(ClientBlockKind::AllowedServer, "git", false).unwrap();
        assert!(!config.clients.contains_key("radar"));
    }

    #[test]
    fn removing_a_server_takes_it_off_every_allow_list() {
        let path = fixture_path();
        std::fs::write(
            &path,
            "[servers.git]\ncommand = \"git-mcp\"\n\n[clients.radar]\nallowed_servers = [\"git\"]\n",
        )
        .unwrap();
        let (config, _) = apply_operator_mutation_knowing(
            &path,
            OperatorMutation::RemoveServer {
                name: "git".to_string(),
            },
            &|_| None,
        )
        .unwrap();
        // A server added later under the same name is not one it gets, and
        // the emptied list still holds.
        assert!(config.clients["radar"].allowed_servers.is_empty());
        assert!(config.clients["radar"].has_allow_list());
    }

    #[test]
    fn a_tool_block_is_stored_by_server_and_tool_and_old_ones_are_brought_over() {
        use crate::config::ToolBlock;

        let path = fixture_path();
        // Written by listed name, as every block was: one of a tool Plug
        // lists, one of a tool it does not, one that is a rule, and one
        // already by server.
        std::fs::write(
            &path,
            r#"[servers.git]
command = "git-mcp"

[clients.pi]
blocked_tools = ["Code__push", "gone__tool", "slack__*", { server = "git", tool = "tag" }]

[clients.cursor]
blocked_tools = ["code__push", "Code__push"]
"#,
        )
        .unwrap();
        // `Code__push` and `Code__send` are the same tool, before and after
        // a rename.
        let tool_behind = |listed: &str| {
            ["code__push", "code__send"]
                .contains(&listed.to_ascii_lowercase().as_str())
                .then(|| ("git".to_string(), "push".to_string()))
        };
        let of = |tool: &str| ToolBlock::Of {
            server: "git".to_string(),
            tool: tool.to_string(),
        };
        let block = |key: &str, target: &str, blocked| {
            apply_operator_mutation_knowing(
                &path,
                OperatorMutation::SetClientBlock {
                    key: key.to_string(),
                    kind: ClientBlockKind::Tool,
                    target: target.to_string(),
                    blocked,
                },
                &tool_behind,
            )
            .unwrap()
            .0
        };

        // The file as it was still loads, and means what it says.
        let loaded = crate::config::load_config(Some(&path)).unwrap();
        assert_eq!(loaded.clients["pi"].blocked_tools.len(), 4);
        assert_eq!(loaded.clients["pi"].blocked_tools[0], "Code__push".into());

        // Saving anything brings the old blocks over.
        let (config, _) =
            apply_operator_mutation_knowing(&path, OperatorMutation::PinToolBlocks, &tool_behind)
                .unwrap();
        assert_eq!(
            config.clients["pi"].blocked_tools,
            [
                of("push"),
                of("tag"),
                "gone__tool".into(),
                "slack__*".into()
            ]
        );
        assert_eq!(config.clients["cursor"].blocked_tools, [of("push")]);
        let reloaded = crate::config::load_config(Some(&path)).unwrap();
        assert_eq!(reloaded.clients, config.clients);
        // Without the tools known, nothing is touched.
        let (unknowing, _) =
            apply_operator_mutation(&path, OperatorMutation::PinToolBlocks).unwrap();
        assert_eq!(unknowing.clients, config.clients);

        // A new block lands in the same form, whatever name the tool goes
        // by that day, and unblocking under the other name clears it.
        let config = block("codex", "Code__send", true);
        assert_eq!(config.clients["codex"].blocked_tools, [of("push")]);
        let config = block("codex", "Code__push", true);
        assert_eq!(config.clients["codex"].blocked_tools, [of("push")]);
        let config = block("codex", "code__push", false);
        assert!(!config.clients.contains_key("codex"));
        // A rule and an unknown name are kept as typed and removed as typed.
        let config = block("codex", "code__*", true);
        assert_eq!(config.clients["codex"].blocked_tools, ["code__*".into()]);
        let config = block("codex", "code__*", false);
        assert!(!config.clients.contains_key("codex"));
    }

    #[test]
    fn a_watch_is_added_once_and_removed_by_event_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"[http]
modern_downstream_enabled = true
auth_mode = "oauth"
public_base_url = "https://plug.example.com"
oauth_scopes = ["tools:read", "events:subscribe"]

[servers.mail]
command = "mail-mcp"
"#,
        )
        .unwrap();
        let watch = |name: &str, server: &str| crate::events::WatchConfig {
            name: name.to_string(),
            server: server.to_string(),
            tool: "unread".to_string(),
            arguments: serde_json::Map::new(),
            every_secs: 60,
            allow_writes: false,
        };
        let add = |watch| apply_operator_mutation(&path, OperatorMutation::AddWatch { watch });

        let (config, _) = add(watch("inbox", "mail")).unwrap();
        assert_eq!(config.events.watch.len(), 1);
        assert!(add(watch("inbox", "mail")).is_err(), "same event twice");
        assert!(add(watch("inbox", "gone")).is_err(), "unknown server");
        assert!(add(watch("In Box", "mail")).is_err(), "bad name");

        let reloaded = crate::config::load_config(Some(&path)).unwrap();
        assert_eq!(reloaded.events, config.events);

        let remove = |event: &str| {
            apply_operator_mutation(
                &path,
                OperatorMutation::RemoveWatch {
                    event: event.to_string(),
                },
            )
        };
        assert!(remove("mail.other").is_err());
        let (config, _) = remove("mail.inbox").unwrap();
        assert!(config.events.is_empty());
    }
}
