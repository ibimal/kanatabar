//! The tray's view of daemon state, driven by the event stream (SPEC §8:
//! "Connects, `Hello` + `Subscribe`, renders state"). Pure reducer — no I/O,
//! no UI — so the client logic is unit-testable without a daemon or a display
//! (the Phase 7 `[AUTO]` gate, SPEC §19).
//!
//! The tray seeds itself with one `GetStatus` + `ListPresets` on connect, then
//! folds each pushed [`Event`] into the held [`Status`] rather than re-polling
//! — zero polling (SPEC §6.6).

use kanatabar_core::ipc::{Event, PresetInfo, Status};
use kanatabar_core::state::SupervisorState;

use crate::model::MenuModel;

/// The tray's last-known daemon state. `None` status means "not seeded yet";
/// `connected` tracks whether the socket is currently up.
#[derive(Debug, Clone, Default)]
pub struct Session {
    connected: bool,
    status: Option<Status>,
    presets: Vec<PresetInfo>,
}

impl Session {
    /// A fresh, disconnected session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the connection lost; the held snapshot is dropped so a stale
    /// status can't be shown as if it were live.
    pub fn disconnect(&mut self) {
        self.connected = false;
        self.status = None;
        self.presets.clear();
    }

    /// Seed from the initial `GetStatus` reply; also marks the session
    /// connected.
    pub fn set_status(&mut self, status: Status) {
        self.connected = true;
        self.status = Some(status);
    }

    /// Seed / refresh the preset list (`ListPresets`).
    pub fn set_presets(&mut self, presets: Vec<PresetInfo>) {
        self.presets = presets;
    }

    /// Whether the session currently believes it is connected and seeded.
    pub fn is_live(&self) -> bool {
        self.connected && self.status.is_some()
    }

    /// Fold one pushed event into the held snapshot. A no-op until the session
    /// has been seeded with a `Status`, since events only carry deltas.
    pub fn apply_event(&mut self, event: &Event) {
        let Some(status) = self.status.as_mut() else {
            return;
        };
        match event {
            Event::StateChanged { to, reason, .. } => {
                status.state = *to;
                match (to, reason) {
                    // Entering Degraded: surface the actionable reason.
                    (SupervisorState::Degraded, Some(reason)) => {
                        status.last_error = Some(reason.describe().to_string());
                    }
                    // Leaving Degraded for a healthy state: clear the note.
                    (state, _) if *state != SupervisorState::Degraded => {
                        status.last_error = None;
                    }
                    _ => {}
                }
            }
            Event::LayerChanged { layer } => {
                status.active_layer = Some(layer.clone());
            }
            Event::ConfigApplied { preset, .. } => {
                if let Some(name) = preset {
                    status.active_preset = Some(name.clone());
                    for p in &mut self.presets {
                        p.active = &p.name == name;
                    }
                }
            }
            Event::DriverIssue { message, .. } => {
                status.last_error = Some(message.clone());
            }
            // Device add/remove and crash notices don't change the rendered
            // model on their own — a StateChanged follows a crash, and the
            // devices submenu is fetched on demand (SPEC §8).
            Event::DeviceChanged { .. } | Event::Crash { .. } => {}
        }
    }

    /// The menu to render right now.
    pub fn menu_model(&self) -> MenuModel {
        match (self.connected, &self.status) {
            (true, Some(status)) => MenuModel::connected(status, &self.presets),
            _ => MenuModel::disconnected(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanatabar_core::ipc::ErrorKind;
    use kanatabar_core::state::DegradedReason;

    fn status(state: SupervisorState) -> Status {
        Status {
            state,
            active_preset: Some("main".into()),
            active_layer: Some("base".into()),
            kanata_pid: Some(42),
            kanata_version: Some("1.8.1".into()),
            driver_ok: Some(true),
            last_error: None,
            degraded_reason: None,
            passthrough: false,
            uptime_s: 1,
            daemon_version: "0.1.0".into(),
        }
    }

    fn presets() -> Vec<PresetInfo> {
        vec![
            PresetInfo {
                name: "main".into(),
                config: "/main.kbd".into(),
                autostart: true,
                active: true,
            },
            PresetInfo {
                name: "gaming".into(),
                config: "/gaming.kbd".into(),
                autostart: false,
                active: false,
            },
        ]
    }

    #[test]
    fn fresh_session_is_disconnected() {
        let s = Session::new();
        assert!(!s.is_live());
        assert!(!s.menu_model().connected);
    }

    #[test]
    fn events_before_seeding_are_ignored() {
        let mut s = Session::new();
        s.apply_event(&Event::LayerChanged {
            layer: "nav".into(),
        });
        assert!(!s.is_live());
    }

    #[test]
    fn seeding_makes_it_live() {
        let mut s = Session::new();
        s.set_status(status(SupervisorState::Running));
        s.set_presets(presets());
        assert!(s.is_live());
        let model = s.menu_model();
        assert!(model.connected);
        assert_eq!(model.presets.len(), 2);
    }

    #[test]
    fn state_change_event_updates_the_model() {
        let mut s = Session::new();
        s.set_status(status(SupervisorState::Running));
        s.apply_event(&Event::StateChanged {
            from: SupervisorState::Running,
            to: SupervisorState::Paused,
            reason: None,
        });
        assert_eq!(s.menu_model().icon, crate::model::IconKind::Paused);
    }

    #[test]
    fn entering_degraded_surfaces_the_reason_in_the_state_line() {
        let mut s = Session::new();
        s.set_status(status(SupervisorState::Running));
        s.apply_event(&Event::StateChanged {
            from: SupervisorState::Backoff,
            to: SupervisorState::Degraded,
            reason: Some(DegradedReason::DriverNotActivated),
        });
        assert_eq!(
            s.menu_model().state_line,
            format!(
                "Degraded — {}",
                DegradedReason::DriverNotActivated.describe()
            )
        );
    }

    #[test]
    fn recovering_from_degraded_clears_the_error() {
        let mut s = Session::new();
        s.set_status(status(SupervisorState::Degraded));
        s.apply_event(&Event::StateChanged {
            from: SupervisorState::Backoff,
            to: SupervisorState::Degraded,
            reason: Some(DegradedReason::RetryBudgetExhausted),
        });
        s.apply_event(&Event::StateChanged {
            from: SupervisorState::Starting,
            to: SupervisorState::Running,
            reason: None,
        });
        assert_eq!(s.menu_model().state_line, "State: Running");
    }

    #[test]
    fn layer_change_event_updates_the_layer_line() {
        let mut s = Session::new();
        s.set_status(status(SupervisorState::Running));
        s.apply_event(&Event::LayerChanged {
            layer: "nav".into(),
        });
        assert_eq!(s.menu_model().layer_line, Some("Layer: nav".to_string()));
    }

    #[test]
    fn config_applied_moves_the_active_checkmark() {
        let mut s = Session::new();
        s.set_status(status(SupervisorState::Running));
        s.set_presets(presets());
        s.apply_event(&Event::ConfigApplied {
            preset: Some("gaming".into()),
            path: "/gaming.kbd".into(),
        });
        let active: Vec<_> = s
            .menu_model()
            .presets
            .into_iter()
            .filter(|p| p.active)
            .map(|p| p.name)
            .collect();
        assert_eq!(active, vec!["gaming".to_string()]);
    }

    #[test]
    fn driver_issue_event_records_the_message() {
        let mut s = Session::new();
        s.set_status(status(SupervisorState::Running));
        s.apply_event(&Event::DriverIssue {
            kind: ErrorKind::VhidDaemonDown,
            message: "VHID daemon down".into(),
        });
        // Once Degraded follows, the state line shows the recorded message
        // rather than the generic fallback.
        s.apply_event(&Event::StateChanged {
            from: SupervisorState::Running,
            to: SupervisorState::Degraded,
            reason: None,
        });
        assert_eq!(s.menu_model().state_line, "Degraded — VHID daemon down");
    }

    #[test]
    fn disconnect_drops_the_snapshot() {
        let mut s = Session::new();
        s.set_status(status(SupervisorState::Running));
        s.set_presets(presets());
        s.disconnect();
        assert!(!s.is_live());
        assert!(!s.menu_model().connected);
        assert!(s.menu_model().presets.is_empty());
    }
}
