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
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::circuit::CircuitState;
use crate::config::{Config, ServerConfig};
use crate::health::spawn_health_checks;
use crate::proxy::{RouterConfig, ToolRouter};
use crate::reload::server_config_changed;
use crate::server::{ServerManager, UpstreamServer, retire_upstream_owned};
use crate::types::{ClientType, ServerHealth, ServerStatus};

/// Broadcast channel capacity. At peak burst (~130 events/sec with 20 servers),
/// this provides ~1 second of buffer. Memory cost: ~25KB.
const EVENT_CHANNEL_CAPACITY: usize = 128;

/// Minimum interval between restarts of the same server.
const RESTART_COOLDOWN: Duration = Duration::from_secs(10);
const RECONNECT_RETRY_MAX_ATTEMPTS: u32 = 5;
const RECONNECT_RETRY_MIN_DELAY: Duration = Duration::from_millis(100);
const RECONNECT_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
const ARTIFACT_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

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
        trace_id: Arc<str>,
        server_id: Arc<str>,
        tool_name: Arc<str>,
    },
    ToolCallCompleted {
        call_id: u64,
        trace_id: Arc<str>,
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

/// Outcome of [`Engine::commit_replacement`].
///
/// Any future code path that installs a freshly-connected upstream into the
/// live server map outside of `reload_config` itself must go through
/// `commit_replacement` (and therefore inherits this staleness check) —
/// never call `ServerManager::replace_server` directly from a new caller.
pub(crate) enum ReplaceOutcome {
    /// Config still matches the snapshot the connect was made against —
    /// upstream installed.
    Committed,
    /// The server was removed, or materially reconfigured (per the SAME
    /// `server_config_changed` predicate reload uses), concurrently with the
    /// connect — the new upstream was retired and the server map was left
    /// untouched, because reload already established the desired reality.
    StaleDiscarded,
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
    health_task_generations: dashmap::DashMap<String, u64>,
    refresh_task_generations: dashmap::DashMap<String, u64>,
    recovery_task_flags: dashmap::DashMap<String, Arc<AtomicBool>>,
    /// Consecutive supervised-restart count per server since its last recovery to
    /// healthy (item 2b). Drives the exponential inter-episode backoff; cleared
    /// when the server returns to healthy.
    supervision_attempts: dashmap::DashMap<String, u32>,
    reload_lock: Mutex<()>,
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
            health_task_generations: dashmap::DashMap::new(),
            refresh_task_generations: dashmap::DashMap::new(),
            recovery_task_flags: dashmap::DashMap::new(),
            supervision_attempts: dashmap::DashMap::new(),
            reload_lock: Mutex::new(()),
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
            if self.server_manager.health.contains_key(name) {
                continue;
            }
            // Fall back to the old startup classification only if no earlier
            // start attempt recorded a more specific health state.
            if server_config.auth.as_deref() == Some("oauth") {
                self.server_manager.mark_auth_required(name);
            } else {
                self.server_manager.mark_start_failure(name);
            }
        }

        // Refresh tool cache after startup
        self.tool_router.refresh_tools().await;
        self.tool_router.prune_artifacts();

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

        // Spawn OAuth token refresh loops
        spawn_refresh_loops(
            Arc::clone(self),
            self.cancel.clone(),
            &config,
            &self.tracker,
        );

        spawn_artifact_prune_loop(self.tool_router.clone(), self.cancel.clone(), &self.tracker);

        Ok(())
    }

    /// Bounded shutdown: cancel tasks, give background work a short drain window,
    /// then explicitly retire upstreams without spending the full caller timeout.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        self.tracker.close();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.tracker.wait()).await;
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

    pub(crate) fn next_health_task_generation(&self, server_name: &str) -> u64 {
        let next = self
            .health_task_generations
            .get(server_name)
            .map(|entry| *entry + 1)
            .unwrap_or(1);
        self.health_task_generations
            .insert(server_name.to_string(), next);
        next
    }

    pub(crate) fn current_health_task_generation(&self, server_name: &str) -> Option<u64> {
        self.health_task_generations
            .get(server_name)
            .map(|entry| *entry)
    }

    pub(crate) fn clear_health_task_generation(&self, server_name: &str) {
        self.health_task_generations.remove(server_name);
    }

    pub(crate) fn next_refresh_task_generation(&self, server_name: &str) -> u64 {
        let next = self
            .refresh_task_generations
            .get(server_name)
            .map(|entry| *entry + 1)
            .unwrap_or(1);
        self.refresh_task_generations
            .insert(server_name.to_string(), next);
        next
    }

    pub(crate) fn current_refresh_task_generation(&self, server_name: &str) -> Option<u64> {
        self.refresh_task_generations
            .get(server_name)
            .map(|entry| *entry)
    }

    pub(crate) fn clear_refresh_task_generation(&self, server_name: &str) {
        self.refresh_task_generations.remove(server_name);
    }

    pub(crate) fn try_claim_recovery_task(&self, server_name: &str) -> Option<Arc<AtomicBool>> {
        let flag = self
            .recovery_task_flags
            .entry(server_name.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone();
        if flag
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            Some(flag)
        } else {
            None
        }
    }

    /// Decide whether the supervisor should restart `server_name` now (item 2b).
    /// Pure read of the current health streak, circuit state, restart history, and
    /// the configured `SupervisionConfig` policy — no side effects.
    pub(crate) fn supervision_due(&self, server_name: &str) -> bool {
        let config = self.config();
        let (health, consecutive_failures) = self.server_manager.health_streak(server_name);
        let circuit_open = self.server_manager.circuit_open(server_name);
        let secs_since_last_restart =
            self.server_manager
                .last_restart_epoch(server_name)
                .map(|e| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_secs().saturating_sub(e))
                        .unwrap_or(0)
                });
        let attempts = self
            .supervision_attempts
            .get(server_name)
            .map(|a| *a)
            .unwrap_or(0);
        config.supervision.should_restart(
            health,
            consecutive_failures,
            circuit_open,
            secs_since_last_restart,
            attempts,
        )
    }

    /// Stamp a restart in the metrics + backoff clock. Called for ANY recovery
    /// episode (crash/disconnect, Failed-health, or supervised) so the backoff
    /// clock is consistent: a crash-recovery suppresses an immediate supervised
    /// restart, and `restart_count` reflects every actual restart.
    pub(crate) fn note_restart(&self, server_name: &str) {
        self.server_manager.record_restart(server_name);
    }

    /// Grow the supervised-restart backoff attempt counter (supervised episodes
    /// only — crash/Failed recoveries stamp the clock but do not escalate it).
    pub(crate) fn grow_supervision_backoff(&self, server_name: &str) {
        *self
            .supervision_attempts
            .entry(server_name.to_string())
            .or_insert(0) += 1;
    }

    /// Convenience for a supervised restart: stamp the clock/metric AND grow the
    /// escalating backoff.
    pub(crate) fn note_supervised_restart(&self, server_name: &str) {
        self.note_restart(server_name);
        self.grow_supervision_backoff(server_name);
    }

    /// Whether enough time has passed since the last restart to consider the
    /// upstream genuinely recovered and clear the escalating backoff. A brief
    /// post-restart healthy blip (before the upstream re-degrades) is NOT settled,
    /// so a flapping upstream's backoff keeps escalating instead of resetting to
    /// the floor every cycle.
    pub(crate) fn supervision_settled(&self, server_name: &str) -> bool {
        let max = self.config().supervision.max_restart_interval_secs;
        match self.server_manager.last_restart_epoch(server_name) {
            None => true,
            Some(epoch) => {
                std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs().saturating_sub(epoch))
                    .unwrap_or(0)
                    >= max
            }
        }
    }

    /// Reset a server's supervised-restart backoff after it has stably recovered.
    pub(crate) fn reset_supervision(&self, server_name: &str) {
        self.supervision_attempts.remove(server_name);
    }

    pub(crate) fn spawn_background_tasks_for_server(
        self: &Arc<Self>,
        server_name: &str,
        server_config: &crate::config::ServerConfig,
    ) {
        crate::health::spawn_health_check(
            self.server_manager.clone(),
            self.tool_router.clone(),
            Arc::clone(self),
            self.event_tx.clone(),
            self.cancel.clone(),
            server_name.to_string(),
            server_config.health_check_interval_secs,
            &self.tracker,
        );
        self.sync_refresh_loop_for_server(server_name, server_config);
    }

    fn sync_refresh_loop_for_server(
        self: &Arc<Self>,
        server_name: &str,
        server_config: &crate::config::ServerConfig,
    ) {
        if server_config.enabled && server_config.auth.as_deref() == Some("oauth") {
            spawn_refresh_loop_for_server(
                Arc::clone(self),
                self.cancel.clone(),
                server_name.to_string(),
                &self.tracker,
            );
        } else {
            self.clear_refresh_task_generation(server_name);
        }
    }

    /// Atomically swap in a new config.
    pub fn store_config(&self, config: Config) {
        self.config.store(Arc::new(config));
    }

    /// Commit a freshly-connected upstream, unless the server's config changed
    /// (or the server vanished) since `connected_with` was snapshotted.
    ///
    /// Holds `reload_lock` across validate+install so an in-flight
    /// `reload_config` is either fully before or fully after this commit —
    /// there is no torn middle to observe. MUST NOT be called from any path
    /// that already holds `reload_lock` (tokio's `Mutex` is not reentrant);
    /// today nothing under `reload_lock` reaches this method (verified: no
    /// caller of `restart_server`/`reconnect_server`/`do_reconnect` is
    /// reachable from `reload.rs`).
    ///
    /// The materiality check MUST use `server_config_changed` — the SAME
    /// predicate `reload_config` uses to decide restart-vs-skip — never
    /// whole-struct equality. Reload only takes authoritative action (stop,
    /// or stop+restart) for servers that predicate calls changed; discarding
    /// here is only justified by "reload already established the desired
    /// reality" for exactly those cases. A non-material change (e.g.
    /// `max_concurrent`, which the predicate deliberately omits) must still
    /// commit, or a successful reconnection would be silently thrown away
    /// and the server would be stranded down.
    ///
    /// Work under the lock is bounded: one `ArcSwap` load, one comparison,
    /// and either `replace_server` (a `DashMap` insert plus either a spawned
    /// grace-period retirement or an awaited retirement bounded by
    /// `UPSTREAM_REPLACEMENT_SHUTDOWN_TIMEOUT`) or the same bounded
    /// retirement applied to the discarded upstream. No network connects
    /// happen under the lock.
    ///
    /// The reverse interleaving is benign: if this commit wins the lock
    /// first, reload's own diff runs immediately after against the
    /// just-installed upstream and stops/restarts it per the new config —
    /// the end state is still the new config's.
    async fn commit_replacement(
        &self,
        server_id: &str,
        connected_with: &ServerConfig,
        upstream: UpstreamServer,
    ) -> ReplaceOutcome {
        let _guard = self.reload_lock.lock().await;
        let current = self.config.load();
        match current.servers.get(server_id) {
            Some(cfg) if !server_config_changed(connected_with, cfg) => {
                self.server_manager
                    .replace_server(server_id, upstream)
                    .await;
                ReplaceOutcome::Committed
            }
            _ => {
                // Never inserted into the map — we hold the only handle, so
                // retire inline (no grace period needed; nothing else can be
                // reading this Arc).
                retire_upstream_owned(
                    server_id.to_string(),
                    Arc::new(upstream),
                    "discarded: server removed or reconfigured during reconnect/restart",
                )
                .await;
                ReplaceOutcome::StaleDiscarded
            }
        }
    }

    /// Restart a specific upstream server. Rate-limited: at most 1 restart
    /// per server per 10 seconds. Applies to both TUI and IPC callers.
    pub async fn restart_server(self: &Arc<Self>, server_id: &str) -> Result<(), anyhow::Error> {
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
                match self
                    .commit_replacement(server_id, &server_config, upstream)
                    .await
                {
                    ReplaceOutcome::Committed => {
                        self.sync_refresh_loop_for_server(server_id, &server_config);
                        self.tool_router.refresh_tools().await;

                        let _ = self.event_tx.send(EngineEvent::ServerStarted {
                            server_id: Arc::from(server_id),
                        });

                        tracing::info!(server = %server_id, "server restarted");
                        Ok(())
                    }
                    ReplaceOutcome::StaleDiscarded => {
                        // The ServerStopped event above is accurate either way — the
                        // old instance WAS stopped. This is a user-facing command, so
                        // it should say why it didn't do what was asked.
                        Err(anyhow::anyhow!(
                            "server '{server_id}' was removed or reconfigured by a concurrent config reload; restart abandoned"
                        ))
                    }
                }
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
            // Cheap early-exit (optimization, not the correctness guarantee —
            // that's commit_replacement below): if the server vanished or was
            // materially reconfigured since we snapshotted it, stop dialing
            // now instead of burning the rest of the retry window. Uses the
            // SAME predicate as the commit check, never equality — a
            // non-material change (e.g. max_concurrent) must NOT trip this,
            // or we would abandon a reconnect that reload never replaced,
            // stranding the server down.
            let current = self.config.load();
            match current.servers.get(server_id) {
                Some(cfg) if !server_config_changed(&server_config, cfg) => {}
                _ => {
                    tracing::info!(
                        server = %server_id,
                        "reconnect abandoned: server removed or reconfigured during retry"
                    );
                    return Ok(());
                }
            }
            drop(current);

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

        match self
            .commit_replacement(server_id, &server_config, upstream)
            .await
        {
            ReplaceOutcome::Committed => {
                self.tool_router.refresh_tools().await;

                let _ = self.event_tx.send(EngineEvent::ServerStarted {
                    server_id: Arc::from(server_id),
                });

                tracing::info!(server = %server_id, "server reconnected");
                Ok(())
            }
            ReplaceOutcome::StaleDiscarded => {
                // The reload already established the desired reality for this
                // server (removed it, or replaced it per the new config) —
                // there is nothing left for this reconnect to do.
                tracing::info!(
                    server = %server_id,
                    "reconnect abandoned: server removed or reconfigured during retry"
                );
                Ok(())
            }
        }
    }

    /// Enable or disable a server. Clones the current config, toggles the
    /// `enabled` field, and applies via `reload_config` for a clean diff.
    pub async fn set_server_enabled(
        self: &Arc<Self>,
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
        self: &Arc<Self>,
        new_config: Config,
    ) -> Result<crate::reload::ReloadReport, anyhow::Error> {
        let _guard = self.reload_lock.lock().await;
        crate::reload::apply_reload(self, new_config).await
    }
}

/// Spawn background token refresh tasks for all OAuth-configured servers.
///
/// Each OAuth server gets a task that:
/// 1. Sleeps until the refresh window opens (computed from cached token expiry)
/// 2. Attempts token refresh via reconnect (which triggers `get_access_token()`)
/// 3. On success: triggers zero-downtime reconnect via `Engine::reconnect_server()`
/// 4. On terminal failure: marks the server `AuthRequired` and exits the loop
///
/// Tasks are spawned via `tracker.spawn()` with `CancellationToken` for clean shutdown.
pub fn spawn_refresh_loops(
    engine: Arc<Engine>,
    cancel: CancellationToken,
    config: &Config,
    tracker: &TaskTracker,
) {
    for (name, sc) in &config.servers {
        if sc.auth.as_deref() != Some("oauth") || !sc.enabled {
            continue;
        }
        spawn_refresh_loop_for_server(engine.clone(), cancel.clone(), name.clone(), tracker);
    }
}

pub fn spawn_artifact_prune_loop(
    tool_router: Arc<ToolRouter>,
    cancel: CancellationToken,
    tracker: &TaskTracker,
) {
    tracker.spawn(async move {
        let mut interval = tokio::time::interval(ARTIFACT_PRUNE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    tool_router.prune_artifacts();
                }
            }
        }
    });
}

pub fn spawn_refresh_loop_for_server(
    engine: Arc<Engine>,
    cancel: CancellationToken,
    server_name: String,
    tracker: &TaskTracker,
) {
    let generation = engine.next_refresh_task_generation(&server_name);
    tracker.spawn(async move {
        run_refresh_loop(&engine, &server_name, cancel, generation).await;
    });
}

/// The per-server refresh loop.
///
/// Monitors the cached token expiry for a single OAuth server and triggers
/// reconnection when the refresh window opens. Exits the loop if the server
/// enters a terminal `AuthRequired` state (refresh token revoked, etc.).
async fn run_refresh_loop(
    engine: &Engine,
    server_name: &str,
    cancel: CancellationToken,
    generation: u64,
) {
    use crate::oauth;

    // Get the credential store for cache access
    let store = oauth::get_or_create_store(server_name);

    let mut next_check = Duration::from_secs(30);
    let mut reconnect_pending = false;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                tracing::debug!(server = %server_name, "refresh loop shutting down");
                break;
            }
            _ = tokio::time::sleep(next_check) => {
                if engine.current_refresh_task_generation(server_name) != Some(generation) {
                    tracing::debug!(server = %server_name, "refresh loop generation superseded");
                    break;
                }

                let current_config = {
                    let cfg = engine.config();
                    cfg.servers.get(server_name).cloned()
                };
                let Some(current_config) = current_config else {
                    engine.clear_refresh_task_generation(server_name);
                    break;
                };
                if !current_config.enabled || current_config.auth.as_deref() != Some("oauth") {
                    break;
                }

                // If a prior iteration already refreshed the token but
                // reconnect failed, skip the refresh and retry reconnect.
                if !reconnect_pending {
                    // Read the cached credentials to check expiry
                    let (received_at, expires_in) = match store.cached_expiry() {
                        Some(t) => t,
                        None => {
                            // No cached token — nothing to refresh
                            next_check = Duration::from_secs(30);
                            continue;
                        }
                    };

                    if !oauth::token_needs_refresh(received_at, expires_in) {
                        // Not due — compute next wake time
                        next_check = oauth::time_until_refresh_window(received_at, expires_in)
                            .min(Duration::from_secs(30))
                            .max(Duration::from_secs(5));
                        continue;
                    }

                    tracing::info!(server = %server_name, "token refresh due, attempting OAuth token refresh");

                    // --- Step 1: Refresh the OAuth token at the token endpoint ---
                    //
                    // Load the server config to get the URL and client_id.
                    let (server_url, oauth_client_id) = {
                        let cfg = engine.config();
                        match cfg.servers.get(server_name) {
                            Some(sc) => (
                                sc.url.clone(),
                                sc.oauth_client_id.clone(),
                            ),
                            None => {
                                tracing::warn!(server = %server_name, "server config not found, exiting refresh loop");
                                engine.clear_refresh_task_generation(server_name);
                                break;
                            }
                        }
                    };

                    let url = match server_url {
                        Some(ref u) => u.as_str(),
                        None => {
                            tracing::warn!(server = %server_name, "no server URL, cannot refresh token");
                            engine.clear_refresh_task_generation(server_name);
                            break;
                        }
                    };

                    let result = oauth::refresh_access_token(
                        server_name,
                        url,
                        oauth_client_id.as_deref(),
                    )
                    .await;

                    match result {
                        oauth::RefreshResult::Refreshed => {
                            publish_token_refresh_exchanged(engine, server_name);
                            tracing::info!(server = %server_name, "OAuth token refreshed, reconnecting with fresh token");
                        }
                        oauth::RefreshResult::InjectedToken => {
                            // Injected tokens cannot be refreshed via OAuth.
                            // They rely on external re-injection via InjectToken IPC.
                            tracing::info!(
                                server = %server_name,
                                "injected token — cannot refresh via OAuth, skipping"
                            );
                            next_check = Duration::from_secs(30);
                            continue;
                        }
                        oauth::RefreshResult::NoRefreshToken => {
                            tracing::warn!(
                                server = %server_name,
                                "no refresh_token available, cannot refresh"
                            );
                            mark_auth_required(engine, server_name).await;
                            break;
                        }
                        oauth::RefreshResult::NoCredentials => {
                            tracing::warn!(
                                server = %server_name,
                                "no stored credentials, marking AuthRequired"
                            );
                            mark_auth_required(engine, server_name).await;
                            break;
                        }
                        oauth::RefreshResult::AuthError(e) => {
                            tracing::warn!(
                                server = %server_name,
                                error = %e,
                                "token refresh rejected by authorization server, marking AuthRequired"
                            );
                            mark_auth_required(engine, server_name).await;
                            break;
                        }
                        oauth::RefreshResult::TransientError(e) => {
                            tracing::warn!(
                                server = %server_name,
                                error = %e,
                                "token refresh failed (transient), retrying in 30s"
                            );
                            next_check = Duration::from_secs(30);
                            continue;
                        }
                    }
                } else {
                    tracing::info!(
                        server = %server_name,
                        "retrying reconnect with already-refreshed token"
                    );
                }

                // --- Step 2: Reconnect with the fresh token ---
                match engine.reconnect_server(server_name).await {
                    Ok(()) => {
                        reconnect_pending = false;
                        tracing::info!(server = %server_name, "zero-downtime token refresh + reconnect succeeded");
                        next_check = Duration::from_secs(30);
                    }
                    Err(e) => {
                        if is_auth_reconnect_error(&e) {
                            tracing::warn!(
                                server = %server_name,
                                "reconnect with fresh token failed (auth error), marking AuthRequired"
                            );
                            mark_auth_required(engine, server_name).await;
                            break;
                        }
                        reconnect_pending = true;
                        tracing::warn!(
                            server = %server_name,
                            error = %e,
                            "reconnect after token refresh failed, retrying in 30s"
                        );
                        next_check = Duration::from_secs(30);
                    }
                }
            }
        }
    }
}

fn publish_token_refresh_exchanged(engine: &Engine, server_name: &str) {
    engine.tool_router().publish_protocol_notification(
        crate::notifications::ProtocolNotification::TokenRefreshExchanged {
            server_id: Arc::from(server_name),
        },
    );
}

/// Mark a server as `AuthRequired` and broadcast the state change.
///
/// Used by `run_refresh_loop` when the token cannot be refreshed (revoked,
/// missing refresh_token, etc.).
async fn mark_auth_required(engine: &Engine, server_name: &str) {
    engine.server_manager().mark_auth_required(server_name);
    engine.tool_router().refresh_tools().await;
    let _ = engine
        .event_sender()
        .send(EngineEvent::ServerHealthChanged {
            server_id: Arc::from(server_name),
            old: ServerHealth::Healthy,
            new: ServerHealth::AuthRequired,
        });
    engine.tool_router().publish_protocol_notification(
        crate::notifications::ProtocolNotification::AuthStateChanged {
            server_id: Arc::from(server_name),
            new_state: ServerHealth::AuthRequired,
        },
    );
}

fn is_retryable_reconnect_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_lowercase();
    let is_rate_limited = message.contains("too many requests")
        || message.contains("429")
        || message.contains("error_code\": \"too_many_requests\"")
        || message.contains("blocked_seconds");
    let is_session_conflict = message.contains("failed to open sse stream: conflict")
        || message.contains("too many active sessions")
        || message.contains("503 service unavailable");

    if is_rate_limited || is_session_conflict {
        return false;
    }

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

fn is_auth_reconnect_error(error: &anyhow::Error) -> bool {
    crate::oauth::is_auth_error_chain(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TransportType};
    use std::collections::HashMap;

    fn test_config() -> Config {
        Config::default()
    }

    fn oauth_mock_stdio_config() -> Config {
        let mut config = Config::default();
        config.servers.insert(
            "oauth-mock".to_string(),
            crate::config::ServerConfig {
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
                auth: Some("oauth".to_string()),
                oauth_client_id: Some("test-client".to_string()),
                oauth_scopes: Some(vec!["read".to_string()]),
                timeout_secs: 30,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),

                sandbox: None,
            },
        );
        config
    }

    /// A real (non-oauth) mock stdio `ServerConfig` backed by
    /// `plug-test-harness`'s `mock-mcp-server` binary — same connect
    /// mechanics as `oauth_mock_stdio_config` above, parameterized so tests
    /// can produce configs that are materially different (`tools` feeds
    /// `args`, which `server_config_changed` compares) or only
    /// non-materially different (`max_concurrent`, which it omits).
    fn mock_stdio_server_config(tools: &str, max_concurrent: usize) -> crate::config::ServerConfig {
        crate::config::ServerConfig {
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
                tools.to_string(),
            ],
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
            max_concurrent,
            health_check_interval_secs: 60,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),

            sandbox: None,
        }
    }

    fn mock_stdio_config(name: &str, tools: &str) -> Config {
        let mut config = Config::default();
        config
            .servers
            .insert(name.to_string(), mock_stdio_server_config(tools, 1));
        config
    }

    // ── Plan 011: reconnect/restart vs. reload interlock ──────────────────
    //
    // These tests cover `Engine::commit_replacement`, the commit protocol
    // that stops `do_reconnect`/`restart_server` from resurrecting a server
    // a concurrent config reload already removed or materially reconfigured.

    /// Bug demonstration (plan 011, step 2): drives the RAW, unconditional
    /// primitives `ServerManager::start_server` + `ServerManager::replace_server`
    /// directly — bypassing `do_reconnect`/`commit_replacement` entirely — to
    /// reproduce the exact defect this plan fixes. `replace_server` itself is
    /// intentionally left unconditional by this plan (plan 012 owns its
    /// grace-spawn cleanup), so this test passes both before and after the
    /// fix: it documents WHY production callers must never call
    /// `replace_server` directly with a stale snapshot. The paired test
    /// directly below, `commit_discards_when_server_removed`, proves the same
    /// scenario is safe once routed through `commit_replacement`.
    #[tokio::test]
    async fn raw_replace_server_resurrects_removed_server() {
        let engine = Arc::new(Engine::new(mock_stdio_config("foo", "echo")));
        let snapshot = engine
            .config()
            .servers
            .get("foo")
            .expect("foo configured")
            .clone();

        // Connect against the snapshot — the "reconnect in flight" step.
        let upstream = engine
            .server_manager()
            .start_server("foo", &snapshot)
            .await
            .expect("mock upstream connects");

        // Meanwhile the operator deletes "foo" and reload runs to completion.
        engine
            .reload_config(Config::default())
            .await
            .expect("reload succeeds");
        assert!(
            !engine.config().servers.contains_key("foo"),
            "foo should be gone from config after reload"
        );

        // The stale reconnect's tail, exactly what `do_reconnect` used to do
        // unconditionally before this plan.
        engine
            .server_manager()
            .replace_server("foo", upstream)
            .await;

        assert!(
            engine.server_manager().get_upstream("foo").is_some(),
            "raw replace_server resurrects a removed server — this is the defect \
             commit_replacement exists to prevent in the real callers"
        );

        engine.shutdown().await;
    }

    /// Test 1: snapshot matches the live config exactly (no reload in
    /// between) — commit installs.
    #[tokio::test]
    async fn commit_installs_when_config_unchanged() {
        let engine = Arc::new(Engine::new(mock_stdio_config("foo", "echo")));
        let snapshot = engine
            .config()
            .servers
            .get("foo")
            .expect("foo configured")
            .clone();

        let upstream = engine
            .server_manager()
            .start_server("foo", &snapshot)
            .await
            .expect("mock upstream connects");

        let outcome = engine.commit_replacement("foo", &snapshot, upstream).await;
        assert!(matches!(outcome, ReplaceOutcome::Committed));
        assert!(
            engine.server_manager().get_upstream("foo").is_some(),
            "committed upstream should be installed"
        );

        engine.shutdown().await;
    }

    /// Test 2 (the flipped step-2 demonstration): same setup as
    /// `raw_replace_server_resurrects_removed_server`, but the stale tail now
    /// goes through `commit_replacement` — the map must NOT contain "foo".
    #[tokio::test]
    async fn commit_discards_when_server_removed() {
        let engine = Arc::new(Engine::new(mock_stdio_config("foo", "echo")));
        let snapshot = engine
            .config()
            .servers
            .get("foo")
            .expect("foo configured")
            .clone();

        let upstream = engine
            .server_manager()
            .start_server("foo", &snapshot)
            .await
            .expect("mock upstream connects");

        engine
            .reload_config(Config::default())
            .await
            .expect("reload succeeds");

        let outcome = engine.commit_replacement("foo", &snapshot, upstream).await;
        assert!(matches!(outcome, ReplaceOutcome::StaleDiscarded));
        assert!(
            engine.server_manager().get_upstream("foo").is_none(),
            "removed server must not be resurrected by a stale commit"
        );

        engine.shutdown().await;
    }

    /// Test 3: reload materially changes "foo" (different `args`, a field
    /// `server_config_changed` covers) — reload restarts it with the new
    /// config. The stale commit must discard and must NOT disturb the
    /// reload-installed instance.
    #[tokio::test]
    async fn commit_discards_on_material_change() {
        let engine = Arc::new(Engine::new(mock_stdio_config("foo", "echo")));
        let snapshot = engine
            .config()
            .servers
            .get("foo")
            .expect("foo configured")
            .clone();

        let upstream = engine
            .server_manager()
            .start_server("foo", &snapshot)
            .await
            .expect("mock upstream connects");

        let mut changed_config = Config::default();
        changed_config
            .servers
            .insert("foo".to_string(), mock_stdio_server_config("echo,greet", 1));
        let report = engine
            .reload_config(changed_config)
            .await
            .expect("reload succeeds");
        assert!(
            report.errors.is_empty(),
            "reload should start the changed server cleanly: {:?}",
            report.errors
        );
        assert!(report.changed.contains(&"foo".to_string()));

        let reload_installed = engine
            .server_manager()
            .get_upstream("foo")
            .expect("reload should have installed its own replacement");

        let outcome = engine.commit_replacement("foo", &snapshot, upstream).await;
        assert!(matches!(outcome, ReplaceOutcome::StaleDiscarded));

        let still_installed = engine
            .server_manager()
            .get_upstream("foo")
            .expect("reload-installed instance must remain");
        assert!(
            Arc::ptr_eq(&reload_installed, &still_installed),
            "a stale commit must not disturb the reload-installed upstream"
        );

        engine.shutdown().await;
    }

    /// Test 4 (the whole-struct-equality regression test): reload changes
    /// ONLY `max_concurrent` — a field `server_config_changed` deliberately
    /// omits — so reload does not touch "foo" at all. The commit must still
    /// install: under whole-struct equality this would incorrectly discard a
    /// successful reconnection and strand the server down.
    #[tokio::test]
    async fn commit_installs_on_non_material_change() {
        let engine = Arc::new(Engine::new(mock_stdio_config("foo", "echo")));
        let snapshot = engine
            .config()
            .servers
            .get("foo")
            .expect("foo configured")
            .clone();

        let upstream = engine
            .server_manager()
            .start_server("foo", &snapshot)
            .await
            .expect("mock upstream connects");

        let mut unchanged_config = Config::default();
        unchanged_config
            .servers
            .insert("foo".to_string(), mock_stdio_server_config("echo", 7));
        let report = engine
            .reload_config(unchanged_config)
            .await
            .expect("reload succeeds");
        assert!(report.unchanged.contains(&"foo".to_string()));
        assert!(
            engine.server_manager().get_upstream("foo").is_none(),
            "reload must not have started anything for an unchanged server"
        );

        let outcome = engine.commit_replacement("foo", &snapshot, upstream).await;
        assert!(
            matches!(outcome, ReplaceOutcome::Committed),
            "non-material config drift must not discard a successful reconnect \
             (regression test for the whole-struct-equality bug)"
        );
        assert!(engine.server_manager().get_upstream("foo").is_some());

        engine.shutdown().await;
    }

    /// Test 6: drives `do_reconnect` (via the public `reconnect_server` entry
    /// point) end to end. "foo" is configured to hang past its 1s connect
    /// timeout — every dial reliably fails with a retryable "timed out"
    /// error, first appending a marker to `dial_log` so the test can count
    /// real dial attempts deterministically (a wall-clock assertion would be
    /// timing-sensitive; a file is not). A concurrent reload removes "foo"
    /// while attempt 1 is still in flight — well before the retry-loop's
    /// early-exit check runs at the top of attempt 2 — so the loop must
    /// abandon without dialing again.
    ///
    /// Seam note: this works because `do_reconnect`'s retry loop has an
    /// explicit synchronization point (the inter-attempt sleep) and only
    /// needs attempt 1 to *fail*, which a command that hangs past
    /// `timeout_secs` gives deterministically. `restart_server` (test 5) has
    /// no retry loop — its only window is the single in-flight connect, which
    /// would need to *succeed* after the interleaving to reach the commit
    /// call at all; that requires a mock upstream that starts failing then
    /// starts succeeding on a fixed config, which no existing fixture
    /// provides. Per the plan, test 5 (and test 7, which has the same
    /// fail-then-succeed requirement) are documented here as not driveable
    /// without building new mock-server machinery, rather than built.
    #[tokio::test]
    async fn reconnect_abandons_between_retries_when_server_removed() {
        let dial_log = tempfile::NamedTempFile::new().expect("create dial log");
        let dial_log_path = dial_log.path().to_string_lossy().to_string();

        let mut config = Config::default();
        config.servers.insert(
            "foo".to_string(),
            crate::config::ServerConfig {
                command: Some("sh".to_string()),
                args: vec![
                    "-c".to_string(),
                    format!("echo dial >> {dial_log_path}; sleep 5"),
                ],
                env: HashMap::new(),
                enabled: true,
                transport: TransportType::Stdio,
                url: None,
                auth_token: None,
                auth: None,
                oauth_client_id: None,
                oauth_scopes: None,
                timeout_secs: 1,
                call_timeout_secs: 300,
                max_concurrent: 1,
                health_check_interval_secs: 60,
                circuit_breaker_enabled: true,
                enrichment: false,
                tool_renames: HashMap::new(),
                tool_groups: Vec::new(),

                sandbox: None,
            },
        );

        let engine = Arc::new(Engine::new(config));

        // Remove "foo" partway through attempt 1's ~1s connect timeout —
        // comfortably before the early-exit check at the top of attempt 2
        // (which runs only after attempt 1 fails AND the 100ms inter-attempt
        // sleep elapses, i.e. around the 1.1s mark).
        let reload_engine = Arc::clone(&engine);
        let reload_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            reload_engine
                .reload_config(Config::default())
                .await
                .expect("reload succeeds");
        });

        let result = engine.reconnect_server("foo").await;
        reload_task.await.expect("reload task panicked");

        assert!(
            result.is_ok(),
            "abandoned reconnect should return Ok — reload already established \
             the desired reality: {result:?}"
        );
        assert!(
            engine.server_manager().get_upstream("foo").is_none(),
            "removed server must not be resurrected"
        );

        let dial_count = std::fs::read_to_string(&dial_log_path)
            .expect("read dial log")
            .lines()
            .count();
        assert_eq!(
            dial_count, 1,
            "retry loop must abandon before dialing again once the server is gone"
        );

        engine.shutdown().await;
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
        let engine = Arc::new(Engine::new(test_config()));
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
        let engine = Arc::new(Engine::new(test_config()));
        let result = engine.restart_server("nonexistent").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown server"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn engine_restart_respawns_refresh_loop_for_oauth_server() {
        let engine = Arc::new(Engine::new(oauth_mock_stdio_config()));
        engine.start().await.expect("engine start");

        assert_eq!(
            engine.current_refresh_task_generation("oauth-mock"),
            Some(1)
        );

        engine
            .restart_server("oauth-mock")
            .await
            .expect("restart server");

        assert_eq!(
            engine.current_refresh_task_generation("oauth-mock"),
            Some(2)
        );

        engine.shutdown().await;
    }

    #[test]
    fn publish_token_refresh_exchanged_emits_protocol_notification() {
        let engine = Engine::new(test_config());
        let mut rx = engine.tool_router().subscribe_notifications();

        publish_token_refresh_exchanged(&engine, "github");

        match rx.try_recv() {
            Ok(crate::notifications::ProtocolNotification::TokenRefreshExchanged { server_id }) => {
                assert_eq!(server_id.as_ref(), "github");
            }
            other => panic!("expected TokenRefreshExchanged, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn engine_set_server_enabled_unknown() {
        let engine = Arc::new(Engine::new(test_config()));
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

                sandbox: None,
            },
        );

        let engine = Arc::new(Engine::new(config));
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

                sandbox: None,
            },
        );

        let engine = Arc::new(Engine::new(config));
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

    #[tokio::test]
    async fn reload_preserves_failed_server_visibility_for_added_server() {
        use crate::config::ServerConfig;
        use std::collections::HashMap;

        let engine = Arc::new(Engine::new(Config::default()));
        engine.start().await.expect("engine start");

        let mut new_config = Config::default();
        new_config.servers.insert(
            "broken".to_string(),
            ServerConfig {
                command: Some("definitely-not-a-real-command".to_string()),
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

                sandbox: None,
            },
        );

        let report = engine
            .reload_config(new_config)
            .await
            .expect("reload report");
        assert!(
            !report.errors.is_empty(),
            "expected reload failure for invalid command"
        );

        let statuses = engine.server_statuses();
        let broken = statuses
            .iter()
            .find(|status| status.server_id == "broken")
            .expect("failed server should remain visible");
        assert_eq!(broken.health, ServerHealth::Failed);
        assert_eq!(broken.tool_count, 0);

        engine.shutdown().await;
    }

    #[test]
    fn recovery_task_claim_is_deduplicated_until_released() {
        let engine = Engine::new(Config::default());

        let first = engine
            .try_claim_recovery_task("github")
            .expect("first claim should succeed");
        assert!(
            engine.try_claim_recovery_task("github").is_none(),
            "second claim should be suppressed while recovery is active"
        );

        first.store(false, Ordering::SeqCst);
        assert!(
            engine.try_claim_recovery_task("github").is_some(),
            "claim should succeed again after the active recovery releases"
        );
    }

    /// The supervision glue (item 2b): a sustained-degraded server becomes due,
    /// noting a restart bumps the metric + the backoff counter so the next tick is
    /// no longer due, and recovery to healthy resets the backoff.
    #[test]
    fn supervision_due_notes_restart_and_resets_on_recovery() {
        use crate::types::{HealthState, ServerHealth};

        let engine = Engine::new(Config::default()); // supervision enabled by default
        let sm = engine.server_manager();

        // Server has been failing health checks past the default threshold (5).
        sm.health.insert(
            "imessage".to_string(),
            HealthState {
                health: ServerHealth::Degraded,
                consecutive_failures: 6,
            },
        );

        assert!(
            engine.supervision_due("imessage"),
            "sustained-degraded server should be due for a supervised restart"
        );

        engine.note_supervised_restart("imessage");
        // The restart is recorded in the per-upstream metrics surfaced to status.
        let snap = sm
            .metrics_snapshot("imessage")
            .expect("metrics recorded after restart");
        assert_eq!(snap.restart_count, 1);
        assert!(snap.last_restart_epoch_secs.is_some());

        // Just restarted -> the inter-episode backoff (>= 60s) suppresses the next.
        assert!(
            !engine.supervision_due("imessage"),
            "should not immediately re-restart within the backoff window"
        );

        // A brief post-restart healthy blip must NOT be treated as settled (the
        // restart clock is seconds old, far short of max_restart_interval), so a
        // flapping upstream keeps its escalating backoff instead of resetting to
        // the floor — the storm-prevention fix.
        assert!(
            !engine.supervision_settled("imessage"),
            "a just-restarted server is not yet stably recovered"
        );

        // A healthy server is never *due* regardless of restart history.
        sm.health.insert(
            "imessage".to_string(),
            HealthState {
                health: ServerHealth::Healthy,
                consecutive_failures: 0,
            },
        );
        assert!(!engine.supervision_due("imessage"));

        // An upstream that has never been restarted is trivially settled.
        assert!(engine.supervision_settled("never-restarted"));
    }

    // Seam semantics recorded for this suite (plan 005):
    // - `supervision_due` is a pure read of health streak + circuit state +
    //   restart history + `SupervisionConfig`; see `Engine::supervision_due`.
    // - "settled" means enough real wall-clock time (`max_restart_interval_secs`)
    //   has elapsed since the last supervised restart -- a healthy health-state
    //   alone is NOT settled. This is the storm-vector fix from PR #67: a
    //   flapping upstream that bounces healthy immediately after a restart must
    //   not have its backoff reset, or it could be restarted every cycle forever.
    // - The inter-episode backoff escalates as
    //   `min(min_restart_interval_secs * 2^attempts, max_restart_interval_secs)`;
    //   pure math is covered by `supervision_backoff_grows_and_caps` in
    //   `config/mod.rs`. `grow_supervision_backoff` / `note_supervised_restart`
    //   increment the `attempts` counter that feeds that math.
    // - `reset_supervision` clears the `attempts` counter entirely (returns the
    //   next required wait to the floor, `min_restart_interval_secs`).

    /// The monitor loop's reset gate (`health.rs`) only calls `reset_supervision`
    /// when `health == Healthy && !circuit_open && supervision_settled(..)`. This
    /// test drives that exact three-part gate directly: a brief post-restart
    /// healthy blip (settled still false, since almost no time has passed) must
    /// leave the escalated backoff attempts counter untouched. This is the
    /// single highest-value regression check for the PR #67 storm-vector fix.
    #[test]
    fn healthy_blip_does_not_reset_supervision_backoff() {
        use crate::types::{HealthState, ServerHealth};

        let engine = Engine::new(Config::default());
        let sm = engine.server_manager();

        sm.health.insert(
            "imessage".to_string(),
            HealthState {
                health: ServerHealth::Degraded,
                consecutive_failures: 6,
            },
        );
        assert!(engine.supervision_due("imessage"));

        engine.note_supervised_restart("imessage");
        assert_eq!(
            *engine.supervision_attempts.get("imessage").unwrap(),
            1,
            "one supervised restart should bump the backoff attempt counter once"
        );

        // The upstream immediately reports healthy again (a blip), but almost no
        // wall-clock time has passed since the restart.
        sm.health.insert(
            "imessage".to_string(),
            HealthState {
                health: ServerHealth::Healthy,
                consecutive_failures: 0,
            },
        );
        assert!(!sm.circuit_open("imessage"));
        assert!(
            !engine.supervision_settled("imessage"),
            "a just-restarted server is not yet stably recovered"
        );

        // Replicate the monitor loop's exact reset gate (health.rs) rather than
        // running the real loop: reset only fires when all three conditions hold.
        let (health_now, _) = sm.health_streak("imessage");
        if health_now == ServerHealth::Healthy
            && !sm.circuit_open("imessage")
            && engine.supervision_settled("imessage")
        {
            engine.reset_supervision("imessage");
        }

        assert_eq!(
            *engine.supervision_attempts.get("imessage").unwrap(),
            1,
            "a healthy blip that isn't settled must NOT reset the escalated backoff"
        );
    }

    /// Once the upstream has been healthy AND the settle window has genuinely
    /// elapsed, the reset gate fires and `reset_supervision` clears the attempt
    /// counter back to the floor. Uses a tiny `max_restart_interval_secs` so the
    /// test only needs to wait ~1 real second rather than the default 600s --
    /// `supervision_settled` reads real `SystemTime`, so paused-tokio-time tricks
    /// do not apply here (see plan 005 timing note).
    #[test]
    fn stable_recovery_resets_supervision_backoff() {
        use crate::types::{HealthState, ServerHealth};
        use std::thread::sleep;
        use std::time::Duration;

        let mut config = Config::default();
        config.supervision.min_restart_interval_secs = 1;
        config.supervision.max_restart_interval_secs = 1;
        let engine = Engine::new(config);
        let sm = engine.server_manager();

        sm.health.insert(
            "imessage".to_string(),
            HealthState {
                health: ServerHealth::Degraded,
                consecutive_failures: 6,
            },
        );
        assert!(engine.supervision_due("imessage"));
        engine.note_supervised_restart("imessage");
        assert_eq!(*engine.supervision_attempts.get("imessage").unwrap(), 1);

        sm.health.insert(
            "imessage".to_string(),
            HealthState {
                health: ServerHealth::Healthy,
                consecutive_failures: 0,
            },
        );

        // Let the (1s) settle window genuinely elapse.
        sleep(Duration::from_millis(1200));
        assert!(
            engine.supervision_settled("imessage"),
            "the settle window has elapsed, so the server is stably recovered"
        );

        let (health_now, _) = sm.health_streak("imessage");
        if health_now == ServerHealth::Healthy
            && !sm.circuit_open("imessage")
            && engine.supervision_settled("imessage")
        {
            engine.reset_supervision("imessage");
        }

        assert!(
            engine.supervision_attempts.get("imessage").is_none(),
            "a stably-recovered server should have its backoff attempts cleared"
        );
    }

    /// `grow_supervision_backoff` / `note_supervised_restart` plumbing: repeated
    /// supervised restarts (without an intervening reset) accumulate the attempt
    /// counter that feeds the exponential backoff math (verified in isolation by
    /// `supervision_backoff_grows_and_caps` in `config/mod.rs`).
    #[test]
    fn repeated_supervised_restarts_accumulate_backoff_attempts() {
        let engine = Engine::new(Config::default());

        engine.note_supervised_restart("imessage");
        engine.note_supervised_restart("imessage");
        engine.note_supervised_restart("imessage");

        assert_eq!(
            *engine.supervision_attempts.get("imessage").unwrap(),
            3,
            "three supervised restarts without a reset should leave attempts at 3"
        );
    }

    /// `SupervisionConfig::enabled = false` must make `supervision_due` never
    /// true, even for a badly-degraded upstream with an open circuit. The pure
    /// math is covered by `supervision_disabled_never_restarts` in
    /// `config/mod.rs`; this test verifies the engine wiring (health streak +
    /// circuit lookup) also respects it end-to-end.
    #[test]
    fn disabled_supervision_never_becomes_due_via_engine() {
        use crate::types::{HealthState, ServerHealth};

        let mut config = Config::default();
        config.supervision.enabled = false;
        let engine = Engine::new(config);
        let sm = engine.server_manager();

        sm.health.insert(
            "imessage".to_string(),
            HealthState {
                health: ServerHealth::Failed,
                consecutive_failures: 100,
            },
        );

        assert!(!engine.supervision_due("imessage"));
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

                sandbox: None,
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
                    if iterations.is_multiple_of(100) {
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

        let err = anyhow::anyhow!(
            "Streamable HTTP error: Error POSTing to endpoint: {{\"error_code\": \"TOO_MANY_REQUESTS\", \"details\": {{\"blocked_seconds\": 600}}}}"
        );
        assert!(!is_retryable_reconnect_error(&err));

        let err = anyhow::anyhow!("Streamable HTTP error: Failed to open SSE stream: Conflict");
        assert!(!is_retryable_reconnect_error(&err));

        let err = anyhow::anyhow!(
            "HTTP 503 Service Unavailable: {{\"jsonrpc\":\"2.0\",\"error\":{{\"message\":\"Too many active sessions. Try again later.\"}}}}"
        );
        assert!(!is_retryable_reconnect_error(&err));
    }

    #[test]
    fn auth_reconnect_error_uses_shared_classifier() {
        let err = anyhow::anyhow!("request failed while calling http://127.0.0.1:4018/callback");
        assert!(!is_auth_reconnect_error(&err));

        let err = anyhow::anyhow!("downstream returned 401 unauthorized");
        assert!(is_auth_reconnect_error(&err));
    }
}
