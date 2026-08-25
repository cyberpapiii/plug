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

const DEFAULT_MANUAL_OAUTH_CALLBACK_PORT: u16 = 45_875;

fn default_dynamic_oauth_redirect_uri() -> String {
    format!("http://localhost:{DEFAULT_MANUAL_OAUTH_CALLBACK_PORT}/callback")
}

struct PersistedOauthClientState {
    reusable_registration: Option<plug_core::oauth::DynamicOauthClientRegistration>,
    has_persisted_credentials: bool,
}

fn persisted_oauth_client_state(
    server_config: &plug_core::config::ServerConfig,
    server_name: &str,
) -> PersistedOauthClientState {
    let persisted_store = oauth::get_or_create_store(server_name);
    let reusable_registration = if server_config.oauth_client_id.is_some() {
        None
    } else {
        persisted_store.dynamic_client_registration()
    };

    PersistedOauthClientState {
        reusable_registration,
        has_persisted_credentials: persisted_store.credential_snapshot().credentials.is_some(),
    }
}

fn loopback_callback_port(redirect_uri: &str) -> anyhow::Result<u16> {
    let port_str = redirect_uri
        .strip_prefix("http://localhost:")
        .or_else(|| redirect_uri.strip_prefix("http://127.0.0.1:"))
        .and_then(|suffix| suffix.strip_suffix("/callback"))
        .ok_or_else(|| {
            anyhow::anyhow!("expected loopback callback redirect URI ending in /callback")
        })?;

    let port: u16 = port_str
        .parse()
        .map_err(|_| anyhow::anyhow!("redirect URI does not contain a valid TCP port"))?;
    if port == 0 {
        anyhow::bail!("redirect URI uses port 0 and cannot be rebound for browser login");
    }

    Ok(port)
}

async fn bind_reusable_callback_listener(
    registration: &plug_core::oauth::DynamicOauthClientRegistration,
) -> anyhow::Result<tokio::net::TcpListener> {
    let port = loopback_callback_port(&registration.redirect_uri)?;
    tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| anyhow::anyhow!("failed to bind stored callback listener: {e}"))
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
            issuer,
        } => cmd_auth_complete(config_path, &server, &code, &state, issuer.as_deref()).await,
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
        crate::AuthCommands::Clients { command } => {
            cmd_downstream_oauth_clients(config_path, command, output).await
        }
        crate::AuthCommands::Owner { command } => {
            cmd_downstream_oauth_owner(config_path, command, output).await
        }
    }
}

struct LocalOperatorClient {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    host_authority: String,
    operator_token: String,
    public_base_url: String,
}

impl LocalOperatorClient {
    fn child_endpoint(&self, segment: &str) -> anyhow::Result<reqwest::Url> {
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid local operator URL"))?
            .push(segment);
        Ok(url)
    }

    async fn send_authenticated(
        &self,
        method: reqwest::Method,
        child_segment: Option<&str>,
    ) -> anyhow::Result<reqwest::Response> {
        self.send_authenticated_with_allow_empty(method, child_segment, false)
            .await
    }

    async fn send_authenticated_with_allow_empty(
        &self,
        method: reqwest::Method,
        child_segment: Option<&str>,
        allow_empty: bool,
    ) -> anyhow::Result<reqwest::Response> {
        let endpoint = match child_segment {
            Some(segment) => self.child_endpoint(segment)?,
            None => self.endpoint.clone(),
        };
        crate::runtime::send_authenticated_operator_request(
            &self.client,
            endpoint,
            &self.host_authority,
            &self.operator_token,
            method,
            allow_empty,
        )
        .await
    }
}

fn local_operator_client(
    config_path: Option<&PathBuf>,
    endpoint_path: &str,
) -> anyhow::Result<LocalOperatorClient> {
    let cfg = config::load_config(config_path)?;
    if cfg.http.auth_mode != plug_core::config::DownstreamAuthMode::Oauth {
        anyhow::bail!("downstream OAuth is not enabled");
    }
    let token_path = plug_core::auth::http_operator_token_path(cfg.http.port);
    let operator_token = std::fs::read_to_string(&token_path)
        .map_err(|error| anyhow::anyhow!("cannot read the local operator token: {error}"))?;
    let operator_token = operator_token.trim().to_string();
    if operator_token.is_empty() {
        anyhow::bail!("the local operator token is empty");
    }
    let scheme = if cfg.http.tls_cert_path.is_some() && cfg.http.tls_key_path.is_some() {
        "https"
    } else {
        "http"
    };
    let host_authority =
        crate::runtime::local_operator_authority(&cfg.http.bind_address, cfg.http.port);
    let connection_authority =
        crate::runtime::local_operator_connection_authority(&cfg.http.bind_address, cfg.http.port);
    let endpoint =
        reqwest::Url::parse(&format!("{scheme}://{connection_authority}{endpoint_path}"))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        // Never let proxy configuration or redirects carry operator headers
        // outside the loopback request boundary.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        // Operator requests terminate on Plug's local listener. Public TLS
        // certificates commonly omit its numeric loopback address.
        .danger_accept_invalid_certs(scheme == "https")
        .build()?;
    Ok(LocalOperatorClient {
        client,
        endpoint,
        host_authority,
        operator_token,
        public_base_url: cfg
            .http
            .public_base_url
            .ok_or_else(|| anyhow::anyhow!("downstream OAuth public base URL is missing"))?,
    })
}

async fn cmd_downstream_oauth_clients(
    config_path: Option<&PathBuf>,
    command: crate::DownstreamOauthClientCommands,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    if config_path.is_none() {
        return cmd_downstream_oauth_clients_via_daemon(command, output).await;
    }
    let operator = local_operator_client(config_path, "/_plug/oauth/clients")?;

    match command {
        crate::DownstreamOauthClientCommands::List => {
            let response = operator
                .send_authenticated(reqwest::Method::GET, None)
                .await?;
            if !response.status().is_success() {
                anyhow::bail!(
                    "the running Plug service rejected the client-list request ({})",
                    response.status()
                );
            }
            let clients = response
                .json::<Vec<plug_core::downstream_oauth::RegisteredClientSummary>>()
                .await?;
            match output {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&clients)?),
                OutputFormat::Text => {
                    if clients.is_empty() {
                        ui::print_info_line("No downstream OAuth clients are registered");
                    } else {
                        println!("{}", style("Registered downstream OAuth clients").bold());
                        for registered in clients {
                            println!(
                                "  {} ({})",
                                style(&registered.client_name).bold(),
                                registered.client_id
                            );
                            println!("    Redirects: {}", registered.redirect_uris.join(", "));
                            println!("    Source: {:?}", registered.source);
                        }
                    }
                }
            }
        }
        crate::DownstreamOauthClientCommands::Revoke { client_id, yes } => {
            if !yes
                && !dialoguer::Confirm::new()
                    .with_prompt(format!("Revoke {client_id} and all of its Plug tokens?"))
                    .default(false)
                    .interact()?
            {
                ui::print_info_line("Revocation cancelled");
                return Ok(());
            }
            let response = operator
                .send_authenticated(reqwest::Method::DELETE, Some(&client_id))
                .await?;
            match response.status() {
                reqwest::StatusCode::NO_CONTENT => ui::print_success_line(format!(
                    "Revoked {client_id} and all of its downstream OAuth grants"
                )),
                reqwest::StatusCode::NOT_FOUND => anyhow::bail!("registered client not found"),
                status => {
                    anyhow::bail!("the running Plug service rejected the revocation ({status})")
                }
            }
        }
    }
    Ok(())
}

async fn cmd_downstream_oauth_clients_via_daemon(
    command: crate::DownstreamOauthClientCommands,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    crate::runtime::ensure_daemon_with_feedback(None, false).await?;
    let auth_token = crate::daemon::read_auth_token()?;
    match command {
        crate::DownstreamOauthClientCommands::List => {
            let response =
                crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::OperatorSnapshot {
                    auth_token,
                })
                .await?;
            let clients = match response {
                plug_core::ipc::IpcResponse::OperatorSnapshot { snapshot } => {
                    snapshot.downstream_clients
                }
                plug_core::ipc::IpcResponse::Error { code, message } => {
                    anyhow::bail!("{code}: {message}")
                }
                other => anyhow::bail!("unexpected daemon response: {other:?}"),
            };
            match output {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&clients)?),
                OutputFormat::Text if clients.is_empty() => {
                    ui::print_info_line("No downstream OAuth clients are registered")
                }
                OutputFormat::Text => {
                    println!("{}", style("Registered downstream OAuth clients").bold());
                    for registered in clients {
                        println!(
                            "  {} ({})",
                            style(&registered.client_name).bold(),
                            registered.client_id
                        );
                        println!("    Redirects: {}", registered.redirect_uris.join(", "));
                        println!("    Source: {:?}", registered.source);
                    }
                }
            }
        }
        crate::DownstreamOauthClientCommands::Revoke { client_id, yes } => {
            if !yes
                && !dialoguer::Confirm::new()
                    .with_prompt(format!("Revoke {client_id} and all of its Plug tokens?"))
                    .default(false)
                    .interact()?
            {
                ui::print_info_line("Revocation cancelled");
                return Ok(());
            }
            match crate::daemon::ipc_request(&plug_core::ipc::IpcRequest::RevokeDownstreamClient {
                auth_token,
                client_id: client_id.clone(),
            })
            .await?
            {
                plug_core::ipc::IpcResponse::DownstreamClientRevoked { .. } => {
                    ui::print_success_line(format!(
                        "Revoked {client_id} and all of its downstream OAuth grants"
                    ));
                }
                plug_core::ipc::IpcResponse::Error { code, message } => {
                    anyhow::bail!("{code}: {message}")
                }
                other => anyhow::bail!("unexpected daemon response: {other:?}"),
            }
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct OwnerBootstrapResponse {
    enrollment_url: String,
}

fn owner_credentials_json(
    credentials: &[plug_core::downstream_oauth::OwnerCredentialSummary],
) -> serde_json::Value {
    serde_json::to_value(credentials).expect("owner credential summaries serialize")
}

fn owner_enrollment_manual_url(
    enrollment_url: &str,
    no_browser: bool,
    browser_opened: bool,
) -> Option<&str> {
    (no_browser || !browser_opened).then_some(enrollment_url)
}

fn validate_owner_enrollment_url<'a>(
    public_base_url: &str,
    enrollment_url: &'a str,
) -> anyhow::Result<&'a str> {
    let expected = reqwest::Url::parse(public_base_url)?;
    let candidate = reqwest::Url::parse(enrollment_url).map_err(|_| {
        anyhow::anyhow!("the local Plug service returned an invalid enrollment URL")
    })?;
    let fragment = candidate.fragment().unwrap_or_default();
    let bootstrap = fragment.strip_prefix("bootstrap=").unwrap_or_default();
    let valid_bootstrap = bootstrap.len() == 43
        && bootstrap
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if candidate.scheme() != "https"
        || candidate.origin() != expected.origin()
        || !candidate.username().is_empty()
        || candidate.password().is_some()
        || candidate.path() != "/oauth/owner/enroll"
        || candidate.query().is_some()
        || !valid_bootstrap
    {
        anyhow::bail!("the local Plug service returned an invalid enrollment URL");
    }
    Ok(enrollment_url)
}

fn owner_removal_prompt(label: &str, removing_final: bool) -> String {
    if removing_final {
        format!(
            "Remove final owner passkey {label}? New downstream OAuth grants will fail until another passkey is enrolled."
        )
    } else {
        format!("Remove owner passkey {label}?")
    }
}

async fn fetch_owner_credentials(
    operator: &LocalOperatorClient,
) -> anyhow::Result<Vec<plug_core::downstream_oauth::OwnerCredentialSummary>> {
    let response = operator
        .send_authenticated(reqwest::Method::GET, None)
        .await?;
    if !response.status().is_success() {
        anyhow::bail!(
            "the running Plug service rejected the owner credential request ({})",
            response.status()
        );
    }
    Ok(response.json().await?)
}

async fn cmd_downstream_oauth_owner(
    config_path: Option<&PathBuf>,
    command: crate::OwnerCommands,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    match command {
        crate::OwnerCommands::Enroll { no_browser } => {
            let operator = local_operator_client(config_path, "/_plug/oauth/owner/bootstrap")?;
            let response = operator
                .send_authenticated(reqwest::Method::POST, None)
                .await?;
            if !response.status().is_success() {
                anyhow::bail!(
                    "the running Plug service rejected owner enrollment ({})",
                    response.status()
                );
            }
            let response: OwnerBootstrapResponse = response.json().await?;
            validate_owner_enrollment_url(&operator.public_base_url, &response.enrollment_url)?;
            let browser_opened = if no_browser {
                false
            } else {
                match open::that(&response.enrollment_url) {
                    Ok(()) => true,
                    Err(_) => {
                        eprintln!("Could not open browser. Open enrollment URL manually.");
                        false
                    }
                }
            };

            match output {
                OutputFormat::Json => {
                    let manual_url = owner_enrollment_manual_url(
                        &response.enrollment_url,
                        no_browser,
                        browser_opened,
                    );
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "browser_opened": browser_opened,
                            "enrollment_url": manual_url,
                        }))?
                    );
                }
                OutputFormat::Text => {
                    if let Some(url) = owner_enrollment_manual_url(
                        &response.enrollment_url,
                        no_browser,
                        browser_opened,
                    ) {
                        println!("Open this URL in your browser to enroll an owner passkey:\n");
                        println!("  {url}");
                    } else {
                        ui::print_success_line("Opened owner passkey enrollment in your browser");
                    }
                }
            }
        }
        crate::OwnerCommands::List => {
            let operator = local_operator_client(config_path, "/_plug/oauth/owner/credentials")?;
            let credentials = fetch_owner_credentials(&operator).await?;
            match output {
                OutputFormat::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&owner_credentials_json(&credentials))?
                ),
                OutputFormat::Text => {
                    if credentials.is_empty() {
                        ui::print_info_line("No owner passkeys are enrolled");
                    } else {
                        println!("{}", style("Enrolled owner passkeys").bold());
                        for credential in credentials {
                            println!("  {}", style(&credential.label).bold());
                            println!("    ID: {}", credential.credential_id);
                            println!("    Created: {}", credential.created_at);
                            if let Some(last_used_at) = credential.last_used_at {
                                println!("    Last used: {last_used_at}");
                            }
                        }
                    }
                }
            }
        }
        crate::OwnerCommands::Remove { credential_id, yes } => {
            let operator = local_operator_client(config_path, "/_plug/oauth/owner/credentials")?;
            let credentials = fetch_owner_credentials(&operator).await?;
            let Some(credential) = credentials
                .iter()
                .find(|credential| credential.credential_id == credential_id)
            else {
                anyhow::bail!("owner credential not found");
            };
            let removing_final = credentials.len() == 1;
            let prompt = owner_removal_prompt(&credential.label, removing_final);
            if !yes
                && !dialoguer::Confirm::new()
                    .with_prompt(prompt)
                    .default(false)
                    .interact()?
            {
                match output {
                    OutputFormat::Json => println!(
                        "{}",
                        serde_json::json!({"credential_id": credential_id, "removed": false})
                    ),
                    OutputFormat::Text => ui::print_info_line("Owner passkey removal cancelled"),
                }
                return Ok(());
            }

            let mut allow_empty = yes || removing_final;
            let mut response = operator
                .send_authenticated_with_allow_empty(
                    reqwest::Method::DELETE,
                    Some(&credential_id),
                    allow_empty,
                )
                .await?;
            if response.status() == reqwest::StatusCode::CONFLICT && !allow_empty {
                let current = fetch_owner_credentials(&operator).await?;
                if !current
                    .iter()
                    .any(|credential| credential.credential_id == credential_id)
                {
                    anyhow::bail!("owner credential not found");
                }
                if !dialoguer::Confirm::new()
                    .with_prompt(owner_removal_prompt(&credential.label, true))
                    .default(false)
                    .interact()?
                {
                    match output {
                        OutputFormat::Json => println!(
                            "{}",
                            serde_json::json!({"credential_id": credential_id, "removed": false})
                        ),
                        OutputFormat::Text => {
                            ui::print_info_line("Owner passkey removal cancelled")
                        }
                    }
                    return Ok(());
                }
                allow_empty = true;
                response = operator
                    .send_authenticated_with_allow_empty(
                        reqwest::Method::DELETE,
                        Some(&credential_id),
                        allow_empty,
                    )
                    .await?;
            }
            match response.status() {
                reqwest::StatusCode::NO_CONTENT => match output {
                    OutputFormat::Json => println!(
                        "{}",
                        serde_json::json!({"credential_id": credential_id, "removed": true})
                    ),
                    OutputFormat::Text => {
                        ui::print_success_line(format!("Removed owner passkey {credential_id}"))
                    }
                },
                reqwest::StatusCode::NOT_FOUND => anyhow::bail!("owner credential not found"),
                status => anyhow::bail!(
                    "the running Plug service rejected owner credential removal ({status})"
                ),
            }
        }
    }
    Ok(())
}

// login

async fn cmd_auth_login(
    config_path: Option<&PathBuf>,
    server_name: &str,
    no_browser: bool,
) -> anyhow::Result<()> {
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
    auth_manager.set_credential_store(cred_store.clone());
    auth_manager.set_state_store(state_store);

    ui::print_info_line("Discovering authorization server metadata...");
    let metadata = auth_manager
        .resolve_metadata()
        .await
        .map_err(|e| anyhow::anyhow!("metadata discovery failed: {e}"))?;
    let authority = oauth::VerifiedOAuthAuthority::verify(url, &metadata.metadata)
        .map_err(|e| anyhow::anyhow!("OAuth authority verification failed: {e}"))?;
    cred_store
        .bind_verified_authority(&authority)
        .map_err(|e| anyhow::anyhow!("stored credential binding conflict: {e}"))?;
    auth_manager.set_metadata(metadata.metadata);

    let scopes: Vec<String> = server_config.oauth_scopes.clone().unwrap_or_default();

    let PersistedOauthClientState {
        reusable_registration,
        has_persisted_credentials,
    } = persisted_oauth_client_state(server_config, server_name);

    // Bind the callback listener early so we know the port for the redirect URI.
    // For reusable registrations, bind the previously registered loopback port when possible.
    // If that redirect cannot be rebound, fall back to manual code entry so we can still
    // preserve registration compatibility without minting a new provider-side integration.
    let callback_listener = if no_browser {
        None
    } else if let Some(registration) = reusable_registration.as_ref() {
        match bind_reusable_callback_listener(registration).await {
            Ok(listener) => Some(listener),
            Err(error) => {
                ui::print_warning_line(format!(
                    "Stored OAuth registration for '{server_name}' uses redirect URI {} which cannot be rebound locally ({error}). Falling back to manual code entry to preserve the existing registration.",
                    registration.redirect_uri
                ));
                None
            }
        }
    } else {
        Some(
            tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|e| anyhow::anyhow!("failed to bind localhost callback listener: {e}"))?,
        )
    };

    let redirect_uri = reusable_registration
        .as_ref()
        .map(|registration| registration.redirect_uri.clone())
        .unwrap_or_else(|| match &callback_listener {
            Some(listener) => {
                let port = listener
                    .local_addr()
                    .expect("callback listener local addr")
                    .port();
                format!("http://localhost:{port}/callback")
            }
            None => default_dynamic_oauth_redirect_uri(),
        });

    if let Some(ref client_id) = server_config.oauth_client_id {
        ui::print_info_line("Using configured OAuth client...");
        let oauth_config =
            rmcp::transport::auth::OAuthClientConfig::new(client_id.clone(), redirect_uri.clone())
                .with_scopes(scopes.clone());
        auth_manager
            .configure_client(oauth_config)
            .map_err(|e| anyhow::anyhow!("failed to configure OAuth client: {e}"))?;
    } else if let Some(registration) = reusable_registration.as_ref() {
        ui::print_info_line("Reusing existing OAuth client registration...");
        let oauth_config = registration.to_oauth_client_config(scopes.clone());
        auth_manager
            .configure_client(oauth_config)
            .map_err(|e| anyhow::anyhow!("failed to configure OAuth client: {e}"))?;
    } else {
        // Dynamic client registration
        if has_persisted_credentials {
            ui::print_warning_line(format!(
                "Stored credentials for '{server_name}' do not include reusable dynamic registration metadata. Plug will register one new OAuth client so future reauth can reuse it safely."
            ));
        }
        ui::print_info_line("Registering client with authorization server...");
        let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let reg_config = auth_manager
            .register_client("plug", &redirect_uri, &scope_refs)
            .await
            .map_err(|e| anyhow::anyhow!("client registration failed: {e}"))?;
        cred_store
            .remember_dynamic_client_registration(&reg_config)
            .map_err(|e| anyhow::anyhow!("failed to persist OAuth client registration: {e}"))?;
    }

    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let auth_url = auth_manager
        .get_authorization_url(&scope_refs)
        .await
        .map_err(|e| anyhow::anyhow!("failed to get authorization URL: {e}"))?;

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

    let (code, csrf_state, callback_issuer) = if let Some(listener) = callback_listener {
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

        (code, state, None)
    };

    authority
        .validate_callback_issuer(callback_issuer.as_deref())
        .map_err(|e| anyhow::anyhow!("OAuth callback issuer validation failed: {e}"))?;

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

// complete (non-interactive code exchange)

/// Non-interactive OAuth code exchange for agents that obtained an authorization
/// code through an external mechanism (e.g. a separate browser step orchestrated
/// by an agent). Completes the token exchange without any browser or stdin
/// interaction.
async fn cmd_auth_complete(
    config_path: Option<&PathBuf>,
    server_name: &str,
    code: &str,
    csrf_state: &str,
    callback_issuer: Option<&str>,
) -> anyhow::Result<()> {
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

    use rmcp::transport::auth::AuthorizationManager;

    let mut auth_manager = AuthorizationManager::new(url)
        .await
        .map_err(|e| anyhow::anyhow!("failed to initialize OAuth for '{server_name}': {e}"))?;

    let cred_store = oauth::CompositeCredentialStore::new(server_name.to_string());
    let state_store = oauth::CompositeStateStore::new(server_name.to_string());
    auth_manager.set_credential_store(cred_store.clone());
    auth_manager.set_state_store(state_store);

    let metadata = auth_manager
        .resolve_metadata()
        .await
        .map_err(|e| anyhow::anyhow!("metadata discovery failed: {e}"))?;
    let authority = oauth::VerifiedOAuthAuthority::verify(url, &metadata.metadata)
        .map_err(|e| anyhow::anyhow!("OAuth authority verification failed: {e}"))?;
    authority
        .validate_callback_issuer(callback_issuer)
        .map_err(|e| anyhow::anyhow!("OAuth callback issuer validation failed: {e}"))?;
    cred_store
        .bind_verified_authority(&authority)
        .map_err(|e| anyhow::anyhow!("stored credential binding conflict: {e}"))?;
    auth_manager.set_metadata(metadata.metadata);

    let scopes: Vec<String> = server_config.oauth_scopes.clone().unwrap_or_default();

    let PersistedOauthClientState {
        reusable_registration,
        has_persisted_credentials,
    } = persisted_oauth_client_state(server_config, server_name);
    let redirect_uri = reusable_registration
        .as_ref()
        .map(|registration| registration.redirect_uri.clone())
        .unwrap_or_else(default_dynamic_oauth_redirect_uri);

    if let Some(ref client_id) = server_config.oauth_client_id {
        let oauth_config =
            rmcp::transport::auth::OAuthClientConfig::new(client_id.clone(), redirect_uri.clone())
                .with_scopes(scopes.clone());
        auth_manager
            .configure_client(oauth_config)
            .map_err(|e| anyhow::anyhow!("failed to configure OAuth client: {e}"))?;
    } else if let Some(registration) = reusable_registration.as_ref() {
        let oauth_config = registration.to_oauth_client_config(scopes.clone());
        auth_manager
            .configure_client(oauth_config)
            .map_err(|e| anyhow::anyhow!("failed to configure OAuth client: {e}"))?;
    } else {
        anyhow::bail!(
            "server '{server_name}' has no reusable OAuth client registration for non-interactive completion. {} Start the login flow first with `plug auth login --server {server_name} --no-browser` or configure `oauth_client_id`.",
            if has_persisted_credentials {
                "Stored credentials exist, but they were created before plug persisted reusable registration metadata."
            } else {
                "No prior reusable registration is available."
            }
        );
    }

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

// inject

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
    let (client_id, refreshable) = oauth::injected_client_identity(
        server_config.auth.as_deref() == Some("oauth"),
        server_config.oauth_client_id.as_deref(),
        existing_client_id,
        refresh_token.is_some(),
    );

    let stored = StoredCredentials::new(client_id, Some(token), vec![], Some(now));

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

// status

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
                    Some(oauth::get_or_create_store(name).fallback_auth_snapshot())
                } else {
                    None
                };
                let has_creds = live.map(|live| live.authenticated).unwrap_or_else(|| {
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
                    && !scopes.is_empty()
                {
                    println!("    Scopes: {}", scopes.join(", "));
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

                let warnings = live.map(|s| s.warnings.clone()).unwrap_or_else(|| {
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
                    Some(oauth::get_or_create_store(name).fallback_auth_snapshot())
                } else {
                    None
                };
                let has_creds = live.map(|live| live.authenticated).unwrap_or_else(|| {
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

// localhost callback listener

/// Accepts a single GET request to `/callback`, extracts `code` and `state`
/// query parameters, returns a success page to the browser, and shuts down.
///
/// Returns `(code, state, issuer)` or an error if the timeout expires or parameters
/// are missing.
async fn await_oauth_callback(
    listener: tokio::net::TcpListener,
    timeout: Duration,
) -> anyhow::Result<(String, String, Option<String>)> {
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
) -> anyhow::Result<(String, String, Option<String>)> {
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
    let issuer = params.get("iss").cloned();

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

    Ok((code, state, issuer))
}

/// Minimal HTML escaping for values interpolated into HTML responses.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

// logout

async fn cmd_auth_logout(server_name: &str) -> anyhow::Result<()> {
    let store = oauth::get_or_create_store(server_name);
    store
        .clear_credentials_preserve_registration()
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
    fn default_dynamic_oauth_redirect_uri_uses_real_loopback_port() {
        assert_eq!(
            default_dynamic_oauth_redirect_uri(),
            format!("http://localhost:{DEFAULT_MANUAL_OAUTH_CALLBACK_PORT}/callback")
        );
    }

    #[test]
    fn loopback_callback_port_accepts_supported_loopback_redirects() {
        assert_eq!(
            loopback_callback_port("http://localhost:43189/callback").unwrap(),
            43189
        );
        assert_eq!(
            loopback_callback_port("http://127.0.0.1:43189/callback").unwrap(),
            43189
        );
    }

    #[test]
    fn loopback_callback_port_rejects_port_zero_and_non_loopback_redirects() {
        assert!(loopback_callback_port("http://localhost:0/callback").is_err());
        assert!(loopback_callback_port("https://example.com/callback").is_err());
    }

    #[test]
    fn reusable_dynamic_registration_skips_configured_clients() {
        crate::install_test_credential_environment();
        let server = plug_core::config::ServerConfig {
            command: None,
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            enabled: true,
            transport: plug_core::config::TransportType::Http,
            protocol_mode: Default::default(),
            url: Some("https://example.com/mcp".to_string()),
            auth_token: None,
            auth: Some("oauth".to_string()),
            oauth_client_id: Some("configured-client".to_string()),
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: std::collections::HashMap::new(),
            tool_groups: Vec::new(),

            sandbox: None,
        };

        assert!(
            persisted_oauth_client_state(&server, "configured")
                .reusable_registration
                .is_none()
        );
    }

    #[tokio::test]
    async fn reusable_dynamic_registration_loads_persisted_registration() {
        crate::install_test_credential_environment();
        let server_name = format!("auth-registration-{}", std::process::id());
        let server = plug_core::config::ServerConfig {
            command: None,
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            enabled: true,
            transport: plug_core::config::TransportType::Http,
            protocol_mode: Default::default(),
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

            sandbox: None,
        };

        let store = oauth::get_or_create_store(&server_name);
        store.clear().await.unwrap();
        store
            .remember_dynamic_client_registration(
                &rmcp::transport::auth::OAuthClientConfig::new(
                    "dynamic-client-123",
                    "http://localhost:43189/callback",
                )
                .with_client_secret("secret-xyz")
                .with_scopes(vec!["read".to_string()]),
            )
            .expect("persist registration");

        let registration =
            persisted_oauth_client_state(&server, &server_name).reusable_registration;
        let registration = registration.expect("expected reusable registration");
        assert_eq!(
            registration,
            oauth::DynamicOauthClientRegistration {
                client_id: "dynamic-client-123".to_string(),
                client_secret: Some("secret-xyz".to_string()),
                redirect_uri: "http://localhost:43189/callback".to_string(),
            }
        );

        store.clear().await.unwrap();
    }

    #[test]
    fn injected_client_identity_requires_configured_oauth_client_for_refresh() {
        let server = plug_core::config::ServerConfig {
            command: None,
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            enabled: true,
            transport: plug_core::config::TransportType::Http,
            protocol_mode: Default::default(),
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

            sandbox: None,
        };

        let (client_id, refreshable) = oauth::injected_client_identity(
            server.auth.as_deref() == Some("oauth"),
            server.oauth_client_id.as_deref(),
            None,
            true,
        );
        assert_eq!(client_id, "client-123");
        assert!(refreshable);

        let (fallback_client_id, fallback_refreshable) = oauth::injected_client_identity(
            server.auth.as_deref() == Some("oauth"),
            server.oauth_client_id.as_deref(),
            None,
            false,
        );
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
            protocol_mode: Default::default(),
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

            sandbox: None,
        };

        let (client_id, refreshable) = oauth::injected_client_identity(
            server.auth.as_deref() == Some("oauth"),
            server.oauth_client_id.as_deref(),
            Some("dynamic-client-123"),
            true,
        );
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

    #[test]
    fn auth_owner_json_contains_summaries_only() {
        let credentials = vec![plug_core::downstream_oauth::OwnerCredentialSummary {
            credential_id: "credential-1".to_string(),
            label: "Plug owner passkey".to_string(),
            created_at: 1,
            last_used_at: None,
        }];

        let json = owner_credentials_json(&credentials);
        let item = &json.as_array().expect("array")[0];
        assert_eq!(item["credential_id"], "credential-1");
        assert!(item.get("passkey").is_none());
        assert!(item.get("public_key").is_none());
        assert!(item.get("ceremony").is_none());
    }

    #[test]
    fn auth_owner_browser_failure_returns_manual_fragment_url() {
        let url = "https://plug.example.com/oauth/owner/enroll#bootstrap=secret";
        assert_eq!(owner_enrollment_manual_url(url, false, false), Some(url));
        assert_eq!(owner_enrollment_manual_url(url, true, false), Some(url));
        assert_eq!(owner_enrollment_manual_url(url, false, true), None);
    }

    #[test]
    fn auth_owner_remove_warns_when_removing_final_credential() {
        let final_prompt = owner_removal_prompt("MacBook passkey", true);
        assert!(final_prompt.contains("final owner passkey"));
        assert!(final_prompt.contains("New downstream OAuth grants will fail"));

        let ordinary_prompt = owner_removal_prompt("iPhone passkey", false);
        assert_eq!(ordinary_prompt, "Remove owner passkey iPhone passkey?");
    }

    #[test]
    fn auth_owner_enrollment_url_requires_exact_public_fragment_contract() {
        let valid = "https://plug.example.com/oauth/owner/enroll#bootstrap=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert_eq!(
            validate_owner_enrollment_url("https://plug.example.com", valid).unwrap(),
            valid
        );
        for invalid in [
            "http://plug.example.com/oauth/owner/enroll#bootstrap=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "https://evil.example/oauth/owner/enroll#bootstrap=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "https://plug.example.com/oauth/owner/enroll?bootstrap=x#bootstrap=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "https://plug.example.com/other#bootstrap=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "https://plug.example.com/oauth/owner/enroll#bootstrap=short",
            "https://user@plug.example.com/oauth/owner/enroll#bootstrap=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(
                validate_owner_enrollment_url("https://plug.example.com", invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[tokio::test]
    async fn auth_owner_rogue_listener_never_receives_reusable_operator_token() {
        plug_core::tls::ensure_rustls_provider_installed();
        let protected_route_hit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let protected_route_state = protected_route_hit.clone();
        let app = axum::Router::new()
            .route(
                "/_plug/operator/proof",
                axum::routing::post(|headers: axum::http::HeaderMap| async move {
                    assert!(!headers.contains_key("x-plug-operator-token"));
                    axum::Json(serde_json::json!({
                        "server_nonce": "22".repeat(32),
                        "proof": "00".repeat(32),
                    }))
                }),
            )
            .route(
                "/_plug/oauth/owner/credentials",
                axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let protected_route_state = protected_route_state.clone();
                    async move {
                        protected_route_state.store(true, std::sync::atomic::Ordering::SeqCst);
                        assert!(!headers.contains_key("x-plug-operator-token"));
                        axum::Json(serde_json::json!([]))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let operator = LocalOperatorClient {
            client,
            endpoint: reqwest::Url::parse(&format!("http://{addr}/_plug/oauth/owner/credentials"))
                .unwrap(),
            host_authority: addr.to_string(),
            operator_token: "operator-secret".to_string(),
            public_base_url: "https://plug.example.com".to_string(),
        };

        let error = operator
            .send_authenticated(reqwest::Method::GET, None)
            .await
            .expect_err("rogue proof must fail");
        assert!(
            error
                .to_string()
                .contains("authenticate local Plug service")
        );
        assert!(!protected_route_hit.load(std::sync::atomic::Ordering::SeqCst));
        server.abort();
    }

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
                b"GET /callback?code=abc123&state=xyz789&iss=https%3A%2F%2Fauth.example HTTP/1.1\r\nHost: localhost\r\n\r\n",
            )
            .await
            .unwrap();

        let (code, state, issuer) = handle.await.unwrap().unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state, "xyz789");
        assert_eq!(issuer.as_deref(), Some("https://auth.example"));
    }

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

        let (code, state, issuer) = handle.await.unwrap().unwrap();
        assert_eq!(code, "abc/123+xyz=");
        assert_eq!(state, "hello world");
        assert_eq!(issuer, None);
    }

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

    #[tokio::test]
    async fn callback_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

        let err = await_oauth_callback(listener, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "got: {}", err);
    }
}
