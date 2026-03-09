//! OAuth authentication commands for upstream MCP servers.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use dialoguer::console::style;
use rmcp::transport::auth::{CredentialStore, StoredCredentials};

use plug_core::config;
use plug_core::oauth;

use crate::ui;
use crate::OutputFormat;

/// Top-level auth command dispatcher.
pub(crate) async fn cmd_auth(
    config_path: Option<&PathBuf>,
    command: crate::AuthCommands,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    match command {
        crate::AuthCommands::Login { server, no_browser } => {
            cmd_auth_login(config_path, &server, no_browser).await
        }
        crate::AuthCommands::Inject {
            server,
            access_token,
            refresh_token,
            expires_in,
        } => cmd_auth_inject(&server, &access_token, refresh_token.as_deref(), expires_in).await,
        crate::AuthCommands::Status => cmd_auth_status(config_path, output).await,
        crate::AuthCommands::Logout { server } => cmd_auth_logout(&server).await,
    }
}

// ---------------------------------------------------------------------------
// login
// ---------------------------------------------------------------------------

async fn cmd_auth_login(
    config_path: Option<&PathBuf>,
    server_name: &str,
    no_browser: bool,
) -> anyhow::Result<()> {
    // 1. Load and validate config ----------------------------------------
    let cfg = config::load_config(config_path)?;
    let server_config = cfg
        .servers
        .get(server_name)
        .ok_or_else(|| anyhow::anyhow!("server '{server_name}' not found in config"))?;

    if server_config.auth.as_deref() != Some("oauth") {
        anyhow::bail!(
            "server '{server_name}' is not configured for OAuth (set auth = \"oauth\" in config)"
        );
    }

    let url = server_config
        .url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("server '{server_name}' has no URL configured"))?;

    ui::print_info_line(format!(
        "Starting OAuth login for server '{server_name}'..."
    ));

    // 2. Build an AuthorizationManager with our persistent stores --------
    use rmcp::transport::auth::AuthorizationManager;

    let mut auth_manager = AuthorizationManager::new(url)
        .await
        .map_err(|e| anyhow::anyhow!("failed to initialize OAuth for '{server_name}': {e}"))?;

    // Create stores for the AuthorizationManager. We create fresh instances
    // rather than reusing the global registry's Arc because set_credential_store
    // takes ownership. The file/keyring paths are the same, so credentials
    // saved by the manager will be visible to the global store on next load.
    let cred_store = oauth::CompositeCredentialStore::new(server_name.to_string());
    let state_store = oauth::CompositeStateStore::new(server_name.to_string());
    auth_manager.set_credential_store(cred_store);
    auth_manager.set_state_store(state_store);

    // 3. Discover authorization server metadata --------------------------
    ui::print_info_line("Discovering authorization server metadata...");
    let metadata = auth_manager
        .discover_metadata()
        .await
        .map_err(|e| anyhow::anyhow!("metadata discovery failed: {e}"))?;
    auth_manager.set_metadata(metadata);

    // 4. Configure or register client ------------------------------------
    let scopes: Vec<String> = server_config.oauth_scopes.clone().unwrap_or_default();

    if let Some(ref client_id) = server_config.oauth_client_id {
        // Pre-registered client: configure directly
        let oauth_config = rmcp::transport::auth::OAuthClientConfig {
            client_id: client_id.clone(),
            client_secret: None,
            scopes: scopes.clone(),
            redirect_uri: "http://localhost:0/callback".to_string(),
        };
        auth_manager
            .configure_client(oauth_config)
            .map_err(|e| anyhow::anyhow!("failed to configure OAuth client: {e}"))?;
    } else {
        // Dynamic client registration
        ui::print_info_line("Registering client with authorization server...");
        let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let reg_config = auth_manager
            .register_client("plug", "http://localhost:0/callback", &scope_refs)
            .await
            .map_err(|e| anyhow::anyhow!("client registration failed: {e}"))?;
        auth_manager
            .configure_client(reg_config)
            .map_err(|e| anyhow::anyhow!("failed to configure registered client: {e}"))?;
    }

    // 5. Generate the authorization URL ----------------------------------
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let auth_url = auth_manager
        .get_authorization_url(&scope_refs)
        .await
        .map_err(|e| anyhow::anyhow!("failed to get authorization URL: {e}"))?;

    // 6. Present to user -------------------------------------------------
    if no_browser {
        println!();
        println!("Open this URL in your browser to authorize:");
        println!();
        println!("  {auth_url}");
        println!();
    } else {
        ui::print_info_line("Opening browser for authorization...");
        if let Err(e) = open::that(&auth_url) {
            eprintln!("Could not open browser: {e}");
            println!();
            println!("Open this URL manually:");
            println!();
            println!("  {auth_url}");
            println!();
        }
    }

    // 7. Collect the callback parameters ---------------------------------
    //
    // The full implementation would start a localhost HTTP listener to capture
    // the redirect automatically. For the initial release we prompt for the
    // authorization code and CSRF state token from the callback URL.
    println!("After authorizing, paste the authorization code from the callback URL:");
    print!("> ");
    use std::io::Write;
    std::io::stdout().flush()?;

    let mut code_line = String::new();
    std::io::stdin().read_line(&mut code_line)?;
    let code = code_line.trim();
    if code.is_empty() {
        anyhow::bail!("no authorization code provided");
    }

    println!("Paste the state parameter from the callback URL:");
    print!("> ");
    std::io::stdout().flush()?;

    let mut state_line = String::new();
    std::io::stdin().read_line(&mut state_line)?;
    let csrf_state = state_line.trim();
    if csrf_state.is_empty() {
        anyhow::bail!("no state parameter provided");
    }

    // 8. Exchange code for token -----------------------------------------
    ui::print_info_line("Exchanging authorization code for token...");
    auth_manager
        .exchange_code_for_token(code, csrf_state)
        .await
        .map_err(|e| anyhow::anyhow!("token exchange failed: {e}"))?;

    ui::print_success_line(format!(
        "Successfully authenticated server '{server_name}'"
    ));

    Ok(())
}

// ---------------------------------------------------------------------------
// inject
// ---------------------------------------------------------------------------

async fn cmd_auth_inject(
    server_name: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    expires_in: Option<u64>,
) -> anyhow::Result<()> {
    use oauth2::{AccessToken, RefreshToken, basic::BasicTokenType};
    use rmcp::transport::auth::VendorExtraTokenFields;

    let store = oauth::get_or_create_store(server_name);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Build a synthetic OAuthTokenResponse via StandardTokenResponse.
    let mut token = oauth2::StandardTokenResponse::<VendorExtraTokenFields, BasicTokenType>::new(
        AccessToken::new(access_token.to_string()),
        BasicTokenType::Bearer,
        VendorExtraTokenFields::default(),
    );

    if let Some(rt) = refresh_token {
        token.set_refresh_token(Some(RefreshToken::new(rt.to_string())));
    }
    if let Some(secs) = expires_in {
        token.set_expires_in(Some(&std::time::Duration::from_secs(secs)));
    }

    let stored = StoredCredentials {
        client_id: "injected".to_string(),
        token_response: Some(token),
        granted_scopes: vec![],
        token_received_at: Some(now),
    };

    store
        .save(stored)
        .await
        .map_err(|e| anyhow::anyhow!("failed to save injected credentials: {e}"))?;

    ui::print_success_line(format!(
        "Injected credentials for server '{server_name}'"
    ));

    if refresh_token.is_some() {
        ui::print_info_line("Refresh token stored -- background refresh will work");
    } else {
        ui::print_info_line("No refresh token -- token will not auto-renew");
    }

    if let Some(secs) = expires_in {
        ui::print_info_line(format!("Token expires in {secs}s"));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

async fn cmd_auth_status(
    config_path: Option<&PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let cfg = config::load_config(config_path)?;

    let mut oauth_servers: Vec<_> = cfg
        .servers
        .iter()
        .filter(|(_, sc)| sc.auth.as_deref() == Some("oauth"))
        .collect();
    oauth_servers.sort_by_key(|(name, _)| (*name).clone());

    if oauth_servers.is_empty() {
        match output {
            OutputFormat::Text => {
                ui::print_info_line("No OAuth-configured servers found");
            }
            OutputFormat::Json => {
                println!("{}", serde_json::json!({ "servers": [] }));
            }
        }
        return Ok(());
    }

    match output {
        OutputFormat::Text => {
            println!();
            println!("{}", style("OAuth Server Status").bold());
            println!("{}", style("─".repeat(50)).dim());

            for (name, sc) in &oauth_servers {
                let store = oauth::get_or_create_store(name);
                let has_creds = store.load().await.ok().flatten().is_some();

                let status = if has_creds {
                    style("authenticated").green()
                } else {
                    style("not authenticated").red()
                };

                let health = if has_creds {
                    plug_core::types::ServerHealth::Healthy
                } else {
                    plug_core::types::ServerHealth::AuthRequired
                };

                println!(
                    "  {} {} ({})",
                    ui::status_marker(&health),
                    style(name).bold(),
                    status,
                );

                if let Some(ref url) = sc.url {
                    println!("    URL: {url}");
                }
                if let Some(ref scopes) = sc.oauth_scopes {
                    if !scopes.is_empty() {
                        println!("    Scopes: {}", scopes.join(", "));
                    }
                }

                if has_creds {
                    if let Some((received_at, expires_in)) = store.cached_expiry() {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let effective = expires_in.unwrap_or(
                            oauth::DEFAULT_TOKEN_LIFETIME_SECS,
                        );
                        let elapsed = now.saturating_sub(received_at);
                        if elapsed < effective {
                            let remaining = effective - elapsed;
                            println!("    Token expires in: {remaining}s");
                        } else {
                            println!("    Token: expired (refresh pending)");
                        }
                    }
                } else {
                    println!("    Run: plug auth login --server {name}");
                }
                println!();
            }
        }
        OutputFormat::Json => {
            let mut servers = Vec::new();
            for (name, sc) in &oauth_servers {
                let store = oauth::get_or_create_store(name);
                let has_creds = store.load().await.ok().flatten().is_some();

                servers.push(serde_json::json!({
                    "name": name,
                    "url": sc.url,
                    "authenticated": has_creds,
                    "scopes": sc.oauth_scopes,
                }));
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "servers": servers
                }))?
            );
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// logout
// ---------------------------------------------------------------------------

async fn cmd_auth_logout(server_name: &str) -> anyhow::Result<()> {
    let store = oauth::get_or_create_store(server_name);
    store
        .clear()
        .await
        .map_err(|e| anyhow::anyhow!("failed to clear credentials: {e}"))?;

    ui::print_success_line(format!("Logged out from server '{server_name}'"));

    Ok(())
}
