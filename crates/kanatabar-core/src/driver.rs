//! Pure parser for `systemextensionsctl list` (SPEC §6.5 driver preflight
//! [HARD]).
//!
//! Decides whether the Karabiner-DriverKit-VirtualHIDDevice system extension is
//! `activated enabled`. Running the command is I/O (the daemon's job); this
//! module parses its text so every state is unit-testable on captured output.

/// The Karabiner DriverKit extension's state in the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverState {
    /// `[activated enabled]` — ready to use.
    ActivatedEnabled,
    /// Present but not fully active (e.g. `activated waiting for user`,
    /// `terminated waiting to uninstall on reboot`). Carries the raw state.
    Present {
        /// The bracketed state text.
        state: String,
    },
    /// The extension is not listed at all.
    NotFound,
}

impl DriverState {
    /// Whether the driver is ready (`activated enabled`).
    pub fn is_ready(&self) -> bool {
        matches!(self, DriverState::ActivatedEnabled)
    }

    /// A user-facing, actionable message for a non-ready state (SPEC §15).
    pub fn message(&self) -> String {
        match self {
            DriverState::ActivatedEnabled => "Karabiner driver activated".to_string(),
            DriverState::Present { state } => format!(
                "Karabiner driver present but not enabled ([{state}]) — approve it in \
                 System Settings → Privacy & Security, or run the Setup Wizard"
            ),
            DriverState::NotFound => {
                "Karabiner-DriverKit-VirtualHIDDevice not installed — run the Setup Wizard"
                    .to_string()
            }
        }
    }
}

/// The bundle/name substring identifying the Karabiner virtual HID extension.
const KARABINER_EXTENSION: &str = "Karabiner-DriverKit-VirtualHIDDevice";

/// The installed Karabiner driver **extension-bundle** version from
/// `systemextensionsctl list` — the bundle column reads `org.pqrs.… (x.y.z/x.y.z)`;
/// the first number is taken. `None` when the extension is absent or the
/// column is unparseable.
///
/// ⚠️ HW finding (2026-07-11): this is the `.dext` **bundle** version (e.g.
/// `1.8.0`), which pqrs versions **independently of the pkg/release version**
/// (v6.x/v8.x) that kanata release notes reference. The two must not be
/// compared against each other.
pub fn parse_driver_version(output: &str) -> Option<crate::kanata::Version> {
    let line = output.lines().find(|l| l.contains(KARABINER_EXTENSION))?;
    let open = line.find('(')?;
    let rest = &line[open + 1..];
    let end = rest.find(['/', ')'])?;
    crate::kanata::parse_version(&rest[..end])
}

/// The Karabiner driver **extension-bundle major** a given kanata release
/// supports (SPEC §2 version coupling). Currently always `None` ("unknown —
/// don't judge"): kanata release notes pin the **pkg** version (e.g. v6.2.0),
/// but `systemextensionsctl` reports the independently-versioned **bundle**
/// (e.g. 1.8.0 — verified on HW 2026-07-11), and no reliable pkg↔bundle
/// mapping is known. Add verified *bundle*-version pairs here if/when pqrs
/// documents them; until then the doctor `driver version` check is
/// report-only.
pub fn supported_driver_major(_kanata: crate::kanata::Version) -> Option<u32> {
    None
}

/// Parse `systemextensionsctl list` output for the Karabiner extension's state.
///
/// Each extension row ends with a bracketed state such as `[activated enabled]`;
/// we find the Karabiner row and read that bracket. [VERIFY] the exact column
/// layout across macOS / `systemextensionsctl` versions.
pub fn parse_systemextensions(output: &str) -> DriverState {
    for line in output.lines() {
        if !line.contains(KARABINER_EXTENSION) {
            continue;
        }
        return match (line.rfind('['), line.rfind(']')) {
            (Some(open), Some(close)) if open < close => {
                let state = line[open + 1..close].trim().to_string();
                if state == "activated enabled" {
                    DriverState::ActivatedEnabled
                } else {
                    DriverState::Present { state }
                }
            }
            // Karabiner row present but no bracketed state we recognize.
            _ => DriverState::Present {
                state: String::new(),
            },
        };
    }
    DriverState::NotFound
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured `systemextensionsctl list` outputs (SPEC §6.5, §19 gate).

    const ACTIVATED: &str = "\
1 extension(s)
--- com.apple.system_extension.driver_extension
enabled\tactive\tteamID\tbundleID (version)\tname\t[state]
*\t*\tX2WJ2XB2YT\torg.pqrs.Karabiner-DriverKit-VirtualHIDDevice (5.0.0/5.0.0)\tKarabiner-DriverKit-VirtualHIDDevice\t[activated enabled]
";

    const WAITING_FOR_USER: &str = "\
1 extension(s)
--- com.apple.system_extension.driver_extension
enabled\tactive\tteamID\tbundleID (version)\tname\t[state]
\t\tX2WJ2XB2YT\torg.pqrs.Karabiner-DriverKit-VirtualHIDDevice (5.0.0/5.0.0)\tKarabiner-DriverKit-VirtualHIDDevice\t[activated waiting for user]
";

    const TERMINATED: &str = "\
1 extension(s)
--- com.apple.system_extension.driver_extension
enabled\tactive\tteamID\tbundleID (version)\tname\t[state]
\t\tX2WJ2XB2YT\torg.pqrs.Karabiner-DriverKit-VirtualHIDDevice (5.0.0/5.0.0)\tKarabiner-DriverKit-VirtualHIDDevice\t[terminated waiting to uninstall on reboot]
";

    const NO_KARABINER: &str = "\
1 extension(s)
--- com.apple.system_extension.driver_extension
enabled\tactive\tteamID\tbundleID (version)\tname\t[state]
*\t*\tABCDE12345\tcom.example.OtherDriver (1.0/1.0)\tOtherDriver\t[activated enabled]
";

    const EMPTY: &str = "0 extension(s)\n";

    #[test]
    fn detects_activated_enabled() {
        assert_eq!(
            parse_systemextensions(ACTIVATED),
            DriverState::ActivatedEnabled
        );
        assert!(parse_systemextensions(ACTIVATED).is_ready());
    }

    #[test]
    fn detects_waiting_for_user() {
        let state = parse_systemextensions(WAITING_FOR_USER);
        assert_eq!(
            state,
            DriverState::Present {
                state: "activated waiting for user".into()
            }
        );
        assert!(!state.is_ready());
        assert!(state.message().contains("Privacy & Security"));
    }

    #[test]
    fn detects_terminated() {
        assert_eq!(
            parse_systemextensions(TERMINATED),
            DriverState::Present {
                state: "terminated waiting to uninstall on reboot".into()
            }
        );
    }

    #[test]
    fn other_driver_is_not_found() {
        assert_eq!(parse_systemextensions(NO_KARABINER), DriverState::NotFound);
        assert!(!parse_systemextensions(NO_KARABINER).is_ready());
    }

    #[test]
    fn empty_list_is_not_found() {
        assert_eq!(parse_systemextensions(EMPTY), DriverState::NotFound);
    }

    #[test]
    fn parses_driver_version_from_bundle_column() {
        use crate::kanata::Version;
        assert_eq!(parse_driver_version(ACTIVATED), Some(Version::new(5, 0, 0)));
        // Version present even when the extension isn't enabled yet.
        assert_eq!(
            parse_driver_version(WAITING_FOR_USER),
            Some(Version::new(5, 0, 0))
        );
        assert_eq!(parse_driver_version(NO_KARABINER), None);
        assert_eq!(parse_driver_version(EMPTY), None);
    }

    #[test]
    fn driver_coupling_is_report_only_until_a_bundle_mapping_is_verified() {
        use crate::kanata::Version;
        // HW 2026-07-11: systemextensionsctl reports the .dext *bundle*
        // version (1.8.0), not the pkg version kanata release notes pin
        // (v6.x/v8.x) — so no version may be judged incompatible yet.
        assert_eq!(supported_driver_major(Version::new(1, 12, 0)), None);
        assert_eq!(supported_driver_major(Version::new(1, 8, 1)), None);
    }

    /// The real `systemextensionsctl list` captured on hardware (2026-07-11):
    /// tab-separated, bundle version `1.8.0` while the installed pkg was a
    /// July-2026 release — the bundle≠pkg fact the coupling rule respects.
    #[test]
    fn parses_the_captured_hw_output() {
        const HW_CAPTURE: &str = "\
1 extension(s)
--- com.apple.system_extension.driver_extension (Go to 'System Settings > General > Login Items & Extensions > Driver Extensions' to modify these system extension(s))
enabled\tactive\tteamID\tbundleID (version)\tname\t[state]
*\t*\tG43BCU2T37\torg.pqrs.Karabiner-DriverKit-VirtualHIDDevice (1.8.0/1.8.0)\torg.pqrs.Karabiner-DriverKit-VirtualHIDDevice\t[activated enabled]
";
        assert_eq!(
            parse_systemextensions(HW_CAPTURE),
            DriverState::ActivatedEnabled
        );
        assert_eq!(
            parse_driver_version(HW_CAPTURE),
            Some(crate::kanata::Version::new(1, 8, 0))
        );
    }
}
