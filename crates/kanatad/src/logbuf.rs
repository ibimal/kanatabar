//! In-memory log capture for `GetLogs`/`FollowLogs` (SPEC §6.6, §7.2).
//!
//! A `tracing_subscriber` layer mirrors every formatted log event into a
//! bounded ring (`kanatabar_core::logring`) and broadcasts it to followers.
//! It sees exactly what the stderr/file log sees — the §6.6 [HARD] logging
//! bans (no keystrokes, no `.kbd` contents) are enforced at the call sites,
//! so nothing extra can leak here.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use kanatabar_core::logring::LogRing;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Follower channel capacity; a lagging follower skips (best-effort push,
/// same policy as events).
const FOLLOW_CAPACITY: usize = 256;

/// Shared handle to the ring + follower broadcast. Cheap to clone.
#[derive(Clone)]
pub struct LogBuffer {
    ring: Arc<Mutex<LogRing>>,
    follow: broadcast::Sender<String>,
}

impl LogBuffer {
    /// A buffer holding at most `capacity` lines.
    pub fn new(capacity: usize) -> Self {
        let (follow, _) = broadcast::channel(FOLLOW_CAPACITY);
        Self {
            ring: Arc::new(Mutex::new(LogRing::new(capacity))),
            follow,
        }
    }

    /// The `tracing` layer that feeds this buffer; register it on the
    /// subscriber registry at startup.
    pub fn layer(&self) -> RingLayer {
        RingLayer {
            buffer: self.clone(),
        }
    }

    /// The newest `n` lines, oldest first.
    pub fn last(&self, n: usize) -> Vec<String> {
        self.lock().last(n)
    }

    /// Subscribe to lines as they are emitted (`FollowLogs`).
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.follow.subscribe()
    }

    /// Append a formatted line (also used directly by tests).
    pub fn push(&self, line: String) {
        self.lock().push(line.clone());
        let _ = self.follow.send(line); // no followers is fine
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LogRing> {
        // Guards only push/copy; a panic inside cannot happen — recover.
        self.ring.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new(kanatabar_core::logring::DEFAULT_LOG_CAPACITY)
    }
}

/// The `tracing_subscriber` layer mirroring events into a [`LogBuffer`].
pub struct RingLayer {
    buffer: LogBuffer,
}

impl<S: tracing::Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = LineVisitor::default();
        event.record(&mut visitor);

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let mut line = format!(
            "{}.{:03} {:>5} {}: {}",
            ts.as_secs(),
            ts.subsec_millis(),
            meta.level(),
            meta.target(),
            visitor.message
        );
        line.push_str(&visitor.fields);
        self.buffer.push(line);
    }
}

/// Collects the `message` field plus `key=value` extras from one event.
#[derive(Default)]
struct LineVisitor {
    message: String,
    fields: String,
}

impl Visit for LineVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={:?}", field.name(), value);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn captures_formatted_events_into_the_ring() {
        let buffer = LogBuffer::new(8);
        let subscriber = tracing_subscriber::registry().with(buffer.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(pid = 42, "kanata spawned");
            tracing::warn!("driver preflight failed");
        });

        let lines = buffer.last(10);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains("kanata spawned") && lines[0].contains("pid=42"),
            "{lines:?}"
        );
        assert!(lines[0].contains("INFO"), "{lines:?}");
        assert!(lines[1].contains("driver preflight failed"), "{lines:?}");
    }

    #[test]
    fn followers_receive_new_lines() {
        let buffer = LogBuffer::new(8);
        let mut rx = buffer.subscribe();
        buffer.push("hello".to_string());
        assert_eq!(rx.try_recv().unwrap(), "hello");
    }

    #[test]
    fn ring_is_bounded() {
        let buffer = LogBuffer::new(2);
        for i in 0..5 {
            buffer.push(format!("l{i}"));
        }
        assert_eq!(buffer.last(10), vec!["l3", "l4"]);
    }
}
