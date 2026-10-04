//! Health-check background tasks for upstream MCP servers.
//!
//! Spawns one tokio task per server that periodically pings the upstream
//! and updates the `HealthState` in `ServerManager.health` (DashMap).
//! On state transitions, triggers `ToolRouter::refresh_tools()` so that
//! failed servers' tools are removed from the cache.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::Config;
use crate::engine::Engine;
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
/// that reconnects it with exponential backoff until it is back.
pub fn spawn_health_checks(
    server_manager: Arc<ServerManager>,
    router: Arc<ToolRouter>,
    engine: Arc<Engine>,
    cancel: CancellationToken,
    config: &Config,
    tracker: &TaskTracker,
) {
    for (name, sc) in &config.servers {
        if !sc.enabled {
            continue;
        }
        // A server whose start already settled has a live task. Spawning a
        // second one would supersede it and hand the server a fresh full
        // interval to wait out, which is exactly what this call is here to
        // avoid for the servers that never settled.
        if engine.current_health_task_generation(name).is_some() {
            continue;
        }
        spawn_health_check(
            server_manager.clone(),
            router.clone(),
            engine.clone(),
            cancel.clone(),
            name.clone(),
            sc.health_check_interval_secs,
            tracker,
        );
    }
}

pub fn spawn_health_check(
    server_manager: Arc<ServerManager>,
    router: Arc<ToolRouter>,
    engine: Arc<Engine>,
    cancel: CancellationToken,
    name: String,
    health_check_interval_secs: u64,
    tracker: &TaskTracker,
) {
    let generation = engine.next_health_task_generation(&name);
    let interval = Duration::from_secs(health_check_interval_secs);
    let tracker_clone = tracker.clone();

    tracker.spawn(async move {
        // A server that is already missing an upstream is known-bad before the
        // loop begins -- typically a local upstream still binding its port
        // after a reboot -- so it skips both the stagger and the consumed first
        // tick and goes straight to recovery. Only servers that started healthy
        // were just contacted and need neither an immediate ping nor a herd.
        let started_healthy = server_manager.get_upstream(&name).is_some();
        if started_healthy {
            let jitter = Duration::from_millis(rand::random_range(0..10_000));
            tokio::time::sleep(jitter).await;
        }

        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // `interval` yields its first tick immediately. For a server that
        // started healthy that tick would be a redundant ping, so it is
        // consumed here. Consuming it unconditionally used to leave a failed
        // server down for a whole interval after login even once it was ready.
        if started_healthy {
            tick.tick().await;
        }

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
                        trigger_recovery(&engine, &name, cancel.clone(), &tracker_clone, false);
                        continue;
                    }

                    let result = health_check_server(&server_manager, &name).await;
                    if let Some((old, new)) = result {
                        tracing::info!(server = %name, ?old, ?new, "health state changed, refreshing tools");
                        router.schedule_tool_list_changed_refresh();

                        if new == ServerHealth::Failed {
                            // Always-on Failed-recovery (not gated by the
                            // supervision flag); stamps the restart clock so a
                            // supervised restart isn't fired again immediately.
                            trigger_recovery(&engine, &name, cancel.clone(), &tracker_clone, false);
                        }
                    }

                    // Active upstream supervision (item 2b / R10): restart an
                    // upstream that stays degraded past the threshold or whose
                    // circuit is open — covering the "connected but failing real
                    // calls" case (e.g. the iMessage continuation leak) that the
                    // Failed-recovery path above does not reach. Bounded by the
                    // SupervisionConfig backoff. The escalating backoff is cleared
                    // only once the upstream has *stably* recovered (settled), so a
                    // flapping upstream's backoff keeps escalating rather than
                    // resetting to the floor on every brief healthy blip.
                    let (health_now, _) = server_manager.health_streak(&name);
                    if health_now == ServerHealth::Healthy
                        && !server_manager.circuit_open(&name)
                        && engine.supervision_settled(&name)
                    {
                        engine.reset_supervision(&name);
                    } else if engine.supervision_due(&name)
                        && trigger_recovery(&engine, &name, cancel.clone(), &tracker_clone, true)
                    {
                        tracing::warn!(
                            server = %name,
                            "item 2b: supervising restart of degraded upstream"
                        );
                    }
                }
            }
        }
    });
}

/// Start a recovery episode and record it. Returns `true` if a new episode was
/// started (deduped: at most one per server). When `supervised`, the per-server
/// escalating backoff counter grows; for every started episode the restart clock
/// and `restart_count` metric are stamped so the backoff clock is consistent
/// across crash/Failed and supervised recoveries.
fn trigger_recovery(
    engine: &Arc<Engine>,
    server_name: &str,
    cancel: CancellationToken,
    tracker: &TaskTracker,
    supervised: bool,
) -> bool {
    if !spawn_proactive_recovery_once(engine, server_name, cancel, tracker) {
        return false;
    }
    if supervised {
        engine.note_supervised_restart(server_name);
    } else {
        engine.note_restart(server_name);
    }
    true
}

/// Spawn a backoff-bounded recovery episode for `server_name`, deduplicated so at
/// most one is active per server. Returns `true` if this call started a new
/// episode, `false` if one was already running (so the supervisor only counts a
/// restart when it actually initiates one).
fn spawn_proactive_recovery_once(
    engine: &Arc<Engine>,
    server_name: &str,
    cancel: CancellationToken,
    tracker: &TaskTracker,
) -> bool {
    let Some(flag) = engine.try_claim_recovery_task(server_name) else {
        tracing::debug!(server = %server_name, "proactive recovery task already active");
        return false;
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
    true
}

/// Delay after the first failed recovery attempt. It doubles per failure.
const RECOVERY_MIN_DELAY: Duration = Duration::from_secs(1);
/// A server that stays down is retried this often for as long as it is down.
const RECOVERY_MAX_DELAY: Duration = Duration::from_secs(60);

/// The backoff of one recovery episode. An episode lasts until the server is
/// back, so the delay keeps growing across failures instead of restarting at
/// the floor every health interval.
#[derive(Debug, Default)]
struct RecoveryBackoff {
    failures: u32,
}

impl RecoveryBackoff {
    /// Count a failed attempt and return how long to wait before the next.
    fn record_failure(&mut self) -> Duration {
        self.failures += 1;
        Self::delay_after(self.failures)
    }

    /// Whether the last failure is the first to wait the full cap.
    fn just_reached_cap(&self) -> bool {
        self.failures > 1
            && Self::delay_after(self.failures) == RECOVERY_MAX_DELAY
            && Self::delay_after(self.failures - 1) < RECOVERY_MAX_DELAY
    }

    fn delay_after(failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(16);
        RECOVERY_MIN_DELAY
            .saturating_mul(1 << doublings)
            .min(RECOVERY_MAX_DELAY)
    }
}

/// Whether `server_name` still needs the recovery episode that is running for
/// it. Someone else may have fixed it meanwhile (a reload, an operator
/// restart, a reactive reconnect), or it may now need the user instead.
fn recovery_still_needed(engine: &Engine, server_name: &str) -> bool {
    let enabled = engine
        .config()
        .servers
        .get(server_name)
        .is_some_and(|config| config.enabled);
    if !enabled {
        return false;
    }
    let manager = engine.server_manager();
    let health = manager.health.get(server_name).map(|entry| entry.health);
    if health == Some(ServerHealth::AuthRequired) {
        return false;
    }
    manager.get_upstream(server_name).is_none()
        || health != Some(ServerHealth::Healthy)
        || manager.circuit_open(server_name)
}

/// Reconnect a failed server until it is back, with exponential backoff from
/// 1s to a minute. On success the server is replaced and its health and
/// circuit breaker state are reset via `replace_server()`.
///
/// Logs the first failure and the moment the backoff reaches its cap as
/// warnings, every other failure at debug, and the recovery once.
async fn spawn_proactive_recovery(engine: &Engine, server_name: &str, cancel: CancellationToken) {
    tracing::info!(server = %server_name, "starting proactive recovery");
    let started = tokio::time::Instant::now();
    let mut backoff = RecoveryBackoff::default();

    loop {
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                tracing::debug!(server = %server_name, "proactive recovery cancelled during shutdown");
                return;
            }
            result = engine.reconnect_server_once(server_name) => result,
        };

        let error = match result {
            Ok(()) => {
                // Ok without an upstream means the reconnect was abandoned for
                // a reload, or another start owns the server. Either way the
                // next health tick decides whether recovery is still needed.
                if engine.server_manager().get_upstream(server_name).is_some() {
                    tracing::info!(
                        server = %server_name,
                        attempts = backoff.failures + 1,
                        elapsed_secs = started.elapsed().as_secs(),
                        "server recovered"
                    );
                }
                return;
            }
            Err(error) => error,
        };

        let delay = backoff.record_failure();
        let attempts = backoff.failures;
        if attempts == 1 {
            tracing::warn!(
                server = %server_name,
                error = %error,
                retry_in_secs = delay.as_secs(),
                "recovery attempt failed; retrying with backoff"
            );
        } else if backoff.just_reached_cap() {
            tracing::warn!(
                server = %server_name,
                attempts,
                error = %error,
                retry_every_secs = RECOVERY_MAX_DELAY.as_secs(),
                "server still down; retrying at the slowest rate"
            );
        } else {
            tracing::debug!(
                server = %server_name,
                attempts,
                error = %error,
                retry_in_secs = delay.as_secs(),
                "recovery attempt failed"
            );
        }

        // Up to a tenth extra, so servers that failed together spread out.
        let jitter = delay.mul_f64(rand::random_range(0.0..0.1));
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                tracing::debug!(server = %server_name, "proactive recovery cancelled during shutdown");
                return;
            }
            _ = tokio::time::sleep(delay + jitter) => {}
        }

        if !recovery_still_needed(engine, server_name) {
            tracing::debug!(server = %server_name, "recovery no longer needed");
            return;
        }
    }
}

/// Ping a single upstream server and update its health state.
///
/// Returns `Some((old, new))` if the health state changed (caller should
/// refresh tools). Returns `None` if unchanged.
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
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::config::{Config, ServerConfig, TransportType};
    use crate::engine::Engine;
    use crate::types::{HealthState, ServerHealth};

    fn unstartable_server_config(health_check_interval_secs: u64) -> ServerConfig {
        ServerConfig {
            command: Some("/nonexistent/plug-health-test-binary".to_string()),
            args: Vec::new(),
            env: HashMap::new(),
            enabled: true,
            transport: TransportType::Stdio,
            protocol_mode: Default::default(),
            url: None,
            auth_token: None,
            auth: None,
            oauth_client_id: None,
            oauth_scopes: None,
            timeout_secs: 30,
            call_timeout_secs: 300,
            max_concurrent: 1,
            health_check_interval_secs,
            circuit_breaker_enabled: true,
            enrichment: false,
            tool_renames: HashMap::new(),
            tool_groups: Vec::new(),
            sandbox: None,
            spec: None,
            operations: Vec::new(),
            token_in: None,
        }
    }

    /// A server that failed to start must be retried without first waiting out a
    /// whole health-check interval. The common case is a local upstream that is
    /// still binding its port when the daemon starts at login: it is ready
    /// seconds later, but the daemon used to leave it down until the first tick.
    #[tokio::test(start_paused = true)]
    async fn startup_failure_is_retried_before_one_health_interval_elapses() {
        const INTERVAL_SECS: u64 = 600;

        let mut config = Config::default();
        config
            .servers
            .insert("dead".to_string(), unstartable_server_config(INTERVAL_SECS));

        let engine = Arc::new(Engine::new(config));
        engine.start().await.expect("a failed start is not fatal");

        // Well past the health task's start jitter (<= 10s) but far short of the
        // 600s interval, so only an unconsumed first tick can have fired.
        tokio::time::sleep(Duration::from_secs(60)).await;

        let restarts = engine
            .server_manager()
            .metrics_snapshot_or_default("dead")
            .restart_count;
        assert!(
            restarts >= 1,
            "a server that failed to start should be retried within the first \
             health interval, but no recovery episode ran in 60s of a {INTERVAL_SECS}s interval"
        );

        engine.shutdown().await;
    }

    /// The 0-10s start stagger exists to spread pings to servers that are up.
    /// A server that failed at boot has nothing to ping and should go straight
    /// to recovery, not sit out a random share of the stagger first.
    #[tokio::test(start_paused = true)]
    async fn startup_failure_skips_the_health_start_jitter() {
        let mut config = Config::default();
        config
            .servers
            .insert("dead".to_string(), unstartable_server_config(600));

        let engine = Arc::new(Engine::new(config));
        engine.start().await.expect("a failed start is not fatal");

        tokio::time::sleep(Duration::from_millis(500)).await;

        let restarts = engine
            .server_manager()
            .metrics_snapshot_or_default("dead")
            .restart_count;
        assert!(
            restarts >= 1,
            "a server that failed to start should be retried at once, not after the start jitter"
        );

        engine.shutdown().await;
    }

    #[test]
    fn recovery_backoff_doubles_to_a_one_minute_cap() {
        let mut backoff = super::RecoveryBackoff::default();
        let delays: Vec<u64> = (0..12)
            .map(|_| backoff.record_failure().as_secs())
            .collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60, 60, 60, 60, 60]);

        let mut backoff = super::RecoveryBackoff::default();
        let reached_cap: Vec<bool> = (0..12)
            .map(|_| {
                backoff.record_failure();
                backoff.just_reached_cap()
            })
            .collect();
        assert_eq!(
            reached_cap.iter().filter(|reached| **reached).count(),
            1,
            "the cap is announced once"
        );
        assert!(
            reached_cap[6],
            "the seventh failure is the first to wait 60s"
        );
    }

    fn events_for<'a>(
        events: &'a [crate::test_log::CapturedEvent],
        server: &'a str,
    ) -> impl Iterator<Item = &'a crate::test_log::CapturedEvent> + 'a {
        let field = format!("server={server} ");
        events
            .iter()
            .filter(move |event| event.fields.contains(&field))
    }

    /// A server that stays down used to be retried about six times a minute
    /// forever: each health tick started a fresh round of five retries from
    /// 1s, and each round logged a warning per attempt plus an error. Now one
    /// episode backs off to once a minute and logs a handful of lines.
    #[tokio::test(start_paused = true)]
    async fn a_server_that_stays_down_backs_off_and_stays_quiet() {
        let capture = crate::test_log::EventCapture::default();
        let _default = tracing::subscriber::set_default(capture.clone());

        let mut config = Config::default();
        config
            .servers
            .insert("dead".to_string(), unstartable_server_config(60));
        let engine = Arc::new(Engine::new(config));
        engine.start().await.expect("a failed start is not fatal");

        tokio::time::sleep(Duration::from_secs(3600)).await;
        engine.shutdown().await;

        let events = capture.events();
        let spawns = events_for(&events, "dead")
            .filter(|event| event.fields.contains("spawning server process"))
            .count();
        // 1s, 2s, ... 32s reach the cap after about a minute, then one attempt
        // per 60s (plus up to 10% jitter) fills the rest of the hour.
        assert!(
            (50..=70).contains(&spawns),
            "expected about 60 start attempts in an hour, got {spawns}"
        );

        let loud: Vec<String> = events_for(&events, "dead")
            .filter(|event| event.level <= tracing::Level::WARN)
            .map(|event| event.fields.clone())
            .collect();
        assert!(
            loud.len() <= 3,
            "expected the startup failure, the first retry failure and the \
             cap notice at most, got {}: {loud:#?}",
            loud.len()
        );
    }

    /// Recovery reports success once, with how many attempts it took.
    #[tokio::test]
    async fn recovery_logs_once_when_the_server_comes_back() {
        let capture = crate::test_log::EventCapture::default();
        let _default = tracing::subscriber::set_default(capture.clone());

        let dir = tempfile::tempdir().expect("tempdir");
        let command = dir.path().join("late-mock-server");
        let mut server = unstartable_server_config(60);
        server.command = Some(command.to_string_lossy().into_owned());
        server.args = vec!["--tools".to_string(), "echo".to_string()];
        let mut config = Config::default();
        config.servers.insert("late".to_string(), server);

        let engine = Arc::new(Engine::new(config));
        engine.start().await.expect("a failed start is not fatal");

        // The upstream appears after the first recovery attempt has failed.
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::os::unix::fs::symlink(plug_test_harness::mock_server_bin(), &command)
            .expect("install the upstream");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while engine.server_manager().get_upstream("late").is_none() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the server should recover once its command exists"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        engine.shutdown().await;

        let events = capture.events();
        let recovered: Vec<&str> = events_for(&events, "late")
            .filter(|event| event.fields.contains("server recovered"))
            .map(|event| event.fields.as_str())
            .collect();
        assert_eq!(recovered.len(), 1, "{recovered:#?}");
        assert!(
            recovered[0].contains("attempts=2 "),
            "one failed attempt, then success: {}",
            recovered[0]
        );
    }

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
