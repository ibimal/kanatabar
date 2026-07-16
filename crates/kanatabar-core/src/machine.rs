//! Table-driven supervisor state machine (SPEC §6.2).
//!
//! The machine is pure: it consumes [`MachineEvent`]s and emits an [`Outcome`]
//! of side-effect [`Action`]s for the daemon to execute. It owns no timers and
//! spawns no processes — the daemon arms timers and feeds the elapsed events
//! back in, which keeps every transition unit-testable on any OS.

use serde::{Deserialize, Serialize};

use crate::backoff::BackoffConfig;
use crate::state::{DegradedReason, ExitClass, SupervisorState};

/// Input to the state machine: user commands, child lifecycle, timers, faults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineEvent {
    /// User asked to start (also: autostart at daemon boot).
    Start,
    /// User asked to stop; the daemon keeps running.
    Stop,
    /// Restart request (user, config change, device re-sync).
    Restart,
    /// User toggled pause.
    Pause,
    /// User resumed from pause.
    Resume,
    /// The daemon spawned the child successfully.
    SpawnSucceeded,
    /// The daemon could not spawn the child (I/O error other than a missing binary).
    SpawnFailed,
    /// The child exited; classification per SPEC §6.1.
    ChildExited(ExitClass),
    /// The backoff timer armed by [`Action::ArmBackoff`] fired.
    BackoffElapsed,
    /// The healthy-window timer armed by [`Action::ArmHealthyTimer`] fired
    /// while the child was still running: reset the retry budget (SPEC §6.2).
    HealthyElapsed,
    /// A preflight or health check failed; go `Degraded` with a reason,
    /// never a crash loop (SPEC §6.5).
    Fault(DegradedReason),
    /// The *running* child reported its output backend gone and released the
    /// input devices — keys pass through unremapped (HW 2026-07-11: driver
    /// version mismatch). Unlike [`MachineEvent::Fault`] the child is left
    /// alive: kanata retries the backend itself and prints a recovery line.
    OutputBackendLost,
    /// The child's output backend came back and it re-grabbed the devices;
    /// leave `Degraded{OutputBackendUnavailable}` without a respawn.
    OutputBackendRecovered,
}

/// Side effect the daemon must perform after a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Preflight + spawn the kanata child (SPEC §6.1).
    SpawnChild,
    /// Gracefully terminate the child: SIGTERM → grace → SIGKILL.
    StopChild,
    /// Arm the crash-recovery timer; feed back `BackoffElapsed` when it fires.
    ArmBackoff {
        /// Delay before the next start attempt, from [`BackoffConfig::delay_ms`].
        delay_ms: u64,
    },
    /// Arm the healthy-window timer; feed back `HealthyElapsed` if still running.
    ArmHealthyTimer,
}

/// A state transition, pushed to IPC subscribers and logged (SPEC §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateChanged {
    /// State before the event.
    pub from: SupervisorState,
    /// State after the event.
    pub to: SupervisorState,
    /// Why we are `Degraded`, when `to == Degraded`.
    pub reason: Option<DegradedReason>,
}

/// Result of handling one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The transition, if the state changed.
    pub transition: Option<StateChanged>,
    /// Side effects to execute, in order.
    pub actions: Vec<Action>,
}

impl Outcome {
    fn ignore() -> Self {
        Self {
            transition: None,
            actions: Vec::new(),
        }
    }
}

/// The supervisor state machine (SPEC §6.2).
#[derive(Debug, Clone)]
pub struct Machine {
    state: SupervisorState,
    /// Consecutive failures since the last healthy window / user start.
    failures: u32,
    reason: Option<DegradedReason>,
    backoff: BackoffConfig,
}

impl Machine {
    /// A machine in `Stopped` with a fresh failure budget.
    pub fn new(backoff: BackoffConfig) -> Self {
        Self {
            state: SupervisorState::Stopped,
            failures: 0,
            reason: None,
            backoff,
        }
    }

    /// Current state.
    pub fn state(&self) -> SupervisorState {
        self.state
    }

    /// Consecutive failures counted against the retry budget.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Why we are `Degraded`, when in that state.
    pub fn degraded_reason(&self) -> Option<DegradedReason> {
        self.reason
    }

    /// Advance the machine. Unexpected `(state, event)` pairs are ignored
    /// (stale timers, exits already handled) rather than panicking.
    pub fn handle(&mut self, event: MachineEvent) -> Outcome {
        use Action as A;
        use MachineEvent as E;
        use SupervisorState as S;

        // Exits we requested were already driven by the Stop/Restart/Pause
        // transition; the exit itself is not news (SPEC §6.1).
        if matches!(event, E::ChildExited(ExitClass::Requested)) {
            return Outcome::ignore();
        }

        match (self.state, event) {
            // ── User start / resume: explicit user intent grants a fresh budget.
            //    Degraded stops the child first: `Degraded{OutputBackendUnavailable}`
            //    keeps kanata alive, and spawning next to it would break
            //    single-instance [HARD] (StopChild is a no-op with no child).
            (S::Degraded, E::Start) => {
                self.failures = 0;
                self.enter(S::Starting, vec![A::StopChild, A::SpawnChild])
            }
            (S::Stopped | S::Paused | S::Backoff, E::Start) | (S::Paused, E::Resume) => {
                self.failures = 0;
                self.enter(S::Starting, vec![A::SpawnChild])
            }

            // ── Restart: from Running/Degraded, stop then respawn; from Backoff
            //    (user action) or Stopped, just spawn. The budget is kept so that
            //    automated restarts (device/config change) can't mask crash loops.
            (S::Running | S::Degraded, E::Restart) => {
                self.enter(S::Starting, vec![A::StopChild, A::SpawnChild])
            }
            (S::Stopped | S::Backoff, E::Restart) => self.enter(S::Starting, vec![A::SpawnChild]),

            // ── Stop: from anywhere active; daemon keeps running. Degraded may
            //    hold a live child (backend-unavailable), so stop it too.
            (S::Starting | S::Running | S::Degraded, E::Stop) => {
                self.enter(S::Stopped, vec![A::StopChild])
            }
            (S::Backoff | S::Paused, E::Stop) => self.enter(S::Stopped, vec![]),

            // ── Pause: only meaningful while running (SPEC §6.2).
            (S::Running, E::Pause) => self.enter(S::Paused, vec![A::StopChild]),

            // ── Spawn results.
            (S::Starting, E::SpawnSucceeded) => self.enter(S::Running, vec![A::ArmHealthyTimer]),
            (S::Starting, E::SpawnFailed) => self.on_failure(),

            // ── Child exits (unrequested).
            (S::Starting | S::Running, E::ChildExited(ExitClass::Crash { .. })) => {
                self.on_failure()
            }
            // Panic escape is an intentional stop, never a crash-loop trigger (SPEC §2, §16).
            (S::Starting | S::Running, E::ChildExited(ExitClass::PanicEscape)) => {
                self.enter(S::Stopped, vec![])
            }

            // ── Timers.
            (S::Backoff, E::BackoffElapsed) => self.enter(S::Starting, vec![A::SpawnChild]),
            (S::Running, E::HealthyElapsed) => {
                self.failures = 0;
                Outcome::ignore()
            }

            // ── Faults from preflight/health checks: actionable Degraded, no loop.
            (S::Starting | S::Backoff, E::Fault(reason)) => self.enter_degraded(reason, vec![]),
            (S::Running, E::Fault(reason)) => self.enter_degraded(reason, vec![A::StopChild]),

            // ── Live output-backend health (HW 2026-07-11: a driver version
            //    mismatch leaves kanata alive but unremapping forever). The
            //    child is deliberately KEPT: kanata retries the backend itself,
            //    so recovery needs no respawn — just flip back to Running.
            (S::Running, E::OutputBackendLost) => {
                self.enter_degraded(DegradedReason::OutputBackendUnavailable, vec![])
            }
            (S::Degraded, E::OutputBackendRecovered)
                if self.reason == Some(DegradedReason::OutputBackendUnavailable) =>
            {
                self.enter(S::Running, vec![A::ArmHealthyTimer])
            }

            // Everything else (stale timers, duplicate commands, exits in
            // terminal states) is deliberately a no-op.
            _ => Outcome::ignore(),
        }
    }

    /// Count a failure against the budget: `Backoff` while budget remains,
    /// `Degraded{RetryBudgetExhausted}` once it is spent (SPEC §6.2).
    fn on_failure(&mut self) -> Outcome {
        self.failures = self.failures.saturating_add(1);
        if self.backoff.budget_spent(self.failures) {
            self.enter_degraded(DegradedReason::RetryBudgetExhausted, vec![])
        } else {
            let delay_ms = self.backoff.delay_ms(self.failures - 1);
            self.enter(
                SupervisorState::Backoff,
                vec![Action::ArmBackoff { delay_ms }],
            )
        }
    }

    fn enter(&mut self, next: SupervisorState, actions: Vec<Action>) -> Outcome {
        self.reason = None;
        self.transition(next, actions)
    }

    fn enter_degraded(&mut self, reason: DegradedReason, actions: Vec<Action>) -> Outcome {
        self.reason = Some(reason);
        self.transition(SupervisorState::Degraded, actions)
    }

    fn transition(&mut self, next: SupervisorState, actions: Vec<Action>) -> Outcome {
        let from = self.state;
        self.state = next;
        Outcome {
            transition: Some(StateChanged {
                from,
                to: next,
                reason: self.reason,
            }),
            actions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action as A;
    use MachineEvent as E;
    use SupervisorState as S;

    fn machine_in(state: S, failures: u32) -> Machine {
        let mut m = Machine::new(BackoffConfig::default());
        m.state = state;
        m.failures = failures;
        m
    }

    fn crash() -> E {
        E::ChildExited(ExitClass::Crash {
            code: Some(1),
            signal: None,
        })
    }

    /// The §6.2 transition table plus edge cases, table-driven per SPEC §15.
    #[test]
    fn transition_table() {
        #[allow(clippy::type_complexity)]
        let cases: Vec<(&str, S, u32, E, S, Vec<A>)> = vec![
            // (name, start state, failures, event, expected state, expected actions)
            (
                "stopped: start spawns",
                S::Stopped,
                0,
                E::Start,
                S::Starting,
                vec![A::SpawnChild],
            ),
            (
                "stopped: stop is a no-op",
                S::Stopped,
                0,
                E::Stop,
                S::Stopped,
                vec![],
            ),
            (
                "stopped: pause is a no-op",
                S::Stopped,
                0,
                E::Pause,
                S::Stopped,
                vec![],
            ),
            (
                "starting: spawn ok arms healthy timer",
                S::Starting,
                0,
                E::SpawnSucceeded,
                S::Running,
                vec![A::ArmHealthyTimer],
            ),
            (
                "starting: spawn failure backs off",
                S::Starting,
                0,
                E::SpawnFailed,
                S::Backoff,
                vec![A::ArmBackoff { delay_ms: 1000 }],
            ),
            (
                "starting: crash during start backs off",
                S::Starting,
                0,
                crash(),
                S::Backoff,
                vec![A::ArmBackoff { delay_ms: 1000 }],
            ),
            (
                "starting: stop kills in-flight child",
                S::Starting,
                0,
                E::Stop,
                S::Stopped,
                vec![A::StopChild],
            ),
            (
                "starting: preflight fault degrades",
                S::Starting,
                0,
                E::Fault(DegradedReason::ConfigBroken),
                S::Degraded,
                vec![],
            ),
            (
                "starting: missing binary degrades",
                S::Starting,
                0,
                E::Fault(DegradedReason::KanataBinMissing),
                S::Degraded,
                vec![],
            ),
            (
                "running: crash backs off with doubled delay",
                S::Running,
                1,
                crash(),
                S::Backoff,
                vec![A::ArmBackoff { delay_ms: 2000 }],
            ),
            (
                "running: crash spends the budget",
                S::Running,
                4,
                crash(),
                S::Degraded,
                vec![],
            ),
            (
                "running: panic escape stops, no backoff",
                S::Running,
                3,
                E::ChildExited(ExitClass::PanicEscape),
                S::Stopped,
                vec![],
            ),
            (
                "running: requested exit ignored",
                S::Running,
                0,
                E::ChildExited(ExitClass::Requested),
                S::Running,
                vec![],
            ),
            (
                "running: restart stops then respawns",
                S::Running,
                0,
                E::Restart,
                S::Starting,
                vec![A::StopChild, A::SpawnChild],
            ),
            (
                "running: stop kills child",
                S::Running,
                0,
                E::Stop,
                S::Stopped,
                vec![A::StopChild],
            ),
            (
                "running: pause kills child",
                S::Running,
                0,
                E::Pause,
                S::Paused,
                vec![A::StopChild],
            ),
            (
                "running: fault stops child and degrades",
                S::Running,
                0,
                E::Fault(DegradedReason::VhidDaemonDown),
                S::Degraded,
                vec![A::StopChild],
            ),
            (
                "running: stale backoff timer ignored",
                S::Running,
                0,
                E::BackoffElapsed,
                S::Running,
                vec![],
            ),
            (
                "backoff: timer fires, retry",
                S::Backoff,
                2,
                E::BackoffElapsed,
                S::Starting,
                vec![A::SpawnChild],
            ),
            (
                "backoff: user start retries immediately",
                S::Backoff,
                2,
                E::Start,
                S::Starting,
                vec![A::SpawnChild],
            ),
            (
                "backoff: restart retries immediately",
                S::Backoff,
                2,
                E::Restart,
                S::Starting,
                vec![A::SpawnChild],
            ),
            (
                "backoff: stop cancels recovery",
                S::Backoff,
                2,
                E::Stop,
                S::Stopped,
                vec![],
            ),
            (
                "backoff: crash of straggler ignored",
                S::Backoff,
                2,
                crash(),
                S::Backoff,
                vec![],
            ),
            // Degraded may hold a live child (backend-unavailable), so every
            // exit from it stops the child first — StopChild is a no-op when
            // no child exists, and skipping it would break single-instance.
            (
                "degraded: user start recovers",
                S::Degraded,
                5,
                E::Start,
                S::Starting,
                vec![A::StopChild, A::SpawnChild],
            ),
            (
                "degraded: user restart recovers",
                S::Degraded,
                5,
                E::Restart,
                S::Starting,
                vec![A::StopChild, A::SpawnChild],
            ),
            (
                "degraded: stop clears",
                S::Degraded,
                5,
                E::Stop,
                S::Stopped,
                vec![A::StopChild],
            ),
            (
                "running: backend lost degrades but keeps the child",
                S::Running,
                0,
                E::OutputBackendLost,
                S::Degraded,
                vec![],
            ),
            (
                "stopped: stale backend-lost ignored",
                S::Stopped,
                0,
                E::OutputBackendLost,
                S::Stopped,
                vec![],
            ),
            (
                "running: stale backend-recovery ignored",
                S::Running,
                0,
                E::OutputBackendRecovered,
                S::Running,
                vec![],
            ),
            (
                "degraded: healthy timer ignored",
                S::Degraded,
                5,
                E::HealthyElapsed,
                S::Degraded,
                vec![],
            ),
            (
                "paused: resume respawns",
                S::Paused,
                0,
                E::Resume,
                S::Starting,
                vec![A::SpawnChild],
            ),
            (
                "paused: start also respawns",
                S::Paused,
                0,
                E::Start,
                S::Starting,
                vec![A::SpawnChild],
            ),
            (
                "paused: stop settles to stopped",
                S::Paused,
                0,
                E::Stop,
                S::Stopped,
                vec![],
            ),
            (
                "paused: pause again is a no-op",
                S::Paused,
                0,
                E::Pause,
                S::Paused,
                vec![],
            ),
        ];

        for (name, start, failures, event, want_state, want_actions) in cases {
            let mut m = machine_in(start, failures);
            let out = m.handle(event);
            assert_eq!(m.state(), want_state, "state after: {name}");
            assert_eq!(out.actions, want_actions, "actions of: {name}");
            let changed = start != want_state;
            assert_eq!(out.transition.is_some(), changed, "transition of: {name}");
            if let Some(t) = out.transition {
                assert_eq!((t.from, t.to), (start, want_state), "from/to of: {name}");
            }
        }
    }

    #[test]
    fn crash_loop_walks_backoff_ladder_then_degrades() {
        let mut m = Machine::new(BackoffConfig::default());
        assert!(m.handle(E::Start).transition.is_some());

        let mut delays = Vec::new();
        loop {
            m.handle(E::SpawnSucceeded);
            let out = m.handle(crash());
            match m.state() {
                S::Backoff => {
                    let [A::ArmBackoff { delay_ms }] = out.actions[..] else {
                        panic!("expected ArmBackoff, got {:?}", out.actions);
                    };
                    delays.push(delay_ms);
                    m.handle(E::BackoffElapsed);
                }
                S::Degraded => break,
                other => panic!("unexpected state {other:?}"),
            }
        }
        // Budget 5: four backoffs on the doubling ladder, then Degraded.
        assert_eq!(delays, vec![1000, 2000, 4000, 8000]);
        assert_eq!(
            m.degraded_reason(),
            Some(DegradedReason::RetryBudgetExhausted)
        );
    }

    #[test]
    fn healthy_window_resets_budget() {
        let mut m = machine_in(S::Running, 4);
        let out = m.handle(E::HealthyElapsed);
        assert!(out.transition.is_none());
        assert_eq!(m.failures(), 0);
        // The next crash starts a fresh ladder instead of degrading.
        let out = m.handle(crash());
        assert_eq!(m.state(), S::Backoff);
        assert_eq!(out.actions, vec![A::ArmBackoff { delay_ms: 1000 }]);
    }

    #[test]
    fn user_start_from_degraded_grants_fresh_budget() {
        let mut m = machine_in(S::Degraded, 5);
        m.reason = Some(DegradedReason::RetryBudgetExhausted);
        m.handle(E::Start);
        assert_eq!(m.failures(), 0);
        assert_eq!(m.degraded_reason(), None);
        // One crash after recovery must back off, not instantly re-degrade.
        m.handle(E::SpawnSucceeded);
        m.handle(crash());
        assert_eq!(m.state(), S::Backoff);
    }

    /// The backend round trip (HW 2026-07-11 driver-mismatch finding): lost →
    /// Degraded with the child kept, recovered → Running with no respawn.
    #[test]
    fn backend_lost_then_recovered_round_trips_without_respawn() {
        let mut m = machine_in(S::Running, 2);
        let out = m.handle(E::OutputBackendLost);
        assert_eq!(m.state(), S::Degraded);
        assert_eq!(
            m.degraded_reason(),
            Some(DegradedReason::OutputBackendUnavailable)
        );
        // No StopChild: kanata retries the backend itself.
        assert_eq!(out.actions, vec![]);

        let out = m.handle(E::OutputBackendRecovered);
        assert_eq!(m.state(), S::Running);
        assert_eq!(m.degraded_reason(), None);
        // No SpawnChild either — same child, remapping resumed.
        assert_eq!(out.actions, vec![A::ArmHealthyTimer]);
        // The failure budget is untouched by the round trip (not a crash).
        assert_eq!(m.failures(), 2);
    }

    /// A recovery line must not lift a Degraded caused by something else
    /// (e.g. RetryBudgetExhausted) — only the backend reason round-trips.
    #[test]
    fn backend_recovery_only_lifts_backend_degraded() {
        let mut m = machine_in(S::Degraded, 5);
        m.reason = Some(DegradedReason::RetryBudgetExhausted);
        let out = m.handle(E::OutputBackendRecovered);
        assert!(out.transition.is_none());
        assert_eq!(m.state(), S::Degraded);
        assert_eq!(
            m.degraded_reason(),
            Some(DegradedReason::RetryBudgetExhausted)
        );
    }

    #[test]
    fn degraded_reason_survives_only_in_degraded() {
        let mut m = machine_in(S::Starting, 0);
        m.handle(E::Fault(DegradedReason::ConfigBroken));
        assert_eq!(m.degraded_reason(), Some(DegradedReason::ConfigBroken));
        m.handle(E::Start);
        assert_eq!(m.degraded_reason(), None);
    }
}
