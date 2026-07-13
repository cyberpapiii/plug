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
        let snapshot = store.runtime_auth_snapshot();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::tests::{
        auth_status_test_context, cleanup_temp_config, clear_store, seeded_credentials,
        temp_config_path, write_oauth_config,
    };
    use rmcp::transport::auth::CredentialStore;

    #[tokio::test]
    async fn auth_status_without_credentials_reports_auth_required() {
        let config_path = temp_config_path("auth-status-missing");
        let server_name = format!("oauth-missing-{}", std::process::id());
        write_oauth_config(&config_path, &[server_name.as_str()]);

        clear_store(&server_name).await;

        let ctx = auth_status_test_context(config_path);
        let response = dispatch_auth_status(&ctx).await;
        let IpcResponse::AuthStatus { servers } = response else {
            panic!("expected auth status response");
        };

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, server_name);
        assert!(!servers[0].authenticated);
        assert_eq!(
            servers[0].health,
            plug_core::types::ServerHealth::AuthRequired
        );
        assert!(servers[0].token_expires_in_secs.is_none());
        assert!(servers[0].warnings.is_empty());

        clear_store(&server_name).await;
        cleanup_temp_config(&ctx.config_path);
    }

    #[tokio::test]
    async fn auth_status_with_credentials_and_no_runtime_reports_degraded() {
        let config_path = temp_config_path("auth-status-degraded");
        let server_name = format!("oauth-degraded-{}", std::process::id());
        write_oauth_config(&config_path, &[server_name.as_str()]);

        let store = plug_core::oauth::get_or_create_store(&server_name);
        clear_store(&server_name).await;
        store.save(seeded_credentials()).await.unwrap();

        let ctx = auth_status_test_context(config_path);
        let response = dispatch_auth_status(&ctx).await;
        let IpcResponse::AuthStatus { servers } = response else {
            panic!("expected auth status response");
        };

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, server_name);
        assert!(servers[0].authenticated);
        assert_eq!(servers[0].health, plug_core::types::ServerHealth::Degraded);
        assert!(servers[0].token_expires_in_secs.is_some());
        assert!(servers[0].warnings.is_empty());

        clear_store(&server_name).await;
        cleanup_temp_config(&ctx.config_path);
    }

    #[tokio::test]
    async fn auth_status_does_not_probe_keyring_only_credentials() {
        let config_path = temp_config_path("auth-status-keyring-only");
        let server_name = format!("oauth-keyring-only-{}", std::process::id());
        write_oauth_config(&config_path, &[server_name.as_str()]);

        clear_store(&server_name).await;
        plug_core::oauth::seed_test_keyring_credentials(&server_name, &seeded_credentials());

        let ctx = auth_status_test_context(config_path);
        let response = dispatch_auth_status(&ctx).await;
        let IpcResponse::AuthStatus { servers } = response else {
            panic!("expected auth status response");
        };

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, server_name);
        assert!(!servers[0].authenticated);
        assert_eq!(
            servers[0].health,
            plug_core::types::ServerHealth::AuthRequired
        );
        assert!(servers[0].token_expires_in_secs.is_none());
        assert!(servers[0].warnings.is_empty());

        let recovery = plug_core::oauth::get_or_create_store(&server_name).fallback_auth_snapshot();
        assert_eq!(recovery.source, Some("keyring"));

        clear_store(&server_name).await;
        cleanup_temp_config(&ctx.config_path);
    }

    #[tokio::test]
    async fn auth_status_prefers_runtime_auth_required_over_cached_credentials() {
        let config_path = temp_config_path("auth-status-runtime");
        let server_name = format!("oauth-runtime-{}", std::process::id());
        write_oauth_config(&config_path, &[server_name.as_str()]);

        let store = plug_core::oauth::get_or_create_store(&server_name);
        clear_store(&server_name).await;
        store.save(seeded_credentials()).await.unwrap();

        let ctx = auth_status_test_context(config_path);
        ctx.server_manager.mark_auth_required(&server_name);

        let response = dispatch_auth_status(&ctx).await;
        let IpcResponse::AuthStatus { servers } = response else {
            panic!("expected auth status response");
        };

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, server_name);
        assert!(servers[0].authenticated);
        assert_eq!(
            servers[0].health,
            plug_core::types::ServerHealth::AuthRequired
        );
        assert!(servers[0].warnings.is_empty());

        clear_store(&server_name).await;
        cleanup_temp_config(&ctx.config_path);
    }
}
