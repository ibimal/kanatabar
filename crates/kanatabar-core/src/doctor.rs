//! Shared `doctor` vocabulary (SPEC §9, §11).
//!
//! `doctor` runs the full preflight and reports a `DoctorReport` (SPEC §7.2);
//! the tray's first-run wizard is UI over the *same* checks (SPEC §11:
//! "single implementation in core/daemon"). The check *execution* is I/O and
//! lives in the daemon; what lives here is the pure, OS-independent part every
//! side must agree on: the **stable check names** (so the wizard can map a step
//! to the check that verifies it, and scripts can key off them) and the
//! **pass/fail aggregation**. Keeping the schema anchored here is what the
//! Phase 8 `[AUTO]` gate ("doctor JSON schema stable") pins.

use crate::ipc::DoctorCheck;

/// Stable [`DoctorCheck::name`] values (SPEC §9). These are part of the wire
/// contract: the daemon emits them, the CLI/wizard match on them — do not
/// rename without bumping the protocol.
pub mod checks {
    /// The daemon is reachable and responding.
    pub const DAEMON: &str = "daemon";
    /// The kanata binary is present (and its version, when known).
    pub const KANATA_BINARY: &str = "kanata binary";
    /// The Karabiner-DriverKit-VirtualHIDDevice pkg is installed on disk (the
    /// VirtualHIDDevice-Manager app is present) — distinct from [`DRIVER`],
    /// which additionally requires the extension to be `activated enabled`.
    /// The wizard's *install* step verifies this while its *activate* step
    /// verifies [`DRIVER`], so the two steps have distinct checks and the
    /// wizard can advance install → activate (SPEC §11; HW Run 3, 2026-07-13).
    pub const DRIVER_PRESENT: &str = "driver present";
    /// The Karabiner DriverKit extension is `activated enabled`.
    pub const DRIVER: &str = "karabiner driver";
    /// The installed driver version matches what the installed kanata
    /// supports (SPEC §2 version coupling; report-only where unknown).
    pub const DRIVER_VERSION: &str = "driver version";
    /// The Karabiner VirtualHIDDevice daemon is running.
    pub const VHID_DAEMON: &str = "vhid daemon";
    /// Something supervises the VHID daemon across reboots (SPEC §6.5a):
    /// KanataBar's LaunchDaemon, Karabiner-Elements, or a user plist.
    pub const VHID_MANAGED: &str = "vhid daemon managed";
    /// Input Monitoring permission (best-effort; SPEC §2, §11 [VERIFY]).
    pub const INPUT_MONITORING: &str = "input monitoring";
    /// The control socket exists with the expected permissions (SPEC §3.2).
    pub const CONTROL_SOCKET: &str = "control socket";
    /// The active preset's `.kbd` passes `kanata --check` (SPEC §6.4).
    pub const ACTIVE_CONFIG: &str = "active config";
    /// `config.toml` parsed (or is absent) — a present-but-broken file is a
    /// failure, so presets/defaults are never silently discarded (SPEC §7.3).
    pub const CONFIG_FILE: &str = "config file";
    /// The supervisor is in a healthy (non-degraded) state.
    pub const SUPERVISOR: &str = "supervisor";
}

/// Every check name, in the canonical report order — the wizard and tests
/// iterate this rather than hard-coding the list.
pub const ALL_CHECKS: [&str; 12] = [
    checks::DAEMON,
    checks::KANATA_BINARY,
    checks::DRIVER_PRESENT,
    checks::DRIVER,
    checks::DRIVER_VERSION,
    checks::VHID_DAEMON,
    checks::VHID_MANAGED,
    checks::INPUT_MONITORING,
    checks::CONTROL_SOCKET,
    checks::ACTIVE_CONFIG,
    checks::CONFIG_FILE,
    checks::SUPERVISOR,
];

/// The setup-class checks (SPEC §11.1): the wizard owns their fix, and the
/// doctor window delegates their failures to it ([HARD] anti-overlap rule).
/// Everything else in [`ALL_CHECKS`] is runtime-class (doctor-only).
pub const SETUP_CHECKS: [&str; 6] = [
    checks::DRIVER_PRESENT,
    checks::DRIVER_VERSION,
    checks::DRIVER,
    checks::VHID_MANAGED,
    checks::INPUT_MONITORING,
    checks::DAEMON,
];

/// Whether `name` is a setup-class check (SPEC §11.1).
pub fn is_setup_check(name: &str) -> bool {
    SETUP_CHECKS.contains(&name)
}

/// Setup is complete ⇔ every setup-class check passes (SPEC §11.1). Drives
/// the wizard's auto-open (and never-auto-open-again) behavior. Checks absent
/// from the report don't count as failing — a partial report (e.g. daemon
/// unreachable produces only the daemon check) still evaluates honestly.
pub fn setup_complete(checks: &[DoctorCheck]) -> bool {
    checks
        .iter()
        .filter(|c| is_setup_check(&c.name))
        .all(|c| c.ok)
}

/// True when every check passed — the overall doctor verdict (SPEC §9 exit
/// codes: 0 when all green).
pub fn all_ok(checks: &[DoctorCheck]) -> bool {
    checks.iter().all(|c| c.ok)
}

/// How many checks failed (for a one-line summary in the tray/notification).
pub fn failed_count(checks: &[DoctorCheck]) -> usize {
    checks.iter().filter(|c| !c.ok).count()
}

/// Render the full checklist as human-readable text — one `✅/❌ name: detail`
/// line per check, each failure's `fix_hint` beneath it, and a summary line.
/// Shared so the CLI (`kanatactl doctor`) and the tray's "Run Doctor" (which
/// has no terminal, so it opens this as a file) show the identical report.
pub fn format_report(checks: &[DoctorCheck]) -> String {
    let mut out = String::new();
    for check in checks {
        let mark = if check.ok { "✅" } else { "❌" };
        out.push_str(&format!("{mark} {}: {}\n", check.name, check.detail));
        if let Some(hint) = &check.fix_hint {
            out.push_str(&format!("   ↳ {hint}\n"));
        }
    }
    match failed_count(checks) {
        0 => out.push_str("\nAll checks passed.\n"),
        n => out.push_str(&format!("\n{n} check(s) failed.\n")),
    }
    out
}

/// A one-line notification summary naming the failing checks (banner-friendly:
/// no newlines, which the osascript fallback can't render). `None` when every
/// check passed. The full detail lives in [`format_report`].
pub fn format_failures_summary(checks: &[DoctorCheck]) -> Option<String> {
    let names: Vec<&str> = checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| c.name.as_str())
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "{} check(s) need attention: {}",
        names.len(),
        names.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, ok: bool) -> DoctorCheck {
        DoctorCheck {
            name: name.to_string(),
            ok,
            detail: "detail".to_string(),
            fix_hint: if ok { None } else { Some("fix it".to_string()) },
        }
    }

    #[test]
    fn all_ok_is_true_only_when_no_check_failed() {
        assert!(all_ok(&[check("a", true), check("b", true)]));
        assert!(!all_ok(&[check("a", true), check("b", false)]));
        assert!(all_ok(&[])); // vacuously true
    }

    #[test]
    fn format_report_marks_and_hints_each_check() {
        let report = format_report(&[check("daemon", true), check("driver", false)]);
        assert!(report.contains("✅ daemon: detail"));
        assert!(report.contains("❌ driver: detail"));
        assert!(report.contains("   ↳ fix it")); // failure hint present
        assert!(report.contains("1 check(s) failed"));
        // All-green report ends with the pass line, no hints.
        let green = format_report(&[check("daemon", true)]);
        assert!(green.contains("All checks passed."));
        assert!(!green.contains("↳"));
    }

    #[test]
    fn failures_summary_names_failures_on_one_line() {
        assert_eq!(format_failures_summary(&[check("daemon", true)]), None);
        let summary =
            format_failures_summary(&[check("daemon", false), check("driver", false)]).unwrap();
        assert!(summary.contains("2 check(s) need attention"));
        assert!(summary.contains("daemon, driver"));
        assert!(!summary.contains('\n'), "banner body must be single-line");
    }

    #[test]
    fn failed_count_counts_failures() {
        assert_eq!(failed_count(&[check("a", true), check("b", false)]), 1);
        assert_eq!(failed_count(&[check("a", false), check("b", false)]), 2);
    }

    #[test]
    fn setup_checks_are_a_subset_of_all_checks_and_classify() {
        for name in SETUP_CHECKS {
            assert!(ALL_CHECKS.contains(&name), "unknown setup check {name}");
            assert!(is_setup_check(name));
        }
        // Runtime-class = the complement; spot-check the split (SPEC §11.1).
        for name in [
            checks::KANATA_BINARY,
            checks::VHID_DAEMON,
            checks::CONTROL_SOCKET,
            checks::ACTIVE_CONFIG,
            checks::CONFIG_FILE,
            checks::SUPERVISOR,
        ] {
            assert!(!is_setup_check(name), "{name} must be runtime-class");
        }
    }

    #[test]
    fn setup_complete_ignores_runtime_failures() {
        // A broken active config is runtime-class: setup stays complete.
        let mut all: Vec<DoctorCheck> = ALL_CHECKS.map(|n| check(n, true)).to_vec();
        assert!(setup_complete(&all));
        all.iter_mut()
            .find(|c| c.name == checks::ACTIVE_CONFIG)
            .expect("present")
            .ok = false;
        assert!(setup_complete(&all), "runtime failure is not a setup gap");
        // But any setup-class failure flips it.
        all.iter_mut()
            .find(|c| c.name == checks::INPUT_MONITORING)
            .expect("present")
            .ok = false;
        assert!(!setup_complete(&all));
        // Vacuously complete on an empty report (partial reports stay honest).
        assert!(setup_complete(&[]));
    }

    #[test]
    fn all_checks_list_has_no_duplicates() {
        let mut seen = std::collections::BTreeSet::new();
        for name in ALL_CHECKS {
            assert!(seen.insert(name), "duplicate check name {name}");
        }
    }

    /// The `[AUTO]` gate's anchor: the `DoctorCheck` wire shape is stable
    /// (field names/optionality), compared as a JSON value so field order is
    /// irrelevant (SPEC §7.2, §19).
    #[test]
    fn doctor_check_json_schema_is_stable() {
        let failed = serde_json::to_value(check(checks::DRIVER, false)).unwrap();
        assert_eq!(
            failed,
            serde_json::json!({
                "name": "karabiner driver",
                "ok": false,
                "detail": "detail",
                "fix_hint": "fix it",
            })
        );
        // A passing check carries a null fix_hint (present, not omitted).
        let ok = serde_json::to_value(check(checks::DAEMON, true)).unwrap();
        assert_eq!(ok["fix_hint"], serde_json::Value::Null);
        assert_eq!(ok["ok"], serde_json::Value::Bool(true));
    }
}
