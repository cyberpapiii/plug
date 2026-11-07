//! The one retry schedule: a delay that doubles from `min` to `max`, plus up
//! to `jitter` of itself at random, which spreads out servers that failed
//! together when it is not zero.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backoff {
    pub min: Duration,
    pub max: Duration,
    /// Fraction of the delay added at random, `0.0..=1.0`.
    pub jitter: f64,
}

impl Backoff {
    /// Redialing an upstream's event stream (streamable HTTP and legacy SSE).
    pub const STREAM: Self = Self {
        min: Duration::from_secs(1),
        max: Duration::from_secs(60),
        jitter: 0.0,
    };

    /// The attempts of one recovery episode for a server that is down.
    pub const RECOVERY: Self = Self {
        min: Duration::from_secs(1),
        max: Duration::from_secs(60),
        jitter: 0.1,
    };

    /// The wait before attempt `retries + 1`, without jitter: `min` after the
    /// first failure, doubling, never more than `max`. Never overflows.
    pub fn delay(&self, retries: u32) -> Duration {
        let factor = 1u32.checked_shl(retries.min(31)).unwrap_or(u32::MAX);
        self.min.saturating_mul(factor).min(self.max)
    }

    /// [`Backoff::delay`] with this schedule's jitter added.
    pub fn jittered(&self, retries: u32) -> Duration {
        let delay = self.delay(retries);
        if self.jitter <= 0.0 {
            return delay;
        }
        delay.mul_f64(1.0 + rand::random_range(0.0..self.jitter))
    }

    /// Whether `retries` is the first wait to reach `max`.
    pub fn first_at_max(&self, retries: u32) -> bool {
        retries > 0 && self.delay(retries) == self.max && self.delay(retries - 1) < self.max
    }
}

impl rmcp::transport::common::client_side_sse::SseRetryPolicy for Backoff {
    fn retry(&self, current_times: usize) -> Option<Duration> {
        Some(self.delay(u32::try_from(current_times).unwrap_or(u32::MAX)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_to_the_cap_and_never_overflows() {
        let delays: Vec<u64> = (0..9)
            .map(|n| Backoff::RECOVERY.delay(n).as_secs())
            .collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        use rmcp::transport::common::client_side_sse::SseRetryPolicy;
        assert_eq!(
            Backoff::STREAM.retry(usize::MAX),
            Some(Duration::from_secs(60))
        );
        let first_at_cap: Vec<u32> = (0..40)
            .filter(|n| Backoff::RECOVERY.first_at_max(*n))
            .collect();
        assert_eq!(first_at_cap, [6]);
    }

    #[test]
    fn jitter_stays_within_its_fraction() {
        for n in 0..10 {
            let delay = Backoff::RECOVERY.delay(n);
            let jittered = Backoff::RECOVERY.jittered(n);
            assert!(jittered >= delay && jittered <= delay.mul_f64(1.1));
        }
    }
}
