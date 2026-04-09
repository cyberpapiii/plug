//! OAuth authentication commands for upstream MCP servers.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dialoguer::console::style;
use rmcp::transport::auth::{CredentialStore, StoredCredentials};

use plug_core::config;
use plug_core::oauth;

use crate::OutputFormat;
use crate::ui;

fn auth_recovery_hint(
    name: &str,
    authenticated: bool,
    health: Option<plug_core::types::ServerHealth>,
) -> String {
    use plug_core::types::ServerHealth;

    match (authenticated, health) {
        (false, Some(ServerHealth::AuthRequired)) | (false, None) => {
            format!("Run: plug auth login --server {name}")
        }
        (true, Some(ServerHealth::AuthRequired)) => format!(
            "Stored credentials are present, but re-auth is required — run: plug auth login --server {name}"
        ),
        (true, Some(ServerHealth::Failed)) => {
            "Credentials are present, but the server is failing for a non-auth reason — check `plug status` and `plug doctor`".to_string()
        }
        (true, Some(ServerHealth::Degraded)) => {
            "Credentials are present, but runtime health is degraded — compare `plug status` and `plug doctor`".to_string()
        }
        _ => String::new(),
    }
}

fn auth_status_source_text(live: bool) -> &'static str {
    if live {
        "Status reflects live daemon auth/runtime state."
    } else {
        "Daemon auth state unavailable; status reflects stored credentials and config only."
    }
}

fn auth_status_json(servers: Vec<serde_json::Value>, live: bool) -> serde_json::Value {
    serde_json::json!({
        "runtime_available": live,
        "servers": servers,
        "status_source": if live {
            "live_daemon"
        } else {
            "stored_credentials_only"
        },
        "status_scope": if live {
            "live_daemon"
        } else {
            "stored_credentials_only"
        }
    })
}

async fn refresh_live_daemon_server(server_name: &str) -> anyhow::Result<bool> {
    if !crate::daemon::socket_path().exists() {
        return Ok(false);
    }

    let auth_token = match crate::daemon::read_auth_token() {
        Ok(token) => token,
        Err(_) => return Ok(false),
    };

    let request = plug_core::ipc::IpcRequest::RestartServer {
        server_id: server_name.to_string(),
        auth_token,
    };

    match crate::daemon::ipc_request(&request).await {
        Ok(plug_core::ipc::IpcResponse::Ok) => Ok(true),
        Ok(plug_core::ipc::IpcResponse::Error { code, message }) => {
            anyhow::bail!("{code}: {message}");
        }
        Ok(other) => anyhow::bail!("unexpected daemon response: {other:?}"),
        Err(err) => Err(err),
    }
}

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
        crate::AuthCommands::Complete {
            server,
            code,
            state,
        } => cmd_auth_complete(config_path, &server, &code, &state).await,
        crate::AuthCommands::Inject {
            server,
            access_token,
            refresh_token,
            expires_in,
        } => {
            cmd_auth_inject(
                config_path,
                &server,
                &access_token,
                refresh_token.as_deref(),
                expires_in,
            )
            .await
        }
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

    // Bind the callback listener early so we know the port for the redirect URI.
    // In --no-browser mode we skip the listener and use manual code entry.
    let callback_listener = if no_browser {
        None
    } else {
        Some(
            tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|e| anyhow::anyhow!("failed to bind localhost callback listener: {e}"))?,
        )
    };

    let redirect_uri = match &callback_listener {
        Some(listener) => {
            let port = listener.local_addr()?.port();
            format!("http://localhost:{port}/callback")
        }
        None => "http://localhost:0/callback".to_string(),
    };

    if let Some(ref client_id) = server_config.oauth_client_id {
        // Pre-registered client: configure directly
        let oauth_config = rmcp::transport::auth::OAuthClientConfig {
            client_id: client_id.clone(),
            client_secret: None,
            scopes: scopes.clone(),
            redirect_uri: redirect_uri.clone(),
        };
        auth_manager
            .configure_client(oauth_config)
            .map_err(|e| anyhow::anyhow!("failed to configure OAuth client: {e}"))?;
    } else {
        // Dynamic client registration
        ui::print_info_line("Registering client with authorization server...");
        let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let reg_config = auth_manager
            .register_client("plug", &redirect_uri, &scope_refs)
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
    let (code, csrf_state) = if let Some(listener) = callback_listener {
        // Localhost callback: wait for the OAuth redirect with a 120s timeout.
        ui::print_info_line("Waiting for OAuth callback on localhost...");
        await_oauth_callback(listener, Duration::from_secs(120)).await?
    } else {
        // Manual entry fallback for --no-browser / headless environments.
        use std::io::Write;
        println!("After authorizing, paste the authorization code from the callback URL:");
        print!("> ");
        std::io::stdout().flush()?;

        let mut code_line = String::new();
        std::io::stdin().read_line(&mut code_line)?;
        let code = code_line.trim().to_string();
        if code.is_empty() {
            anyhow::bail!("no authorization code provided");
        }

        println!("Paste the state parameter from the callback URL:");
        print!("> ");
        std::io::stdout().flush()?;

        let mut state_line = String::new();
        std::io::stdin().read_line(&mut state_line)?;
        let state = state_line.trim().to_string();
        if state.is_empty() {
            anyhow::bail!("no state parameter provided");
        }

        (code, state)
    };

    // 8. Exchange code for token -----------------------------------------
    ui::print_info_line("Exchanging authorization code for token...");
    auth_manager
        .exchange_code_for_token(&code, &csrf_state)
        .await
        .map_err(|e| anyhow::anyhow!("token exchange failed: {e}"))?;

    ui::print_success_line(format!("Successfully authenticated server '{server_name}'"));
    match refresh_live_daemon_server(server_name).await {
        Ok(true) => {
            ui::print_info_line(format!(
                "Refreshed live daemon state for server '{server_name}'"
            ));
        }
        Ok(false) => {}
        Err(err) => {
            ui::print_warning_line(format!(
                "Credentials were saved, but the running service did not reload them automatically: {err}. Next: run `plug stop && plug start`."
            ));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// complete (non-interactive code exchange)
// ---------------------------------------------------------------------------

/// Non-interactive OAuth code exchange for agents that obtained an authorization
/// code through an external mechanism (e.g. a separate browser step orchestrated
/// by an agent). Completes the token exchange without any browser or stdin
/// interaction.
async fn cmd_auth_complete(
    config_path: Option<&PathBuf>,
    server_name: &str,
    code: &str,
    csrf_state: &str,
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

    // 2. Build an AuthorizationManager with persistent stores ------------
    use rmcp::transport::auth::AuthorizationManager;

    let mut auth_manager = AuthorizationManager::new(url)
        .await
        .map_err(|e| anyhow::anyhow!("failed to initialize OAuth for '{server_name}': {e}"))?;

    let cred_store = oauth::CompositeCredentialStore::new(server_name.to_string());
    let state_store = oauth::CompositeStateStore::new(server_name.to_string());
    auth_manager.set_credential_store(cred_store);
    auth_manager.set_state_store(state_store);

    // 3. Discover metadata and configure client --------------------------
    let metadata = auth_manager
        .discover_metadata()
        .await
        .map_err(|e| anyhow::anyhow!("metadata discovery failed: {e}"))?;
    auth_manager.set_metadata(metadata);

    let scopes: Vec<String> = server_config.oauth_scopes.clone().unwrap_or_default();

    // Use the placeholder redirect URI — in the complete flow the redirect
    // has already happened externally, so this is only needed for client
    // configuration / registration parity.
    let redirect_uri = "http://localhost:0/callback";

    if let Some(ref client_id) = server_config.oauth_client_id {
        let oauth_config = rmcp::transport::auth::OAuthClientConfig {
            client_id: client_id.clone(),
            client_secret: None,
            scopes: scopes.clone(),
            redirect_uri: redirect_uri.to_string(),
        };
        auth_manager
            .configure_client(oauth_config)
            .map_err(|e| anyhow::anyhow!("failed to configure OAuth client: {e}"))?;
    } else {
        let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let reg_config = auth_manager
            .register_client("plug", redirect_uri, &scope_refs)
            .await
            .map_err(|e| anyhow::anyhow!("client registration failed: {e}"))?;
        auth_manager
            .configure_client(reg_config)
            .map_err(|e| anyhow::anyhow!("failed to configure registered client: {e}"))?;
    }

    // 4. Exchange code for token -----------------------------------------
    ui::print_info_line("Exchanging authorization code for token...");
    auth_manager
        .exchange_code_for_token(code, csrf_state)
        .await
        .map_err(|e| anyhow::anyhow!("token exchange failed: {e}"))?;

    ui::print_success_line(format!("Successfully authenticated server '{server_name}'"));
    match refresh_live_daemon_server(server_name).await {
        Ok(true) => {
            ui::print_info_line(format!(
                "Refreshed live daemon state for server '{server_name}'"
            ));
        }
        Ok(false) => {}
        Err(err) => {
            ui::print_warning_line(format!(
                "Credentials were saved, but the running service did not reload them automatically: {err}. Next: run `plug stop && plug start`."
            ));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// inject
// ---------------------------------------------------------------------------

fn injected_client_identity(
    server_config: Option<&plug_core::config::ServerConfig>,
    existing_client_id: Option<&str>,
    has_refresh_token: bool,
) -> (String, bool) {
    let Some(server_config) = server_config else {
        return ("injected".to_string(), false);
    };
    if !has_refresh_token || server_config.auth.as_deref() != Some("oauth") {
        return ("injected".to_string(), false);
    }
    if let Some(client_id) = server_config.oauth_client_id.as_deref() {
        return (client_id.to_string(), true);
    }
    if let Some(client_id) = existing_client_id.filter(|client_id| *client_id != "injected") {
        return (client_id.to_string(), true);
    }
    ("injected".to_string(), false)
}

async fn cmd_auth_inject(
    config_path: Option<&PathBuf>,
    server_name: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    expires_in: Option<u64>,
) -> anyhow::Result<()> {
    use oauth2::{AccessToken, RefreshToken, basic::BasicTokenType};
    use rmcp::transport::auth::VendorExtraTokenFields;

    let cfg = config::load_config(config_path)?;
    let server_config = cfg
        .servers
        .get(server_name)
        .ok_or_else(|| anyhow::anyhow!("server '{server_name}' not found in config"))?;
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

    let snapshot = store.credential_snapshot();
    let existing_client_id = snapshot
        .credentials
        .as_ref()
        .map(|creds| creds.client_id.as_str());
    let (client_id, refreshable) =
        injected_client_identity(Some(server_config), existing_client_id, refresh_token.is_some());

    let stored = StoredCredentials {
        client_id,
        token_response: Some(token),
        granted_scopes: vec![],
        token_received_at: Some(now),
    };

    store
        .save(stored)
        .await
        .map_err(|e| anyhow::anyhow!("failed to save injected credentials: {e}"))?;

    match refresh_live_daemon_server(server_name).await {
        Ok(true) => ui::print_info_line("Refreshed live daemon server state"),
        Ok(false) => {}
        Err(err) => ui::print_warning_line(format!(
            "Stored credentials but failed to refresh the live daemon state: {err}"
        )),
    }

    ui::print_success_line(format!("Injected credentials for server '{server_name}'"));

    if refresh_token.is_some() {
        if refreshable {
            ui::print_info_line("Refresh token stored -- background refresh is enabled");
        } else {
            ui::print_warning_line(
                "Refresh token stored, but automatic refresh is unavailable without a configured OAuth client ID.",
            );
        }
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
                println!(
                    "{}",
                    serde_json::to_string_pretty(&auth_status_json(Vec::new(), false))?
                );
            }
        }
        return Ok(());
    }

    let live_auth_status =
        match crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::AuthStatus).await {
            Ok(plug_core::ipc::IpcResponse::AuthStatus { servers }) => Some(
                servers
                    .into_iter()
                    .map(|s| (s.name.clone(), s))
                    .collect::<std::collections::HashMap<_, _>>(),
            ),
            _ => None,
        };

    match output {
        OutputFormat::Text => {
            println!();
            println!("{}", style("OAuth Server Status").bold());
            println!("{}", style("─".repeat(50)).dim());
            println!(
                "{}",
                style(auth_status_source_text(live_auth_status.is_some())).dim()
            );
            if live_auth_status.is_none() {
                ui::print_warning_line(
                    "Live daemon auth state is unavailable. Start the shared service with `plug start` for authoritative runtime auth status.",
                );
            }
            println!();

            for (name, sc) in &oauth_servers {
                let live = live_auth_status.as_ref().and_then(|m| m.get(*name));
                let snapshot = if live.is_none() {
                    Some(oauth::get_or_create_store(name).credential_snapshot())
                } else {
                    None
                };
                let has_creds = live
                    .map(|live| live.authenticated)
                    .unwrap_or_else(|| {
                        snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.credentials.as_ref())
                            .is_some()
                    });

                let health = live.map(|s| s.health);

                let status = match (has_creds, health) {
                    (false, _) => style("not authenticated").red(),
                    (true, Some(plug_core::types::ServerHealth::AuthRequired)) => {
                        style("credentials present, re-auth required").yellow()
                    }
                    (true, Some(plug_core::types::ServerHealth::Failed)) => {
                        style("credentials present, server failed").red()
                    }
                    (true, Some(plug_core::types::ServerHealth::Degraded)) => {
                        style("authenticated, degraded").yellow()
                    }
                    (true, Some(plug_core::types::ServerHealth::Healthy)) => {
                        style("authenticated").green()
                    }
                    (true, None) => style("credentials present, runtime unavailable").yellow(),
                };

                println!(
                    "  {} {} ({})",
                    ui::status_marker(&health.unwrap_or(plug_core::types::ServerHealth::Degraded)),
                    style(name).bold(),
                    status,
                );

                if let Some(ref url) = sc.url {
                    println!("    URL: {url}");
                }
                if let Some(scopes) = live
                    .and_then(|s| s.scopes.clone())
                    .or_else(|| sc.oauth_scopes.clone())
                {
                    if !scopes.is_empty() {
                        println!("    Scopes: {}", scopes.join(", "));
                    }
                }

                if let Some(remaining) = live.and_then(|s| s.token_expires_in_secs) {
                    println!("    Token expires in: {remaining}s");
                } else if let Some(remaining) = snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.token_expires_in_secs)
                {
                    println!("    Token expires in: {remaining}s");
                } else if has_creds {
                    println!("    Token: expired (refresh pending)");
                }

                let warnings = live
                    .map(|s| s.warnings.clone())
                    .unwrap_or_else(|| {
                        snapshot
                            .as_ref()
                            .map(|snapshot| snapshot.warnings.clone())
                            .unwrap_or_default()
                    });
                for warning in warnings {
                    ui::print_warning_line(format!("{name}: {warning}"));
                }

                let hint = auth_recovery_hint(name, has_creds, health);
                if !hint.is_empty() {
                    println!("    {hint}");
                }
                println!();
            }
        }
        OutputFormat::Json => {
            let mut servers = Vec::new();
            for (name, sc) in &oauth_servers {
                let live = live_auth_status.as_ref().and_then(|m| m.get(*name));
                let snapshot = if live.is_none() {
                    Some(oauth::get_or_create_store(name).credential_snapshot())
                } else {
                    None
                };
                let has_creds = live
                    .map(|live| live.authenticated)
                    .unwrap_or_else(|| {
                        snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.credentials.as_ref())
                            .is_some()
                    });
                let health = live.map(|s| s.health);

                servers.push(serde_json::json!({
                    "name": name,
                    "url": live.and_then(|s| s.url.clone()).or_else(|| sc.url.clone()),
                    "authenticated": has_creds,
                    "health": health.map(|value| format!("{value:?}")),
                    "scopes": live.and_then(|s| s.scopes.clone()).or_else(|| sc.oauth_scopes.clone()),
                    "token_expires_in_secs": live
                        .and_then(|s| s.token_expires_in_secs)
                        .or_else(|| snapshot.as_ref().and_then(|snapshot| snapshot.token_expires_in_secs)),
                    "warnings": live
                        .map(|s| s.warnings.clone())
                        .unwrap_or_else(|| snapshot.as_ref().map(|snapshot| snapshot.warnings.clone()).unwrap_or_default()),
                    "recovery_hint": auth_recovery_hint(name, has_creds, health),
                    "status_source": if live.is_some() {
                        "live_daemon"
                    } else {
                        "stored_credentials_only"
                    },
                    "status_scope": if live.is_some() {
                        "live_daemon"
                    } else {
                        "stored_credentials_only"
                    },
                }));
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&auth_status_json(
                    servers,
                    live_auth_status.is_some(),
                ))?
            );
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// localhost callback listener
// ---------------------------------------------------------------------------

/// Accepts a single GET request to `/callback`, extracts `code` and `state`
/// query parameters, returns a success page to the browser, and shuts down.
///
/// Returns `(code, state)` or an error if the timeout expires or parameters
/// are missing.
async fn await_oauth_callback(
    listener: tokio::net::TcpListener,
    timeout: Duration,
) -> anyhow::Result<(String, String)> {
    // Wrap the entire accept + read + respond cycle in the timeout so a
    // slow or malicious connection cannot hang the CLI indefinitely.
    tokio::time::timeout(timeout, await_oauth_callback_inner(listener))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "timed out waiting for OAuth callback ({}s)",
                timeout.as_secs()
            )
        })?
}

async fn await_oauth_callback_inner(
    listener: tokio::net::TcpListener,
) -> anyhow::Result<(String, String)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut stream, _addr) = listener
        .accept()
        .await
        .map_err(|e| anyhow::anyhow!("failed to accept callback connection: {e}"))?;

    // Read the HTTP request in a loop until we see the end-of-headers
    // marker (\r\n\r\n). A single read() is not guaranteed to return the
    // full request on a TCP stream.
    let mut buf = vec![0u8; 4096];
    let mut total = 0;
    loop {
        let n = stream
            .read(&mut buf[total..])
            .await
            .map_err(|e| anyhow::anyhow!("failed to read callback request: {e}"))?;
        if n == 0 {
            break;
        }
        total += n;
        if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if total >= buf.len() {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf[..total]);

    // Parse the request line: "GET /callback?code=...&state=... HTTP/1.1"
    let request_line = request.lines().next().unwrap_or("");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();

    // Extract query parameters using standard URL decoding. OAuth callback
    // values are opaque and must be forwarded exactly as decoded from the URL.
    let request_url = format!("http://localhost{path}");
    let params = reqwest::Url::parse(&request_url)
        .map_err(|e| anyhow::anyhow!("invalid callback URL: {e}"))?;
    let params: std::collections::HashMap<String, String> = params
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    // Check for an error response from the authorization server.
    if let Some(err) = params.get("error") {
        let desc = params
            .get("error_description")
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        let escaped_err = html_escape(err);
        let escaped_desc = html_escape(&desc);
        let error_html = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             Connection: close\r\n\r\n\
             <html><body><h2>Authentication failed</h2>\
             <p>{escaped_err}{escaped_desc}</p>\
             <p>You can close this tab.</p></body></html>"
        );
        let _ = stream.write_all(error_html.as_bytes()).await;
        let _ = stream.shutdown().await;
        anyhow::bail!("authorization server returned error: {err}{desc}");
    }

    let code = params
        .get("code")
        .ok_or_else(|| anyhow::anyhow!("callback URL missing 'code' parameter"))?
        .to_string();
    let state = params
        .get("state")
        .ok_or_else(|| anyhow::anyhow!("callback URL missing 'state' parameter"))?
        .to_string();

    // Respond with a success page and close.
    let success_html = "HTTP/1.1 200 OK\r\n\
        Content-Type: text/html; charset=utf-8\r\n\
        Connection: close\r\n\r\n\
        <html><body>\
        <h2>Authentication successful</h2>\
        <p>You can close this tab and return to the terminal.</p>\
        </body></html>";
    let _ = stream.write_all(success_html.as_bytes()).await;
    let _ = stream.shutdown().await;

    Ok((code, state))
}

/// Minimal HTML escaping for values interpolated into HTML responses.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
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
    match refresh_live_daemon_server(server_name).await {
        Ok(true) => {
            ui::print_info_line(format!(
                "Refreshed live daemon state for server '{server_name}'"
            ));
        }
        Ok(false) => {}
        Err(err) => {
            ui::print_warning_line(format!(
                "Stored credentials were cleared, but the running service did not reload them automatically: {err}. Next: run `plug stop && plug start`."
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn injected_client_identity_requires_configured_oauth_client_for_refresh() {
        let server = plug_core::config::ServerConfig {
            command: None,
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            enabled: true,
            transport: plug_core::config::TransportType::Http,
            url: Some("https://example.com/mcp".to_string()),
            auth_token: None,
            auth: Some("oauth".to_string()),
            oauth_client_id: Some("client-123".to_string()),
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: std::collections::HashMap::new(),
            tool_groups: Vec::new(),
        };

        let (client_id, refreshable) = injected_client_identity(Some(&server), None, true);
        assert_eq!(client_id, "client-123");
        assert!(refreshable);

        let (fallback_client_id, fallback_refreshable) =
            injected_client_identity(Some(&server), None, false);
        assert_eq!(fallback_client_id, "injected");
        assert!(!fallback_refreshable);
    }

    #[test]
    fn injected_client_identity_reuses_existing_oauth_client_for_refresh() {
        let server = plug_core::config::ServerConfig {
            command: None,
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            enabled: true,
            transport: plug_core::config::TransportType::Http,
            url: Some("https://example.com/mcp".to_string()),
            auth_token: None,
            auth: Some("oauth".to_string()),
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: std::collections::HashMap::new(),
            tool_groups: Vec::new(),
        };

        let (client_id, refreshable) =
            injected_client_identity(Some(&server), Some("dynamic-client-123"), true);
        assert_eq!(client_id, "dynamic-client-123");
        assert!(refreshable);
    }

    #[test]
    fn auth_status_source_text_distinguishes_live_from_fallback() {
        assert!(auth_status_source_text(true).contains("live daemon"));
        assert!(auth_status_source_text(false).contains("stored credentials"));
    }

    #[test]
    fn auth_status_json_exposes_source_and_compat_scope() {
        let servers = vec![serde_json::json!({
            "name": "notion",
            "status_source": "live_daemon",
            "warnings": ["token file mirror exists but keyring entry is missing"],
        })];
        let json = auth_status_json(servers, true);
        assert_eq!(json["runtime_available"], true);
        assert_eq!(json["status_source"], "live_daemon");
        assert_eq!(json["status_scope"], "live_daemon");
        assert_eq!(json["servers"][0]["name"], "notion");
        assert_eq!(
            json["servers"][0]["warnings"][0],
            "token file mirror exists but keyring entry is missing"
        );
    }

    #[test]
    fn auth_status_json_empty_case_keeps_stable_envelope() {
        let json = auth_status_json(Vec::new(), false);
        assert_eq!(json["runtime_available"], false);
        assert_eq!(json["status_source"], "stored_credentials_only");
        assert_eq!(json["status_scope"], "stored_credentials_only");
        assert!(json["servers"].as_array().is_some());
        assert_eq!(json["servers"].as_array().unwrap().len(), 0);
    }

    /// Simulates a browser redirect delivering code and state to the callback
    /// listener. Proves the happy path extracts both parameters correctly.
    #[tokio::test]
    async fn callback_extracts_code_and_state() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle =
            tokio::spawn(
                async move { await_oauth_callback(listener, Duration::from_secs(5)).await },
            );

        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        client
            .write_all(
                b"GET /callback?code=abc123&state=xyz789 HTTP/1.1\r\nHost: localhost\r\n\r\n",
            )
            .await
            .unwrap();

        let (code, state) = handle.await.unwrap().unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state, "xyz789");
    }

    /// Proves that percent-encoded callback parameters are decoded before
    /// token exchange.
    #[tokio::test]
    async fn callback_decodes_percent_encoded_code_and_state() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle =
            tokio::spawn(
                async move { await_oauth_callback(listener, Duration::from_secs(5)).await },
            );

        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        client
            .write_all(
                b"GET /callback?code=abc%2F123%2Bxyz%3D&state=hello%20world HTTP/1.1\r\nHost: localhost\r\n\r\n",
            )
            .await
            .unwrap();

        let (code, state) = handle.await.unwrap().unwrap();
        assert_eq!(code, "abc/123+xyz=");
        assert_eq!(state, "hello world");
    }

    /// Proves that the listener returns an error when the authorization server
    /// redirects with an error parameter instead of a code.
    #[tokio::test]
    async fn callback_returns_error_on_oauth_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle =
            tokio::spawn(
                async move { await_oauth_callback(listener, Duration::from_secs(5)).await },
            );

        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        client
            .write_all(
                b"GET /callback?error=access_denied&error_description=user+refused HTTP/1.1\r\nHost: localhost\r\n\r\n",
            )
            .await
            .unwrap();

        let err = handle.await.unwrap().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("access_denied"), "got: {msg}");
    }

    /// Proves that missing `code` parameter is rejected.
    #[tokio::test]
    async fn callback_rejects_missing_code() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle =
            tokio::spawn(
                async move { await_oauth_callback(listener, Duration::from_secs(5)).await },
            );

        let mut client = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        client
            .write_all(b"GET /callback?state=xyz789 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();

        let err = handle.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("missing 'code'"), "got: {}", err);
    }

    /// Proves that the listener times out if no connection arrives.
    #[tokio::test]
    async fn callback_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

        let err = await_oauth_callback(listener, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "got: {}", err);
    }
}
