//! Device monitor pipeline (SPEC §6.3): a source of keyboard presence events,
//! debounced into a single kanata re-sync restart — the core gap this project
//! closes (SPEC §1).
//!
//! The real source is IOKit on a dedicated CFRunLoop thread ([`IoKitMonitor`],
//! backed by `ffi::iokit`). A [`Monitor`] trait lets tests inject fake events
//! without IOKit, root, or hardware (SPEC §17, §18); the pure relevance and
//! debounce rules live in `kanatabar_core`.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanatabar_core::device::{is_resync_relevant, DeviceChange, DeviceDescriptor};
use kanatabar_core::ipc::{DeviceInfo, Event};
use kanatabar_core::state::SupervisorState;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::events::DaemonEvents;
use crate::supervisor::{Command, SupervisorClient};

/// A registry change delivered by a [`Monitor`].
#[derive(Debug, Clone)]
pub struct DeviceEvent {
    /// Added or removed.
    pub change: DeviceChange,
    /// What was added/removed (presence facts only).
    pub descriptor: DeviceDescriptor,
    /// True for the devices already present when the monitor armed (the
    /// initial enumeration): recorded in the registry for `GetDevices`, but
    /// never debounced into a restart and never pushed as `DeviceChanged` —
    /// existing devices are not hotplugs (SPEC §6.3).
    pub initial: bool,
}

/// Live view of the devices the monitor has seen, served by `GetDevices`
/// (SPEC §7.2, §9). Cheap to clone. `matched` reflects the same relevance
/// rule the re-sync filter uses (keyboard, not Karabiner-virtual) — i.e.
/// "would kanata try to grab this", the honest approximation available
/// without kanata's own device list.
#[derive(Clone, Default)]
pub struct DeviceRegistry {
    inner: Arc<Mutex<BTreeMap<String, DeviceDescriptor>>>,
}

impl DeviceRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, DeviceDescriptor>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Stable key for a device (names alone can collide across vendors).
    fn key(descriptor: &DeviceDescriptor) -> String {
        format!(
            "{}#{}",
            descriptor.name,
            descriptor.vendor_id.unwrap_or_default()
        )
    }

    /// Record an add/remove.
    pub fn apply(&self, change: DeviceChange, descriptor: &DeviceDescriptor) {
        let mut map = self.lock();
        match change {
            DeviceChange::Added => {
                map.insert(Self::key(descriptor), descriptor.clone());
            }
            DeviceChange::Removed => {
                map.remove(&Self::key(descriptor));
            }
        }
    }

    /// The current device list for `GetDevices`, sorted by name.
    pub fn snapshot(&self) -> Vec<DeviceInfo> {
        self.lock()
            .values()
            .map(|descriptor| DeviceInfo {
                name: descriptor.name.clone(),
                matched: is_resync_relevant(DeviceChange::Added, descriptor),
            })
            .collect()
    }
}

/// A source of keyboard presence events (SPEC §6.3). Behind a trait so tests
/// inject fake events and the real IOKit source stays swappable (§18).
pub trait Monitor {
    /// Begin delivering events on `sink` from a background thread until the
    /// returned [`MonitorHandle`] is dropped.
    fn start(
        self: Box<Self>,
        sink: mpsc::UnboundedSender<DeviceEvent>,
    ) -> io::Result<MonitorHandle>;
}

/// Keeps a [`Monitor`]'s background thread alive; stops it on drop.
pub struct MonitorHandle {
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl MonitorHandle {
    /// Build a handle from a stop action (run once on drop).
    pub(crate) fn new(stop: Box<dyn FnOnce() + Send>) -> Self {
        Self { stop: Some(stop) }
    }
}

impl Drop for MonitorHandle {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

/// Consume device events, filter to re-sync-relevant keyboard changes, debounce
/// bursts over `window`, and restart kanata once per settle window so it
/// re-scans devices (SPEC §6.2 [HARD], §6.3). Runs until `events` closes.
///
/// A re-sync restart is only issued while kanata is `Running`: a hotplug should
/// not start a stopped or paused kanata.
pub async fn run(
    mut events: mpsc::UnboundedReceiver<DeviceEvent>,
    supervisor: SupervisorClient,
    window: Duration,
    registry: DeviceRegistry,
    bus: DaemonEvents,
) {
    info!(
        window_ms = window.as_millis() as u64,
        "device monitor pipeline started"
    );
    let mut deadline: Option<Instant> = None;

    loop {
        tokio::select! {
            biased;
            maybe = events.recv() => match maybe {
                None => break, // monitor gone
                Some(event) => {
                    // Every event updates the GetDevices registry, including
                    // the initial enumeration.
                    registry.apply(event.change, &event.descriptor);
                    if event.initial {
                        debug!(name = %event.descriptor.name, "initial device recorded");
                    } else if is_resync_relevant(event.change, &event.descriptor) {
                        debug!(
                            name = %event.descriptor.name,
                            change = ?event.change,
                            "relevant device change; (re)arming debounce",
                        );
                        deadline = Some(Instant::now() + window);
                        // Push Event::DeviceChanged to subscribers (SPEC §7.2).
                        bus.publish(Event::DeviceChanged {
                            added: event.change == DeviceChange::Added,
                            name: event.descriptor.name.clone(),
                        });
                    } else {
                        debug!(
                            name = %event.descriptor.name,
                            "ignoring non-keyboard / virtual device change",
                        );
                    }
                }
            },
            () = sleep_until_opt(deadline), if deadline.is_some() => {
                deadline = None;
                if supervisor.snapshot().state == SupervisorState::Running {
                    info!("device change settled; restarting kanata to re-sync devices");
                    if let Err(err) = supervisor.send(Command::Restart).await {
                        warn!(%err, "could not request device re-sync restart");
                    }
                } else {
                    debug!("device change settled, but kanata is not running; no re-sync");
                }
            }
        }
    }

    info!("device monitor pipeline stopped");
}

/// Await a deadline, or pend forever when there is none (keeps the `select!`
/// arm total without spinning).
async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// The real device source: IOKit keyboard add/terminate notifications on a
/// dedicated CFRunLoop thread (SPEC §3.1 [HARD], §6.3). All unsafe lives in
/// `ffi::iokit`; this is the safe [`Monitor`] wrapper.
pub struct IoKitMonitor;

impl Monitor for IoKitMonitor {
    fn start(
        self: Box<Self>,
        sink: mpsc::UnboundedSender<DeviceEvent>,
    ) -> io::Result<MonitorHandle> {
        crate::ffi::iokit::start_monitor(sink)
    }
}
