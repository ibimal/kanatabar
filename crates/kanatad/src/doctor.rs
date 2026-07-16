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
    vec![
        daemon_check(started),
        kanata_binary_check(configmgr, health, peer_uid),
        driver_present_check(driver_probe.is_some()),
        driver,
        driver_version_check(driver_probe.is_some(), health).await,
        vhid,
        vhid_managed_check(driver_probe.is_some()).await,
        input_monitoring_check(),
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
             release notes, then run the Setup Wizard to activate + approve it",
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
                "run the Setup Wizard, or `'{}' activate` and approve the extension in \
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
             notes name, then run the Setup Wizard",
        )
    }
}

/// TCC grants cannot be read without private APIs (TCC.db is SIP-protected
/// even from root), so this is informational (always OK); the *runtime*
/// detection is behavioral — a TCC-denied kanata crash is classified from its
/// output into `Degraded{InputMonitoringDenied}` (§6.5).
///
/// Verified on HW (macOS 26.5.1, kanata 1.12, 2026-07-11) by a full grant
/// matrix: the grants attach to **kanatad** (the launchd daemon is the TCC
/// responsible process for its kanata child); BOTH Input Monitoring AND
/// Accessibility are required; the kanata binary itself needs none (its
/// self-registered entry is not consulted); linker-signed ad-hoc binaries
/// hold grants fine. Because grants pin kanatad's code hash, a KanataBar
/// update invalidates them (kanata updates do not).
fn input_monitoring_check() -> DoctorCheck {
    let daemon_bin = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/usr/local/bin/kanatad".to_string());
    DoctorCheck {
        name: checks::INPUT_MONITORING.to_string(),
        ok: true,
        detail: "cannot be verified automatically (TCC is unreadable by design); \
                 a denial is detected at kanata startup and reported as Degraded"
            .to_string(),
        fix_hint: Some(format!(
            "grant BOTH Input Monitoring AND Accessibility to {daemon_bin} in System \
             Settings → Privacy & Security (+ button, Cmd+Shift+G to type the path) — \
             macOS attributes kanata's device access to the supervising daemon, and \
             kanata's own self-registered entry is NOT the one checked. After updating \
             KanataBar, remove (−) and re-add (+) both entries (kanata updates don't \
             affect them)"
        )),
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
    match configmgr.validate(&cfg, peer_uid, None).await {
        Ok(canonical) => ok(
            checks::ACTIVE_CONFIG,
            format!("{}{preset} passes kanata --check", canonical.display()),
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
