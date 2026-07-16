//! Gate-2 integration tests (SPEC §19): the control server driven in-process
//! through the real `kanatactl` client — Hello/status/lifecycle round-trips,
//! Subscribe event delivery, and a simulated wrong-uid rejection. No root,
//! devices, or real kanata (SPEC §17).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanatabar_core::ipc::{RequestPayload, ResponsePayload};
use kanatabar_core::state::SupervisorState as S;
use kanatactl::Client;
use kanatad::config::SupervisorConfig;
use kanatad::configmgr::{ConfigManager, ConfigPaths};
use kanatad::control::{self, auth::AuthPolicy, ControlConfig};
use kanatad::supervisor::{self, SupervisorHandle};
use tokio::task::JoinHandle;
use tokio::time::sleep;

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

struct Harness {
    socket: PathBuf,
    handle: SupervisorHandle,
    health: kanatad::health::HealthState,
    logs: kanatad::logbuf::LogBuffer,
    devices: kanatad::device::DeviceRegistry,
    control: JoinHandle<std::io::Result<()>>,
    _tmp: tempfile::TempDir,
}

impl Harness {
    async fn start(auth: AuthPolicy) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("test.kbd");
        std::fs::write(&cfg, ";; mock\n").expect("write cfg");
        let socket = tmp.path().join("kanatabar.sock");

        let mut config = SupervisorConfig::new(mock_bin(), cfg);
        config.state_dir = Some(tmp.path().join("state"));
        let preflight_timeout = config.preflight_timeout;

        let handle = supervisor::start(config);
        let bus = kanatad::events::DaemonEvents::default();
        let configmgr = ConfigManager::load(
            handle.client(),
            handle.active_config(),
            ConfigPaths::under(tmp.path()),
            mock_bin(),
            preflight_timeout,
            bus.clone(),
        );
        let health = kanatad::health::HealthState::default();
        let logs = kanatad::logbuf::LogBuffer::default();
        let devices = kanatad::device::DeviceRegistry::default();
        let control = tokio::spawn(control::serve(
            ControlConfig {
                socket_path: socket.clone(),
                socket_gid: None,
            },
            handle.client(),
            configmgr,
            health.clone(),
            auth,
            Instant::now(),
            None, // no driver probe in tests (SPEC §6.5)
            logs.clone(),
            devices.clone(),
            bus,
        ));

        wait_for_socket(&socket).await;
        Self {
            socket,
            handle,
            health,
            logs,
            devices,
            control,
            _tmp: tmp,
        }
    }

    async fn stop(self) {
        self.control.abort();
        self.handle.shutdown().await.expect("clean shutdown");
    }
}

fn allow_all() -> AuthPolicy {
    Arc::new(|_uid| true)
}

async fn wait_for_socket(path: &Path) {
    for _ in 0..400 {
        if path.exists() {
            return;
        }
        sleep(Duration::from_millis(5)).await;
    }
    panic!("control socket never appeared at {}", path.display());
}

/// Connect, retrying until the accept loop is ready.
async fn connect(path: &Path) -> Client {
    for _ in 0..400 {
        if let Ok(client) = Client::connect(path).await {
            return client;
        }
        sleep(Duration::from_millis(5)).await;
    }
    panic!("could not connect to {}", path.display());
}

async fn status(client: &mut Client) -> kanatabar_core::ipc::Status {
    match client
        .request(RequestPayload::GetStatus)
        .await
        .unwrap()
        .payload
    {
        ResponsePayload::Status(status) => status,
        other => panic!("expected Status, got {other:?}"),
    }
}

async fn ack(client: &mut Client, payload: RequestPayload) {
    match client.request(payload).await.unwrap().payload {
        ResponsePayload::Ack => {}
        other => panic!("expected Ack, got {other:?}"),
    }
}

async fn wait_until_state(client: &mut Client, want: S) {
    for _ in 0..400 {
        if status(client).await.state == want {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("state never became {want:?}");
}

/// Hello handshake + a status round-trip on a fresh daemon.
#[tokio::test]
async fn hello_then_status_round_trips() {
    let h = Harness::start(allow_all()).await;
    let mut client = connect(&h.socket).await;

    let status = status(&mut client).await;
    assert_eq!(status.state, S::Stopped);
    assert_eq!(status.kanata_pid, None);
    assert!(!status.daemon_version.is_empty());

    h.stop().await;
}

/// Lifecycle commands are acked and reflected in status (start → running →
/// stopped), driving the real supervisor + mock-kanata.
#[tokio::test]
async fn lifecycle_commands_round_trip() {
    let h = Harness::start(allow_all()).await;
    let mut client = connect(&h.socket).await;

    ack(&mut client, RequestPayload::Start).await;
    wait_until_state(&mut client, S::Running).await;
    assert!(status(&mut client).await.kanata_pid.is_some());

    ack(&mut client, RequestPayload::Pause).await;
    wait_until_state(&mut client, S::Paused).await;

    ack(&mut client, RequestPayload::Resume).await;
    wait_until_state(&mut client, S::Running).await;

    ack(&mut client, RequestPayload::Stop).await;
    wait_until_state(&mut client, S::Stopped).await;
    assert_eq!(status(&mut client).await.kanata_pid, None);

    h.stop().await;
}

/// A subscribed connection receives state-change events driven by another
/// connection's commands (SPEC §7.2 `Event{StateChanged}`).
#[tokio::test]
async fn subscribe_receives_state_changes() {
    let h = Harness::start(allow_all()).await;
    let mut watcher = connect(&h.socket).await;
    let mut commander = connect(&h.socket).await;

    ack(&mut watcher, RequestPayload::Subscribe).await;
    ack(&mut commander, RequestPayload::Start).await;

    // Expect the Stopped→Starting→Running progression.
    let mut seen = Vec::new();
    for _ in 0..3 {
        match tokio::time::timeout(Duration::from_secs(10), watcher.next_event())
            .await
            .expect("event timeout")
            .expect("event stream")
        {
            kanatabar_core::ipc::Event::StateChanged { to, .. } => {
                seen.push(to);
                if to == S::Running {
                    break;
                }
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(seen.contains(&S::Starting), "missing Starting in {seen:?}");
    assert!(seen.contains(&S::Running), "missing Running in {seen:?}");

    h.stop().await;
}

/// A subscribed connection receives `Event::LayerChanged` when the TCP relay
/// reports a new layer (SPEC §7.2) — pushed live, not only via `GetStatus`.
#[tokio::test]
async fn subscribe_receives_layer_changes() {
    let h = Harness::start(allow_all()).await;
    let mut watcher = connect(&h.socket).await;

    ack(&mut watcher, RequestPayload::Subscribe).await;

    // Drive the shared health state the way the relay does (health::tcp).
    h.health.set_active_layer(Some("nav".to_string()));
    // A repeat of the same layer must not produce a second event…
    h.health.set_active_layer(Some("nav".to_string()));
    // …but a genuine change must.
    h.health.set_active_layer(Some("base".to_string()));

    let mut layers = Vec::new();
    for _ in 0..2 {
        match tokio::time::timeout(Duration::from_secs(10), watcher.next_event())
            .await
            .expect("event timeout")
            .expect("event stream")
        {
            kanatabar_core::ipc::Event::LayerChanged { layer } => layers.push(layer),
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(layers, vec!["nav".to_string(), "base".to_string()]);

    h.stop().await;
}

/// A peer the policy rejects cannot complete the handshake — the server drops
/// the connection before any exchange (SPEC §7.1). The wrong-uid case is
/// simulated by injecting a deny-all policy, since the test runs as one uid.
#[tokio::test]
async fn unauthorized_peer_is_rejected() {
    let deny_all: AuthPolicy = Arc::new(|_uid| false);
    let h = Harness::start(deny_all).await;

    // The socket exists, but the handshake must fail (connection closed).
    let result = Client::connect(&h.socket).await;
    assert!(
        result.is_err(),
        "deny-all policy must reject the connection"
    );

    h.stop().await;
}

/// Sending a request before Hello is refused with InvalidRequest.
#[tokio::test]
async fn request_before_hello_is_invalid() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let h = Harness::start(allow_all()).await;

    let stream = UnixStream::connect(&h.socket).await.unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    write_half
        .write_all(b"{\"id\":1,\"type\":\"GetStatus\"}\n")
        .await
        .unwrap();
    write_half.flush().await.unwrap();

    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["type"], "Error");
    assert_eq!(value["kind"], "InvalidRequest");

    h.stop().await;
}

/// `GetLogs` answers with N `LogLine` frames then a terminating `Ack`
/// (SPEC §6.6, §7.2), newest lines last.
#[tokio::test]
async fn get_logs_streams_buffered_lines_then_acks() {
    let h = Harness::start(allow_all()).await;
    for i in 0..5 {
        h.logs.push(format!("buffered line {i}"));
    }
    let mut client = connect(&h.socket).await;

    let id = client
        .send_request(RequestPayload::GetLogs { lines: 3 })
        .await
        .expect("send GetLogs");
    let mut lines = Vec::new();
    loop {
        let response = tokio::time::timeout(Duration::from_secs(5), client.next_response())
            .await
            .expect("GetLogs timeout")
            .expect("frame");
        assert_eq!(response.id, Some(id), "frames echo the request id");
        match response.payload {
            ResponsePayload::LogLine { line } => lines.push(line),
            ResponsePayload::Ack => break,
            other => panic!("unexpected frame {other:?}"),
        }
    }
    // The newest 3 of 5, oldest first.
    assert_eq!(
        lines,
        vec!["buffered line 2", "buffered line 3", "buffered line 4"]
    );

    h.stop().await;
}

/// `FollowLogs` acks, then streams lines pushed after the subscription
/// (SPEC §6.6 `--follow`).
#[tokio::test]
async fn follow_logs_streams_new_lines() {
    let h = Harness::start(allow_all()).await;
    let mut client = connect(&h.socket).await;

    ack(&mut client, RequestPayload::FollowLogs).await;
    h.logs.push("live line".to_string());

    let response = tokio::time::timeout(Duration::from_secs(5), client.next_response())
        .await
        .expect("follow timeout")
        .expect("frame");
    match response.payload {
        ResponsePayload::LogLine { line } => assert_eq!(line, "live line"),
        other => panic!("unexpected frame {other:?}"),
    }

    h.stop().await;
}

/// `GetDevices` reports the registry's live list (SPEC §7.2).
#[tokio::test]
async fn get_devices_reports_the_registry() {
    use kanatabar_core::device::{DeviceChange, DeviceDescriptor};
    let h = Harness::start(allow_all()).await;
    h.devices.apply(
        DeviceChange::Added,
        &DeviceDescriptor {
            name: "Test KB".into(),
            vendor_id: Some(0x1234),
            usage_page: Some(0x01),
            usage: Some(0x06),
        },
    );
    let mut client = connect(&h.socket).await;

    let response = client
        .request(RequestPayload::GetDevices)
        .await
        .expect("GetDevices");
    match response.payload {
        ResponsePayload::Devices { devices } => {
            assert_eq!(devices.len(), 1);
            assert_eq!(devices[0].name, "Test KB");
            assert!(devices[0].matched);
        }
        other => panic!("unexpected reply {other:?}"),
    }

    h.stop().await;
}

/// A committed apply pushes `Event::ConfigApplied` to subscribers (SPEC §7.2),
/// and `SetAutostart` then persists the flag for the active selection — or is
/// refused with an actionable error when nothing is active.
#[tokio::test]
async fn apply_pushes_config_applied_and_autostart_needs_a_preset() {
    let h = Harness::start(allow_all()).await;
    let mut watcher = connect(&h.socket).await;
    let mut commander = connect(&h.socket).await;

    ack(&mut watcher, RequestPayload::Subscribe).await;

    // SetAutostart with no active preset → InvalidRequest, not a panic.
    let response = commander
        .request(RequestPayload::SetAutostart { enabled: true })
        .await
        .expect("SetAutostart");
    match response.payload {
        ResponsePayload::Error { kind, message } => {
            assert_eq!(kind, kanatabar_core::ipc::ErrorKind::InvalidRequest);
            assert!(message.contains("preset"), "{message}");
        }
        other => panic!("expected an error, got {other:?}"),
    }

    // Apply a fresh path-safe config; the watcher sees ConfigApplied.
    let new_cfg = h._tmp.path().join("applied.kbd");
    std::fs::write(&new_cfg, ";; new\n").expect("write cfg");
    ack(
        &mut commander,
        RequestPayload::ApplyConfig {
            path: new_cfg.display().to_string(),
        },
    )
    .await;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no ConfigApplied event"
        );
        match tokio::time::timeout(Duration::from_secs(10), watcher.next_event())
            .await
            .expect("event timeout")
            .expect("event stream")
        {
            kanatabar_core::ipc::Event::ConfigApplied { preset, path } => {
                assert_eq!(preset, None);
                assert!(path.ends_with("applied.kbd"), "{path}");
                break;
            }
            _other => continue, // interleaved StateChanged from the restart
        }
    }

    h.stop().await;
}
