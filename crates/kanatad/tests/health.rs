//! Gate-5 [AUTO] integration tests (SPEC §19): the driver preflight wiring
//! (injected probe → Degraded, never a crash loop), the orphan sweep, and the
//! kanata TCP layer relay. The pure `systemextensionsctl` parser is unit-tested
//! in `kanatabar_core::driver`. No root, real driver, or real kanata (§17).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kanatabar_core::machine::StateChanged;
use kanatabar_core::state::{DegradedReason, SupervisorState as S};
use kanatad::config::SupervisorConfig;
use kanatad::health::driver::{DriverHealth, DriverProbe};
use kanatad::health::{orphan, tcp, HealthState};
use kanatad::supervisor::{self, Command};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

fn target_dir() -> PathBuf {
    let mut dir = std::env::current_exe().expect("test exe path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir
}

fn mock_bin() -> PathBuf {
    let bin = target_dir().join("mock-kanata");
    assert!(
        bin.exists(),
        "run `cargo build --workspace` first: {}",
        bin.display()
    );
    bin
}

fn probe(result: DriverHealth) -> DriverProbe {
    Arc::new(move || Box::pin(async move { result }))
}

async fn collect_until(
    events: &mut broadcast::Receiver<StateChanged>,
    stop: impl Fn(&StateChanged) -> bool,
    max: usize,
) -> Vec<StateChanged> {
    let mut seen = Vec::new();
    while seen.len() < max {
        let change = timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("timed out")
            .expect("event stream");
        let done = stop(&change);
        seen.push(change);
        if done {
            return seen;
        }
    }
    panic!("no matching transition in {max}: {seen:?}");
}

/// [HARD] With the driver not activated, the supervisor goes straight to
/// Degraded{DriverNotActivated} — no spawn, no crash loop (SPEC §6.5).
/// Gate-defining (the probe wiring; the parser is unit-tested in core).
#[tokio::test]
async fn driver_not_activated_degrades_without_spawn() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("t.kbd");
    std::fs::write(&cfg, ";; mock\n").unwrap();
    let mut config = SupervisorConfig::new(mock_bin(), cfg);
    config.driver_probe = Some(probe(DriverHealth::DriverNotActivated));

    let handle = supervisor::start(config);
    let mut events = handle.subscribe();
    handle.send(Command::Start).await.unwrap();

    let seen = collect_until(&mut events, |c| c.to == S::Degraded, 3).await;
    assert_eq!(
        seen.iter().map(|c| c.to).collect::<Vec<_>>(),
        vec![S::Starting, S::Degraded]
    );
    assert_eq!(
        seen.last().unwrap().reason,
        Some(DegradedReason::DriverNotActivated)
    );
    assert_eq!(handle.snapshot().kanata_pid, None);

    handle.shutdown().await.unwrap();
}

/// A down VHID daemon degrades with the matching reason (SPEC §6.5).
#[tokio::test]
async fn vhid_daemon_down_degrades() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("t.kbd");
    std::fs::write(&cfg, ";; mock\n").unwrap();
    let mut config = SupervisorConfig::new(mock_bin(), cfg);
    config.driver_probe = Some(probe(DriverHealth::VhidDaemonDown));

    let handle = supervisor::start(config);
    let mut events = handle.subscribe();
    handle.send(Command::Start).await.unwrap();

    let seen = collect_until(&mut events, |c| c.to == S::Degraded, 3).await;
    assert_eq!(
        seen.last().unwrap().reason,
        Some(DegradedReason::VhidDaemonDown)
    );

    handle.shutdown().await.unwrap();
}

/// A healthy probe lets kanata start; health.driver_ok is set true (SPEC §6.5).
#[tokio::test]
async fn healthy_driver_allows_start() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("t.kbd");
    std::fs::write(&cfg, ";; mock\n").unwrap();
    let health = HealthState::default();
    let mut config = SupervisorConfig::new(mock_bin(), cfg);
    config.driver_probe = Some(probe(DriverHealth::Ok));
    config.health = health.clone();

    let handle = supervisor::start(config);
    let mut events = handle.subscribe();
    handle.send(Command::Start).await.unwrap();

    collect_until(&mut events, |c| c.to == S::Running, 3).await;
    assert_eq!(health.snapshot().driver_ok, Some(true));
    // The mock reports a version via clap's --version.
    assert!(health.snapshot().kanata_version.is_some());

    handle.shutdown().await.unwrap();
}

/// The orphan sweep kills a live kanata recorded from a previous daemon
/// (SPEC §6.1b), and leaves an unrelated process alone.
#[tokio::test]
async fn orphan_sweep_kills_recorded_kanata() {
    // Spawn a "leftover" mock-kanata directly (as if from a previous daemon).
    let mut orphan_proc = std::process::Command::new(mock_bin())
        .arg("--cfg")
        .arg("/tmp/none.kbd")
        .spawn()
        .unwrap();
    let pid = orphan_proc.id();
    assert!(orphan::is_alive(pid));

    orphan::sweep(Some(pid), &mock_bin(), Duration::from_secs(3)).await;

    // Reap and confirm it was terminated.
    let _ = orphan_proc.wait();
    assert!(!orphan::is_alive(pid), "orphan should be gone after sweep");

    // A pid whose executable differs from kanata is left alone: our own test
    // process is alive and is not mock-kanata.
    let self_pid = std::process::id();
    orphan::sweep(Some(self_pid), &mock_bin(), Duration::from_millis(200)).await;
    assert!(
        orphan::is_alive(self_pid),
        "sweep must not touch non-kanata pids"
    );
}

/// The TCP relay reflects kanata layer events into HealthState (SPEC §6.5).
#[tokio::test]
async fn tcp_relay_reflects_layer_changes() {
    // A free-ish high port for the mock's fake TCP server.
    let port = 53899;
    let mut kanata = std::process::Command::new(mock_bin())
        .arg("--cfg")
        .arg("/tmp/none.kbd")
        .arg("--port")
        .arg(port.to_string())
        .arg("--mock-layer")
        .arg("nav")
        .spawn()
        .unwrap();

    let health = HealthState::default();
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));

    // Retry connect while the mock's listener comes up; relay updates health as
    // lines arrive. The mock holds the connection open, so bound the read.
    let mut connected = false;
    for _ in 0..50 {
        if let Ok(stream) = tokio::net::TcpStream::connect(addr).await {
            connected = true;
            let _ = timeout(
                Duration::from_millis(400),
                tcp::read_layers(stream, &health),
            )
            .await;
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert!(connected, "could not connect to mock kanata TCP server");
    assert_eq!(health.snapshot().active_layer, Some("nav".to_string()));

    let _ = kanata.kill();
    let _ = kanata.wait();
}

/// HW finding (2026-07-11): with a *foreign* server squatting kanata's TCP
/// port, the relay stayed connected and kept reporting its layer while kanata
/// was dead (`layer: base` alongside `Degraded`). The relay must drop the
/// connection — and clear the layer — as soon as kanata leaves `Running`.
#[tokio::test]
async fn relay_disconnects_and_clears_layer_when_kanata_stops() {
    // A foreign layer server that never closes its connection.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = stream
                    .write_all(b"{\"LayerChange\":{\"new\":\"squatter\"}}\n")
                    .await;
                // Hold the connection open indefinitely.
                loop {
                    sleep(Duration::from_secs(3600)).await;
                }
            });
        }
    });

    // A long-running mock child so the supervisor sits in Running.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("test.kbd");
    std::fs::write(&cfg, ";; mock\n").unwrap();
    let config = kanatad::config::SupervisorConfig::new(mock_bin(), cfg);
    let handle = supervisor::start(config);
    handle.send(Command::Start).await.unwrap();
    for _ in 0..400 {
        if handle.snapshot().state == S::Running {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    let health = HealthState::default();
    let relay = tokio::spawn(tcp::run(port, handle.client(), health.clone()));

    // The relay connects to the foreign server and picks up its layer.
    let mut got_layer = false;
    for _ in 0..400 {
        if health.snapshot().active_layer.as_deref() == Some("squatter") {
            got_layer = true;
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert!(got_layer, "relay never picked up the foreign layer");

    // Stop kanata: the server stays alive, but the relay must let go.
    handle.send(Command::Stop).await.unwrap();
    let mut cleared = false;
    for _ in 0..400 {
        if health.snapshot().active_layer.is_none() {
            cleared = true;
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert!(
        cleared,
        "layer still set after Stop: {:?}",
        health.snapshot().active_layer
    );

    relay.abort();
    server.abort();
    handle.shutdown().await.unwrap();
}
