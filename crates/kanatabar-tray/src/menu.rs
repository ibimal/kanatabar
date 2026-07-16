//! Menu item identity: the stable ids the GUI shell assigns to `muda` menu
//! items, and the pure mapping from a clicked id to the control request it
//! issues (SPEC §7.2, §8). Keeping the mapping here — free of any UI type —
//! makes "which click does what" unit-testable without a display.

use kanatabar_core::ipc::RequestPayload;

/// Stable menu-item ids (also used to rebuild the menu without losing identity
/// across refreshes).
pub mod ids {
    /// Start kanata.
    pub const START: &str = "start";
    /// Stop kanata.
    pub const STOP: &str = "stop";
    /// Restart kanata.
    pub const RESTART: &str = "restart";
    /// Pause remapping.
    pub const PAUSE: &str = "pause";
    /// Resume remapping.
    pub const RESUME: &str = "resume";
    /// Open the active preset's `.kbd` in the default editor (SPEC §8).
    pub const EDIT_CONFIG: &str = "edit-config";
    /// Re-validate the active preset's `.kbd` and report (SPEC §8).
    pub const VALIDATE_CONFIG: &str = "validate-config";
    /// Show the daemon's device list (SPEC §8).
    pub const DEVICES: &str = "devices";
    /// Open the daemon log directory (SPEC §8 "View Logs").
    pub const VIEW_LOGS: &str = "view-logs";
    /// Toggle "Launch at Login" (the per-user LaunchAgent).
    pub const LOGIN: &str = "login";
    /// Run `doctor` and show the result (SPEC §9).
    pub const DOCTOR: &str = "doctor";
    /// Open the first-run setup wizard steps (SPEC §11).
    pub const WIZARD: &str = "wizard";
    /// Quit the tray (leaves the daemon running).
    pub const QUIT: &str = "quit";
    /// Prefix for a preset item; the suffix is the preset name.
    pub const PRESET_PREFIX: &str = "preset:";
}

/// The control request a clicked menu id maps to, or `None` for items handled
/// entirely in the shell (Quit, the disabled status lines, submenu labels).
pub fn payload_for(id: &str) -> Option<RequestPayload> {
    match id {
        ids::START => Some(RequestPayload::Start),
        ids::STOP => Some(RequestPayload::Stop),
        ids::RESTART => Some(RequestPayload::Restart),
        ids::PAUSE => Some(RequestPayload::Pause),
        ids::RESUME => Some(RequestPayload::Resume),
        other => other
            .strip_prefix(ids::PRESET_PREFIX)
            .map(|name| RequestPayload::SwitchPreset {
                name: name.to_string(),
            }),
    }
}

/// The stable id for a preset item.
pub fn preset_id(name: &str) -> String {
    format!("{}{name}", ids::PRESET_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_ids_map_to_their_requests() {
        assert_eq!(payload_for(ids::START), Some(RequestPayload::Start));
        assert_eq!(payload_for(ids::STOP), Some(RequestPayload::Stop));
        assert_eq!(payload_for(ids::RESTART), Some(RequestPayload::Restart));
        assert_eq!(payload_for(ids::PAUSE), Some(RequestPayload::Pause));
        assert_eq!(payload_for(ids::RESUME), Some(RequestPayload::Resume));
    }

    #[test]
    fn shell_handled_and_unknown_ids_have_no_request() {
        // Quit, the login toggle, doctor, the wizard, and the config/devices/
        // logs items are handled entirely in the shell (they need paths or
        // multi-frame replies, not a single fire-and-forget command).
        assert_eq!(payload_for(ids::QUIT), None);
        assert_eq!(payload_for(ids::LOGIN), None);
        assert_eq!(payload_for(ids::DOCTOR), None);
        assert_eq!(payload_for(ids::WIZARD), None);
        assert_eq!(payload_for(ids::EDIT_CONFIG), None);
        assert_eq!(payload_for(ids::VALIDATE_CONFIG), None);
        assert_eq!(payload_for(ids::DEVICES), None);
        assert_eq!(payload_for(ids::VIEW_LOGS), None);
        assert_eq!(payload_for("status-line"), None);
        assert_eq!(payload_for(""), None);
    }

    #[test]
    fn preset_id_round_trips_to_a_switch_request() {
        let id = preset_id("gaming");
        assert_eq!(id, "preset:gaming");
        assert_eq!(
            payload_for(&id),
            Some(RequestPayload::SwitchPreset {
                name: "gaming".to_string()
            })
        );
    }

    #[test]
    fn preset_name_with_a_colon_is_preserved() {
        // Only the first `preset:` prefix is stripped; the rest is the name.
        let id = preset_id("weird:name");
        assert_eq!(
            payload_for(&id),
            Some(RequestPayload::SwitchPreset {
                name: "weird:name".to_string()
            })
        );
    }
}
