//! Pure rules for Karabiner VirtualHIDDevice-daemon supervision (SPEC §6.5a).
//!
//! The driver pkg installs the daemon binary but registers **no LaunchDaemon**
//! (SPEC §2, verified against pqrs-org docs): unless something keeps it
//! running, every reboot leaves kanata dead. KanataBar supervises it via its
//! own LaunchDaemon — but only when nothing else already does (Karabiner-
//! Elements, or a user-made plist such as the community
//! `org.pqrs.karabiner-daemon`). **Never run a second instance** [HARD].
//!
//! Running `launchctl` and stat'ing the daemon binary are I/O (the installer's
//! and daemon's job); this module is the parsing and the decision, so every
//! case is unit-testable on captured output.

/// launchd label for KanataBar's own VHID-daemon job (SPEC §6.5a).
pub const VHIDD_LABEL: &str = "io.github.ibimal.kanatabar.vhidd";

/// Where the driver pkg installs the daemon binary (SPEC §2; matches the
/// pqrs-org README's documented invocation, checked 2026-07). [VERIFY] per
/// Karabiner-DriverKit-VirtualHIDDevice release.
pub const VHIDD_BINARY: &str = "/Library/Application Support/org.pqrs/\
     Karabiner-DriverKit-VirtualHIDDevice/Applications/\
     Karabiner-VirtualHIDDevice-Daemon.app/Contents/MacOS/\
     Karabiner-VirtualHIDDevice-Daemon";

/// The extension-activation manager the driver pkg installs (hidden in
/// /Applications — the dot is intentional). The pqrs README's documented
/// activation is `<MANAGER_BINARY> activate`, run **without** sudo in the
/// logged-in user's session (checked 2026-07); the tray wizard runs it
/// (SPEC §11 step: activate, then approve in System Settings). [VERIFY] per
/// release.
pub const MANAGER_BINARY: &str = "/Applications/\
     .Karabiner-VirtualHIDDevice-Manager.app/Contents/MacOS/\
     Karabiner-VirtualHIDDevice-Manager";

/// Who (if anyone) keeps the VHID daemon alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VhiddManagement {
    /// KanataBar's own LaunchDaemon ([`VHIDD_LABEL`]) is registered.
    Ours,
    /// Some other Karabiner-related launchd job exists (Karabiner-Elements'
    /// services, or a user plist like `org.pqrs.karabiner-daemon`). Carries
    /// the first matching label.
    Other(String),
    /// Nothing supervises the daemon — the reboot-leaves-kanata-dead case
    /// `kanatactl install` fixes (SPEC §6.5a).
    Unmanaged,
}

impl VhiddManagement {
    /// A user-facing one-liner for `doctor` (SPEC §15).
    pub fn describe(&self) -> String {
        match self {
            Self::Ours => "managed by KanataBar's LaunchDaemon".to_string(),
            Self::Other(label) => format!("managed by `{label}` (not ours; left alone)"),
            Self::Unmanaged => {
                "no LaunchDaemon keeps the Karabiner VirtualHIDDevice daemon running \
                 — it will not survive a reboot"
                    .to_string()
            }
        }
    }
}

/// Extract the label column from `launchctl list` output (header
/// `PID\tStatus\tLabel`, one job per line; labels contain no whitespace).
pub fn parse_launchctl_list(output: &str) -> Vec<String> {
    output
        .lines()
        .skip(1) // header
        .filter_map(|line| line.split_whitespace().nth(2))
        .map(str::to_string)
        .collect()
}

/// Whether a launchd label is a per-process *instance* (`<bundle-id>-0x<token>`)
/// rather than a plist-backed service. DriverKit extensions and app extensions
/// show up in `launchctl list` this way — e.g. the Karabiner driver itself as
/// `org.pqrs.Karabiner-DriverKit-VirtualHIDDevice-0x100083453` (seen on HW,
/// 2026-07-11). An instance label is the *driver running*, not something that
/// supervises the VHID daemon, and must not count as management.
fn is_instance_label(label: &str) -> bool {
    label
        .rsplit_once("-0x")
        .is_some_and(|(_, token)| !token.is_empty() && token.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Classify who manages the VHID daemon from the system-domain launchd labels
/// (SPEC §6.5a): our own label wins, else any plist-backed service label
/// mentioning `karabiner` (case-insensitive — Karabiner-Elements services and
/// community plists all match; the driver extension's own `-0x…` process
/// instance does **not**), else unmanaged.
pub fn classify<'a>(labels: impl IntoIterator<Item = &'a str>) -> VhiddManagement {
    let mut other = None;
    for label in labels {
        if label == VHIDD_LABEL {
            return VhiddManagement::Ours;
        }
        if other.is_none()
            && !is_instance_label(label)
            && label.to_ascii_lowercase().contains("karabiner")
        {
            other = Some(label.to_string());
        }
    }
    match other {
        Some(label) => VhiddManagement::Other(label),
        None => VhiddManagement::Unmanaged,
    }
}

/// Whether `kanatactl install` should (re)write and bootstrap our vhidd
/// LaunchDaemon (SPEC §6.5a): yes when unmanaged (first install) or already
/// ours (refresh); never when someone else manages the daemon, and never
/// without the daemon binary on disk (driver not installed yet — the wizard
/// step comes first).
pub fn should_install(management: &VhiddManagement, daemon_binary_present: bool) -> bool {
    daemon_binary_present
        && matches!(
            management,
            VhiddManagement::Ours | VhiddManagement::Unmanaged
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAUNCHCTL_KE: &str = "\
PID	Status	Label
1	0	com.apple.launchd
543	0	org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon
-	0	com.example.unrelated
";

    const LAUNCHCTL_COMMUNITY: &str = "\
PID	Status	Label
1	0	com.apple.launchd
812	0	org.pqrs.karabiner-daemon
";

    const LAUNCHCTL_OURS: &str = "\
PID	Status	Label
1	0	com.apple.launchd
900	0	io.github.ibimal.kanatabar.vhidd
543	0	org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon
";

    const LAUNCHCTL_NONE: &str = "\
PID	Status	Label
1	0	com.apple.launchd
-	0	com.example.unrelated
";

    fn classify_output(output: &str) -> VhiddManagement {
        let labels = parse_launchctl_list(output);
        classify(labels.iter().map(String::as_str))
    }

    #[test]
    fn parses_labels_from_launchctl_list() {
        assert_eq!(
            parse_launchctl_list(LAUNCHCTL_COMMUNITY),
            vec!["com.apple.launchd", "org.pqrs.karabiner-daemon"]
        );
        assert!(parse_launchctl_list("PID\tStatus\tLabel\n").is_empty());
        assert!(parse_launchctl_list("").is_empty());
    }

    #[test]
    fn karabiner_elements_job_is_other() {
        assert_eq!(
            classify_output(LAUNCHCTL_KE),
            VhiddManagement::Other(
                "org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon".into()
            )
        );
    }

    #[test]
    fn community_plist_is_other() {
        assert_eq!(
            classify_output(LAUNCHCTL_COMMUNITY),
            VhiddManagement::Other("org.pqrs.karabiner-daemon".into())
        );
    }

    #[test]
    fn our_label_wins_even_with_karabiner_labels_present() {
        assert_eq!(classify_output(LAUNCHCTL_OURS), VhiddManagement::Ours);
    }

    #[test]
    fn no_karabiner_job_is_unmanaged() {
        assert_eq!(classify_output(LAUNCHCTL_NONE), VhiddManagement::Unmanaged);
    }

    /// HW finding (2026-07-11): the DriverKit extension itself appears in
    /// `launchctl list` as a `-0x…` process instance. That is the *driver
    /// running*, not daemon management — treating it as management made
    /// `kanatactl install` skip our vhidd job and left the daemon down.
    #[test]
    fn driver_extension_instance_is_not_management() {
        const LAUNCHCTL_EXTENSION_ONLY: &str = "\
PID	Status	Label
1	0	com.apple.launchd
2843	0	org.pqrs.Karabiner-DriverKit-VirtualHIDDevice-0x100083453
";
        assert_eq!(
            classify_output(LAUNCHCTL_EXTENSION_ONLY),
            VhiddManagement::Unmanaged
        );
        // A real (plist-backed) manager alongside the instance still counts.
        const LAUNCHCTL_BOTH: &str = "\
PID	Status	Label
2843	0	org.pqrs.Karabiner-DriverKit-VirtualHIDDevice-0x100083453
812	0	org.pqrs.karabiner-daemon
";
        assert_eq!(
            classify_output(LAUNCHCTL_BOTH),
            VhiddManagement::Other("org.pqrs.karabiner-daemon".into())
        );
    }

    #[test]
    fn install_only_when_unmanaged_or_ours_and_binary_present() {
        // Never a second instance [HARD]: someone else manages it → hands off.
        assert!(!should_install(&VhiddManagement::Other("x".into()), true));
        // Unmanaged + binary present → install; refresh our own too.
        assert!(should_install(&VhiddManagement::Unmanaged, true));
        assert!(should_install(&VhiddManagement::Ours, true));
        // Driver not installed yet → nothing to run; skip.
        assert!(!should_install(&VhiddManagement::Unmanaged, false));
    }

    #[test]
    fn descriptions_are_actionable() {
        assert!(VhiddManagement::Unmanaged.describe().contains("reboot"));
        assert!(VhiddManagement::Other("org.pqrs.karabiner-daemon".into())
            .describe()
            .contains("org.pqrs.karabiner-daemon"));
    }
}
