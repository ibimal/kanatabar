//! Exponential reconnect for the tray's control-socket client (SPEC §8:
//! "exponential reconnect when the daemon bounces"). Reuses the same pure
//! arithmetic the daemon uses for crash-retry (`kanatabar_core::backoff`)
//! rather than growing a second backoff curve.

use std::time::Duration;

use kanatabar_core::backoff::BackoffConfig;

/// Tracks reconnect attempts and hands back the next delay.
#[derive(Debug, Clone)]
pub struct Reconnector {
    backoff: BackoffConfig,
    attempt: u32,
}

impl Reconnector {
    /// A reconnector using the given backoff curve.
    pub fn new(backoff: BackoffConfig) -> Self {
        Self {
            backoff,
            attempt: 0,
        }
    }

    /// Delay before the next attempt; advances the attempt counter.
    pub fn next_delay(&mut self) -> Duration {
        let delay = Duration::from_millis(self.backoff.delay_ms(self.attempt));
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    /// A connection succeeded: the next disconnect starts over at the shortest
    /// delay.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

impl Default for Reconnector {
    fn default() -> Self {
        Self::new(BackoffConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BackoffConfig {
        BackoffConfig {
            base_ms: 100,
            cap_ms: 800,
            budget: 5,
            reset_after_s: 60,
        }
    }

    #[test]
    fn delays_double_then_cap() {
        let mut r = Reconnector::new(cfg());
        let got: Vec<u64> = (0..5).map(|_| r.next_delay().as_millis() as u64).collect();
        assert_eq!(got, vec![100, 200, 400, 800, 800]);
    }

    #[test]
    fn reset_starts_over() {
        let mut r = Reconnector::new(cfg());
        r.next_delay();
        r.next_delay();
        r.reset();
        assert_eq!(r.next_delay().as_millis(), 100);
    }
}
