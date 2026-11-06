use dialoguer::console::style;
use plug_core::clients::{ClientTarget, LinkTransport, Scope, ServerEntry};
use std::path::{Path, PathBuf};

use crate::OutputFormat;
use crate::ui::{cli_prompt_theme, print_banner, print_info_line, print_warning_line};

/// One client in `plug clients --output json`. The app reads these fields.
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
    /// The link target of the client Plug recognised.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) target: Option<String>,
    pub(crate) client_info: Option<String>,
    /// The program that started a local connector, read from the process
    /// table rather than from what the client says about itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) host: Option<plug_core::downstream_identity::ClientHost>,
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
        if self.client_type != plug_core::types::ClientType::Unknown.to_string() {
            return &self.client_type;
        }
        let linked = self
            .key
            .as_deref()
            .and_then(ClientTarget::from_slug)
            .map(ClientTarget::display_name);
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
    /// Give a client only the servers and tools you name, and nothing added
    /// later
    Only {
        /// The client: its key from `plug clients -v`, or the name it shows
        /// under while connected
        client: String,
        /// A server it gets, by its name in `plug servers`
        #[arg(long = "server", value_name = "SERVER")]
        servers: Vec<String>,
        /// A single tool it gets, by the name in `plug tools`
        #[arg(long = "tool", value_name = "TOOL")]
        tools: Vec<String>,
        /// Take the named servers and tools off the list instead
        #[arg(long, conflicts_with = "off")]
        remove: bool,
        /// End the list. The client keeps the servers on it, and gets
        /// servers added later
        #[arg(long)]
        off: bool,
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
        [] if wanted.contains(':') || ClientTarget::from_slug(wanted).is_some() => {
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

/// The connected clients, with the names and places already given, so a
/// client can be picked by one.
async fn connected_clients(config_path: Option<&PathBuf>) -> Vec<LiveSessionView> {
    let (live, _, _) = crate::runtime::fetch_live_sessions(config_path).await;
    let config = plug_core::config::load_config(config_path).ok();
    live_session_views(&live, config.as_ref(), &downstream_grants().await)
}

fn client_settings(config_path: Option<&PathBuf>, key: &str) -> plug_core::config::ClientSettings {
    plug_core::config::load_config(config_path)
        .ok()
        .and_then(|config| config.clients.get(key).cloned())
        .unwrap_or_default()
}

pub(crate) async fn cmd_client_rename(
    config_path: Option<&PathBuf>,
    client: String,
    name: String,
) -> anyhow::Result<()> {
    let key = resolve_client_key(&client, &connected_clients(config_path).await)?;
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
    let key = resolve_client_key(&client, &connected_clients(config_path).await)?;
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
    let key = resolve_client_access_key(&client, &connected_clients(config_path).await)?;
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

    let settings = client_settings(config_path, &key);
    print_info_line(client_blocks_line(&key, &settings));
    if !key.starts_with("oauth:") {
        print_info_line(
            style("This keeps the tool list tidy. It is not a security boundary: only a remote client's grant is verified.").dim(),
        );
    }
    Ok(())
}

/// `plug clients only`: put servers and tools on a client's allow list, take
/// them off, or drop the list.
pub(crate) async fn cmd_client_only(
    config_path: Option<&PathBuf>,
    client: String,
    servers: Vec<String>,
    tools: Vec<String>,
    remove: bool,
    off: bool,
) -> anyhow::Result<()> {
    use plug_core::operator::ClientBlockKind;

    if !off && servers.is_empty() && tools.is_empty() {
        anyhow::bail!("say what with --server <name> or --tool <name>, or end the list with --off");
    }
    let key = resolve_client_access_key(&client, &connected_clients(config_path).await)?;
    let targets: Vec<(ClientBlockKind, String)> = if off {
        vec![(ClientBlockKind::AllowList, "*".to_string())]
    } else {
        servers
            .into_iter()
            .map(|server| (ClientBlockKind::AllowedServer, server))
            .chain(
                tools
                    .into_iter()
                    .map(|tool| (ClientBlockKind::AllowedTool, tool)),
            )
            .collect()
    };
    for (kind, target) in targets {
        crate::commands::servers::apply_server_mutation(
            config_path,
            plug_core::operator::OperatorMutation::SetClientBlock {
                key: key.clone(),
                kind,
                target,
                blocked: !remove && !off,
            },
        )
        .await?;
    }

    let settings = client_settings(config_path, &key);
    print_info_line(client_allow_line(&key, &settings));
    if !settings.blocked_servers.is_empty() || !settings.blocked_tools.is_empty() {
        print_info_line(client_blocks_line(&key, &settings));
    }
    if !key.starts_with("oauth:") {
        print_info_line(
            style("This keeps the tool list tidy. It is not a security boundary: only a remote client's grant is verified.").dim(),
        );
    }
    Ok(())
}

pub(crate) fn client_allow_line(key: &str, settings: &plug_core::config::ClientSettings) -> String {
    let mut parts = Vec::new();
    if !settings.allowed_servers.is_empty() {
        parts.push(format!("servers {}", settings.allowed_servers.join(", ")));
    }
    if !settings.allowed_tools.is_empty() {
        let tools: Vec<String> = settings
            .allowed_tools
            .iter()
            .map(ToString::to_string)
            .collect();
        parts.push(format!("tools {}", tools.join(", ")));
    }
    if parts.is_empty() && settings.has_allow_list() {
        format!("{key} gets nothing: its list is empty.")
    } else if parts.is_empty() {
        format!("{key} gets every server, and each one added later.")
    } else {
        format!("{key} gets only {}.", parts.join("; "))
    }
}

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

/// Plug's entry in `target`'s user settings file, if it has one.
pub(crate) fn linked(target: ClientTarget) -> Option<ServerEntry> {
    let path = target.config_path(Scope::User)?;
    let content = std::fs::read_to_string(path).ok()?;
    target.layout().read_plug(&content)
}

pub(crate) fn link_transport(entry: &ServerEntry) -> LinkTransport {
    if entry.url.is_some() {
        LinkTransport::Http
    } else {
        LinkTransport::Stdio
    }
}

/// The listed clients that have a link to Plug, each with its link.
pub(crate) fn linked_clients() -> Vec<(ClientTarget, ServerEntry)> {
    ClientTarget::listed()
        .filter_map(|target| Some((target, linked(target)?)))
        .collect()
}

fn lazy_tool_policy_summary(config: &plug_core::config::Config, target: ClientTarget) -> String {
    let policy = plug_core::clients::lazy_tool_policy(
        config.meta_tool_mode,
        &config.lazy_tools,
        target.slug(),
    );
    format!(
        "{target} lazy tools: {} ({}) - {}",
        policy.mode.label(),
        policy.origin.label(),
        policy.reason
    )
}

pub(crate) fn live_transport_name(transport: plug_core::ipc::LiveSessionTransport) -> &'static str {
    match transport {
        plug_core::ipc::LiveSessionTransport::DaemonProxy => "daemon_proxy",
        plug_core::ipc::LiveSessionTransport::Http => "http",
    }
}

pub(crate) fn client_views(
    live: &[plug_core::ipc::IpcLiveSessionInfo],
    config: Option<&plug_core::config::Config>,
) -> Vec<ClientView> {
    let default_config = plug_core::config::Config::default();
    let config = config.unwrap_or(&default_config);
    let mut views = ClientTarget::listed()
        .map(|target| {
            let sessions: Vec<_> = live
                .iter()
                .filter(|session| session.target.as_deref() == Some(target.slug()))
                .collect();
            let live_transports: std::collections::BTreeSet<&str> = sessions
                .iter()
                .map(|session| live_transport_name(session.transport))
                .collect();
            let link = linked(target);
            let lazy_policy = plug_core::clients::lazy_tool_policy(
                config.meta_tool_mode,
                &config.lazy_tools,
                target.slug(),
            );
            ClientView {
                name: target.display_name().to_string(),
                target: target.slug().to_string(),
                linked: link.is_some(),
                linked_transport: link
                    .as_ref()
                    .map(|entry| link_transport(entry).as_str().to_string()),
                linked_endpoint: link.and_then(|entry| entry.url),
                detected: target.is_installed(),
                live: !sessions.is_empty(),
                live_sessions: sessions.len(),
                live_transports: live_transports.into_iter().map(str::to_string).collect(),
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
    match crate::ipc_client::operator(plug_core::ipc::OperatorRequest::Snapshot).await {
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
        .map(|session| (session, session.client_key.clone()))
        .map(|(session, key)| LiveSessionView {
            access_key: session.access_key.clone(),
            name: key
                .as_ref()
                .and_then(|key| config?.clients.get(key)?.name.clone()),
            place: key
                .as_ref()
                .and_then(|key| config?.clients.get(key)?.place.clone()),
            grant_name: key.as_ref().and_then(|key| {
                grants
                    .iter()
                    .find(|grant| grant.client_key == *key)
                    .map(|grant| grant.client_name.trim().to_string())
                    .filter(|name| !name.is_empty())
            }),
            key,
            transport: live_transport_name(session.transport).to_string(),
            client_id: session.client_id.clone(),
            session_id: session.session_id.clone(),
            client_type: session.client_type.to_string(),
            target: session.target.clone(),
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

/// The listed clients that are installed or already linked, and whether each
/// is linked.
fn detected_or_linked_clients() -> Vec<(ClientTarget, bool)> {
    ClientTarget::listed()
        .map(|target| (target, linked(target).is_some()))
        .filter(|(target, linked)| *linked || target.is_installed())
        .collect()
}

/// The address an HTTP link points at, from the config when it loads.
fn link_http_url(config: Option<&plug_core::config::Config>) -> String {
    config
        .map(|config| config.http.mcp_url())
        .unwrap_or_else(|| plug_core::config::HttpConfig::default().mcp_url())
}

fn prompt_link_transport(http_url: &str, prompt_label: &str) -> anyhow::Result<LinkTransport> {
    let selection = dialoguer::Select::with_theme(&cli_prompt_theme())
        .with_prompt(prompt_label)
        .items([
            "stdio via `plug connect`",
            &format!("HTTP via `{http_url}`"),
        ])
        .default(0)
        .interact()?;
    Ok(if selection == 1 {
        LinkTransport::Http
    } else {
        LinkTransport::Stdio
    })
}

/// What `plug link` and `plug unlink` did to one client's settings file.
#[derive(Debug, serde::Serialize)]
pub(crate) struct LinkChange {
    pub(crate) target: &'static str,
    pub(crate) name: &'static str,
    pub(crate) path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transport: Option<&'static str>,
}

fn print_link_changes(
    output: &OutputFormat,
    key: &str,
    changes: &[LinkChange],
    config: Option<&plug_core::config::Config>,
) -> anyhow::Result<()> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ key: changes }))?
        ),
        OutputFormat::Text => {
            let default_config = plug_core::config::Config::default();
            for change in changes {
                if let Some(target) = ClientTarget::from_slug(change.target)
                    && change.transport.is_some()
                {
                    print_info_line(
                        style(lazy_tool_policy_summary(
                            config.unwrap_or(&default_config),
                            target,
                        ))
                        .dim(),
                    );
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn cmd_link(
    config_path: Option<&PathBuf>,
    targets: Vec<String>,
    all: bool,
    yes: bool,
    transport: Option<LinkTransport>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use dialoguer::{Confirm, MultiSelect};

    let config = plug_core::config::load_config(config_path).ok();
    let http_url = link_http_url(config.as_ref());
    // `--yes` takes stdio unless a transport was asked for.
    let requested = transport.or(yes.then_some(LinkTransport::Stdio));
    let transport_for = |label: &str| match requested {
        Some(transport) => Ok(transport),
        None => prompt_link_transport(&http_url, label),
    };
    let mut changes = Vec::new();

    if !targets.is_empty() || all {
        let targets: Vec<ClientTarget> = if all {
            let detected = detected_or_linked_clients();
            if detected.is_empty() {
                anyhow::bail!(
                    "no detected clients found; pass explicit targets or run `plug link` interactively"
                );
            }
            detected.into_iter().map(|(target, _)| target).collect()
        } else {
            targets
                .iter()
                .map(|name| name.parse().map_err(|error: String| anyhow::anyhow!(error)))
                .collect::<anyhow::Result<_>>()?
        };
        let transport = transport_for("How should selected clients connect to plug?")?;
        for target in targets {
            changes.push(write_link(target, transport, &http_url)?);
        }
        return print_link_changes(output, "linked", &changes, config.as_ref());
    }

    print_banner(
        "◆",
        "Link clients",
        "Choose which AI clients should point at plug",
    );

    let label = |target: ClientTarget, linked: bool, detected: bool| match (linked, detected) {
        (true, _) => format!("{target}  {}", style("[linked]").green().dim()),
        (false, true) => format!("{target}  {}", style("[detected]").cyan().dim()),
        (false, false) => target.to_string(),
    };
    let mut items = detected_or_linked_clients()
        .into_iter()
        .map(|(target, linked)| (label(target, linked, true), target, linked))
        .collect::<Vec<_>>();
    let show_all = if items.is_empty() {
        print_warning_line("No clients detected.");
        if yes {
            println!(
                "Pass explicit targets like `plug link claude-code cursor` or run `plug link` interactively."
            );
            return Ok(());
        }
        if !Confirm::with_theme(&cli_prompt_theme())
            .with_prompt("Show all supported clients?")
            .default(true)
            .interact()?
        {
            return Ok(());
        }
        true
    } else {
        !yes && Confirm::with_theme(&cli_prompt_theme())
            .with_prompt("Show all supported clients?")
            .default(false)
            .interact()?
    };
    if show_all {
        items = ClientTarget::listed()
            .map(|target| {
                let linked = linked(target).is_some();
                (label(target, linked, false), target, linked)
            })
            .collect();
    }

    let selections = if yes {
        (0..items.len()).collect::<Vec<_>>()
    } else {
        let labels: Vec<_> = items.iter().map(|(label, ..)| label.clone()).collect();
        let defaults: Vec<_> = items.iter().map(|(.., linked)| *linked).collect();
        MultiSelect::with_theme(&cli_prompt_theme())
            .with_prompt("Space to toggle [Linked], Enter to apply")
            .items(&labels)
            .defaults(&defaults)
            .interact()?
    };

    for (index, (_, target, was_linked)) in items.iter().enumerate() {
        let selected = selections.contains(&index);
        if selected && !was_linked {
            let transport = transport_for(&format!("How should {target} connect to plug?"))?;
            changes.push(write_link(*target, transport, &http_url)?);
        } else if !selected && *was_linked {
            remove_link(*target)?;
        }
    }
    print_link_changes(output, "linked", &changes, config.as_ref())?;

    if yes {
        return Ok(());
    }

    println!();
    if Confirm::with_theme(&cli_prompt_theme())
        .with_prompt("Configure custom client?")
        .default(false)
        .interact()?
    {
        link_custom_client(
            transport_for("How should this client connect to plug?")?,
            &http_url,
        )?;
    }
    Ok(())
}

/// The shapes a client Plug has no row for can take.
fn custom_layouts() -> [(&'static str, plug_core::clients::Layout); 4] {
    use plug_core::clients::{EntryShape, FileFormat, Layout};
    [
        ("JSON", *ClientTarget::Cursor.layout()),
        (
            "JSON (VS Code style)",
            Layout {
                format: FileFormat::Json,
                servers_at: &["mcp", "servers"],
                also_reads: &[],
                shape: EntryShape::Standard,
                schema_version: None,
            },
        ),
        ("TOML", *ClientTarget::CodexCli.layout()),
        ("YAML", *ClientTarget::Goose.layout()),
    ]
}

fn link_custom_client(transport: LinkTransport, http_url: &str) -> anyhow::Result<()> {
    use dialoguer::{Input, Select};

    let path: String = Input::with_theme(&cli_prompt_theme())
        .with_prompt("Config path")
        .interact_text()?;
    let path = match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("cannot find your home folder"))?
            .join(rest),
        None => PathBuf::from(path),
    };
    let layouts = custom_layouts();
    let format = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Format")
        .items(layouts.map(|(name, _)| name))
        .default(0)
        .interact()?;
    let layout = layouts[format].1;
    let command = match transport {
        LinkTransport::Stdio => crate::install::canonical_client_command()?
            .to_string_lossy()
            .into_owned(),
        LinkTransport::Http => String::new(),
    };
    let entry = layout.entry(&match transport {
        LinkTransport::Stdio => plug_core::clients::PlugLink::Stdio {
            command: &command,
            args: &["connect"],
        },
        LinkTransport::Http => plug_core::clients::PlugLink::Http { url: http_url },
    });
    edit_file(&path, |existing| layout.link(existing, &entry))
}

/// Write `edit` of the file's contents back. A missing file is empty.
fn edit_file(path: &Path, edit: impl FnOnce(&str) -> anyhow::Result<String>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = match std::fs::read_to_string(path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    std::fs::write(path, edit(&existing)?)?;
    Ok(())
}

fn write_link(
    target: ClientTarget,
    transport: LinkTransport,
    http_url: &str,
) -> anyhow::Result<LinkChange> {
    let command = match transport {
        LinkTransport::Stdio => crate::install::canonical_client_command()?
            .to_string_lossy()
            .into_owned(),
        LinkTransport::Http => String::new(),
    };
    let entry = target.plug_entry(transport, &command, http_url);
    let path = target
        .config_path(Scope::User)
        .ok_or_else(|| anyhow::anyhow!("cannot find your home folder"))?;
    edit_file(&path, |existing| target.layout().link(existing, &entry))?;
    Ok(LinkChange {
        target: target.slug(),
        name: target.display_name(),
        path,
        transport: Some(transport.as_str()),
    })
}

/// Take Plug out of `target`'s settings file. `None` when there is no file.
fn remove_link(target: ClientTarget) -> anyhow::Result<Option<LinkChange>> {
    let Some(path) = target.config_path(Scope::User) else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    edit_file(&path, |existing| target.layout().unlink(existing))?;
    Ok(Some(LinkChange {
        target: target.slug(),
        name: target.display_name(),
        path,
        transport: None,
    }))
}

pub(crate) fn cmd_unlink(
    targets: Vec<String>,
    all: bool,
    yes: bool,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use dialoguer::{Confirm, MultiSelect};

    let mut changes = Vec::new();
    if !targets.is_empty() {
        for name in &targets {
            let target: ClientTarget = name
                .parse()
                .map_err(|error: String| anyhow::anyhow!(error))?;
            changes.extend(remove_link(target)?);
        }
        return print_link_changes(output, "unlinked", &changes, None);
    }

    let items: Vec<ClientTarget> = linked_clients()
        .into_iter()
        .map(|(target, _)| target)
        .collect();
    if items.is_empty() {
        if matches!(output, OutputFormat::Json) {
            return print_link_changes(output, "unlinked", &changes, None);
        }
        print_warning_line("No linked clients found.");
        return Ok(());
    }

    if all || yes {
        for target in items {
            changes.extend(remove_link(target)?);
        }
        return print_link_changes(output, "unlinked", &changes, None);
    }

    print_banner(
        "◆",
        "Unlink clients",
        "Remove plug from selected AI client configs",
    );
    if !Confirm::with_theme(&cli_prompt_theme())
        .with_prompt("Choose which linked clients to remove?")
        .default(true)
        .interact()?
    {
        return Ok(());
    }
    let labels = items.iter().map(ToString::to_string).collect::<Vec<_>>();
    let selections = MultiSelect::with_theme(&cli_prompt_theme())
        .with_prompt("Space to toggle, Enter to unlink")
        .items(&labels)
        .defaults(&vec![true; labels.len()])
        .interact()?;
    for index in selections {
        changes.extend(remove_link(items[index])?);
    }
    print_link_changes(output, "unlinked", &changes, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_http_url_falls_back_to_the_default_listener() {
        assert_eq!(link_http_url(None), "http://localhost:3282/mcp");
        let mut config = plug_core::config::Config::default();
        config.http.public_base_url = Some("https://plug.example.com/base".to_string());
        assert_eq!(
            link_http_url(Some(&config)),
            "https://plug.example.com/base/mcp"
        );
    }

    #[test]
    fn the_client_list_leaves_roocode_out_and_names_current_products() {
        let views = client_views(&[], None);
        assert!(views.iter().all(|view| view.target != "roocode"));
        assert!(
            views
                .iter()
                .any(|view| view.target == "devin" && view.name == "Devin")
        );
        assert!(!views.iter().any(|view| view.name.contains("Windsurf")));
        assert_eq!(views.len(), ClientTarget::listed().count());
    }
}
