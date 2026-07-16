//! Phase 7 `[AUTO]` gate (SPEC §19): the tray's UI-less client logic driven
//! end-to-end against the real `kanatad` + `mock-kanata` over a temp-dir socket
//! — no display, no root, no real kanata (SPEC §17). Exercises the same
//! connect → seed → subscribe → reconnect loop the GUI shell runs, asserting
//! the resulting `MenuModel` tracks daemon state and that a menu command
//! (`Stop`) is delivered.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use kanatabar_core::backoff::BackoffConfig;
use kanatabar_core::ipc::RequestPayload;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::UnboundedReceiver;

use kanatabar_tray::conn::{self, Update};
use kanatabar_tray::model::{IconKind, MenuModel};

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

/// A daemon child that is SIGKILLed on drop so a failing assert can't leak it.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_daemon(socket: &PathBuf, cfg: &PathBuf, state: &PathBuf) -> Daemon {
    let child = Command::new(bin("kanatad"))
        .arg("run")
        .env("KANATABAR_SOCK", socket)
        .env("KANATABAR_CFG", cfg)
        .env("KANATABAR_KANATA_BIN", bin("mock-kanata"))
        .env("KANATABAR_STATE", state)
        .env("KANATABAR_SKIP_DRIVER_CHECK", "true")
        .env("RUST_LOG", "warn")
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn kanatad");
    Daemon(child)
}

fn new_runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// Fast reconnect so tests don't wait out the 1s default base delay while the
/// daemon boots.
fn fast_backoff() -> BackoffConfig {
    BackoffConfig {
        base_ms: 50,
        cap_ms: 200,
        budget: 5,
        reset_after_s: 60,
    }
}

/// Drain model updates until `pred` matches one, or panic after `timeout`.
fn wait_for_model<F>(
    rt: &Runtime,
    rx: &mut UnboundedReceiver<Update>,
    timeout: Duration,
    mut pred: F,
) -> MenuModel
where
    F: FnMut(&MenuModel) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("timed out waiting for a matching model update");
        let update = rt
            .block_on(async { tokio::time::timeout(remaining, rx.recv()).await })
            .expect("timed out waiting for a model update")
            .expect("update channel closed");
        if let Update::Model(model) = update {
            if pred(&model) {
                return model;
            }
        }
    }
}

#[test]
fn tray_client_tracks_daemon_state_and_delivers_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("test.kbd");
    std::fs::write(&cfg, ";; mock\n").unwrap();
    let socket = tmp.path().join("kanatabar.sock");
    let state = tmp.path().join("state");

    let rt = new_runtime();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Update>();
    let stream = rt.spawn(conn::run_event_stream(socket.clone(), tx, fast_backoff()));

    // The daemon starts after the client is already looping → exercises the
    // reconnect path (SPEC §8): the client retries until the socket appears.
    let _daemon = spawn_daemon(&socket, &cfg, &state);

    // The daemon autostarts kanata → the model should reach a connected,
    // Running state.
    let running = wait_for_model(&rt, &mut rx, Duration::from_secs(20), |m| {
        m.connected && m.icon == IconKind::Running
    });
    assert_eq!(running.state_line, "State: Running");

    // Issue a menu command over its own connection (SPEC §8) and observe the
    // model follow the daemon to Stopped.
    rt.block_on(conn::send_command(&socket, RequestPayload::Stop))
        .expect("Stop command accepted");
    let stopped = wait_for_model(&rt, &mut rx, Duration::from_secs(20), |m| {
        m.connected && m.state_line == "State: Stopped"
    });
    assert_eq!(stopped.icon, IconKind::Idle);

    stream.abort();
}

#[test]
fn tray_client_reports_disconnected_when_no_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("absent.sock");

    let rt = new_runtime();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Update>();
    let stream = rt.spawn(conn::run_event_stream(socket, tx, fast_backoff()));

    // With nothing listening, the very first model pushed is "disconnected".
    let model = wait_for_model(&rt, &mut rx, Duration::from_secs(5), |_| true);
    assert!(!model.connected);
    assert_eq!(model.icon, IconKind::Disconnected);

    stream.abort();
}
