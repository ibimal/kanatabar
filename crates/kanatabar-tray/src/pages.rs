//! The Phase 12 page↔shell ipc protocol (docs/design/phase12-ui-layer.md).
//!
//! Embedded pages talk to the shell only through short opaque messages; this
//! module is the single parser, in the lib so the protocol is [AUTO]-tested.
//! The security posture: pages never send commands, paths, or URLs — actions
//! carry an *index* into the static [`crate::wizard::steps`] table, validated
//! here, and the shell executes only what that table says.

use crate::wizard;

/// Which Phase 12 window a message came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageId {
    /// Devices window (SPEC §8).
    Devices,
    /// Health Check window (SPEC §11.3).
    Health,
    /// Setup Assistant window (SPEC §11.2).
    Wizard,
}

/// A parsed, validated page message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PageMessage {
    /// The page loaded and can accept `__render` calls.
    Ready,
    /// The page asked to close (Escape).
    Close,
    /// The page reported its natural content height (logical px).
    Height(f64),
    /// Health page: open the Setup Assistant (§11.3 tier-2 delegation).
    OpenWizard,
    /// Wizard page: run step `index`'s `run` argv ("Do it for me").
    RunStep(usize),
    /// Wizard page: `open(1)` step `index`'s open target.
    OpenStep(usize),
}

/// Parse a raw ipc message from `page`. Unknown/malformed/out-of-place
/// messages return `None` — silently dropped, never "best-effort" executed.
pub fn parse(page: PageId, message: &str) -> Option<PageMessage> {
    match message {
        "ready" => return Some(PageMessage::Ready),
        "close" => return Some(PageMessage::Close),
        "open-wizard" if page == PageId::Health => return Some(PageMessage::OpenWizard),
        _ => {}
    }
    if let Some(height) = message.strip_prefix("height:") {
        return height
            .parse::<f64>()
            .ok()
            .filter(|h| h.is_finite() && *h >= 0.0)
            .map(PageMessage::Height);
    }
    if page == PageId::Wizard {
        let steps = wizard::steps();
        if let Some(index) = parse_index(message, "run:", steps.len()) {
            // Only steps that declare a run argv are runnable.
            return steps[index].run.map(|_| PageMessage::RunStep(index));
        }
        if let Some(index) = parse_index(message, "open:", steps.len()) {
            return steps[index].open.map(|_| PageMessage::OpenStep(index));
        }
    }
    None
}

fn parse_index(message: &str, prefix: &str, len: usize) -> Option<usize> {
    message
        .strip_prefix(prefix)
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|i| *i < len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_messages_parse_for_every_page() {
        for page in [PageId::Devices, PageId::Health, PageId::Wizard] {
            assert_eq!(parse(page, "ready"), Some(PageMessage::Ready));
            assert_eq!(parse(page, "close"), Some(PageMessage::Close));
            assert_eq!(
                parse(page, "height:512.5"),
                Some(PageMessage::Height(512.5))
            );
            assert_eq!(parse(page, "height:nan"), None);
            assert_eq!(parse(page, "height:-4"), None);
            assert_eq!(parse(page, "reboot"), None);
        }
    }

    #[test]
    fn open_wizard_is_health_only() {
        assert_eq!(
            parse(PageId::Health, "open-wizard"),
            Some(PageMessage::OpenWizard)
        );
        assert_eq!(parse(PageId::Devices, "open-wizard"), None);
        assert_eq!(parse(PageId::Wizard, "open-wizard"), None);
    }

    #[test]
    fn step_actions_are_wizard_only_and_validated_against_the_table() {
        let steps = wizard::steps();
        let runnable = steps.iter().position(|s| s.run.is_some()).expect("exists");
        let openable = steps.iter().position(|s| s.open.is_some()).expect("exists");
        let inert = steps
            .iter()
            .position(|s| s.run.is_none() && s.open.is_none())
            .expect("exists");

        assert_eq!(
            parse(PageId::Wizard, &format!("run:{runnable}")),
            Some(PageMessage::RunStep(runnable))
        );
        assert_eq!(
            parse(PageId::Wizard, &format!("open:{openable}")),
            Some(PageMessage::OpenStep(openable))
        );
        // A step with no run/open can't be actioned even with a valid index.
        assert_eq!(parse(PageId::Wizard, &format!("run:{inert}")), None);
        assert_eq!(parse(PageId::Wizard, &format!("open:{inert}")), None);
        // Out-of-bounds and non-wizard pages are rejected.
        assert_eq!(parse(PageId::Wizard, &format!("run:{}", steps.len())), None);
        assert_eq!(parse(PageId::Health, &format!("run:{runnable}")), None);
        assert_eq!(parse(PageId::Wizard, "run:abc"), None);
    }
}
