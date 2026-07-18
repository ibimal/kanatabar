//! Gate-6 install/uninstall audit (SPEC §19): the real `kanatactl` binary
//! installing/uninstalling into a temp-dir prefix, with `launchctl` skipped
//! (no real launchd to bootstrap a fake-prefix plist into). Exercises the
//! exact file list `install` creates and confirms `uninstall` removes every
//! one of them — "uninstall leaves nothing behind" (SPEC §10, §16) — without
//! root and without touching the real system (SPEC §17).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn target_dir() -> PathBuf {
    let mut dir = std::env::current_exe().expect("test exe path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir
}

/// `kanatactl install`'s `sibling_bin` lookup needs kanatad/kanatabar-tray
/// built next to it; `just check`/gate scripts always `cargo build
/// --workspace` first.
fn require_workspace_built() {
    for name in ["kanatad", "kanatactl", "kanatabar-tray"] {
        let path = target_dir().join(name);
        assert!(
            path.exists(),
            "run `cargo build --workspace` first: {}",
            path.display()
        );
    }
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kanatactl"))
        .args(args)
        .output()
        .expect("run kanatactl")
}

/// Every regular file under `root`, relative to it, sorted.
fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    if root.exists() {
        walk(root, root, &mut out);
    }
    out.sort();
    out
}

#[test]
fn install_creates_exactly_the_expected_files() {
    require_workspace_built();
    let prefix = tempfile::tempdir().expect("tempdir");

    let output = run(&[
        "install",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut expected = vec![
        PathBuf::from("usr/local/bin/kanatad"),
        PathBuf::from("usr/local/bin/kanatactl"),
        PathBuf::from("usr/local/bin/kanatabar-tray"),
        PathBuf::from("Library/Logs/KanataBar"), // a dir, but create_dir_all leaves no file; see below
        PathBuf::from("Library/LaunchDaemons/io.github.ibimal.kanatabar.daemon.plist"),
        // Shell completions ride with the CLI binary.
        PathBuf::from("usr/local/etc/bash_completion.d/kanatactl"),
        PathBuf::from("usr/local/share/zsh/site-functions/_kanatactl"),
        PathBuf::from("usr/local/share/fish/vendor_completions.d/kanatactl.fish"),
    ];
    // The agent plist lands under the target user's home, wherever that
    // resolves in this environment (SUDO_UID or /dev/console owner).
    let found = files_under(prefix.path());
    // Logs dir itself isn't a file; drop it from the expectation and assert
    // it exists separately.
    expected.retain(|p| p != Path::new("Library/Logs/KanataBar"));
    assert!(
        prefix.path().join("Library/Logs/KanataBar").is_dir(),
        "logs dir missing"
    );
    for path in &expected {
        assert!(
            found.contains(path),
            "expected {} in {:?}",
            path.display(),
            found
        );
    }
    let agent_plists: Vec<_> = found
        .iter()
        .filter(|p| p.ends_with("io.github.ibimal.kanatabar.agent.plist"))
        .collect();
    assert_eq!(
        agent_plists.len(),
        1,
        "expected exactly one agent plist in {found:?}"
    );

    // Daemon binary content matches the real kanatad this test was built with.
    let installed_daemon = fs::read(prefix.path().join("usr/local/bin/kanatad")).unwrap();
    let real_daemon = fs::read(target_dir().join("kanatad")).unwrap();
    assert_eq!(installed_daemon, real_daemon);

    let mode = fs::metadata(prefix.path().join("usr/local/bin/kanatad"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755, "daemon binary must be 0755");

    // Uninstall must remove every file install created, though the (shared,
    // never-recursively-deleted) usr/local/bin and Library/LaunchDaemons
    // directories themselves are left in place, empty.
    let output = run(&[
        "uninstall",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "uninstall failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let remaining = files_under(prefix.path());
    assert!(remaining.is_empty(), "left behind: {remaining:?}");
    assert!(
        !prefix.path().join("Library/Logs/KanataBar").exists(),
        "logs dir should be removed (KanataBar-exclusive)"
    );
}

/// SPEC §6.5a: with the driver's daemon binary present and nothing managing
/// it (no launchd labels in a `--skip-launchctl` test run), install registers
/// our vhidd LaunchDaemon plist — and uninstall removes the plist while
/// leaving Karabiner's own daemon binary untouched.
#[test]
fn vhidd_plist_installed_when_daemon_binary_present_and_unmanaged() {
    require_workspace_built();
    let prefix = tempfile::tempdir().expect("tempdir");

    // Fake the driver-installed daemon binary under the prefix.
    let vhidd_binary = prefix
        .path()
        .join(kanatabar_core::vhidd::VHIDD_BINARY.trim_start_matches('/'));
    fs::create_dir_all(vhidd_binary.parent().unwrap()).unwrap();
    fs::write(&vhidd_binary, b"#!/bin/sh\n").unwrap();

    let output = run(&[
        "install",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let plist = prefix.path().join(format!(
        "Library/LaunchDaemons/{}.plist",
        kanatabar_core::vhidd::VHIDD_LABEL
    ));
    assert!(plist.is_file(), "vhidd plist not written");
    let mode = fs::metadata(&plist).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o644, "vhidd plist must be 0644");
    let body = fs::read_to_string(&plist).unwrap();
    assert!(body.contains(kanatabar_core::vhidd::VHIDD_LABEL));
    assert!(body.contains("Karabiner-VirtualHIDDevice-Daemon"));
    assert!(body.contains("<key>KeepAlive</key><true/>"));
    assert!(body.contains("<key>ProcessType</key><string>Interactive</string>"));

    let output = run(&[
        "uninstall",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());
    assert!(!plist.exists(), "uninstall must remove our vhidd plist");
    assert!(
        vhidd_binary.is_file(),
        "uninstall must never touch Karabiner's own daemon binary"
    );
}

/// SPEC §6.5a: without the daemon binary (driver not installed) the vhidd
/// plist is skipped — there is nothing to run yet; the wizard step comes
/// first. (The default install-audit test above also relies on this.)
#[test]
fn vhidd_plist_skipped_without_daemon_binary() {
    require_workspace_built();
    let prefix = tempfile::tempdir().expect("tempdir");

    let output = run(&[
        "install",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());
    let plist = prefix.path().join(format!(
        "Library/LaunchDaemons/{}.plist",
        kanatabar_core::vhidd::VHIDD_LABEL
    ));
    assert!(!plist.exists(), "vhidd plist must be skipped: {plist:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Karabiner driver not installed"),
        "install should say why the vhidd job was skipped"
    );

    // Uninstall still succeeds and leaves nothing.
    let output = run(&[
        "uninstall",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());
    assert!(files_under(prefix.path()).is_empty());
}

/// Plant a fake `/Applications/KanataBar.app` under the prefix, with the
/// given bundle identifier in its Info.plist.
fn plant_app_bundle(prefix: &Path, bundle_id: &str) -> PathBuf {
    let app = prefix.join("Applications/KanataBar.app");
    let macos = app.join("Contents/MacOS");
    fs::create_dir_all(&macos).unwrap();
    fs::write(macos.join("kanatabar-tray"), b"#!/bin/sh\n").unwrap();
    fs::write(
        app.join("Contents/Info.plist"),
        format!(
            "<plist><dict><key>CFBundleIdentifier</key><string>{bundle_id}</string></dict></plist>"
        ),
    )
    .unwrap();
    app
}

/// SPEC §12: pkg installs carry the tray inside /Applications/KanataBar.app —
/// install must point the agent there and NOT copy a second tray binary to
/// /usr/local/bin; uninstall removes the bundle (it is ours, by bundle id).
#[test]
fn install_prefers_the_bundled_tray_when_the_app_is_present() {
    require_workspace_built();
    let prefix = tempfile::tempdir().expect("tempdir");
    let app = plant_app_bundle(prefix.path(), "io.github.ibimal.kanatabar");

    let output = run(&[
        "install",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        !prefix.path().join("usr/local/bin/kanatabar-tray").exists(),
        "bundled install must not also copy a bare tray binary"
    );

    // A stale bare tray from an earlier tarball install must be removed when
    // the install switches to the bundle (HW 2026-07-16: launchd kept running
    // a months-old leftover). Re-run install with one planted.
    let stale = prefix.path().join("usr/local/bin/kanatabar-tray");
    fs::write(&stale, b"stale").unwrap();
    let output = run(&[
        "install",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());
    assert!(!stale.exists(), "stale bare tray must be removed");
    let found = files_under(prefix.path());
    let agent_plist = found
        .iter()
        .find(|p| p.ends_with("io.github.ibimal.kanatabar.agent.plist"))
        .expect("agent plist written");
    let body = fs::read_to_string(prefix.path().join(agent_plist)).unwrap();
    assert!(
        body.contains("<string>/Applications/KanataBar.app/Contents/MacOS/kanatabar-tray</string>"),
        "agent must run the bundled tray: {body}"
    );

    let output = run(&[
        "uninstall",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());
    assert!(!app.exists(), "uninstall must remove our app bundle");
    assert!(files_under(prefix.path()).is_empty());
}

/// Uninstall must never delete an app that merely sits at our path but is not
/// ours (its Info.plist carries a different bundle identifier).
#[test]
fn uninstall_leaves_a_foreign_app_bundle_alone() {
    require_workspace_built();
    let prefix = tempfile::tempdir().expect("tempdir");
    let app = plant_app_bundle(prefix.path(), "com.example.impostor");

    let output = run(&[
        "uninstall",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());
    assert!(app.exists(), "foreign bundle must survive uninstall");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("leaving it alone"),
        "uninstall should say why the bundle was kept"
    );
}

/// The pkg postinstall scenario (SPEC §12): kanatactl runs FROM /usr/local/bin
/// under the prefix, so `sibling_bin` resolves to the very files install would
/// copy onto themselves. The self-copy guard must keep them intact — without
/// it, unlink-then-copy deletes the source and the install destroys itself.
#[test]
fn postinstall_style_self_install_keeps_the_binaries() {
    require_workspace_built();
    let prefix = tempfile::tempdir().expect("tempdir");
    let bin_dir = prefix.path().join("usr/local/bin");
    fs::create_dir_all(&bin_dir).unwrap();
    for name in ["kanatad", "kanatactl", "kanatabar-tray"] {
        fs::copy(target_dir().join(name), bin_dir.join(name)).unwrap();
    }

    let output = Command::new(bin_dir.join("kanatactl"))
        .args([
            "install",
            "--skip-launchctl",
            "--prefix",
            prefix.path().to_str().unwrap(),
        ])
        .output()
        .expect("run the payloaded kanatactl");
    assert!(
        output.status.success(),
        "self-install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["kanatad", "kanatactl", "kanatabar-tray"] {
        let installed = fs::read(bin_dir.join(name)).unwrap();
        let original = fs::read(target_dir().join(name)).unwrap();
        assert_eq!(installed, original, "{name} must survive self-install");
    }
}

#[test]
fn uninstall_removes_preexisting_config_and_state() {
    require_workspace_built();
    let prefix = tempfile::tempdir().expect("tempdir");
    let support_dir = prefix.path().join("Library/Application Support/KanataBar");
    fs::create_dir_all(&support_dir).unwrap();
    fs::write(support_dir.join("config.toml"), "# fake").unwrap();
    fs::write(support_dir.join("state.json"), "{}").unwrap();
    fs::create_dir_all(support_dir.join("backups")).unwrap();
    fs::write(support_dir.join("backups/last.kbd"), "(defcfg)").unwrap();

    let socket_dir = prefix.path().join("var/run");
    fs::create_dir_all(&socket_dir).unwrap();
    fs::write(socket_dir.join("kanatabar.sock"), "").unwrap();

    let output = run(&[
        "uninstall",
        "--daemon-only",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "uninstall failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!support_dir.exists(), "config/state must be removed");
    assert!(!socket_dir.join("kanatabar.sock").exists());
}

#[test]
fn daemon_only_skips_the_agent() {
    require_workspace_built();
    let prefix = tempfile::tempdir().expect("tempdir");
    let output = run(&[
        "install",
        "--daemon-only",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert!(output.status.success());
    assert!(!prefix.path().join("usr/local/bin/kanatabar-tray").exists());
    let found = files_under(prefix.path());
    assert!(
        found
            .iter()
            .all(|p| !p.ends_with("io.github.ibimal.kanatabar.agent.plist")),
        "daemon-only must not touch the agent plist: {found:?}"
    );
}

#[test]
fn mutually_exclusive_flags_are_a_usage_error() {
    require_workspace_built();
    let prefix = tempfile::tempdir().expect("tempdir");
    let output = run(&[
        "install",
        "--daemon-only",
        "--agent-only",
        "--skip-launchctl",
        "--prefix",
        prefix.path().to_str().unwrap(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected the usage exit code"
    );
}
