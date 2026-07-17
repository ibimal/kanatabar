//! First-run setup wizard model (SPEC §11).
//!
//! The wizard walks a fresh machine through: install the Karabiner driver →
//! activate + approve the system extension → grant Input Monitoring → install
//! the daemon + agent → run `doctor` and show a green checklist. Per SPEC §11
//! it is "UI over the same checks `kanatactl doctor` runs (single
//! implementation in core/daemon)": each step names the `doctor` check that
//! verifies it (`kanatabar_core::doctor::checks`), so the wizard never
//! re-implements a check — it opens the relevant pane and re-runs `doctor`.
//!
//! This module is the pure, testable step model; the GUI shell renders it and
//! the actual verification is the daemon's `Doctor` handler. Every step is
//! re-checkable (SPEC §11).

use std::collections::BTreeSet;

use kanatabar_core::doctor::checks;
use kanatabar_core::ipc::DoctorCheck;
use kanatabar_core::state::DegradedReason;

/// One wizard step (SPEC §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WizardStep {
    /// Short step title.
    pub title: &'static str,
    /// What the user must do.
    pub instruction: &'static str,
    /// A command (argv) the wizard runs *for* the user when it lands on this
    /// step — e.g. the Karabiner manager's `activate` request, which is
    /// documented to run without sudo in the user's session (SPEC §11 step 2:
    /// activation is scriptable; only the approval click is not).
    pub run: Option<&'static [&'static str]>,
    /// A URL or `x-apple.systempreferences:` pane to `open(1)` for this step,
    /// when one applies (SPEC §11: "openable, not clickable, by us").
    pub open: Option<&'static str>,
    /// A command for the *user* to run, shown as copyable text — steps that
    /// need sudo, which the tray never runs itself (SPEC §11.2 [HARD]).
    pub copy: Option<&'static str>,
    /// A TCC permission the wizard can ask the *daemon* to request for the
    /// user ("Set it up for me"): the daemon registers kanatad's entry in the
    /// Privacy pane, then the user toggles it on. The request runs in kanatad,
    /// not the tray — TCC grants attach to the calling process and kanatad is
    /// the responsible process (SPEC §2 [HARD]).
    pub request: Option<kanatabar_core::ipc::PermissionKind>,
    /// The `doctor` check name that verifies this step
    /// (`kanatabar_core::doctor::checks`), when the step maps to one.
    pub verifies: Option<&'static str>,
}

/// The Karabiner extension-activation request (pqrs README, checked 2026-07):
/// `Karabiner-VirtualHIDDevice-Manager activate`, no sudo. Idempotent — safe
/// to re-run on every wizard pass.
pub const ACTIVATE_DRIVER_CMD: &[&str] = &[kanatabar_core::vhidd::MANAGER_BINARY, "activate"];

/// The System Settings panes the wizard can open (SPEC §11). [VERIFY] the exact
/// anchors per macOS version — they are documented in docs/HW-TESTS.md.
pub mod panes {
    /// Login Items & Extensions (system-extension approval lives here on
    /// recent macOS; older releases use the Security pane's "Extensions…").
    pub const EXTENSIONS: &str =
        "x-apple.systempreferences:com.apple.preference.security?Extensions";
    /// Privacy & Security → Input Monitoring.
    pub const INPUT_MONITORING: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";
    /// Privacy & Security → Accessibility (some kanata configs need it too —
    /// SPEC §2 [VERIFY]).
    pub const ACCESSIBILITY: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
}

/// Where to obtain the Karabiner DriverKit VirtualHIDDevice driver (SPEC §11,
/// §23). [VERIFY] the exact commands/version per Karabiner release.
pub const KARABINER_DRIVER_URL: &str =
    "https://github.com/pqrs-org/Karabiner-DriverKit-VirtualHIDDevice/releases";

/// The ordered wizard steps (SPEC §11).
pub fn steps() -> Vec<WizardStep> {
    vec![
        WizardStep {
            title: "Install the Karabiner driver",
            instruction: "kanata needs the Karabiner-DriverKit-VirtualHIDDevice driver — \
                          install the version named in your kanata release notes \
                          (SPEC §2: the newest driver is not always the supported one).",
            run: None,
            open: Some(KARABINER_DRIVER_URL),
            copy: None,
            request: None,
            verifies: Some(checks::DRIVER_PRESENT),
        },
        WizardStep {
            title: "Match the driver version to kanata",
            instruction: "If doctor reports a driver/kanata version mismatch, install the \
                          driver release your kanata version supports.",
            run: None,
            open: Some(KARABINER_DRIVER_URL),
            copy: None,
            request: None,
            verifies: Some(checks::DRIVER_VERSION),
        },
        WizardStep {
            title: "Activate & approve the extension",
            instruction: "KanataBar has requested the extension's activation for you \
                          (Karabiner-VirtualHIDDevice-Manager activate); approve it in \
                          System Settings → Privacy & Security, then re-check.",
            run: Some(ACTIVATE_DRIVER_CMD),
            open: Some(panes::EXTENSIONS),
            copy: None,
            request: None,
            verifies: Some(checks::DRIVER),
        },
        WizardStep {
            title: "Keep the VHID daemon running",
            instruction: "Nothing supervises the Karabiner VirtualHIDDevice daemon, so it \
                          dies on reboot. Run `sudo kanatactl install` — it registers \
                          KanataBar's LaunchDaemon for it (skipped automatically when \
                          Karabiner-Elements or your own plist already manages it).",
            run: None,
            open: None,
            copy: Some("sudo kanatactl install"),
            request: None,
            verifies: Some(checks::VHID_MANAGED),
        },
        WizardStep {
            title: "Grant Input Monitoring",
            instruction: "Add /usr/local/bin/kanatad to Input Monitoring (+ button, \
                          Cmd+Shift+G to type the path) — macOS attributes kanata's device \
                          access to the supervising daemon, so kanata's own self-registered \
                          entry is not the one checked. After updating KanataBar, remove (−) \
                          and re-add (+) this entry (kanata updates don't affect it).",
            run: None,
            open: Some(panes::INPUT_MONITORING),
            copy: None,
            request: Some(kanatabar_core::ipc::PermissionKind::InputMonitoring),
            verifies: Some(checks::INPUT_MONITORING),
        },
        WizardStep {
            title: "Grant Accessibility",
            instruction: "Add /usr/local/bin/kanatad to Accessibility too (+ button, \
                          Cmd+Shift+G to type the path) — macOS requires BOTH permissions \
                          on the supervising daemon. After updating KanataBar, remove (−) \
                          and re-add (+) this entry (kanata updates don't affect it).",
            run: None,
            open: Some(panes::ACCESSIBILITY),
            copy: None,
            request: Some(kanatabar_core::ipc::PermissionKind::Accessibility),
            verifies: Some(checks::ACCESSIBILITY),
        },
        WizardStep {
            title: "Install the KanataBar service",
            instruction: "Install the background daemon and agent: run \
                          `sudo kanatactl install` in Terminal.",
            run: None,
            open: None,
            copy: Some("sudo kanatactl install"),
            request: None,
            verifies: Some(checks::DAEMON),
        },
        WizardStep {
            title: "Verify",
            instruction: "Run doctor — every check should be green.",
            run: None,
            open: None,
            copy: None,
            request: None,
            verifies: None,
        },
    ]
}

/// The wizard step (index into [`steps`]) that fixes a given doctor check —
/// the *inverted* `verifies` mapping. This is how the doctor window delegates
/// a setup-class failure to the wizard ("Open Setup Assistant at this step",
/// SPEC §11.3 tier 2 / the §11 [HARD] anti-overlap rule).
pub fn step_index_for_check(check_name: &str) -> Option<usize> {
    steps()
        .iter()
        .position(|step| step.verifies == Some(check_name))
}

/// The doctor check whose wizard step fixes a given runtime degradation, when
/// one exists. Doctor checks can't see everything — TCC (Input Monitoring) is
/// unreadable by design, so its check never fails statically — but the
/// *supervisor* knows the runtime truth. Mapping the degraded reason back
/// onto a check name lets the wizard trust the daemon over the checklist
/// (HW Run 9 finding: a fresh install sat in `Degraded{InputMonitoringDenied}`
/// while the wizard congratulated the user).
pub fn check_for_degraded_reason(reason: DegradedReason) -> Option<&'static str> {
    match reason {
        DegradedReason::InputMonitoringDenied => Some(checks::INPUT_MONITORING),
        DegradedReason::DriverNotActivated => Some(checks::DRIVER),
        DegradedReason::VhidDaemonDown => Some(checks::VHID_DAEMON),
        DegradedReason::KanataBinMissing => Some(checks::KANATA_BINARY),
        DegradedReason::ConfigBroken => Some(checks::ACTIVE_CONFIG),
        // No setup step exists for these: budget exhaustion and grab/port
        // conflicts are operational states, not onboarding gaps; the
        // output-backend loss self-recovers (SPEC §6.5).
        DegradedReason::RetryBudgetExhausted
        | DegradedReason::DeviceGrabConflict
        | DegradedReason::TcpPortConflict
        | DegradedReason::OutputBackendUnavailable => None,
    }
}

/// The first wizard step whose verifying `doctor` check is currently failing —
/// i.e. the next thing the user should fix (SPEC §11). A supervisor
/// degradation counts as its mapped check failing, even when the static check
/// passes (see [`check_for_degraded_reason`]). `None` when every checked step
/// is satisfied. Drives the tray's one-click "Setup Wizard…".
pub fn first_unsatisfied(
    checks: &[DoctorCheck],
    degraded: Option<DegradedReason>,
) -> Option<WizardStep> {
    let mut failed: BTreeSet<&str> = checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| c.name.as_str())
        .collect();
    if let Some(name) = degraded.and_then(check_for_degraded_reason) {
        failed.insert(name);
    }
    steps()
        .into_iter()
        .find(|step| step.verifies.is_some_and(|name| failed.contains(name)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanatabar_core::doctor::ALL_CHECKS;

    fn check(name: &str, ok: bool) -> DoctorCheck {
        DoctorCheck {
            name: name.to_string(),
            ok,
            detail: String::new(),
            fix_hint: None,
        }
    }

    #[test]
    fn steps_are_non_empty_and_ordered_from_driver_to_verify() {
        let steps = steps();
        assert!(steps.len() >= 5);
        assert_eq!(steps.first().unwrap().title, "Install the Karabiner driver");
        assert_eq!(steps.last().unwrap().title, "Verify");
    }

    #[test]
    fn every_step_check_is_a_real_doctor_check() {
        // The wizard must reference the single source of check names, or a
        // renamed check would silently stop verifying a step (SPEC §11).
        for step in steps() {
            if let Some(name) = step.verifies {
                assert!(
                    ALL_CHECKS.contains(&name),
                    "step {:?} verifies unknown check {name}",
                    step.title
                );
            }
        }
    }

    #[test]
    fn first_unsatisfied_points_at_the_earliest_failing_step() {
        // Pkg not installed (no manager app) → both driver checks fail, and the
        // *install* step (verifies DRIVER_PRESENT) is the first failing one.
        let checks = [
            check(checks::DRIVER_PRESENT, false),
            check(checks::DRIVER, false),
            check(checks::DAEMON, false),
        ];
        assert_eq!(
            first_unsatisfied(&checks, None).unwrap().title,
            "Install the Karabiner driver"
        );

        // Pkg present but not activated/approved → DRIVER_PRESENT passes, so the
        // install step is satisfied and the wizard advances to the *activate*
        // step (verifies DRIVER). This is the transition HW Run 3 found broken:
        // before, both steps verified DRIVER and the activate step was shadowed.
        let checks = [
            check(checks::DRIVER_PRESENT, true),
            check(checks::DRIVER_VERSION, true),
            check(checks::DRIVER, false),
        ];
        assert_eq!(
            first_unsatisfied(&checks, None).unwrap().title,
            "Activate & approve the extension"
        );

        // Driver ok but the service isn't installed → the install step.
        let checks = [
            check(checks::DRIVER_PRESENT, true),
            check(checks::DRIVER, true),
            check(checks::INPUT_MONITORING, true),
            check(checks::DAEMON, false),
        ];
        assert_eq!(
            first_unsatisfied(&checks, None).unwrap().title,
            "Install the KanataBar service"
        );

        // Everything green → nothing to do.
        let checks = ALL_CHECKS.map(|name| check(name, true));
        assert_eq!(first_unsatisfied(&checks, None), None);
    }

    #[test]
    fn a_runtime_degradation_overrides_a_green_checklist() {
        // The HW Run 9 finding: every static check green while the supervisor
        // sits in Degraded{InputMonitoringDenied} — the wizard must jump to the
        // grant step, not congratulate the user. (The static permission check
        // can read a definitive denial now, but the behavioral degraded reason
        // is still the runtime backstop when the static read is indeterminate.)
        let checks = ALL_CHECKS.map(|name| check(name, true));
        assert_eq!(
            first_unsatisfied(&checks, Some(DegradedReason::InputMonitoringDenied))
                .unwrap()
                .title,
            "Grant Input Monitoring"
        );

        // Same for a driver deactivated behind a green pkg-present check.
        assert_eq!(
            first_unsatisfied(&checks, Some(DegradedReason::DriverNotActivated))
                .unwrap()
                .title,
            "Activate & approve the extension"
        );

        // Reasons with no setup step leave a green checklist green.
        assert_eq!(
            first_unsatisfied(&checks, Some(DegradedReason::RetryBudgetExhausted)),
            None
        );
    }

    #[test]
    fn every_setup_check_maps_back_to_a_step() {
        // The doctor window's delegation relies on the inversion covering
        // every setup-class check (SPEC §11.1/§11.3).
        for name in kanatabar_core::doctor::SETUP_CHECKS {
            let idx = step_index_for_check(name)
                .unwrap_or_else(|| panic!("setup check {name} has no wizard step"));
            assert_eq!(steps()[idx].verifies, Some(name));
        }
        assert_eq!(step_index_for_check("no such check"), None);
    }

    #[test]
    fn sudo_steps_carry_a_copyable_command_and_never_a_run() {
        // SPEC §11.2 [HARD]: the tray never elevates — anything needing sudo
        // is copyable text, and a step that runs something never needs sudo.
        for step in steps() {
            if let Some(copy) = step.copy {
                assert!(copy.starts_with("sudo "), "copy is for sudo commands");
                assert!(step.run.is_none(), "{:?} both runs and copies", step.title);
            }
        }
        let installs: Vec<&str> = steps().iter().filter_map(|s| s.copy).collect();
        assert_eq!(
            installs,
            ["sudo kanatactl install", "sudo kanatactl install"]
        );
    }

    #[test]
    fn every_mapped_degraded_reason_names_a_real_doctor_check() {
        for reason in [
            DegradedReason::InputMonitoringDenied,
            DegradedReason::DriverNotActivated,
            DegradedReason::VhidDaemonDown,
            DegradedReason::KanataBinMissing,
            DegradedReason::ConfigBroken,
        ] {
            let name = check_for_degraded_reason(reason).expect("mapped");
            assert!(ALL_CHECKS.contains(&name), "{reason:?} → unknown {name}");
        }
    }

    #[test]
    fn activate_step_runs_the_documented_manager_command() {
        // pqrs README (checked 2026-07): activation is
        // `<manager> activate`, no sudo, in the user's session — exactly what
        // the tray runs. The manager path is the single source in core::vhidd.
        let step = steps()
            .into_iter()
            .find(|s| s.title == "Activate & approve the extension")
            .expect("activate step exists");
        let argv = step.run.expect("activate step runs the manager");
        assert_eq!(
            argv,
            [kanatabar_core::vhidd::MANAGER_BINARY, "activate"],
            "must match the pqrs-documented activation invocation"
        );
        assert!(
            argv[0].starts_with("/Applications/.Karabiner-VirtualHIDDevice-Manager.app/"),
            "manager lives (hidden) in /Applications: {}",
            argv[0]
        );
        // No other step runs a command; every run target is an absolute path.
        for step in steps() {
            if let Some(argv) = step.run {
                assert!(argv[0].starts_with('/'), "run must be absolute: {argv:?}");
            }
        }
    }

    #[test]
    fn panes_and_url_are_openable_schemes() {
        assert!(panes::EXTENSIONS.starts_with("x-apple.systempreferences:"));
        assert!(panes::INPUT_MONITORING.starts_with("x-apple.systempreferences:"));
        assert!(KARABINER_DRIVER_URL.starts_with("https://"));
        // Every step that declares an `open` target uses one of them.
        for step in steps() {
            if let Some(url) = step.open {
                assert!(
                    url.starts_with("x-apple.systempreferences:") || url.starts_with("https://"),
                    "step {:?} has a non-openable target {url}",
                    step.title
                );
            }
        }
    }
}
