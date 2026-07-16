//! Pure path-safety rules for user-supplied config paths (SPEC §6.4, §14).
//!
//! The root daemon reads `.kbd` files whose paths arrive over the control
//! socket. The I/O — canonicalizing, opening, and `fstat`-ing the file — lives
//! in the daemon; this module is the pure decision made from the resulting
//! facts, so every accept/reject case is unit-testable on any OS.

/// Facts about an opened file, gathered by `fstat` on the daemon side so the
/// decision applies to the exact inode that was opened (TOCTOU-safe, §14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathFacts {
    /// The opened path resolves to a regular file (not a dir, device, fifo…).
    pub is_regular_file: bool,
    /// Owning uid of the file.
    pub uid: u32,
    /// Unix permission bits (`st_mode & 0o7777`).
    pub mode: u32,
}

/// Why a path was rejected (maps to `ErrorKind::PathRejected`, SPEC §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathReject {
    /// Not a regular file (symlink target, directory, device, …).
    NotRegularFile,
    /// Owned by neither root nor the requesting connection's uid.
    ForeignOwner,
    /// Group- or world-writable, so another user could swap its contents.
    Writable,
}

impl PathReject {
    /// A user-facing, actionable one-liner (SPEC §15).
    pub fn describe(self) -> &'static str {
        match self {
            Self::NotRegularFile => "path is not a regular file",
            Self::ForeignOwner => {
                "config must be owned by root or by you; refusing a foreign-owned file"
            }
            Self::Writable => "config is group- or world-writable; tighten it to 0644 or stricter",
        }
    }
}

/// Decide whether an opened file is safe to read as root (SPEC §6.4 [HARD]):
/// it must be a regular file, owned by root or the requesting uid, and not
/// group/world-writable.
pub fn check(facts: &PathFacts, requesting_uid: u32) -> Result<(), PathReject> {
    if !facts.is_regular_file {
        return Err(PathReject::NotRegularFile);
    }
    if facts.uid != 0 && facts.uid != requesting_uid {
        return Err(PathReject::ForeignOwner);
    }
    // 0o022 = group-write | other-write.
    if facts.mode & 0o022 != 0 {
        return Err(PathReject::Writable);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQ_UID: u32 = 501;

    fn facts(is_regular_file: bool, uid: u32, mode: u32) -> PathFacts {
        PathFacts {
            is_regular_file,
            uid,
            mode,
        }
    }

    #[test]
    fn accepts_root_owned_readonly() {
        assert_eq!(check(&facts(true, 0, 0o644), REQ_UID), Ok(()));
    }

    #[test]
    fn accepts_requester_owned_readonly() {
        assert_eq!(check(&facts(true, REQ_UID, 0o600), REQ_UID), Ok(()));
        assert_eq!(check(&facts(true, REQ_UID, 0o644), REQ_UID), Ok(()));
    }

    #[test]
    fn rejects_non_regular_file() {
        assert_eq!(
            check(&facts(false, 0, 0o644), REQ_UID),
            Err(PathReject::NotRegularFile)
        );
    }

    #[test]
    fn rejects_foreign_owner() {
        // Owned by some other unprivileged user.
        assert_eq!(
            check(&facts(true, 999, 0o644), REQ_UID),
            Err(PathReject::ForeignOwner)
        );
    }

    #[test]
    fn rejects_group_writable() {
        assert_eq!(
            check(&facts(true, 0, 0o664), REQ_UID),
            Err(PathReject::Writable)
        );
    }

    #[test]
    fn rejects_world_writable() {
        assert_eq!(
            check(&facts(true, REQ_UID, 0o646), REQ_UID),
            Err(PathReject::Writable)
        );
    }

    #[test]
    fn regular_file_check_precedes_owner_check() {
        // A non-regular file owned by a foreign uid reports the file-type fault.
        assert_eq!(
            check(&facts(false, 999, 0o777), REQ_UID),
            Err(PathReject::NotRegularFile)
        );
    }
}
