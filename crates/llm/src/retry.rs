//! Retry policy for provider attempts.
//!
//! Backoff is deterministic exponential growth with no jitter. This is a
//! single-user desktop application talking to one provider at a time, so the
//! thundering-herd problem jitter solves does not exist here, and a
//! deterministic schedule keeps the retry tests exact.

use std::time::Duration;

/// Attempt count and backoff schedule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    /// Total attempts, including the first. `1` disables retrying.
    pub max_attempts: u32,
    /// Delay after the first failed attempt.
    pub initial_backoff: Duration,
    /// Ceiling applied to every computed delay.
    pub max_backoff: Duration,
    /// Growth factor applied per attempt.
    pub multiplier: u32,
    /// Honour a `retry-after` header when the provider sends one.
    pub honor_retry_after: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(8),
            multiplier: 2,
            honor_retry_after: true,
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// Delay before the attempt following `attempt` (one-based).
    #[must_use]
    pub fn backoff(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(16);
        let factor = self.multiplier.max(1).saturating_pow(exponent);
        self.initial_backoff
            .saturating_mul(factor)
            .min(self.max_backoff)
    }

    /// Delay to wait after `attempt`, preferring the provider's own advice.
    #[must_use]
    pub fn delay_after(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        match retry_after {
            Some(advice) if self.honor_retry_after => advice.min(self.max_backoff),
            _ => self.backoff(attempt),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RetryPolicy;
    use std::time::Duration;

    #[test]
    fn backoff_grows_then_saturates_at_the_ceiling() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.backoff(1), Duration::from_millis(250));
        assert_eq!(policy.backoff(2), Duration::from_millis(500));
        assert_eq!(policy.backoff(3), Duration::from_millis(1000));
        assert_eq!(policy.backoff(30), Duration::from_secs(8));
    }

    #[test]
    fn retry_after_wins_but_stays_bounded() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.delay_after(1, Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
        assert_eq!(
            policy.delay_after(1, Some(Duration::from_secs(3600))),
            Duration::from_secs(8)
        );
        let ignoring = RetryPolicy {
            honor_retry_after: false,
            ..RetryPolicy::default()
        };
        assert_eq!(
            ignoring.delay_after(1, Some(Duration::from_secs(2))),
            Duration::from_millis(250)
        );
        assert_eq!(RetryPolicy::none().max_attempts, 1);
    }
}
