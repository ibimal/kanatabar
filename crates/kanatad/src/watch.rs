//! Active-config file watch (SPEC §6.4): watch the active `.kbd`, debounce
//! bursts of change events, then revalidate and restart — or, if the edit is
//! broken, keep the running config (SPEC §16).
//!
//! Uses the `notify` crate (filesystem events, no polling — SPEC §6.6). We
//! watch the *parent directory* so editor rename-replace is caught, and filter
//! to the active file's name. The active path can change (preset switch), so
//! the watch is re-armed after each reload.

use std::path::PathBuf;
use std::time::Duration;

use notify::{Event, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::config::ActiveConfig;
use crate::configmgr::ConfigManager;

/// Handle to the watch task; abort it on shutdown.
pub struct ConfigWatchHandle {
    task: JoinHandle<()>,
}

impl ConfigWatchHandle {
    /// Stop watching.
    pub fn abort(&self) {
        self.task.abort();
    }
}

/// Start watching the active config for on-disk edits.
pub fn spawn(
    manager: ConfigManager,
    active: ActiveConfig,
    debounce: Duration,
) -> notify::Result<ConfigWatchHandle> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<PathBuf>>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            // Only content-changing kinds matter; ignore metadata/access noise.
            if event.kind.is_modify() || event.kind.is_create() || event.kind.is_remove() {
                let _ = tx.send(event.paths);
            }
        }
    })?;

    let mut watched_dir = rearm(&mut watcher, &active, None);

    let task = tokio::spawn(async move {
        // Keep the watcher alive for the lifetime of the task.
        let mut watcher = watcher;
        loop {
            let Some(paths) = rx.recv().await else {
                break; // sender dropped
            };
            if !touches_active(&paths, &active) {
                continue;
            }

            // Debounce: swallow further events until the dir is quiet for
            // `debounce` (SPEC §6.2 [HARD] coalescing).
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(debounce) => break,
                    more = rx.recv() => {
                        if more.is_none() { return; }
                    }
                }
            }

            info!("active config changed on disk; revalidating");
            match manager.reload_active().await {
                Ok(true) => info!("active config reloaded"),
                Ok(false) => warn!("edited config is invalid; keeping the running config"),
                Err(err) => warn!(%err, "config reload failed"),
            }

            // The active path may have moved; re-arm on the current parent.
            watched_dir = rearm(&mut watcher, &active, watched_dir);
        }
    });

    Ok(ConfigWatchHandle { task })
}

/// Whether any changed path is the active config file (matched by name+parent).
fn touches_active(paths: &[PathBuf], active: &ActiveConfig) -> bool {
    let target = active.spawn_target().kanata_cfg;
    let name = target.file_name();
    name.is_some() && paths.iter().any(|p| p.file_name() == name)
}

/// Watch the parent directory of the current active config, unwatching the
/// previous one. Returns the newly-watched directory.
fn rearm(
    watcher: &mut notify::RecommendedWatcher,
    active: &ActiveConfig,
    previous: Option<PathBuf>,
) -> Option<PathBuf> {
    let dir = active.spawn_target().kanata_cfg.parent().map(PathBuf::from);
    if dir == previous {
        return previous;
    }
    if let Some(old) = &previous {
        let _ = watcher.unwatch(old);
    }
    if let Some(new) = &dir {
        if let Err(err) = watcher.watch(new, RecursiveMode::NonRecursive) {
            debug!(%err, dir = %new.display(), "could not watch config directory");
        }
    }
    dir
}
