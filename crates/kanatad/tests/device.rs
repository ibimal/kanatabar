//! Gate-4 [AUTO] integration tests (SPEC §19): the device-monitor pipeline
//! driven by fake events through the real debounce loop + supervisor +
//! mock-kanata — a hotplug storm collapses to a single re-sync restart, spaced
//! changes restart individually, and non-keyboard/virtual changes are ignored.
//! No IOKit, root, devices, or real kanata (SPEC §17).

use std::path::PathBuf;
use std::time::Duration;

use kanatabar_core::device::{DeviceChange, DeviceDescriptor};
use kanatabar_core::machine::StateChanged;
use kanatabar_core::state::SupervisorState as S;
use kanatad::config::SupervisorConfig;
use kanatad::device::{self, DeviceEvent};
use kanatad::supervisor::{self, Command, SupervisorHandle};
use tokio::sync::{broadcast, mpsc};
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

const WINDOW: Duration = Duration::from_millis(150);

struct Harness {
    handle: SupervisorHandle,
    device_tx: mpsc::UnboundedSender<DeviceEvent>,
    device_task: tokio::task::JoinHandle<()>,
    registry: device::DeviceRegistry,
    bus: kanatad::events::DaemonEvents,
    _tmp: tempfile::TempDir,
}

impl Harness {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("test.kbd");
        std::fs::write(&cfg, ";; mock\n").unwrap();
        let mut config = SupervisorConfig::new(mock_bin(), cfg);
        config.state_dir = Some(tmp.path().join("state"));
        config.kill_grace = Duration::from_secs(2);

        let handle = supervisor::start(config);
        let (device_tx, device_rx) = mpsc::unbounded_channel();
        let registry = device::DeviceRegistry::default();
        let bus = kanatad::events::DaemonEvents::default();
        let device_task = tokio::spawn(device::run(
            device_rx,
            handle.client(),
            WINDOW,
            registry.clone(),
            bus.clone(),
        ));

        // Bring kanata up so re-sync restarts have something to restart.
        handle.send(Command::Start).await.unwrap();
        for _ in 0..400 {
            if handle.snapshot().state == S::Running {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(handle.snapshot().state, S::Running);

        Self {
            handle,
            device_tx,
            device_task,
            registry,
            bus,
            _tmp: tmp,
        }
    }

    fn send(&self, change: DeviceChange, descriptor: DeviceDescriptor) {
        self.device_tx
            .send(DeviceEvent {
                change,
                descriptor,
                initial: false,
            })
            .expect("device pipeline alive");
    }

    fn send_initial(&self, descriptor: DeviceDescriptor) {
        self.device_tx
            .send(DeviceEvent {
                change: DeviceChange::Added,
                descriptor,
                initial: true,
            })
            .expect("device pipeline alive");
    }

    async fn stop(self) {
        self.device_task.abort();
        self.handle.shutdown().await.unwrap();
    }
}

fn keyboard(name: &str) -> DeviceDescriptor {
    DeviceDescriptor {
        name: name.into(),
        vendor_id: Some(0x1234),
        usage_page: Some(0x01),
        usage: Some(0x06),
    }
}

/// Count `to == Starting` transitions received on `sub` over `settle`, which is
/// one per re-sync restart (from Running).
async fn count_restarts(sub: &mut broadcast::Receiver<StateChanged>, settle: Duration) -> usize {
    sleep(settle).await;
    let mut restarts = 0;
    while let Ok(change) = sub.try_recv() {
        if change.to == S::Starting {
            restarts += 1;
        }
    }
    restarts
}

/// A hotplug storm (dock connect) coalesces into exactly one restart (SPEC §16).
#[tokio::test]
async fn hotplug_storm_collapses_to_one_restart() {
    let h = Harness::start().await;
    let mut sub = h.handle.subscribe();

    // A burst of many keyboard add/remove events well within one window.
    for i in 0..12 {
        let change = if i % 2 == 0 {
            DeviceChange::Added
        } else {
            DeviceChange::Removed
        };
        h.send(change, keyboard(&format!("KB {i}")));
        sleep(Duration::from_millis(5)).await;
    }

    let restarts = count_restarts(&mut sub, WINDOW * 3).await;
    assert_eq!(
        restarts, 1,
        "storm must produce exactly one re-sync restart"
    );

    h.stop().await;
}

/// Changes spaced beyond the window restart individually.
#[tokio::test]
async fn spaced_changes_restart_each() {
    let h = Harness::start().await;
    let mut sub = h.handle.subscribe();

    for i in 0..3 {
        h.send(DeviceChange::Added, keyboard(&format!("KB {i}")));
        // Wait out the window (plus slack for the restart) between events.
        sleep(WINDOW + Duration::from_millis(250)).await;
    }

    let restarts = count_restarts(&mut sub, WINDOW * 2).await;
    assert_eq!(restarts, 3, "three well-separated changes → three restarts");

    h.stop().await;
}

/// Non-keyboard and Karabiner-virtual changes are ignored — no restart (§6.3).
#[tokio::test]
async fn irrelevant_changes_do_not_restart() {
    let h = Harness::start().await;
    let mut sub = h.handle.subscribe();

    // A mouse (usage 2).
    h.send(
        DeviceChange::Added,
        DeviceDescriptor {
            name: "Logitech Mouse".into(),
            vendor_id: Some(0x046D),
            usage_page: Some(0x01),
            usage: Some(0x02),
        },
    );
    // The Karabiner virtual keyboard (kanata's own output).
    h.send(
        DeviceChange::Added,
        DeviceDescriptor {
            name: "Karabiner DriverKit VirtualHIDKeyboard".into(),
            vendor_id: Some(0x16C0),
            usage_page: Some(0x01),
            usage: Some(0x06),
        },
    );

    let restarts = count_restarts(&mut sub, WINDOW * 3).await;
    assert_eq!(
        restarts, 0,
        "irrelevant device changes must not restart kanata"
    );

    h.stop().await;
}

/// A hotplug while kanata is stopped does not start it (re-sync is safety net,
/// not an autostart).
#[tokio::test]
async fn no_restart_when_not_running() {
    let h = Harness::start().await;
    h.handle.send(Command::Stop).await.unwrap();
    for _ in 0..200 {
        if h.handle.snapshot().state == S::Stopped {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    let mut sub = h.handle.subscribe();

    h.send(DeviceChange::Added, keyboard("Late KB"));
    let restarts = count_restarts(&mut sub, WINDOW * 3).await;
    assert_eq!(restarts, 0, "a hotplug must not start a stopped kanata");
    assert_eq!(h.handle.snapshot().state, S::Stopped);

    h.stop().await;
}

/// Initial-enumeration events populate the GetDevices registry but are not
/// hotplugs: no debounce, no restart, no DeviceChanged push (SPEC §6.3, §7.2).
#[tokio::test]
async fn initial_devices_fill_registry_without_restarting() {
    let h = Harness::start().await;
    let mut sub = h.handle.subscribe();
    let mut bus = h.bus.subscribe();

    h.send_initial(keyboard("Built-in KB"));
    h.send_initial(DeviceDescriptor {
        name: "Karabiner DriverKit VirtualHIDKeyboard".into(),
        vendor_id: Some(0x16C0),
        usage_page: Some(0x01),
        usage: Some(0x06),
    });

    let restarts = count_restarts(&mut sub, WINDOW * 3).await;
    assert_eq!(restarts, 0, "initial devices are not hotplugs");
    assert!(bus.try_recv().is_err(), "initial devices are not pushed");

    let devices = h.registry.snapshot();
    assert_eq!(devices.len(), 2, "{devices:?}");
    let builtin = devices.iter().find(|d| d.name == "Built-in KB").unwrap();
    assert!(builtin.matched, "a real keyboard is matched");
    let virt = devices
        .iter()
        .find(|d| d.name.contains("VirtualHID"))
        .unwrap();
    assert!(!virt.matched, "the Karabiner virtual device is not matched");

    h.stop().await;
}

/// Composite devices present several HID nodes under one product name (HW
/// Run 6: Keychron K3 Pro = 3 nodes; HW Run 10 finding: "Apple Internal
/// Keyboard / Trackpad" showed unmatched). The registry must classify the
/// device as matched when ANY node is a keyboard — in either enumeration
/// order, and still as one row.
#[test]
fn composite_device_is_matched_whichever_node_enumerates_last() {
    let node = |page, usage| DeviceDescriptor {
        name: "Apple Internal Keyboard / Trackpad".into(),
        vendor_id: Some(0x05AC),
        usage_page: Some(page),
        usage: Some(usage),
    };
    let keyboard_node = node(0x01, 0x06);
    let trackpad_node = node(0x01, 0x05); // pointer: not resync-relevant
    let vendor_node = DeviceDescriptor {
        usage_page: None,
        usage: None,
        ..keyboard_node.clone()
    };

    for order in [
        [&keyboard_node, &trackpad_node, &vendor_node],
        [&trackpad_node, &vendor_node, &keyboard_node],
        [&vendor_node, &keyboard_node, &trackpad_node],
    ] {
        let registry = device::DeviceRegistry::default();
        for descriptor in order {
            registry.apply(DeviceChange::Added, descriptor);
        }
        let devices = registry.snapshot();
        assert_eq!(devices.len(), 1, "one row per physical device");
        assert!(
            devices[0].matched,
            "keyboard node must win regardless of order: {devices:?}"
        );
    }

    // A device with no keyboard node anywhere stays unmatched.
    let registry = device::DeviceRegistry::default();
    registry.apply(DeviceChange::Added, &trackpad_node);
    registry.apply(DeviceChange::Added, &vendor_node);
    assert!(!registry.snapshot()[0].matched);
}

/// A relevant hotplug updates the registry AND pushes Event::DeviceChanged to
/// the bus (SPEC §7.2); removal takes it out of the registry again.
#[tokio::test]
async fn hotplug_updates_registry_and_pushes_device_changed() {
    let h = Harness::start().await;
    let mut bus = h.bus.subscribe();

    h.send(DeviceChange::Added, keyboard("USB KB"));
    let event = tokio::time::timeout(Duration::from_secs(5), bus.recv())
        .await
        .expect("DeviceChanged timeout")
        .expect("bus open");
    assert_eq!(
        event,
        kanatabar_core::ipc::Event::DeviceChanged {
            added: true,
            name: "USB KB".to_string(),
        }
    );
    assert!(h.registry.snapshot().iter().any(|d| d.name == "USB KB"));

    h.send(DeviceChange::Removed, keyboard("USB KB"));
    let event = tokio::time::timeout(Duration::from_secs(5), bus.recv())
        .await
        .expect("DeviceChanged timeout")
        .expect("bus open");
    assert_eq!(
        event,
        kanatabar_core::ipc::Event::DeviceChanged {
            added: false,
            name: "USB KB".to_string(),
        }
    );
    assert!(!h.registry.snapshot().iter().any(|d| d.name == "USB KB"));

    h.stop().await;
}
