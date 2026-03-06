use std::path::PathBuf;
use std::sync::Arc;

use crate::OutputFormat;
use crate::daemon;
use crate::ui::{print_banner, print_info_line, print_success_line};

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
    Ok(DaemonProxySession {
        reader,
        writer,
        client_id,
        client_info,
        session_id,
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

pub(crate) async fn cmd_serve(
    config_path: Option<&std::path::PathBuf>,
    _stdio: bool,
) -> anyhow::Result<()> {
    let config = plug_core::config::load_config(config_path)?;
    let engine = Arc::new(plug_core::engine::Engine::new(config.clone()));
    engine.start().await?;
    let sessions = plug_core::http::session::SessionManager::new(
        config.http.session_timeout_secs,
        config.http.max_sessions,
    );
    sessions.spawn_cleanup_task(engine.cancel_token().clone());
    let http_state = Arc::new(plug_core::http::server::HttpState {
        router: engine.tool_router().clone(),
        sessions,
        cancel: engine.cancel_token().clone(),
        sse_channel_capacity: config.http.sse_channel_capacity,
    });
    let router = plug_core::http::server::build_router(http_state);
    let addr = format!("{}:{}", config.http.bind_address, config.http.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("serving on http://{addr}");
    axum::serve(listener, router).await?;
    Ok(())
}
