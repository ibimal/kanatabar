//! Health checks and live status (SPEC §6.5): driver preflight, orphan sweep,
//! kanata TCP layer relay, and the shared health snapshot surfaced in `Status`.

pub mod driver;
pub mod orphan;
pub mod tcp;

/// Start the sleep/wake monitor (SPEC §6.5); the safe wrapper over the IOKit
/// power FFI lives in `ffi::power`.
pub use crate::ffi::power::start_power_monitor;

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

/// Shared, mutable health facts surfaced in `Status` (SPEC §7.2). Written by the
/// supervisor (driver/version at spawn) and the TCP relay (active layer); read
/// by the control server. Cheap to clone (an `Arc`).
///
/// Layer changes are also broadcast so subscribed control connections push
/// `Event::LayerChanged` live (SPEC §7.2) instead of clients only seeing the
/// layer on the next `GetStatus`.
#[derive(Clone)]
pub struct HealthState {
    inner: Arc<Mutex<HealthInner>>,
    layer_events: broadcast::Sender<String>,
}

impl Default for HealthState {
    fn default() -> Self {
        // Capacity matches the supervisor's event channel; lagged subscribers
        // skip (best-effort push, same policy as StateChanged).
        let (layer_events, _) = broadcast::channel(256);
        Self {
            inner: Arc::default(),
            layer_events,
        }
    }
}

#[derive(Default)]
struct HealthInner {
    active_layer: Option<String>,
    kanata_version: Option<String>,
    driver_ok: Option<bool>,
}

/// A read-only view of [`HealthState`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HealthSnapshot {
    /// Active kanata layer, when known via the TCP relay.
    pub active_layer: Option<String>,
    /// Reported kanata version.
    pub kanata_version: Option<String>,
    /// Driver preflight result; `None` until first checked.
    pub driver_ok: Option<bool>,
}

impl HealthState {
    fn lock(&self) -> std::sync::MutexGuard<'_, HealthInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Set the active layer (TCP relay); `None` clears it when kanata stops.
    /// A *changed* named layer is broadcast to [`Self::subscribe_layers`].
    pub fn set_active_layer(&self, layer: Option<String>) {
        let changed = {
            let mut inner = self.lock();
            let changed = inner.active_layer != layer;
            inner.active_layer = layer.clone();
            changed
        };
        if changed {
            if let Some(name) = layer {
                let _ = self.layer_events.send(name); // no subscribers is fine
            }
        }
    }

    /// Subscribe to live layer changes (pushed as `Event::LayerChanged`).
    pub fn subscribe_layers(&self) -> broadcast::Receiver<String> {
        self.layer_events.subscribe()
    }

    /// Set the reported kanata version.
    pub fn set_kanata_version(&self, version: Option<String>) {
        self.lock().kanata_version = version;
    }

    /// Set the driver preflight result.
    pub fn set_driver_ok(&self, ok: Option<bool>) {
        self.lock().driver_ok = ok;
    }

    /// Snapshot the current health facts.
    pub fn snapshot(&self) -> HealthSnapshot {
        let inner = self.lock();
        HealthSnapshot {
            active_layer: inner.active_layer.clone(),
            kanata_version: inner.kanata_version.clone(),
            driver_ok: inner.driver_ok,
        }
    }
}
