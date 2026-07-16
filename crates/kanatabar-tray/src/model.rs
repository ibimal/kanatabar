//! Pure state → menu view-model (SPEC §8). No `tray-icon`/`muda`/`tao` types
//! here: this is "what should the menu look like", unit-testable without a
//! display — the Phase 7 gate's `[AUTO]` half (SPEC §19).

use kanatabar_core::ipc::{PresetInfo, Status};
use kanatabar_core::state::SupervisorState;

/// Which status glyph to show. SPEC §8 names four (running / paused / degraded
/// / disconnected); `Idle` covers the remaining reachable `SupervisorState`s
/// (Stopped / Starting / Backoff) that the spec doesn't call out a distinct
/// icon for — they still need *some* glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    /// Child running and healthy.
    Running,
    /// Remapping paused by the user.
    Paused,
    /// Degraded — needs user action (badge).
    Degraded,
    /// Connected but neither running, paused, nor degraded.
    Idle,
    /// No connection to the daemon.
    Disconnected,
}

impl IconKind {
    /// The glyph for a connected daemon reporting `state`.
    pub fn for_state(state: SupervisorState) -> Self {
        match state {
            SupervisorState::Running => Self::Running,
            SupervisorState::Paused => Self::Paused,
            SupervisorState::Degraded => Self::Degraded,
            SupervisorState::Stopped | SupervisorState::Starting | SupervisorState::Backoff => {
                Self::Idle
            }
        }
    }
}

/// Which lifecycle commands the menu should currently enable. Mirrors
/// `kanatabar_core::machine`'s transition table (SPEC §6.2) so the tray never
/// offers a command the daemon would reject — the daemon remains the authority
/// (it re-validates), but greying out impossible actions is better UX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// `Start` is available.
    pub start: bool,
    /// `Stop` is available.
    pub stop: bool,
    /// `Restart` is available.
    pub restart: bool,
    /// `Pause` is available.
    pub pause: bool,
    /// `Resume` is available.
    pub resume: bool,
}

impl Capabilities {
    /// Enabled commands for a connected daemon in `state`.
    pub fn for_state(state: SupervisorState) -> Self {
        use SupervisorState as S;
        match state {
            S::Stopped => Self {
                start: true,
                stop: false,
                restart: true,
                pause: false,
                resume: false,
            },
            S::Starting => Self {
                start: false,
                stop: true,
                restart: false,
                pause: false,
                resume: false,
            },
            S::Running => Self {
                start: false,
                stop: true,
                restart: true,
                pause: true,
                resume: false,
            },
            S::Backoff | S::Degraded => Self {
                start: true,
                stop: true,
                restart: true,
                pause: false,
                resume: false,
            },
            S::Paused => Self {
                start: true,
                stop: true,
                restart: false,
                pause: false,
                resume: true,
            },
        }
    }
}

/// One entry in the Presets submenu (checkmarked when active, SPEC §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetEntry {
    /// Preset name (also its menu-item id, `preset:<name>`).
    pub name: String,
    /// Whether it is the active preset (shows a checkmark).
    pub active: bool,
}

/// Everything the menu needs, computed from the daemon's last-known state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuModel {
    /// Status-bar glyph.
    pub icon: IconKind,
    /// Whether the daemon is currently reachable.
    pub connected: bool,
    /// The (disabled) state line at the top of the menu (SPEC §8).
    pub state_line: String,
    /// The (disabled) active-layer line, when a layer is known (SPEC §8).
    pub layer_line: Option<String>,
    /// Which lifecycle commands to enable.
    pub caps: Capabilities,
    /// Presets submenu entries.
    pub presets: Vec<PresetEntry>,
    /// The active preset's `.kbd` path — the target of "Edit Config" /
    /// "Validate Config" (SPEC §8); `None` disables those items (safe config
    /// or no preset selected).
    pub active_config: Option<String>,
}

impl MenuModel {
    /// No connection to the daemon: reconnect glyph, everything disabled
    /// (SPEC §8: "exponential reconnect when the daemon bounces").
    pub fn disconnected() -> Self {
        Self {
            icon: IconKind::Disconnected,
            connected: false,
            state_line: "Disconnected — reconnecting…".to_string(),
            layer_line: None,
            caps: Capabilities::default(),
            presets: Vec::new(),
            active_config: None,
        }
    }

    /// Built from a live `Status` plus the preset list (SPEC §8: "state line +
    /// active layer (disabled items) · … · Presets submenu (checkmark)").
    pub fn connected(status: &Status, presets: &[PresetInfo]) -> Self {
        Self {
            icon: IconKind::for_state(status.state),
            connected: true,
            state_line: state_line(status),
            layer_line: status.active_layer.as_ref().map(|l| format!("Layer: {l}")),
            caps: Capabilities::for_state(status.state),
            presets: presets
                .iter()
                .map(|p| PresetEntry {
                    name: p.name.clone(),
                    active: p.active,
                })
                .collect(),
            active_config: presets.iter().find(|p| p.active).map(|p| p.config.clone()),
        }
    }
}

/// The top (disabled) menu line. For `Degraded` it carries the actionable
/// reason so the fix is visible without opening the CLI (SPEC §8, §15).
fn state_line(status: &Status) -> String {
    match status.state {
        SupervisorState::Degraded => {
            let reason = status
                .last_error
                .as_deref()
                .unwrap_or("see kanatactl status");
            format!("Degraded — {}", short_reason(reason))
        }
        other => format!("State: {other:?}"),
    }
}

/// A `Degraded` reason can be a long multi-sentence fix hint; shown verbatim in
/// the disabled menu item it stretches the whole menu across the screen (HW
/// finding 2026-07-13). Keep the menu narrow with the first sentence, capped —
/// the full hint still rides the degraded notification.
fn short_reason(reason: &str) -> String {
    const MAX: usize = 56;
    let first = reason.split(". ").next().unwrap_or(reason);
    let first = first.trim_end_matches('.');
    if first.chars().count() <= MAX {
        first.to_string()
    } else {
        let head: String = first.chars().take(MAX).collect();
        format!("{}…", head.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: SupervisorState) -> Status {
        Status {
            state,
            active_preset: None,
            active_layer: None,
            kanata_pid: None,
            kanata_version: None,
            driver_ok: None,
            last_error: None,
            degraded_reason: None,
            uptime_s: 0,
            daemon_version: "0.1.0".to_string(),
        }
    }

    #[test]
    fn degraded_state_line_is_truncated_so_the_menu_stays_narrow() {
        // HW finding 2026-07-13: a long degraded reason stretched the menu
        // across the screen.
        let long = "kanata cannot reach its virtual-keyboard output — keys are NOT remapped. \
                    Likely a Karabiner driver version mismatch: install the driver pkg named in \
                    your kanata release notes, then approve it and Restart";
        let mut s = status(SupervisorState::Degraded);
        s.last_error = Some(long.to_string());
        let line = state_line(&s);
        assert!(line.starts_with("Degraded — "));
        assert!(line.chars().count() < 80, "menu line too long: {line:?}");
        // A short reason passes through intact (no ellipsis).
        let mut s2 = status(SupervisorState::Degraded);
        s2.last_error = Some("VHID daemon is not running".to_string());
        assert_eq!(state_line(&s2), "Degraded — VHID daemon is not running");
    }

    #[test]
    fn icon_covers_the_four_spec_states_plus_idle() {
        assert_eq!(
            IconKind::for_state(SupervisorState::Running),
            IconKind::Running
        );
        assert_eq!(
            IconKind::for_state(SupervisorState::Paused),
            IconKind::Paused
        );
        assert_eq!(
            IconKind::for_state(SupervisorState::Degraded),
            IconKind::Degraded
        );
        for idle in [
            SupervisorState::Stopped,
            SupervisorState::Starting,
            SupervisorState::Backoff,
        ] {
            assert_eq!(IconKind::for_state(idle), IconKind::Idle, "{idle:?}");
        }
    }

    #[test]
    fn capabilities_mirror_the_machine_transition_table() {
        use SupervisorState as S;
        let cases = [
            (
                S::Stopped,
                Capabilities {
                    start: true,
                    stop: false,
                    restart: true,
                    pause: false,
                    resume: false,
                },
            ),
            (
                S::Starting,
                Capabilities {
                    start: false,
                    stop: true,
                    restart: false,
                    pause: false,
                    resume: false,
                },
            ),
            (
                S::Running,
                Capabilities {
                    start: false,
                    stop: true,
                    restart: true,
                    pause: true,
                    resume: false,
                },
            ),
            (
                S::Backoff,
                Capabilities {
                    start: true,
                    stop: true,
                    restart: true,
                    pause: false,
                    resume: false,
                },
            ),
            (
                S::Degraded,
                Capabilities {
                    start: true,
                    stop: true,
                    restart: true,
                    pause: false,
                    resume: false,
                },
            ),
            (
                S::Paused,
                Capabilities {
                    start: true,
                    stop: true,
                    restart: false,
                    pause: false,
                    resume: true,
                },
            ),
        ];
        for (state, expected) in cases {
            assert_eq!(Capabilities::for_state(state), expected, "state {state:?}");
        }
    }

    #[test]
    fn disconnected_disables_everything() {
        let model = MenuModel::disconnected();
        assert_eq!(model.icon, IconKind::Disconnected);
        assert!(!model.connected);
        assert_eq!(model.caps, Capabilities::default());
        assert!(model.presets.is_empty());
        assert!(model.layer_line.is_none());
    }

    #[test]
    fn connected_shows_layer_line_only_when_known() {
        let mut s = status(SupervisorState::Running);
        assert_eq!(MenuModel::connected(&s, &[]).layer_line, None);
        s.active_layer = Some("nav".to_string());
        assert_eq!(
            MenuModel::connected(&s, &[]).layer_line,
            Some("Layer: nav".to_string())
        );
    }

    #[test]
    fn degraded_state_line_carries_the_reason() {
        let mut s = status(SupervisorState::Degraded);
        s.last_error = Some("kanata keeps crashing".to_string());
        assert_eq!(
            MenuModel::connected(&s, &[]).state_line,
            "Degraded — kanata keeps crashing"
        );
    }

    #[test]
    fn presets_carry_the_active_flag() {
        let presets = vec![
            PresetInfo {
                name: "main".into(),
                config: "/a.kbd".into(),
                autostart: true,
                active: true,
            },
            PresetInfo {
                name: "gaming".into(),
                config: "/b.kbd".into(),
                autostart: false,
                active: false,
            },
        ];
        let model = MenuModel::connected(&status(SupervisorState::Running), &presets);
        assert_eq!(
            model.presets,
            vec![
                PresetEntry {
                    name: "main".into(),
                    active: true
                },
                PresetEntry {
                    name: "gaming".into(),
                    active: false
                },
            ]
        );
    }
}
