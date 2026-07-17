//! Health-Check-window view-model (SPEC §11.3, Phase 12).
//!
//! Pure and display-free like [`crate::devwin`]: `Vec<DoctorCheck>` in, a
//! serializable [`HealthView`] out. The doctor is unordered, stateless, and
//! exhaustive — every check renders with its detail and fix hint. Fix
//! affordances follow the §11.3 tiers, with the §11 [HARD] anti-overlap rule
//! baked in: a failing *setup-class* check gets exactly one action — open the
//! Setup Assistant at the step that fixes it — never its own onboarding flow.
//! Everything else shows its `fix_hint` (already actionable text from the
//! daemon). "Copy report" carries the §9 `doctor --json` bug-report bundle.

use kanatabar_core::doctor::{failed_count, is_setup_check};
use kanatabar_core::ipc::DoctorCheck;
use serde::Serialize;

use crate::wizard;

/// Everything the health page renders, in report order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthView {
    /// One-line summary under the title ("All 12 checks passed" /
    /// "2 of 12 checks failing").
    pub summary: String,
    /// True when every check passed (the page styles the summary by this).
    pub all_ok: bool,
    /// Fetch failure to display instead of rows, when the daemon call failed.
    pub error: Option<String>,
    /// Check rows, in the daemon's canonical report order.
    pub rows: Vec<CheckRow>,
    /// The §9 bug-report bundle for "Copy report": the raw `DoctorCheck`
    /// array, exactly what `kanatactl doctor --json` prints.
    pub report: Vec<DoctorCheck>,
}

/// One check row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckRow {
    /// Stable check name (`kanatabar_core::doctor::checks`).
    pub name: String,
    /// Did the check pass?
    pub ok: bool,
    /// Human detail line from the daemon.
    pub detail: String,
    /// Fix hint (failures only; `None` on passing checks).
    pub fix_hint: Option<String>,
    /// The wizard step (index into `wizard::steps()`) that fixes this
    /// failure, when it is setup-class — renders as "Open Setup Assistant"
    /// (§11.3 tier 2). `None` for passing rows and runtime-class failures.
    pub wizard_step: Option<usize>,
}

/// Build the view for a fetched doctor report.
pub fn view(checks: &[DoctorCheck]) -> HealthView {
    let rows: Vec<CheckRow> = checks
        .iter()
        .map(|c| CheckRow {
            name: c.name.clone(),
            ok: c.ok,
            detail: c.detail.clone(),
            fix_hint: c.fix_hint.clone(),
            wizard_step: (!c.ok && is_setup_check(&c.name))
                .then(|| wizard::step_index_for_check(&c.name))
                .flatten(),
        })
        .collect();
    let failed = failed_count(checks);
    HealthView {
        summary: match failed {
            0 => format!("All {} checks passed", checks.len()),
            n => format!("{n} of {} checks failing", checks.len()),
        },
        all_ok: failed == 0,
        error: None,
        rows,
        report: checks.to_vec(),
    }
}

/// Build the view for a failed fetch (daemon unreachable / request rejected).
pub fn error(message: &str) -> HealthView {
    HealthView {
        summary: "Health check unavailable".to_string(),
        all_ok: false,
        error: Some(message.to_string()),
        rows: Vec::new(),
        report: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanatabar_core::doctor::{checks, ALL_CHECKS};

    fn check(name: &str, ok: bool) -> DoctorCheck {
        DoctorCheck {
            name: name.to_string(),
            ok,
            detail: "detail".to_string(),
            fix_hint: if ok { None } else { Some("fix it".to_string()) },
        }
    }

    #[test]
    fn failing_setup_checks_delegate_to_their_wizard_step() {
        let view = view(&[
            check(checks::DRIVER, false),        // setup-class → wizard
            check(checks::ACTIVE_CONFIG, false), // runtime-class → hint only
            check(checks::DAEMON, true),         // passing setup → no action
        ]);
        let by_name = |n: &str| view.rows.iter().find(|r| r.name == n).expect("row");
        let step = by_name(checks::DRIVER).wizard_step.expect("delegates");
        assert_eq!(
            wizard::steps()[step].verifies,
            Some(checks::DRIVER),
            "delegation lands on the step that verifies the check"
        );
        assert_eq!(by_name(checks::ACTIVE_CONFIG).wizard_step, None);
        assert_eq!(by_name(checks::DAEMON).wizard_step, None);
        assert_eq!(view.summary, "2 of 3 checks failing");
        assert!(!view.all_ok);
    }

    #[test]
    fn all_green_report_summarizes_and_carries_no_actions() {
        let checks: Vec<DoctorCheck> = ALL_CHECKS.map(|n| check(n, true)).to_vec();
        let view = view(&checks);
        assert_eq!(view.summary, "All 12 checks passed");
        assert!(view.all_ok);
        assert!(view.rows.iter().all(|r| r.wizard_step.is_none()));
        // The copyable report is the raw §9 bundle, untransformed.
        assert_eq!(view.report, checks);
    }

    #[test]
    fn error_view_carries_the_message_and_no_rows() {
        let view = error("connect failed");
        assert_eq!(view.error.as_deref(), Some("connect failed"));
        assert!(view.rows.is_empty() && view.report.is_empty());
    }

    /// The JSON shape the embedded page's `render()` consumes — pinned like
    /// the devices view. `report` rides along in the §9 wire shape (itself
    /// pinned by core's doctor_check_json_schema_is_stable).
    #[test]
    fn view_json_shape_is_stable() {
        let json = serde_json::to_value(view(&[check(checks::DRIVER, false)])).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({
                "summary": "1 of 1 checks failing",
                "all_ok": false,
                "error": null,
                "rows": [{
                    "name": "karabiner driver",
                    "ok": false,
                    "detail": "detail",
                    "fix_hint": "fix it",
                    "wizard_step": 2,
                }],
                "report": [{
                    "name": "karabiner driver",
                    "ok": false,
                    "detail": "detail",
                    "fix_hint": "fix it",
                }],
            })
        );
    }
}
