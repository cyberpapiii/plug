//! Core Engine — unified runtime for plug MCP multiplexer.
//!
//! The Engine owns all shared state (servers, routing, config, health) and
//! exposes it through a query API. TUI, daemon, and CLI are thin frontends
//! that subscribe to [`EngineEvent`]s via `tokio::sync::broadcast`.
//!
//! All fields are private — consumers access state through methods that
//! return value types, never through direct field access.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
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
        let router_config = RouterConfig {
            prefix_delimiter: config.prefix_delimiter.clone(),
            priority_tools: config.priority_tools.clone(),
            tool_description_max_chars: config.tool_description_max_chars,
            tool_search_threshold: config.tool_search_threshold,
            tool_filter_enabled: config.tool_filter_enabled,
        };
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let tool_router = Arc::new(
            ToolRouter::new(server_manager.clone(), router_config)
                .with_event_tx(event_tx.clone()),
        );

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
    /// Uses `self.tracker.spawn()` for all background tasks to enable
    /// ordered shutdown via `TaskTracker::wait()`.
    pub async fn start(&self) -> Result<(), anyhow::Error> {
        let config = self.config.load();

        // Start all upstream servers
        self.server_manager.start_all(&config).await?;

        // Refresh tool cache after startup
        self.tool_router.refresh_tools().await;

        let tool_count = self.tool_router.tool_count();
        let _ = self.event_tx.send(EngineEvent::ToolCacheRefreshed { tool_count });

        // Emit ServerStarted events for each running server
        for status in self.server_manager.server_statuses() {
            let _ = self.event_tx.send(EngineEvent::ServerStarted {
                server_id: Arc::from(status.server_id.as_str()),
            });
        }

        // Spawn health checkers using TaskTracker
        spawn_health_checks(
            self.server_manager.clone(),
            self.tool_router.clone(),
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
        match ServerManager::start_server(server_id, &server_config).await {
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

    /// Enable or disable a server. Data-in pattern — caller specifies
    /// the desired state explicitly.
    pub async fn set_server_enabled(
        &self,
        _server_id: &str,
        _enabled: bool,
    ) -> Result<(), anyhow::Error> {
        // TODO: Implement in Sub-phase C when config hot-reload is added.
        // For now, this is a placeholder that validates the server exists.
        let config = self.config.load();
        if !config.servers.contains_key(_server_id) {
            anyhow::bail!("unknown server: {_server_id}");
        }
        anyhow::bail!("set_server_enabled not yet implemented — requires config hot-reload")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

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
    async fn broadcast_lagged_recovery() {
        let engine = Engine::new(test_config());

        // Create a receiver with capacity 128
        let mut rx = engine.subscribe();

        // Fill the buffer beyond capacity to trigger Lagged
        for i in 0..200 {
            let _ = engine.event_tx.send(EngineEvent::ToolCacheRefreshed {
                tool_count: i,
            });
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
}
