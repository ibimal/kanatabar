//! IOKit device-notification source on a dedicated CFRunLoop thread
//! (SPEC §3.1 [HARD], §6.3).
//!
//! Registers `IOServiceAddMatchingNotification` for `IOHIDDevice` **first-match**
//! (arrival) events and, per matched device, an
//! `IOServiceAddInterestNotification(kIOGeneralInterest)` that fires on that
//! device's **termination** (removal). Everything runs on a CFRunLoop on its own
//! OS thread and reads each device's presence facts from the registry (no key
//! events, **no Input Monitoring** — §6.3 [HARD]), forwarding them to the async
//! pipeline over an `mpsc` channel. Keyboard/Karabiner filtering is downstream.
//!
//! **HW finding #1 (2026-07-11) and its fix (2026-07-12).** SPEC §6.3 named
//! `IOServiceAddMatchingNotification` with add + terminate notifications. On this
//! macOS two of those constants are **unsupported for `IOHIDDevice` matching**
//! and return `kIOReturnUnsupported` (0xe00002c7): `kIOMatchedNotification`
//! (`"IOServiceMatched"`, the original add path) *and* `kIOTerminatedNotification`
//! (`"IOServiceTerminated"`, the removal path) — isolated with the diagnostic in
//! `tests/iokit_smoke.rs`. `kIOFirstMatchNotification` (`"IOServiceFirstMatch"`)
//! works. Removal is therefore tracked the canonical way — a general-interest
//! notification per device (Apple's `USBPrivateDataSample` pattern) — which
//! needs no Input Monitoring, preserving the §6.3 [HARD] invariant. (The
//! `IOHIDManager` alternative was rejected: `IOHIDManagerOpen` returns
//! `kIOReturnNotPermitted` without Input Monitoring, coupling detection to a
//! grant it must not need.)
//!
//! [HW] The notification wiring cannot be exercised without real hardware; the
//! [AUTO] gate covers the debounce pipeline with fake events, and an `#[ignore]`d
//! smoke test (`tests/iokit_smoke.rs`) arms the real source on demand. See
//! `docs/HW-TESTS.md`.

use std::ffi::{c_char, c_void, CStr};
use std::io;
use std::thread;

use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::{
    kCFAllocatorDefault, CFAllocatorRef, CFGetTypeID, CFRelease, CFTypeRef,
};
use core_foundation_sys::dictionary::{CFDictionaryRef, CFMutableDictionaryRef};
use core_foundation_sys::number::CFNumberRef;
use core_foundation_sys::runloop::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRef, CFRunLoopRun,
    CFRunLoopSourceRef, CFRunLoopStop,
};
use core_foundation_sys::string::CFStringRef;
use kanatabar_core::device::{DeviceChange, DeviceDescriptor};
use tokio::sync::mpsc;
use tracing::debug;

use crate::device::{DeviceEvent, MonitorHandle};

// IOKit scalar types (all Mach ports are 32-bit) and opaque handles.
type MachPortT = u32;
type IoObjectT = u32;
type IoIteratorT = u32;
type KernReturnT = i32;
type IoNotificationPortRef = *mut c_void;
type IoServiceMatchingCallback = extern "C" fn(*mut c_void, IoIteratorT);
/// `IOServiceInterestCallback`: `(refcon, service, messageType, messageArgument)`.
type IoServiceInterestCallback = extern "C" fn(*mut c_void, IoObjectT, u32, *mut c_void);

const KERN_SUCCESS: KernReturnT = 0;

/// IOKit notification type for a device *arrival* (SPEC §6.3). This is
/// `kIOFirstMatchNotification`, not `kIOMatchedNotification` — the latter
/// (`"IOServiceMatched"`) returns `kIOReturnUnsupported` for IOHIDDevice
/// matching (HW finding #1).
const KIO_FIRST_MATCH_NOTIFICATION: &CStr = c"IOServiceFirstMatch";
/// General-interest notification type; we watch it per device for termination
/// (`kIOTerminatedNotification` is unsupported for IOHIDDevice — finding #1).
const KIO_GENERAL_INTEREST: &CStr = c"IOGeneralInterest";
/// `kIOMessageServiceIsTerminated` — the general-interest message meaning the
/// device has gone away (`0xe0000010`, `iokit_common_msg(0x010)`).
const KIO_MESSAGE_SERVICE_IS_TERMINATED: u32 = 0xe000_0010;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IONotificationPortCreate(main_port: MachPortT) -> IoNotificationPortRef;
    fn IONotificationPortGetRunLoopSource(notify: IoNotificationPortRef) -> CFRunLoopSourceRef;
    fn IONotificationPortDestroy(notify: IoNotificationPortRef);
    fn IOServiceAddMatchingNotification(
        notify_port: IoNotificationPortRef,
        notification_type: *const c_char,
        matching: CFDictionaryRef,
        callback: IoServiceMatchingCallback,
        ref_con: *mut c_void,
        notification: *mut IoIteratorT,
    ) -> KernReturnT;
    fn IOServiceAddInterestNotification(
        notify_port: IoNotificationPortRef,
        service: IoObjectT,
        interest_type: *const c_char,
        callback: IoServiceInterestCallback,
        ref_con: *mut c_void,
        notification: *mut IoObjectT,
    ) -> KernReturnT;
    fn IOIteratorNext(iterator: IoIteratorT) -> IoObjectT;
    fn IOObjectRelease(object: IoObjectT) -> KernReturnT;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObjectT,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
}

/// Refcon for the arrival callback: where to send events, and the notification
/// port so each new device can arm its own removal notification.
struct AddContext {
    sink: mpsc::UnboundedSender<DeviceEvent>,
    port: IoNotificationPortRef,
}

/// Refcon for one device's removal (general-interest) notification. Owns the
/// device object reference and the notification object, both released when the
/// device terminates; carries the descriptor captured at arrival so removal can
/// be reported without reading a gone device.
struct RemovalContext {
    sink: mpsc::UnboundedSender<DeviceEvent>,
    descriptor: DeviceDescriptor,
    device: IoObjectT,
    notification: IoObjectT,
}

/// A `CFRunLoopRef` we only ever hand to `CFRunLoopStop`, which Apple documents
/// as safe to call from any thread.
struct SendRunLoop(CFRunLoopRef);
// SAFETY: the wrapped ref is used solely with the thread-safe CFRunLoopStop.
unsafe impl Send for SendRunLoop {}

/// Start the IOKit monitor. Spawns the CFRunLoop thread and returns a handle
/// that stops it on drop; the thread reports setup success/failure before the
/// run loop starts.
pub fn start_monitor(sink: mpsc::UnboundedSender<DeviceEvent>) -> io::Result<MonitorHandle> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<io::Result<SendRunLoop>>();

    let join = thread::Builder::new()
        .name("kanatabar-iokit".to_string())
        .spawn(move || run_loop_thread(sink, ready_tx))?;

    match ready_rx.recv() {
        Ok(Ok(run_loop)) => {
            let stop: Box<dyn FnOnce() + Send> = Box::new(move || {
                // Bind the whole wrapper so the closure captures the `Send`
                // `SendRunLoop`, not the bare `*mut __CFRunLoop` field (Rust
                // 2021 disjoint closure captures would otherwise grab the ptr).
                let run_loop = run_loop;
                // SAFETY: CFRunLoopStop is thread-safe; run_loop is a valid ref
                // to the monitor thread's run loop until that thread exits,
                // which we then join.
                unsafe { CFRunLoopStop(run_loop.0) };
                let _ = join.join();
            });
            Ok(MonitorHandle::new(stop))
        }
        Ok(Err(err)) => {
            let _ = join.join();
            Err(err)
        }
        Err(_) => Err(io::Error::other("IOKit monitor thread exited during setup")),
    }
}

/// Body of the CFRunLoop thread: register the arrival notification, report
/// readiness, then run the loop until stopped.
fn run_loop_thread(
    sink: mpsc::UnboundedSender<DeviceEvent>,
    ready_tx: std::sync::mpsc::Sender<io::Result<SendRunLoop>>,
) {
    // SAFETY: standard IOKit notification setup. `add_ctx` is valid for the
    // whole run loop and freed below. Each IOServiceMatching call returns a
    // fresh +1 dict that IOServiceAddMatchingNotification consumes.
    let (port, add_ctx) = unsafe {
        let port = IONotificationPortCreate(0);
        if port.is_null() {
            let _ = ready_tx.send(Err(io::Error::other("IONotificationPortCreate failed")));
            return;
        }

        let source = IONotificationPortGetRunLoopSource(port);
        let run_loop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);

        // The context must outlive the notification; freed after the loop.
        let add_ctx = Box::into_raw(Box::new(AddContext { sink, port }));

        if let Err(err) = register_added(port, add_ctx) {
            let _ = ready_tx.send(Err(err));
            IONotificationPortDestroy(port);
            drop(Box::from_raw(add_ctx));
            return;
        }

        if ready_tx.send(Ok(SendRunLoop(run_loop))).is_err() {
            IONotificationPortDestroy(port);
            drop(Box::from_raw(add_ctx));
            return;
        }
        (port, add_ctx)
    };

    debug!("IOKit device monitor running");
    // SAFETY: runs on this thread until CFRunLoopStop is called from the handle.
    unsafe { CFRunLoopRun() };

    // SAFETY: the loop has stopped; tear down the port and free the add context.
    // Per-device `RemovalContext`s for devices still present are intentionally
    // not chased down here — a bounded leak at monitor teardown (daemon exit),
    // reclaimed by the OS; the normal removal path frees each one.
    unsafe {
        IONotificationPortDestroy(port);
        drop(Box::from_raw(add_ctx));
    }
    debug!("IOKit device monitor stopped");
}

/// Register the IOHIDDevice first-match notification and arm it by draining the
/// initial iterator (which also emits the pre-existing devices as `initial`
/// events and arms their removal notifications).
///
/// # Safety
/// `port` must be a live notification port and `ctx` must remain valid for the
/// lifetime of the notification.
unsafe fn register_added(port: IoNotificationPortRef, ctx: *mut AddContext) -> io::Result<()> {
    let mut iterator: IoIteratorT = 0;
    // SAFETY: IOServiceMatching returns a fresh dict consumed by the call below.
    let matching = unsafe { IOServiceMatching(c"IOHIDDevice".as_ptr()) };
    if matching.is_null() {
        return Err(io::Error::other("IOServiceMatching returned null"));
    }
    // SAFETY: valid port, matching dict, callback, and out-iterator pointer.
    let kr = unsafe {
        IOServiceAddMatchingNotification(
            port,
            KIO_FIRST_MATCH_NOTIFICATION.as_ptr(),
            matching as CFDictionaryRef,
            matched_callback,
            ctx as *mut c_void,
            &mut iterator,
        )
    };
    if kr != KERN_SUCCESS {
        return Err(io::Error::other(format!(
            "IOServiceAddMatchingNotification failed: {kr:#x}"
        )));
    }
    // Drain existing matches to arm the notification, reporting them as the
    // initial enumeration (SPEC §7.2) rather than hotplugs (SPEC §6.3).
    // SAFETY: `iterator` is valid from the call above; `ctx` valid per contract.
    unsafe { process_added(iterator, &*ctx, true) };
    Ok(())
}

extern "C" fn matched_callback(refcon: *mut c_void, iterator: IoIteratorT) {
    // SAFETY: refcon is the `add_ctx` pointer, valid for the run loop's life;
    // callback invocations are real hotplugs (initial = false).
    unsafe { process_added(iterator, &*(refcon as *const AddContext), false) };
}

/// Drain an arrival iterator: for each device, emit an `Added` event and arm a
/// per-device removal (general-interest) notification.
///
/// # Safety
/// `iterator` must be a valid `io_iterator_t`; `ctx` must be valid.
unsafe fn process_added(iterator: IoIteratorT, ctx: &AddContext, initial: bool) {
    loop {
        // SAFETY: valid iterator; IOIteratorNext returns 0 when exhausted.
        let device = unsafe { IOIteratorNext(iterator) };
        if device == 0 {
            break;
        }
        // SAFETY: `device` is a live registry entry.
        let descriptor = unsafe { read_descriptor(device) };
        let _ = ctx.sink.send(DeviceEvent {
            change: DeviceChange::Added,
            descriptor: descriptor.clone(),
            initial,
        });

        // Arm this device's removal notification (kIOTerminatedNotification is
        // unsupported for IOHIDDevice, so we watch general interest per device).
        let removal = Box::into_raw(Box::new(RemovalContext {
            sink: ctx.sink.clone(),
            descriptor,
            device,
            notification: 0,
        }));
        let mut notification: IoObjectT = 0;
        // SAFETY: live port and device; `removal` outlives the notification
        // (freed in the callback on termination).
        let kr = unsafe {
            IOServiceAddInterestNotification(
                ctx.port,
                device,
                KIO_GENERAL_INTEREST.as_ptr(),
                removal_callback,
                removal as *mut c_void,
                &mut notification,
            )
        };
        if kr == KERN_SUCCESS {
            // SAFETY: `removal` is a valid, uniquely-owned box; store the
            // notification object to release when the device terminates. The
            // `device` reference from IOIteratorNext is intentionally kept
            // (released on termination), so do NOT release it here.
            unsafe { (*removal).notification = notification };
        } else {
            // Could not track removal; don't leak — report the add, but drop
            // the removal bookkeeping and release the device reference.
            // SAFETY: `removal` is a valid box we just created and no C code
            // holds it; `device` is our +1 reference.
            unsafe {
                debug!(
                    kr = format!("{kr:#x}"),
                    name = %(*removal).descriptor.name,
                    "could not arm removal notification for device",
                );
                IOObjectRelease(device);
                drop(Box::from_raw(removal));
            }
        }
    }
}

extern "C" fn removal_callback(
    refcon: *mut c_void,
    _service: IoObjectT,
    message_type: u32,
    _message_argument: *mut c_void,
) {
    // General interest fires for several message types; only termination means
    // the device is gone. Anything else: leave the context in place.
    if message_type != KIO_MESSAGE_SERVICE_IS_TERMINATED {
        return;
    }
    // SAFETY: refcon is the `RemovalContext` we leaked when arming this
    // notification; termination fires at most once, so reclaim the box now.
    let ctx = unsafe { Box::from_raw(refcon as *mut RemovalContext) };
    let _ = ctx.sink.send(DeviceEvent {
        change: DeviceChange::Removed,
        descriptor: ctx.descriptor.clone(),
        initial: false,
    });
    // SAFETY: release the notification object and the device reference this
    // context owned; both are live until now.
    unsafe {
        IOObjectRelease(ctx.notification);
        IOObjectRelease(ctx.device);
    }
}

/// Read presence facts (Product, VendorID, PrimaryUsagePage, PrimaryUsage) from
/// a registry entry.
///
/// # Safety
/// `entry` must be a valid registry entry (`io_object_t`).
unsafe fn read_descriptor(entry: IoObjectT) -> DeviceDescriptor {
    DeviceDescriptor {
        // SAFETY: valid entry; read_* handle null/typed properties.
        name: unsafe { read_string(entry, "Product") }.unwrap_or_default(),
        vendor_id: unsafe { read_number(entry, "VendorID") }.map(|n| n as u32),
        usage_page: unsafe { read_number(entry, "PrimaryUsagePage") }.map(|n| n as u32),
        usage: unsafe { read_number(entry, "PrimaryUsage") }.map(|n| n as u32),
    }
}

/// Read a string registry property, or `None` if absent or not a string.
///
/// # Safety
/// `entry` must be a valid registry entry.
unsafe fn read_string(entry: IoObjectT, key: &str) -> Option<String> {
    let cf_key = CFString::new(key);
    // SAFETY: valid entry and key ref; returns a +1 CFTypeRef or null.
    let raw = unsafe {
        IORegistryEntryCreateCFProperty(entry, cf_key.as_concrete_TypeRef(), kCFAllocatorDefault, 0)
    };
    if raw.is_null() {
        return None;
    }
    // SAFETY: raw is a valid +1 CF object; check its type before adopting it.
    unsafe {
        if CFGetTypeID(raw) == CFString::type_id() {
            let string = CFString::wrap_under_create_rule(raw as CFStringRef);
            Some(string.to_string())
        } else {
            CFRelease(raw);
            None
        }
    }
}

/// Read an integer registry property, or `None` if absent or not a number.
///
/// # Safety
/// `entry` must be a valid registry entry.
unsafe fn read_number(entry: IoObjectT, key: &str) -> Option<i64> {
    let cf_key = CFString::new(key);
    // SAFETY: valid entry and key ref; returns a +1 CFTypeRef or null.
    let raw = unsafe {
        IORegistryEntryCreateCFProperty(entry, cf_key.as_concrete_TypeRef(), kCFAllocatorDefault, 0)
    };
    if raw.is_null() {
        return None;
    }
    // SAFETY: raw is a valid +1 CF object; check its type before adopting it.
    unsafe {
        if CFGetTypeID(raw) == CFNumber::type_id() {
            CFNumber::wrap_under_create_rule(raw as CFNumberRef).to_i64()
        } else {
            CFRelease(raw);
            None
        }
    }
}
