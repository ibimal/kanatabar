//! Gate-1 integration tests (SPEC §19): the supervisor loop driven against
//! mock-kanata in a temp dir — crash→backoff→degraded, healthy-window reset,
//! graceful SIGTERM, transitions logged. No root, devices, or real kanata
//! (SPEC §17).

use std::path::PathBuf;
use std::time::Duration;

use kanatabar_core::backoff::BackoffConfig;
use kanatabar_core::ipc::Event;
use kanatabar_core::machine::StateChanged;
use kanatabar_core::state::{DegradedReason, SupervisorState as S};
use kanatad::config::SupervisorConfig;
use kanatad::supervisor::{self, Command, SupervisorHandle};
use tokio::sync::broadcast;
use tokio::time::timeout;

/// `target/debug` from a test executable in `target/debug/deps`.
fn target_dir() -> PathBuf {
    let mut dir = std::env::current_exe().expect("test exe path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir
}

/// mock-kanata is built by `cargo test --workspace` (its own integration test
/// forces the bin target) and by the gate's `cargo build --workspace`.
fn mock_bin() -> PathBuf {
    let bin = target_dir().join("mock-kanata");
    assert!(
        bin.exists(),
        "mock-kanata not built at {}; run `cargo build --workspace` first",
        bin.display()
    );
    bin
}

struct Harness {
    handle: SupervisorHandle,
    events: broadcast::Receiver<StateChanged>,
    /// The daemon event bus the supervisor publishes `Event::Crash` on
    /// (SPEC §8); the control server shares this same bus in production.
    bus: broadcast::Receiver<Event>,
    _tmp: tempfile::TempDir,
}

/// Start a supervisor against mock-kanata with fast test timings.
fn harness(extra_args: &[&str], mutate: impl FnOnce(&mut SupervisorConfig)) -> Harness {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg_path = tmp.path().join("test.kbd");
    std::fs::write(&cfg_path, ";; mock config\n").expect("write cfg");

    let mut config = SupervisorConfig::new(mock_bin(), cfg_path);
    config.extra_args = extra_args.iter().map(ToString::to_string).collect();
    config.state_dir = Some(tmp.path().join("state"));
    config.backoff = BackoffConfig {
        base_ms: 20,
        cap_ms: 100,
        budget: 3,
        reset_after_s: 60,
    };
    config.kill_grace = Duration::from_secs(2);
    mutate(&mut config);

    // Subscribe to the crash-event bus before the supervisor starts so no
    // published `Event::Crash` is missed.
    let bus = config.events.subscribe();
    let handle = supervisor::start(config);
    let events = handle.subscribe();
    Harness {
        handle,
        events,
        bus,
        _tmp: tmp,
    }
}

/// Collect `to`-states until `stop` matches (inclusive) or `max` transitions.
async fn collect_until(
    events: &mut broadcast::Receiver<StateChanged>,
    stop: impl Fn(&StateChanged) -> bool,
    max: usize,
) -> Vec<StateChanged> {
    let mut seen = Vec::new();
    while seen.len() < max {
        let change = timeout(Duration::from_secs(10), events.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for transition; saw {seen:?}"))
            .expect("event stream closed");
        let done = stop(&change);
        seen.push(change);
        if done {
            return seen;
        }
    }
    panic!("no matching transition within {max} events: {seen:?}");
}

fn to_states(changes: &[StateChanged]) -> Vec<S> {
    changes.iter().map(|c| c.to).collect()
}

/// Gate 1: crash → backoff → degraded, exact sequence with budget 3.
#[tokio::test]
async fn crash_backoff_then_degraded() {
    let mut h = harness(&["--mock-exit-after-ms=40", "--mock-exit-code=1"], |_| {});
    h.handle.send(Command::Start).await.unwrap();

    let seen = collect_until(&mut h.events, |c| c.to == S::Degraded, 16).await;
    assert_eq!(
        to_states(&seen),
        vec![
            S::Starting,
            S::Running,
            S::Backoff,
            S::Starting,
            S::Running,
            S::Backoff,
            S::Starting,
            S::Running,
            S::Degraded,
        ]
    );
    let last = seen.last().unwrap();
    assert_eq!(last.reason, Some(DegradedReason::RetryBudgetExhausted));

    h.handle.shutdown().await.unwrap();
}

/// Gate 1: a healthy window resets the budget — with a window shorter than
/// the child's lifetime, repeated crashes never reach Degraded.
#[tokio::test]
async fn healthy_window_resets_budget() {
    let mut h = harness(
        &["--mock-exit-after-ms=700", "--mock-exit-code=1"],
        |config| {
            config.backoff.budget = 2;
            config.healthy_window = Duration::from_millis(100);
        },
    );
    h.handle.send(Command::Start).await.unwrap();

    // Three full crash cycles; budget 2 would have degraded on the second
    // crash if the reset were broken.
    let seen = collect_until(
        &mut h.events,
        |c| c.to == S::Backoff,
        4, // Starting, Running, Backoff + slack
    )
    .await;
    assert!(seen.iter().all(|c| c.to != S::Degraded));
    for _ in 0..2 {
        let seen = collect_until(&mut h.events, |c| c.to == S::Backoff, 4).await;
        assert!(
            seen.iter().all(|c| c.to != S::Degraded),
            "degraded despite healthy-window resets: {seen:?}"
        );
    }

    h.handle.shutdown().await.unwrap();
}

/// Control for the reset test: same crash cadence, but the healthy window is
/// far longer than the child's lifetime, so budget 2 → Degraded on crash #2.
#[tokio::test]
async fn budget_spent_without_healthy_reset() {
    let mut h = harness(
        &["--mock-exit-after-ms=200", "--mock-exit-code=1"],
        |config| {
            config.backoff.budget = 2;
            config.healthy_window = Duration::from_secs(60);
        },
    );
    h.handle.send(Command::Start).await.unwrap();

    let seen = collect_until(&mut h.events, |c| c.to == S::Degraded, 8).await;
    assert_eq!(
        to_states(&seen),
        vec![
            S::Starting,
            S::Running,
            S::Backoff,
            S::Starting,
            S::Running,
            S::Degraded
        ]
    );

    h.handle.shutdown().await.unwrap();
}

/// A missing kanata binary is an immediate, actionable Degraded — no backoff
/// spin (SPEC §16).
#[tokio::test]
async fn missing_binary_degrades_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("test.kbd");
    std::fs::write(&cfg_path, ";; mock\n").unwrap();
    let config = SupervisorConfig::new(tmp.path().join("no-such-kanata"), cfg_path);

    let handle = supervisor::start(config);
    let mut events = handle.subscribe();
    handle.send(Command::Start).await.unwrap();

    let seen = collect_until(&mut events, |c| c.to == S::Degraded, 3).await;
    assert_eq!(to_states(&seen), vec![S::Starting, S::Degraded]);
    assert_eq!(
        seen.last().unwrap().reason,
        Some(DegradedReason::KanataBinMissing)
    );

    handle.shutdown().await.unwrap();
}

/// A config rejected by `--check` never spawns; Degraded{ConfigBroken}
/// (SPEC §6.4 [HARD]: a bad config can never take the keyboard down).
#[tokio::test]
async fn broken_config_refused_by_preflight() {
    let mut h = harness(&["--mock-fail-check"], |_| {});
    h.handle.send(Command::Start).await.unwrap();

    let seen = collect_until(&mut h.events, |c| c.to == S::Degraded, 3).await;
    assert_eq!(to_states(&seen), vec![S::Starting, S::Degraded]);
    assert_eq!(
        seen.last().unwrap().reason,
        Some(DegradedReason::ConfigBroken)
    );
    assert_eq!(h.handle.snapshot().kanata_pid, None);

    h.handle.shutdown().await.unwrap();
}

/// §6.2 user lifecycle: Running → Paused → Running → Stopped.
#[tokio::test]
async fn pause_resume_stop_cycle() {
    let mut h = harness(&[], |_| {});
    h.handle.send(Command::Start).await.unwrap();
    collect_until(&mut h.events, |c| c.to == S::Running, 3).await;
    assert!(h.handle.snapshot().kanata_pid.is_some());

    h.handle.send(Command::Pause).await.unwrap();
    collect_until(&mut h.events, |c| c.to == S::Paused, 2).await;
    assert_eq!(h.handle.snapshot().kanata_pid, None);

    h.handle.send(Command::Resume).await.unwrap();
    collect_until(&mut h.events, |c| c.to == S::Running, 3).await;

    h.handle.send(Command::Stop).await.unwrap();
    collect_until(&mut h.events, |c| c.to == S::Stopped, 2).await;
    assert_eq!(h.handle.snapshot().kanata_pid, None);

    h.handle.shutdown().await.unwrap();
}

/// Panic escape (clean unrequested exit) → Stopped, never Backoff (SPEC §16).
#[tokio::test]
async fn panic_escape_stops_without_backoff() {
    let mut h = harness(&["--mock-exit-after-ms=60", "--mock-exit-code=0"], |_| {});
    h.handle.send(Command::Start).await.unwrap();

    let seen = collect_until(&mut h.events, |c| c.to == S::Stopped, 4).await;
    assert_eq!(to_states(&seen), vec![S::Starting, S::Running, S::Stopped]);

    h.handle.shutdown().await.unwrap();
}

/// Gate 1: graceful SIGTERM against the real `kanatad run` binary — clean
/// exit 0, child reaped, final state persisted, transitions logged (§6.1 [HARD]).
#[test]
fn graceful_sigterm_persists_state_and_logs_transitions() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("test.kbd");
    std::fs::write(&cfg_path, ";; mock\n").unwrap();
    let state_dir = tmp.path().join("state");
    // Bind the control socket in the temp dir; the default path needs root.
    let socket = tmp.path().join("kanatabar.sock");

    let daemon = std::process::Command::new(env!("CARGO_BIN_EXE_kanatad"))
        .args(["run", "--cfg"])
        .arg(&cfg_path)
        .arg("--kanata-bin")
        .arg(mock_bin())
        .arg("--state-dir")
        .arg(&state_dir)
        .env("KANATABAR_SOCK", &socket)
        .env("KANATABAR_SKIP_DRIVER_CHECK", "true")
        .env("RUST_LOG", "info")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn kanatad");

    // Wait for state.json to report Running with a child pid.
    let state_path = state_dir.join("state.json");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let kanata_pid = loop {
        assert!(
            std::time::Instant::now() < deadline,
            "never reached Running"
        );
        if let Ok(body) = std::fs::read_to_string(&state_path) {
            let json: serde_json::Value = serde_json::from_str(&body).expect("valid state.json");
            if json["state"] == "Running" {
                break json["kanata_pid"].as_u64().expect("pid present") as i32;
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("SIGTERM kanatad");

    let output = daemon.wait_with_output().expect("kanatad exit");
    // launchd must see a clean exit (§6.1 [HARD]).
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Final state persisted as Stopped with no child.
    let body = std::fs::read_to_string(&state_path).expect("state.json after exit");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["state"], "Stopped");
    assert_eq!(json["kanata_pid"], serde_json::Value::Null);

    // No orphan: the mock child must be gone.
    let gone = matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(kanata_pid), None),
        Err(nix::errno::Errno::ESRCH)
    );
    assert!(
        gone,
        "mock-kanata pid {kanata_pid} still alive after daemon exit"
    );

    // Transitions logged with from/to fields (§6.2).
    let logs = String::from_utf8_lossy(&output.stderr);
    for needle in [
        "state transition",
        "from=Stopped to=Starting",
        "from=Starting to=Running",
        "to=Stopped",
    ] {
        assert!(logs.contains(needle), "missing {needle:?} in logs:\n{logs}");
    }
}

/// SPEC §2 hardening: a crash whose output carries the macOS TCC denial
/// signature (jtroo/kanata#1037 — Input Monitoring not granted, or silently
/// invalidated by a kanata update) goes straight to an actionable
/// `Degraded{InputMonitoringDenied}` — no futile backoff/respawn loop.
#[tokio::test]
async fn permission_denied_output_degrades_without_backoff() {
    let mut h = harness(
        &[
            "--mock-stderr-line",
            "IOHIDDeviceOpen error: (iokit/common) privilege violation",
            "--mock-exit-after-ms",
            "50",
            "--mock-exit-code",
            "1",
        ],
        |_| {},
    );
    h.handle.send(Command::Start).await.expect("start");

    let seen = collect_until(&mut h.events, |c| c.to == S::Degraded, 8).await;
    assert!(
        !seen.iter().any(|c| c.to == S::Backoff),
        "TCC denial must not burn the retry budget: {seen:?}"
    );
    assert_eq!(
        h.handle.snapshot().degraded_reason,
        Some(DegradedReason::InputMonitoringDenied)
    );
    // The reason's message must be actionable (SPEC §15) and name the caveat.
    let msg = DegradedReason::InputMonitoringDenied.describe();
    assert!(msg.contains("Input Monitoring"), "{msg}");
    assert!(
        msg.contains("re-add"),
        "must mention the update caveat: {msg}"
    );

    h.handle.shutdown().await.expect("shutdown");
}

/// SPEC §2 hardening: "exclusive access / device already open" in the crash
/// output (another remapper holds the keyboard — e.g. a Karabiner-Elements
/// grabber or a second kanata) degrades with the grab-conflict reason.
#[tokio::test]
async fn device_in_use_output_degrades_with_conflict_reason() {
    let mut h = harness(
        &[
            "--mock-stderr-line",
            "IOHIDDeviceOpen error: exclusive access and device already open",
            "--mock-exit-after-ms",
            "50",
            "--mock-exit-code",
            "1",
        ],
        |_| {},
    );
    h.handle.send(Command::Start).await.expect("start");

    let seen = collect_until(&mut h.events, |c| c.to == S::Degraded, 8).await;
    assert!(
        !seen.iter().any(|c| c.to == S::Backoff),
        "a grab conflict must not burn the retry budget: {seen:?}"
    );
    assert_eq!(
        h.handle.snapshot().degraded_reason,
        Some(DegradedReason::DeviceGrabConflict)
    );

    h.handle.shutdown().await.expect("shutdown");
}

/// An ordinary crash (no fault signature in the output) still takes the
/// normal crash→backoff path — the classifier must not over-trigger.
#[tokio::test]
async fn plain_crash_still_backs_off() {
    let mut h = harness(
        &["--mock-exit-after-ms", "50", "--mock-exit-code", "1"],
        |_| {},
    );
    h.handle.send(Command::Start).await.expect("start");

    let seen = collect_until(&mut h.events, |c| c.to == S::Backoff, 8).await;
    assert!(
        seen.iter().any(|c| c.to == S::Backoff),
        "plain crash should back off: {seen:?}"
    );

    h.handle.shutdown().await.expect("shutdown");
}

/// Await the next bus event, or panic on timeout.
async fn next_bus_event(bus: &mut broadcast::Receiver<Event>) -> Event {
    timeout(Duration::from_secs(10), bus.recv())
        .await
        .expect("timed out waiting for a bus event")
        .expect("bus closed")
}

/// HW finding (2026-07-13, Run 7): a kanata crash posted NO desktop
/// notification because the daemon never emitted `Event::Crash`. A plain crash
/// (no actionable fault, recovers via Backoff→Running — never Degraded) must
/// publish `Event::Crash` on the daemon bus so the tray can notify (SPEC §8).
#[tokio::test]
async fn plain_crash_publishes_crash_event_on_the_bus() {
    let mut h = harness(
        &["--mock-exit-after-ms", "50", "--mock-exit-code", "1"],
        |_| {},
    );
    h.handle.send(Command::Start).await.expect("start");

    // The first bus event on a plain crash must be Crash, carrying the real
    // exit code (mock exits with code 1, no signal).
    match next_bus_event(&mut h.bus).await {
        Event::Crash { code, signal } => {
            assert_eq!(code, Some(1), "crash carries the exit code");
            assert_eq!(signal, None, "a code exit has no signal");
        }
        other => panic!("expected Event::Crash, got {other:?}"),
    }

    h.handle.shutdown().await.expect("shutdown");
}

/// A fault-classified crash (TCC denial) goes straight to Degraded — the tray
/// notifies on that transition, so publishing `Event::Crash` too would
/// double-notify. The bus must stay silent on this path.
#[tokio::test]
async fn fault_crash_does_not_publish_crash_event() {
    let mut h = harness(
        &[
            "--mock-stderr-line",
            "IOHIDDeviceOpen error: (iokit/common) privilege violation",
            "--mock-exit-after-ms",
            "50",
            "--mock-exit-code",
            "1",
        ],
        |_| {},
    );
    h.handle.send(Command::Start).await.expect("start");

    // Drive it to the Degraded transition the tray notifies on.
    collect_until(&mut h.events, |c| c.to == S::Degraded, 8).await;

    // No Event::Crash may have been published for this fault path.
    assert!(
        matches!(h.bus.try_recv(), Err(broadcast::error::TryRecvError::Empty)),
        "a fault-classified crash must not also publish Event::Crash"
    );

    h.handle.shutdown().await.expect("shutdown");
}

/// HW finding (2026-07-11): a driver-pkg/kanata protocol mismatch (v8.0.0 with
/// kanata 1.12.0) leaves kanata ALIVE but unremapping — it releases the input
/// devices and waits for the backend forever, while status says Running and
/// every doctor check is green. The supervisor must classify the live log
/// narrative into `Degraded{OutputBackendUnavailable}` — WITHOUT killing the
/// child (kanata retries the backend itself) and without burning the budget.
#[tokio::test]
async fn backend_unavailable_degrades_but_keeps_the_child() {
    let mut h = harness(
        &[
            "--mock-stdout-script",
            "0:output backend unavailable — releasing input devices",
        ],
        |config| config.backend_grace = Duration::from_millis(100),
    );
    h.handle.send(Command::Start).await.expect("start");

    let seen = collect_until(&mut h.events, |c| c.to == S::Degraded, 4).await;
    assert_eq!(to_states(&seen), vec![S::Starting, S::Running, S::Degraded]);
    assert!(
        !seen.iter().any(|c| c.to == S::Backoff),
        "a backend outage must not burn the retry budget: {seen:?}"
    );
    let snapshot = h.handle.snapshot();
    assert_eq!(
        snapshot.degraded_reason,
        Some(DegradedReason::OutputBackendUnavailable)
    );
    // The child is deliberately kept alive — kanata self-recovers.
    assert!(
        snapshot.kanata_pid.is_some(),
        "child must stay alive through backend-degraded"
    );
    // The message must name the likely cause (SPEC §15).
    let msg = DegradedReason::OutputBackendUnavailable.describe();
    assert!(msg.contains("NOT remapped"), "{msg}");
    assert!(msg.contains("version mismatch"), "{msg}");

    h.handle.shutdown().await.expect("shutdown");
}

/// A backend blip that recovers inside the grace window (e.g. launchd reviving
/// a killed VHID daemon in ~1s) must not flap to Degraded at all.
#[tokio::test]
async fn backend_blip_within_grace_never_degrades() {
    let mut h = harness(
        &[
            "--mock-stdout-script",
            "0:output backend unavailable — releasing input devices",
            "--mock-stdout-script",
            "80:output backend and console session ready — re-grabbing input devices",
        ],
        |config| config.backend_grace = Duration::from_millis(500),
    );
    h.handle.send(Command::Start).await.expect("start");

    collect_until(&mut h.events, |c| c.to == S::Running, 3).await;
    // Well past the grace window: no further transition may have fired.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        matches!(
            h.events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ),
        "a recovered blip must not produce any transition"
    );
    assert_eq!(h.handle.snapshot().state, S::Running);

    h.handle.shutdown().await.expect("shutdown");
}

/// The full round trip: backend lost → Degraded (child kept), backend back →
/// Running again with the SAME child — no respawn, budget untouched.
#[tokio::test]
async fn backend_recovery_returns_to_running_without_respawn() {
    let mut h = harness(
        &[
            "--mock-stdout-script",
            "0:output backend unavailable — releasing input devices",
            "--mock-stdout-script",
            "400:output backend and console session ready — re-grabbing input devices",
        ],
        |config| config.backend_grace = Duration::from_millis(100),
    );
    h.handle.send(Command::Start).await.expect("start");

    collect_until(&mut h.events, |c| c.to == S::Degraded, 4).await;
    let pid_degraded = h.handle.snapshot().kanata_pid.expect("child alive");

    let seen = collect_until(&mut h.events, |c| c.to == S::Running, 2).await;
    assert_eq!(to_states(&seen), vec![S::Running]);
    assert_eq!(
        h.handle.snapshot().kanata_pid,
        Some(pid_degraded),
        "recovery must reuse the same child, not respawn"
    );

    h.handle.shutdown().await.expect("shutdown");
}

/// HW finding (2026-07-11): kanata panics with `AddrInUse` when its TCP port
/// is taken (another kanata/kanata-tray, or a leftover process). A respawn
/// cannot fix it — expect `Degraded{TcpPortConflict}` naming the cause, not
/// five futile retries into a generic "keeps crashing".
#[tokio::test]
async fn tcp_port_conflict_degrades_with_the_cause_named() {
    let mut h = harness(
        &[
            "--mock-stderr-line",
            r#"TCP server starts: Os { code: 48, kind: AddrInUse, message: "Address already in use" }"#,
            "--mock-exit-after-ms",
            "50",
            "--mock-exit-code",
            "1",
        ],
        |_| {},
    );
    h.handle.send(Command::Start).await.expect("start");

    let seen = collect_until(&mut h.events, |c| c.to == S::Degraded, 8).await;
    assert!(
        !seen.iter().any(|c| c.to == S::Backoff),
        "a port conflict must not burn the retry budget: {seen:?}"
    );
    assert_eq!(
        h.handle.snapshot().degraded_reason,
        Some(DegradedReason::TcpPortConflict)
    );
    assert!(
        DegradedReason::TcpPortConflict
            .describe()
            .contains("tcp_port"),
        "the message must point at the config knob"
    );

    h.handle.shutdown().await.expect("shutdown");
}
