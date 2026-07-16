//! Peer-credential lookup for the control socket (SPEC §7.1, §18).
//!
//! macOS exposes the peer's effective uid/gid on a connected `AF_UNIX` stream
//! socket via `getpeereid(2)`, which is simpler and less error-prone than
//! decoding `LOCAL_PEERCRED` into a `struct xucred` by hand.

use std::io;
use std::os::fd::RawFd;

/// The effective uid of the process on the other end of `fd`.
///
/// `fd` must be a connected `AF_UNIX` stream socket.
pub fn peer_uid(fd: RawFd) -> io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;

    // SAFETY: `fd` is a valid, connected AF_UNIX socket fd owned by the caller
    // for the duration of this call. `&mut uid`/`&mut gid` are valid, aligned,
    // writable pointers to `uid_t`/`gid_t`; getpeereid writes through them only
    // on success and does not retain them past the call.
    let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };

    if rc == 0 {
        Ok(uid as u32)
    } else {
        Err(io::Error::last_os_error())
    }
}
