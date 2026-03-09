//! Core Engine — unified runtime for plug MCP multiplexer.
//!
//! The Engine owns all shared state (servers, routing, config, health) and
//! exposes it through a query API. TUI, daemon, and CLI are thin frontends
//! that subscribe to [`EngineEvent`]s via `tokio::sync::broadcast`.
//!
//! All fields are private — consumers access state through methods that
//! return value types, never through direct field access.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::circuit::CircuitState;
use crate::config::Config;
use crate::health::spawn_health_checks;
use crate::proxy::{RouterConfig, ToolRouter};
use crate::server::ServerManager;
use crate::types::{ClientType, ServerHealth, ServerStatus};

/// Broadcast channel capacity. At peak burst (~130 events/sec with 20 servers),
/// this provides ~1 second of buffer. Memory cost: ~25KB.
const EVENT_CHANNEL_CAPACITY: usize = 128;

/// Minimum interval between restarts of the same server.
const RESTART_COOLDOWN: Duration = Duration::from_secs(10);
const RECONNECT_RETRY_MAX_ATTEMPTS: u32 = 5;
const RECONNECT_RETRY_MIN_DELAY: Duration = Duration::from_millis(100);
const RECONNECT_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);

/// Monotonic counter for correlating ToolCallStarted/ToolCallCompleted events.
static NEXT_CALL_ID: AtomicU64 = AtomicU64::new(1);

/// Generate a unique call ID for tool call correlation.
pub fn next_call_id() -> u64 {
    NEXT_CALL_ID.fetch_add(1, Ordering::Relaxed)
}

/// Events emitted by the Engine for observability consumers (TUI, daemon, CLI).
///
/// Uses `Arc<str>` for string fields — O(1) clone on broadcast fan-out
/// instead of O(n) String clone. Create via `Arc::from("value")`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EngineEvent {
    ServerHealthChanged {
        server_id: Arc<str>,
        old: ServerHealth,
        new: ServerHealth,
    },
    CircuitBreakerTripped {
        server_id: Arc<str>,
        state: CircuitState,
    },
    ToolCacheRefreshed {
        tool_count: usize,
    },
    ToolDefinitionDriftDetected {
        tool_names: Vec<Arc<str>>,
    },
    ClientConnected {
        session_id: Arc<str>,
        client_type: ClientType,
    },
    ClientDisconnected {
        session_id: Arc<str>,
    },
    ToolCallStarted {
        call_id: u64,
        server_id: Arc<str>,
        tool_name: Arc<str>,
    },
    ToolCallCompleted {
        call_id: u64,
        server_id: Arc<str>,
        tool_name: Arc<str>,
        duration_ms: u64,
        success: bool,
    },
    ServerStarted {
        server_id: Arc<str>,
    },
    ServerStopped {
        server_id: Arc<str>,
    },
    Error {
        context: Arc<str>,
        message: Arc<str>,
    },
    ConfigReloaded,
}

/// Read-only, Clone-able snapshot of Engine state for initial TUI population
/// and Lagged recovery.
#[derive(Clone, Debug)]
pub struct EngineSnapshot {
    pub servers: Vec<ServerStatus>,
    pub tool_count: usize,
    pub uptime: Duration,
}

/// The core runtime that owns all shared state.
///
/// Created by `Engine::new()`, started with `Engine::start()`, stopped with
/// `Engine::shutdown()`. All consumers (TUI, daemon, CLI) interact through
/// the query API and event subscription.
pub struct Engine {
    server_manager: Arc<ServerManager>,
    tool_router: Arc<ToolRouter>,
    config: Arc<ArcSwap<Config>>,
    cancel: CancellationToken,
    tracker: TaskTracker,
    event_tx: broadcast::Sender<EngineEvent>,
    started_at: Instant,
    /// Per-server last restart timestamp for rate limiting.
    restart_timestamps: dashmap::DashMap<String, Instant>,
}

impl Engine {
    /// Create a new Engine from configuration.
    ///
    /// Does NOT start any servers — call `start()` to begin.
    pub fn new(config: Config) -> Self {
        let server_manager = Arc::new(ServerManager::new());
        let router_config = RouterConfig::from(&config);
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let tool_router = Arc::new(
            ToolRouter::new(server_manager.clone(), router_config).with_event_tx(event_tx.clone()),
        );
        server_manager.set_tool_router(Arc::downgrade(&tool_router));

        Self {
            server_manager,
            tool_router,
            config: Arc::new(ArcSwap::from_pointee(config)),
            cancel: CancellationToken::new(),
            tracker: TaskTracker::new(),
            event_tx,
            started_at: Instant::now(),
            restart_timestamps: dashmap::DashMap::new(),
        }
    }

    /// Start all upstream servers and spawn health checkers.
    ///
    /// Requires `Arc<Self>` so that health checks and tool router can hold
    /// a weak reference back to Engine for session recovery (reconnect on error).
    /// Uses `self.tracker.spawn()` for all background tasks to enable
    /// ordered shutdown via `TaskTracker::wait()`.
    pub async fn start(self: &Arc<Self>) -> Result<(), anyhow::Error> {
        let config = self.config.load();

        // Wire up the Engine reference for session recovery in ToolRouter
        self.tool_router.set_engine(Arc::downgrade(self));

        // Start all upstream servers
        self.server_manager.start_all(&config).await?;

        // Startup failures are currently non-fatal. Preserve them in daemon
        // state so status output is honest and proactive recovery can retry
        // local services that come up shortly after boot/login.
        for (name, server_config) in &config.servers {
            if !server_config.enabled || self.server_manager.get_upstream(name).is_some() {
                continue;
            }
            self.server_manager.mark_start_failure(name);
        }

        // Refresh tool cache after startup
        self.tool_router.refresh_tools().await;

        let tool_count = self.tool_router.tool_count();
        let _ = self
            .event_tx
            .send(EngineEvent::ToolCacheRefreshed { tool_count });

        // Emit ServerStarted events for each running server
        for status in self.server_manager.server_statuses() {
            let _ = self.event_tx.send(EngineEvent::ServerStarted {
                server_id: Arc::from(status.server_id.as_str()),
            });
        }

        // Spawn health checkers using TaskTracker (with Engine ref for proactive recovery)
        spawn_health_checks(
            self.server_manager.clone(),
            self.tool_router.clone(),
            Arc::clone(self),
            self.event_tx.clone(),
            self.cancel.clone(),
            &config,
            &self.tracker,
        );

        Ok(())
    }

    /// Ordered shutdown: cancel tasks → wait for drain → shutdown servers.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        self.tracker.close();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.tracker.wait()).await;
        self.server_manager.shutdown_all().await;
    }

    /// Subscribe to the Engine event bus.
    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.event_tx.subscribe()
    }

    /// Get a read-only snapshot of the current Engine state.
    pub fn snapshot(&self) -> EngineSnapshot {
        EngineSnapshot {
            servers: self.server_manager.server_statuses(),
            tool_count: self.tool_router.tool_count(),
            uptime: self.started_at.elapsed(),
        }
    }

    /// Return status information for all upstream servers.
    pub fn server_statuses(&self) -> Vec<ServerStatus> {
        self.server_manager.server_statuses()
    }

    /// Return the full merged tool list (for clients with no limit).
    pub fn tool_list(&self) -> Arc<Vec<rmcp::model::Tool>> {
        self.tool_router.list_tools_for_client(ClientType::Unknown)
    }

    /// Get the Engine's cancellation token (for shutdown coordination).
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Get a reference to the ToolRouter (for ProxyHandler/HTTP handlers).
    pub fn tool_router(&self) -> &Arc<ToolRouter> {
        &self.tool_router
    }

    /// Get a reference to the ServerManager.
    pub fn server_manager(&self) -> &Arc<ServerManager> {
        &self.server_manager
    }

    /// Get a clone of the event sender (for transport-layer event emission).
    pub fn event_sender(&self) -> broadcast::Sender<EngineEvent> {
        self.event_tx.clone()
    }

    /// Get the loaded config.
    pub fn config(&self) -> arc_swap::Guard<Arc<Config>> {
        self.config.load()
    }

    /// Get a reference to the task tracker (for spawning background tasks).
    pub fn tracker(&self) -> &TaskTracker {
        &self.tracker
    }

    /// Atomically swap in a new config.
    pub fn store_config(&self, config: Config) {
        self.config.store(Arc::new(config));
    }

    /// Restart a specific upstream server. Rate-limited: at most 1 restart
    /// per server per 10 seconds. Applies to both TUI and IPC callers.
    pub async fn restart_server(&self, server_id: &str) -> Result<(), anyhow::Error> {
        // Atomic rate limit check + timestamp update via entry API
        {
            use dashmap::mapref::entry::Entry;
            match self.restart_timestamps.entry(server_id.to_string()) {
                Entry::Occupied(mut entry) => {
                    if entry.get().elapsed() < RESTART_COOLDOWN {
                        let remaining = RESTART_COOLDOWN - entry.get().elapsed();
                        anyhow::bail!(
                            "restart rate limited — try again in {}s",
                            remaining.as_secs() + 1
                        );
                    }
                    entry.insert(Instant::now());
                }
                Entry::Vacant(entry) => {
                    entry.insert(Instant::now());
                }
            }
        }

        let config = self.config.load();
        let server_config = config
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("unknown server: {server_id}"))?
            .clone();

        let _ = self.event_tx.send(EngineEvent::ServerStopped {
            server_id: Arc::from(server_id),
        });

        // Restart the server
        match self
            .server_manager
            .start_server(server_id, &server_config)
            .await
        {
            Ok(upstream) => {
                self.server_manager.replace_server(server_id, upstream);
                self.tool_router.refresh_tools().await;

                let _ = self.event_tx.send(EngineEvent::ServerStarted {
                    server_id: Arc::from(server_id),
                });

                tracing::info!(server = %server_id, "server restarted");
                Ok(())
            }
            Err(e) => {
                let _ = self.event_tx.send(EngineEvent::Error {
                    context: Arc::from("restart_server"),
                    message: Arc::from(e.to_string().as_str()),
                });
                Err(e)
            }
        }
    }

    /// Reconnect a specific upstream server without rate limiting.
    ///
    /// Used by reactive recovery (tool call path) and proactive recovery
    /// (health check path). Unlike `restart_server()`, this has no cooldown
    /// because session recovery should not be throttled.
    ///
    /// Uses an AtomicBool per-server to prevent concurrent reconnects —
    /// if another caller is already reconnecting, returns Ok immediately.
    pub async fn reconnect_server(&self, server_id: &str) -> Result<(), anyhow::Error> {
        let reconnecting = self.server_manager.get_reconnecting_flag(server_id);

        // Try to claim the reconnect — if already in progress, return Ok
        if reconnecting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::debug!(server = %server_id, "reconnect already in progress, skipping");
            return Ok(());
        }

        // RAII guard ensures the flag is always cleared, even on panic or task cancellation.
        struct ReconnectGuard(Arc<AtomicBool>);
        impl Drop for ReconnectGuard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _guard = ReconnectGuard(reconnecting);

        self.do_reconnect(server_id).await
    }

    /// Internal reconnection logic shared by `reconnect_server`.
    async fn do_reconnect(&self, server_id: &str) -> Result<(), anyhow::Error> {
        let config = self.config.load();
        let server_config = config
            .servers
            .get(server_id)
            .ok_or_else(|| anyhow::anyhow!("unknown server: {server_id}"))?
            .clone();

        let mut attempt = 1;
        let mut delay = RECONNECT_RETRY_MIN_DELAY;
        let upstream = loop {
            match self
                .server_manager
                .start_server(server_id, &server_config)
                .await
            {
                Ok(upstream) => break upstream,
                Err(e)
                    if attempt < RECONNECT_RETRY_MAX_ATTEMPTS
                        && is_retryable_reconnect_error(&e) =>
                {
                    tracing::warn!(
                        server = %server_id,
                        attempt,
                        max_attempts = RECONNECT_RETRY_MAX_ATTEMPTS,
                        retry_in_ms = delay.as_millis(),
                        error = %e,
                        "reconnect attempt failed during upstream readiness window"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    delay = (delay * 2).min(RECONNECT_RETRY_MAX_DELAY);
                }
                Err(e) => {
                    let _ = self.event_tx.send(EngineEvent::Error {
                        context: Arc::from("reconnect_server"),
                        message: Arc::from(e.to_string()),
                    });
                    return Err(e);
                }
            }
        };

        self.server_manager.replace_server(server_id, upstream);
        self.tool_router.refresh_tools().await;

        let _ = self.event_tx.send(EngineEvent::ServerStarted {
            server_id: Arc::from(server_id),
        });

        tracing::info!(server = %server_id, "server reconnected");
        Ok(())
    }

    /// Enable or disable a server. Clones the current config, toggles the
    /// `enabled` field, and applies via `reload_config` for a clean diff.
    pub async fn set_server_enabled(
        &self,
        server_id: &str,
        enabled: bool,
    ) -> Result<(), anyhow::Error> {
        let mut new_config = (**self.config.load()).clone();
        let server = new_config
            .servers
            .get_mut(server_id)
            .ok_or_else(|| anyhow::anyhow!("unknown server: {server_id}"))?;

        if server.enabled == enabled {
            return Ok(()); // No change needed
        }

        server.enabled = enabled;
        self.reload_config(new_config).await?;
        Ok(())
    }

    /// Reload configuration from a new Config.
    ///
    /// Diffs the old and new configs, starts/stops/restarts servers as needed,
    /// refreshes the tool cache, and emits `ConfigReloaded`.
    pub async fn reload_config(
        &self,
        new_config: Config,
    ) -> Result<crate::reload::ReloadReport, anyhow::Error> {
        crate::reload::apply_reload(self, new_config).await
    }
}

fn is_retryable_reconnect_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("connection refused")
        || message.contains("connection reset")
        || message.contains("broken pipe")
        || message.contains("timed out")
        || message.contains("timed out waiting")
        || message.contains("session not found")
        || message.contains("404")
        || message.contains("failed to connect to http upstream")
        || message.contains("failed to list tools")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TransportType};

    fn test_config() -> Config {
        Config::default()
    }

    #[test]
    fn engine_creation() {
        let engine = Engine::new(test_config());
        let snapshot = engine.snapshot();
        assert!(snapshot.servers.is_empty());
        assert_eq!(snapshot.tool_count, 0);
    }

    #[test]
    fn engine_subscribe() {
        let engine = Engine::new(test_config());
        let mut rx = engine.subscribe();

        // No events yet — try_recv should return empty
        assert!(rx.try_recv().is_err());

        // Send an event
        let _ = engine.event_tx.send(EngineEvent::ConfigReloaded);
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, EngineEvent::ConfigReloaded));
    }

    #[test]
    fn engine_event_arc_str_clone() {
        // Verify Arc<str> is O(1) clone
        let event = EngineEvent::ServerStarted {
            server_id: Arc::from("test-server"),
        };
        let cloned = event.clone();
        if let (
            EngineEvent::ServerStarted { server_id: a },
            EngineEvent::ServerStarted { server_id: b },
        ) = (&event, &cloned)
        {
            // Arc::ptr_eq confirms same underlying allocation
            assert!(Arc::ptr_eq(a, b));
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn call_id_monotonic() {
        let a = next_call_id();
        let b = next_call_id();
        let c = next_call_id();
        assert!(b > a);
        assert!(c > b);
    }

    #[tokio::test]
    async fn engine_shutdown_without_start() {
        let engine = Engine::new(test_config());
        // Should not panic even without start()
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn engine_restart_rate_limit() {
        let engine = Engine::new(test_config());
        // Insert a recent timestamp
        engine
            .restart_timestamps
            .insert("test-server".to_string(), Instant::now());

        let result = engine.restart_server("test-server").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("rate limited"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn engine_restart_unknown_server() {
        let engine = Engine::new(test_config());
        let result = engine.restart_server("nonexistent").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown server"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn engine_set_server_enabled_unknown() {
        let engine = Engine::new(test_config());
        let result = engine.set_server_enabled("nonexistent", true).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn engine_set_server_enabled_noop() {
        use crate::config::ServerConfig;
        use std::collections::HashMap;

        let mut config = Config::default();
        config.servers.insert(
            "test".to_string(),
            ServerConfig {
                command: Some("test-server".to_string()),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                auth_token: None,
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );

        let engine = Engine::new(config);
        // Already enabled — should be a no-op
        let result = engine.set_server_enabled("test", true).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn engine_set_server_enabled_toggle() {
        use crate::config::ServerConfig;
        use std::collections::HashMap;

        let mut config = Config::default();
        config.servers.insert(
            "test".to_string(),
            ServerConfig {
                command: Some("test-server".to_string()),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                auth_token: None,
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );

        let engine = Engine::new(config);
        let mut rx = engine.subscribe();

        // Disable the server — triggers reload which emits ConfigReloaded
        let result = engine.set_server_enabled("test", false).await;
        assert!(result.is_ok());

        // Config should reflect the change
        let cfg = engine.config();
        assert!(!cfg.servers["test"].enabled);

        // Should have emitted ConfigReloaded (possibly after other events)
        let mut found_reloaded = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, EngineEvent::ConfigReloaded) {
                found_reloaded = true;
                break;
            }
        }
        assert!(found_reloaded, "expected ConfigReloaded event");
    }

    /// Verify that concurrent reads (snapshot, server_statuses, tool_list) remain
    /// safe while reload_config removes/adds servers. The ArcSwap-based design
    /// guarantees wait-free reads; this test documents that invariant.
    #[tokio::test]
    async fn concurrent_reads_during_reload() {
        use crate::config::ServerConfig;
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicBool, Ordering};

        // Config A: has "alpha" server (will be removed by reload)
        let mut config_a = Config::default();
        config_a.servers.insert(
            "alpha".to_string(),
            ServerConfig {
                command: Some("test-server".to_string()),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                auth_token: None,
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),
            },
        );

        // Config B: empty (removes "alpha")
        let config_b = Config::default();

        let engine = Arc::new(Engine::new(config_a));
        let done = Arc::new(AtomicBool::new(false));

        // Spawn reader tasks that continuously call snapshot/server_statuses/tool_list
        let mut readers = Vec::new();
        for _ in 0..4 {
            let eng = engine.clone();
            let done_flag = done.clone();
            readers.push(tokio::spawn(async move {
                let mut iterations = 0u64;
                while !done_flag.load(Ordering::Relaxed) {
                    let _snap = eng.snapshot();
                    let _statuses = eng.server_statuses();
                    let _tools = eng.tool_list();
                    iterations += 1;
                    // Yield to avoid starving the reload task
                    if iterations % 100 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
                iterations
            }));
        }

        // Run several reload cycles concurrently with the readers
        // (reload with empty config — no actual MCP servers to start/stop,
        // but it exercises the full config diff + swap + event path)
        for _ in 0..10 {
            let _ = engine.reload_config(config_b.clone()).await;
            tokio::task::yield_now().await;
        }

        // Signal readers to stop
        done.store(true, Ordering::Relaxed);

        // All readers should complete without panic
        for handle in readers {
            let iterations = handle.await.expect("reader task panicked");
            assert!(iterations > 0, "reader should have executed at least once");
        }

        // Config should reflect the last reload (empty)
        let snap = engine.snapshot();
        assert!(snap.servers.is_empty());
    }

    #[tokio::test]
    async fn broadcast_lagged_recovery() {
        let engine = Engine::new(test_config());

        // Create a receiver with capacity 128
        let mut rx = engine.subscribe();

        // Fill the buffer beyond capacity to trigger Lagged
        for i in 0..200 {
            let _ = engine
                .event_tx
                .send(EngineEvent::ToolCacheRefreshed { tool_count: i });
        }

        // First recv should be Lagged
        match rx.recv().await {
            Err(broadcast::error::RecvError::Lagged(n)) => {
                assert!(n > 0);
                // Recovery: get snapshot
                let snapshot = engine.snapshot();
                assert!(snapshot.servers.is_empty());
            }
            Ok(_) => {
                // Some events may still be receivable — that's fine
            }
            Err(broadcast::error::RecvError::Closed) => {
                panic!("channel should not be closed");
            }
        }
    }

    #[test]
    fn retryable_reconnect_error_classifier_matches_restart_window_failures() {
        let err = anyhow::anyhow!(
            "failed to connect to HTTP upstream: error sending request for url (http://localhost:8000/mcp): connection refused"
        );
        assert!(is_retryable_reconnect_error(&err));

        let err = anyhow::anyhow!("failed to list tools: HTTP 404 Not Found: Session not found");
        assert!(is_retryable_reconnect_error(&err));

        let err = anyhow::anyhow!("stdio transport requires a command");
        assert!(!is_retryable_reconnect_error(&err));
    }
}
