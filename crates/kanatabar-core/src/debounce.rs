//! Trailing-debounce coalescing (SPEC §6.2 [HARD]).
//!
//! Device and config events coalesce over a window into a single action: a
//! burst fires once, `window` after its last event. This module is the pure
//! model — property-tested here (SPEC §17) — while the daemon's device loop
//! implements the same trailing-reset semantics with real tokio timers.

use std::time::Duration;

/// A trailing debouncer parameterized by its coalescing window.
#[derive(Debug, Clone, Copy)]
pub struct Debouncer {
    window_ms: u64,
}

impl Debouncer {
    /// A debouncer with the given coalescing window.
    pub fn new(window: Duration) -> Self {
        Self {
            window_ms: window.as_millis() as u64,
        }
    }

    /// The coalescing window.
    pub fn window(&self) -> Duration {
        Duration::from_millis(self.window_ms)
    }

    /// Fire times (ms) for a sequence of event times (ascending ms).
    ///
    /// Two consecutive events belong to the same burst iff they are within
    /// `window` of each other; each burst fires once, `window` after its last
    /// event. An empty input never fires.
    pub fn coalesce(&self, events_ms: &[u64]) -> Vec<u64> {
        let mut fires = Vec::new();
        let mut iter = events_ms.iter().copied();
        let Some(mut last) = iter.next() else {
            return fires;
        };
        for event in iter {
            // Debounce inputs are monotonic; guard against out-of-order noise.
            let gap = event.saturating_sub(last);
            if gap > self.window_ms {
                // The previous burst's timer already elapsed: it fires, and a
                // new burst begins at this event.
                fires.push(last.saturating_add(self.window_ms));
            }
            last = event;
        }
        fires.push(last.saturating_add(self.window_ms));
        fires
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deb(ms: u64) -> Debouncer {
        Debouncer::new(Duration::from_millis(ms))
    }

    #[test]
    fn empty_never_fires() {
        assert!(deb(100).coalesce(&[]).is_empty());
    }

    #[test]
    fn single_event_fires_once_after_window() {
        assert_eq!(deb(100).coalesce(&[50]), vec![150]);
    }

    #[test]
    fn storm_within_window_collapses_to_one_fire() {
        // A dock hotplug storm: many events each within the window (SPEC §16).
        let storm: Vec<u64> = (0..20).map(|i| i * 10).collect(); // 0,10,…,190
        let fires = deb(100).coalesce(&storm);
        assert_eq!(
            fires,
            vec![190 + 100],
            "exactly one restart per settle window"
        );
    }

    #[test]
    fn events_spaced_beyond_window_fire_separately() {
        // Bluetooth reconnect cycles far apart → one fire each.
        assert_eq!(deb(100).coalesce(&[0, 200, 400]), vec![100, 300, 500]);
    }

    #[test]
    fn boundary_gap_equal_to_window_stays_one_burst() {
        // A gap exactly equal to the window still resets the timer (same burst).
        assert_eq!(deb(100).coalesce(&[0, 100, 200]), vec![300]);
    }

    proptest::proptest! {
        /// Fires are strictly increasing, one per burst, never more than the
        /// number of events, and at least one when non-empty (SPEC §17).
        #[test]
        fn coalesce_properties(
            mut events in proptest::collection::vec(0u64..1_000_000, 1..64),
            window in 1u64..10_000,
        ) {
            events.sort_unstable();
            let fires = deb(window).coalesce(&events);

            proptest::prop_assert!(!fires.is_empty());
            proptest::prop_assert!(fires.len() <= events.len());
            for pair in fires.windows(2) {
                proptest::prop_assert!(pair[0] < pair[1], "fires strictly increasing");
            }

            // The count equals 1 + (number of gaps strictly greater than window).
            let big_gaps = events
                .windows(2)
                .filter(|w| w[1] - w[0] > window)
                .count();
            proptest::prop_assert_eq!(fires.len(), big_gaps + 1);
        }

        /// A pure storm (all events within one window of the previous) always
        /// collapses to exactly one fire.
        #[test]
        fn storm_is_always_one_fire(
            base in 0u64..1000,
            window in 5u64..1000,
            count in 1usize..50,
        ) {
            // Each event is `window` after the previous → single burst.
            let events: Vec<u64> = (0..count as u64).map(|i| base + i * window).collect();
            proptest::prop_assert_eq!(deb(window).coalesce(&events).len(), 1);
        }
    }
}
