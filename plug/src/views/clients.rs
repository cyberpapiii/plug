use dialoguer::console::style;
use plug_core::clients::{ClientTarget, LinkTransport};

use crate::ui::{
    print_banner, print_heading, print_info_line, print_label_value, print_warning_line,
};
use crate::views::live;

/// One client in `plug clients --output json`. The app reads these fields.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ClientView {
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) linked: bool,
    pub(crate) linked_transport: Option<LinkTransport>,
    pub(crate) linked_endpoint: Option<String>,
    pub(crate) detected: bool,
    pub(crate) live: bool,
    pub(crate) live_sessions: usize,
    pub(crate) live_transports: Vec<plug_core::ipc::LiveSessionTransport>,
    pub(crate) lazy_tool_mode: String,
    pub(crate) lazy_tool_mode_origin: String,
    pub(crate) lazy_tool_mode_reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LiveSessionView {
    pub(crate) transport: plug_core::ipc::LiveSessionTransport,
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

fn configured_client_state_text(client: &ClientView) -> String {
    match client.live_sessions {
        _ if !client.live => "not connected now".to_string(),
        1 => "connected now".to_string(),
        count => format!("connected now, {count} connections"),
    }
}

fn configured_client_link_text(client: &ClientView) -> Option<String> {
    let transport = client.linked_transport?;
    let mut detail = format!("linked via {transport}");
    if let Some(endpoint) = client.linked_endpoint.as_deref() {
        detail.push_str(" -> ");
        detail.push_str(endpoint);
    }
    Some(detail)
}

fn configured_client_lazy_tool_text(client: &ClientView) -> String {
    format!(
        "lazy tools: {} ({}) - {}",
        client.lazy_tool_mode, client.lazy_tool_mode_origin, client.lazy_tool_mode_reason
    )
}

pub(crate) fn client_list_json(
    clients: &[ClientView],
    live_sessions: &[LiveSessionView],
    inventory: &live::LiveInventory,
    daemon_error: Option<&str>,
    config_error: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "clients": clients,
        "live_sessions": live_sessions,
        "live_session_count": inventory.session_count,
        "live_session_transports": inventory.session_transports,
        "daemon_error": daemon_error,
        "config_error": config_error,
        "lazy_tool_policy_scope": "configured",
        "lazy_tool_live_policy_note": "clients[].lazy_tool_* fields reflect config/default resolution; live sessions keep their daemon-start policy until restart after lazy_tools edits",
        "restart_required_for_lazy_tool_config_changes": inventory.session_count > 0,
    })
}

/// Live sessions for one client, so `plug clients` shows `Claude Code  12`
/// instead of twelve near-identical rows.
#[derive(Debug, PartialEq, Eq)]
struct LiveSessionGroup<'a> {
    client: &'a str,
    /// Where the owner says the client runs.
    place: Option<&'a str>,
    sessions: usize,
    transports: Vec<&'a str>,
    longest_connected_secs: u64,
}

/// A client's name, with where it runs when the owner has said.
fn client_and_place(client: &str, place: Option<&str>) -> String {
    match place {
        Some(place) => format!("{client} ({place})"),
        None => client.to_string(),
    }
}

fn transport_label(transport: plug_core::ipc::LiveSessionTransport) -> &'static str {
    match transport {
        plug_core::ipc::LiveSessionTransport::DaemonProxy => "local",
        plug_core::ipc::LiveSessionTransport::Http => "http",
    }
}

/// Busiest client first, then by name.
fn live_session_groups(sessions: &[LiveSessionView]) -> Vec<LiveSessionGroup<'_>> {
    let mut groups: Vec<LiveSessionGroup<'_>> = Vec::new();
    for session in sessions {
        let transport = transport_label(session.transport);
        match groups.iter_mut().find(|group| {
            group.client == session.label() && group.place == session.place.as_deref()
        }) {
            Some(group) => {
                group.sessions += 1;
                if !group.transports.contains(&transport) {
                    group.transports.push(transport);
                }
                group.longest_connected_secs =
                    group.longest_connected_secs.max(session.connected_secs);
            }
            None => groups.push(LiveSessionGroup {
                client: session.label(),
                place: session.place.as_deref(),
                sessions: 1,
                transports: vec![transport],
                longest_connected_secs: session.connected_secs,
            }),
        }
    }
    groups.sort_by(|a, b| b.sessions.cmp(&a.sessions).then(a.client.cmp(b.client)));
    groups
}

fn print_live_session_rows(sessions: &[LiveSessionView]) {
    println!(
        "  {:<18} {:<14} {:<12} {:<10} {:<10} {}",
        style("SESSION").dim(),
        style("CLIENT").dim(),
        style("TRANSPORT").dim(),
        style("CONNECTED").dim(),
        style("IDLE").dim(),
        style("KEY").dim()
    );
    println!(
        "  {}",
        style("--------------------------------------------------------------------------").dim()
    );
    for session in sessions {
        let idle = session
            .last_activity_secs
            .map(crate::ui::format_duration)
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  {:<18} {:<14} {:<12} {:<10} {:<10} {}",
            &session.session_id[..session.session_id.len().min(18)],
            client_and_place(session.label(), session.place.as_deref()),
            transport_label(session.transport),
            crate::ui::format_duration(session.connected_secs),
            idle,
            session.key.as_deref().unwrap_or("-"),
        );
    }
}

/// One line for every client Plug knows about but has not linked, noting the
/// ones that are installed or connected anyway.
fn unlinked_clients_text(clients: &[ClientView]) -> Option<String> {
    let names = clients
        .iter()
        .filter(|client| !client.linked)
        .map(|client| {
            if client.live {
                format!("{} (connected now)", client.name)
            } else if client.detected {
                format!("{} (on this Mac)", client.name)
            } else {
                client.name.clone()
            }
        })
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(", "))
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

/// `plug clients` as text.
pub(crate) fn print_client_list(
    clients: &[ClientView],
    live_sessions: &[LiveSessionView],
    inventory: &live::LiveInventory,
    daemon_error: Option<&str>,
    config_error: Option<&str>,
    config: Option<&plug_core::config::Config>,
    verbose: bool,
) {
    print_banner("◆", "Clients", "The clients that use Plug");
    if let Some(error) = daemon_error {
        print_warning_line(format!(
            "Plug is not answering ({error}), so it cannot say who is connected now."
        ));
        println!();
    }
    if let Some(error) = config_error {
        print_warning_line(format!(
            "The settings file has a problem: {error}. Run `plug config check`."
        ));
        println!();
    }
    let linked_count = clients.iter().filter(|client| client.linked).count();
    let detected_count = clients.iter().filter(|client| client.detected).count();
    print_heading("Summary");
    print_label_value("Set up", style(linked_count).green().bold());
    print_label_value("On this Mac", style(detected_count).cyan().bold());
    if daemon_error.is_some() {
        print_label_value("Connected now", style("unknown").yellow().bold());
    } else {
        print_label_value("Connected now", style(inventory.session_count).bold());
    }
    if verbose && daemon_error.is_none() {
        print_label_value("Live Transports", inventory.session_transports.summary());
        if inventory.session_count > 0 {
            print_info_line(
                "Lazy tool modes below are configured values; live sessions keep their daemon-start policy until restart after lazy_tools edits.",
            );
        }
    }
    println!();
    print_heading("Connected now");
    if live_sessions.is_empty() {
        print_info_line("No client is connected right now.");
    } else {
        println!(
            "  {:<24} {:<12} {:<14} {}",
            style("CLIENT").dim(),
            style("CONNECTIONS").dim(),
            style("FROM").dim(),
            style("LONGEST").dim()
        );
        println!(
            "  {}",
            style("----------------------------------------------------------------").dim()
        );
        for group in live_session_groups(live_sessions) {
            println!(
                "  {:<24} {:<12} {:<14} {}",
                client_and_place(group.client, group.place),
                group.sessions,
                group.transports.join("+"),
                crate::ui::format_duration(group.longest_connected_secs),
            );
        }
        if verbose {
            println!();
            print_live_session_rows(live_sessions);
        } else {
            print_info_line(style("`plug clients -v` lists each connection.").dim());
        }
    }

    let blocks = config
        .iter()
        .flat_map(|config| config.clients.iter())
        .filter(|(_, settings)| {
            !settings.blocked_servers.is_empty() || !settings.blocked_tools.is_empty()
        })
        .collect::<Vec<_>>();
    if !blocks.is_empty() {
        println!();
        print_heading("Switched off for a client");
        for (key, settings) in blocks {
            print_info_line(client_blocks_line(key, settings));
        }
    }

    let allow_lists = config
        .iter()
        .flat_map(|config| config.clients.iter())
        .filter(|(_, settings)| settings.has_allow_list())
        .collect::<Vec<_>>();
    if !allow_lists.is_empty() {
        println!();
        print_heading("Given only what is named");
        for (key, settings) in allow_lists {
            print_info_line(client_allow_line(key, settings));
        }
    }

    println!();
    print_heading("Set up to use Plug");
    let linked_clients = clients.iter().filter(|client| client.linked);
    if linked_count == 0 {
        print_info_line("No client is set up yet. Run `plug link` to connect one.");
    } else {
        println!("  {:<24} {}", style("CLIENT").dim(), style("STATE").dim());
        println!(
            "  {}",
            style("----------------------------------------------------------------").dim()
        );
    }
    for client in linked_clients {
        println!(
            "  {:<24} {}",
            client.name,
            style(configured_client_state_text(client)).dim()
        );
        if verbose {
            if let Some(link_text) = configured_client_link_text(client) {
                print_info_line(style(link_text).dim());
            }
            print_info_line(style(configured_client_lazy_tool_text(client)).dim());
        }
    }
    if let Some(unlinked) = unlinked_clients_text(clients) {
        println!();
        print_label_value("Not set up", style(unlinked).dim());
    }
}

#[cfg(test)]
impl LiveSessionView {
    /// A session with nothing known about it but its client, transport and age.
    pub(crate) fn sample(
        client_type: &str,
        transport: plug_core::ipc::LiveSessionTransport,
        connected_secs: u64,
    ) -> Self {
        Self {
            session_id: format!("{client_type}-{connected_secs}"),
            client_type: client_type.to_string(),
            target: None,
            transport,
            client_id: None,
            client_info: None,
            host: None,
            key: None,
            name: None,
            place: None,
            grant_name: None,
            access_key: None,
            connected_secs,
            last_activity_secs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ClientView, LiveSessionView};
    use super::{
        LiveSessionGroup, client_list_json, configured_client_lazy_tool_text,
        configured_client_link_text, configured_client_state_text, live_session_groups,
        unlinked_clients_text,
    };
    use crate::views::live::{LiveInventory, LiveSessionTransportCounts};

    #[test]
    fn client_list_json_includes_inventory_contract_fields() {
        let clients = vec![ClientView {
            name: "Claude".to_string(),
            target: "claude-desktop".to_string(),
            linked: true,
            linked_transport: Some(plug_core::clients::LinkTransport::Http),
            linked_endpoint: Some("https://plug.example.com/mcp".to_string()),
            detected: true,
            live: true,
            live_sessions: 1,
            live_transports: vec![plug_core::ipc::LiveSessionTransport::Http],
            lazy_tool_mode: "bridge".to_string(),
            lazy_tool_mode_origin: "auto_default".to_string(),
            lazy_tool_mode_reason: "client benefits from plug-owned search bridge discovery"
                .to_string(),
        }];
        let live_sessions = vec![LiveSessionView {
            session_id: "session-1".to_string(),
            client_type: "claude_desktop".to_string(),
            target: None,
            transport: plug_core::ipc::LiveSessionTransport::Http,
            client_id: None,
            client_info: Some("Claude Desktop".to_string()),
            host: None,
            key: None,
            name: None,
            place: None,
            grant_name: None,
            access_key: None,
            connected_secs: 12,
            last_activity_secs: Some(3),
        }];
        let inventory = LiveInventory {
            session_count: 1,
            session_transports: LiveSessionTransportCounts {
                daemon_proxy: 0,
                http: 1,
            },
        };

        let json = client_list_json(
            &clients,
            &live_sessions,
            &inventory,
            Some("daemon unavailable"),
            Some("invalid lazy config"),
        );

        assert_eq!(json["live_session_count"], 1);
        assert_eq!(json["live_session_transports"]["http"], 1);
        assert_eq!(json["daemon_error"], "daemon unavailable");
        assert_eq!(json["config_error"], "invalid lazy config");
        assert_eq!(json["lazy_tool_policy_scope"], "configured");
        assert_eq!(json["restart_required_for_lazy_tool_config_changes"], true);
    }

    #[test]
    fn configured_client_state_text_prefers_compact_presence_summary() {
        let client = ClientView {
            name: "Codex CLI".to_string(),
            target: "codex-cli".to_string(),
            linked: true,
            linked_transport: Some(plug_core::clients::LinkTransport::Stdio),
            linked_endpoint: None,
            detected: true,
            live: true,
            live_sessions: 2,
            live_transports: vec![plug_core::ipc::LiveSessionTransport::DaemonProxy],
            lazy_tool_mode: "native".to_string(),
            lazy_tool_mode_origin: "auto_default".to_string(),
            lazy_tool_mode_reason: "client has its own native lazy/deferred tool discovery"
                .to_string(),
        };
        assert_eq!(
            configured_client_state_text(&client),
            "connected now, 2 connections"
        );
    }

    #[test]
    fn configured_client_link_text_includes_endpoint_when_present() {
        let client = ClientView {
            name: "Claude Code".to_string(),
            target: "claude-code".to_string(),
            linked: true,
            linked_transport: Some(plug_core::clients::LinkTransport::Http),
            linked_endpoint: Some("https://plug.example.com/mcp".to_string()),
            detected: false,
            live: false,
            live_sessions: 0,
            live_transports: Vec::new(),
            lazy_tool_mode: "native".to_string(),
            lazy_tool_mode_origin: "auto_default".to_string(),
            lazy_tool_mode_reason: "client has its own native lazy/deferred tool discovery"
                .to_string(),
        };
        assert_eq!(
            configured_client_link_text(&client).as_deref(),
            Some("linked via http -> https://plug.example.com/mcp")
        );
    }

    #[test]
    fn configured_client_lazy_tool_text_explains_mode_origin_and_reason() {
        let client = ClientView {
            name: "OpenCode".to_string(),
            target: "opencode".to_string(),
            linked: true,
            linked_transport: Some(plug_core::clients::LinkTransport::Stdio),
            linked_endpoint: None,
            detected: true,
            live: false,
            live_sessions: 0,
            live_transports: Vec::new(),
            lazy_tool_mode: "bridge".to_string(),
            lazy_tool_mode_origin: "client_override".to_string(),
            lazy_tool_mode_reason: "configured for this client target".to_string(),
        };

        assert_eq!(
            configured_client_lazy_tool_text(&client),
            "lazy tools: bridge (client_override) - configured for this client target"
        );
    }

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
    fn one_client_in_two_places_is_two_groups_each_saying_where() {
        let here = session("Claude Code", LOCAL, 30);
        let mut there = session("Claude Code", HTTP, 10);
        there.place = Some("Work laptop".to_string());
        let sessions = [here, there];

        let groups = live_session_groups(&sessions);
        let shown = groups
            .iter()
            .map(|group| super::client_and_place(group.client, group.place))
            .collect::<Vec<_>>();
        assert_eq!(shown, ["Claude Code", "Claude Code (Work laptop)"]);
    }

    #[test]
    fn an_unrecognised_session_is_grouped_under_the_program_that_started_it() {
        let mut hosted = session("Unknown", LOCAL, 30);
        hosted.host = Some(plug_core::downstream_identity::ClientHost {
            name: "Hermes".to_string(),
            executable: "/Applications/Hermes.app/Contents/MacOS/Hermes".to_string(),
            app: Some("/Applications/Hermes.app".to_string()),
            script: None,
        });
        let mut named = session("Cursor", LOCAL, 10);
        named.host = hosted.host.clone();
        let sessions = [hosted, named, session("Unknown", LOCAL, 5)];

        let groups = live_session_groups(&sessions);
        let clients = groups.iter().map(|group| group.client).collect::<Vec<_>>();
        assert_eq!(clients, ["Cursor", "Hermes", "Unknown"]);
    }

    #[test]
    fn a_session_whose_link_names_its_client_shows_under_that_client() {
        let mut linked = session("Unknown", LOCAL, 30);
        linked.host = Some(plug_core::downstream_identity::ClientHost {
            name: "node".to_string(),
            executable: "/opt/homebrew/bin/node".to_string(),
            app: None,
            script: None,
        });
        assert_eq!(linked.label(), "node");
        // A remote client Plug does not know goes by the name it signed in
        // under.
        linked.grant_name = Some("Poke".to_string());
        assert_eq!(linked.label(), "Poke");
        linked.grant_name = None;
        linked.key = Some("pi".to_string());
        assert_eq!(linked.label(), "Pi");
        linked.name = Some("Work Pi".to_string());
        assert_eq!(linked.label(), "Work Pi");
    }

    fn client(name: &str, linked: bool, detected: bool, live: bool) -> ClientView {
        ClientView {
            name: name.to_string(),
            target: name.to_lowercase(),
            linked,
            linked_transport: None,
            linked_endpoint: None,
            detected,
            live,
            live_sessions: usize::from(live),
            live_transports: Vec::new(),
            lazy_tool_mode: "native".to_string(),
            lazy_tool_mode_origin: "auto_default".to_string(),
            lazy_tool_mode_reason: String::new(),
        }
    }

    #[test]
    fn live_sessions_group_by_client_busiest_first() {
        let sessions = vec![
            session("Claude Code", LOCAL, 60),
            session("Codex CLI", LOCAL, 30),
            session("Codex CLI", LOCAL, 7_200),
            session("Codex CLI", HTTP, 10),
            session("Cursor", HTTP, 5),
        ];
        assert_eq!(
            live_session_groups(&sessions),
            vec![
                LiveSessionGroup {
                    client: "Codex CLI",
                    place: None,
                    sessions: 3,
                    transports: vec!["local", "http"],
                    longest_connected_secs: 7_200,
                },
                LiveSessionGroup {
                    client: "Claude Code",
                    place: None,
                    sessions: 1,
                    transports: vec!["local"],
                    longest_connected_secs: 60,
                },
                LiveSessionGroup {
                    client: "Cursor",
                    place: None,
                    sessions: 1,
                    transports: vec!["http"],
                    longest_connected_secs: 5,
                },
            ]
        );
    }

    #[test]
    fn unlinked_clients_collapse_into_one_line() {
        let clients = vec![
            client("Claude Code", true, true, true),
            client("Claude Desktop", false, true, false),
            client("Goose", false, false, true),
            client("Zed", false, false, false),
        ];
        assert_eq!(
            unlinked_clients_text(&clients).as_deref(),
            Some("Claude Desktop (on this Mac), Goose (connected now), Zed")
        );
        assert_eq!(unlinked_clients_text(&clients[..1]), None);
    }
}
