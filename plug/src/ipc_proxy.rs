//! IPC proxy handler — bridges stdio MCP ↔ daemon IPC.
//!
//! `IpcProxyHandler` implements rmcp's `ServerHandler` trait but forwards
//! all tool calls through the daemon's shared Engine via Unix socket IPC.
//! This is what `plug connect` uses when a daemon is running.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use plug_core::ipc::{self, IpcRequest, IpcResponse};

const DAEMON_PING_INTERVAL: Duration = Duration::from_secs(1);

struct SharedConnection {
    conn: Mutex<crate::runtime::DaemonProxySession>,
    config_path: Option<PathBuf>,
}

/// MCP server handler that proxies all requests through the daemon via IPC.
///
/// Holds a persistent IPC connection to the daemon. The connection is
/// established during `cmd_connect` and reused for all MCP traffic.
///
/// A single mutex guards the entire round-trip (write + read) to prevent
/// concurrent requests from reading each other's responses.
pub struct IpcProxyHandler {
    shared: Arc<SharedConnection>,
    heartbeat: JoinHandle<()>,
}

#[derive(Clone, Copy)]
enum RetryPolicy {
    SafeToRetry,
    UnsafeToRetry,
}

struct TransportFailure {
    message: String,
    reconnectable: bool,
}

impl IpcProxyHandler {
    /// Create a new proxy handler from an established IPC connection.
    pub fn new(session: crate::runtime::DaemonProxySession, config_path: Option<PathBuf>) -> Self {
        let shared = Arc::new(SharedConnection {
            conn: Mutex::new(session),
            config_path,
        });
        let heartbeat = tokio::spawn(Self::heartbeat_loop(shared.clone()));
        Self { shared, heartbeat }
    }

    /// Send an IPC request and read the response.
    ///
    /// Holds the connection lock for the entire round-trip to ensure
    /// request-response pairing under concurrent MCP calls.
    async fn session_round_trip<F>(
        &self,
        retry_policy: RetryPolicy,
        build_request: F,
    ) -> Result<IpcResponse, McpError>
    where
        F: Fn(&str) -> IpcRequest,
    {
        let mut conn = self.shared.conn.lock().await;
        let request = build_request(&conn.session_id);
        let payload = serde_json::to_vec(&request)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        match Self::try_round_trip_locked(&mut conn, &payload).await {
            Ok(response) => Ok(response),
            Err(failure) if failure.reconnectable => {
                tracing::warn!(error = %failure.message, "daemon IPC connection lost; reconnecting");
                self.reconnect_locked(&mut conn).await?;
                match retry_policy {
                    RetryPolicy::SafeToRetry => {
                        let rebound = build_request(&conn.session_id);
                        let retry_payload = serde_json::to_vec(&rebound)
                            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                        Self::try_round_trip_locked(&mut conn, &retry_payload)
                            .await
                            .map_err(|e| {
                                McpError::internal_error(
                                    format!("IPC retry failed after reconnect: {}", e.message),
                                    None,
                                )
                            })
                    }
                    RetryPolicy::UnsafeToRetry => Err(McpError::internal_error(
                        "REQUEST_RETRY_UNSAFE: daemon connection recovered; retry the tool call",
                        None,
                    )),
                }
            }
            Err(failure) => Err(McpError::internal_error(failure.message, None)),
        }
    }

    async fn try_round_trip_locked(
        conn: &mut crate::runtime::DaemonProxySession,
        payload: &[u8],
    ) -> Result<IpcResponse, TransportFailure> {
        ipc::write_frame(&mut conn.writer, payload)
            .await
            .map_err(|e| Self::transport_failure("IPC write failed", e))?;

        let frame = ipc::read_frame(&mut conn.reader)
            .await
            .map_err(|e| Self::transport_failure("IPC read failed", e))?
            .ok_or_else(|| TransportFailure {
                message: "daemon closed connection".to_string(),
                reconnectable: true,
            })?;

        serde_json::from_slice(&frame).map_err(|e| TransportFailure {
            message: format!("invalid IPC response: {e}"),
            reconnectable: false,
        })
    }

    async fn reconnect_locked(
        &self,
        conn: &mut crate::runtime::DaemonProxySession,
    ) -> Result<(), McpError> {
        Self::refresh_session_locked(self.shared.config_path.as_ref(), conn).await
    }

    async fn heartbeat_loop(shared: Arc<SharedConnection>) {
        let mut tick = tokio::time::interval(DAEMON_PING_INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        tick.tick().await;

        loop {
            tick.tick().await;
            if let Err(error) = Self::ping_once(&shared).await {
                tracing::debug!(error = %error, "daemon heartbeat ping failed");
            }
        }
    }

    async fn ping_once(shared: &Arc<SharedConnection>) -> Result<(), McpError> {
        let mut conn = shared.conn.lock().await;
        let payload = serde_json::to_vec(&IpcRequest::Ping {
            session_id: conn.session_id.clone(),
        })
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        match Self::try_round_trip_locked(&mut conn, &payload).await {
            Ok(IpcResponse::Pong) => Ok(()),
            Ok(IpcResponse::Error { code, message }) => {
                if matches!(code.as_str(), "SESSION_REPLACED" | "SESSION_MISMATCH") {
                    tracing::warn!(code = %code, message = %message, "daemon heartbeat detected stale session; reconnecting");
                    Self::refresh_session_locked(shared.config_path.as_ref(), &mut conn).await?;
                    return Ok(());
                }
                Err(McpError::internal_error(format!("{code}: {message}"), None))
            }
            Ok(other) => Err(McpError::internal_error(
                format!("unexpected IPC ping response: {other:?}"),
                None,
            )),
            Err(failure) if failure.reconnectable => {
                tracing::warn!(error = %failure.message, "daemon heartbeat lost connection; reconnecting");
                Self::refresh_session_locked(shared.config_path.as_ref(), &mut conn).await?;
                Ok(())
            }
            Err(failure) => Err(McpError::internal_error(failure.message, None)),
        }
    }

    async fn refresh_session_locked(
        config_path: Option<&PathBuf>,
        conn: &mut crate::runtime::DaemonProxySession,
    ) -> Result<(), McpError> {
        let session = crate::runtime::establish_daemon_proxy_session(
            config_path,
            conn.client_id.clone(),
            conn.client_info.clone(),
        )
        .await
        .map_err(|e| McpError::internal_error(format!("daemon reconnect failed: {e}"), None))?;
        *conn = session;
        Ok(())
    }

    fn transport_failure(context: &str, error: anyhow::Error) -> TransportFailure {
        let reconnectable = error.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::UnexpectedEof
            )
        });

        TransportFailure {
            message: format!("{context}: {error}"),
            reconnectable,
        }
    }
}

impl Drop for IpcProxyHandler {
    fn drop(&mut self) {
        self.heartbeat.abort();
    }
}

#[allow(clippy::manual_async_fn)]
impl ServerHandler for IpcProxyHandler {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability {
            list_changed: Some(true),
        });

        InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("plug", env!("CARGO_PKG_VERSION")))
    }

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, McpError>> + Send + '_ {
        async move {
            let client_name = request.client_info.name.to_string();
            tracing::info!(
                client = %client_name,
                "client connected via IPC proxy"
            );
            self.shared.conn.lock().await.client_info = Some(client_name.clone());

            // Forward client info to daemon for client-type-aware tool filtering
            if let Err(e) = self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::UpdateSession {
                        session_id: session_id.to_string(),
                        client_info: client_name.clone(),
                    }
                })
                .await
            {
                tracing::warn!(error = %e, "failed to update session client info");
            }

            context.peer.set_peer_info(request);
            Ok(self.get_info())
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move {
            match self
                .session_round_trip(RetryPolicy::SafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "tools/list".to_string(),
                        params: None,
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    serde_json::from_value(payload).map_err(|e| {
                        McpError::internal_error(format!("failed to parse tools/list: {e}"), None)
                    })
                }
                IpcResponse::Error { code, message } => {
                    Err(McpError::internal_error(format!("{code}: {message}"), None))
                }
                other => Err(McpError::internal_error(
                    format!("unexpected IPC response: {other:?}"),
                    None,
                )),
            }
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            let params = serde_json::json!({
                "name": request.name,
                "arguments": request.arguments,
            });
            match self
                .session_round_trip(RetryPolicy::UnsafeToRetry, |session_id| {
                    IpcRequest::McpRequest {
                        session_id: session_id.to_string(),
                        method: "tools/call".to_string(),
                        params: Some(params.clone()),
                    }
                })
                .await?
            {
                IpcResponse::McpResponse { payload } => {
                    // Check if this is an error response before attempting CallToolResult parse
                    if payload.get("code").is_some() {
                        if let Ok(err) = serde_json::from_value::<McpError>(payload.clone()) {
                            return Err(err);
                        }
                    }
                    serde_json::from_value::<CallToolResult>(payload).map_err(|e| {
                        McpError::internal_error(
                            format!("unexpected tool call response: {e}"),
                            None,
                        )
                    })
                }
                IpcResponse::Error { code, message } => {
                    Err(McpError::internal_error(format!("{code}: {message}"), None))
                }
                other => Err(McpError::internal_error(
                    format!("unexpected IPC response: {other:?}"),
                    None,
                )),
            }
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListResourcesResult::default()))
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListPromptsResult::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use crate::daemon::{clear_test_runtime_paths, run_daemon, set_test_runtime_paths};
    use plug_core::config::{Config, ServerConfig, TransportType};
    use plug_core::engine::Engine;
    use rmcp::ServiceExt as _;
    use rmcp::handler::client::ClientHandler;
    use tokio::task::JoinHandle;

    fn daemon_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[derive(Clone)]
    struct TestClient;

    impl ClientHandler for TestClient {
        fn get_info(&self) -> ClientInfo {
            ClientInfo::default()
        }
    }

    fn mock_server_config() -> ServerConfig {
        ServerConfig {
            command: Some("cargo".to_string()),
            args: vec![
                "run".to_string(),
                "--quiet".to_string(),
                "-p".to_string(),
                "plug-test-harness".to_string(),
                "--bin".to_string(),
                "mock-mcp-server".to_string(),
                "--".to_string(),
                "--tools".to_string(),
                "echo".to_string(),
            ],
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            url: None,
            auth_token: None,
            timeout_secs: 10,
            call_timeout_secs: 5,
            max_concurrent: 4,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
        }
    }

    async fn spawn_test_daemon(
        config: Config,
        config_path: std::path::PathBuf,
    ) -> (Arc<Engine>, JoinHandle<anyhow::Result<()>>) {
        let engine = Arc::new(Engine::new(config));
        engine.start().await.expect("engine start");
        let engine_for_task = Arc::clone(&engine);
        let handle = tokio::spawn(async move { run_daemon(engine_for_task, config_path, 0).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        if handle.is_finished() {
            let result = handle.await.expect("daemon task join");
            panic!("daemon exited before readiness: {result:?}");
        }
        tokio::time::timeout(Duration::from_secs(5), crate::runtime::wait_for_daemon_ready())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "daemon ready timeout (socket path: {}, task_finished: {})",
                    crate::daemon::socket_path().display(),
                    handle.is_finished()
                )
            })
            .expect("daemon ready");
        (engine, handle)
    }

    #[tokio::test]
    async fn daemon_backed_proxy_recovers_after_daemon_restart() {
        let _guard = daemon_test_lock().lock().await;

        let temp = std::env::temp_dir().join(format!("pdc-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        let runtime_root = temp.join("r");
        let state_root = temp.join("s");
        std::fs::create_dir_all(&runtime_root).expect("create runtime root");
        std::fs::create_dir_all(&state_root).expect("create state root");
        set_test_runtime_paths(runtime_root.clone(), state_root.clone());

        let config_path = temp.join("plug.toml");
        let mut config = Config::default();
        config
            .servers
            .insert("mock".to_string(), mock_server_config());
        std::fs::write(&config_path, toml::to_string(&config).expect("serialize config"))
            .expect("write config");

        let (engine_a, daemon_a) = spawn_test_daemon(config.clone(), config_path.clone()).await;

        let session = crate::runtime::establish_daemon_proxy_session(
            Some(&config_path),
            "client-continuity".to_string(),
            None,
        )
        .await
        .expect("establish daemon proxy session");
        let proxy = IpcProxyHandler::new(session, Some(config_path.clone()));
        let shared = Arc::clone(&proxy.shared);
        let initial_session_id = {
            let conn = shared.conn.lock().await;
            conn.session_id.clone()
        };

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = proxy
                .serve(server_transport)
                .await
                .expect("start IPC proxy server");
            let _ = server.waiting().await;
        });

        let client = TestClient
            .serve(client_transport)
            .await
            .expect("connect downstream client");

        let initial_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let _initial_tools = loop {
            let tools = tokio::time::timeout(Duration::from_secs(5), client.peer().list_all_tools())
                .await
                .expect("initial tools timeout")
                .expect("initial tools");
            if tools.iter().any(|tool| tool.name == "Mock__echo") {
                break tools;
            }
            assert!(
                tokio::time::Instant::now() < initial_deadline,
                "expected daemon-backed proxy to expose Mock__echo"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let initial_result = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool(
                CallToolRequestParams::new("Mock__echo").with_arguments(
                    serde_json::json!({"input": "before"}).as_object().unwrap().clone(),
                ),
            ),
        )
        .await
        .expect("initial tool call timeout")
        .expect("initial tool call");
        assert!(format!("{initial_result:?}").contains("before"));

        engine_a.shutdown().await;
        daemon_a
            .await
            .expect("daemon task join")
            .expect("daemon shutdown cleanly");

        let (engine_b, daemon_b) = spawn_test_daemon(config, config_path.clone()).await;

        let engine_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            match engine_b
                .tool_router()
                .call_tool(
                    "Mock__echo",
                    Some(
                        serde_json::json!({"input": "engine-ready"})
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                )
                .await
            {
                Ok(_) => break,
                Err(error)
                    if error.message.contains("server unavailable")
                        && tokio::time::Instant::now() < engine_deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => panic!("restarted daemon never became ready: {error:?}"),
            }
        }

        let repaired_tools = tokio::time::timeout(Duration::from_secs(5), client.peer().list_all_tools())
            .await
            .expect("tools after reconnect timeout")
            .expect("tools after reconnect");
        assert!(
            repaired_tools.iter().any(|tool| tool.name == "Mock__echo"),
            "expected repaired proxy to expose Mock__echo"
        );
        let repaired_session_id = { shared.conn.lock().await.session_id.clone() };
        assert_ne!(
            repaired_session_id, initial_session_id,
            "reconnect should replace the daemon session"
        );

        engine_b.shutdown().await;
        daemon_b
            .await
            .expect("restarted daemon task join")
            .expect("restarted daemon shutdown cleanly");

        clear_test_runtime_paths();
        let _ = std::fs::remove_dir_all(&temp);
    }
}
