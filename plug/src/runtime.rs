use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::OutputFormat;
use crate::daemon;
use crate::ui::{print_banner, print_info_line, print_success_line};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use tokio_util::sync::CancellationToken;

const OPERATOR_LIVE_SESSIONS_PATH: &str = "/_plug/live-sessions";
const OPERATOR_TOKEN_HEADER: &str = "x-plug-operator-token";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonQueryAvailability {
    Unreachable,
    IpcUnavailable,
    Available,
}

impl DaemonQueryAvailability {
    pub(crate) fn runtime_available(self) -> bool {
        matches!(self, Self::Available)
    }

    pub(crate) fn daemon_reachable(self) -> bool {
        !matches!(self, Self::Unreachable)
    }

    pub(crate) fn status_source(self) -> &'static str {
        match self {
            Self::Available => "live_daemon",
            Self::IpcUnavailable => "ipc_unavailable",
            Self::Unreachable => "runtime_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveClientSupport {
    Supported,
    DaemonRestartRequired,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LiveInventoryAvailability {
    pub(crate) partial: bool,
    pub(crate) unavailable_sources: Vec<&'static str>,
}

#[derive(Debug, Clone, Copy, serde::Serialize, Default)]
pub(crate) struct LiveSessionTransportCounts {
    pub(crate) daemon_proxy: usize,
    pub(crate) http: usize,
    pub(crate) sse: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LiveInventoryMetadata {
    pub(crate) session_count: usize,
    pub(crate) session_transports: LiveSessionTransportCounts,
    pub(crate) scope: plug_core::ipc::LiveSessionInventoryScope,
    pub(crate) availability: LiveInventoryAvailability,
    pub(crate) http_sessions_included: bool,
}

pub(crate) fn live_inventory_availability(
    scope: plug_core::ipc::LiveSessionInventoryScope,
) -> LiveInventoryAvailability {
    match scope {
        plug_core::ipc::LiveSessionInventoryScope::TransportComplete => LiveInventoryAvailability {
            partial: false,
            unavailable_sources: Vec::new(),
        },
        plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly => LiveInventoryAvailability {
            partial: true,
            unavailable_sources: vec!["http"],
        },
        plug_core::ipc::LiveSessionInventoryScope::HttpOnly => LiveInventoryAvailability {
            partial: true,
            unavailable_sources: vec!["daemon_proxy"],
        },
        plug_core::ipc::LiveSessionInventoryScope::Unavailable => LiveInventoryAvailability {
            partial: true,
            unavailable_sources: vec!["daemon_proxy", "http"],
        },
    }
}

pub(crate) fn live_session_transport_counts(
    sessions: &[plug_core::ipc::IpcLiveSessionInfo],
) -> LiveSessionTransportCounts {
    let mut counts = LiveSessionTransportCounts::default();
    for session in sessions {
        match session.transport {
            plug_core::ipc::LiveSessionTransport::DaemonProxy => counts.daemon_proxy += 1,
            plug_core::ipc::LiveSessionTransport::Http => counts.http += 1,
            plug_core::ipc::LiveSessionTransport::Sse => counts.sse += 1,
        }
    }
    counts
}

pub(crate) fn live_inventory_metadata(
    sessions: &[plug_core::ipc::IpcLiveSessionInfo],
    scope: plug_core::ipc::LiveSessionInventoryScope,
) -> LiveInventoryMetadata {
    LiveInventoryMetadata {
        session_count: sessions.len(),
        session_transports: live_session_transport_counts(sessions),
        availability: live_inventory_availability(scope),
        http_sessions_included: matches!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete
                | plug_core::ipc::LiveSessionInventoryScope::HttpOnly
        ),
        scope,
    }
}

enum LiveSessionSourceState {
    Available(Vec<plug_core::ipc::IpcLiveSessionInfo>),
    Unavailable,
}

impl LiveSessionSourceState {
    fn available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    fn into_sessions(self) -> Vec<plug_core::ipc::IpcLiveSessionInfo> {
        match self {
            Self::Available(sessions) => sessions,
            Self::Unavailable => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct OperatorLiveSessionsResponse {
    sessions: Vec<plug_core::ipc::IpcLiveSessionInfo>,
}

#[derive(Clone)]
struct OperatorHttpState {
    http_state: Arc<plug_core::http::server::HttpState>,
    operator_token: Arc<str>,
}

async fn operator_live_sessions(
    State(state): State<Arc<OperatorHttpState>>,
    headers: HeaderMap,
) -> Result<Json<OperatorLiveSessionsResponse>, StatusCode> {
    let provided = headers
        .get(OPERATOR_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !plug_core::auth::verify_auth_token(provided, &state.operator_token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let sessions = state
        .http_state
        .sessions
        .session_snapshots()
        .into_iter()
        .map(|snapshot| plug_core::ipc::IpcLiveSessionInfo {
            transport: match snapshot.transport {
                plug_core::session::DownstreamTransport::Http => {
                    plug_core::ipc::LiveSessionTransport::Http
                }
                plug_core::session::DownstreamTransport::Sse => {
                    plug_core::ipc::LiveSessionTransport::Sse
                }
            },
            client_id: None,
            session_id: snapshot.session_id,
            client_type: snapshot.client_type,
            client_info: None,
            connected_secs: snapshot.connected_seconds,
            last_activity_secs: Some(snapshot.idle_seconds),
        })
        .collect();

    Ok(Json(OperatorLiveSessionsResponse { sessions }))
}

fn build_runtime_router(
    http_state: Arc<plug_core::http::server::HttpState>,
    operator_token: Arc<str>,
) -> Router {
    let operator_state = Arc::new(OperatorHttpState {
        http_state: http_state.clone(),
        operator_token,
    });
    let operator_router = Router::new()
        .route(OPERATOR_LIVE_SESSIONS_PATH, get(operator_live_sessions))
        .with_state(operator_state);

    plug_core::http::server::build_router(http_state).merge(operator_router)
}

struct ConfiguredHttpRuntime {
    router: Router,
    sessions: Arc<dyn plug_core::session::SessionStore>,
}

fn build_configured_http_runtime(
    config: &plug_core::config::Config,
    engine: &Arc<plug_core::engine::Engine>,
) -> anyhow::Result<ConfiguredHttpRuntime> {
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
    let auth_token = resolve_downstream_bearer_token(&config.http)?;
    let operator_token = Arc::<str>::from(plug_core::auth::load_or_generate_token(
        &plug_core::auth::http_operator_token_path(config.http.port),
    )?);
    let downstream_oauth =
        plug_core::downstream_oauth::DownstreamOauthConfig::from_http_config(&config.http)
            .map(plug_core::downstream_oauth::DownstreamOauthManager::new);

    let http_state = Arc::new(plug_core::http::server::HttpState {
        router: tool_router.clone(),
        sessions: Arc::clone(&sessions),
        cancel: engine.cancel_token().clone(),
        auth_mode: config.http.auth_mode.clone(),
        downstream_oauth,
        sse_channel_capacity: config.http.sse_channel_capacity,
        allowed_origins: config
            .http
            .allowed_origins
            .iter()
            .cloned()
            .map(Arc::<str>::from)
            .collect(),
        notification_task_started: std::sync::atomic::AtomicBool::new(false),
        auth_token,
        roots_capable_sessions: dashmap::DashMap::new(),
        pending_client_requests: dashmap::DashMap::new(),
        reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
        client_capabilities: dashmap::DashMap::new(),
    });

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
                .client_capabilities
                .remove(&session_id);
            http_state_for_expiry
                .pending_client_requests
                .retain(|(pending_session_id, _), _| pending_session_id != &session_id);
            if tool_router.clear_roots_for_target(&target) {
                tool_router.forward_roots_list_changed_to_upstreams().await;
            }
            tool_router.remove_client_log_level(&session_id);
            let lazy_session_key = plug_core::proxy::ToolRouter::lazy_session_key(
                plug_core::proxy::DownstreamTransport::Http,
                &session_id,
            );
            tool_router.clear_lazy_session(&lazy_session_key);
            tool_router.unregister_downstream_bridge(&target);
        }
    });

    Ok(ConfiguredHttpRuntime {
        router: build_runtime_router(http_state, operator_token),
        sessions,
    })
}

fn local_http_inventory_url(http: &plug_core::config::HttpConfig) -> String {
    let scheme = if http.tls_cert_path.is_some() && http.tls_key_path.is_some() {
        "https"
    } else {
        "http"
    };
    let host = match http.bind_address.as_str() {
        "0.0.0.0" | "::" | "[::]" => "localhost",
        bind if plug_core::config::http_bind_is_loopback(bind) => "localhost",
        bind => bind,
    };
    format!(
        "{scheme}://{host}:{}{}",
        http.port, OPERATOR_LIVE_SESSIONS_PATH
    )
}

async fn fetch_http_live_sessions(config_path: Option<&PathBuf>) -> LiveSessionSourceState {
    let config = match plug_core::config::load_config(config_path) {
        Ok(config) => config,
        Err(_) => return LiveSessionSourceState::Unavailable,
    };
    let token_path = plug_core::auth::http_operator_token_path(config.http.port);
    fetch_http_live_sessions_from(local_http_inventory_url(&config.http), &token_path).await
}

async fn fetch_http_live_sessions_from(
    url: String,
    token_path: &std::path::Path,
) -> LiveSessionSourceState {
    let token = match std::fs::read_to_string(token_path) {
        Ok(token) => token,
        Err(_) => return LiveSessionSourceState::Unavailable,
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        return LiveSessionSourceState::Unavailable;
    }

    plug_core::tls::ensure_rustls_provider_installed();
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(2))
        .build();
    let client = match client {
        Ok(client) => client,
        Err(_) => return LiveSessionSourceState::Unavailable,
    };
    let response = client
        .get(url)
        .header(OPERATOR_TOKEN_HEADER, token)
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(_) => return LiveSessionSourceState::Unavailable,
    };
    if !response.status().is_success() {
        return LiveSessionSourceState::Unavailable;
    }
    match response.json::<OperatorLiveSessionsResponse>().await {
        Ok(body) => LiveSessionSourceState::Available(body.sessions),
        Err(_) => LiveSessionSourceState::Unavailable,
    }
}

fn merge_live_session_sources(
    daemon_scope: plug_core::ipc::LiveSessionInventoryScope,
    daemon_source: LiveSessionSourceState,
    http_source: LiveSessionSourceState,
) -> (
    Vec<plug_core::ipc::IpcLiveSessionInfo>,
    plug_core::ipc::LiveSessionInventoryScope,
) {
    let daemon_available = daemon_source.available();
    let http_available = http_source.available();
    let mut daemon_sessions = daemon_source.into_sessions();
    let mut http_sessions = http_source.into_sessions();
    let scope = match (daemon_available, http_available) {
        (true, true) => plug_core::ipc::LiveSessionInventoryScope::TransportComplete,
        (true, false) => daemon_scope,
        (false, true) => plug_core::ipc::LiveSessionInventoryScope::HttpOnly,
        (false, false) => plug_core::ipc::LiveSessionInventoryScope::Unavailable,
    };

    daemon_sessions.append(&mut http_sessions);
    daemon_sessions.sort_by(|a, b| {
        let transport_order = |transport: plug_core::ipc::LiveSessionTransport| match transport {
            plug_core::ipc::LiveSessionTransport::DaemonProxy => 0,
            plug_core::ipc::LiveSessionTransport::Http => 1,
            plug_core::ipc::LiveSessionTransport::Sse => 2,
        };
        transport_order(a.transport)
            .cmp(&transport_order(b.transport))
            .then(a.client_type.to_string().cmp(&b.client_type.to_string()))
            .then(a.session_id.cmp(&b.session_id))
    });

    (daemon_sessions, scope)
}

pub(crate) async fn fetch_live_sessions(
    config_path: Option<&PathBuf>,
) -> (
    Vec<plug_core::ipc::IpcLiveSessionInfo>,
    plug_core::ipc::LiveSessionInventoryScope,
    LiveClientSupport,
) {
    let (daemon_source, daemon_scope, support) =
        match daemon::ipc_request(&plug_core::ipc::IpcRequest::ListLiveSessions).await {
            Ok(plug_core::ipc::IpcResponse::LiveSessions { sessions, scope }) => (
                LiveSessionSourceState::Available(sessions),
                scope,
                LiveClientSupport::Supported,
            ),
            Ok(plug_core::ipc::IpcResponse::Clients { clients }) => {
                let sessions = clients
                    .into_iter()
                    .map(|client| plug_core::ipc::IpcLiveSessionInfo {
                        transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
                        client_id: Some(client.client_id),
                        session_id: client.session_id,
                        client_type: client
                            .client_info
                            .as_deref()
                            .map(plug_core::client_detect::detect_client)
                            .unwrap_or(plug_core::types::ClientType::Unknown),
                        client_info: client.client_info,
                        connected_secs: client.connected_secs,
                        last_activity_secs: None,
                    })
                    .collect();
                (
                    LiveSessionSourceState::Available(sessions),
                    plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly,
                    LiveClientSupport::Supported,
                )
            }
            Ok(plug_core::ipc::IpcResponse::Error { code, .. }) if code == "PARSE_ERROR" => (
                LiveSessionSourceState::Unavailable,
                plug_core::ipc::LiveSessionInventoryScope::Unavailable,
                LiveClientSupport::DaemonRestartRequired,
            ),
            _ => (
                LiveSessionSourceState::Unavailable,
                plug_core::ipc::LiveSessionInventoryScope::Unavailable,
                LiveClientSupport::Supported,
            ),
        };

    if matches!(
        daemon_scope,
        plug_core::ipc::LiveSessionInventoryScope::TransportComplete
    ) {
        let sessions = daemon_source.into_sessions();
        return (sessions, daemon_scope, support);
    }

    let http_source = fetch_http_live_sessions(config_path).await;

    let (sessions, scope) = merge_live_session_sources(daemon_scope, daemon_source, http_source);

    (sessions, scope, support)
}

pub(crate) struct DaemonProxySession {
    pub(crate) reader: tokio::net::unix::OwnedReadHalf,
    pub(crate) writer: tokio::net::unix::OwnedWriteHalf,
    pub(crate) client_id: String,
    pub(crate) client_info: Option<String>,
    pub(crate) session_id: String,
    pub(crate) capabilities: rmcp::model::ServerCapabilities,
    pub(crate) pending_notifications: Vec<plug_core::ipc::IpcResponse>,
}

enum PendingIpcResponse {
    Registered(String),
    Capabilities(rmcp::model::ServerCapabilities),
}

async fn read_pending_or_matching_response(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    expected_client_id: &str,
    pending_notifications: &mut Vec<plug_core::ipc::IpcResponse>,
    matcher: impl Fn(&plug_core::ipc::IpcResponse) -> Option<PendingIpcResponse>,
) -> anyhow::Result<PendingIpcResponse> {
    loop {
        let frame = plug_core::ipc::read_frame(reader).await?.ok_or_else(|| {
            anyhow::anyhow!("daemon closed connection while waiting for response")
        })?;
        let response: plug_core::ipc::IpcResponse = serde_json::from_slice(&frame)
            .map_err(|e| anyhow::anyhow!("invalid daemon response: {e}"))?;

        if let Some(matched) = matcher(&response) {
            return Ok(matched);
        }

        match response {
            plug_core::ipc::IpcResponse::Error { code, message } => {
                anyhow::bail!("{code}: {message}");
            }
            plug_core::ipc::IpcResponse::Registered {
                protocol_version,
                client_id,
                session_id,
            } => {
                if protocol_version != plug_core::ipc::IPC_PROTOCOL_VERSION {
                    anyhow::bail!(
                        "daemon/client protocol mismatch: daemon=v{protocol_version}, client=v{}",
                        plug_core::ipc::IPC_PROTOCOL_VERSION
                    );
                }
                if client_id != expected_client_id {
                    anyhow::bail!(
                        "daemon/client registration mismatch: expected client_id {expected_client_id}, got {client_id}"
                    );
                }
                return Ok(PendingIpcResponse::Registered(session_id));
            }
            resp @ (plug_core::ipc::IpcResponse::LoggingNotification { .. }
            | plug_core::ipc::IpcResponse::ToolListChangedNotification
            | plug_core::ipc::IpcResponse::ResourceListChangedNotification
            | plug_core::ipc::IpcResponse::PromptListChangedNotification
            | plug_core::ipc::IpcResponse::ProgressNotification { .. }
            | plug_core::ipc::IpcResponse::CancelledNotification { .. }
            | plug_core::ipc::IpcResponse::AuthStateChanged { .. }) => {
                pending_notifications.push(resp);
            }
            other => {
                anyhow::bail!("unexpected daemon response while waiting for IPC setup: {other:?}");
            }
        }
    }
}

pub(crate) async fn establish_daemon_proxy_session(
    config_path: Option<&PathBuf>,
    client_id: String,
    client_info: Option<String>,
) -> anyhow::Result<DaemonProxySession> {
    let stream = match daemon::connect_to_daemon().await {
        Some(stream) => stream,
        None => {
            let mut child = auto_start_daemon(config_path)?;
            wait_for_daemon_ready(Some(&mut child)).await?
        }
    };

    let (mut reader, mut writer) = stream.into_split();
    let mut pending_notifications = Vec::new();
    let register_req = plug_core::ipc::IpcRequest::Register {
        protocol_version: plug_core::ipc::IPC_PROTOCOL_VERSION,
        client_id: client_id.clone(),
        client_info: client_info.clone(),
    };
    let payload = serde_json::to_vec(&register_req)?;
    plug_core::ipc::write_frame(&mut writer, &payload).await?;
    let session_id = match read_pending_or_matching_response(
        &mut reader,
        &client_id,
        &mut pending_notifications,
        |response| match response {
            plug_core::ipc::IpcResponse::Registered { session_id, .. } => {
                Some(PendingIpcResponse::Registered(session_id.clone()))
            }
            _ => None,
        },
    )
    .await?
    {
        PendingIpcResponse::Registered(session_id) => session_id,
        PendingIpcResponse::Capabilities(_) => unreachable!("registration response expected"),
    };
    let capabilities_req = plug_core::ipc::IpcRequest::Capabilities {
        session_id: session_id.clone(),
    };
    let capabilities_payload = serde_json::to_vec(&capabilities_req)?;
    plug_core::ipc::write_frame(&mut writer, &capabilities_payload).await?;
    let capabilities = match read_pending_or_matching_response(
        &mut reader,
        &client_id,
        &mut pending_notifications,
        |response| match response {
            plug_core::ipc::IpcResponse::Capabilities { capabilities } => {
                serde_json::from_value(capabilities.clone())
                    .ok()
                    .map(PendingIpcResponse::Capabilities)
            }
            _ => None,
        },
    )
    .await?
    {
        PendingIpcResponse::Capabilities(capabilities) => capabilities,
        PendingIpcResponse::Registered(_) => unreachable!("capabilities response expected"),
    };
    Ok(DaemonProxySession {
        reader,
        writer,
        client_id,
        client_info,
        session_id,
        capabilities,
        pending_notifications,
    })
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

pub(crate) fn auto_start_daemon(
    config_path: Option<&std::path::PathBuf>,
) -> anyhow::Result<std::process::Child> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve").arg("--daemon");
    if let Some(path) = config_path {
        cmd.arg("--config").arg(path);
    }
    for (key, value) in
        plug_core::dotenv::read_dotenv_vars_for_config(config_path.map(|path| path.as_path()))
    {
        cmd.env(key, value);
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    Ok(cmd.spawn()?)
}

pub(crate) async fn wait_for_daemon_ready(
    mut child: Option<&mut std::process::Child>,
) -> anyhow::Result<tokio::net::UnixStream> {
    let mut delay = std::time::Duration::from_millis(10);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(stream) = daemon::connect_to_daemon().await {
            return Ok(stream);
        }
        if let Some(child) = child.as_mut() {
            if let Some(status) = child.try_wait()? {
                anyhow::bail!("daemon exited before becoming ready (status: {status})");
            }
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
        let mut child = auto_start_daemon(config_path)?;
        wait_for_daemon_ready(Some(&mut child)).await?;
        if announce {
            print_info_line("Started background service.");
        }
        return Ok(true);
    }
    Ok(false)
}

pub(crate) async fn daemon_running() -> bool {
    daemon::connect_to_daemon().await.is_some()
}

pub(crate) async fn daemon_query<T>(
    request: &plug_core::ipc::IpcRequest,
    decode: impl FnOnce(plug_core::ipc::IpcResponse) -> Option<T>,
) -> (DaemonQueryAvailability, Option<T>) {
    if !daemon_running().await {
        return (DaemonQueryAvailability::Unreachable, None);
    }

    match daemon::ipc_request(request).await {
        Ok(response) => match decode(response) {
            Some(value) => (DaemonQueryAvailability::Available, Some(value)),
            None => (DaemonQueryAvailability::IpcUnavailable, None),
        },
        Err(_) => (DaemonQueryAvailability::IpcUnavailable, None),
    }
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

    print_banner("◆", "Service", "Shared background runtime");
    if started {
        print_success_line("Started shared background service.");
    } else {
        print_info_line("Shared background service is already running.");
    }
    Ok(())
}

pub(crate) async fn cmd_daemon(config_path: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
    let config_path = config_path
        .cloned()
        .unwrap_or_else(plug_core::config::default_config_path);
    let config = plug_core::config::load_config(Some(&config_path))?;
    preflight_http_bind(&config.http)?;
    let engine = std::sync::Arc::new(plug_core::engine::Engine::new(config));
    engine.start().await?;
    let http_runtime = build_configured_http_runtime(&engine.config(), &engine)?;
    let cancel = engine.cancel_token().clone();
    plug_core::watcher::spawn_config_watcher(
        engine.clone(),
        config_path.clone(),
        cancel.clone(),
        engine.tracker(),
    );
    let http_config = engine.config().http.clone();
    let http_future = serve_router(
        http_runtime.router,
        &http_config,
        engine.cancel_token().clone(),
    );
    tokio::pin!(http_future);
    let daemon_future = daemon::run_daemon(
        engine.clone(),
        config_path,
        engine.config().daemon_grace_period_secs,
        Some(http_runtime.sessions),
    );
    tokio::pin!(daemon_future);
    tokio::select! {
        result = &mut http_future => {
            result?;
        }
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
    cmd_daemon(config_path).await
}

fn resolve_downstream_bearer_token(
    http: &plug_core::config::HttpConfig,
) -> anyhow::Result<Option<Arc<str>>> {
    match http.auth_mode {
        plug_core::config::DownstreamAuthMode::Auto => {
            if !plug_core::config::http_bind_is_loopback(&http.bind_address) {
                let token_path = plug_core::auth::http_auth_token_path(http.port);
                let token = plug_core::auth::load_or_generate_token(&token_path)?;
                tracing::info!("HTTP auth enabled (auto mode on non-loopback bind address)");
                Ok(Some(Arc::<str>::from(token.as_str())))
            } else {
                Ok(None)
            }
        }
        plug_core::config::DownstreamAuthMode::None => Ok(None),
        plug_core::config::DownstreamAuthMode::Bearer => {
            let token_path = plug_core::auth::http_auth_token_path(http.port);
            let token = plug_core::auth::load_or_generate_token(&token_path)?;
            tracing::info!("HTTP bearer auth enabled");
            Ok(Some(Arc::<str>::from(token.as_str())))
        }
        plug_core::config::DownstreamAuthMode::Oauth => Ok(None),
    }
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
    use crate::daemon::{clear_test_runtime_paths, run_daemon, set_test_runtime_paths};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use plug_core::session::SessionStore;
    use rcgen::generate_simple_self_signed;
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, RootCertStore};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::TlsConnector;
    use tower::util::ServiceExt;

    fn unique_temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "plug-runtime-{}-{}-{}",
            label,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn runtime_path_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    async fn spawn_http_test_server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });
        (
            format!("http://{}{}", addr, OPERATOR_LIVE_SESSIONS_PATH),
            handle,
        )
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
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            public_base_url: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            oauth_scopes: None,
            bind_address: "127.0.0.1".to_string(),
            port: addr.port(),
            allowed_origins: Vec::new(),
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
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
            client_capabilities: dashmap::DashMap::new(),
        });
        let router = build_runtime_router(state, Arc::from("test-operator-token"));

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

    #[test]
    fn resolve_downstream_bearer_token_auto_loopback_disables_auth() {
        let http = plug_core::config::HttpConfig::default();
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

    #[test]
    fn merge_live_session_sources_marks_transport_complete_when_both_sources_exist() {
        let daemon = vec![plug_core::ipc::IpcLiveSessionInfo {
            transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
            client_id: Some("daemon".to_string()),
            session_id: "daemon-1".to_string(),
            client_type: plug_core::types::ClientType::ClaudeCode,
            client_info: Some("Claude Code".to_string()),
            connected_secs: 10,
            last_activity_secs: None,
        }];
        let http = vec![plug_core::ipc::IpcLiveSessionInfo {
            transport: plug_core::ipc::LiveSessionTransport::Http,
            client_id: None,
            session_id: "http-1".to_string(),
            client_type: plug_core::types::ClientType::ClaudeDesktop,
            client_info: None,
            connected_secs: 5,
            last_activity_secs: Some(1),
        }];

        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly,
            LiveSessionSourceState::Available(daemon),
            LiveSessionSourceState::Available(http),
        );

        assert_eq!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete
        );
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn merge_live_session_sources_marks_transport_complete_when_http_source_is_idle() {
        let daemon = vec![plug_core::ipc::IpcLiveSessionInfo {
            transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
            client_id: Some("daemon".to_string()),
            session_id: "daemon-1".to_string(),
            client_type: plug_core::types::ClientType::ClaudeCode,
            client_info: Some("Claude Code".to_string()),
            connected_secs: 10,
            last_activity_secs: None,
        }];

        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly,
            LiveSessionSourceState::Available(daemon),
            LiveSessionSourceState::Available(Vec::new()),
        );

        assert_eq!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete
        );
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn merge_live_session_sources_preserves_daemon_proxy_only_scope_when_http_unavailable() {
        let daemon = vec![plug_core::ipc::IpcLiveSessionInfo {
            transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
            client_id: Some("daemon".to_string()),
            session_id: "daemon-1".to_string(),
            client_type: plug_core::types::ClientType::ClaudeCode,
            client_info: Some("Claude Code".to_string()),
            connected_secs: 10,
            last_activity_secs: None,
        }];

        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly,
            LiveSessionSourceState::Available(daemon),
            LiveSessionSourceState::Unavailable,
        );

        assert_eq!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly
        );
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn merge_live_session_sources_marks_http_only_without_daemon() {
        let http = vec![plug_core::ipc::IpcLiveSessionInfo {
            transport: plug_core::ipc::LiveSessionTransport::Http,
            client_id: None,
            session_id: "http-1".to_string(),
            client_type: plug_core::types::ClientType::ClaudeDesktop,
            client_info: None,
            connected_secs: 5,
            last_activity_secs: Some(1),
        }];

        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::Unavailable,
            LiveSessionSourceState::Unavailable,
            LiveSessionSourceState::Available(http),
        );

        assert_eq!(scope, plug_core::ipc::LiveSessionInventoryScope::HttpOnly);
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn merge_live_session_sources_marks_http_only_when_http_source_is_idle() {
        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::Unavailable,
            LiveSessionSourceState::Unavailable,
            LiveSessionSourceState::Available(Vec::new()),
        );

        assert_eq!(scope, plug_core::ipc::LiveSessionInventoryScope::HttpOnly);
        assert!(sessions.is_empty());
    }

    #[test]
    fn merge_live_session_sources_marks_unavailable_when_no_sources_exist() {
        let (sessions, scope) = merge_live_session_sources(
            plug_core::ipc::LiveSessionInventoryScope::Unavailable,
            LiveSessionSourceState::Unavailable,
            LiveSessionSourceState::Unavailable,
        );

        assert_eq!(
            scope,
            plug_core::ipc::LiveSessionInventoryScope::Unavailable
        );
        assert!(sessions.is_empty());
    }

    #[test]
    fn live_inventory_availability_marks_missing_sources() {
        let complete = live_inventory_availability(
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete,
        );
        assert!(!complete.partial);
        assert!(complete.unavailable_sources.is_empty());

        let daemon_only =
            live_inventory_availability(plug_core::ipc::LiveSessionInventoryScope::DaemonProxyOnly);
        assert!(daemon_only.partial);
        assert_eq!(daemon_only.unavailable_sources, vec!["http"]);

        let http_only =
            live_inventory_availability(plug_core::ipc::LiveSessionInventoryScope::HttpOnly);
        assert!(http_only.partial);
        assert_eq!(http_only.unavailable_sources, vec!["daemon_proxy"]);

        let unavailable =
            live_inventory_availability(plug_core::ipc::LiveSessionInventoryScope::Unavailable);
        assert!(unavailable.partial);
        assert_eq!(
            unavailable.unavailable_sources,
            vec!["daemon_proxy", "http"]
        );
    }

    #[test]
    fn live_inventory_metadata_reports_counts_and_availability() {
        let sessions = vec![
            plug_core::ipc::IpcLiveSessionInfo {
                transport: plug_core::ipc::LiveSessionTransport::DaemonProxy,
                client_id: Some("daemon".to_string()),
                session_id: "daemon-1".to_string(),
                client_type: plug_core::types::ClientType::ClaudeCode,
                client_info: Some("Claude Code".to_string()),
                connected_secs: 10,
                last_activity_secs: None,
            },
            plug_core::ipc::IpcLiveSessionInfo {
                transport: plug_core::ipc::LiveSessionTransport::Http,
                client_id: None,
                session_id: "http-1".to_string(),
                client_type: plug_core::types::ClientType::ClaudeDesktop,
                client_info: None,
                connected_secs: 5,
                last_activity_secs: Some(1),
            },
        ];

        let metadata = live_inventory_metadata(
            &sessions,
            plug_core::ipc::LiveSessionInventoryScope::TransportComplete,
        );

        assert_eq!(metadata.session_count, 2);
        assert_eq!(metadata.session_transports.daemon_proxy, 1);
        assert_eq!(metadata.session_transports.http, 1);
        assert_eq!(metadata.session_transports.sse, 0);
        assert!(!metadata.availability.partial);
        assert!(metadata.http_sessions_included);
    }

    #[tokio::test]
    async fn wait_for_daemon_ready_fails_fast_when_spawned_process_exits() {
        let _guard = runtime_path_test_lock().lock().await;
        let runtime_root = unique_temp_dir("daemon-ready-runtime");
        let state_root = unique_temp_dir("daemon-ready-state");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn short-lived child");

        let error = wait_for_daemon_ready(Some(&mut child))
            .await
            .expect_err("expected readiness wait to fail");
        assert!(
            error
                .to_string()
                .contains("daemon exited before becoming ready"),
            "unexpected readiness failure: {error}"
        );

        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    #[tokio::test]
    async fn wait_for_daemon_ready_succeeds_when_daemon_is_running() {
        let _guard = runtime_path_test_lock().lock().await;
        let temp = std::env::temp_dir().join(format!(
            "pr-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");

        let daemon_engine = Arc::clone(&engine);
        let daemon_handle =
            tokio::spawn(async move { run_daemon(daemon_engine, config_path, 0, None).await });

        tokio::time::sleep(Duration::from_millis(100)).await;
        if daemon_handle.is_finished() {
            let daemon_result = daemon_handle.await.expect("daemon task join");
            panic!("daemon exited before readiness: {daemon_result:?}");
        }

        let stream = tokio::time::timeout(Duration::from_secs(5), wait_for_daemon_ready(None))
            .await
            .expect("daemon readiness wait timed out")
            .expect("daemon should become ready");
        drop(stream);

        engine.shutdown().await;
        let daemon_result = tokio::time::timeout(Duration::from_secs(5), daemon_handle)
            .await
            .expect("daemon join timed out")
            .expect("daemon task join");
        assert!(daemon_result.is_ok(), "daemon should shut down cleanly");

        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
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
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = runtime_root.join("config.toml");
        std::fs::write(&config_path, "").expect("write config");
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");

        let daemon_engine = Arc::clone(&engine);
        let daemon_config_path = config_path.clone();
        let daemon_handle =
            tokio::spawn(
                async move { run_daemon(daemon_engine, daemon_config_path, 0, None).await },
            );

        tokio::time::sleep(Duration::from_millis(100)).await;
        if daemon_handle.is_finished() {
            let daemon_result = daemon_handle.await.expect("daemon task join");
            panic!("daemon exited before readiness: {daemon_result:?}");
        }

        tokio::time::timeout(Duration::from_secs(5), wait_for_daemon_ready(None))
            .await
            .expect("daemon readiness wait timed out")
            .expect("daemon should become ready");

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

        clear_test_runtime_paths();
        std::fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
        std::fs::remove_dir_all(state_root).expect("cleanup state root");
    }

    #[tokio::test]
    async fn operator_live_sessions_requires_token() {
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");
        let sessions: Arc<dyn plug_core::session::SessionStore> =
            Arc::new(plug_core::session::StatefulSessionStore::new(1800, 100));
        let state = Arc::new(plug_core::http::server::HttpState {
            router: engine.tool_router().clone(),
            sessions,
            cancel: engine.cancel_token().clone(),
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
            client_capabilities: dashmap::DashMap::new(),
        });

        let app = build_runtime_router(state, Arc::from("expected-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .uri(OPERATOR_LIVE_SESSIONS_PATH)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn operator_live_sessions_returns_http_snapshot_inventory() {
        let engine = Arc::new(plug_core::engine::Engine::new(
            plug_core::config::Config::default(),
        ));
        engine.start().await.expect("engine start");
        let store = plug_core::session::StatefulSessionStore::new(1800, 100);
        let session_id = store.create_session().expect("session");
        store
            .set_client_type(&session_id, plug_core::types::ClientType::ClaudeDesktop)
            .expect("set client type");
        let sessions: Arc<dyn plug_core::session::SessionStore> = Arc::new(store);
        let state = Arc::new(plug_core::http::server::HttpState {
            router: engine.tool_router().clone(),
            sessions,
            cancel: engine.cancel_token().clone(),
            auth_mode: plug_core::config::DownstreamAuthMode::Auto,
            downstream_oauth: None,
            sse_channel_capacity: 32,
            allowed_origins: Vec::new(),
            notification_task_started: AtomicBool::new(false),
            auth_token: None,
            roots_capable_sessions: dashmap::DashMap::new(),
            pending_client_requests: dashmap::DashMap::new(),
            reverse_request_counter: std::sync::atomic::AtomicU64::new(1),
            client_capabilities: dashmap::DashMap::new(),
        });

        let app = build_runtime_router(state, Arc::from("expected-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .uri(OPERATOR_LIVE_SESSIONS_PATH)
                    .header(OPERATOR_TOKEN_HEADER, "expected-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let parsed: OperatorLiveSessionsResponse =
            serde_json::from_slice(&body).expect("json body");
        assert_eq!(parsed.sessions.len(), 1);
        assert_eq!(
            parsed.sessions[0].transport,
            plug_core::ipc::LiveSessionTransport::Http
        );
        assert_eq!(
            parsed.sessions[0].client_type,
            plug_core::types::ClientType::ClaudeDesktop
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn fetch_http_live_sessions_from_returns_unavailable_when_token_missing() {
        let dir = unique_temp_dir("missing-token");
        let token_path = dir.join("operator.token");

        let state =
            fetch_http_live_sessions_from("http://127.0.0.1:9/nowhere".to_string(), &token_path)
                .await;

        assert!(matches!(state, LiveSessionSourceState::Unavailable));
        std::fs::remove_dir_all(dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn fetch_http_live_sessions_from_returns_unavailable_when_token_empty() {
        let dir = unique_temp_dir("empty-token");
        let token_path = dir.join("operator.token");
        std::fs::write(&token_path, "\n").expect("write empty token");

        let state =
            fetch_http_live_sessions_from("http://127.0.0.1:9/nowhere".to_string(), &token_path)
                .await;

        assert!(matches!(state, LiveSessionSourceState::Unavailable));
        std::fs::remove_dir_all(dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn fetch_http_live_sessions_from_returns_unavailable_on_unauthorized() {
        let dir = unique_temp_dir("unauthorized");
        let token_path = dir.join("operator.token");
        std::fs::write(&token_path, "expected-token").expect("write token");
        let app = Router::new().route(
            OPERATOR_LIVE_SESSIONS_PATH,
            get(|| async { StatusCode::UNAUTHORIZED }),
        );
        let (url, handle) = spawn_http_test_server(app).await;

        let state = fetch_http_live_sessions_from(url, &token_path).await;

        handle.abort();
        assert!(matches!(state, LiveSessionSourceState::Unavailable));
        std::fs::remove_dir_all(dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn fetch_http_live_sessions_from_returns_unavailable_on_invalid_json() {
        let dir = unique_temp_dir("invalid-json");
        let token_path = dir.join("operator.token");
        std::fs::write(&token_path, "expected-token").expect("write token");
        let app = Router::new().route(OPERATOR_LIVE_SESSIONS_PATH, get(|| async { "not-json" }));
        let (url, handle) = spawn_http_test_server(app).await;

        let state = fetch_http_live_sessions_from(url, &token_path).await;

        handle.abort();
        assert!(matches!(state, LiveSessionSourceState::Unavailable));
        std::fs::remove_dir_all(dir).expect("cleanup temp dir");
    }
}
