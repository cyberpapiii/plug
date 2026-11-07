//! Health-check background tasks for upstream MCP servers.
//!
//! One task per server pings it on its interval and records the result with
//! the server manager. A change of health schedules a tool-list refresh, so a
//! failed server's tools leave the list. A server that is down gets one
//! recovery episode, which reconnects it until it is back.

use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::backoff::Backoff;
use crate::config::{Config, ServerConfig};
use crate::server::ServerManager;
use crate::supervision::Supervision;
use crate::types::ServerHealth;

/// Bound on one health ping.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What one reconnect did. It makes one attempt and never retries: a server
/// that stays down is the recovery episode's to bring back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconnect {
    /// A fresh connection replaced the old one.
    Reconnected,
    /// Another start of this server was already running, so this one did
    /// nothing.
    AlreadyRunning,
    /// The server was removed or reconfigured while it connected. The reload
    /// that did it already put the new config in place, so the connection
    /// was dropped.
    Abandoned,
}

/// Bringing a server back, for the parts of Plug that see a connection fail.
#[allow(
    clippy::double_must_use,
    reason = "async_trait generates redundant must_use on boxed futures"
)]
#[async_trait::async_trait]
pub trait Recovery: Send + Sync {
    /// Reconnect `server` once now. The caller logs a failure; one that
    /// should keep trying calls [`Recovery::recover_server`] instead of
    /// looping.
    async fn reconnect_server(&self, server: &str) -> anyhow::Result<Reconnect>;
    /// Have `server` reconnected until it works: its one recovery episode,
    /// started now if none is running. For a caller that saw the connection
    /// go bad in a way health checks do not.
    fn recover_server(&self, server: &str);
}

/// A `Recovery` that is never there, for a router with no runtime behind it.
#[cfg(test)]
pub(crate) fn no_recovery() -> Weak<dyn Recovery> {
    enum Absent {}
    #[async_trait::async_trait]
    impl Recovery for Absent {
        async fn reconnect_server(&self, _: &str) -> anyhow::Result<Reconnect> {
            match *self {}
        }
        fn recover_server(&self, _: &str) {
            match *self {}
        }
    }
    Weak::<Absent>::new()
}

/// The health tasks and recovery episodes of every server, with what they
/// read and the `Recovery` they reconnect through.
#[derive(Clone)]
pub(crate) struct Monitor {
    servers: Arc<ServerManager>,
    supervision: Arc<Supervision>,
    config: Arc<ArcSwap<Config>>,
    cancel: CancellationToken,
    tracker: TaskTracker,
    recovery: Weak<dyn Recovery>,
}

impl Monitor {
    pub(crate) fn new(
        servers: Arc<ServerManager>,
        supervision: Arc<Supervision>,
        config: Arc<ArcSwap<Config>>,
        cancel: CancellationToken,
        tracker: TaskTracker,
        recovery: Weak<dyn Recovery>,
    ) -> Self {
        Self {
            servers,
            supervision,
            config,
            cancel,
            tracker,
            recovery,
        }
    }

    /// Spawn the health task of every enabled server that has none yet.
    pub(crate) fn spawn_missing_checks(&self, config: &Config) {
        for (name, server_config) in &config.servers {
            // A server whose start already settled has a live task. Spawning a
            // second one would supersede it and hand the server a fresh full
            // interval to wait out, which is exactly what this call is here to
            // avoid for the servers that never settled.
            if server_config.enabled && self.supervision.health_tasks.current(name).is_none() {
                self.spawn_check(name, server_config);
            }
        }
    }

    /// Spawn `name`'s health task, superseding any it had. The task ends when
    /// the server is removed, turned off, or given another interval.
    pub(crate) fn spawn_check(&self, name: &str, server_config: &ServerConfig) {
        let generation = self.supervision.health_tasks.next(name);
        let interval_secs = server_config.health_check_interval_secs;
        let monitor = self.clone();
        let name = name.to_string();
        self.tracker.spawn(async move {
            // A server that is already missing an upstream is known-bad before the
            // loop begins -- typically a local upstream still binding its port
            // after a reboot -- so it skips both the stagger and the consumed first
            // tick and goes straight to recovery. Only servers that started healthy
            // were just contacted and need neither an immediate ping nor a herd.
            let started_healthy = monitor.servers.get_upstream(&name).is_some();
            if started_healthy {
                let stagger = Duration::from_millis(rand::random_range(0..10_000));
                tokio::time::sleep(stagger).await;
            }

            let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            // `interval` yields its first tick immediately. For a server that
            // started healthy that tick would be a redundant ping, so it is
            // consumed here. Consuming it unconditionally used to leave a failed
            // server down for a whole interval after login even once it was ready.
            if started_healthy {
                tick.tick().await;
            }

            let check_now = monitor.servers.check_now_signal();
            loop {
                tokio::select! {
                    biased;
                    _ = monitor.cancel.cancelled() => {
                        tracing::debug!(server = %name, "health check task shutting down");
                        break;
                    }
                    _ = tick.tick() => {}
                    _ = check_now.notified() => {}
                }
                if monitor.supervision.health_tasks.current(&name) != Some(generation) {
                    tracing::debug!(server = %name, "health check generation superseded");
                    break;
                }
                let Some(current_config) = monitor.config.load().servers.get(&name).cloned() else {
                    monitor.supervision.health_tasks.clear(&name);
                    break;
                };
                if !current_config.enabled {
                    break;
                }
                monitor.check_once(&name).await;
            }
        });
    }

    async fn check_once(&self, name: &str) {
        let servers = &self.servers;
        let health = servers.health(name);
        if health == Some(ServerHealth::AuthRequired) {
            tracing::debug!(server = %name, "skipping health check for AuthRequired server");
            return;
        }

        match servers.get_upstream(name) {
            None if health == Some(ServerHealth::Failed) => {
                self.start_recovery(name, false);
                return;
            }
            // A connection that has ended cannot answer a ping, so there is
            // nothing to count toward: the server is down now. Its tools leave
            // the list until it is back.
            Some(upstream) if upstream.client.is_transport_closed() => {
                tracing::warn!(server = %name, "server connection ended; reconnecting");
                servers.mark_start_failure(name);
                servers.tools_changed();
                self.start_recovery(name, false);
                return;
            }
            _ => {}
        }

        if let Some((old, new)) = health_check_server(servers, name).await {
            tracing::info!(server = %name, ?old, ?new, "health state changed, refreshing tools");
            servers.tools_changed();
            if new == ServerHealth::Failed {
                self.start_recovery(name, false);
            }
        }

        // Supervision restarts a server that stays degraded past the threshold
        // or whose circuit is open: it is connected but failing real calls (the
        // iMessage continuation leak), which the Failed path above never sees.
        let (health_now, consecutive_failures) = servers.health_streak(name);
        let circuit_open = servers.circuit_open(name);
        let policy = self.config.load().supervision.clone();
        let supervision = &self.supervision;
        if health_now == ServerHealth::Healthy
            && !circuit_open
            && supervision.settled(&policy, name)
        {
            supervision.reset(name);
        } else if supervision.restart_due(
            &policy,
            name,
            health_now,
            consecutive_failures,
            circuit_open,
        ) && self.start_recovery(name, true)
        {
            tracing::warn!(server = %name, "supervising restart of degraded upstream");
        }
    }

    /// Have `server_name` reconnected until it works, even while it looks
    /// healthy: someone saw its connection go bad.
    pub(crate) fn recover(&self, server_name: &str) {
        self.supervision.want_reconnect(server_name);
        self.start_recovery(server_name, false);
    }

    /// Start `server_name`'s recovery episode unless one is running. Returns
    /// whether this call started it. Every episode stamps the restart clock;
    /// `supervised` ones also grow the supervisor's backoff.
    pub(crate) fn start_recovery(&self, server_name: &str, supervised: bool) -> bool {
        let Some(claim) = self.supervision.recovering.claim(server_name) else {
            tracing::debug!(server = %server_name, "proactive recovery task already active");
            return false;
        };
        self.supervision.note_restart(server_name, supervised);
        self.servers.record_restart(server_name);

        let monitor = self.clone();
        let server_name = server_name.to_string();
        self.tracker.spawn(async move {
            let _claim = claim;
            monitor.recover_until_back(&server_name).await;
        });
        true
    }

    /// Whether `server_name` still needs the recovery episode that is running
    /// for it. Someone else may have fixed it meanwhile (a reload, an operator
    /// restart, a reactive reconnect), or it may now need the user instead.
    fn recovery_still_needed(&self, server_name: &str) -> bool {
        let enabled = self
            .config
            .load()
            .servers
            .get(server_name)
            .is_some_and(|config| config.enabled);
        if !enabled {
            return false;
        }
        let health = self.servers.health(server_name);
        if health == Some(ServerHealth::AuthRequired) {
            return false;
        }
        self.servers.get_upstream(server_name).is_none()
            || health != Some(ServerHealth::Healthy)
            || self.servers.circuit_open(server_name)
            || self.supervision.reconnect_wanted(server_name)
    }

    /// One recovery episode: reconnect, one attempt at a time on
    /// [`Backoff::RECOVERY`], until the server is back or no longer needs it.
    ///
    /// Logs the first failure and the moment the backoff reaches its cap as
    /// warnings, every other failure at debug, and the recovery once.
    async fn recover_until_back(&self, server_name: &str) {
        tracing::info!(server = %server_name, "starting proactive recovery");
        let started = tokio::time::Instant::now();
        // An episode lasts until the server is back, so the wait keeps growing
        // across failures instead of restarting at the floor every interval.
        let backoff = Backoff::RECOVERY;
        let mut failures: u32 = 0;
        let check_now = self.servers.check_now_signal();

        loop {
            let Some(recovery) = self.recovery.upgrade() else {
                return;
            };
            let result = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => {
                    tracing::debug!(server = %server_name, "proactive recovery cancelled during shutdown");
                    return;
                }
                result = recovery.reconnect_server(server_name) => result,
            };
            drop(recovery);

            let delay = backoff.jittered(failures);
            match result {
                Ok(Reconnect::Reconnected) => {
                    tracing::info!(
                        server = %server_name,
                        attempts = failures + 1,
                        elapsed_secs = started.elapsed().as_secs(),
                        "server recovered"
                    );
                    return;
                }
                // The reload that changed the server started what it needs.
                Ok(Reconnect::Abandoned) => return,
                // Someone else is starting it. Look again after the wait: that
                // start may fail too, and then this episode is still the one.
                Ok(Reconnect::AlreadyRunning) => {}
                Err(error) => {
                    let reached_cap = backoff.first_at_max(failures);
                    failures += 1;
                    if failures == 1 {
                        tracing::warn!(
                            server = %server_name,
                            error = %error,
                            retry_in_secs = delay.as_secs(),
                            "recovery attempt failed; retrying with backoff"
                        );
                    } else if reached_cap {
                        tracing::warn!(
                            server = %server_name,
                            attempts = failures,
                            error = %error,
                            retry_every_secs = backoff.max.as_secs(),
                            "server still down; retrying at the slowest rate"
                        );
                    } else {
                        tracing::debug!(
                            server = %server_name,
                            attempts = failures,
                            error = %error,
                            retry_in_secs = delay.as_secs(),
                            "recovery attempt failed"
                        );
                    }
                }
            }

            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => {
                    tracing::debug!(server = %server_name, "proactive recovery cancelled during shutdown");
                    return;
                }
                _ = tokio::time::sleep(delay) => {}
                _ = check_now.notified() => {
                    // The Mac woke or its network changed: what kept the server
                    // down may be gone, so start again from the shortest wait.
                    failures = 0;
                }
            }

            if !self.recovery_still_needed(server_name) {
                tracing::debug!(server = %server_name, "recovery no longer needed");
                return;
            }
        }
    }

    /// Have every server checked the moment the Mac wakes or its network
    /// comes back. Connections that died in the meantime are found and
    /// replaced at once instead of at each server's next tick or on the first
    /// call that fails.
    pub(crate) fn spawn_wake_watch(&self) {
        let servers = Arc::clone(&self.servers);
        let cancel = self.cancel.clone();
        self.tracker.spawn(async move {
            let mut address = outbound_address();
            loop {
                let before = std::time::SystemTime::now();
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(WAKE_WATCH_TICK) => {}
                }
                let wall_elapsed = before.elapsed().unwrap_or_default();
                let address_now = outbound_address();
                if slept_through(wall_elapsed) {
                    tracing::info!(
                        asleep_secs = wall_elapsed.as_secs(),
                        "the Mac woke; checking every server now"
                    );
                    servers.check_servers_now();
                } else if network_changed(address, address_now) {
                    tracing::info!("the network came back or changed; checking every server now");
                    servers.check_servers_now();
                }
                address = address_now;
            }
        });
    }
}

/// How often the wake watch looks at the clock.
const WAKE_WATCH_TICK: Duration = Duration::from_secs(10);

/// Whether the Mac slept during a wait of `WAKE_WATCH_TICK`: the wall clock
/// moved well past it while the timer, which stops during sleep, did not.
fn slept_through(wall_elapsed: Duration) -> bool {
    wall_elapsed > WAKE_WATCH_TICK + Duration::from_secs(20)
}

/// The address this Mac would reach the internet from, or `None` when it has
/// no route out. Connecting a UDP socket picks the route and sends nothing.
fn outbound_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("1.1.1.1", 53)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

/// Whether the network came back or moved: there is a way out now, and it is
/// not the one there was a moment ago.
fn network_changed(before: Option<std::net::IpAddr>, now: Option<std::net::IpAddr>) -> bool {
    now.is_some() && now != before
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
    let result = tokio::time::timeout(PROBE_TIMEOUT, upstream.client.peer().list_tools(None)).await;

    let success = matches!(result, Ok(Ok(_)));

    // A server whose call slots are all taken may not answer the ping until
    // a call is done. Silence then is not a fault, and counting it would
    // restart the server under a long call.
    if result.is_err() && mgr.busy_with_a_call(name) {
        tracing::debug!(server = %name, "health ping unanswered while a call is running; not counted");
        return None;
    }

    let (old_health, new_health) = mgr.record_check(name, success);
    let changed = old_health != new_health;
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

    use std::time::Duration;

    use crate::config::{Config, ServerConfig};
    use crate::engine::Engine;

    fn unstartable_server_config(health_check_interval_secs: u64) -> ServerConfig {
        ServerConfig {
            command: Some("/nonexistent/plug-health-test-binary".to_string()),
            health_check_interval_secs,
            ..Default::default()
        }
    }

    #[test]
    fn a_network_that_returns_or_moves_is_a_change_and_one_that_leaves_is_not() {
        let home = Some("192.168.1.5".parse().unwrap());
        let cafe = Some("10.0.0.9".parse().unwrap());
        assert!(super::network_changed(None, home));
        assert!(super::network_changed(home, cafe));
        assert!(!super::network_changed(home, home));
        assert!(!super::network_changed(home, None));
        assert!(!super::network_changed(None, None));
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

        let engine = Engine::new(config);
        engine.start().await;

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
        let engine = Engine::new(config);
        engine.start().await;

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

    /// A recovery that finds another start of its server running waits it
    /// out and tries again. It used to end there, and a server whose other
    /// start then failed was left down with nothing bringing it back.
    #[tokio::test(start_paused = true)]
    async fn recovery_outlasts_another_start_of_the_same_server() {
        let capture = crate::test_log::EventCapture::default();
        let _default = tracing::subscriber::set_default(capture.clone());

        let mut config = Config::default();
        config
            .servers
            .insert("dead".to_string(), unstartable_server_config(600));
        let engine = Engine::new(config);
        engine.server_manager().mark_start_failure("dead");

        let other_start = engine.supervision().starting.claim("dead");
        assert!(engine.health().start_recovery("dead", false));
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(other_start);
        tokio::time::sleep(Duration::from_secs(30)).await;
        engine.shutdown().await;

        let spawns = events_for(&capture.events(), "dead")
            .filter(|event| event.fields.contains("spawning server process"))
            .count();
        assert!(spawns >= 1, "recovery gave up while another start ran");
    }

    /// Recovery reports success once, with how many attempts it took.
    ///
    /// Real time: the upstream is a real process, and a paused clock would
    /// fire its start timeout while the runtime waits on the child's pipes.
    #[tokio::test]
    async fn recovery_logs_once_when_the_server_comes_back() {
        let mock = plug_test_harness::mock_server_bin();
        let capture = crate::test_log::EventCapture::default();
        let _default = tracing::subscriber::set_default(capture.clone());

        let dir = tempfile::tempdir().expect("tempdir");
        let command = dir.path().join("late-mock-server");
        let mut server = unstartable_server_config(60);
        server.command = Some(command.to_string_lossy().into_owned());
        server.args = vec!["--tools".to_string(), "echo".to_string()];
        let mut config = Config::default();
        config.servers.insert("late".to_string(), server);

        let engine = Engine::new(config);
        engine.start().await;
        let mut notifications = engine.tool_router().subscribe_notifications();

        // The upstream appears once the first recovery attempt has failed.
        tokio::time::timeout(Duration::from_secs(15), async {
            while !events_for(&capture.events(), "late")
                .any(|event| event.fields.contains("recovery attempt failed"))
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the first recovery attempt should fail");
        std::os::unix::fs::symlink(mock, &command).expect("install the upstream");

        tokio::time::timeout(Duration::from_secs(15), async {
            while !events_for(&capture.events(), "late")
                .any(|event| event.fields.contains("server recovered"))
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the server should recover once its command exists");
        // Clients hear that the server's tools are back.
        assert!(
            std::iter::from_fn(|| notifications.try_recv().ok())
                .any(|n| n == crate::notifications::ProtocolNotification::ToolListChanged)
        );
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
}
