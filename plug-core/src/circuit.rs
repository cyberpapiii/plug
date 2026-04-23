//! Lock-free circuit breaker for the MCP multiplexer proxy.
//!
//! Uses atomic state transitions and a [`tokio::sync::Semaphore`] for half-open
//! probe slots, avoiding any mutex contention on the hot path.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::time::Instant;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const STATE_CLOSED: u8 = 0;
const STATE_OPEN: u8 = 1;
const STATE_HALF_OPEN: u8 = 2;

// ---------------------------------------------------------------------------
// Epoch helpers (process-wide monotonic base)
// ---------------------------------------------------------------------------

static EPOCH: OnceLock<Instant> = OnceLock::new();

fn epoch() -> Instant {
    *EPOCH.get_or_init(Instant::now)
}

fn nanos_since_epoch() -> u64 {
    epoch().elapsed().as_nanos() as u64
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Observable state of the circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Error returned when a call is rejected by the circuit breaker.
#[derive(Debug, Clone, thiserror::Error)]
#[error("circuit breaker is open")]
pub struct CircuitBreakerError;

/// Configuration for [`CircuitBreaker`].
#[derive(Clone, Debug)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before the circuit opens.
    pub failure_threshold: u32,
    /// How long the circuit stays open before transitioning to half-open.
    pub open_duration: Duration,
    /// Number of probe calls allowed in the half-open state.
    pub probe_count: usize,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            open_duration: Duration::from_secs(30),
            probe_count: 2,
        }
    }
}

// ---------------------------------------------------------------------------
// CircuitBreaker
// ---------------------------------------------------------------------------

/// A lock-free circuit breaker.
///
/// State machine: `Closed → Open → HalfOpen → Closed` (or back to Open).
pub struct CircuitBreaker {
    state: AtomicU8,
    failure_count: AtomicU32,
    /// Nanoseconds since [`EPOCH`] when the circuit was opened. `u64::MAX` means "not set".
    open_since_nanos: AtomicU64,
    /// Semaphore starts with 0 permits; permits are added on Open→HalfOpen transition.
    probe_semaphore: Semaphore,
    config: CircuitBreakerConfig,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: AtomicU8::new(STATE_CLOSED),
            failure_count: AtomicU32::new(0),
            open_since_nanos: AtomicU64::new(u64::MAX),
            probe_semaphore: Semaphore::new(0),
            config,
        }
    }

    /// Check whether a call is allowed to proceed.
    ///
    /// - **Closed**: always returns `Ok(())`.
    /// - **Open**: if `open_duration` has elapsed, transitions to HalfOpen and
    ///   attempts to acquire a probe permit. Otherwise returns `Err`.
    /// - **HalfOpen**: tries to acquire a probe permit. Returns `Err` if none
    ///   remain.
    pub fn call_allowed(&self) -> Result<(), CircuitBreakerError> {
        let s = self.state.load(Ordering::Acquire);

        match s {
            STATE_CLOSED => Ok(()),

            STATE_OPEN => {
                let opened_at = self.open_since_nanos.load(Ordering::Acquire);
                let now = nanos_since_epoch();
                let elapsed = Duration::from_nanos(now.saturating_sub(opened_at));

                if elapsed >= self.config.open_duration {
                    // Try to transition Open → HalfOpen.
                    if self
                        .state
                        .compare_exchange(
                            STATE_OPEN,
                            STATE_HALF_OPEN,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        // Drain leftover permits from previous half-open cycle
                        // to avoid permit accumulation across multiple cycles.
                        while self.probe_semaphore.try_acquire().is_ok() {}
                        self.probe_semaphore.add_permits(self.config.probe_count);
                    }
                    // Whether we won the CAS or someone else did, try a probe.
                    self.try_acquire_probe()
                } else {
                    Err(CircuitBreakerError)
                }
            }

            STATE_HALF_OPEN => self.try_acquire_probe(),

            _ => Err(CircuitBreakerError),
        }
    }

    /// Record a successful call.
    ///
    /// - **Closed**: resets the failure counter.
    /// - **HalfOpen**: transitions back to Closed and resets the failure counter.
    pub fn on_success(&self) {
        let s = self.state.load(Ordering::Acquire);

        match s {
            STATE_CLOSED => {
                self.failure_count.store(0, Ordering::Relaxed);
            }
            STATE_HALF_OPEN
                if self
                    .state
                    .compare_exchange(
                        STATE_HALF_OPEN,
                        STATE_CLOSED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok() =>
            {
                // HalfOpen → Closed
                self.failure_count.store(0, Ordering::Relaxed);
                self.open_since_nanos.store(u64::MAX, Ordering::Release);
            }
            _ => {}
        }
    }

    /// Record a failed call.
    ///
    /// - **Closed**: increments the failure counter. If the threshold is reached,
    ///   transitions to Open and records the timestamp.
    /// - **HalfOpen**: transitions immediately back to Open.
    pub fn on_failure(&self) {
        let s = self.state.load(Ordering::Acquire);

        match s {
            STATE_CLOSED => {
                let prev = self.failure_count.fetch_add(1, Ordering::Relaxed);
                // `prev` is the value *before* the add, so the new count is prev + 1.
                if prev + 1 >= self.config.failure_threshold
                    && self
                        .state
                        .compare_exchange(
                            STATE_CLOSED,
                            STATE_OPEN,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                {
                    self.open_since_nanos
                        .store(nanos_since_epoch(), Ordering::Release);
                }
            }
            STATE_HALF_OPEN
                if self
                    .state
                    .compare_exchange(
                        STATE_HALF_OPEN,
                        STATE_OPEN,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok() =>
            {
                self.open_since_nanos
                    .store(nanos_since_epoch(), Ordering::Release);
            }
            _ => {}
        }
    }

    /// Force-reset the circuit breaker to the Closed state.
    ///
    /// Useful after a successful manual reconnection.
    pub fn reset(&self) {
        self.state.store(STATE_CLOSED, Ordering::Release);
        self.failure_count.store(0, Ordering::Relaxed);
        self.open_since_nanos.store(u64::MAX, Ordering::Release);
    }

    /// Return the current [`CircuitState`].
    pub fn state(&self) -> CircuitState {
        match self.state.load(Ordering::Acquire) {
            STATE_CLOSED => CircuitState::Closed,
            STATE_OPEN => CircuitState::Open,
            STATE_HALF_OPEN => CircuitState::HalfOpen,
            _ => unreachable!("invalid circuit breaker state"),
        }
    }

    // -- private helpers ----------------------------------------------------

    /// Try to acquire a half-open probe permit.
    fn try_acquire_probe(&self) -> Result<(), CircuitBreakerError> {
        match self.probe_semaphore.try_acquire() {
            Ok(permit) => {
                permit.forget(); // consume permanently
                Ok(())
            }
            Err(_) => Err(CircuitBreakerError),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_breaker() -> CircuitBreaker {
        CircuitBreaker::new(CircuitBreakerConfig::default())
    }

    fn breaker_with(threshold: u32, open_secs: u64, probes: usize) -> CircuitBreaker {
        CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: threshold,
            open_duration: Duration::from_secs(open_secs),
            probe_count: probes,
        })
    }

    /// Trip the breaker by recording `n` failures (must be >= threshold).
    fn trip(cb: &CircuitBreaker) {
        for _ in 0..cb.config.failure_threshold {
            cb.on_failure();
        }
    }

    // 1. Closed state always allows calls.
    #[test]
    fn closed_allows_calls() {
        let cb = default_breaker();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.call_allowed().is_ok());
        assert!(cb.call_allowed().is_ok());
    }

    // 2. After threshold failures the breaker opens and rejects calls.
    #[test]
    fn opens_after_threshold_failures() {
        let cb = breaker_with(3, 30, 2);
        cb.on_failure();
        cb.on_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.on_failure(); // 3rd failure → opens
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(cb.call_allowed().is_err());
    }

    // 3. A success resets the failure count so the breaker stays closed.
    #[test]
    fn success_resets_failure_count() {
        let cb = breaker_with(3, 30, 2);
        cb.on_failure();
        cb.on_failure();
        cb.on_success(); // reset
        cb.on_failure();
        cb.on_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    // 4. Open state rejects immediately (before duration elapses).
    #[test]
    fn open_rejects_calls() {
        let cb = breaker_with(1, 9999, 2);
        trip(&cb);
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(cb.call_allowed().is_err());
        assert!(cb.call_allowed().is_err());
    }

    // 5. After open_duration the breaker transitions to HalfOpen and allows probes.
    #[tokio::test(start_paused = true)]
    async fn transitions_to_half_open_after_duration() {
        let cb = breaker_with(1, 30, 2);
        trip(&cb);
        assert_eq!(cb.state(), CircuitState::Open);

        tokio::time::advance(Duration::from_secs(31)).await;

        assert!(cb.call_allowed().is_ok());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    // 6. A success in HalfOpen closes the breaker.
    #[tokio::test(start_paused = true)]
    async fn half_open_success_closes() {
        let cb = breaker_with(1, 30, 2);
        trip(&cb);

        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(cb.call_allowed().is_ok());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.on_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    // 7. A failure in HalfOpen re-opens the breaker.
    #[tokio::test(start_paused = true)]
    async fn half_open_failure_reopens() {
        let cb = breaker_with(1, 30, 2);
        trip(&cb);

        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(cb.call_allowed().is_ok());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.on_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    // 8. Only probe_count calls are allowed while in HalfOpen.
    #[tokio::test(start_paused = true)]
    async fn probe_count_limits_half_open() {
        let cb = breaker_with(1, 30, 2);
        trip(&cb);

        tokio::time::advance(Duration::from_secs(31)).await;

        assert!(cb.call_allowed().is_ok()); // probe 1
        assert!(cb.call_allowed().is_ok()); // probe 2
        assert!(cb.call_allowed().is_err()); // no more probes
    }

    // 9. reset() forces the breaker back to Closed.
    #[test]
    fn reset_forces_closed() {
        let cb = breaker_with(1, 9999, 2);
        trip(&cb);
        assert_eq!(cb.state(), CircuitState::Open);

        cb.reset();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.call_allowed().is_ok());
    }

    // 10. Many concurrent failures don't cause corruption.
    #[tokio::test]
    async fn concurrent_trip_idempotent() {
        use std::sync::Arc;

        let cb = Arc::new(breaker_with(5, 9999, 2));
        let mut handles = Vec::new();

        for _ in 0..100 {
            let cb = Arc::clone(&cb);
            handles.push(tokio::spawn(async move {
                cb.on_failure();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // After 100 failures the breaker must be open — no panic, no corruption.
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(cb.call_allowed().is_err());
    }
}
