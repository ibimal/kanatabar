//! Peer-credential authorization policy for the control socket (SPEC §7.1, §14).
//!
//! Allowed peers: uid 0, the daemon's own euid (which *is* 0 in production, so
//! this adds nothing there — but lets same-user dev/test connect), and the
//! current console user (owner of `/dev/console`, re-read per connection so
//! fast user switching is respected — SPEC §16). Everyone else is rejected and
//! logged.

use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

/// A pluggable authorization decision: given a peer uid, may it connect?
/// Boxed so integration tests can inject a policy that simulates a
/// wrong-uid peer without needing multiple real uids.
pub type AuthPolicy = Arc<dyn Fn(u32) -> bool + Send + Sync>;

/// The pure allow rule (SPEC §7.1), separated for unit testing.
pub fn authorize(peer_uid: u32, own_euid: u32, console_uid: Option<u32>) -> bool {
    peer_uid == 0 || peer_uid == own_euid || console_uid == Some(peer_uid)
}

/// uid that owns `/dev/console`, i.e. the current console user (SPEC §18).
pub fn console_uid() -> Option<u32> {
    std::fs::metadata("/dev/console").ok().map(|m| m.uid())
}

/// The production policy: uid 0 + own euid + current console user, with the
/// console user re-evaluated on every connection.
pub fn system_policy() -> AuthPolicy {
    let own_euid = nix::unistd::geteuid().as_raw();
    Arc::new(move |peer_uid| authorize(peer_uid, own_euid, console_uid()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_always_allowed() {
        assert!(authorize(0, 501, None));
        assert!(authorize(0, 0, Some(999)));
    }

    #[test]
    fn own_euid_is_allowed() {
        assert!(authorize(501, 501, None));
    }

    #[test]
    fn console_user_is_allowed() {
        assert!(authorize(501, 0, Some(501)));
    }

    #[test]
    fn other_uids_are_rejected() {
        // Not root, not the daemon's euid, not the console user.
        assert!(!authorize(777, 0, Some(501)));
        assert!(!authorize(777, 501, None));
        assert!(!authorize(777, 501, Some(502)));
    }
}
