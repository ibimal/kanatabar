//! "Launch at Login" — the per-user LaunchAgent toggle (SPEC §8). Reuses the
//! label the installer wrote the plist under (`kanatactl::install`) rather than
//! duplicating the string. The `launchctl` *decision* (`toggle_args`) is pure
//! and unit-tested; actually running it is the untestable side effect.

use std::path::{Path, PathBuf};
use std::process::Command;

pub use kanatactl::install::AGENT_LABEL;

/// Absolute `launchctl` path — no `PATH` lookup (§14 house rule for anything
/// that drives system state).
pub const LAUNCHCTL: &str = "/bin/launchctl";

/// The `launchctl` arguments to flip the agent to the *opposite* of
/// `currently_loaded`, in the caller's `gui/<uid>` domain (SPEC §10).
pub fn toggle_args(currently_loaded: bool, uid: u32, agent_plist: &Path) -> Vec<String> {
    if currently_loaded {
        vec!["bootout".to_string(), format!("gui/{uid}/{AGENT_LABEL}")]
    } else {
        vec![
            "bootstrap".to_string(),
            format!("gui/{uid}"),
            agent_plist.display().to_string(),
        ]
    }
}

/// Whether the agent job is currently loaded in the caller's `gui/<uid>`
/// domain.
pub fn is_loaded(uid: u32) -> bool {
    Command::new(LAUNCHCTL)
        .args(["print", &format!("gui/{uid}/{AGENT_LABEL}")])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Default per-user agent plist path (SPEC §3.2), given the user's home dir.
pub fn default_agent_plist(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{AGENT_LABEL}.plist"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loaded_toggles_to_bootout() {
        let args = toggle_args(
            true,
            501,
            Path::new("/Users/x/Library/LaunchAgents/a.plist"),
        );
        assert_eq!(
            args,
            vec!["bootout".to_string(), format!("gui/501/{AGENT_LABEL}")]
        );
    }

    #[test]
    fn unloaded_toggles_to_bootstrap_with_the_plist_path() {
        let plist = Path::new("/Users/x/Library/LaunchAgents/a.plist");
        let args = toggle_args(false, 501, plist);
        assert_eq!(
            args,
            vec![
                "bootstrap".to_string(),
                "gui/501".to_string(),
                "/Users/x/Library/LaunchAgents/a.plist".to_string(),
            ]
        );
    }

    #[test]
    fn default_agent_plist_matches_the_installer_layout() {
        let path = default_agent_plist(Path::new("/Users/alice"));
        assert_eq!(
            path,
            PathBuf::from(format!(
                "/Users/alice/Library/LaunchAgents/{AGENT_LABEL}.plist"
            ))
        );
    }
}
