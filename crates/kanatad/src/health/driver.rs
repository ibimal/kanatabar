//! Driver preflight (SPEC §6.5 [HARD]): the Karabiner DriverKit extension must
//! be `activated enabled` and the Karabiner VirtualHIDDevice daemon must be
//! running, or the supervisor goes `Degraded` — never a crash loop.
//!
//! The pure `systemextensionsctl` parsing lives in `kanatabar_core::driver`;
//! here we run the commands and combine them into a [`DriverHealth`]. The
//! probe is injectable so the supervisor can be tested without the real driver.

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;

use kanatabar_core::driver::{parse_systemextensions, DriverState};
use tokio::process::Command;
use tracing::{debug, warn};

/// Result of the driver + VHID-daemon preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverHealth {
    /// Driver activated + VHID daemon running.
    Ok,
    /// The DriverKit extension is not `activated enabled`.
    DriverNotActivated,
    /// The VirtualHIDDevice daemon is not running.
    VhidDaemonDown,
}

/// An async driver probe. Injectable so the supervisor can be driven without
/// the real driver in tests (SPEC §17).
pub type DriverProbe =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = DriverHealth> + Send>> + Send + Sync>;

/// The production probe: parse `systemextensionsctl list`, then confirm the
/// VHID daemon process is alive.
pub fn system_probe() -> DriverProbe {
    Arc::new(|| Box::pin(check()))
}

/// Run the real preflight (SPEC §6.5).
pub async fn check() -> DriverHealth {
    match extension_state().await {
        Some(state) if state.is_ready() => {}
        Some(state) => {
            warn!(detail = state.message(), "driver preflight failed");
            return DriverHealth::DriverNotActivated;
        }
        None => {
            warn!("could not run systemextensionsctl; treating driver as not activated");
            return DriverHealth::DriverNotActivated;
        }
    }

    if vhid_daemon_running().await {
        DriverHealth::Ok
    } else {
        warn!("Karabiner VirtualHIDDevice daemon not running");
        DriverHealth::VhidDaemonDown
    }
}

/// Raw `systemextensionsctl list` output, or `None` if it could not be run.
/// Absolute path: a root daemon must not resolve helpers via `PATH` (§14).
/// Public so `doctor` can also read the driver *version* from it (§6.5a).
pub async fn systemextensions_output() -> Option<String> {
    let output = Command::new("/usr/bin/systemextensionsctl")
        .arg("list")
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `systemextensionsctl list`, or `None` if it could not be run.
async fn extension_state() -> Option<DriverState> {
    let text = systemextensions_output().await?;
    let state = parse_systemextensions(&text);
    debug!(?state, "parsed systemextensionsctl list");
    Some(state)
}

/// Whether the Karabiner VHID daemon process is running. [VERIFY] the exact
/// bundle path against the installed Karabiner driver.
async fn vhid_daemon_running() -> bool {
    // `pgrep -f` matches the full command line; anchoring on the bundle-relative
    // executable path (present wherever the daemon is installed from) avoids
    // false positives like `tail -f …Karabiner-VirtualHIDDevice-Daemon.log`.
    // Absolute pgrep path: no `PATH` lookup from a root daemon (§14).
    Command::new("/usr/bin/pgrep")
        .arg("-f")
        .arg("Karabiner-VirtualHIDDevice-Daemon.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

/// System-domain launchd labels (`launchctl list` — absolute path, §14), for
/// the §6.5a "who manages the VHID daemon" doctor check. Empty on failure.
pub async fn system_launchd_labels() -> Vec<String> {
    let output = match Command::new("/bin/launchctl")
        .arg("list")
        .stdin(Stdio::null())
        .output()
        .await
    {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    kanatabar_core::vhidd::parse_launchctl_list(&String::from_utf8_lossy(&output.stdout))
}
