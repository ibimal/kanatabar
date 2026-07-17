//! macOS TCC permission *status* for kanatad, read from the daemon's own
//! grant (SPEC §9, §11 [VERIFY]).
//!
//! The grants that govern kanata's device access attach to the **supervising
//! daemon** (kanatad is the TCC responsible process for its kanata child —
//! SPEC §2 [HARD, verified]), so the doctor — which runs in-process in
//! kanatad — reads kanatad's own grant here. These are the same public
//! per-process APIs a foreground remapper (e.g. Thaw) uses on itself;
//! crucially they ask the OS "does the *calling* process hold this grant?"
//! and do **not** read the SIP-protected TCC.db.
//!
//! [VERIFY / HW-pending] The reads' accuracy *from a root LaunchDaemon in the
//! system context* is not yet hardware-verified (docs/HW-TESTS.md). The
//! daemon-doctor policy is therefore conservative: a definitive `Denied` is
//! trusted (a real failure), but `Unknown` falls back to the behavioral
//! backstop (a denied kanata dies at startup → `Degraded{InputMonitoringDenied}`,
//! §6.5) rather than risking a false red.
//!
//! All `unsafe` in the daemon lives under this module (CLAUDE.md, SPEC §14).

/// The result of an `IOHIDCheckAccess` query (`IOHIDAccessType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessStatus {
    /// The grant is present (`kIOHIDAccessTypeGranted`).
    Granted,
    /// The grant was explicitly denied (`kIOHIDAccessTypeDenied`).
    Denied,
    /// Not yet determined (`kIOHIDAccessTypeUnknown`) — never requested, or
    /// the daemon context can't resolve it.
    Unknown,
}

// IOHIDRequestType (IOKit/hid/IOHIDLib.h): the access we care about is the
// ability to *listen* to HID events (Input Monitoring).
const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;

// IOHIDAccessType (IOKit/hid/IOHIDLib.h).
const K_IOHID_ACCESS_TYPE_GRANTED: u32 = 0;
const K_IOHID_ACCESS_TYPE_DENIED: u32 = 1;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request_type: u32) -> u32;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    // Apple's `Boolean` is an unsigned char; model it as u8 and compare != 0.
    fn AXIsProcessTrusted() -> u8;
}

/// The calling process's Input Monitoring (listen-event) grant.
pub fn input_monitoring_access() -> AccessStatus {
    // SAFETY: `IOHIDCheckAccess` is a pure query with no pointer arguments;
    // it returns an `IOHIDAccessType` enum value. Safe to call from any
    // thread.
    let raw = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
    match raw {
        K_IOHID_ACCESS_TYPE_GRANTED => AccessStatus::Granted,
        K_IOHID_ACCESS_TYPE_DENIED => AccessStatus::Denied,
        // kIOHIDAccessTypeUnknown (2) and any future value.
        _ => AccessStatus::Unknown,
    }
}

/// Whether the calling process is a trusted Accessibility client. The
/// non-prompting form (`AXIsProcessTrusted`) — never surfaces a dialog, so
/// it is safe to poll from the doctor.
pub fn accessibility_trusted() -> bool {
    // SAFETY: `AXIsProcessTrusted` takes no arguments and only reads the
    // calling process's trust state; it never prompts.
    unsafe { AXIsProcessTrusted() != 0 }
}
