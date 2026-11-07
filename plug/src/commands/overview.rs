//! `plug` with no command, and `plug status`.

use plug_core::clients::LinkTransport;

use crate::OutputFormat;
use crate::commands::clients::{link_transport, linked_clients};
use crate::ipc_client;
use crate::views::live;
use crate::views::overview::{
    LiveStatus, live_status_json, offline_status_json, overview_json, overview_next_actions,
    print_live_status, print_offline_status, print_overview, print_overview_banner,
    print_overview_without_config,
};

/// Each linked client's slug and the transport its link uses.
fn linked_topology(config: Option<&plug_core::config::Config>) -> Vec<(String, LinkTransport)> {
    let plug = config.map(|config| config.http.clone()).unwrap_or_default();
    linked_clients(&plug)
        .into_iter()
        .map(|(target, entry)| (target.slug().to_string(), link_transport(&entry)))
        .collect()
}

fn owner_approval_state(
    config: &plug_core::config::Config,
) -> Option<plug_core::downstream_oauth::OwnerEnrollmentStatus> {
    plug_core::downstream_oauth::DownstreamOauthConfig::from_http_config(&config.http)
        .map(|oauth| plug_core::downstream_oauth::inspect_owner_enrollment(&oauth))
}

pub(crate) async fn cmd_overview(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let config_path = config_path
        .cloned()
        .unwrap_or_else(plug_core::paths::default_config_path);
    let config_exists = config_path.exists();
    let loaded = if config_exists {
        plug_core::config::load_config(Some(&config_path)).ok()
    } else {
        None
    };
    let linked = linked_topology(loaded.as_ref());
    let linked_clients: Vec<String> = linked.iter().map(|(target, _)| target.clone()).collect();
    let live_clients = ipc_client::live_sessions().await;
    let inventory = live::inventory(&live_clients);

    if matches!(output, OutputFormat::Json) {
        let daemon_running = ipc_client::daemon_running().await;
        let config = loaded;
        let server_count = config.as_ref().map(|c| c.servers.len()).unwrap_or(0);
        let owner_approval = config.as_ref().and_then(owner_approval_state);
        let next_actions = overview_next_actions(
            config_exists,
            linked_clients.is_empty(),
            daemon_running,
            owner_approval,
        );
        println!(
            "{}",
            serde_json::to_string_pretty(&overview_json(
                config_exists,
                &config_path,
                daemon_running,
                server_count,
                &linked_clients,
                &inventory,
                owner_approval,
                &next_actions,
            ))?
        );
        return Ok(());
    }

    print_overview_banner();

    if !config_exists {
        print_overview_without_config(&config_path);
        return Ok(());
    }

    let config = plug_core::config::load_config(Some(&config_path))?;
    let daemon_running = ipc_client::daemon_running().await;
    let owner_approval = owner_approval_state(&config);
    print_overview(
        &config_path,
        &config,
        &linked,
        &inventory,
        daemon_running,
        !daemon_running && ipc_client::daemon_starting(),
        owner_approval,
    );
    Ok(())
}

pub(crate) async fn cmd_status(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
    show_token: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    let daemon_running = ipc_client::daemon_running().await;

    let config = plug_core::config::load_config(config_path).ok();
    let linked_clients = linked_topology(config.as_ref());
    let http_auth = config.as_ref().and_then(|c| {
        if !c.http.requires_bearer() {
            return None;
        }

        let token_path = plug_core::auth::http_auth_token_path(c.http.port);
        let token = if show_token {
            std::fs::read_to_string(&token_path)
                .ok()
                .map(|t| t.trim().to_string())
        } else {
            None
        };
        Some((token_path.exists(), token))
    });

    let (status_availability, live_status) = ipc_client::query(
        plug_core::ipc::IpcRequest::Status,
        |response| match response {
            plug_core::ipc::IpcResponse::Status {
                servers,
                clients,
                uptime_secs,
                runtime_version,
                resource_subscriptions,
            } => Some((
                servers,
                clients,
                uptime_secs,
                runtime_version,
                resource_subscriptions,
            )),
            _ => None,
        },
    )
    .await;

    if let Some((servers, clients, uptime_secs, runtime_version, resource_subscriptions)) =
        live_status
    {
        let live_sessions = ipc_client::live_sessions().await;
        let status = LiveStatus {
            servers,
            daemon_proxy_clients: clients,
            uptime_secs,
            runtime_version,
            resource_subscriptions,
            inventory: live::inventory(&live_sessions),
            daemon_running,
            config,
            linked_clients,
            http_auth,
        };
        if matches!(output, OutputFormat::Text) {
            print_live_status(&status, verbose);
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&live_status_json(&status))?
            );
        }
        return Ok(());
    }

    let config = plug_core::config::load_config(config_path)?;
    if matches!(output, OutputFormat::Text) {
        print_offline_status(&config, status_availability);
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&offline_status_json(&config, status_availability))?
        );
    }
    Ok(())
}
