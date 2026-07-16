//! Daemon-wide event bus for the IPC events that don't come from the
//! supervisor's state machine: `DeviceChanged` (device monitor) and
//! `ConfigApplied` (config manager) (SPEC §7.2). Subscribed control
//! connections relay them to clients; `StateChanged` (supervisor broadcast)
//! and `LayerChanged` (health broadcast) keep their existing channels.

use kanatabar_core::ipc::Event;
use tokio::sync::broadcast;

/// Channel capacity; a lagging subscriber skips (best-effort push).
const CAPACITY: usize = 256;

/// Cheap-to-clone publisher handle.
#[derive(Clone)]
pub struct DaemonEvents {
    tx: broadcast::Sender<Event>,
}

impl DaemonEvents {
    /// Publish an event to all subscribed control connections.
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(event); // no subscribers is fine
    }

    /// Subscribe (one receiver per `Subscribe`d control connection).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for DaemonEvents {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        Self { tx }
    }
}
