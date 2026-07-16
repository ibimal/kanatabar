//! Process-path lookup for the orphan sweep (SPEC §6.1b).
//!
//! `proc_pidpath(2)` gives the executable path of a pid, so the sweep can
//! confirm a recorded pid is still *our* kanata (not a reused pid) before
//! killing it.

use std::ffi::c_void;

/// The executable path of `pid`, or `None` if the process is gone or the call
/// fails.
pub fn process_path(pid: u32) -> Option<String> {
    // PROC_PIDPATHINFO_MAXSIZE (4 * MAXPATHLEN).
    let mut buf = vec![0u8; 4096];

    // SAFETY: `buf` is a valid, writable allocation of `buf.len()` bytes;
    // proc_pidpath writes at most that many bytes and returns the length
    // written (>0) or <=0 on error. It does not retain the pointer.
    let len = unsafe {
        libc::proc_pidpath(
            pid as i32,
            buf.as_mut_ptr() as *mut c_void,
            buf.len() as u32,
        )
    };
    if len <= 0 {
        return None;
    }
    buf.truncate(len as usize);
    String::from_utf8(buf).ok()
}
