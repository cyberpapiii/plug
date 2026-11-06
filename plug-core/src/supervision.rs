//! Who is bringing each upstream back, and how hard to push.
//!
//! One place for the per-server restart history (the supervised-restart
//! count, the restart clock, the operator cooldown), the "one at a time"
//! claims on starting a server and on running a recovery episode for it, and
//! the generation of each server's background tasks.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use dashmap::{DashMap, DashSet};

use crate::backoff::Backoff;
use crate::config::SupervisionConfig;
use crate::types::ServerHealth;

/// Minimum interval between operator restarts of the same server.
const OPERATOR_RESTART_COOLDOWN: Duration = Duration::from_secs(10);

/// A set of per-server jobs of which at most one runs at a time.
#[derive(Debug, Default)]
pub(crate) struct InFlight(Arc<DashSet<String>>);

/// Held while a claimed job runs; dropping it lets the next one in.
#[derive(Debug)]
pub(crate) struct Claim {
    set: Arc<DashSet<String>>,
    key: String,
}

impl InFlight {
    pub(crate) fn claim(&self, key: &str) -> Option<Claim> {
        self.0.insert(key.to_string()).then(|| Claim {
            set: Arc::clone(&self.0),
            key: key.to_string(),
        })
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.set.remove(&self.key);
    }
}

/// The generation of each server's long-running background task. Starting a
/// new task supersedes the old one, which sees another generation and exits.
/// Generations are never reused, so a task cannot mistake a later server of
/// the same name for its own.
#[derive(Debug, Default)]
pub(crate) struct Generations {
    current: DashMap<String, u64>,
    issued: AtomicU64,
}

impl Generations {
    pub(crate) fn next(&self, key: &str) -> u64 {
        let generation = self.issued.fetch_add(1, Ordering::Relaxed) + 1;
        self.current.insert(key.to_string(), generation);
        generation
    }

    pub(crate) fn current(&self, key: &str) -> Option<u64> {
        self.current.get(key).map(|entry| *entry)
    }

    pub(crate) fn clear(&self, key: &str) {
        self.current.remove(key);
    }
}

#[derive(Debug, Default)]
struct History {
    /// Supervised restarts since the server last settled healthy.
    supervised_restarts: u32,
    /// The most recent restart of any kind. Wall clock, so a Mac that slept
    /// through the wait does not wait it out again.
    last_restart: Option<SystemTime>,
    last_operator_restart: Option<Instant>,
    /// A caller saw the connection go bad in a way health does not show (a
    /// call that timed out, a token that was just refreshed), so recovery
    /// reconnects even while the server still looks healthy.
    reconnect_wanted: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Supervision {
    history: DashMap<String, History>,
    /// One start of an upstream at a time: restart and reconnect share it.
    pub(crate) starting: InFlight,
    /// One recovery episode per server.
    pub(crate) recovering: InFlight,
    pub(crate) health_tasks: Generations,
    pub(crate) refresh_tasks: Generations,
}

impl Supervision {
    /// Stamp a restart of `name`. Every recovery episode stamps the clock, so
    /// a crash recovery holds off a supervised restart right after it; only a
    /// supervised one escalates the backoff.
    pub(crate) fn note_restart(&self, name: &str, supervised: bool) {
        let mut history = self.history.entry(name.to_string()).or_default();
        history.last_restart = Some(SystemTime::now());
        if supervised {
            history.supervised_restarts = history.supervised_restarts.saturating_add(1);
        }
    }

    /// Whether the supervisor should restart `name` now.
    pub(crate) fn restart_due(
        &self,
        policy: &SupervisionConfig,
        name: &str,
        health: ServerHealth,
        consecutive_failures: u32,
        circuit_open: bool,
    ) -> bool {
        let (attempts, since_last) = self
            .history
            .get(name)
            .map(|history| (history.supervised_restarts, since(history.last_restart)))
            .unwrap_or((0, None));
        restart_due(
            policy,
            health,
            consecutive_failures,
            circuit_open,
            since_last,
            attempts,
        )
    }

    /// Whether `name` has gone long enough without a restart to call its
    /// recovery stable. A healthy blip right after a restart is not, so a
    /// flapping server's backoff keeps growing instead of dropping to the
    /// floor every cycle.
    pub(crate) fn settled(&self, policy: &SupervisionConfig, name: &str) -> bool {
        self.history
            .get(name)
            .and_then(|history| since(history.last_restart))
            .is_none_or(|elapsed| elapsed >= Duration::from_secs(policy.max_restart_interval_secs))
    }

    /// The server recovered for good: its next supervised restart starts the
    /// backoff from the floor again.
    pub(crate) fn reset(&self, name: &str) {
        if let Some(mut history) = self.history.get_mut(name) {
            history.supervised_restarts = 0;
        }
    }

    /// Drop the restart history of `name`, so a server stopped by a reload
    /// starts fresh.
    pub(crate) fn forget(&self, name: &str) {
        self.history.remove(name);
    }

    /// Take the operator restart slot for `name`, or say how long is left.
    pub(crate) fn claim_operator_restart(&self, name: &str) -> Result<(), Duration> {
        let mut history = self.history.entry(name.to_string()).or_default();
        if let Some(last) = history.last_operator_restart {
            let elapsed = last.elapsed();
            if elapsed < OPERATOR_RESTART_COOLDOWN {
                return Err(OPERATOR_RESTART_COOLDOWN - elapsed);
            }
        }
        history.last_operator_restart = Some(Instant::now());
        Ok(())
    }

    pub(crate) fn want_reconnect(&self, name: &str) {
        self.history
            .entry(name.to_string())
            .or_default()
            .reconnect_wanted = true;
    }

    pub(crate) fn reconnect_wanted(&self, name: &str) -> bool {
        self.history
            .get(name)
            .is_some_and(|history| history.reconnect_wanted)
    }

    pub(crate) fn reconnected(&self, name: &str) {
        if let Some(mut history) = self.history.get_mut(name) {
            history.reconnect_wanted = false;
        }
    }
}

fn since(time: Option<SystemTime>) -> Option<Duration> {
    time.map(|time| time.elapsed().unwrap_or_default())
}

/// The supervisor's rule: a server that stays degraded past the threshold,
/// or whose circuit is open, is restarted, but only once the wait since its
/// last restart has passed. That wait doubles with each supervised restart
/// that did not stick, from `min_restart_interval_secs` to
/// `max_restart_interval_secs`.
fn restart_due(
    policy: &SupervisionConfig,
    health: ServerHealth,
    consecutive_failures: u32,
    circuit_open: bool,
    since_last_restart: Option<Duration>,
    attempts: u32,
) -> bool {
    if !policy.enabled {
        return false;
    }
    let sustained = matches!(health, ServerHealth::Degraded | ServerHealth::Failed)
        && consecutive_failures >= policy.degraded_restart_threshold;
    if !(sustained || circuit_open) {
        return false;
    }
    let backoff = Backoff {
        min: Duration::from_secs(policy.min_restart_interval_secs),
        max: Duration::from_secs(policy.max_restart_interval_secs),
        jitter: 0.0,
    };
    since_last_restart.is_none_or(|elapsed| elapsed >= backoff.delay(attempts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SupervisionConfig {
        SupervisionConfig {
            enabled: true,
            degraded_restart_threshold: 5,
            min_restart_interval_secs: 60,
            max_restart_interval_secs: 600,
        }
    }

    fn due(
        policy: &SupervisionConfig,
        health: ServerHealth,
        failures: u32,
        circuit_open: bool,
        since_secs: Option<u64>,
        attempts: u32,
    ) -> bool {
        restart_due(
            policy,
            health,
            failures,
            circuit_open,
            since_secs.map(Duration::from_secs),
            attempts,
        )
    }

    #[test]
    fn a_disabled_supervisor_never_restarts() {
        let policy = SupervisionConfig {
            enabled: false,
            ..policy()
        };
        assert!(!due(&policy, ServerHealth::Failed, 100, true, None, 0));
    }

    #[test]
    fn a_degraded_streak_or_an_open_circuit_is_needed() {
        let policy = policy();
        assert!(!due(&policy, ServerHealth::Healthy, 0, false, None, 0));
        assert!(!due(&policy, ServerHealth::Degraded, 4, false, None, 0));
        assert!(due(&policy, ServerHealth::Degraded, 5, false, None, 0));
        assert!(due(&policy, ServerHealth::Healthy, 0, true, None, 0));
    }

    #[test]
    fn the_wait_between_restarts_doubles_to_the_cap() {
        let policy = policy();
        assert!(!due(&policy, ServerHealth::Failed, 6, false, Some(30), 0));
        assert!(due(&policy, ServerHealth::Failed, 6, false, Some(60), 0));
        assert!(!due(&policy, ServerHealth::Failed, 6, false, Some(119), 1));
        assert!(due(&policy, ServerHealth::Failed, 6, false, Some(120), 1));
        assert!(!due(&policy, ServerHealth::Failed, 6, false, Some(599), 50));
        assert!(due(&policy, ServerHealth::Failed, 6, false, Some(600), 50));
    }

    #[test]
    fn a_claim_is_held_until_dropped() {
        let in_flight = InFlight::default();
        let claim = in_flight.claim("a").expect("first claim");
        assert!(in_flight.claim("a").is_none());
        assert!(in_flight.claim("b").is_some());
        drop(claim);
        assert!(in_flight.claim("a").is_some());
    }

    #[test]
    fn a_restart_stamps_the_clock_and_only_a_supervised_one_escalates() {
        let supervision = Supervision::default();
        let policy = policy();
        supervision.note_restart("s", false);
        assert!(!supervision.settled(&policy, "s"));
        assert!(!supervision.restart_due(&policy, "s", ServerHealth::Failed, 6, false));
        assert_eq!(supervision.history.get("s").unwrap().supervised_restarts, 0);
        supervision.note_restart("s", true);
        assert_eq!(supervision.history.get("s").unwrap().supervised_restarts, 1);
        supervision.forget("s");
        assert!(supervision.settled(&policy, "s"));
        assert!(supervision.restart_due(&policy, "s", ServerHealth::Failed, 6, false));
    }
}
