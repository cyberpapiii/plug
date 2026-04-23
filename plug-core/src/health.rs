//! Health-check background tasks for upstream MCP servers.
//!
//! Spawns one tokio task per server that periodically pings the upstream
//! and updates the `HealthState` in `ServerManager.health` (DashMap).
//! On state transitions, triggers `ToolRouter::refresh_tools()` so that
//! failed servers' tools are removed from the cache.

use std::sync::Arc;
use std::time::Duration;

use backon::{ExponentialBuilder, Retryable as _};
use rand::Rng as _;
use tokio::sync::broadcast;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::Config;
use crate::engine::{Engine, EngineEvent};
use crate::proxy::ToolRouter;
use crate::server::ServerManager;
use crate::types::ServerHealth;

/// Spawn health-check background tasks for all enabled servers.
///
/// Each server gets its own tokio task with a staggered start (random jitter)
/// to avoid thundering-herd pings. Tasks run until `cancel` is triggered.
/// Uses `tracker.spawn()` for ordered shutdown via `TaskTracker::wait()`.
///
/// When a server transitions to `Failed`, spawns a proactive recovery task
/// that attempts reconnection with exponential backoff via `backon`.
pub fn spawn_health_checks(
    server_manager: Arc<ServerManager>,
    router: Arc<ToolRouter>,
    engine: Arc<Engine>,
    event_tx: broadcast::Sender<EngineEvent>,
    cancel: CancellationToken,
    config: &Config,
    tracker: &TaskTracker,
) {
    for (name, sc) in &config.servers {
        if !sc.enabled {
            continue;
        }
        spawn_health_check(
            server_manager.clone(),
            router.clone(),
            engine.clone(),
            event_tx.clone(),
            cancel.clone(),
            name.clone(),
            sc.health_check_interval_secs,
            tracker,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_health_check(
    server_manager: Arc<ServerManager>,
    router: Arc<ToolRouter>,
    engine: Arc<Engine>,
    event_tx: broadcast::Sender<EngineEvent>,
    cancel: CancellationToken,
    name: String,
    health_check_interval_secs: u64,
    tracker: &TaskTracker,
) {
    let generation = engine.next_health_task_generation(&name);
    let interval = Duration::from_secs(health_check_interval_secs);
    let tracker_clone = tracker.clone();

    tracker.spawn(async move {
        let jitter = Duration::from_millis(rand::thread_rng().gen_range(0..10_000));
        tokio::time::sleep(jitter).await;

        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        tick.tick().await;

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    tracing::debug!(server = %name, "health check task shutting down");
                    break;
                }
                _ = tick.tick() => {
                    if engine.current_health_task_generation(&name) != Some(generation) {
                        tracing::debug!(server = %name, "health check generation superseded");
                        break;
                    }

                    let current_config = {
                        let cfg = engine.config();
                        cfg.servers.get(&name).cloned()
                    };
                    let Some(current_config) = current_config else {
                        engine.clear_health_task_generation(&name);
                        break;
                    };
                    if !current_config.enabled || current_config.health_check_interval_secs != health_check_interval_secs {
                        break;
                    }

                    let is_auth_required = server_manager
                        .health
                        .get(&name)
                        .is_some_and(|entry| entry.health == ServerHealth::AuthRequired);
                    if is_auth_required {
                        tracing::debug!(server = %name, "skipping health check for AuthRequired server");
                        continue;
                    }

                    let missing_upstream = server_manager.get_upstream(&name).is_none();
                    let startup_failed = server_manager
                        .health
                        .get(&name)
                        .is_some_and(|entry| entry.health == ServerHealth::Failed);

                    if missing_upstream && startup_failed {
                        spawn_proactive_recovery_once(&engine, &name, cancel.clone(), &tracker_clone);
                        continue;
                    }

                    let result = health_check_server(&server_manager, &name).await;
                    if let Some((old, new)) = result {
                        tracing::info!(server = %name, ?old, ?new, "health state changed, refreshing tools");
                        router.schedule_tool_list_changed_refresh();
                        let _ = event_tx.send(EngineEvent::ServerHealthChanged {
                            server_id: Arc::from(name.as_str()),
                            old,
                            new,
                        });

                        if new == ServerHealth::Failed {
                            spawn_proactive_recovery_once(&engine, &name, cancel.clone(), &tracker_clone);
                        }
                    }
                }
            }
        }
    });
}

fn spawn_proactive_recovery_once(
    engine: &Arc<Engine>,
    server_name: &str,
    cancel: CancellationToken,
    tracker: &TaskTracker,
) {
    let Some(flag) = engine.try_claim_recovery_task(server_name) else {
        tracing::debug!(server = %server_name, "proactive recovery task already active");
        return;
    };

    let engine = Arc::clone(engine);
    let server_name = server_name.to_string();
    tracker.spawn(async move {
        struct RecoveryGuard(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for RecoveryGuard {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let _guard = RecoveryGuard(flag);
        spawn_proactive_recovery(&engine, &server_name, cancel).await;
    });
}

/// Attempt proactive recovery of a failed server with exponential backoff.
///
/// Uses `backon` to retry `Engine::reconnect_server()` up to 5 times
/// with delays from 1s to 60s. On success, the server is replaced and
/// health/circuit breaker state is reset via `replace_server()`.
async fn spawn_proactive_recovery(engine: &Engine, server_name: &str, cancel: CancellationToken) {
    tracing::info!(server = %server_name, "starting proactive recovery");

    let reconnect = || async { engine.reconnect_server(server_name).await };

    let recovery = reconnect
        .retry(
            ExponentialBuilder::default()
                .with_min_delay(Duration::from_secs(1))
                .with_max_delay(Duration::from_secs(60))
                .with_max_times(5)
                .with_jitter(),
        )
        .notify(|err, dur| {
            tracing::warn!(
                server = %server_name,
                error = %err,
                retry_in_ms = dur.as_millis(),
                "proactive recovery attempt failed, will retry"
            );
        });

    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            tracing::debug!(server = %server_name, "proactive recovery cancelled during shutdown");
        }
        result = recovery => {
            match result {
                Ok(()) => {
                    tracing::info!(server = %server_name, "proactive recovery succeeded");
                }
                Err(e) => {
                    tracing::error!(
                        server = %server_name,
                        error = %e,
                        "proactive recovery exhausted (5 attempts), will retry on next health cycle"
                    );
                }
            }
        }
    }
}

/// Ping a single upstream server and update its health state.
///
/// Returns `Some((old, new))` if the health state changed (caller should
/// refresh tools and emit event). Returns `None` if unchanged.
async fn health_check_server(
    mgr: &ServerManager,
    name: &str,
) -> Option<(ServerHealth, ServerHealth)> {
    let upstream = match mgr.get_upstream(name) {
        Some(u) => u,
        None => return None,
    };

    // Use the first list_tools page as a lightweight liveness probe rather than
    // enumerating the full merged surface on every health cycle.
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        upstream
            .client
            .peer()
            .list_tools(None)
            .await
            .map_err(|e| anyhow::anyhow!("health probe failed: {e}"))
    })
    .await;

    let success = matches!(result, Ok(Ok(_)));

    // Clone-and-drop pattern: extract state, drop guard, then use data.
    let mut entry = mgr.health.entry(name.to_string()).or_default();
    let old_health = entry.health;
    let changed = if success {
        entry.record_success()
    } else {
        entry.record_failure()
    };
    let new_health = entry.health;
    drop(entry); // Drop DashMap guard before any .await

    if changed {
        if success {
            tracing::info!(server = %name, health = ?new_health, "health improved");
        } else {
            tracing::warn!(server = %name, health = ?new_health, "health degraded");
        }
        Some((old_health, new_health))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{HealthState, ServerHealth};

    #[test]
    fn health_state_transitions_to_degraded() {
        let mut state = HealthState::new();
        assert_eq!(state.health, ServerHealth::Healthy);

        // 2 failures: still healthy
        assert!(!state.record_failure());
        assert!(!state.record_failure());
        assert_eq!(state.health, ServerHealth::Healthy);

        // 3rd failure: transitions to degraded
        assert!(state.record_failure());
        assert_eq!(state.health, ServerHealth::Degraded);
    }

    #[test]
    fn health_state_transitions_to_failed() {
        let mut state = HealthState::new();

        // 3 failures → Degraded
        for _ in 0..3 {
            state.record_failure();
        }
        assert_eq!(state.health, ServerHealth::Degraded);

        // 3 more failures → Failed (6 total)
        for _ in 0..2 {
            assert!(!state.record_failure());
        }
        assert!(state.record_failure()); // 6th
        assert_eq!(state.health, ServerHealth::Failed);
    }

    #[test]
    fn health_state_recovers_on_success() {
        let mut state = HealthState::new();

        // Drive to Failed
        for _ in 0..6 {
            state.record_failure();
        }
        assert_eq!(state.health, ServerHealth::Failed);

        // 1 success → Degraded
        assert!(state.record_success());
        assert_eq!(state.health, ServerHealth::Degraded);

        // 1 more success → Healthy
        assert!(state.record_success());
        assert_eq!(state.health, ServerHealth::Healthy);
    }

    #[test]
    fn success_resets_failure_count() {
        let mut state = HealthState::new();

        state.record_failure();
        state.record_failure();
        // 1 success resets count
        state.record_success();

        // Need 3 more failures to reach Degraded
        assert!(!state.record_failure());
        assert!(!state.record_failure());
        assert!(state.record_failure()); // 3rd since reset
        assert_eq!(state.health, ServerHealth::Degraded);
    }
}
