//! Supervisor state machine types (SPEC §6.2).
//!
//! Phase 0 defines the vocabulary; Phase 1 adds the table-driven transition
//! function and its tests.

use serde::{Deserialize, Serialize};

/// Top-level supervisor state (SPEC §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SupervisorState {
    /// No kanata child; entered on init or user stop.
    Stopped,
    /// A spawn is in flight (start, reload, device/config change).
    Starting,
    /// Child spawned and healthy.
    Running,
    /// Waiting out an exponential-backoff timer after a crash or failed start.
    Backoff,
    /// Retry budget exhausted, driver missing, or config broken; requires user action.
    Degraded,
    /// User-requested pause; child stopped intentionally.
    Paused,
}

/// Why the supervisor is in `Degraded` and what user action fixes it (SPEC §6.2, §6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DegradedReason {
    /// Too many consecutive crashes/failed starts; retry budget spent.
    RetryBudgetExhausted,
    /// Karabiner DriverKit extension is not `activated enabled`.
    DriverNotActivated,
    /// Karabiner VirtualHIDDevice daemon is not running.
    VhidDaemonDown,
    /// The active config failed `kanata --check`; a bad config can never
    /// take the keyboard down (SPEC §6.4).
    ConfigBroken,
    /// The kanata binary is missing — a clear message, not a crash loop (SPEC §16).
    KanataBinMissing,
    /// macOS TCC denied kanata's device access. Verified on HW (macOS 26.5.1,
    /// kanata 1.12, 2026-07-11): the grants attach to **kanatad** (the launchd
    /// daemon is the TCC responsible process for its kanata child), BOTH
    /// Input Monitoring AND Accessibility are required, and the kanata
    /// binary itself needs no grants (SPEC §2). Won't fix itself; retrying
    /// is futile, so no crash loop.
    InputMonitoringDenied,
    /// Another process holds exclusive access to the keyboard (a Karabiner-
    /// Elements grabber, or a second kanata — SPEC §2 single-instance).
    DeviceGrabConflict,
    /// kanata's TCP port is taken by another process; kanata panics with
    /// `AddrInUse` at startup and respawning cannot fix it (HW 2026-07-11).
    TcpPortConflict,
    /// kanata is alive but its DriverKit output backend never came up, so it
    /// released the input devices and keys pass through UNREMAPPED (HW
    /// 2026-07-11: driver pkg v8.0.0 with kanata 1.12.0 — a protocol
    /// mismatch; only driver v6.2.0 worked). kanata keeps retrying and
    /// self-recovers if the backend returns, so the child is left running —
    /// the supervisor flips back to `Running` on the recovery line.
    OutputBackendUnavailable,
}

impl DegradedReason {
    /// A user-facing, actionable one-liner (SPEC §15: never a raw Debug dump).
    pub fn describe(self) -> &'static str {
        match self {
            Self::RetryBudgetExhausted => {
                "kanata keeps crashing — restart it, or load the last-known-good config"
            }
            Self::DriverNotActivated => "Karabiner driver not activated — run the Setup Wizard",
            Self::VhidDaemonDown => {
                "Karabiner VirtualHIDDevice daemon is not running — reinstall the Karabiner driver"
            }
            Self::ConfigBroken => "active config failed validation — fix it or roll back",
            Self::KanataBinMissing => "kanata binary not found — install kanata or set kanata_bin",
            Self::InputMonitoringDenied => {
                "macOS denied kanata input access — grant BOTH Input Monitoring AND \
                 Accessibility to /usr/local/bin/kanatad in System Settings (kanata's own \
                 entry is not the one macOS checks); after updating KanataBar, remove (−) \
                 and re-add (+) both entries. KanataBar restarts kanata automatically \
                 once both are granted"
            }
            Self::DeviceGrabConflict => {
                "another program holds the keyboard (Karabiner-Elements grabber or a second \
                 kanata?) — quit it, then Start"
            }
            Self::TcpPortConflict => {
                "kanata's TCP port is taken (another kanata or kanata-tray?) — free the port \
                 or change tcp_port in config.toml, then Start"
            }
            Self::OutputBackendUnavailable => {
                "kanata cannot reach its virtual-keyboard output — keys are NOT remapped. \
                 Likely a Karabiner driver version mismatch: install the driver pkg named \
                 in your kanata release notes (the newest pqrs release is not always \
                 compatible), then approve it and Restart"
            }
        }
    }
}

/// Classification of a kanata child exit (SPEC §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExitClass {
    /// We signaled the child (stop, restart, reload).
    Requested,
    /// The user force-exited kanata via the panic escape chord; treated as an
    /// intentional stop, never a crash-loop trigger (SPEC §2).
    PanicEscape,
    /// Unexpected exit with the raw code or signal for diagnostics.
    Crash {
        /// Process exit code, if it exited normally.
        code: Option<i32>,
        /// Terminating signal number, if killed by a signal.
        signal: Option<i32>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_state_serde_round_trip() {
        for state in [
            SupervisorState::Stopped,
            SupervisorState::Starting,
            SupervisorState::Running,
            SupervisorState::Backoff,
            SupervisorState::Degraded,
            SupervisorState::Paused,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let back: SupervisorState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, back);
        }
    }

    #[test]
    fn exit_class_serde_round_trip() {
        for exit in [
            ExitClass::Requested,
            ExitClass::PanicEscape,
            ExitClass::Crash {
                code: Some(1),
                signal: None,
            },
            ExitClass::Crash {
                code: None,
                signal: Some(9),
            },
        ] {
            let json = serde_json::to_string(&exit).unwrap();
            let back: ExitClass = serde_json::from_str(&json).unwrap();
            assert_eq!(exit, back);
        }
    }
}
