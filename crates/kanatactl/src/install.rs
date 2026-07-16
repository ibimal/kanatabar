//! `kanatactl install`/`uninstall` (SPEC §9, §10).
//!
//! Writes the launchd job plists, copies the daemon/CLI/tray binaries to
//! `/usr/local/bin`, creates `/Library/Logs/KanataBar` (launchd does not
//! create `StandardOutPath`'s parent directory, so it must exist before the
//! job is bootstrapped), and bootstraps/boots out the jobs via `launchctl`.
//!
//! Real installs run as root (`sudo kanatactl install`, SPEC §9). `prefix` and
//! `skip_launchctl` on [`InstallConfig`] let tests exercise the exact same
//! file-layout logic against a temp directory, unprivileged, without a real
//! launchd — the same injectable-for-tests house style as
//! `KANATABAR_SOCK`/`KANATABAR_STATE` (CLAUDE.md).
//!
//! Binaries are expected alongside the running `kanatactl` (e.g. the same
//! `cargo build --workspace` output directory); Phase 9 packaging places them
//! via the pkg payload instead and does not need this lookup.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// launchd label for the root daemon job (SPEC §3.2, §10).
pub const DAEMON_LABEL: &str = "io.github.ibimal.kanatabar.daemon";
/// The app bundle the pkg installs (SPEC §12); absent on tarball installs.
pub const APP_BUNDLE: &str = "/Applications/KanataBar.app";
/// The tray binary inside the bundle — the agent's program when bundled.
pub const BUNDLED_TRAY: &str = "/Applications/KanataBar.app/Contents/MacOS/kanatabar-tray";
/// The tray binary on tarball installs (`kanatactl install` copies it there).
pub const UNBUNDLED_TRAY: &str = "/usr/local/bin/kanatabar-tray";
/// KanataBar's bundle identifier (SPEC §21.1).
pub const BUNDLE_ID: &str = "io.github.ibimal.kanatabar";
/// launchd label for the per-user tray job (SPEC §3.2, §10).
pub const AGENT_LABEL: &str = "io.github.ibimal.kanatabar.agent";
/// launchd label for our Karabiner VHID-daemon job (SPEC §6.5a); the single
/// source is `kanatabar_core::vhidd`.
pub use kanatabar_core::vhidd::VHIDD_LABEL;

const DAEMON_PLIST: &str =
    include_str!("../../../resources/launchd/io.github.ibimal.kanatabar.daemon.plist");
const AGENT_PLIST_TEMPLATE: &str =
    include_str!("../../../resources/launchd/io.github.ibimal.kanatabar.agent.plist");
const VHIDD_PLIST: &str =
    include_str!("../../../resources/launchd/io.github.ibimal.kanatabar.vhidd.plist");

/// Render the per-user agent plist, substituting the `__HOME__` placeholder
/// with the target user's home directory and `__TRAY_BIN__` with the tray
/// program to run ([`BUNDLED_TRAY`] on pkg installs, [`UNBUNDLED_TRAY`] on
/// tarball installs — SPEC §12). Pure so it's unit-testable without touching
/// the filesystem; `/Library/Logs/KanataBar` is root-owned (SPEC §3.2) so the
/// (unprivileged) agent logs under the user's own home instead.
pub fn render_agent_plist(home: &str, tray_bin: &str) -> String {
    AGENT_PLIST_TEMPLATE
        .replace("__HOME__", home)
        .replace("__TRAY_BIN__", tray_bin)
}

/// Which component(s) an install/uninstall touches (`--daemon-only`/
/// `--agent-only`, SPEC §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Daemon,
    Agent,
    Both,
}

impl Component {
    fn wants_daemon(self) -> bool {
        matches!(self, Component::Daemon | Component::Both)
    }

    fn wants_agent(self) -> bool {
        matches!(self, Component::Agent | Component::Both)
    }
}

/// How and where to install/uninstall.
#[derive(Debug, Clone)]
pub struct InstallConfig {
    /// Root prefix for every installed path; `/` in production. Tests point
    /// this at a temp directory so install/uninstall can run unprivileged
    /// without touching the real system.
    pub prefix: PathBuf,
    /// Skip `launchctl bootstrap`/`bootout` — a fake `prefix` has no plist
    /// launchd can actually load. Production always runs it.
    pub skip_launchctl: bool,
    pub component: Component,
}

impl Default for InstallConfig {
    fn default() -> Self {
        Self {
            prefix: PathBuf::from("/"),
            skip_launchctl: false,
            component: Component::Both,
        }
    }
}

/// The user the agent job is installed for.
#[derive(Debug, Clone)]
pub struct TargetUser {
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

/// Resolve the user to install the agent for: `SUDO_UID` (set by `sudo`, the
/// documented invocation, SPEC §9) if present, else the console user (owner
/// of `/dev/console` — SPEC §18, the same fallback the control-socket auth
/// policy uses).
pub fn target_user() -> Result<TargetUser> {
    let uid: u32 = match std::env::var("SUDO_UID") {
        Ok(val) => val.parse().context("parsing SUDO_UID")?,
        Err(_) => fs::metadata("/dev/console")
            .map(|m| m.uid())
            .context("no SUDO_UID and /dev/console is unreadable; run via sudo")?,
    };
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .context("looking up target user")?;
    let user = user.with_context(|| format!("no passwd entry for uid {uid}"))?;
    Ok(TargetUser {
        uid,
        gid: user.gid.as_raw(),
        home: user.dir,
    })
}

/// Report of exactly what `install` touched, for the uninstall audit (SPEC
/// §10, §16: "uninstall leaves nothing").
#[derive(Debug, Default)]
pub struct InstallReport {
    pub created: Vec<PathBuf>,
}

/// Report of exactly what `uninstall` removed.
#[derive(Debug, Default)]
pub struct UninstallReport {
    pub removed: Vec<PathBuf>,
}

struct Paths {
    daemon_bin: PathBuf,
    ctl_bin: PathBuf,
    tray_bin: PathBuf,
    app_bundle: PathBuf,
    daemon_plist: PathBuf,
    vhidd_plist: PathBuf,
    vhidd_binary: PathBuf,
    logs_dir: PathBuf,
    support_dir: PathBuf,
    socket: PathBuf,
}

impl Paths {
    fn new(prefix: &Path) -> Self {
        Self {
            daemon_bin: under(prefix, "/usr/local/bin/kanatad"),
            ctl_bin: under(prefix, "/usr/local/bin/kanatactl"),
            tray_bin: under(prefix, UNBUNDLED_TRAY),
            app_bundle: under(prefix, APP_BUNDLE),
            daemon_plist: under(
                prefix,
                format!("/Library/LaunchDaemons/{DAEMON_LABEL}.plist"),
            ),
            vhidd_plist: under(
                prefix,
                format!("/Library/LaunchDaemons/{VHIDD_LABEL}.plist"),
            ),
            vhidd_binary: under(prefix, kanatabar_core::vhidd::VHIDD_BINARY),
            logs_dir: under(prefix, "/Library/Logs/KanataBar"),
            support_dir: under(prefix, "/Library/Application Support/KanataBar"),
            socket: under(prefix, "/var/run/kanatabar.sock"),
        }
    }
}

/// Join an absolute path onto `prefix`; identity when `prefix` is `/` (SPEC
/// paths are used verbatim in production).
fn under(prefix: &Path, abs: impl AsRef<Path>) -> PathBuf {
    let abs = abs.as_ref();
    if prefix == Path::new("/") {
        abs.to_path_buf()
    } else {
        prefix.join(abs.strip_prefix("/").unwrap_or(abs))
    }
}

fn agent_plist_path(prefix: &Path, home: &Path) -> PathBuf {
    under(
        prefix,
        home.join("Library/LaunchAgents")
            .join(format!("{AGENT_LABEL}.plist")),
    )
}

/// Refuse to touch the real system without root (SPEC §9: `sudo kanatactl
/// install`). A non-default `prefix` is a test/dev invocation and skips this.
fn check_privilege(cfg: &InstallConfig) -> Result<()> {
    if cfg.prefix == Path::new("/") && !nix::unistd::geteuid().is_root() {
        bail!("kanatactl install/uninstall must run as root — try again with sudo");
    }
    Ok(())
}

/// Install the requested component(s): copy binaries, write plists, bootstrap
/// the launchd job(s).
pub fn install(cfg: &InstallConfig) -> Result<InstallReport> {
    check_privilege(cfg)?;
    let paths = Paths::new(&cfg.prefix);
    let mut report = InstallReport::default();

    if cfg.component.wants_daemon() {
        install_binary(&sibling_bin("kanatad")?, &paths.daemon_bin, cfg)?;
        report.created.push(paths.daemon_bin.clone());
        install_binary(&sibling_bin("kanatactl")?, &paths.ctl_bin, cfg)?;
        report.created.push(paths.ctl_bin.clone());

        fs::create_dir_all(&paths.logs_dir)?;
        set_owner_mode(&paths.logs_dir, cfg, 0o755)?;
        report.created.push(paths.logs_dir.clone());

        write_file(&paths.daemon_plist, DAEMON_PLIST, 0o644, cfg)?;
        report.created.push(paths.daemon_plist.clone());

        if !cfg.skip_launchctl {
            // Reinstall/upgrade path (`brew upgrade`, SPEC §13): bootstrap
            // fails with "service already loaded" if a previous install's job
            // is running — and aborting here used to leave the agent half of
            // the install (tray binary, agent plist) stale. Bootout first,
            // best-effort: it fails harmlessly when the job isn't loaded.
            let _ = Command::new(LAUNCHCTL)
                .args(["bootout", &format!("system/{DAEMON_LABEL}")])
                .status();
            bootstrap_with_retry("system", &paths.daemon_plist)?;
        }

        install_vhidd_daemon(cfg, &paths, &mut report)?;
    }

    if cfg.component.wants_agent() {
        let user = target_user()?;
        // Pkg installs place the tray inside /Applications/KanataBar.app
        // (SPEC §12) and the agent runs it from there; tarball installs have
        // no bundle, so copy the bare binary to /usr/local/bin as before.
        let tray_program = if paths
            .app_bundle
            .join("Contents/MacOS/kanatabar-tray")
            .is_file()
        {
            // A bare tray left by an earlier tarball install would sit at
            // /usr/local/bin forever, stale (nothing updates it once the
            // agent runs from the bundle) — and HW 2026-07-16 showed launchd
            // happily running such a leftover. Remove it.
            if paths.tray_bin.symlink_metadata().is_ok() {
                fs::remove_file(&paths.tray_bin)
                    .with_context(|| format!("removing stale {}", paths.tray_bin.display()))?;
            }
            BUNDLED_TRAY
        } else {
            install_binary(&sibling_bin("kanatabar-tray")?, &paths.tray_bin, cfg)?;
            report.created.push(paths.tray_bin.clone());
            UNBUNDLED_TRAY
        };

        let agent_plist = agent_plist_path(&cfg.prefix, &user.home);
        let rendered = render_agent_plist(&user.home.display().to_string(), tray_program);
        write_user_file(&agent_plist, &rendered, 0o644, cfg, &user)?;
        report.created.push(agent_plist.clone());

        if !cfg.skip_launchctl {
            // Same reinstall story as the daemon: replace a loaded agent.
            let _ = Command::new(LAUNCHCTL)
                .args(["bootout", &format!("gui/{}/{AGENT_LABEL}", user.uid)])
                .status();
            bootstrap_with_retry(&format!("gui/{}", user.uid), &agent_plist)?;
        }
    }

    Ok(report)
}

/// Uninstall the requested component(s): boot the launchd job(s) out and
/// remove every path `install` could have created — leave nothing behind
/// (SPEC §10, §16).
pub fn uninstall(cfg: &InstallConfig) -> Result<UninstallReport> {
    check_privilege(cfg)?;
    let paths = Paths::new(&cfg.prefix);
    let mut report = UninstallReport::default();

    if cfg.component.wants_daemon() {
        if !cfg.skip_launchctl {
            // Best-effort: bootout fails if the job isn't loaded, which is
            // fine (e.g. uninstall after a crash, or a repeat uninstall).
            let _ = Command::new(LAUNCHCTL)
                .args(["bootout", &format!("system/{DAEMON_LABEL}")])
                .status();
            // Ours only (§6.5a): never touch pqrs/Karabiner-Elements/user jobs.
            let _ = Command::new(LAUNCHCTL)
                .args(["bootout", &format!("system/{VHIDD_LABEL}")])
                .status();
        }
        remove_path(&paths.vhidd_plist, &mut report)?;
        remove_path(&paths.daemon_plist, &mut report)?;
        remove_path(&paths.daemon_bin, &mut report)?;
        remove_path(&paths.ctl_bin, &mut report)?;
        remove_path(&paths.socket, &mut report)?;
        remove_path(&paths.support_dir, &mut report)?;
        remove_path(&paths.logs_dir, &mut report)?;
    }

    if cfg.component.wants_agent() {
        let user = target_user()?;
        let agent_plist = agent_plist_path(&cfg.prefix, &user.home);
        if !cfg.skip_launchctl {
            let _ = Command::new(LAUNCHCTL)
                .args(["bootout", &format!("gui/{}/{AGENT_LABEL}", user.uid)])
                .status();
        }
        remove_path(&agent_plist, &mut report)?;
        remove_path(&paths.tray_bin, &mut report)?;
        remove_app_bundle(&paths, &mut report)?;
    }

    Ok(report)
}

/// SPEC §6.5a: the Karabiner driver pkg registers no LaunchDaemon for its
/// VirtualHIDDevice daemon, so without help kanata dies on every reboot.
/// Register ours — but only when the daemon binary exists (driver installed)
/// and nothing else supervises it (Karabiner-Elements, a user plist): **never
/// run a second instance** [HARD]. The pure decision lives in
/// `kanatabar_core::vhidd`; this gathers the facts and acts.
fn install_vhidd_daemon(
    cfg: &InstallConfig,
    paths: &Paths,
    report: &mut InstallReport,
) -> Result<()> {
    let labels = system_launchd_labels(cfg);
    let management = kanatabar_core::vhidd::classify(labels.iter().map(String::as_str));
    let binary_present = paths.vhidd_binary.is_file();

    if !kanatabar_core::vhidd::should_install(&management, binary_present) {
        match management {
            kanatabar_core::vhidd::VhiddManagement::Other(label) => {
                println!("VHID daemon already managed by `{label}`; leaving it alone");
            }
            _ if !binary_present => {
                println!(
                    "Karabiner driver not installed (no VHID daemon binary); \
                     run the Setup Wizard, then `sudo kanatactl install` again"
                );
            }
            _ => {}
        }
        return Ok(());
    }

    write_file(&paths.vhidd_plist, VHIDD_PLIST, 0o644, cfg)?;
    report.created.push(paths.vhidd_plist.clone());

    if !cfg.skip_launchctl {
        // Refresh case (label already ours): bootout first, best-effort, so
        // bootstrap doesn't fail with "service already loaded".
        let _ = Command::new(LAUNCHCTL)
            .args(["bootout", &format!("system/{VHIDD_LABEL}")])
            .status();
        bootstrap_with_retry("system", &paths.vhidd_plist)?;
    }
    Ok(())
}

/// System-domain launchd labels, for the §6.5a management check. Empty when
/// `skip_launchctl` (tests have no real launchd) or when `launchctl` fails.
fn system_launchd_labels(cfg: &InstallConfig) -> Vec<String> {
    if cfg.skip_launchctl {
        return Vec::new();
    }
    match Command::new(LAUNCHCTL).arg("list").output() {
        Ok(output) => {
            kanatabar_core::vhidd::parse_launchctl_list(&String::from_utf8_lossy(&output.stdout))
        }
        Err(_) => Vec::new(),
    }
}

/// Locate a binary expected next to the running `kanatactl`.
fn sibling_bin(name: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolving current executable")?;
    let dir = exe
        .parent()
        .context("current executable has no parent directory")?;
    let candidate = dir.join(name);
    if !candidate.exists() {
        bail!(
            "{name} not found next to {} — build the workspace first (cargo build --workspace)",
            exe.display()
        );
    }
    Ok(candidate)
}

fn install_binary(src: &Path, dst: &Path, cfg: &InstallConfig) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    // The pkg's postinstall runs the freshly-payloaded kanatactl from
    // /usr/local/bin (SPEC §12): src and dst are then the same file, and the
    // unlink below would delete the source. Fix up perms/owner and stop.
    if let (Ok(s), Ok(d)) = (src.canonicalize(), dst.canonicalize()) {
        if s == d {
            fs::set_permissions(dst, fs::Permissions::from_mode(0o755))?;
            if cfg.prefix == Path::new("/") {
                chown_root(dst)?;
            }
            return Ok(());
        }
    }
    // Unlink first: `fs::copy` truncates in place, and truncating a *running*
    // binary (upgrade over a live daemon) corrupts/kills the process — unlink
    // leaves the running process on the old inode instead.
    if dst.symlink_metadata().is_ok() {
        fs::remove_file(dst).with_context(|| format!("removing old {}", dst.display()))?;
    }
    fs::copy(src, dst)
        .with_context(|| format!("copying {} to {}", src.display(), dst.display()))?;
    fs::set_permissions(dst, fs::Permissions::from_mode(0o755))?;
    if cfg.prefix == Path::new("/") {
        chown_root(dst)?;
    }
    Ok(())
}

fn write_file(path: &Path, contents: &str, mode: u32, cfg: &InstallConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    if cfg.prefix == Path::new("/") {
        chown_root(path)?;
    }
    Ok(())
}

/// Write a file into the *user's* home as root (the agent plist): the file and
/// any `LaunchAgents` directory we create end up owned by the user — launchd
/// expects user-owned agent plists, and a root-owned `~/Library/LaunchAgents`
/// would block the user from ever adding their own agents. Because the parent
/// directory is user-controlled, refuse symlinks in place of the directory or
/// file and replace rather than truncate-through (§14).
fn write_user_file(
    path: &Path,
    contents: &str,
    mode: u32,
    cfg: &InstallConfig,
    user: &TargetUser,
) -> Result<()> {
    let real = cfg.prefix == Path::new("/");
    if let Some(parent) = path.parent() {
        match parent.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!(
                    "{} is a symlink; refusing to write through it",
                    parent.display()
                );
            }
            Ok(_) => {}
            Err(_) => {
                fs::create_dir_all(parent)?;
                if real {
                    chown_user(parent, user)?;
                }
            }
        }
    }
    // Never write through a pre-existing symlink at the target path.
    if let Ok(meta) = path.symlink_metadata() {
        if meta.file_type().is_symlink() {
            bail!(
                "{} is a symlink; refusing to write through it",
                path.display()
            );
        }
        fs::remove_file(path)?;
    }
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    if real {
        chown_user(path, user)?;
    }
    Ok(())
}

fn set_owner_mode(path: &Path, cfg: &InstallConfig, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    if cfg.prefix == Path::new("/") {
        chown_root(path)?;
    }
    Ok(())
}

/// `chown root:wheel` — only reachable once `check_privilege` confirmed we
/// are root (SPEC §3.2 ownership table).
fn chown_root(path: &Path) -> Result<()> {
    std::os::unix::fs::chown(path, Some(0), Some(0))
        .with_context(|| format!("chown root:wheel {}", path.display()))
}

/// `chown <user>:<group>` for files installed into the user's home.
fn chown_user(path: &Path, user: &TargetUser) -> Result<()> {
    std::os::unix::fs::chown(path, Some(user.uid), Some(user.gid))
        .with_context(|| format!("chown {} {}", user.uid, path.display()))
}

/// Remove `/Applications/KanataBar.app` — but only after confirming it is
/// ours (its `Info.plist` carries [`BUNDLE_ID`]): uninstall must never delete
/// a stranger's app that happens to sit at our path.
fn remove_app_bundle(paths: &Paths, report: &mut UninstallReport) -> Result<()> {
    let info = paths.app_bundle.join("Contents/Info.plist");
    let Ok(body) = fs::read(&info) else {
        return Ok(()); // no bundle (tarball install) — nothing to do
    };
    if !String::from_utf8_lossy(&body).contains(BUNDLE_ID) {
        println!(
            "{} does not identify as {BUNDLE_ID}; leaving it alone",
            paths.app_bundle.display()
        );
        return Ok(());
    }
    remove_path(&paths.app_bundle, report)
}

fn remove_path(path: &Path, report: &mut UninstallReport) -> Result<()> {
    // symlink_metadata, not exists(): exists() follows symlinks and reports
    // false for a dangling one, which uninstall must still remove.
    let Ok(meta) = path.symlink_metadata() else {
        return Ok(());
    };
    if meta.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    report.removed.push(path.to_path_buf());
    Ok(())
}

/// Absolute path: an installer running as root must not resolve `launchctl`
/// via `PATH` (§14).
const LAUNCHCTL: &str = "/bin/launchctl";

/// `launchctl bootstrap`, retried briefly. Every bootstrap here follows a
/// best-effort `bootout` of the same label, and `bootout` is asynchronous —
/// a bootstrap issued while the previous instance is still draining fails
/// with `Bootstrap failed: 5: Input/output error` (HW 2026-07-16: the pkg
/// postinstall raced its own bootout over a live daemon; a manual reinstall
/// minutes earlier won the same race by luck).
fn bootstrap_with_retry(target: &str, plist: &Path) -> Result<()> {
    const ATTEMPTS: u32 = 10;
    const DELAY: std::time::Duration = std::time::Duration::from_millis(500);
    let plist = plist.display().to_string();
    for attempt in 1..=ATTEMPTS {
        let status = Command::new(LAUNCHCTL)
            .args(["bootstrap", target, &plist])
            .status()
            .context("running launchctl")?;
        if status.success() {
            return Ok(());
        }
        if attempt < ATTEMPTS {
            std::thread::sleep(DELAY);
        }
    }
    bail!("launchctl bootstrap {target} {plist} still failing after {ATTEMPTS} attempts");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_plist_substitutes_home_and_tray() {
        let rendered = render_agent_plist("/Users/alice", UNBUNDLED_TRAY);
        assert!(rendered.contains("/Users/alice/Library/Logs/KanataBar/tray.out.log"));
        assert!(rendered.contains("/Users/alice/Library/Logs/KanataBar/tray.err.log"));
        assert!(rendered.contains("<string>/usr/local/bin/kanatabar-tray</string>"));
        assert!(!rendered.contains("__HOME__"));
        assert!(!rendered.contains("__TRAY_BIN__"));
        assert!(rendered.contains(AGENT_LABEL));
    }

    #[test]
    fn agent_plist_points_into_the_bundle_when_asked() {
        let rendered = render_agent_plist("/Users/alice", BUNDLED_TRAY);
        assert!(rendered.contains(
            "<string>/Applications/KanataBar.app/Contents/MacOS/kanatabar-tray</string>"
        ));
        assert!(!rendered.contains("__TRAY_BIN__"));
    }

    #[test]
    fn agent_template_has_no_hardcoded_tray_path() {
        assert!(AGENT_PLIST_TEMPLATE.contains("__TRAY_BIN__"));
        assert!(!AGENT_PLIST_TEMPLATE.contains("/usr/local/bin/kanatabar-tray"));
    }

    #[test]
    fn install_binary_self_copy_is_a_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("kanatad");
        fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let cfg = InstallConfig {
            prefix: dir.path().to_path_buf(),
            skip_launchctl: true,
            component: Component::Both,
        };
        install_binary(&bin, &bin, &cfg).expect("self-copy must not fail");
        assert_eq!(fs::read(&bin).unwrap(), b"#!/bin/sh\n", "content survives");
        let mode = fs::metadata(&bin).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn daemon_plist_matches_spec_paths() {
        assert!(DAEMON_PLIST.contains(DAEMON_LABEL));
        assert!(DAEMON_PLIST.contains("/usr/local/bin/kanatad"));
        assert!(DAEMON_PLIST.contains("/Library/Logs/KanataBar/kanatad.out.log"));
        assert!(DAEMON_PLIST.contains("<key>KeepAlive</key><true/>"));
    }

    #[test]
    fn under_prefix_joins_and_identity() {
        assert_eq!(
            under(Path::new("/"), "/usr/local/bin"),
            Path::new("/usr/local/bin")
        );
        assert_eq!(
            under(Path::new("/tmp/root"), "/usr/local/bin"),
            Path::new("/tmp/root/usr/local/bin")
        );
    }

    #[test]
    fn component_selection() {
        assert!(Component::Both.wants_daemon());
        assert!(Component::Both.wants_agent());
        assert!(Component::Daemon.wants_daemon());
        assert!(!Component::Daemon.wants_agent());
        assert!(Component::Agent.wants_agent());
        assert!(!Component::Agent.wants_daemon());
    }

    #[test]
    fn check_privilege_skips_when_prefix_is_not_root() {
        let cfg = InstallConfig {
            prefix: PathBuf::from("/tmp/kanatabar-test-prefix"),
            ..InstallConfig::default()
        };
        check_privilege(&cfg).expect("non-default prefix bypasses the root check");
    }

    #[test]
    fn check_privilege_requires_root_for_the_real_prefix() {
        if nix::unistd::geteuid().is_root() {
            return; // running as root in this environment; nothing to assert.
        }
        assert!(check_privilege(&InstallConfig::default()).is_err());
    }
}
