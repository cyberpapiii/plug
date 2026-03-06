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

pub(crate) async fn connect_via_daemon(
    config_path: Option<&std::path::PathBuf>,
) -> anyhow::Result<()> {
    let stream = match daemon::connect_to_daemon().await {
        Some(stream) => stream,
        None => {
            auto_start_daemon(config_path)?;
            wait_for_daemon_ready().await?
        }
    };

    let (mut reader, mut writer) = stream.into_split();
    let register_req = plug_core::ipc::IpcRequest::Register { client_info: None };
    let payload = serde_json::to_vec(&register_req)?;
    plug_core::ipc::write_frame(&mut writer, &payload).await?;

    let frame = plug_core::ipc::read_frame(&mut reader)
        .await?
        .ok_or_else(|| anyhow::anyhow!("daemon closed"))?;

    let response: plug_core::ipc::IpcResponse = serde_json::from_slice(&frame)?;
    let session_id = match response {
        plug_core::ipc::IpcResponse::Registered { session_id } => session_id,
        _ => anyhow::bail!("registration failed"),
    };

    let proxy = crate::ipc_proxy::IpcProxyHandler::new(reader, writer, session_id);
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
    let config = plug_core::config::load_config(config_path)?;
    let engine = std::sync::Arc::new(plug_core::engine::Engine::new(config));
    engine.start().await?;
    let cancel = engine.cancel_token().clone();
    plug_core::watcher::spawn_config_watcher(engine.clone(), cancel.clone(), engine.tracker());
    tokio::select! {
        _ = daemon::run_daemon(engine.clone(), 30) => {}
        _ = daemon::shutdown_signal(cancel) => {}
    }
    engine.shutdown().await;
    Ok(())
}

pub(crate) async fn cmd_daemon_stop() -> anyhow::Result<()> {
    let auth_token = daemon::read_auth_token()?;
    let req = plug_core::ipc::IpcRequest::Shutdown { auth_token };
    if let Ok(plug_core::ipc::IpcResponse::Ok) = daemon::ipc_request(&req).await {
        println!("stopped");
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
    let http_state = Arc::new(plug_core::http::server::HttpState {
        router: engine.tool_router().clone(),
        sessions: plug_core::http::session::SessionManager::new(3600, 100),
        cancel: engine.cancel_token().clone(),
        sse_channel_capacity: 100,
    });
    let router = plug_core::http::server::build_router(http_state);
    let addr = format!("{}:{}", config.http.bind_address, config.http.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("serving on http://{addr}");
    axum::serve(listener, router).await?;
    Ok(())
}
