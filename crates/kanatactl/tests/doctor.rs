//! Phase 8 `[AUTO]` gate (SPEC §19): `kanatactl doctor` end-to-end against the
//! real `kanatad` + `mock-kanata` over a temp-dir socket — no root, no real
//! kanata, no Karabiner (SPEC §17). Pins the `DoctorReport` JSON schema on the
//! real path and the offline behaviour (an unreachable daemon is itself the
//! first failed check, SPEC §9).

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

fn target_dir() -> PathBuf {
    let mut dir = std::env::current_exe().expect("test exe path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir
}

fn bin(name: &str) -> PathBuf {
    let path = target_dir().join(name);
    assert!(
        path.exists(),
        "run `cargo build --workspace` first: {}",
        path.display()
    );
    path
}

/// Run `kanatactl --socket <socket> <args...>`; return (exit code, stdout).
fn ctl(socket: &Path, args: &[&str]) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_kanatactl"))
        .arg("--socket")
        .arg(socket)
        .args(args)
        .output()
        .expect("run kanatactl");
    let code = output.status.code().expect("kanatactl exit code");
    (code, String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A daemon child, terminated on drop so a failing assert can't leak it.
/// SIGTERM first (the daemon's §6.1 graceful shutdown reaps its kanata/mock
/// child — a SIGKILL would orphan it onto the shared TCP port, the leak that
/// broke the HW pass on 2026-07-11), then SIGKILL if it dawdles.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        terminate_gracefully(&mut self.0);
    }
}

fn terminate_gracefully(child: &mut Child) {
    if let Ok(Some(_)) = child.try_wait() {
        return; // already exited (and reaped) — nothing to signal
    }
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn wait_for_state(socket: &Path, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (code, stdout) = ctl(socket, &["status", "--json"]);
        if code != 3 {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
                if json["state"] == want {
                    return;
                }
            }
        }
        assert!(Instant::now() < deadline, "state never became {want}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn check_named<'a>(checks: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    checks
        .as_array()
        .expect("checks is an array")
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no check named {name} in {checks}"))
}

#[test]
fn doctor_reports_a_stable_all_green_report_against_the_mock() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("test.kbd");
    std::fs::write(&cfg, ";; mock\n").unwrap();
    let socket = tmp.path().join("kanatabar.sock");
    let state = tmp.path().join("state");

    let _daemon = Daemon(
        Command::new(bin("kanatad"))
            .arg("run")
            .env("KANATABAR_SOCK", &socket)
            .env("KANATABAR_CFG", &cfg)
            .env("KANATABAR_KANATA_BIN", bin("mock-kanata"))
            .env("KANATABAR_STATE", &state)
            .env("KANATABAR_SKIP_DRIVER_CHECK", "true")
            .env("RUST_LOG", "warn")
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn kanatad"),
    );
    wait_for_state(&socket, "Running");

    // Human-readable form: a checklist with marks, exit 0 when all green.
    let (code, stdout) = ctl(&socket, &["doctor"]);
    assert_eq!(code, 0, "all-green doctor should exit 0; got:\n{stdout}");
    assert!(stdout.contains('✅'), "expected checkmarks:\n{stdout}");
    assert!(stdout.contains("All checks passed"), "{stdout}");

    // JSON form: stable schema — `{"checks":[{name,ok,detail,fix_hint}, …]}`.
    let (code, stdout) = ctl(&socket, &["doctor", "--json"]);
    assert_eq!(code, 0);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("doctor --json is JSON");
    let checks = &report["checks"];

    // The full check set is present, in schema, and all green in the mock env.
    for name in [
        "daemon",
        "kanata binary",
        "karabiner driver",
        "vhid daemon",
        "input monitoring",
        "control socket",
        "active config",
        "supervisor",
    ] {
        let check = check_named(checks, name);
        assert_eq!(check["ok"], true, "{name} should be ok: {check}");
        assert!(check["name"].is_string(), "{name}: name");
        assert!(check["detail"].is_string(), "{name}: detail");
        // fix_hint is present (nullable) on every check.
        assert!(check.get("fix_hint").is_some(), "{name}: fix_hint key");
    }
    assert_eq!(
        check_named(checks, "daemon")["fix_hint"],
        serde_json::Value::Null
    );
}

#[test]
fn doctor_is_useful_offline_with_the_daemon_check_failing() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("absent.sock");

    // No daemon: exit 3 (cannot connect), but still a rendered report.
    let (code, stdout) = ctl(&socket, &["doctor"]);
    assert_eq!(code, 3, "offline doctor should exit 3 (cannot connect)");
    assert!(stdout.contains('❌'), "expected a failed check:\n{stdout}");

    let (code, stdout) = ctl(&socket, &["doctor", "--json"]);
    assert_eq!(code, 3);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("offline doctor is JSON");
    let daemon = check_named(&report["checks"], "daemon");
    assert_eq!(daemon["ok"], false);
    assert!(
        daemon["fix_hint"].is_string(),
        "offline daemon check has a hint"
    );
}
