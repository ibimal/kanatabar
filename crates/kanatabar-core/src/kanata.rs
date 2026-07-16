//! Pure parsers for kanata's CLI/TCP surface (SPEC §2, §6.5).
//!
//! - `kanata --version` output → a comparable version, for the known-good-floor
//!   warning.
//! - kanata TCP NDJSON messages → layer changes, for the live layer relay.
//!
//! Running kanata and the TCP socket are I/O (the daemon); parsing is pure.
//! [VERIFY] the exact version format and TCP message set against the installed
//! kanata version.

/// A semantic-ish version triple parsed from `kanata --version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    /// Major.
    pub major: u32,
    /// Minor.
    pub minor: u32,
    /// Patch.
    pub patch: u32,
}

impl Version {
    /// Construct a version.
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Extract the first `x.y.z` version from `kanata --version` output, e.g.
/// `kanata 1.7.0` → `1.7.0`. Missing patch defaults to 0.
pub fn parse_version(output: &str) -> Option<Version> {
    for token in output.split(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        let token = token.trim_start_matches('v');
        let mut parts = token.split('.');
        // Skip tokens that don't start with a `major.minor` numeric pair.
        let Some(major) = parse_u32(parts.next()) else {
            continue;
        };
        let Some(minor) = parts.next().and_then(|m| parse_u32(Some(m))) else {
            continue;
        };
        let patch = parts.next().and_then(|p| parse_u32(Some(p))).unwrap_or(0);
        return Some(Version::new(major, minor, patch));
    }
    None
}

fn parse_u32(part: Option<&str>) -> Option<u32> {
    let s = part?;
    if s.is_empty() {
        return None;
    }
    // Stop at the first non-digit (handles "0-beta", "1.7.0-pre").
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Well-known kanata install locations, in resolution order (SPEC §7.3):
/// `/usr/local/bin` (Intel Homebrew, manual installs — and the historical
/// KanataBar default) before `/opt/homebrew/bin` (Apple-silicon Homebrew).
/// A **fixed allowlist, never `$PATH`** [HARD, §14]: the root daemon executes
/// this binary as root, and `$PATH` is attacker-influenceable (launchd's
/// default PATH wouldn't contain the brew prefix anyway). The order is
/// deterministic so the resolved binary — and therefore its TCC Input
/// Monitoring grant (SPEC §2) — is stable across restarts.
pub const KANATA_BIN_CANDIDATES: [&str; 2] = ["/usr/local/bin/kanata", "/opt/homebrew/bin/kanata"];

/// kanata locations that are **not** auto-trusted for root execution but are
/// common enough to name in diagnostics: `cargo install kanata` lands in
/// `~/.cargo/bin` and MacPorts in `/opt/local/bin`. The daemon probes these
/// only to turn an unhelpful "not found" into "found at X — point KanataBar at
/// it" guidance; it never *runs* them without an explicit `kanata_bin` in
/// config.toml, because executing a user-writable path as root would break the
/// §14 invariant (the user setting `kanata_bin` is their informed opt-in).
///
/// Pure (no I/O): returns the candidate paths for `home`; the caller probes.
pub fn alt_kanata_locations(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    vec![
        home.join(".cargo/bin/kanata"),
        std::path::PathBuf::from("/opt/local/bin/kanata"),
    ]
}

/// kanata's conventional per-user config directory, `~/.config/kanata`, where
/// existing kanata users keep their `.kbd` files. KanataBar doesn't read from
/// here itself (presets point wherever the user likes) but scans it to help a
/// new user turn an existing config into a preset. Pure (no I/O).
pub fn kanata_config_dir(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".config/kanata")
}

/// A fault recognizable in kanata's output (SPEC §2, §6.5): conditions a
/// respawn cannot fix, so the supervisor should go `Degraded` with the right
/// hint instead of burning the retry budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrFault {
    /// macOS TCC denied device access — Input Monitoring not granted (or
    /// silently invalidated by a binary update, SPEC §2).
    PermissionDenied,
    /// Another process holds the device exclusively (second remapper).
    DeviceInUse,
    /// kanata's TCP port is taken (another kanata/kanata-tray, or a leftover
    /// process) — kanata panics with `AddrInUse` at startup (seen on HW
    /// 2026-07-11); respawning cannot fix it while the squatter lives.
    PortInUse,
}

/// Classify one line of kanata output for a give-up-worthy fault, or `None`.
///
/// Patterns from real kanata errors ([VERIFY] against each release):
/// - Input Monitoring missing (jtroo/kanata#1037):
///   `IOHIDDeviceOpen error: (iokit/common) privilege violation`
/// - device held by another remapper: "exclusive access"/"already open"
///   (SPEC §2: a second kanata fails with "device already open/in use").
pub fn classify_fault_line(line: &str) -> Option<StderrFault> {
    let lower = line.to_ascii_lowercase();
    // Older kanata (#1037): "IOHIDDeviceOpen error: (iokit/common) privilege
    // violation". kanata v1.12.0 (HW capture 2026-07-11): "failed to open
    // keyboard device(s): kanata needs macOS Input Monitoring permission. …"
    if lower.contains("privilege violation")
        || lower.contains("not permitted")
        || lower.contains("input monitoring permission")
    {
        return Some(StderrFault::PermissionDenied);
    }
    if lower.contains("exclusive access") || lower.contains("already open") {
        return Some(StderrFault::DeviceInUse);
    }
    // HW capture 2026-07-11 (kanata v1.12.0 panic):
    // `TCP server starts: Os { code: 48, kind: AddrInUse, message: "Address already in use" }`
    if lower.contains("address already in use") || lower.contains("addrinuse") {
        return Some(StderrFault::PortInUse);
    }
    None
}

/// A live output-backend health transition read from kanata's log stream
/// (SPEC §6.5). Unlike [`StderrFault`] these are not exit-adjacent: kanata
/// stays alive, releases the input devices (keys pass through UNREMAPPED),
/// and keeps retrying — so the supervisor tracks them on the *running* child
/// and can recover to `Running` without a respawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendEvent {
    /// The DriverKit virtual keyboard is unreachable and kanata released the
    /// input devices (HW 2026-07-11: a driver-pkg/kanata protocol mismatch —
    /// v8.0.0 with kanata 1.12.0 — presents exactly like this, forever).
    Down,
    /// The backend (and console session) came back; kanata re-grabs and
    /// remapping resumes.
    Up,
}

/// Classify one line of kanata output for a live backend transition, or `None`.
///
/// Patterns verbatim from kanata v1.12.0 (HW capture + binary strings,
/// 2026-07-11; [VERIFY] against each release):
/// - down: `output backend not ready after 10s. Key output may fail until the
///   backend recovers.` and `output backend unavailable — releasing input
///   devices` (kanata's own 10s wait precedes these, so they are already
///   debounced signals, not first-blip noise).
/// - up: `output backend and console session ready — re-grabbing input devices`.
///
/// Deliberately NOT matched:
/// - `console session paused (lock/user-switch) — releasing input devices` —
///   kanata releases devices on every screen lock; normal, self-recovering.
/// - `output backend unavailable during write` / `(will recover)` — transient
///   write blips kanata absorbs without releasing the devices.
pub fn classify_backend_line(line: &str) -> Option<BackendEvent> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("output backend and console session ready") {
        return Some(BackendEvent::Up);
    }
    if lower.contains("output backend not ready after")
        || (lower.contains("output backend unavailable") && lower.contains("releasing input"))
    {
        return Some(BackendEvent::Down);
    }
    None
}

/// VHID driver-status lines kanata prints raw (no `[LEVEL] timestamp` prefix)
/// while connected to the Karabiner VHID driver. `virtual_hid_keyboard_ready`
/// repeats ~once per second forever (HW capture 2026-07-13: 50k of 55k lines
/// in `kanatad.err.log`), which would drown the INFO log and the `GetLogs`
/// ring; the rest are the one-shot startup handshake in the same family.
/// Matched **exactly** (after trimming) so a genuine `[INFO]`-prefixed kanata
/// line — or anything else — can never be mistaken for heartbeat noise.
const VHID_STATUS_NOISE: [&str; 6] = [
    "virtual_hid_keyboard_ready true",
    "virtual_hid_keyboard_ready false",
    "connected",
    "driver activated: true",
    "driver connected: true",
    "driver version matched: true",
];

/// Is this kanata output line VHID-status heartbeat noise (SPEC §6.6)?
///
/// True only for the exact raw driver-status lines above — callers relay
/// these at DEBUG instead of INFO. Classification (faults, backend health)
/// still runs on every line regardless; only log verbosity is affected.
pub fn is_vhid_status_noise(line: &str) -> bool {
    VHID_STATUS_NOISE.contains(&line.trim())
}

/// The active layer named by a kanata TCP `LayerChange` message, or `None` for
/// any other/unparseable message (SPEC §2 example
/// `{"LayerChange":{"new":"<layer>"}}`).
pub fn parse_layer_change(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    value
        .get("LayerChange")?
        .get("new")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_locations_include_cargo_and_macports() {
        let alts = alt_kanata_locations(std::path::Path::new("/Users/alice"));
        assert!(alts.contains(&std::path::PathBuf::from("/Users/alice/.cargo/bin/kanata")));
        assert!(alts.contains(&std::path::PathBuf::from("/opt/local/bin/kanata")));
        // Never overlaps the auto-trusted allowlist (those are handled first).
        for alt in &alts {
            assert!(!KANATA_BIN_CANDIDATES.contains(&alt.to_str().unwrap()));
        }
    }

    #[test]
    fn kanata_config_dir_is_the_xdg_convention() {
        assert_eq!(
            kanata_config_dir(std::path::Path::new("/Users/alice")),
            std::path::PathBuf::from("/Users/alice/.config/kanata")
        );
    }

    #[test]
    fn parses_plain_version() {
        assert_eq!(parse_version("kanata 1.7.0"), Some(Version::new(1, 7, 0)));
    }

    #[test]
    fn parses_version_with_trailing_text() {
        assert_eq!(
            parse_version("kanata 1.8.1 (some build info)"),
            Some(Version::new(1, 8, 1))
        );
    }

    #[test]
    fn parses_two_component_version() {
        assert_eq!(parse_version("kanata 1.9"), Some(Version::new(1, 9, 0)));
    }

    #[test]
    fn parses_prerelease_patch() {
        assert_eq!(
            parse_version("kanata 1.7.0-prerelease"),
            Some(Version::new(1, 7, 0))
        );
    }

    #[test]
    fn no_version_returns_none() {
        assert_eq!(parse_version("kanata unknown"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn version_ordering_supports_floor() {
        let floor = Version::new(1, 7, 0);
        assert!(parse_version("kanata 1.6.0").unwrap() < floor);
        assert!(parse_version("kanata 1.7.0").unwrap() >= floor);
        assert!(parse_version("kanata 1.8.1").unwrap() >= floor);
    }

    #[test]
    fn classifies_input_monitoring_denial() {
        // Verbatim from jtroo/kanata#1037.
        assert_eq!(
            classify_fault_line("IOHIDDeviceOpen error: (iokit/common) privilege violation"),
            Some(StderrFault::PermissionDenied)
        );
        assert_eq!(
            classify_fault_line("IOHIDDeviceOpen error: (iokit/common) not permitted"),
            Some(StderrFault::PermissionDenied)
        );
        // Verbatim from kanata v1.12.0, captured on HW 2026-07-11 — the
        // friendly wording contains none of the older patterns.
        assert_eq!(
            classify_fault_line(
                "failed to open keyboard device(s): kanata needs macOS Input Monitoring \
                 permission. Enable kanata in System Settings -> Privacy & Security -> \
                 Input Monitoring, then re-run kanata."
            ),
            Some(StderrFault::PermissionDenied)
        );
    }

    #[test]
    fn classifies_device_in_use() {
        assert_eq!(
            classify_fault_line("IOHIDDeviceOpen error: exclusive access and device already open"),
            Some(StderrFault::DeviceInUse)
        );
    }

    #[test]
    fn classifies_tcp_port_conflict() {
        // Verbatim from kanata v1.12.0's panic, captured on HW 2026-07-11.
        assert_eq!(
            classify_fault_line(
                r#"TCP server starts: Os { code: 48, kind: AddrInUse, message: "Address already in use" }"#
            ),
            Some(StderrFault::PortInUse)
        );
        // "already in use" for a *device* must not classify as a port issue
        // (it contains neither "address" nor "AddrInUse").
        assert_eq!(
            classify_fault_line("device already in use by another process"),
            None
        );
    }

    #[test]
    fn ordinary_output_is_not_a_fault() {
        assert_eq!(classify_fault_line("kanata v1.12.0 starting"), None);
        // The generic follow-up line alone is not specific enough to classify.
        assert_eq!(
            classify_fault_line("failed to open keyboard device(s): Couldn't register any device"),
            None
        );
        assert_eq!(classify_fault_line(""), None);
    }

    #[test]
    fn classifies_backend_release_as_down() {
        // Verbatim from kanata v1.12.0 stdout, captured on HW 2026-07-11
        // (driver pkg v8.0.0 protocol mismatch — kanata alive, keys unmapped).
        assert_eq!(
            classify_backend_line("output backend unavailable — releasing input devices"),
            Some(BackendEvent::Down)
        );
        assert_eq!(
            classify_backend_line(
                "output backend not ready after 10s. Key output may fail until the \
                 backend recovers."
            ),
            Some(BackendEvent::Down)
        );
        // The real HW release line combines "during write" with the release
        // clause. Unlike the bare "during write" transient blip (which is
        // None, see `transient_write_blips_are_not_backend_events`), this one
        // releases the devices, so it must classify as Down.
        assert_eq!(
            classify_backend_line(
                "output backend unavailable during write — releasing input devices"
            ),
            Some(BackendEvent::Down)
        );
    }

    #[test]
    fn classifies_backend_recovery_as_up() {
        // From kanata v1.12.0 binary strings (target `output-backend-recovery`).
        assert_eq!(
            classify_backend_line(
                "output backend and console session ready — re-grabbing input devices"
            ),
            Some(BackendEvent::Up)
        );
    }

    #[test]
    fn screen_lock_release_is_not_a_backend_event() {
        // kanata releases devices on every screen lock / fast user switch —
        // normal and self-recovering, never a Degraded trigger.
        assert_eq!(
            classify_backend_line(
                "console session paused (lock/user-switch) — releasing input devices"
            ),
            None
        );
        assert_eq!(classify_backend_line("console session restored"), None);
    }

    #[test]
    fn transient_write_blips_are_not_backend_events() {
        // kanata absorbs these without releasing the devices.
        assert_eq!(
            classify_backend_line("output backend unavailable during write"),
            None
        );
        assert_eq!(
            classify_backend_line("+: output backend unavailable (will recover)"),
            None
        );
    }

    #[test]
    fn ordinary_output_is_not_a_backend_event() {
        assert_eq!(
            classify_backend_line("keyboard grabbed, entering event processing loop"),
            None
        );
        assert_eq!(
            classify_backend_line("Waiting for DriverKit virtual keyboard... (2.0s/10.0s)"),
            None
        );
        assert_eq!(classify_backend_line(""), None);
    }

    #[test]
    fn vhid_status_heartbeat_is_noise() {
        // Verbatim raw (unprefixed) driver-status lines from kanata v1.12.0,
        // captured on HW 2026-07-13 (~1/s while connected to the VHID driver).
        assert!(is_vhid_status_noise("virtual_hid_keyboard_ready true"));
        assert!(is_vhid_status_noise("virtual_hid_keyboard_ready false"));
        assert!(is_vhid_status_noise("connected"));
        assert!(is_vhid_status_noise("driver activated: true"));
        assert!(is_vhid_status_noise("driver connected: true"));
        assert!(is_vhid_status_noise("driver version matched: true"));
        // Trailing whitespace from the pipe must not defeat the match.
        assert!(is_vhid_status_noise("virtual_hid_keyboard_ready true\r"));
    }

    #[test]
    fn real_log_lines_are_not_noise() {
        // kanata's genuine log lines carry a timestamp + [LEVEL] prefix.
        assert!(!is_vhid_status_noise(
            "17:05:04.2338 [INFO] Starting kanata proper"
        ));
        // Backend-health and fault lines must never be downgraded.
        assert!(!is_vhid_status_noise(
            "output backend unavailable — releasing input devices"
        ));
        assert!(!is_vhid_status_noise(
            "output backend and console session ready — re-grabbing input devices"
        ));
        assert!(!is_vhid_status_noise(
            "console session paused (lock/user-switch) — releasing input devices"
        ));
        // Unexpected driver states are diagnostic, not heartbeat.
        assert!(!is_vhid_status_noise("driver activated: false"));
        assert!(!is_vhid_status_noise("driver version matched: false"));
        assert!(!is_vhid_status_noise(""));
    }

    #[test]
    fn parses_layer_change() {
        assert_eq!(
            parse_layer_change(r#"{"LayerChange":{"new":"nav"}}"#),
            Some("nav".to_string())
        );
    }

    #[test]
    fn ignores_other_messages() {
        assert_eq!(
            parse_layer_change(r#"{"LayerNames":{"names":["base"]}}"#),
            None
        );
        assert_eq!(parse_layer_change("not json"), None);
        assert_eq!(parse_layer_change(r#"{"LayerChange":{}}"#), None);
    }
}
