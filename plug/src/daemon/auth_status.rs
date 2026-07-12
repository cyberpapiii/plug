//! Auth-status IPC surface — per-server OAuth state for CLI/agent inspection.

use plug_core::ipc::IpcResponse;

use super::ConnectionContext;

/// Handle `AuthStatus` — return per-server OAuth state from config + credential stores.
pub(super) async fn dispatch_auth_status(ctx: &ConnectionContext) -> IpcResponse {
    use plug_core::oauth;

    let config = plug_core::config::load_config(Some(&ctx.config_path));
    let config = match config {
        Ok(cfg) => cfg,
        Err(e) => {
            return IpcResponse::Error {
                code: "CONFIG_LOAD_FAILED".to_string(),
                message: e.to_string(),
            };
        }
    };

    // Get runtime health from server manager
    let statuses = ctx.server_manager.server_statuses();
    let status_map: std::collections::HashMap<&str, &plug_core::types::ServerStatus> =
        statuses.iter().map(|s| (s.server_id.as_str(), s)).collect();

    let mut oauth_servers: Vec<_> = config
        .servers
        .iter()
        .filter(|(_, sc)| sc.auth.as_deref() == Some("oauth"))
        .collect();
    oauth_servers.sort_by_key(|(name, _)| (*name).clone());

    let mut servers = Vec::new();
    for (name, sc) in &oauth_servers {
        let store = oauth::get_or_create_store(name);
        let snapshot = store.fallback_auth_snapshot();
        let has_creds = snapshot.credentials.is_some();

        let health = status_map
            .get(name.as_str())
            .map(|s| s.health)
            .unwrap_or_else(|| {
                if has_creds {
                    plug_core::types::ServerHealth::Degraded
                } else {
                    plug_core::types::ServerHealth::AuthRequired
                }
            });

        servers.push(plug_core::ipc::IpcAuthServerInfo {
            name: (*name).clone(),
            url: sc.url.clone(),
            authenticated: has_creds,
            health,
            scopes: sc.oauth_scopes.clone(),
            token_expires_in_secs: snapshot.token_expires_in_secs,
            warnings: snapshot.warnings,
        });
    }

    IpcResponse::AuthStatus { servers }
}
