use dialoguer::console::style;
use plug_core::clients::{ClientTarget, LinkTransport, Scope, ServerEntry};
use plug_core::config::HttpConfig;
use std::path::{Path, PathBuf};

use crate::OutputFormat;
use crate::ui::{
    can_prompt_interactively, cli_prompt_theme, print_banner, print_info_line, print_success_line,
    print_warning_line,
};
use crate::views::clients::{
    ClientView, LiveSessionView, client_allow_line, client_blocks_line, client_list_json,
    print_client_list,
};
use dialoguer::Select;

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
    let live = crate::ipc_client::live_sessions().await;
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
    if plug_core::downstream_identity::grant_of_client_key(&key).is_none() {
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
    if plug_core::downstream_identity::grant_of_client_key(&key).is_none() {
        print_info_line(
            style("This keeps the tool list tidy. It is not a security boundary: only a remote client's grant is verified.").dim(),
        );
    }
    Ok(())
}

/// Plug's own HTTP settings, which say which entries in a client's file
/// are Plug: from the config when it loads, else the defaults.
pub(crate) fn own_http(config: Option<&plug_core::config::Config>) -> HttpConfig {
    config.map(|config| config.http.clone()).unwrap_or_default()
}

/// Plug's entry in `target`'s user settings file, if it has one.
pub(crate) fn linked(target: ClientTarget, plug: &HttpConfig) -> Option<ServerEntry> {
    let path = target.config_path(Scope::User)?;
    let content = std::fs::read_to_string(path).ok()?;
    target.layout().read_plug(&content, plug)
}

pub(crate) fn link_transport(entry: &ServerEntry) -> LinkTransport {
    if entry.url.is_some() {
        LinkTransport::Http
    } else {
        LinkTransport::Stdio
    }
}

/// The listed clients that have a link to Plug, each with its link.
pub(crate) fn linked_clients(plug: &HttpConfig) -> Vec<(ClientTarget, ServerEntry)> {
    ClientTarget::listed()
        .filter_map(|target| Some((target, linked(target, plug)?)))
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
            let live_transports: std::collections::BTreeSet<_> =
                sessions.iter().map(|session| session.transport).collect();
            let link = linked(target, &config.http);
            let lazy_policy = plug_core::clients::lazy_tool_policy(
                config.meta_tool_mode,
                &config.lazy_tools,
                target.slug(),
            );
            ClientView {
                name: target.display_name().to_string(),
                target: target.slug().to_string(),
                linked: link.is_some(),
                linked_transport: link.as_ref().map(link_transport),
                linked_endpoint: link.and_then(|entry| entry.url),
                detected: target.is_installed(),
                live: !sessions.is_empty(),
                live_sessions: sessions.len(),
                live_transports: live_transports.into_iter().collect(),
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
            transport: session.transport,
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
fn detected_or_linked_clients(plug: &HttpConfig) -> Vec<(ClientTarget, bool)> {
    ClientTarget::listed()
        .map(|target| (target, linked(target, plug).is_some()))
        .filter(|(target, linked)| *linked || target.is_installed())
        .collect()
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
    pub(crate) transport: Option<LinkTransport>,
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
    let plug = own_http(config.as_ref());
    // The address an HTTP link points at.
    let http_url = plug.mcp_url();
    // `--yes` takes stdio unless a transport was asked for.
    let requested = transport.or(yes.then_some(LinkTransport::Stdio));
    let transport_for = |label: &str| match requested {
        Some(transport) => Ok(transport),
        None => prompt_link_transport(&http_url, label),
    };
    let mut changes = Vec::new();

    if !targets.is_empty() || all {
        let targets: Vec<ClientTarget> = if all {
            let detected = detected_or_linked_clients(&plug);
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
            changes.push(write_link(target, transport, &plug)?);
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
    let mut items = detected_or_linked_clients(&plug)
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
                let linked = linked(target, &plug).is_some();
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
            changes.push(write_link(*target, transport, &plug)?);
        } else if !selected && *was_linked {
            remove_link(*target, &plug)?;
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
            &plug,
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

fn link_custom_client(transport: LinkTransport, plug: &HttpConfig) -> anyhow::Result<()> {
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
    let http_url = plug.mcp_url();
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
        LinkTransport::Http => plug_core::clients::PlugLink::Http { url: &http_url },
    });
    edit_file(&path, |existing| layout.link(existing, &entry, plug))
}

/// Write `edit` of the file's contents back, whole or not at all. A missing
/// file is empty.
fn edit_file(path: &Path, edit: impl FnOnce(&str) -> anyhow::Result<String>) -> anyhow::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    plug_core::fs_perm::write_atomic_keeping_mode(path, edit(&existing)?.as_bytes())?;
    Ok(())
}

fn write_link(
    target: ClientTarget,
    transport: LinkTransport,
    plug: &HttpConfig,
) -> anyhow::Result<LinkChange> {
    let command = match transport {
        LinkTransport::Stdio => crate::install::canonical_client_command()?
            .to_string_lossy()
            .into_owned(),
        LinkTransport::Http => String::new(),
    };
    let entry = target.plug_entry(transport, &command, &plug.mcp_url());
    let path = target
        .config_path(Scope::User)
        .ok_or_else(|| anyhow::anyhow!("cannot find your home folder"))?;
    edit_file(&path, |existing| {
        target.layout().link(existing, &entry, plug)
    })?;
    Ok(LinkChange {
        target: target.slug(),
        name: target.display_name(),
        path,
        transport: Some(transport),
    })
}

/// Take Plug out of `target`'s settings file. `None` when there is no file.
fn remove_link(target: ClientTarget, plug: &HttpConfig) -> anyhow::Result<Option<LinkChange>> {
    let Some(path) = target.config_path(Scope::User) else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    edit_file(&path, |existing| target.layout().unlink(existing, plug))?;
    Ok(Some(LinkChange {
        target: target.slug(),
        name: target.display_name(),
        path,
        transport: None,
    }))
}

pub(crate) fn cmd_unlink(
    config_path: Option<&PathBuf>,
    targets: Vec<String>,
    all: bool,
    yes: bool,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    use dialoguer::{Confirm, MultiSelect};

    let plug = own_http(plug_core::config::load_config(config_path).ok().as_ref());
    let mut changes = Vec::new();
    if !targets.is_empty() {
        for name in &targets {
            let target: ClientTarget = name
                .parse()
                .map_err(|error: String| anyhow::anyhow!(error))?;
            changes.extend(remove_link(target, &plug)?);
        }
        return print_link_changes(output, "unlinked", &changes, None);
    }

    let items: Vec<ClientTarget> = linked_clients(&plug)
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
            changes.extend(remove_link(target, &plug)?);
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
        changes.extend(remove_link(items[index], &plug)?);
    }
    print_link_changes(output, "unlinked", &changes, None)
}

async fn prompt_lazy_tool_mode(config_path: Option<&PathBuf>) -> anyhow::Result<()> {
    use plug_core::types::LazyToolSetting;

    let (_, config) = crate::commands::config::load_editable_config(config_path)?;
    let targets: Vec<ClientTarget> = ClientTarget::listed().collect();
    let labels = targets
        .iter()
        .map(|target| {
            let policy = plug_core::clients::lazy_tool_policy(
                config.meta_tool_mode,
                &config.lazy_tools,
                target.slug(),
            );
            format!(
                "{:<20} {} ({})",
                target.display_name(),
                policy.mode.label(),
                policy.origin.label()
            )
        })
        .collect::<Vec<_>>();

    let target_selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Choose client")
        .items(&labels)
        .default(0)
        .interact_opt()?;
    let Some(target_index) = target_selection else {
        return Ok(());
    };
    let target = targets[target_index];
    let slug = target.slug();

    let settings = [
        LazyToolSetting::Auto,
        LazyToolSetting::Standard,
        LazyToolSetting::Native,
        LazyToolSetting::Bridge,
    ];
    let current = config
        .lazy_tools
        .clients
        .get(slug)
        .copied()
        .unwrap_or_default();
    let mode_selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Lazy tool mode")
        .items(settings.map(|setting| setting.label()))
        .default(settings.iter().position(|s| *s == current).unwrap_or(0))
        .interact_opt()?;
    let Some(mode_index) = mode_selection else {
        return Ok(());
    };

    crate::commands::servers::apply_server_mutation(
        config_path,
        plug_core::operator::OperatorMutation::SetLazyTools {
            client: slug.to_string(),
            setting: settings[mode_index],
        },
    )
    .await?;
    let (_, config) = crate::commands::config::load_editable_config(config_path)?;
    let policy =
        plug_core::clients::lazy_tool_policy(config.meta_tool_mode, &config.lazy_tools, slug);
    print_success_line(format!(
        "{} lazy tools: {} ({})",
        target,
        policy.mode.label(),
        policy.origin.label()
    ));
    print_warning_line(
        "Restart the plug daemon before relying on this lazy tool mode in live client sessions.",
    );
    Ok(())
}

async fn prompt_client_actions(config_path: Option<&PathBuf>) -> anyhow::Result<bool> {
    let options = [
        "Done",
        "Link clients",
        "Unlink clients",
        "Configure lazy tools",
    ];
    let selection = Select::with_theme(&cli_prompt_theme())
        .with_prompt("Choose action")
        .items(options)
        .default(0)
        .interact_opt()?;

    match selection {
        Some(1) => {
            cmd_link(
                config_path,
                Vec::new(),
                false,
                false,
                None,
                &OutputFormat::Text,
            )?;
            Ok(true)
        }
        Some(2) => {
            cmd_unlink(config_path, Vec::new(), false, false, &OutputFormat::Text)?;
            Ok(true)
        }
        Some(3) => {
            prompt_lazy_tool_mode(config_path).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) async fn cmd_client_list(
    config_path: Option<&PathBuf>,
    output: &OutputFormat,
    verbose: bool,
) -> anyhow::Result<()> {
    let interactive = matches!(output, OutputFormat::Text) && can_prompt_interactively();

    loop {
        let daemon_error = if crate::ipc_client::daemon_running().await {
            None
        } else if crate::ipc_client::daemon_starting() {
            Some("daemon is starting".to_string())
        } else {
            Some("daemon not running".to_string())
        };
        let live = crate::ipc_client::live_sessions().await;
        let inventory = crate::views::live::inventory(&live);
        let config_result = plug_core::config::load_config(config_path);
        let config_error = config_result.as_ref().err().map(|error| error.to_string());
        let config = config_result.ok();
        let clients = client_views(&live, config.as_ref());
        let grants = downstream_grants().await;
        let live_sessions = live_session_views(&live, config.as_ref(), &grants);

        if matches!(output, OutputFormat::Json) {
            println!(
                "{}",
                serde_json::to_string_pretty(&client_list_json(
                    &clients,
                    &live_sessions,
                    &inventory,
                    daemon_error.as_deref(),
                    config_error.as_deref(),
                ))?
            );
            return Ok(());
        }

        print_client_list(
            &clients,
            &live_sessions,
            &inventory,
            daemon_error.as_deref(),
            config_error.as_deref(),
            config.as_ref(),
            verbose,
        );

        if !interactive {
            break;
        }
        println!();
        if !prompt_client_actions(config_path).await? {
            break;
        }
        println!();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: plug_core::ipc::LiveSessionTransport =
        plug_core::ipc::LiveSessionTransport::DaemonProxy;
    const HTTP: plug_core::ipc::LiveSessionTransport = plug_core::ipc::LiveSessionTransport::Http;
    fn session(
        client_type: &str,
        transport: plug_core::ipc::LiveSessionTransport,
        connected_secs: u64,
    ) -> LiveSessionView {
        LiveSessionView::sample(client_type, transport, connected_secs)
    }

    #[test]
    fn a_block_goes_under_the_key_the_requests_carry() {
        // A remote session on the shared token shows under what it reports,
        // but its requests are the shared key's.
        let mut shared = session("Cursor", HTTP, 30);
        shared.key = Some("cursor".to_string());
        shared.access_key = Some("remote:shared".to_string());
        let mut local = session("Unknown", LOCAL, 10);
        local.key = Some("pi".to_string());
        local.access_key = Some("pi".to_string());
        let sessions = [shared, local];

        assert_eq!(resolve_client_key("Cursor", &sessions).unwrap(), "cursor");
        assert_eq!(
            resolve_client_access_key("Cursor", &sessions).unwrap(),
            "remote:shared"
        );
        assert_eq!(
            resolve_client_access_key("remote:shared", &sessions).unwrap(),
            "remote:shared"
        );
        assert_eq!(resolve_client_access_key("Pi", &sessions).unwrap(), "pi");
        // Not connected: a key or a target is taken as written.
        assert_eq!(
            resolve_client_access_key("oauth:abc", &sessions).unwrap(),
            "oauth:abc"
        );
        assert_eq!(
            resolve_client_access_key("warp", &sessions).unwrap(),
            "warp"
        );
        assert!(resolve_client_access_key("nobody", &sessions).is_err());
    }

    #[test]
    fn a_client_to_rename_is_found_by_the_name_it_shows_under() {
        let mut hermes = session("Unknown", LOCAL, 30);
        hermes.host = Some(plug_core::downstream_identity::ClientHost {
            name: "python3".to_string(),
            executable: "/opt/hermes/bin/python3".to_string(),
            app: None,
            script: None,
        });
        hermes.key = Some("host:/opt/hermes/bin/python3".to_string());
        let mut renamed = hermes.clone();
        renamed.name = Some("Hermes".to_string());
        let nameless = session("Unknown", LOCAL, 5);

        let key = |wanted, sessions: &[LiveSessionView]| {
            resolve_client_key(wanted, sessions).map_err(|error| error.to_string())
        };
        assert_eq!(
            key("PYTHON3", &[hermes.clone()]).unwrap(),
            "host:/opt/hermes/bin/python3"
        );
        assert_eq!(renamed.label(), "Hermes");
        assert_eq!(
            key("hermes", &[renamed]).unwrap(),
            "host:/opt/hermes/bin/python3"
        );
        // Not connected: a key or a known client is taken as given.
        assert_eq!(key("cursor", &[]).unwrap(), "cursor");
        assert_eq!(key("oauth:abc", &[]).unwrap(), "oauth:abc");
        assert!(key("nobody", &[]).unwrap_err().contains("plug clients -v"));
        assert!(
            key("Unknown", &[nameless])
                .unwrap_err()
                .contains("cannot tell")
        );
    }

    #[test]
    fn link_http_url_falls_back_to_the_default_listener() {
        assert_eq!(own_http(None).mcp_url(), "http://localhost:3282/mcp");
        let mut config = plug_core::config::Config::default();
        config.http.public_base_url = Some("https://plug.example.com/base".to_string());
        assert_eq!(
            own_http(Some(&config)).mcp_url(),
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
