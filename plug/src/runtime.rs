use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::OutputFormat;
use crate::daemon;
use crate::ui::{print_banner, print_info_line, print_success_line};
use axum::Router;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveClientSupport {
    Supported,
    DaemonRestartRequired,
}

pub(crate) async fn fetch_live_clients() -> (Vec<plug_core::ipc::IpcClientInfo>, LiveClientSupport)
{
    match daemon::ipc_request(&plug_core::ipc::IpcRequest::ListClients).await {
        Ok(plug_core::ipc::IpcResponse::Clients { clients }) => {
            (clients, LiveClientSupport::Supported)
        }
        Ok(plug_core::ipc::IpcResponse::Error { code, .. }) if code == "PARSE_ERROR" => {
            (Vec::new(), LiveClientSupport::DaemonRestartRequired)
        }
        _ => (Vec::new(), LiveClientSupport::Supported),
    }
}

pub(crate) struct DaemonProxySession {
    pub(crate) reader: tokio::net::unix::OwnedReadHalf,
    pub(crate) writer: tokio::net::unix::OwnedWriteHalf,
    pub(crate) client_id: String,
    pub(crate) client_info: Option<String>,
    pub(crate) session_id: String,
    pub(crate) capabilities: rmcp::model::ServerCapabilities,
}

pub(crate) async fn establish_daemon_proxy_session(
    config_path: Option<&PathBuf>,
    client_id: String,
    client_info: Option<String>,
) -> anyhow::Result<DaemonProxySession> {
    let stream = match daemon::connect_to_daemon().await {
        Some(stream) => stream,
        None => {
            auto_start_daemon(config_path)?;
            wait_for_daemon_ready().await?
        }
    };

    let (mut reader, mut writer) = stream.into_split();
    let register_req = plug_core::ipc::IpcRequest::Register {
        protocol_version: plug_core::ipc::IPC_PROTOCOL_VERSION,
        client_id: client_id.clone(),
        client_info: client_info.clone(),
    };
    let payload = serde_json::to_vec(&register_req)?;
    plug_core::ipc::write_frame(&mut writer, &payload).await?;

    let frame = plug_core::ipc::read_frame(&mut reader)
        .await?
        .ok_or_else(|| anyhow::anyhow!("daemon closed during registration"))?;

    let session_id = parse_registered_session(&frame, &client_id)?;
    let capabilities_req = plug_core::ipc::IpcRequest::Capabilities {
        session_id: session_id.clone(),
    };
    let capabilities_payload = serde_json::to_vec(&capabilities_req)?;
    plug_core::ipc::write_frame(&mut writer, &capabilities_payload).await?;
    let capabilities_frame = plug_core::ipc::read_frame(&mut reader)
        .await?
        .ok_or_else(|| anyhow::anyhow!("daemon closed while fetching capabilities"))?;
    let capabilities = parse_capabilities_response(&capabilities_frame)?;
    Ok(DaemonProxySession {
        reader,
        writer,
        client_id,
        client_info,
        session_id,
        capabilities,
    })
}

fn parse_registered_session(frame: &[u8], expected_client_id: &str) -> anyhow::Result<String> {
    let value: serde_json::Value = serde_json::from_slice(frame)
        .map_err(|e| anyhow::anyhow!("invalid daemon registration response: {e}"))?;

    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("Error") => {
            let response: plug_core::ipc::IpcResponse = serde_json::from_value(value)?;
            if let plug_core::ipc::IpcResponse::Error { code, message } = response {
                anyhow::bail!("{code}: {message}");
            }
            unreachable!("validated Error response")
        }
        Some("Registered") => {
            let protocol_version = value
                .get("protocol_version")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "daemon/client protocol mismatch: restart plug connect after upgrading"
                    )
                })?;
            if protocol_version != u64::from(plug_core::ipc::IPC_PROTOCOL_VERSION) {
                anyhow::bail!(
                    "daemon/client protocol mismatch: daemon=v{protocol_version}, client=v{}",
                    plug_core::ipc::IPC_PROTOCOL_VERSION
                );
            }

            let response_client_id = value
                .get("client_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "daemon/client protocol mismatch: restart plug connect after upgrading"
                    )
                })?;
            if response_client_id != expected_client_id {
                anyhow::bail!(
                    "daemon/client registration mismatch: expected client_id {expected_client_id}, got {response_client_id}"
                );
            }

            value
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("registration failed: missing session_id"))
        }
        Some(other) => anyhow::bail!("registration failed: unexpected response type {other}"),
        None => anyhow::bail!("registration failed: malformed response"),
    }
}

fn parse_capabilities_response(frame: &[u8]) -> anyhow::Result<rmcp::model::ServerCapabilities> {
    let response: plug_core::ipc::IpcResponse = serde_json::from_slice(frame)
        .map_err(|e| anyhow::anyhow!("invalid daemon capabilities response: {e}"))?;

    match response {
        plug_core::ipc::IpcResponse::Capabilities { capabilities } => {
            serde_json::from_value(capabilities)
                .map_err(|e| anyhow::anyhow!("invalid daemon capabilities payload: {e}"))
        }
        plug_core::ipc::IpcResponse::Error { code, message } => anyhow::bail!("{code}: {message}"),
        other => anyhow::bail!("unexpected daemon capabilities response: {other:?}"),
    }
}

pub(crate) async fn connect_via_daemon(
    config_path: Option<&std::path::PathBuf>,
) -> anyhow::Result<()> {
    let client_id = uuid::Uuid::new_v4().to_string();
    let session = establish_daemon_proxy_session(config_path, client_id, None).await?;
    let proxy = crate::ipc_proxy::IpcProxyHandler::new(session, config_path.cloned());
    use rmcp::ServiceExt as _;
    let transport = rmcp::transport::io::stdio();
    let service = proxy
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let _ = service.waiting().await;
    Ok(())
}

pub(crate) async fn connect_standalone(
    config_path: Option<&std::path::PathBuf>,
) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;
    let engine = std::sync::Arc::new(plug_core::engine::Engine::new(config));
    engine.start().await?;
    let proxy = plug_core::proxy::ProxyHandler::from_router(engine.tool_router().clone());
    use rmcp::ServiceExt as _;
    let transport = rmcp::transport::io::stdio();
    let service = proxy
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let _ = service.waiting().await;
    engine.shutdown().await;
    Ok(())
}

pub(crate) fn auto_start_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve").arg("--daemon");
    if let Some(path) = config_path {
        cmd.arg("--config").arg(path);
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    cmd.spawn()?;
    Ok(())
}

pub(crate) async fn wait_for_daemon_ready() -> anyhow::Result<tokio::net::UnixStream> {
    let mut delay = std::time::Duration::from_millis(10);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(stream) = daemon::connect_to_daemon().await {
            return Ok(stream);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(std::time::Duration::from_millis(500));
    }
    anyhow::bail!("daemon failed to start")
}

pub(crate) async fn ensure_daemon_with_feedback(
    config_path: Option<&std::path::PathBuf>,
    announce: bool,
) -> anyhow::Result<bool> {
    if daemon::connect_to_daemon().await.is_none() {
        auto_start_daemon(config_path)?;
        wait_for_daemon_ready().await?;
        if announce {
            print_info_line("Started background service.");
        }
        return Ok(true);
    }
    Ok(false)
}

pub(crate) async fn cmd_connect(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    match connect_via_daemon(config_path).await {
        Ok(()) => return Ok(()),
        Err(e) => {
            tracing::error!(error = %e, "daemon proxy failed — falling back to standalone mode");
        }
    }
    connect_standalone(config_path).await
}

pub(crate) async fn cmd_start(
    config_path: Option<&std::path::PathBuf>,
    output: &OutputFormat,
) -> anyhow::Result<()> {
    let started = ensure_daemon_with_feedback(config_path, false).await?;

    if matches!(output, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "command": "start",
                "started": started,
                "running": daemon::connect_to_daemon().await.is_some(),
            }))?
        );
        return Ok(());
    }

    print_banner("◆", "Service", "Background daemon");
    if started {
        print_success_line("Started background service.");
    } else {
        print_info_line("Background service is already running.");
    }
    Ok(())
}

pub(crate) async fn cmd_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let config_path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    let config = plug_core::config::load_config(Some(&config_path))?;
    let engine = std::sync::Arc::new(plug_core::engine::Engine::new(config));
    engine.start().await?;
    let cancel = engine.cancel_token().clone();
    plug_core::watcher::spawn_config_watcher(
        engine.clone(),
        config_path.clone(),
        cancel.clone(),
        engine.tracker(),
    );
    let daemon_future = daemon::run_daemon(
        engine.clone(),
        config_path,
        engine.config().daemon_grace_period_secs,
    );
    tokio::pin!(daemon_future);
    tokio::select! {
        result = &mut daemon_future => {
            result?;
        }
        _ = daemon::shutdown_signal(cancel) => {}
    }
    engine.shutdown().await;
    Ok(())
}

pub(crate) async fn cmd_daemon_stop() -> anyhow::Result<()> {
    let auth_token = daemon::read_auth_token()?;
    let req = plug_core::ipc::IpcRequest::Shutdown { auth_token };
    match daemon::ipc_request(&req).await? {
        plug_core::ipc::IpcResponse::Ok => println!("stopped"),
        plug_core::ipc::IpcResponse::Error { code, message } => {
            anyhow::bail!("{code}: {message}");
        }
        other => anyhow::bail!("unexpected daemon response: {other:?}"),
    }
    Ok(())
}

pub(crate) async fn cmd_serve(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;
    let engine = Arc::new(plug_core::engine::Engine::new(config.clone()));
    engine.start().await?;
    let (expiry_tx, mut expiry_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let sessions: Arc<dyn plug_core::session::SessionStore> = Arc::new(
        plug_core::session::StatefulSessionStore::new(
            config.http.session_timeout_secs,
            config.http.max_sessions,
        )
        .with_expiry_notifier(expiry_tx),
    );
    sessions.spawn_cleanup_task(engine.cancel_token().clone());
    let tool_router = engine.tool_router().clone();
    // Generate or load auth token for non-loopback bind addresses
    let auth_token = if !plug_core::config::http_bind_is_loopback(&config.http.bind_address) {
        let token_path = plug_core::auth::http_auth_token_path(config.http.port);
        let token = plug_core::auth::load_or_generate_token(&token_path)?;
        tracing::info!("HTTP auth enabled (non-loopback bind address)");
        Some(Arc::<str>::from(token.as_str()))
    } else {
        None
    };

    let http_state = Arc::new(plug_core::http::server::HttpState {
        router: tool_router.clone(),
        sessions,
        cancel: engine.cancel_token().clone(),
        sse_channel_capacity: config.http.sse_channel_capacity,
        notification_task_started: std::sync::atomic::AtomicBool::new(false),
        auth_token,
        roots_capable_sessions: dashmap::DashMap::new(),
        pending_client_requests: dashmap::DashMap::new(),
        reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
    });

    // Spawn cleanup listener for expired HTTP sessions — handles
    // resource subscription cleanup, roots cache cleanup, and per-client log level removal.
    let http_state_for_expiry = Arc::clone(&http_state);
    tokio::spawn(async move {
        while let Some(session_id) = expiry_rx.recv().await {
            let target = plug_core::notifications::NotificationTarget::Http {
                session_id: Arc::from(session_id.as_str()),
            };
            tool_router.cleanup_subscriptions_for_target(&target).await;
            http_state_for_expiry
                .roots_capable_sessions
                .remove(&session_id);
            http_state_for_expiry
                .pending_client_requests
                .retain(|(pending_session_id, _), _| pending_session_id != &session_id);
            if tool_router.clear_roots_for_target(&target) {
                tool_router.forward_roots_list_changed_to_upstreams().await;
            }
            tool_router.remove_client_log_level(&session_id);
        }
    });
    let router = plug_core::http::server::build_router(http_state);
    serve_router(router, &config.http, engine.cancel_token().clone()).await?;
    Ok(())
}

async fn serve_router(
    router: Router,
    http: &plug_core::config::HttpConfig,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("{}:{}", http.bind_address, http.port).parse()?;
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        cancel.cancelled().await;
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
    });

    if let (Some(cert_path), Some(key_path)) = (&http.tls_cert_path, &http.tls_key_path) {
        plug_core::tls::ensure_rustls_provider_installed();
        let tls_config =
            axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path).await?;
        println!("serving on https://{addr}");
        axum_server::bind_rustls(addr, tls_config)
            .handle(handle)
            .serve(router.into_make_service())
            .await?;
    } else {
        println!("serving on http://{addr}");
        axum_server::bind(addr)
            .handle(handle)
            .serve(router.into_make_service())
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use rcgen::generate_simple_self_signed;
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, RootCertStore};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::TlsConnector;

    async fn spawn_https_test_server(
        router: Router,
    ) -> anyhow::Result<(
        SocketAddr,
        CancellationToken,
        rustls::pki_types::CertificateDer<'static>,
    )> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        drop(listener);

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
            session_timeout_secs: 1800,
            max_sessions: 100,
            sse_channel_capacity: 32,
        };

        tokio::spawn({
            let cancel = cancel.clone();
            async move {
                let _ = serve_router(router, &http, cancel).await;
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
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");
        let sessions: Arc<dyn plug_core::session::SessionStore> =
            Arc::new(plug_core::session::StatefulSessionStore::new(1800, 100));
        sessions.spawn_cleanup_task(engine.cancel_token().clone());
        let state = Arc::new(plug_core::http::server::HttpState {
            router: engine.tool_router().clone(),
            sessions,
            cancel: engine.cancel_token().clone(),
            sse_channel_capacity: 32,
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
        });
        let router = plug_core::http::server::build_router(state);

        let (addr, cancel, cert_der) = spawn_https_test_server(router)
            .await
            .expect("start https test server");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

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
}
