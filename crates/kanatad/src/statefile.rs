//! `state.json` persistence (SPEC §3.2, §6.2).
//!
//! Written atomically (temp file + rename) with mode 0600 on every state
//! transition and at shutdown. Persistence failures are logged, never fatal —
//! the supervisor's job is keeping kanata alive.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use kanatabar_core::state::{DegradedReason, SupervisorState};
use serde::{Deserialize, Serialize};

/// Contents of `state.json` (SPEC §3.2: active preset, child PID,
/// last-known-good pointers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    /// File format version.
    pub schema: u32,
    /// Supervisor state at write time.
    pub state: SupervisorState,
    /// Why we are `Degraded`, when applicable.
    pub degraded_reason: Option<DegradedReason>,
    /// Live child pid, used for the orphan sweep after a daemon crash (§6.1b).
    pub kanata_pid: Option<u32>,
    /// Active preset name, when one is selected.
    #[serde(default)]
    pub active_preset: Option<String>,
    /// Path of the last-known-good `.kbd` backup, when one exists (SPEC §6.4).
    #[serde(default)]
    pub last_known_good: Option<String>,
    /// Seconds since the Unix epoch at write time.
    pub updated_unix: u64,
}

impl PersistedState {
    /// Snapshot of the current supervisor status.
    pub fn now(
        state: SupervisorState,
        degraded_reason: Option<DegradedReason>,
        kanata_pid: Option<u32>,
        active_preset: Option<String>,
        last_known_good: Option<String>,
    ) -> Self {
        Self {
            schema: 1,
            state,
            degraded_reason,
            kanata_pid,
            active_preset,
            last_known_good,
            updated_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}

/// Load `state.json` from `dir`, or `None` if absent/unreadable. Used for the
/// orphan sweep at startup (SPEC §6.1b).
pub fn load(dir: &Path) -> Option<PersistedState> {
    let text = fs::read_to_string(dir.join("state.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Atomically write `state.json` into `dir`, creating the directory if needed.
pub fn persist(dir: &Path, state: &PersistedState) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let body = serde_json::to_vec_pretty(state)?;
    let tmp = dir.join("state.json.tmp");
    fs::write(&tmp, body)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, dir.join("state.json"))
}
