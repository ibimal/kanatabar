//! Single-instance guard (SPEC §8: "single instance (bail if socket already
//! served by another tray via a per-user lock file)"). An exclusive,
//! non-blocking `flock` on a lock file — the *root* `/Library/Application
//! Support/KanataBar` (SPEC §3.2) isn't writable by an unprivileged user, so
//! this lives under the user's own `~/Library` instead.

use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

use nix::fcntl::{Flock, FlockArg};

/// Held for the process lifetime; dropping releases the lock (BSD `flock`
/// releases on close, and `Flock`'s own `Drop` unlocks explicitly).
pub struct SingleInstanceLock {
    path: PathBuf,
    _file: Flock<File>,
}

impl SingleInstanceLock {
    /// Acquire the lock at `path`, creating it (and its parent dir) if needed.
    /// Fails immediately — never blocks — if another instance already holds it.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|err| {
                io::Error::new(err.kind(), format!("opening {}: {err}", path.display()))
            })?;
        let locked = Flock::lock(file, FlockArg::LockExclusiveNonblock)
            .map_err(|(_file, errno)| io::Error::from(errno))?;
        Ok(Self {
            path: path.to_path_buf(),
            _file: locked,
        })
    }

    /// The lock file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Default lock path for the current user (SPEC §8).
pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join("Library/Application Support/KanataBar/tray.lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_while_first_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.lock");
        let _first = SingleInstanceLock::acquire(&path).expect("first acquire");
        assert!(SingleInstanceLock::acquire(&path).is_err());
    }

    #[test]
    fn lock_is_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.lock");
        {
            let _first = SingleInstanceLock::acquire(&path).expect("first acquire");
        }
        SingleInstanceLock::acquire(&path).expect("lock released after drop");
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/tray.lock");
        SingleInstanceLock::acquire(&path).expect("creates parents");
        assert!(path.exists());
    }
}
