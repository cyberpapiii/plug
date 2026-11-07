use std::net::SocketAddr;
use std::sync::Arc;

use crate::OutputFormat;
use crate::daemon;
use crate::operator_http::{build_runtime_router, local_operator_authority};
use crate::ui::{print_banner, print_info_line, print_success_line};
use anyhow::Context as _;
use axum::Router;
use tokio_util::sync::CancellationToken;

struct ConfiguredHttpRuntime {
    router: Router,
    state: Arc<plug_core::http::server::HttpState>,
}

fn build_configured_http_runtime(
    config: &plug_core::config::Config,
    engine: &Arc<plug_core::engine::Engine>,
) -> anyhow::Result<ConfiguredHttpRuntime> {
    let sessions = Arc::new(plug_core::session::SessionStore::new(
        config.http.session_timeout_secs,
        config.http.max_sessions,
    ));
    let tool_router = engine.tool_router().clone();
    let auth_token = resolve_downstream_bearer_token(&config.http)?;
    let operator_token = Arc::<str>::from(plug_core::auth::load_or_generate_token(
        &plug_core::auth::http_operator_token_path(config.http.port),
    )?);
    let downstream_oauth =
        plug_core::downstream_oauth::DownstreamOauthConfig::from_http_config(&config.http)
            .map(plug_core::downstream_oauth::DownstreamOauthManager::try_new)
            .transpose()
            .map_err(|error| anyhow::anyhow!(error))?;
    if config.http.auth_mode == plug_core::config::DownstreamAuthMode::Oauth
        && downstream_oauth.is_none()
    {
        anyhow::bail!("http.public_base_url is required when http.auth_mode = \"oauth\"");
    }

    // Slack events are one feature among many: a secret that cannot be read
    // or state that cannot be opened leaves them off, and everything else on.
    let slack_events = config.http.slack_events.as_ref().and_then(|event_config| {
        let open = || -> anyhow::Result<_> {
            let oauth = downstream_oauth
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Slack events require OAuth"))?;
            let secret = plug_core::slack_events::credentials::SigningCredential::new(
                &event_config.team_id,
                &event_config.app_id,
            )?
            .load(&plug_core::slack_events::credentials::KeychainSigningSecrets)?;
            plug_core::slack_events::SlackEvents::open(
                event_config.clone(),
                secret,
                &plug_core::slack_events::state_dir(event_config)?,
                Arc::new(plug_core::slack_events::RuntimeAccess::new(
                    engine,
                    oauth,
                    event_config.clone(),
                )),
                Arc::new(plug_core::events::HttpsDelivery),
            )
        };
        let events = open()
            .inspect_err(|error| tracing::error!(%error, "Slack events unavailable"))
            .ok()?;
        events.spawn(engine.cancel_token().clone());
        Some(events)
    });

    // Opened whenever OAuth is on, so a watch added by a reload needs no restart.
    // A watch is a convenience: failing to open its state must not stop Plug.
    let watch_events = downstream_oauth.clone().and_then(|oauth| {
        plug_core::events::WatchEvents::open(
            &plug_core::paths::events_dir().join("watch"),
            Arc::new(plug_core::engine::RuntimeWatchAccess::new(engine, oauth)),
            Arc::new(plug_core::events::HttpsDelivery),
        )
        .inspect_err(|error| tracing::warn!(%error, "watch events unavailable"))
        .ok()
    });
    if let Some(events) = &watch_events {
        engine.set_watch_events(Arc::clone(events));
        events.spawn(engine.cancel_token().clone());
    }
    let event_sources: Vec<Arc<dyn plug_core::events::EventSource>> = slack_events
        .into_iter()
        .map(|events| events as Arc<dyn plug_core::events::EventSource>)
        .chain(watch_events.map(|events| events as Arc<dyn plug_core::events::EventSource>))
        .collect();

    let auth = match (downstream_oauth.clone(), auth_token) {
        (Some(manager), _) => plug_core::http::server::HttpAuth::Oauth(manager),
        (None, Some(token)) => plug_core::http::server::HttpAuth::Bearer(token),
        (None, None) => plug_core::http::server::HttpAuth::Open,
    };
    let http_state = Arc::new(
        plug_core::http::server::HttpState::new(
            tool_router.clone(),
            sessions,
            engine.cancel_token().clone(),
        )
        .with_auth(auth)
        .with_event_sources(event_sources)
        .with_sse_channel_capacity(config.http.sse_channel_capacity)
        .with_allowed_origins(
            config
                .http
                .allowed_origins
                .iter()
                .cloned()
                .map(Arc::<str>::from)
                .collect(),
        )
        .with_public_base_url(config.http.public_base_url.as_deref()),
    );

    Ok(ConfiguredHttpRuntime {
        router: build_runtime_router(
            Arc::clone(&http_state),
            operator_token,
            Arc::from(local_operator_authority(
                &config.http.bind_address,
                config.http.port,
            )),
        ),
        state: http_state,
    })
}

pub(crate) async fn ensure_daemon_with_feedback(
    config_path: Option<&std::path::PathBuf>,
    announce: bool,
) -> anyhow::Result<bool> {
    let started = crate::service::ensure_started(config_path.map(|path| path.as_path())).await?;
    if started && announce {
        print_info_line("Started background service.");
    }
    Ok(started)
}

pub(crate) async fn cmd_start(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let started = crate::service::ensure_started(config_path.map(|path| path.as_path())).await?;

    if matches!(output, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "command": "start",
                "started": started,
                "running": crate::ipc_client::daemon_running().await,
            }))?
        );
        return Ok(());
    }

    print_banner("◆", "Service", "Shared background runtime");
    if started {
        print_success_line("Started shared background service.");
    } else {
        print_info_line("Shared background service is already running.");
    }
    Ok(())
}

pub(crate) async fn cmd_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    require_canonical_daemon_owner()?;
    serve_until(config_path, daemon::termination_signal()).await
}

/// Run the daemon until `stop` resolves or an operator asks it to shut down.
/// Either way it lets running calls drain, then the socket, the PID file and
/// the token are removed and HTTP shuts down gracefully before this returns.
async fn serve_until(
    config_path: Option<&std::path::PathBuf>,
    stop: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    // Claim daemon ownership before Engine::start() can read Keychain entries
    // or spawn upstream processes.
    let runtime_lock = daemon::acquire_runtime_lock()?;
    let config_path = config_path
        .cloned()
        .unwrap_or_else(plug_core::paths::default_config_path);
    let config = plug_core::config::load_config(Some(&config_path))?;
    preflight_http_bind(&config.http)?;
    let engine = plug_core::engine::Engine::new(config);

    // The IPC socket binds at once; local clients see servers arrive through
    // `list_changed`. Remote clients cannot be told the catalog grew, so HTTP
    // binds only after the whole catalog has started.
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let startup = {
        let engine = engine.clone();
        let config_path = config_path.clone();
        tokio::spawn(async move {
            engine.start().await;
            let _ = started_tx.send(());
            engine.pin_tool_blocks(&config_path).await;
        })
    };
    let http_runtime = build_configured_http_runtime(&engine.config(), &engine)?;
    let cancel = engine.cancel_token().clone();
    drop(plug_core::watcher::spawn_config_watcher(
        engine.clone(),
        config_path.clone(),
        cancel.clone(),
        engine.tracker(),
    ));
    let http_config = engine.config().http.clone();
    let http_router = http_runtime.router;
    let http = async {
        let started = tokio::select! {
            started = started_rx => started,
            () = cancel.cancelled() => return Ok(()),
        };
        if started.is_err() {
            anyhow::bail!("daemon startup ended without reporting a result");
        }
        serve_router(http_router, &http_config, cancel.clone()).await
    };
    // launchd's record of who started this process does not change while
    // it runs, and reading it runs `launchctl`.
    let ownership = tokio::task::spawn_blocking(crate::service::ipc_ownership)
        .await
        .unwrap_or(plug_core::ipc::DaemonOwnershipMode::Unknown);
    let ipc = daemon::run_daemon_with_lock(
        engine.clone(),
        config_path,
        http_runtime.state,
        runtime_lock,
        ownership,
    );
    // Whichever side ends first ends the other through the cancel token, and
    // both are awaited, so each runs its own teardown.
    let (http_result, ipc_result, ()) = tokio::join!(
        async {
            let result = http.await;
            cancel.cancel();
            result
        },
        async {
            let result = ipc.await;
            cancel.cancel();
            result
        },
        daemon::shutdown_on(stop, cancel.clone(), engine.tool_router().clone()),
    );
    // A cold start still in flight owns upstreams that shutdown has to retire,
    // so give it a bounded chance to finish before anything is torn down.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), startup).await;
    engine.shutdown().await;
    ipc_result.and(http_result)
}

#[cfg(any(target_os = "macos", test))]
fn daemon_invocation_is_canonical(
    app_installed: bool,
    app_managed: bool,
    parent_is_launchd: bool,
    development: bool,
) -> bool {
    development || !app_installed || (app_managed && parent_is_launchd)
}

#[cfg(target_os = "macos")]
fn require_canonical_daemon_owner() -> anyhow::Result<()> {
    #[cfg(test)]
    return Ok(());

    #[cfg(not(test))]
    {
        let development = plug_core::paths::is_development();
        let app_installed = crate::install::resolve_verified_app()?.is_some();
        let app_managed =
            crate::service::inspect()?.ownership == crate::service::ServiceOwnership::AppManaged;
        let parent = std::process::Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &std::process::id().to_string()])
            .output()?;
        let parent_is_launchd =
            parent.status.success() && String::from_utf8_lossy(&parent.stdout).trim() == "1";
        anyhow::ensure!(
            daemon_invocation_is_canonical(
                app_installed,
                app_managed,
                parent_is_launchd,
                development,
            ),
            "Plug.app owns daemon startup; open Plug.app to start or repair the service"
        );
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
fn require_canonical_daemon_owner() -> anyhow::Result<()> {
    Ok(())
}

pub(crate) async fn cmd_daemon_stop() -> anyhow::Result<()> {
    match crate::ipc_client::operator(plug_core::ipc::OperatorRequest::Shutdown).await? {
        plug_core::ipc::IpcResponse::Ok => {
            print_success_line("Stopped the shared background service.")
        }
        plug_core::ipc::IpcResponse::Error { code, message } => {
            anyhow::bail!("{code}: {message}");
        }
        other => anyhow::bail!("unexpected daemon response: {other:?}"),
    }
    Ok(())
}

/// The one restart instruction every message gives, worded for whoever owns
/// the daemon. With Plug.app installed its Restart button pauses connected
/// clients across the swap; `plug stop` would drop them instead.
pub(crate) fn restart_advice() -> &'static str {
    restart_advice_for(matches!(
        crate::install::resolve_verified_app(),
        Ok(Some(_))
    ))
}

fn restart_advice_for(app_installed: bool) -> &'static str {
    if app_installed {
        "Restart Plug from Plug.app (Settings → Plug → Restart)"
    } else {
        "Run `plug stop`, then `plug start`"
    }
}

fn resolve_downstream_bearer_token(
    http: &plug_core::config::HttpConfig,
) -> anyhow::Result<Option<Arc<str>>> {
    if !http.requires_bearer() {
        return Ok(None);
    }
    let token_path = plug_core::auth::http_auth_token_path(http.port);
    let token = plug_core::auth::load_or_generate_token(&token_path)?;
    tracing::info!(mode = http.auth_mode.label(), "HTTP bearer auth enabled");
    Ok(Some(Arc::<str>::from(token.as_str())))
}

fn preflight_http_bind(http: &plug_core::config::HttpConfig) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("{}:{}", http.bind_address, http.port).parse()?;
    let listener = std::net::TcpListener::bind(addr).map_err(|error| {
        anyhow::anyhow!("failed to bind downstream HTTP address {addr}: {error}")
    })?;
    drop(listener);
    Ok(())
}

async fn serve_router(
    router: Router,
    http: &plug_core::config::HttpConfig,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("{}:{}", http.bind_address, http.port).parse()?;
    let listener = std::net::TcpListener::bind(addr)
        .with_context(|| format!("failed to bind HTTP on {addr}"))?;
    tracing::info!(%addr, tls = http.tls_enabled(), "serving HTTP");
    serve_listener(listener, router, http, cancel).await
}

/// How long open HTTP connections get to finish once the daemon cancels.
pub(crate) const HTTP_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Serve `router` on a bound listener, over TLS when `http` configures it,
/// until `cancel` fires; then shut down gracefully.
async fn serve_listener(
    listener: std::net::TcpListener,
    router: Router,
    http: &plug_core::config::HttpConfig,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    listener.set_nonblocking(true)?;
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        cancel.cancelled().await;
        shutdown_handle.graceful_shutdown(Some(HTTP_SHUTDOWN_GRACE));
    });
    let service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();

    match (&http.tls_cert_path, &http.tls_key_path) {
        (Some(cert_path), Some(key_path)) => {
            plug_core::tls::ensure_rustls_provider_installed();
            let tls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path).await?;
            axum_server::from_tcp_rustls(listener, tls_config)?
                .handle(handle)
                .serve(service)
                .await?;
        }
        _ => {
            axum_server::from_tcp(listener)?
                .handle(handle)
                .serve(service)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{run_daemon, set_test_runtime_paths};
    use std::sync::Arc;

    use std::time::Duration;

    use rcgen::generate_simple_self_signed;
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, RootCertStore};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::TlsConnector;

    #[test]
    fn installed_app_rejects_foreign_daemon_spawners() {
        assert!(daemon_invocation_is_canonical(true, true, true, false));
        assert!(!daemon_invocation_is_canonical(true, true, false, false));
        assert!(!daemon_invocation_is_canonical(true, false, true, false));
        assert!(daemon_invocation_is_canonical(true, false, false, true));
        assert!(daemon_invocation_is_canonical(false, false, false, false));
    }

    #[test]
    fn restart_advice_sends_app_users_to_the_app() {
        assert!(restart_advice_for(true).contains("Plug.app"));
        assert!(!restart_advice_for(true).contains("plug stop"));
        assert!(restart_advice_for(false).contains("`plug stop`, then `plug start`"));
    }

    fn runtime_path_test_lock() -> &'static tokio::sync::Mutex<()> {
        plug_core::paths::test::lock()
    }

    async fn spawn_https_test_server(
        router: Router,
    ) -> anyhow::Result<(
        SocketAddr,
        CancellationToken,
        rustls::pki_types::CertificateDer<'static>,
    )> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;

        let cert = generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
        let cert_der = cert.cert.der().clone();
        let cert_pem = cert.cert.pem();
        let key_pem = cert.signing_key.serialize_pem();

        let temp = std::env::temp_dir().join(format!(
            "plug-https-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir_all(&temp)?;
        let cert_path = temp.join("cert.pem");
        let key_path = temp.join("key.pem");
        std::fs::write(&cert_path, &cert_pem)?;
        std::fs::write(&key_path, &key_pem)?;

        let cancel = CancellationToken::new();
        let http = plug_core::config::HttpConfig {
            bind_address: "127.0.0.1".to_string(),
            port: addr.port(),
            tls_cert_path: Some(cert_path),
            tls_key_path: Some(key_path),
            ..Default::default()
        };

        tokio::spawn({
            let cancel = cancel.clone();
            async move {
                let _ = serve_listener(listener, router, &http, cancel).await;
            }
        });

        Ok((addr, cancel, cert_der))
    }

    async fn send_https_request(
        addr: SocketAddr,
        cert_der: rustls::pki_types::CertificateDer<'static>,
        request: String,
    ) -> String {
        let mut tls = connect_https_stream(addr, cert_der).await;
        tls.write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        String::from_utf8(response).expect("utf8 response")
    }

    async fn connect_https_stream(
        addr: SocketAddr,
        cert_der: rustls::pki_types::CertificateDer<'static>,
    ) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
        plug_core::tls::ensure_rustls_provider_installed();
        let mut roots = RootCertStore::empty();
        roots.add(cert_der).expect("add test cert to roots");
        let client_config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(client_config));
        let tcp = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect to https server");
        let server_name = ServerName::try_from("localhost").expect("valid server name");
        connector
            .connect(server_name, tcp)
            .await
            .expect("complete tls handshake")
    }

    #[tokio::test]
    async fn serve_router_supports_https() {
        let engine = plug_core::engine::Engine::new(plug_core::config::Config::default());
        engine.start().await;
        let sessions = Arc::new(plug_core::session::SessionStore::new(1800, 100));
        let state = Arc::new(plug_core::http::server::HttpState::new(
            engine.tool_router().clone(),
            sessions,
            engine.cancel_token().clone(),
        ));
        let router = build_runtime_router(
            state,
            Arc::from("test-operator-token"),
            Arc::from("127.0.0.1:3282"),
        );

        let (addr, cancel, cert_der) = spawn_https_test_server(router)
            .await
            .expect("start https test server");

        let initialize_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "https-test", "version": "1.0"}
            }
        })
        .to_string();
        let initialize_request = format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            initialize_body.len(),
            initialize_body
        );
        let initialize_response =
            send_https_request(addr, cert_der.clone(), initialize_request).await;
        assert!(
            initialize_response.contains("200 OK"),
            "unexpected initialize response: {initialize_response}"
        );
        assert!(
            initialize_response
                .to_ascii_lowercase()
                .contains("mcp-session-id:"),
            "missing session id header: {initialize_response}"
        );
        assert!(
            initialize_response.contains("\"serverInfo\""),
            "missing initialize payload: {initialize_response}"
        );

        let session_header = initialize_response
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("mcp-session-id:"))
            .and_then(|line| line.split_once(':'))
            .map(|(_, value)| value.trim().to_string())
            .expect("session id header");

        let list_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })
        .to_string();
        let list_request = format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nMcp-Session-Id: {}\r\nMCP-Protocol-Version: 2025-11-25\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            session_header,
            list_body.len(),
            list_body
        );
        let list_response = send_https_request(addr, cert_der.clone(), list_request).await;
        assert!(
            list_response.contains("200 OK"),
            "unexpected tools/list response: {list_response}"
        );
        assert!(
            list_response.contains("\"tools\""),
            "missing tools payload: {list_response}"
        );

        let mut sse = connect_https_stream(addr, cert_der).await;
        let sse_request = format!(
            "GET /mcp HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\nMcp-Session-Id: {}\r\nConnection: close\r\n\r\n",
            session_header
        );
        sse.write_all(sse_request.as_bytes())
            .await
            .expect("write sse request");
        let mut buf = vec![0_u8; 1024];
        let n = tokio::time::timeout(std::time::Duration::from_secs(1), sse.read(&mut buf))
            .await
            .expect("sse read timeout")
            .expect("read sse bytes");
        let sse_response = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(
            sse_response.contains("200 OK"),
            "unexpected sse response: {sse_response}"
        );
        assert!(
            sse_response.contains("text/event-stream"),
            "missing sse content type: {sse_response}"
        );
        assert!(
            sse_response.contains("id: 0"),
            "missing sse priming event: {sse_response}"
        );

        cancel.cancel();
        engine.shutdown().await;
    }

    #[test]
    fn resolve_downstream_bearer_token_auto_loopback_disables_auth() {
        let http = plug_core::config::HttpConfig::default();
        let token = resolve_downstream_bearer_token(&http).expect("resolve token");
        assert!(token.is_none());
    }

    #[test]
    fn resolve_downstream_bearer_token_auto_loopback_no_public_base_url_disables_auth() {
        let http = plug_core::config::HttpConfig {
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            public_base_url: None,
            ..plug_core::config::HttpConfig::default()
        };
        let token = resolve_downstream_bearer_token(&http).expect("resolve token");
        assert!(token.is_none());
    }

    #[test]
    fn resolve_downstream_bearer_token_none_disables_auth() {
        let http = plug_core::config::HttpConfig {
            auth_mode: plug_core::config::DownstreamAuthMode::None,
            bind_address: "0.0.0.0".to_string(),
            ..plug_core::config::HttpConfig::default()
        };
        let token = resolve_downstream_bearer_token(&http).expect("resolve token");
        assert!(token.is_none());
    }

    #[test]
    fn resolve_downstream_bearer_token_oauth_uses_non_bearer_path() {
        let http = plug_core::config::HttpConfig {
            auth_mode: plug_core::config::DownstreamAuthMode::Oauth,
            public_base_url: Some("https://plug.example.com".to_string()),
            ..plug_core::config::HttpConfig::default()
        };
        let token = resolve_downstream_bearer_token(&http).expect("oauth should skip bearer token");
        assert!(token.is_none());
    }

    #[test]
    fn preflight_http_bind_fails_fast_when_port_is_occupied() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind occupied port");
        let port = listener.local_addr().expect("listener addr").port();

        let http = plug_core::config::HttpConfig {
            bind_address: "127.0.0.1".to_string(),
            port,
            ..plug_core::config::HttpConfig::default()
        };

        let error = preflight_http_bind(&http).expect_err("expected preflight bind failure");
        assert!(
            error
                .to_string()
                .contains("failed to bind downstream HTTP address"),
            "unexpected preflight error: {error}"
        );
    }

    #[tokio::test]
    async fn ensure_daemon_with_feedback_returns_false_when_daemon_is_already_running() {
        let _guard = runtime_path_test_lock().lock().await;
        let temp = std::env::temp_dir().join(format!(
            "pr-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        let _paths = set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = plug_core::engine::Engine::new(plug_core::config::Config::default());
        engine.start().await;

        let daemon_engine = Arc::clone(&engine);
        let daemon_config_path = config_path.clone();
        let daemon_handle =
            tokio::spawn(async move { run_daemon(daemon_engine, daemon_config_path).await });

        tokio::time::sleep(Duration::from_millis(100)).await;
        if daemon_handle.is_finished() {
            let daemon_result = daemon_handle.await.expect("daemon task join");
            panic!("daemon exited before readiness: {daemon_result:?}");
        }

        tokio::time::timeout(Duration::from_secs(5), crate::ipc_client::wait_for_daemon())
            .await
            .expect("daemon readiness wait timed out");

        let started = ensure_daemon_with_feedback(Some(&config_path), false)
            .await
            .expect("ensure_daemon_with_feedback should succeed");
        assert!(
            !started,
            "already-running daemon should not report fresh start"
        );

        engine.shutdown().await;
        let daemon_result = tokio::time::timeout(Duration::from_secs(5), daemon_handle)
            .await
            .expect("daemon join timed out")
            .expect("daemon task join");
        assert!(daemon_result.is_ok(), "daemon should shut down cleanly");
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    /// A stop signal and an operator `Shutdown` end in the same teardown: when
    /// the daemon returns, its socket, PID file and token are gone and its
    /// HTTP port no longer accepts connections.
    #[tokio::test]
    async fn every_way_of_stopping_the_daemon_cleans_up() {
        let _guard = runtime_path_test_lock().lock().await;
        for via_operator in [false, true] {
            let temp = std::env::temp_dir().join(format!(
                "pst-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ));
            let _paths = set_test_runtime_paths(temp.join("r"), temp.join("s"));
            std::fs::create_dir_all(plug_core::paths::dir()).expect("create runtime dir");
            let mut config = plug_core::config::Config::default();
            let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("reserve HTTP port");
            let port = probe.local_addr().expect("HTTP address").port();
            drop(probe);
            config.http.port = port;
            let config_path = plug_core::paths::dir().join("config.toml");
            std::fs::write(
                &config_path,
                toml::to_string(&config).expect("serialize config"),
            )
            .expect("write config");

            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
            let daemon = tokio::spawn(async move {
                serve_until(Some(&config_path), async {
                    let _ = stop_rx.await;
                })
                .await
            });
            tokio::time::timeout(Duration::from_secs(5), crate::ipc_client::wait_for_daemon())
                .await
                .expect("daemon socket never came up");
            tokio::time::timeout(Duration::from_secs(5), async {
                while tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .is_err()
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("HTTP never came up");

            if via_operator {
                let response =
                    crate::ipc_client::operator(plug_core::ipc::OperatorRequest::Shutdown)
                        .await
                        .expect("send Shutdown");
                assert!(matches!(response, plug_core::ipc::IpcResponse::Ok));
            } else {
                stop_tx.send(()).expect("daemon is listening for the stop");
            }
            tokio::time::timeout(Duration::from_secs(15), daemon)
                .await
                .expect("daemon did not stop")
                .expect("daemon task join")
                .expect("daemon stops cleanly");

            assert!(
                !plug_core::paths::socket_path().exists(),
                "socket left behind"
            );
            assert!(
                !plug_core::paths::pid_path().exists(),
                "PID file left behind"
            );
            assert!(
                !plug_core::paths::token_path().exists(),
                "token left behind"
            );
            assert!(
                tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .is_err(),
                "HTTP still accepts connections"
            );
            let _ = std::fs::remove_dir_all(&temp);
        }
    }
}
