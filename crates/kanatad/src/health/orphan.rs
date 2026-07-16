//! Orphan sweep (SPEC §6.1b): before the first spawn, kill any kanata left
//! running by a previous daemon (e.g. after a `kill -9`), so the single-instance
//! invariant holds [HARD]. Guards against pid reuse by matching the executable.

use std::path::Path;
use std::time::Duration;

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tracing::{info, warn};

use crate::ffi::proc::process_path;

/// Whether `pid` is alive (visible to `kill(pid, 0)`).
pub fn is_alive(pid: u32) -> bool {
    !matches!(
        kill(Pid::from_raw(pid as i32), None),
        Err(nix::errno::Errno::ESRCH)
    )
}

/// Whether the process at `pid` is running the same executable as
/// `expected_binary` (basename match), so we only sweep our own kanata.
fn is_expected(pid: u32, expected_binary: &Path) -> bool {
    let Some(expected_name) = expected_binary.file_name() else {
        return false;
    };
    match process_path(pid) {
        Some(path) => Path::new(&path).file_name() == Some(expected_name),
        None => false,
    }
}

/// Sweep a recorded kanata pid: if it is alive and is our kanata, SIGTERM →
/// wait ≤ `grace` → SIGKILL (SPEC §6.1b). No-op if the pid is absent, dead, or
/// belongs to a different (reused) process.
pub async fn sweep(recorded_pid: Option<u32>, expected_binary: &Path, grace: Duration) {
    let Some(pid) = recorded_pid else {
        return;
    };
    if !is_alive(pid) {
        return;
    }
    if !is_expected(pid, expected_binary) {
        warn!(
            pid,
            "recorded kanata pid is alive but runs a different executable; not sweeping"
        );
        return;
    }

    warn!(pid, "sweeping orphaned kanata from a previous daemon");
    terminate(pid, grace).await;
}

/// Sweep *stray* kanata processes beyond the recorded pid (SPEC §6.1b: "also
/// scan for stray kanata processes"): any live process whose executable is
/// `expected_binary`, even when `state.json` was lost or corrupt after a
/// `kill -9`. Without this, a surviving kanata makes every spawn fail with
/// "device already open" and the daemon backs off into `Degraded`.
pub async fn sweep_strays(expected_binary: &Path, grace: Duration) {
    for pid in find_by_binary(expected_binary).await {
        // `find_by_binary` over-matches (pgrep -f is a substring match), so
        // confirm via the kernel-reported executable path before killing.
        if is_expected(pid, expected_binary) {
            warn!(pid, "sweeping stray kanata (not in state.json)");
            terminate(pid, grace).await;
        }
    }
}

/// Pids whose command line mentions `binary` (absolute pgrep, no `PATH`
/// lookup from a root daemon — §14). Candidates only; callers must confirm
/// with [`is_expected`].
async fn find_by_binary(binary: &Path) -> Vec<u32> {
    let output = match tokio::process::Command::new("/usr/bin/pgrep")
        .arg("-f")
        .arg(binary.as_os_str())
        .stdin(std::process::Stdio::null())
        .output()
        .await
    {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    let own_pid = std::process::id();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .filter(|&pid| pid != own_pid)
        .collect()
}

/// SIGTERM → wait ≤ `grace` → SIGKILL (bounded poll, one-shot at startup).
async fn terminate(pid: u32, grace: Duration) {
    let target = Pid::from_raw(pid as i32);
    let _ = kill(target, Signal::SIGTERM);

    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if !is_alive(pid) {
            info!(pid, "orphan exited after SIGTERM");
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    warn!(pid, "orphan ignored SIGTERM; sending SIGKILL");
    let _ = kill(target, Signal::SIGKILL);
}
