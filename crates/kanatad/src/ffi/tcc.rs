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
//! HW-verified 2026-07-17/18 (docs/HW-TESTS.md #19): reads are accurate from
//! the daemon context but launch-cached per process, so the doctor calls
//! these from a freshly spawned probe child (`kanatad tcc-status`) for live
//! status. TCC semantics captured on HW: an explicit deny record and a
//! stale (pre-update) record both read `Denied`; **no record at all** reads
//! `Granted` for Input Monitoring (allow-unless-denied for the root daemon)
//! but not-trusted for Accessibility. The prompting *request* APIs
//! (`IOHIDRequestAccess`/`AXIsProcessTrustedWithOptions`) were removed after
//! HW showed they register nothing from the system context.
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

/// Wire form for the `kanatad tcc-status` probe (`granted`/`denied`/
/// `unknown`); [`std::str::FromStr`] is the inverse.
impl std::fmt::Display for AccessStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AccessStatus::Granted => "granted",
            AccessStatus::Denied => "denied",
            AccessStatus::Unknown => "unknown",
        })
    }
}

impl std::str::FromStr for AccessStatus {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "granted" => Ok(AccessStatus::Granted),
            "denied" => Ok(AccessStatus::Denied),
            "unknown" => Ok(AccessStatus::Unknown),
            _ => Err(()),
        }
    }
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
