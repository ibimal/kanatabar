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

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;

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
    // Triggers a TCC request for the calling process; returns whether access
    // is (now) granted. Registers the process in the Input Monitoring pane.
    fn IOHIDRequestAccess(request_type: u32) -> u8;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    // Apple's `Boolean` is an unsigned char; model it as u8 and compare != 0.
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    // Options key: when its value is true, the call prompts / registers the
    // process in the Accessibility pane. A framework-provided CFString global.
    static kAXTrustedCheckOptionPrompt: CFStringRef;
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

/// Ask macOS to grant **this process** Input Monitoring (SPEC §11.2). From a
/// GUI session this prompts; from the daemon's system context it registers
/// kanatad's entry in the Input Monitoring pane (denied until the user
/// toggles it). Returns whether access is granted afterward.
///
/// [VERIFY / HW-pending] Whether a root LaunchDaemon's request actually
/// registers the entry from the system context is not yet hardware-confirmed
/// (docs/HW-TESTS.md #19).
pub fn request_input_monitoring() -> bool {
    // SAFETY: pure request call, no pointer arguments; triggers a TCC request
    // for the calling process and returns the resulting grant as a Boolean.
    unsafe { IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) != 0 }
}

/// Ask macOS to grant **this process** Accessibility (SPEC §11.2), the
/// prompting counterpart of [`accessibility_trusted`]. Same daemon-context
/// caveat as [`request_input_monitoring`]. Returns the trust state afterward.
pub fn request_accessibility() -> bool {
    // SAFETY: `kAXTrustedCheckOptionPrompt` is a framework-provided CFString
    // global, valid for the process lifetime; wrap under the get rule (no
    // ownership transfer) to key the options dictionary.
    let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let options =
        CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())]);
    // SAFETY: the options dict is passed by borrowed ref; the call reads it
    // and returns the trust state, prompting when a GUI session is available.
    unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) != 0 }
}
