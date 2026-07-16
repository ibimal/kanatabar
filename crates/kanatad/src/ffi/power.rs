//! Sleep/wake notifications via `IORegisterForSystemPower` on a dedicated
//! CFRunLoop thread (SPEC §6.5 [HARD], §3.1).
//!
//! On wake, the daemon re-verifies health and re-syncs kanata (a common
//! real-world failure a naive supervisor misses: the device grab is lost across
//! sleep). We must acknowledge sleep messages promptly or the system stalls.
//!
//! [HW] Cannot be exercised without real sleep/wake; see `docs/HW-TESTS.md`.
//! All unsafe is confined here (CLAUDE.md).

use std::ffi::c_void;
use std::io;
use std::thread;

use core_foundation_sys::runloop::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRef, CFRunLoopRun,
    CFRunLoopSourceRef, CFRunLoopStop,
};
use tracing::debug;

use crate::device::MonitorHandle;

type IoObjectT = u32;
type IoConnectT = u32;
type IoNotificationPortRef = *mut c_void;
type IoServiceInterestCallback = extern "C" fn(
    refcon: *mut c_void,
    service: IoObjectT,
    message_type: u32,
    message_arg: *mut c_void,
);

// IOKit power-management message types (IOMessage.h).
const K_IO_MESSAGE_CAN_SYSTEM_SLEEP: u32 = 0xE000_0270;
const K_IO_MESSAGE_SYSTEM_WILL_SLEEP: u32 = 0xE000_0280;
const K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON: u32 = 0xE000_0320;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut c_void,
        the_port: *mut IoNotificationPortRef,
        callback: IoServiceInterestCallback,
        notifier: *mut IoObjectT,
    ) -> IoConnectT;
    fn IODeregisterForSystemPower(notifier: *mut IoObjectT) -> i32;
    fn IONotificationPortGetRunLoopSource(notify: IoNotificationPortRef) -> CFRunLoopSourceRef;
    fn IONotificationPortDestroy(notify: IoNotificationPortRef);
    fn IOAllowPowerChange(kern_port: IoConnectT, notification_id: isize) -> i32;
}

/// Callback context: what to run on wake, and the root port needed to
/// acknowledge sleep. `root_port` is filled in after registration, before the
/// run loop (and any callback) starts.
struct Context {
    on_wake: Box<dyn Fn() + Send>,
    root_port: IoConnectT,
}

struct SendRunLoop(CFRunLoopRef);
// SAFETY: used only with the thread-safe CFRunLoopStop.
unsafe impl Send for SendRunLoop {}

/// Start the sleep/wake monitor. `on_wake` runs on the CFRunLoop thread each
/// time the system wakes. Returns a handle that stops the thread on drop.
pub fn start_power_monitor(on_wake: Box<dyn Fn() + Send>) -> io::Result<MonitorHandle> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<io::Result<SendRunLoop>>();

    let join = thread::Builder::new()
        .name("kanatabar-power".to_string())
        .spawn(move || power_thread(on_wake, ready_tx))?;

    match ready_rx.recv() {
        Ok(Ok(run_loop)) => {
            let stop: Box<dyn FnOnce() + Send> = Box::new(move || {
                let run_loop = run_loop; // capture the Send wrapper whole
                                         // SAFETY: CFRunLoopStop is thread-safe and run_loop is valid
                                         // until the thread we then join exits.
                unsafe { CFRunLoopStop(run_loop.0) };
                let _ = join.join();
            });
            Ok(MonitorHandle::new(stop))
        }
        Ok(Err(err)) => {
            let _ = join.join();
            Err(err)
        }
        Err(_) => Err(io::Error::other("power monitor thread exited during setup")),
    }
}

fn power_thread(
    on_wake: Box<dyn Fn() + Send>,
    ready_tx: std::sync::mpsc::Sender<io::Result<SendRunLoop>>,
) {
    let ctx = Box::into_raw(Box::new(Context {
        on_wake,
        root_port: 0,
    }));

    // SAFETY: standard IORegisterForSystemPower setup; `ctx` is valid for the
    // whole run loop and freed below. `root_port` is written into the context
    // before the loop (and any callback) runs.
    let (port, notifier) = unsafe {
        let mut notify_port: IoNotificationPortRef = std::ptr::null_mut();
        let mut notifier: IoObjectT = 0;
        let root_port = IORegisterForSystemPower(
            ctx as *mut c_void,
            &mut notify_port,
            power_callback,
            &mut notifier,
        );
        if root_port == 0 || notify_port.is_null() {
            let _ = ready_tx.send(Err(io::Error::other("IORegisterForSystemPower failed")));
            drop(Box::from_raw(ctx));
            return;
        }
        (*ctx).root_port = root_port;

        let source = IONotificationPortGetRunLoopSource(notify_port);
        let run_loop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);

        if ready_tx.send(Ok(SendRunLoop(run_loop))).is_err() {
            IODeregisterForSystemPower(&mut notifier);
            IONotificationPortDestroy(notify_port);
            drop(Box::from_raw(ctx));
            return;
        }
        (notify_port, notifier)
    };

    debug!("power (sleep/wake) monitor running");
    // SAFETY: runs until CFRunLoopStop is called from the handle.
    unsafe { CFRunLoopRun() };

    // SAFETY: loop stopped; deregister, destroy the port, free the context.
    unsafe {
        let mut notifier = notifier;
        IODeregisterForSystemPower(&mut notifier);
        IONotificationPortDestroy(port);
        drop(Box::from_raw(ctx));
    }
    debug!("power monitor stopped");
}

extern "C" fn power_callback(
    refcon: *mut c_void,
    _service: IoObjectT,
    message_type: u32,
    message_arg: *mut c_void,
) {
    // SAFETY: refcon is the `ctx` pointer, valid for the run loop's lifetime,
    // accessed only on this (the run loop) thread.
    let ctx = unsafe { &*(refcon as *const Context) };
    match message_type {
        // Must acknowledge sleep promptly or the system stalls.
        K_IO_MESSAGE_CAN_SYSTEM_SLEEP | K_IO_MESSAGE_SYSTEM_WILL_SLEEP => {
            // SAFETY: root_port was set before the loop started.
            unsafe { IOAllowPowerChange(ctx.root_port, message_arg as isize) };
        }
        K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON => {
            debug!("system woke; running wake handler");
            (ctx.on_wake)();
        }
        _ => {}
    }
}
