//! Fixed-capacity in-memory log ring (SPEC §6.6): the newest N formatted log
//! lines, served over IPC by `GetLogs`/`FollowLogs` (SPEC §7.2, §9).
//!
//! Pure data structure — capturing tracing events and the broadcast to
//! followers are the daemon's job. Never fed keystrokes or `.kbd` contents
//! (SPEC §6.6 [HARD]): it only ever sees what the logging policy already
//! allows onto disk.

use std::collections::VecDeque;

/// Default ring capacity (SPEC §6.6: "e.g. 2000 lines").
pub const DEFAULT_LOG_CAPACITY: usize = 2000;

/// A bounded ring of formatted log lines; pushing beyond capacity drops the
/// oldest line.
#[derive(Debug)]
pub struct LogRing {
    capacity: usize,
    lines: VecDeque<String>,
}

impl LogRing {
    /// An empty ring holding at most `capacity` lines (at least 1).
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            lines: VecDeque::new(),
        }
    }

    /// Append a line, evicting the oldest when full.
    pub fn push(&mut self, line: String) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// The newest `n` lines, oldest first (ready to print in order).
    pub fn last(&self, n: usize) -> Vec<String> {
        let skip = self.lines.len().saturating_sub(n);
        self.lines.iter().skip(skip).cloned().collect()
    }

    /// Number of buffered lines.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

impl Default for LogRing {
    fn default() -> Self {
        Self::new(DEFAULT_LOG_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring_with(cap: usize, n: usize) -> LogRing {
        let mut ring = LogRing::new(cap);
        for i in 0..n {
            ring.push(format!("line {i}"));
        }
        ring
    }

    #[test]
    fn keeps_only_the_newest_capacity_lines() {
        let ring = ring_with(3, 5);
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.last(10), vec!["line 2", "line 3", "line 4"]);
    }

    #[test]
    fn last_n_returns_newest_in_order() {
        let ring = ring_with(10, 5);
        assert_eq!(ring.last(2), vec!["line 3", "line 4"]);
        assert_eq!(ring.last(0), Vec::<String>::new());
        // Asking for more than buffered returns everything.
        assert_eq!(ring.last(99).len(), 5);
    }

    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let mut ring = LogRing::new(0);
        ring.push("a".into());
        ring.push("b".into());
        assert_eq!(ring.last(9), vec!["b"]);
    }

    #[test]
    fn empty_ring_reports_empty() {
        let ring = LogRing::new(4);
        assert!(ring.is_empty());
        assert!(ring.last(3).is_empty());
    }
}
