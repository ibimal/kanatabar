//! Exponential-backoff computation for crash recovery (SPEC §6.2).
//!
//! Pure arithmetic only; the daemon owns the timers.

use serde::{Deserialize, Serialize};

/// Backoff tuning knobs; constants come from the supervisor config, with these
/// defaults from SPEC §6.2 / §7.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackoffConfig {
    /// First retry delay in milliseconds.
    pub base_ms: u64,
    /// Ceiling for the exponential delay in milliseconds.
    pub cap_ms: u64,
    /// Consecutive failures allowed before entering `Degraded`.
    pub budget: u32,
    /// A healthy run of this many seconds resets the failure budget.
    pub reset_after_s: u64,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            base_ms: 1000,
            cap_ms: 30_000,
            budget: 5,
            reset_after_s: 60,
        }
    }
}

impl BackoffConfig {
    /// Delay before retry number `attempt` (0-based): `base * 2^attempt`,
    /// saturating, capped at `cap_ms`.
    pub fn delay_ms(&self, attempt: u32) -> u64 {
        let factor = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
        self.base_ms.saturating_mul(factor).min(self.cap_ms)
    }

    /// True once `consecutive_failures` has spent the retry budget and the
    /// supervisor must enter `Degraded` instead of retrying.
    pub fn budget_spent(&self, consecutive_failures: u32) -> bool {
        consecutive_failures >= self.budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let cfg = BackoffConfig::default();
        assert_eq!(cfg.base_ms, 1000);
        assert_eq!(cfg.cap_ms, 30_000);
        assert_eq!(cfg.budget, 5);
        assert_eq!(cfg.reset_after_s, 60);
    }

    #[test]
    fn delay_doubles_then_caps() {
        let cfg = BackoffConfig::default();
        assert_eq!(cfg.delay_ms(0), 1000);
        assert_eq!(cfg.delay_ms(1), 2000);
        assert_eq!(cfg.delay_ms(2), 4000);
        assert_eq!(cfg.delay_ms(3), 8000);
        assert_eq!(cfg.delay_ms(4), 16_000);
        assert_eq!(cfg.delay_ms(5), 30_000); // 32s capped to 30s
        assert_eq!(cfg.delay_ms(63), 30_000);
        assert_eq!(cfg.delay_ms(64), 30_000); // shift overflow saturates
        assert_eq!(cfg.delay_ms(u32::MAX), 30_000);
    }

    #[test]
    fn budget_boundary() {
        let cfg = BackoffConfig::default();
        assert!(!cfg.budget_spent(4));
        assert!(cfg.budget_spent(5));
        assert!(cfg.budget_spent(6));
    }

    proptest::proptest! {
        /// The delay never exceeds the cap and never drops below
        /// `min(base, cap)`, for any attempt and tuning (SPEC §15).
        #[test]
        fn delay_bounded(
            attempt in 0u32..10_000,
            base_ms in 1u64..1_000_000,
            cap_ms in 1u64..1_000_000,
        ) {
            let cfg = BackoffConfig { base_ms, cap_ms, ..Default::default() };
            let delay = cfg.delay_ms(attempt);
            proptest::prop_assert!(delay <= cap_ms);
            proptest::prop_assert!(delay >= base_ms.min(cap_ms));
        }

        /// Delays are monotonically non-decreasing in the attempt number.
        #[test]
        fn delay_monotonic(
            attempt in 0u32..200,
            base_ms in 1u64..1_000_000,
            cap_ms in 1u64..1_000_000,
        ) {
            let cfg = BackoffConfig { base_ms, cap_ms, ..Default::default() };
            proptest::prop_assert!(cfg.delay_ms(attempt) <= cfg.delay_ms(attempt + 1));
        }
    }
}
