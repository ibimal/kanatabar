//! `#[ignore]`d smoke test for the real IOKit device monitor (HW, SPEC §6.3,
//! §17). The [AUTO] gate exercises the debounce pipeline with fake events; this
//! arms the *actual* `ffi::iokit` source on the running Mac to confirm it arms
//! and enumerates — the thing HW finding #1 (2026-07-11) showed the original
//! `IOServiceAddMatchingNotification` wiring could not do even as root
//! (`kIOReturnUnsupported`, because `kIOMatchedNotification` /
//! `kIOTerminatedNotification` are unsupported for IOHIDDevice; fixed 2026-07-12
//! by `kIOFirstMatchNotification` + per-device general-interest removal).
//!
//! Not part of CI (needs real hardware/registry). Run explicitly:
//!   cargo test -p kanatad --test iokit_smoke -- --ignored --nocapture

use std::time::Duration;

use kanatad::device::{IoKitMonitor, Monitor};
use tokio::sync::mpsc;

/// Arm the real monitor and assert it reports at least one already-present
/// device as an `initial` event within a short window. A pass proves the
/// notification armed (finding #1's fix) and the registry enumeration works; a
/// failure to arm would surface as an `Err` from `start`.
#[tokio::test]
#[ignore = "requires real hardware / IOKit registry"]
async fn iokit_monitor_arms_and_enumerates() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _handle = Box::new(IoKitMonitor)
        .start(tx)
        .expect("IOKit monitor must arm (finding #1: kIOReturnUnsupported must be gone)");

    let mut initial = 0usize;
    let mut sample = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        if event.initial {
            initial += 1;
            if sample.len() < 12 {
                sample.push(format!(
                    "{} (vendor={:?}, usage_page={:?}, usage={:?})",
                    event.descriptor.name,
                    event.descriptor.vendor_id,
                    event.descriptor.usage_page,
                    event.descriptor.usage
                ));
            }
        }
    }

    eprintln!("initial devices enumerated: {initial}");
    for line in &sample {
        eprintln!("  - {line}");
    }
    assert!(
        initial > 0,
        "expected at least one present HID device in the initial enumeration; \
         got none (monitor armed but enumerated nothing?)"
    );
    // Dropping `_handle` stops the CFRunLoop thread and joins it.
}
