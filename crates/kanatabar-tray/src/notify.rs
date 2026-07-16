//! Desktop notifications (SPEC §8): crash, entered `Degraded` (with fix hint),
//! recovery.
//!
//! Two deliveries behind one trait: pkg installs run the tray from
//! `KanataBar.app`, where [`BundledNotifier`] posts through
//! `UNUserNotificationCenter` (KanataBar's own name/icon; clicking focuses
//! the app). Unbundled dev builds can't use that framework (it requires an
//! app bundle), so they fall back to `osascript` (SPEC §18) — whose
//! notifications attribute to Script Editor, the HW Run 9 finding that
//! motivated the bundled path. The *decision* (`notification_for`) is pure
//! and unit-tested; the delivery is the untestable side effect.

use std::process::Command;

use kanatabar_core::ipc::Event;
use kanatabar_core::state::SupervisorState;

/// A place to send a user-facing notification.
pub trait Notifier: Send + Sync {
    /// Show a notification with the given title and body.
    fn notify(&self, title: &str, body: &str);
}

/// `osascript -e 'display notification …'` — needs no app bundle (SPEC §18).
pub struct OsascriptNotifier;

impl Notifier for OsascriptNotifier {
    fn notify(&self, title: &str, body: &str) {
        // Observability for the HW runbook (docs/HW-TESTS.md): the operator
        // greps tray.err.log to confirm degraded/recovered/crash notifications
        // fired. Title/body are user-facing notification text (never keystrokes
        // or .kbd contents — CLAUDE.md), so they are safe to log.
        tracing::info!(title, body, "posted notification");
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            escape(body),
            escape(title),
        );
        // `Command` never goes through a shell, so this is not shell injection
        // (SPEC §14); only the AppleScript string literal needs escaping so
        // `body`/`title` can't break out of their double quotes.
        let _ = Command::new("osascript").arg("-e").arg(script).status();
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `UNUserNotificationCenter` delivery — notifications carry KanataBar's own
/// name and icon, and clicking one focuses the app instead of Script Editor.
/// Only constructible when the process runs from an app bundle.
pub struct BundledNotifier {
    _priv: (),
}

impl BundledNotifier {
    /// `Some` when running from an app bundle (the pkg install layout);
    /// requests notification authorization — the one-time system prompt —
    /// as a side effect. `None` for unbundled dev builds, whose callers fall
    /// back to [`OsascriptNotifier`].
    pub fn new() -> Option<Self> {
        let bundle = crate::ffi::bundle_identifier()?;
        tracing::info!(%bundle, "bundled: notifications via UNUserNotificationCenter");
        crate::ffi::request_notification_authorization();
        Some(Self { _priv: () })
    }
}

impl Notifier for BundledNotifier {
    fn notify(&self, title: &str, body: &str) {
        // Same observability chokepoint as the osascript path — the HW
        // runbook greps tray.err.log for this line.
        tracing::info!(title, body, "posted notification");
        crate::ffi::post_notification(title, body);
    }
}

/// Decide whether `event` warrants a notification, and its `(title, body)`.
///
/// "Recovery" is scoped to leaving `Degraded` specifically — a routine
/// crash → `Backoff` → `Running` cycle is normal operation and would be noisy
/// to announce every time (SPEC §8).
pub fn notification_for(event: &Event) -> Option<(String, String)> {
    match event {
        Event::Crash { code, signal } => Some((
            "KanataBar".to_string(),
            format!("kanata crashed ({})", describe_exit(*code, *signal)),
        )),
        Event::StateChanged {
            to: SupervisorState::Degraded,
            reason,
            ..
        } => Some((
            "KanataBar — Degraded".to_string(),
            reason
                .map(|r| r.describe().to_string())
                .unwrap_or_else(|| "check kanatactl status".to_string()),
        )),
        Event::StateChanged {
            from: SupervisorState::Degraded,
            to: SupervisorState::Running,
            ..
        } => Some((
            "KanataBar".to_string(),
            "Recovered — kanata is running again".to_string(),
        )),
        _ => None,
    }
}

fn describe_exit(code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (Some(code), _) => format!("exit code {code}"),
        (None, Some(signal)) => format!("signal {signal}"),
        (None, None) => "unknown reason".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanatabar_core::ipc::ErrorKind;
    use kanatabar_core::state::DegradedReason;

    #[test]
    fn escape_keeps_the_applescript_literal_closed() {
        assert_eq!(escape(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape(r"back\slash"), r"back\\slash");
    }

    #[test]
    fn bundled_notifier_requires_an_app_bundle() {
        // Test binaries are unbundled, so this doubles as the dev-fallback
        // guarantee: no bundle → no BundledNotifier → osascript path.
        assert!(BundledNotifier::new().is_none());
    }

    #[test]
    fn crash_notifies_with_the_exit_reason() {
        let (title, body) = notification_for(&Event::Crash {
            code: Some(11),
            signal: None,
        })
        .expect("crash notifies");
        assert_eq!(title, "KanataBar");
        assert!(body.contains("exit code 11"), "{body}");

        let (_, sig) = notification_for(&Event::Crash {
            code: None,
            signal: Some(9),
        })
        .expect("signal crash notifies");
        assert!(sig.contains("signal 9"), "{sig}");
    }

    #[test]
    fn entering_degraded_notifies_with_the_fix_hint() {
        let (_, body) = notification_for(&Event::StateChanged {
            from: SupervisorState::Backoff,
            to: SupervisorState::Degraded,
            reason: Some(DegradedReason::DriverNotActivated),
        })
        .expect("degraded notifies");
        assert_eq!(body, DegradedReason::DriverNotActivated.describe());
    }

    #[test]
    fn leaving_degraded_for_running_notifies_recovery() {
        assert!(notification_for(&Event::StateChanged {
            from: SupervisorState::Degraded,
            to: SupervisorState::Running,
            reason: None,
        })
        .is_some());
    }

    #[test]
    fn routine_backoff_recovery_is_silent() {
        assert_eq!(
            notification_for(&Event::StateChanged {
                from: SupervisorState::Backoff,
                to: SupervisorState::Running,
                reason: None,
            }),
            None
        );
    }

    #[test]
    fn layer_and_device_changes_are_silent() {
        assert_eq!(
            notification_for(&Event::LayerChanged {
                layer: "nav".into()
            }),
            None
        );
        assert_eq!(
            notification_for(&Event::DeviceChanged {
                added: true,
                name: "Keychron K2".into(),
            }),
            None
        );
        assert_eq!(
            notification_for(&Event::DriverIssue {
                kind: ErrorKind::VhidDaemonDown,
                message: "down".into(),
            }),
            None
        );
    }
}
