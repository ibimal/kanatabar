//! `doctor` — the full preflight checklist (SPEC §9), served over IPC as a
//! `DoctorReport` (SPEC §7.2) and reused by the tray's first-run wizard
//! (SPEC §11: "single implementation in core/daemon"). This is also the
//! manual-QA oracle.
//!
//! The stable check *names* and the pass/fail aggregation live in
//! `kanatabar_core::doctor`; the actual probing (subprocesses, filesystem
//! stat, `kanata --check`) is I/O and lives here. Each check is independent and
//! degrades to an actionable failure rather than panicking (SPEC §15).

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

use kanatabar_core::doctor::checks;
use kanatabar_core::ipc::DoctorCheck;
use kanatabar_core::state::SupervisorState;

use crate::configmgr::ConfigManager;
use crate::health::driver::{DriverHealth, DriverProbe};
use crate::health::HealthState;
use crate::supervisor::SupervisorClient;

/// Expected control-socket permission bits (SPEC §3.2, §7.1: mode 0660).
const EXPECTED_SOCKET_MODE: u32 = 0o660;

fn ok(name: &str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        ok: true,
        detail: detail.into(),
        fix_hint: None,
    }
}

fn fail(name: &str, detail: impl Into<String>, fix: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        ok: false,
        detail: detail.into(),
        fix_hint: Some(fix.into()),
    }
}

/// Run every check and return the report in canonical order (SPEC §9).
pub async fn run(
    supervisor: &SupervisorClient,
    health: &HealthState,
    configmgr: &ConfigManager,
    socket_path: &Path,
    driver_probe: Option<&DriverProbe>,
    peer_uid: u32,
    started: Instant,
) -> Vec<DoctorCheck> {
    let (driver, vhid) = driver_checks(driver_probe).await;
    let (input_monitoring, accessibility) = permission_checks().await;
    vec![
        daemon_check(started),
        kanata_binary_check(configmgr, health, peer_uid),
        driver_present_check(driver_probe.is_some()),
        driver,
        driver_version_check(driver_probe.is_some(), health).await,
        vhid,
        vhid_managed_check(driver_probe.is_some()).await,
        input_monitoring,
        accessibility,
        socket_check(socket_path),
        active_config_check(configmgr, peer_uid).await,
        config_file_check(configmgr).await,
        supervisor_check(supervisor),
    ]
}

/// SPEC §7.3/§9: report whether `config.toml` loaded. A present-but-broken
/// file is a **failure** — its presets and defaults are ignored — so the v0.1.0
/// silent-fallback (empty presets, no signal) can't recur. Missing is fine.
async fn config_file_check(configmgr: &ConfigManager) -> DoctorCheck {
    use kanatabar_core::config::ConfigStatus;
    match configmgr.config_status().await {
        ConfigStatus::Missing => ok(
            checks::CONFIG_FILE,
            "no config.toml (using built-in defaults)",
        ),
        ConfigStatus::Loaded { presets } => ok(
            checks::CONFIG_FILE,
            format!("config.toml loaded ({presets} preset(s))"),
        ),
        ConfigStatus::Invalid { error } => fail(
            checks::CONFIG_FILE,
            format!("config.toml is invalid: {error}"),
            "fix the syntax (it must start with `schema = 1`), then run \
             `kanatactl config reload` — its presets and defaults are ignored until then",
        ),
    }
}

/// SPEC §2/§6.5a: each kanata release pins a supported driver version —
/// "install the latest" can be wrong. Fails only on a *known* mismatch;
/// unknown couplings and skipped preflights report ok with detail (honest
/// oracle, [VERIFY] per release).
async fn driver_version_check(probe_enabled: bool, health: &HealthState) -> DoctorCheck {
    if !probe_enabled {
        return ok(
            checks::DRIVER_VERSION,
            "skipped (driver preflight disabled via --skip-driver-check)",
        );
    }
    let Some(output) = crate::health::driver::systemextensions_output().await else {
        return ok(checks::DRIVER_VERSION, "systemextensionsctl unavailable");
    };
    let Some(driver) = kanatabar_core::driver::parse_driver_version(&output) else {
        return ok(
            checks::DRIVER_VERSION,
            "driver not listed; see the `karabiner driver` check",
        );
    };
    let kanata = health
        .snapshot()
        .kanata_version
        .and_then(|v| kanatabar_core::kanata::parse_version(&v));
    match kanata.and_then(kanatabar_core::driver::supported_driver_major) {
        Some(expected) if driver.major != expected => fail(
            checks::DRIVER_VERSION,
            format!("driver extension v{driver} installed, but this kanata supports v{expected}.x"),
            format!(
                "install the Karabiner-DriverKit-VirtualHIDDevice release \
                 named in your kanata release notes (its extension is v{expected}.x; \
                 newest is not always right)"
            ),
        ),
        Some(expected) => ok(
            checks::DRIVER_VERSION,
            format!("driver extension v{driver} (kanata supports v{expected}.x)"),
        ),
        None => ok(
            checks::DRIVER_VERSION,
            format!(
                "driver extension bundle v{driver} — note this differs from the pqrs \
                 pkg/release version; check your kanata release notes name the pkg you installed"
            ),
        ),
    }
}

/// SPEC §6.5a: something must keep the VHID daemon alive across reboots —
/// ours, Karabiner-Elements', or a user plist. Unmanaged is the failure
/// `sudo kanatactl install` fixes.
async fn vhid_managed_check(probe_enabled: bool) -> DoctorCheck {
    if !probe_enabled {
        return ok(
            checks::VHID_MANAGED,
            "skipped (driver preflight disabled via --skip-driver-check)",
        );
    }
    let labels = crate::health::driver::system_launchd_labels().await;
    let management = kanatabar_core::vhidd::classify(labels.iter().map(String::as_str));
    match management {
        kanatabar_core::vhidd::VhiddManagement::Unmanaged => fail(
            checks::VHID_MANAGED,
            management.describe(),
            "run `sudo kanatactl install` to register KanataBar's VHID-daemon LaunchDaemon",
        ),
        managed => ok(checks::VHID_MANAGED, managed.describe()),
    }
}

fn daemon_check(started: Instant) -> DoctorCheck {
    ok(
        checks::DAEMON,
        format!(
            "kanatad {} reachable, up {}s",
            env!("CARGO_PKG_VERSION"),
            started.elapsed().as_secs()
        ),
    )
}

fn kanata_binary_check(
    configmgr: &ConfigManager,
    health: &HealthState,
    peer_uid: u32,
) -> DoctorCheck {
    let bin = configmgr.active_target().kanata_bin;
    let version = health.snapshot().kanata_version;
    if bin.is_file() {
        let version = version.unwrap_or_else(|| "version unknown".to_string());
        return ok(
            checks::KANATA_BINARY,
            format!("{} ({version})", bin.display()),
        );
    }
    // Not in the auto-detected locations. Before the generic message, look
    // where `cargo install kanata` / MacPorts put it (a common case an early
    // user hit) so we can point the user at the exact fix instead of "not
    // found". We never auto-run it from there — that would trust a
    // user-writable path from root (§14); the user opts in via kanata_bin.
    if let Some(found) = alt_kanata_location(peer_uid) {
        return fail(
            checks::KANATA_BINARY,
            format!(
                "not in the auto-detected locations, but found at {}",
                found.display()
            ),
            format!(
                "that path isn't auto-trusted (the root daemon won't run a user-writable \
                 binary). Point KanataBar at it: add to config.toml under [defaults]\n    \
                 kanata_bin = \"{}\"\nthen restart the daemon: \
                 sudo launchctl kickstart -k system/io.github.ibimal.kanatabar.daemon",
                found.display()
            ),
        );
    }
    fail(
        checks::KANATA_BINARY,
        format!("not found: {}", bin.display()),
        format!(
            "install kanata (e.g. `brew install kanata`) — the daemon auto-detects {} — \
             or set kanata_bin in config.toml, then restart kanatad",
            kanatabar_core::kanata::KANATA_BIN_CANDIDATES.join(" or ")
        ),
    )
}

/// Probe the common non-allowlist kanata locations for the requesting user
/// (their `~/.cargo/bin`, MacPorts), returning the first that exists. Used only
/// to enrich the "not found" message (SPEC §7.3, §14).
fn alt_kanata_location(peer_uid: u32) -> Option<PathBuf> {
    let home = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(peer_uid))
        .ok()
        .flatten()
        .map(|u| u.dir)?;
    kanatabar_core::kanata::alt_kanata_locations(&home)
        .into_iter()
        .find(|p| p.is_file())
}

/// SPEC §11: the wizard's *install* step verifies the driver pkg is present on
/// disk (the VirtualHIDDevice-Manager app the pkg installs), which is distinct
/// from the *activate* step's `checks::DRIVER` ("activated + enabled"). Two
/// distinct checks are what let the wizard advance install → activate: on a
/// genuine first run the pkg installs the manager app *before* the extension is
/// registered/activated, so this passes while `checks::DRIVER` still fails
/// (HW Run 3, 2026-07-13 — before this both verified `DRIVER` and the activate
/// step was unreachable). The manager-app-on-disk signal is the same one
/// `driver_not_activated_check` uses to tell pkg-missing from not-activated.
/// Skipped-OK under `--skip-driver-check` so dev/CI doctor stays green (§6.5).
fn driver_present_check(probe_enabled: bool) -> DoctorCheck {
    if !probe_enabled {
        return ok(
            checks::DRIVER_PRESENT,
            "skipped (driver preflight disabled via --skip-driver-check)",
        );
    }
    let manager = Path::new(kanatabar_core::vhidd::MANAGER_BINARY);
    if manager.is_file() {
        ok(
            checks::DRIVER_PRESENT,
            format!(
                "Karabiner-VirtualHIDDevice-Manager present ({})",
                manager.display()
            ),
        )
    } else {
        fail(
            checks::DRIVER_PRESENT,
            "Karabiner-DriverKit-VirtualHIDDevice pkg not installed \
             (no VirtualHIDDevice-Manager on disk)",
            "install the Karabiner-DriverKit-VirtualHIDDevice pkg named in your kanata \
             release notes, then run the Setup Assistant to activate + approve it",
        )
    }
}

/// One driver probe → the driver + VHID-daemon checks. With no probe (dev/CI
/// `--skip-driver-check`, SPEC §6.5) both are reported as skipped-OK so the
/// oracle is honest about what ran.
async fn driver_checks(driver_probe: Option<&DriverProbe>) -> (DoctorCheck, DoctorCheck) {
    let Some(probe) = driver_probe else {
        let detail = "skipped (driver preflight disabled via --skip-driver-check)";
        return (ok(checks::DRIVER, detail), ok(checks::VHID_DAEMON, detail));
    };
    match probe().await {
        DriverHealth::Ok => (
            ok(
                checks::DRIVER,
                "Karabiner DriverKit extension activated + enabled",
            ),
            ok(
                checks::VHID_DAEMON,
                "Karabiner VirtualHIDDevice daemon running",
            ),
        ),
        DriverHealth::DriverNotActivated => (
            driver_not_activated_check(),
            fail(
                checks::VHID_DAEMON,
                "not verified — activate the driver first",
                "activate the Karabiner driver, then re-run doctor",
            ),
        ),
        DriverHealth::VhidDaemonDown => (
            ok(
                checks::DRIVER,
                "Karabiner DriverKit extension activated + enabled",
            ),
            fail(
                checks::VHID_DAEMON,
                "Karabiner VirtualHIDDevice daemon is not running",
                "reinstall the Karabiner-VirtualHIDDevice driver (it ships the daemon)",
            ),
        ),
    }
}

/// A precise `karabiner driver` failure: whether the *pkg* is missing (no
/// manager app on disk → install it) or merely not activated/approved (manager
/// present → run its documented `activate` request, then approve). The
/// activation invocation is the pqrs-README one (`core::vhidd::MANAGER_BINARY
/// activate`, no sudo — the wizard runs it automatically).
fn driver_not_activated_check() -> DoctorCheck {
    let manager = Path::new(kanatabar_core::vhidd::MANAGER_BINARY);
    if manager.is_file() {
        fail(
            checks::DRIVER,
            "Karabiner DriverKit extension is not activated + enabled",
            format!(
                "run the Setup Assistant, or `'{}' activate` and approve the extension in \
                 System Settings → Privacy & Security",
                manager.display()
            ),
        )
    } else {
        fail(
            checks::DRIVER,
            "Karabiner-DriverKit-VirtualHIDDevice is not installed \
             (no VirtualHIDDevice-Manager on disk)",
            "install the Karabiner-DriverKit-VirtualHIDDevice pkg the kanata release \
             notes name, then run the Setup Assistant",
        )
    }
}

/// kanatad's own binary path, for the permission fix hints (macOS shows the
/// grant against this path). Falls back to the install location.
fn daemon_bin_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/usr/local/bin/kanatad".to_string())
}

/// The shared tail of both permission hints: TCC binds to kanatad's code
/// identity, so a KanataBar update silently invalidates the grants even
/// though the entries still show enabled — remove (−) and re-add (+) is the
/// fix (SPEC §2 [HARD, verified]). Kanata updates do not affect them.
const GRANT_HINT_TAIL: &str = "add it with the + button (Cmd+Shift+G types the path). \
     macOS attributes kanata's device access to the supervising daemon, so kanata's own \
     self-registered entry is NOT the one checked. After updating KanataBar, remove (−) \
     and re-add (+) the entry (kanata updates don't affect it)";

/// Test/CI escape hatch (mirrors `--skip-driver-check`): a real TCC read only
/// reflects a grant when kanatad runs as the root LaunchDaemon, which `cargo
/// test` can't provide, so tests set `KANATABAR_SKIP_PERMISSION_CHECK` to keep
/// the permission checks green. Never set in production.
pub(crate) fn skip_permission_checks() -> bool {
    std::env::var_os("KANATABAR_SKIP_PERMISSION_CHECK").is_some()
}

const PERMISSION_SKIPPED: &str = "skipped (permission preflight disabled)";

/// The `kanatad tcc-status` probe output: this process's own grant reads, one
/// `key=value` per line. Runs in the *spawned* probe child, so each call is a
/// fresh TCC evaluation (the whole point — see [`probe_tcc_status`]).
pub fn tcc_status_output() -> String {
    use crate::ffi::tcc;
    format!(
        "input_monitoring={}\naccessibility={}\n",
        tcc::input_monitoring_access(),
        tcc::accessibility_trusted()
    )
}

/// Parse [`tcc_status_output`]. `None` on any malformed/missing field —
/// callers fall back to the in-process read rather than guess.
fn parse_tcc_status(text: &str) -> Option<(crate::ffi::tcc::AccessStatus, bool)> {
    let mut im = None;
    let mut ax = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("input_monitoring=") {
            im = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("accessibility=") {
            ax = v.trim().parse::<bool>().ok();
        }
    }
    Some((im?, ax?))
}

/// Read the daemon's TCC grants via a **freshly spawned probe child**
/// (`kanatad tcc-status`). The parent's own verdict is launch-cached in both
/// directions (HW 2026-07-18, docs/HW-TESTS.md #19: a revoke still read
/// `granted` 60+ s later), but TCC attributes a child to the responsible
/// process — kanatad — so the child's read is evaluated afresh, giving the
/// doctor live status the way a GUI app's poll gets it. `None` when the probe
/// can't run or emits garbage; callers fall back to the in-process read.
///
/// HW-confirmed live both directions for BOTH permissions (ledger #19,
/// 2026-07-18). §14 note: the probe is `current_exe()` — the very binary
/// already running as root, not a user-writable path lookup.
async fn probe_tcc_status() -> Option<(crate::ffi::tcc::AccessStatus, bool)> {
    let exe = std::env::current_exe().ok()?;
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::process::Command::new(exe).arg("tcc-status").output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_tcc_status(&String::from_utf8(output.stdout).ok()?)
}

/// Live answer to "does kanatad hold BOTH grants right now?" via the fresh
/// probe — the supervisor's retry-on-grant watch uses this while
/// `Degraded{InputMonitoringDenied}` (SPEC §6.5). `None` when the probe
/// fails (the watch keeps waiting rather than guessing; the in-process
/// fallback would be launch-cached and could never observe the grant).
pub(crate) async fn tcc_grants_ready() -> Option<bool> {
    let (im, ax) = probe_tcc_status().await?;
    Some(im == crate::ffi::tcc::AccessStatus::Granted && ax)
}

/// Both permission checks, from one probe run (fresh-child read, in-process
/// fallback). Returns `(input_monitoring, accessibility)`.
async fn permission_checks() -> (DoctorCheck, DoctorCheck) {
    use crate::ffi::tcc;
    if skip_permission_checks() {
        return (
            ok(checks::INPUT_MONITORING, PERMISSION_SKIPPED),
            ok(checks::ACCESSIBILITY, PERMISSION_SKIPPED),
        );
    }
    let (im, ax, via) = match probe_tcc_status().await {
        Some((im, ax)) => (im, ax, "fresh probe"),
        // In-process fallback: accurate at daemon start, but launch-cached —
        // stale across grant/revoke until the daemon restarts.
        None => (
            tcc::input_monitoring_access(),
            tcc::accessibility_trusted(),
            "in-process read; launch-cached",
        ),
    };
    (
        input_monitoring_check(im, via),
        accessibility_check(ax, via),
    )
}

/// Input Monitoring grant for kanatad, read from the daemon's **own** access
/// via `IOHIDCheckAccess` (SPEC §9, §11). The grants that govern kanata's
/// device access attach to kanatad (the launchd job is the TCC responsible
/// process — SPEC §2 [HARD]); the doctor runs in-process here, so it reads the
/// grant that actually matters.
///
/// The daemon-context read is **HW-confirmed** (docs/HW-TESTS.md #19,
/// 2026-07-17): a granted daemon reads `Granted`, an un/stale-granted one
/// reads `Denied`. So both `Denied` and `Unknown` (never granted) fail the
/// check honestly; only `Granted` passes. Note a grant takes effect only
/// after the daemon restarts (a process caches its launch-time TCC decision),
/// so this stays red until then even after the user toggles the pane — which
/// is correct, since remapping is genuinely broken until the restart.
fn input_monitoring_check(status: crate::ffi::tcc::AccessStatus, via: &str) -> DoctorCheck {
    use crate::ffi::tcc::AccessStatus;
    let hint = format!(
        "grant Input Monitoring to {} in System Settings → Privacy & Security → \
         Input Monitoring — {GRANT_HINT_TAIL}",
        daemon_bin_path()
    );
    match status {
        AccessStatus::Granted => ok(
            checks::INPUT_MONITORING,
            format!("granted (IOHIDCheckAccess, {via})"),
        ),
        AccessStatus::Denied => fail(
            checks::INPUT_MONITORING,
            format!("denied — kanatad is not permitted to monitor input ({via})"),
            hint,
        ),
        AccessStatus::Unknown => fail(
            checks::INPUT_MONITORING,
            format!("not granted — kanatad has no Input Monitoring grant ({via})"),
            hint,
        ),
    }
}

/// Accessibility grant for kanatad, read from the daemon's **own** trust
/// state via `AXIsProcessTrusted` (SPEC §9, §11). macOS requires BOTH this and
/// Input Monitoring; surfacing them as separate checks lets the wizard guide
/// each grant independently. HW-confirmed alongside Input Monitoring
/// (docs/HW-TESTS.md #19): `trusted` passes, not-trusted fails. Same
/// restart-to-take-effect caveat as [`input_monitoring_check`].
fn accessibility_check(trusted: bool, via: &str) -> DoctorCheck {
    let hint = format!(
        "grant Accessibility to {} in System Settings → Privacy & Security → \
         Accessibility — {GRANT_HINT_TAIL}",
        daemon_bin_path()
    );
    if trusted {
        ok(
            checks::ACCESSIBILITY,
            format!("granted (AXIsProcessTrusted, {via})"),
        )
    } else {
        fail(
            checks::ACCESSIBILITY,
            format!("not granted — kanatad is not a trusted Accessibility client ({via})"),
            hint,
        )
    }
}

fn socket_check(socket_path: &Path) -> DoctorCheck {
    match std::fs::metadata(socket_path) {
        Ok(meta) => {
            let mode = meta.permissions().mode() & 0o777;
            if mode == EXPECTED_SOCKET_MODE {
                ok(
                    checks::CONTROL_SOCKET,
                    format!(
                        "{} (uid {}, mode {:o})",
                        socket_path.display(),
                        meta.uid(),
                        mode
                    ),
                )
            } else {
                fail(
                    checks::CONTROL_SOCKET,
                    format!(
                        "{} has mode {:o}, expected {:o}",
                        socket_path.display(),
                        mode,
                        EXPECTED_SOCKET_MODE
                    ),
                    "reinstall KanataBar; the socket must be root:staff mode 0660",
                )
            }
        }
        Err(err) => fail(
            checks::CONTROL_SOCKET,
            format!("{}: {err}", socket_path.display()),
            "the daemon recreates the socket on start; restart kanatad",
        ),
    }
}

async fn active_config_check(configmgr: &ConfigManager, peer_uid: u32) -> DoctorCheck {
    let cfg = configmgr.active_target().kanata_cfg;
    let preset = configmgr
        .active_preset()
        .map(|p| format!(" (preset `{p}`)"))
        .unwrap_or_default();
    let passthrough = configmgr.active_is_passthrough();
    match configmgr.validate(&cfg, peer_uid, None).await {
        Ok(canonical) => ok(
            checks::ACTIVE_CONFIG,
            if passthrough {
                // Don't surface the internal safe.kbd path — name what it is.
                "passthrough (no preset active — remapping nothing) passes kanata --check"
                    .to_string()
            } else {
                format!("{}{preset} passes kanata --check", canonical.display())
            },
        ),
        Err(err) => fail(
            checks::ACTIVE_CONFIG,
            format!("{}{preset}: {err}", cfg.display()),
            "fix the .kbd, switch preset, or roll back to last-known-good",
        ),
    }
}

fn supervisor_check(supervisor: &SupervisorClient) -> DoctorCheck {
    let snapshot = supervisor.snapshot();
    let detail = format!("state: {:?}", snapshot.state);
    match snapshot.state {
        SupervisorState::Degraded => fail(
            checks::SUPERVISOR,
            detail,
            snapshot
                .degraded_reason
                .map(|r| r.describe().to_string())
                .unwrap_or_else(|| "see `kanatactl status`".to_string()),
        ),
        _ => ok(checks::SUPERVISOR, detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::tcc::AccessStatus;

    #[test]
    fn tcc_status_output_round_trips_through_the_parser() {
        // The probe child prints via tcc_status_output; the daemon parses the
        // same shape back. Real grant values vary by context, so pin the
        // format with synthetic lines and round-trip the live output's shape.
        let (im, ax) =
            parse_tcc_status("input_monitoring=denied\naccessibility=true\n").expect("parses");
        assert_eq!(im, AccessStatus::Denied);
        assert!(ax);
        let (im, ax) = parse_tcc_status("accessibility=false\ninput_monitoring=granted\n")
            .expect("order-independent");
        assert_eq!(im, AccessStatus::Granted);
        assert!(!ax);
        // The live output (whatever this test context's grants are) parses.
        assert!(parse_tcc_status(&tcc_status_output()).is_some());
    }

    #[test]
    fn malformed_probe_output_is_rejected_not_guessed() {
        for bad in [
            "",
            "input_monitoring=granted\n", // missing accessibility
            "accessibility=true\n",       // missing input monitoring
            "input_monitoring=yes\naccessibility=true", // unknown value
            "input_monitoring=granted\naccessibility=1", // non-bool
            "garbage",
        ] {
            assert!(parse_tcc_status(bad).is_none(), "accepted: {bad:?}");
        }
    }
}
