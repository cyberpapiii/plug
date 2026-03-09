//! OAuth authentication commands for upstream MCP servers.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dialoguer::console::style;
use rmcp::transport::auth::{CredentialStore, StoredCredentials};

use plug_core::config;
use plug_core::oauth;

use crate::OutputFormat;
use crate::ui;

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

    ui::print_success_line(format!("Injected credentials for server '{server_name}'"));

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
                        let effective = expires_in.unwrap_or(oauth::DEFAULT_TOKEN_LIFETIME_SECS);
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut stream, _addr) = tokio::time::timeout(timeout, listener.accept())
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for OAuth callback (120s)"))?
        .map_err(|e| anyhow::anyhow!("failed to accept callback connection: {e}"))?;

    // Read the HTTP request. The callback is a simple browser GET, so a
    // small buffer is plenty.
    let mut buf = vec![0u8; 4096];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| anyhow::anyhow!("failed to read callback request: {e}"))?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Parse the request line: "GET /callback?code=...&state=... HTTP/1.1"
    let request_line = request.lines().next().unwrap_or("");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();

    // Extract query parameters. Simple parser — OAuth callback query strings
    // contain only ASCII keys/values so percent-decoding is not needed here.
    let query = path.split('?').nth(1).unwrap_or("");
    let params: std::collections::HashMap<&str, &str> = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .collect();

    // Check for an error response from the authorization server.
    if let Some(&err) = params.get("error") {
        let desc = params
            .get("error_description")
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        let error_html = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             Connection: close\r\n\r\n\
             <html><body><h2>Authentication failed</h2>\
             <p>{err}{desc}</p>\
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

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
