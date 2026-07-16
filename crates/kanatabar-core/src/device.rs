//! Pure device-relevance rules (SPEC §6.3).
//!
//! Decide which registry changes warrant a kanata re-sync restart: keyboards
//! only, excluding the Karabiner virtual device and kanata's own outputs, to
//! avoid restart feedback loops [HARD]. No I/O — the daemon reads the IOKit
//! registry (presence only, no key events) and feeds descriptors here.

/// Whether a device was added to or removed from the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceChange {
    /// A device appeared (matched notification).
    Added,
    /// A device was removed (terminated notification).
    Removed,
}

/// Registry facts about a device (presence only, SPEC §6.3). Numeric fields are
/// optional because they may be unreadable on termination.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceDescriptor {
    /// Device "Product" name.
    pub name: String,
    /// USB/HID vendor id.
    pub vendor_id: Option<u32>,
    /// HID primary usage page.
    pub usage_page: Option<u32>,
    /// HID primary usage.
    pub usage: Option<u32>,
}

/// HID Generic Desktop usage page (USB HID Usage Tables).
const USAGE_PAGE_GENERIC_DESKTOP: u32 = 0x01;
/// HID Keyboard usage.
const USAGE_KEYBOARD: u32 = 0x06;
/// Karabiner VirtualHIDDevice vendor id. [VERIFY] against the installed driver.
const KARABINER_VENDOR_ID: u32 = 0x16C0;

/// Whether a HID usage-page/usage pair identifies a keyboard.
pub fn is_keyboard(usage_page: u32, usage: u32) -> bool {
    usage_page == USAGE_PAGE_GENERIC_DESKTOP && usage == USAGE_KEYBOARD
}

/// The Karabiner virtual keyboard — which is also how kanata's own output
/// appears — matched by name or vendor. Filtered to avoid restart feedback
/// loops [HARD] (SPEC §6.3). [VERIFY] the exact name/vendor per driver version.
pub fn is_karabiner_virtual(name: &str, vendor_id: Option<u32>) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("karabiner")
        || lower.contains("virtualhid")
        || vendor_id == Some(KARABINER_VENDOR_ID)
}

/// Whether a registry change should trigger a debounced re-sync restart.
///
/// Excludes the Karabiner virtual device always. A device with known usage
/// counts only if it is a keyboard. When the usage is unknown (common on
/// termination), only a removal counts — so unplugging a keyboard still
/// re-syncs, but an unclassifiable *addition* is ignored to avoid thrash.
pub fn is_resync_relevant(change: DeviceChange, desc: &DeviceDescriptor) -> bool {
    if is_karabiner_virtual(&desc.name, desc.vendor_id) {
        return false;
    }
    match (desc.usage_page, desc.usage) {
        (Some(page), Some(usage)) => is_keyboard(page, usage),
        _ => matches!(change, DeviceChange::Removed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kbd() -> DeviceDescriptor {
        DeviceDescriptor {
            name: "Keychron K2".into(),
            vendor_id: Some(0x05AC),
            usage_page: Some(0x01),
            usage: Some(0x06),
        }
    }

    fn mouse() -> DeviceDescriptor {
        DeviceDescriptor {
            name: "Logitech Mouse".into(),
            vendor_id: Some(0x046D),
            usage_page: Some(0x01),
            usage: Some(0x02), // pointer
        }
    }

    #[test]
    fn keyboard_add_and_remove_are_relevant() {
        assert!(is_resync_relevant(DeviceChange::Added, &kbd()));
        assert!(is_resync_relevant(DeviceChange::Removed, &kbd()));
    }

    #[test]
    fn non_keyboard_is_ignored() {
        assert!(!is_resync_relevant(DeviceChange::Added, &mouse()));
        assert!(!is_resync_relevant(DeviceChange::Removed, &mouse()));
    }

    #[test]
    fn karabiner_virtual_is_ignored_by_name() {
        let dev = DeviceDescriptor {
            name: "Karabiner DriverKit VirtualHIDKeyboard".into(),
            vendor_id: Some(0x16C0),
            usage_page: Some(0x01),
            usage: Some(0x06),
        };
        assert!(!is_resync_relevant(DeviceChange::Added, &dev));
    }

    #[test]
    fn karabiner_virtual_is_ignored_by_vendor() {
        let dev = DeviceDescriptor {
            name: "some keyboard".into(),
            vendor_id: Some(KARABINER_VENDOR_ID),
            usage_page: Some(0x01),
            usage: Some(0x06),
        };
        assert!(!is_resync_relevant(DeviceChange::Added, &dev));
    }

    #[test]
    fn unknown_usage_relevant_only_on_removal() {
        let dev = DeviceDescriptor {
            name: "mystery device".into(),
            vendor_id: Some(0x1234),
            usage_page: None,
            usage: None,
        };
        assert!(!is_resync_relevant(DeviceChange::Added, &dev));
        assert!(is_resync_relevant(DeviceChange::Removed, &dev));
    }
}
