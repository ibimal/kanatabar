//! Gate-2 end-to-end test (SPEC §19): the real `kanatactl` binary driving the
//! real `kanatad` binary over a temp-dir socket, with mock-kanata as the child.
//! Exercises exit codes (SPEC §9) and the status/start/stop round-trip. No
//! root, devices, or real kanata (SPEC §17).

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

/// A daemon child, terminated on drop so a failing assert can't leak it —
/// SIGTERM first so the §6.1 graceful shutdown reaps the kanata/mock child
/// (a bare SIGKILL orphans it onto the shared TCP port; HW 2026-07-11).
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Ok(Some(_)) = self.0.try_wait() {
            return; // already exited (and reaped)
        }
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.0.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run `kanatactl --socket <socket> <args...>` and return (exit code, stdout).
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

/// Poll `kanatactl status --json` until the reported state matches, retrying
/// through "cannot connect" (exit 3) while the daemon boots.
fn wait_for_state(socket: &Path, want: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (code, stdout) = ctl(socket, &["status", "--json"]);
        if code != 3 {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
                if json["state"] == want {
                    return json;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "state never became {want}; last stdout: {stdout}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn cli_round_trips_against_daemon_and_mock() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("test.kbd");
    std::fs::write(&cfg, ";; mock\n").unwrap();
    let socket = tmp.path().join("kanatabar.sock");
    let state = tmp.path().join("state");

    let mut daemon = Daemon(
        Command::new(bin("kanatad"))
            .arg("run")
            .env("KANATABAR_SOCK", &socket)
            .env("KANATABAR_CFG", &cfg)
            .env("KANATABAR_KANATA_BIN", bin("mock-kanata"))
            .env("KANATABAR_STATE", &state)
            // No real Karabiner driver in CI; skip the driver preflight (§6.5).
            .env("KANATABAR_SKIP_DRIVER_CHECK", "true")
            .env("RUST_LOG", "warn")
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn kanatad"),
    );

    // The daemon autostarts kanata on boot → status should reach Running.
    let running = wait_for_state(&socket, "Running");
    assert!(
        running["kanata_pid"].as_u64().is_some(),
        "no pid: {running}"
    );
    assert_eq!(running["daemon_version"], env!("CARGO_PKG_VERSION"));

    // Stop → exit 0, status Stopped.
    let (code, _) = ctl(&socket, &["stop"]);
    assert_eq!(code, 0, "stop should exit 0");
    let stopped = wait_for_state(&socket, "Stopped");
    assert_eq!(stopped["kanata_pid"], serde_json::Value::Null);

    // Start again → exit 0, back to Running.
    let (code, _) = ctl(&socket, &["start"]);
    assert_eq!(code, 0, "start should exit 0");
    wait_for_state(&socket, "Running");

    // A human-readable status also succeeds.
    let (code, stdout) = ctl(&socket, &["status"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("state:"),
        "unexpected status output: {stdout}"
    );

    // Graceful shutdown: SIGTERM → clean exit 0 (SPEC §6.1 [HARD]).
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon.0.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("SIGTERM kanatad");
    let status = daemon.0.wait().expect("kanatad exit");
    assert_eq!(status.code(), Some(0), "daemon should exit cleanly");

    // Socket removed on exit (SPEC §7.1).
    assert!(!socket.exists(), "socket left behind after shutdown");

    // With the daemon gone, kanatactl reports "cannot connect" (exit 3, §9).
    let (code, _) = ctl(&socket, &["status"]);
    assert_eq!(code, 3, "expected cannot-connect exit code");
}

/// The Milestone-B surface end-to-end: `logs` collects LogLine frames until
/// the Ack (SPEC §6.6), `devices` renders the registry, `config validate`
/// accepts/refuses, and `preset list` reports the empty default.
#[test]
fn logs_devices_config_and_presets_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("test.kbd");
    std::fs::write(&cfg, ";; mock\n").unwrap();
    let socket = tmp.path().join("kanatabar.sock");
    let state = tmp.path().join("state");

    let daemon = Daemon(
        Command::new(bin("kanatad"))
            .arg("run")
            .env("KANATABAR_SOCK", &socket)
            .env("KANATABAR_CFG", &cfg)
            .env("KANATABAR_KANATA_BIN", bin("mock-kanata"))
            .env("KANATABAR_STATE", &state)
            .env("KANATABAR_SKIP_DRIVER_CHECK", "true")
            // The ring mirrors the log filter: keep info-level so it has content.
            .env("RUST_LOG", "info")
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn kanatad"),
    );
    wait_for_state(&socket, "Running");

    // logs: startup lines are buffered; the multi-frame exchange terminates.
    let (code, stdout) = ctl(&socket, &["logs", "-n", "100"]);
    assert_eq!(code, 0, "logs failed: {stdout}");
    assert!(
        stdout.contains("supervisor loop started") || stdout.contains("kanata spawned"),
        "expected startup lines in the ring:\n{stdout}"
    );

    // devices: environment-dependent — on a Mac where the IOKit monitor arms
    // even unprivileged, the initial enumeration lists real devices; in a
    // headless/CI sandbox the empty hint renders. Either way: exit 0, output.
    let (code, stdout) = ctl(&socket, &["devices"]);
    assert_eq!(code, 0, "devices failed: {stdout}");
    assert!(!stdout.trim().is_empty(), "devices printed nothing");

    // config validate: good and broken (mock-kanata rejects "BROKEN").
    let good = tmp.path().join("good.kbd");
    std::fs::write(&good, ";; fine\n").unwrap();
    let (code, stdout) = ctl(&socket, &["config", "validate", good.to_str().unwrap()]);
    assert_eq!(code, 0, "validate failed: {stdout}");
    let broken = tmp.path().join("broken.kbd");
    std::fs::write(&broken, "BROKEN\n").unwrap();
    let (code, _) = ctl(&socket, &["config", "validate", broken.to_str().unwrap()]);
    assert_eq!(code, 1, "a broken config must be an operational error");

    // preset list: none configured → the guided empty-state, exit 0. (The
    // suggestion tail depends on the tester's ~/.config/kanata, so assert only
    // the stable header + the add-command hint.)
    let (code, stdout) = ctl(&socket, &["preset", "list"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("No presets configured."), "{stdout}");
    assert!(stdout.contains("kanatactl preset add"), "{stdout}");

    // autostart with no active preset → operational error, exit 1.
    let (code, _) = ctl(&socket, &["autostart", "on"]);
    assert_eq!(code, 1);

    drop(daemon); // graceful SIGTERM via the guard
}
