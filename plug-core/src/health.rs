//! Health-check background tasks for upstream MCP servers.
//!
//! Spawns one tokio task per server that periodically pings the upstream
//! and updates the `HealthState` in `ServerManager.health` (DashMap).
//! On state transitions, triggers `ToolRouter::refresh_tools()` so that
//! failed servers' tools are removed from the cache.

use std::sync::Arc;
use std::time::Duration;

use rand::Rng as _;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::proxy::ToolRouter;
use crate::server::ServerManager;

/// Spawn health-check background tasks for all enabled servers.
///
/// Each server gets its own tokio task with a staggered start (random jitter)
/// to avoid thundering-herd pings. Tasks run until `cancel` is triggered.
pub fn spawn_health_checks(
    server_manager: Arc<ServerManager>,
    router: Arc<ToolRouter>,
    cancel: CancellationToken,
    config: &Config,
) {
    for (name, sc) in &config.servers {
        if !sc.enabled {
            continue;
        }

        let name = name.clone();
        let interval = Duration::from_secs(sc.health_check_interval_secs);
        let mgr = server_manager.clone();
        let router = router.clone();
        let cancel = cancel.clone();

        tokio::spawn(async move {
            // Stagger start with random 0-10s jitter to avoid thundering herd
            let jitter = Duration::from_millis(rand::thread_rng().gen_range(0..10_000));
            tokio::time::sleep(jitter).await;

            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

            // Skip the first immediate tick (server just started)
            tick.tick().await;

            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        tracing::debug!(server = %name, "health check task shutting down");
                        break;
                    }
                    _ = tick.tick() => {
                        let changed = health_check_server(&mgr, &name).await;
                        if changed {
                            tracing::info!(server = %name, "health state changed, refreshing tools");
                            router.refresh_tools().await;
                        }
                    }
                }
            }
        });
    }
}

/// Ping a single upstream server and update its health state.
///
/// Returns `true` if the health state changed (caller should refresh tools).
async fn health_check_server(mgr: &ServerManager, name: &str) -> bool {
    let upstream = match mgr.get_upstream(name) {
        Some(u) => u,
        None => return false,
    };

    // Use list_tools as a lightweight health probe (universal across MCP servers).
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        upstream
            .client
            .peer()
            .list_all_tools()
            .await
            .map_err(|e| anyhow::anyhow!("health probe failed: {e}"))
    })
    .await;

    let success = matches!(result, Ok(Ok(_)));

    // Update health state in DashMap
    let mut entry = mgr.health.entry(name.to_string()).or_default();
    if success {
        let changed = entry.record_success();
        if changed {
            tracing::info!(
                server = %name,
                health = ?entry.health,
                "health improved"
            );
        }
        changed
    } else {
        let changed = entry.record_failure();
        if changed {
            tracing::warn!(
                server = %name,
                health = ?entry.health,
                consecutive_failures = entry.consecutive_failures,
                "health degraded"
            );
        }
        changed
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
