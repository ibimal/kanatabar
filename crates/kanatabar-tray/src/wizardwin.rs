//! Setup-Assistant-window view-model (SPEC §11.2, Phase 12).
//!
//! Pure and display-free: doctor checks (+ the supervisor's degraded reason,
//! which overrides a green checklist — HW Run 9) in, a serializable
//! [`WizardView`] out. The wizard is ordered, stateful, and goal-driven: all
//! steps render as a checklist, the first unsatisfied step is expanded with
//! its instruction and actions, everything else is collapsed. Actions are
//! *indices into the static step table* — the page can only pick from
//! [`crate::wizard::steps`], never send commands or URLs of its own.
//!
//! The daemon executes `doctor`, so an unreachable daemon means no report at
//! all; callers synthesize a failing `daemon` check for that case (see
//! [`daemon_unreachable_checks`]) — which honestly lands the wizard on the
//! "Install the KanataBar service" step instead of erroring out.

use kanatabar_core::doctor::checks;
use kanatabar_core::ipc::DoctorCheck;
use kanatabar_core::state::DegradedReason;
use serde::Serialize;

use crate::wizard::{self, WizardStep};

/// Everything the wizard page renders, in step order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WizardView {
    /// One-line summary under the title ("Step 3 of 7" / "Setup complete").
    pub summary: String,
    /// Every checked step is satisfied — the goal state.
    pub done: bool,
    /// Completion panel (only when `done`; the caller supplies it because it
    /// may involve a preset lookup — `wizard_completion_window` in the shell).
    pub completion: Option<Completion>,
    /// Step rows, in wizard order.
    pub rows: Vec<StepRow>,
}

/// The completion panel: a message plus an optional command the *user* runs,
/// rendered as a copyable code chip (same affordance as sudo steps — the
/// page never glues the command into prose).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Completion {
    /// Congratulation / next-step message.
    pub message: String,
    /// A `kanatactl preset add …` command to copy, when there is a next step.
    pub command: Option<String>,
}

/// How a step renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StepState {
    /// Satisfied (or before the current step): collapsed, checkmark.
    Done,
    /// The first unsatisfied step: expanded, instruction + actions.
    Current,
    /// After the current step: collapsed, dimmed.
    Pending,
}

/// One step row. `index` is the row's position in [`wizard::steps`] — the
/// page's action messages (`run:<index>` / `open:<index>`) carry it back and
/// the shell validates it against the same static table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepRow {
    /// Index into `wizard::steps()`.
    pub index: usize,
    /// Step title.
    pub title: String,
    /// Full instruction (rendered only on the current step).
    pub instruction: String,
    /// Render state.
    pub state: StepState,
    /// The current step has a command the wizard can run for the user
    /// ("Do it for me" — e.g. driver activation; never sudo).
    pub can_run: bool,
    /// Label for the step's open-target button, when it has one
    /// ("Open System Settings" / "Open download page").
    pub open_label: Option<String>,
    /// A command for the *user* to run, shown as copyable code (sudo steps —
    /// the tray never elevates, SPEC §11.2 [HARD]).
    pub copy: Option<String>,
}

/// Build the view. `completion` is only rendered when every checked step is
/// satisfied (pass `None` when unknown; a default message is used).
pub fn view(
    checks: &[DoctorCheck],
    degraded: Option<DegradedReason>,
    completion: Option<Completion>,
) -> WizardView {
    let steps = wizard::steps();
    let current = wizard::first_unsatisfied(checks, degraded)
        .and_then(|step| steps.iter().position(|s| s.title == step.title));
    let done = current.is_none();

    let rows: Vec<StepRow> = steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let state = match current {
                None => StepState::Done,
                Some(c) if index < c => StepState::Done,
                Some(c) if index == c => StepState::Current,
                Some(_) => StepState::Pending,
            };
            row(index, step, state)
        })
        .collect();

    WizardView {
        summary: match current {
            None => "Setup complete".to_string(),
            Some(c) => format!("Step {} of {}", c + 1, steps.len()),
        },
        done,
        completion: done.then(|| {
            completion.unwrap_or_else(|| Completion {
                message: "All checks passed — you're set up.".to_string(),
                command: None,
            })
        }),
        rows,
    }
}

/// The synthetic report for an unreachable daemon: exactly one failing
/// `daemon` check, so `first_unsatisfied` lands on the install step and the
/// wizard renders honest guidance instead of a bare error.
pub fn daemon_unreachable_checks(err: &str) -> Vec<DoctorCheck> {
    vec![DoctorCheck {
        name: checks::DAEMON.to_string(),
        ok: false,
        detail: format!("daemon unreachable: {err}"),
        fix_hint: Some("sudo kanatactl install".to_string()),
    }]
}

fn row(index: usize, step: &WizardStep, state: StepState) -> StepRow {
    StepRow {
        index,
        title: step.title.to_string(),
        instruction: step.instruction.to_string(),
        state,
        can_run: state == StepState::Current && step.run.is_some(),
        open_label: (state == StepState::Current)
            .then_some(step.open)
            .flatten()
            .map(|target| {
                if target.starts_with("x-apple.systempreferences:") {
                    "Open System Settings".to_string()
                } else {
                    "Open download page".to_string()
                }
            }),
        copy: (state == StepState::Current)
            .then_some(step.copy)
            .flatten()
            .map(str::to_string),
    }
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
    fn first_failing_step_is_current_with_its_actions() {
        // Driver pkg present but not activated → the activate step (index 2)
        // is current; it has a run command and a Settings pane, no sudo copy.
        let checks = [
            check(checks::DRIVER_PRESENT, true),
            check(checks::DRIVER_VERSION, true),
            check(checks::DRIVER, false),
        ];
        let view = view(&checks, None, None);
        assert_eq!(view.summary, "Step 3 of 8");
        assert!(!view.done && view.completion.is_none());
        let current = &view.rows[2];
        assert_eq!(current.state, StepState::Current);
        assert!(current.can_run);
        assert_eq!(current.open_label.as_deref(), Some("Open System Settings"));
        assert_eq!(current.copy, None);
        assert_eq!(view.rows[0].state, StepState::Done);
        assert_eq!(view.rows[3].state, StepState::Pending);
        // Collapsed rows carry no actions.
        assert!(!view.rows[3].can_run && view.rows[3].open_label.is_none());
    }

    #[test]
    fn sudo_step_renders_copyable_command_not_a_run_button() {
        let checks = [check(checks::VHID_MANAGED, false)];
        let view = view(&checks, None, None);
        let current = view
            .rows
            .iter()
            .find(|r| r.state == StepState::Current)
            .expect("current");
        assert_eq!(current.copy.as_deref(), Some("sudo kanatactl install"));
        assert!(!current.can_run, "sudo is never a button (SPEC §11.2)");
    }

    #[test]
    fn grant_steps_open_their_pane_and_never_run_anything() {
        // The grant steps' only action is opening the right Settings pane —
        // HW 2026-07-18: the daemon's TCC request APIs register nothing from
        // the system context, so there is no "do it for me" for permissions;
        // the instruction teaches the manual +/path route.
        let im = view(&[check(checks::INPUT_MONITORING, false)], None, None);
        let current = im
            .rows
            .iter()
            .find(|r| r.state == StepState::Current)
            .expect("current");
        assert_eq!(current.title, "Grant Input Monitoring");
        assert!(!current.can_run);
        assert_eq!(current.open_label.as_deref(), Some("Open System Settings"));
    }

    #[test]
    fn degradation_overrides_green_and_completion_renders_when_done() {
        let all_green: Vec<DoctorCheck> = ALL_CHECKS.map(|n| check(n, true)).to_vec();
        // HW Run 9: green checklist + Degraded{InputMonitoringDenied} → the
        // grant step is current, not the congratulation.
        let view_degraded = view(
            &all_green,
            Some(DegradedReason::InputMonitoringDenied),
            None,
        );
        assert!(!view_degraded.done);
        assert_eq!(
            view_degraded.rows[4].state,
            StepState::Current,
            "grant step current"
        );
        // Truly done: every row Done, the supplied completion rendered.
        let supplied = Completion {
            message: "Add a preset to start.".to_string(),
            command: Some("kanatactl preset add main ~/.config/kanata/main.kbd".to_string()),
        };
        let view_done = view(&all_green, None, Some(supplied.clone()));
        assert!(view_done.done);
        assert_eq!(view_done.summary, "Setup complete");
        assert_eq!(view_done.completion, Some(supplied));
        assert!(view_done.rows.iter().all(|r| r.state == StepState::Done));
        // Done with no caller-supplied completion: the default message, no chip.
        let view_default = view(&all_green, None, None);
        assert_eq!(
            view_default.completion,
            Some(Completion {
                message: "All checks passed — you're set up.".to_string(),
                command: None,
            })
        );
    }

    #[test]
    fn unreachable_daemon_lands_on_the_install_step() {
        let checks = daemon_unreachable_checks("connect refused");
        let view = view(&checks, None, None);
        let current = view
            .rows
            .iter()
            .find(|r| r.state == StepState::Current)
            .expect("current");
        assert_eq!(current.title, "Install the KanataBar service");
        assert_eq!(current.copy.as_deref(), Some("sudo kanatactl install"));
    }

    /// The JSON shape the embedded page's `render()` consumes — pinned like
    /// the devices and health views.
    #[test]
    fn view_json_shape_is_stable() {
        let json = serde_json::to_value(view(&[check(checks::VHID_MANAGED, false)], None, None))
            .expect("serializes");
        assert_eq!(json["summary"], "Step 4 of 8");
        assert_eq!(json["done"], false);
        assert_eq!(json["completion"], serde_json::Value::Null);
        assert_eq!(
            json["rows"][3],
            serde_json::json!({
                "index": 3,
                "title": "Keep the VHID daemon running",
                "instruction": wizard::steps()[3].instruction,
                "state": "current",
                "can_run": false,
                "open_label": null,
                "copy": "sudo kanatactl install",
            })
        );
        assert_eq!(json["rows"][0]["state"], "done");
        assert_eq!(json["rows"][4]["state"], "pending");

        // Completion object shape (rendered as message + copyable chip).
        let all_green: Vec<DoctorCheck> = ALL_CHECKS.map(|n| check(n, true)).to_vec();
        let done = serde_json::to_value(view(
            &all_green,
            None,
            Some(Completion {
                message: "msg".to_string(),
                command: Some("kanatactl preset add a b".to_string()),
            }),
        ))
        .expect("serializes");
        assert_eq!(
            done["completion"],
            serde_json::json!({
                "message": "msg",
                "command": "kanatactl preset add a b",
            })
        );
    }
}
