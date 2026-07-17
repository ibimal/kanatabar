//! Devices-window view-model (SPEC §8, Phase 12).
//!
//! Pure and display-free (the Phase 12 [AUTO] gate tests this module; nothing
//! here touches tao/wry): `Vec<DeviceInfo>` in, a serializable [`DevicesView`]
//! out. The GUI shell serializes it with serde and pushes it into the webview
//! as a single `render(<json>)` call — the JS side inserts every string with
//! `textContent`, so nothing in this module needs to know about HTML escaping
//! (docs/design/phase12-ui-layer.md, webview hardening).

use kanatabar_core::ipc::DeviceInfo;
use serde::Serialize;

/// Everything the devices page renders, in display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DevicesView {
    /// One-line summary shown under the title ("3 devices · 1 matched").
    pub summary: String,
    /// Fetch failure to display instead of rows, when the daemon call failed.
    pub error: Option<String>,
    /// Device rows, matched first, then case-insensitively by name.
    pub rows: Vec<DeviceRow>,
}

/// One device row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceRow {
    /// Display name: the device's own, or `DeviceInfo::UNNAMED` when the
    /// device reported no product string (HW Run 10 finding: real IOHID
    /// devices can be nameless, which rendered as blank rows).
    pub name: String,
    /// Whether kanata is currently matching/grabbing it.
    pub matched: bool,
    /// True when `name` is the placeholder (the page dims these rows).
    pub unnamed: bool,
}

/// Build the view for a fetched device list. Matched devices sort first (they
/// are what the user opened the window to confirm), then named devices
/// case-insensitively, then unnamed ones; the sort is stable so equal keys
/// keep daemon order.
pub fn view(devices: &[DeviceInfo]) -> DevicesView {
    let mut rows: Vec<DeviceRow> = devices
        .iter()
        .map(|d| DeviceRow {
            name: d.display_name().to_string(),
            matched: d.matched,
            unnamed: d.is_unnamed(),
        })
        .collect();
    rows.sort_by(|a, b| {
        b.matched
            .cmp(&a.matched)
            .then_with(|| a.unnamed.cmp(&b.unnamed))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    DevicesView {
        summary: summary(&rows),
        error: None,
        rows,
    }
}

/// Build the view for a failed fetch (daemon unreachable / request rejected).
pub fn error(message: &str) -> DevicesView {
    DevicesView {
        summary: "Device list unavailable".to_string(),
        error: Some(message.to_string()),
        rows: Vec::new(),
    }
}

fn summary(rows: &[DeviceRow]) -> String {
    if rows.is_empty() {
        return "No input devices visible".to_string();
    }
    let matched = rows.iter().filter(|r| r.matched).count();
    let devices = match rows.len() {
        1 => "1 device".to_string(),
        n => format!("{n} devices"),
    };
    match matched {
        0 => format!("{devices} · none matched"),
        n => format!("{devices} · {n} matched"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, matched: bool) -> DeviceInfo {
        DeviceInfo {
            name: name.to_string(),
            matched,
        }
    }

    #[test]
    fn matched_devices_sort_first_then_names_case_insensitively() {
        let view = view(&[
            dev("zsa voyager", false),
            dev("Apple Internal Keyboard", false),
            dev("G502 X", true),
        ]);
        let names: Vec<&str> = view.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["G502 X", "Apple Internal Keyboard", "zsa voyager"]);
        assert!(view.rows[0].matched);
        assert_eq!(view.error, None);
    }

    #[test]
    fn nameless_devices_get_the_placeholder_and_sort_last() {
        // The HW Run 10 finding (2026-07-17): real IOHID devices can report
        // no product string — they must render as a dimmed placeholder, not
        // a blank row, and sit below the named devices.
        let view = view(&[
            dev("", false),
            dev("BTM", false),
            dev("   ", false),
            dev("Apple Internal Keyboard", false),
        ]);
        let names: Vec<&str> = view.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Apple Internal Keyboard",
                "BTM",
                DeviceInfo::UNNAMED,
                DeviceInfo::UNNAMED
            ]
        );
        assert!(!view.rows[0].unnamed);
        assert!(view.rows[2].unnamed && view.rows[3].unnamed);
        // ... but a matched device still leads, nameless or not.
        let matched = super::view(&[dev("BTM", false), dev("", true)]);
        assert_eq!(matched.rows[0].name, DeviceInfo::UNNAMED);
        assert!(matched.rows[0].matched);
        assert_eq!(matched.summary, "2 devices · 1 matched");
    }

    #[test]
    fn summary_counts_devices_and_matches() {
        assert_eq!(
            view(&[dev("a", true), dev("b", false)]).summary,
            "2 devices · 1 matched"
        );
        assert_eq!(view(&[dev("a", false)]).summary, "1 device · none matched");
        assert_eq!(view(&[]).summary, "No input devices visible");
    }

    #[test]
    fn error_view_carries_the_message_and_no_rows() {
        let view = error("connect failed");
        assert_eq!(view.error.as_deref(), Some("connect failed"));
        assert!(view.rows.is_empty());
    }

    /// The JSON shape the embedded page's `render()` consumes — pinned like
    /// the doctor schema so an HTML/model edit can't silently drift apart.
    #[test]
    fn view_json_shape_is_stable() {
        let json = serde_json::to_value(view(&[dev("G502 X", true)])).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({
                "summary": "1 device · 1 matched",
                "error": null,
                "rows": [{ "name": "G502 X", "matched": true, "unnamed": false }],
            })
        );
    }
}
