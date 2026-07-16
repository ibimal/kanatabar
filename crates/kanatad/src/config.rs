//! Supervisor runtime configuration and the live spawn target.
//!
//! [`SupervisorConfig`] holds the fixed knobs (timings, backoff, paths).
//! [`ActiveConfig`] is the *mutable* part shared with the config manager: the
//! kanata binary/config/args to spawn next, plus the active preset and
//! last-known-good pointer. The supervisor reads it at each spawn and persist;
//! the config manager updates it on apply/switch (SPEC §6.4).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanatabar_core::backoff::BackoffConfig;
use kanatabar_core::kanata::Version;

use crate::events::DaemonEvents;
use crate::health::driver::DriverProbe;
use crate::health::HealthState;

/// Known-good kanata version floor; below this the daemon logs a warning
/// (SPEC §6.5). [VERIFY] against the installed kanata.
pub const KANATA_VERSION_FLOOR: Version = Version::new(1, 7, 0);

/// Resolve the kanata binary to supervise (SPEC §7.3): an explicit setting
/// (CLI flag / env / config.toml `defaults.kanata_bin`) always wins; otherwise
/// the first existing well-known location
/// (`kanatabar_core::kanata::KANATA_BIN_CANDIDATES` — never `$PATH`, §14).
/// When nothing exists, the first candidate is returned anyway so the
/// "kanata binary not found" degraded path and doctor hint name the canonical
/// install location. `exists` is injected so the decision is unit-testable.
pub fn resolve_kanata_bin(
    explicit: Option<PathBuf>,
    exists: impl Fn(&std::path::Path) -> bool,
) -> PathBuf {
    if let Some(bin) = explicit {
        return bin;
    }
    for candidate in kanatabar_core::kanata::KANATA_BIN_CANDIDATES {
        let path = std::path::Path::new(candidate);
        if exists(path) {
            return path.to_path_buf();
        }
    }
    PathBuf::from(kanatabar_core::kanata::KANATA_BIN_CANDIDATES[0])
}

/// Everything needed to spawn one kanata child (SPEC §6.1).
#[derive(Debug, Clone)]
pub struct SpawnTarget {
    /// kanata (or mock-kanata) binary to spawn.
    pub kanata_bin: PathBuf,
    /// `.kbd` config passed as `--cfg`.
    pub kanata_cfg: PathBuf,
    /// Extra arguments appended to the spawn and to `--check` preflights.
    pub extra_args: Vec<String>,
    /// TCP port for kanata's NDJSON server (`--port`), for the layer relay
    /// (SPEC §6.5). Preserved across preset switches; not passed to `--check`.
    pub tcp_port: Option<u16>,
    /// The uid whose IPC request vetted `kanata_cfg` (SPEC §6.4). When set,
    /// the supervisor re-runs the path-safety check against this uid right
    /// before every spawn, shrinking the validate→spawn TOCTOU window (§14).
    /// `None` for daemon-selected targets (safe config, `--cfg`, autostart
    /// preset from root-owned `config.toml`), which are not re-vetted — as
    /// before Phase 8.5.
    pub vetted_uid: Option<u32>,
}

/// Fixed supervisor knobs. The spawn target lives in [`ActiveConfig`].
#[derive(Clone)]
pub struct SupervisorConfig {
    /// Initial spawn target (seeds [`ActiveConfig`]).
    pub kanata_bin: PathBuf,
    /// Initial `.kbd` config.
    pub kanata_cfg: PathBuf,
    /// Initial extra arguments.
    pub extra_args: Vec<String>,
    /// Directory holding `state.json`; `None` disables persistence (dev).
    pub state_dir: Option<PathBuf>,
    /// Backoff tuning (SPEC §6.2).
    pub backoff: BackoffConfig,
    /// Healthy run length that resets the retry budget. Normally derived from
    /// `backoff.reset_after_s`; injectable so tests can use milliseconds.
    pub healthy_window: Duration,
    /// SIGTERM → SIGKILL grace when stopping the child (SPEC §6.1: ≤3s).
    pub kill_grace: Duration,
    /// Upper bound on a `--check` preflight run.
    pub preflight_timeout: Duration,
    /// Grace after the child reports its output backend gone (devices
    /// released, keys unremapped) before going `Degraded` (SPEC §6.5).
    /// kanata's own 10s startup wait precedes the release line, and launchd
    /// revives a crashed VHID daemon in ~1s, so anything surviving this
    /// window is persistent (HW 2026-07-11: driver version mismatch).
    /// A recovery line inside the window cancels it. Injectable for tests.
    pub backend_grace: Duration,
    /// kanata TCP port for the layer relay (`--port`); `None` disables it.
    pub tcp_port: Option<u16>,
    /// Driver preflight run before every spawn (SPEC §6.5 [HARD]); `None`
    /// skips the check (dev/tests without the real Karabiner driver).
    pub driver_probe: Option<DriverProbe>,
    /// Shared health facts surfaced in `Status` (driver_ok, kanata_version).
    pub health: HealthState,
    /// Daemon event bus for IPC events the state machine can't express as a
    /// transition — here, `Event::Crash` on a plain (recoverable) crash so the
    /// tray posts a "kanata crashed" notification (SPEC §8; HW 2026-07-13 Run 7).
    /// Shares the same bus as the control server; a `Default` bus with no
    /// subscribers is fine for tests that don't observe it.
    pub events: DaemonEvents,
}

impl SupervisorConfig {
    /// Config with spec defaults for the given binary and `.kbd` path.
    pub fn new(kanata_bin: PathBuf, kanata_cfg: PathBuf) -> Self {
        let backoff = BackoffConfig::default();
        Self {
            kanata_bin,
            kanata_cfg,
            extra_args: Vec::new(),
            state_dir: None,
            healthy_window: Duration::from_secs(backoff.reset_after_s),
            backoff,
            kill_grace: Duration::from_secs(3),
            preflight_timeout: Duration::from_secs(10),
            backend_grace: Duration::from_secs(15),
            tcp_port: None,
            driver_probe: None,
            health: HealthState::default(),
            events: DaemonEvents::default(),
        }
    }

    /// The initial spawn target implied by this config.
    pub fn initial_target(&self) -> SpawnTarget {
        SpawnTarget {
            kanata_bin: self.kanata_bin.clone(),
            kanata_cfg: self.kanata_cfg.clone(),
            extra_args: self.extra_args.clone(),
            tcp_port: self.tcp_port,
            vetted_uid: None,
        }
    }
}

/// Config-related runtime state shared between the supervisor and the config
/// manager. Cheap to clone (an `Arc`); all mutations are short, non-async
/// critical sections so the `std::sync::Mutex` is never held across `.await`.
#[derive(Clone)]
pub struct ActiveConfig {
    inner: Arc<Mutex<ActiveInner>>,
}

struct ActiveInner {
    target: SpawnTarget,
    preset: Option<String>,
    last_known_good: Option<PathBuf>,
}

/// A read-only view of the config state, for persisting `state.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSnapshot {
    /// Active preset name, when one is selected.
    pub preset: Option<String>,
    /// Path of the last-known-good backup, when one exists.
    pub last_known_good: Option<PathBuf>,
}

impl ActiveConfig {
    /// Seed the shared state with an initial spawn target and no preset.
    pub fn new(target: SpawnTarget) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ActiveInner {
                target,
                preset: None,
                last_known_good: None,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ActiveInner> {
        // The mutex only guards small clones/assignments; poisoning would mean
        // a panic inside one of those, which cannot happen, so recover the guard.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The target to spawn next (read by the supervisor at each spawn).
    pub fn spawn_target(&self) -> SpawnTarget {
        self.lock().target.clone()
    }

    /// Set the active config path (and optional per-preset binary/args) and
    /// which preset it belongs to. `vetted_uid` records which uid's request
    /// vetted the path (IPC applies), or `None` for daemon-selected targets.
    pub fn set_active(
        &self,
        cfg: PathBuf,
        preset: Option<String>,
        kanata_bin: Option<PathBuf>,
        extra_args: Option<Vec<String>>,
        vetted_uid: Option<u32>,
    ) {
        let mut inner = self.lock();
        inner.target.kanata_cfg = cfg;
        if let Some(bin) = kanata_bin {
            inner.target.kanata_bin = bin;
        }
        if let Some(args) = extra_args {
            inner.target.extra_args = args;
        }
        inner.preset = preset;
        inner.target.vetted_uid = vetted_uid;
    }

    /// Record the last-known-good backup path (SPEC §6.4).
    pub fn set_last_known_good(&self, path: PathBuf) {
        self.lock().last_known_good = Some(path);
    }

    /// A snapshot of the preset and last-known-good pointers.
    pub fn snapshot(&self) -> ActiveSnapshot {
        let inner = self.lock();
        ActiveSnapshot {
            preset: inner.preset.clone(),
            last_known_good: inner.last_known_good.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn explicit_setting_always_wins() {
        // Even when candidates exist, an explicit path is used verbatim.
        let bin = resolve_kanata_bin(Some(PathBuf::from("/custom/kanata")), |_| true);
        assert_eq!(bin, Path::new("/custom/kanata"));
    }

    #[test]
    fn first_existing_candidate_wins_in_order() {
        // Both exist → /usr/local first (deterministic; TCC stability, §2).
        let bin = resolve_kanata_bin(None, |_| true);
        assert_eq!(bin, Path::new("/usr/local/bin/kanata"));

        // Only the Apple-silicon brew path exists → it is chosen.
        let bin = resolve_kanata_bin(None, |p| p == Path::new("/opt/homebrew/bin/kanata"));
        assert_eq!(bin, Path::new("/opt/homebrew/bin/kanata"));
    }

    #[test]
    fn nothing_found_falls_back_to_the_canonical_location() {
        // The degraded message / doctor hint should name /usr/local/bin.
        let bin = resolve_kanata_bin(None, |_| false);
        assert_eq!(bin, Path::new("/usr/local/bin/kanata"));
    }
}
