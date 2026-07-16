//! Gate-3 integration tests (SPEC §19): config/presets against the real
//! ConfigManager + supervisor + mock-kanata in temp dirs — invalid config
//! refused while the old child keeps running, last-known-good rollback,
//! path-safety rejections, TOML load, and preset switching. No root, devices,
//! or real kanata (SPEC §17).

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use kanatabar_core::state::SupervisorState as S;
use kanatad::config::SupervisorConfig;
use kanatad::configmgr::{ConfigError, ConfigManager, ConfigPaths, Rollback};
use kanatad::supervisor::{self, Command, SupervisorHandle};

fn target_dir() -> PathBuf {
    let mut dir = std::env::current_exe().expect("test exe path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir
}

fn mock_bin() -> PathBuf {
    let bin = target_dir().join("mock-kanata");
    assert!(
        bin.exists(),
        "run `cargo build --workspace` first: {}",
        bin.display()
    );
    bin
}

/// The mock treats `--check` on a config whose path contains "broken" as a
/// failure? No — the mock decides via `--mock-fail-check`. For config-content
/// validity we instead rely on a sentinel in the file the manager passes
/// through `--check`; the mock passes all checks unless flagged. So to model a
/// "broken" config we point at a path the mock is told to reject via extra
/// args. Simpler: the mock passes any real file; we model brokenness with a
/// dedicated bad binary. Here we use a wrapper: a config file whose first line
/// is "BROKEN" is rejected by `mock-kanata --check` (see mock-kanata).
const GOOD: &str = ";; good config\n(defsrc)\n(deflayer base)\n";
const BROKEN: &str = "BROKEN\n";

struct Fixture {
    handle: SupervisorHandle,
    manager: ConfigManager,
    dir: tempfile::TempDir,
    uid: u32,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let seed_cfg = dir.path().join("seed.kbd");
        std::fs::write(&seed_cfg, GOOD).unwrap();

        let mut config = SupervisorConfig::new(mock_bin(), seed_cfg);
        config.state_dir = Some(dir.path().join("state"));
        config.backoff.base_ms = 20;
        config.kill_grace = Duration::from_secs(2);
        let preflight_timeout = config.preflight_timeout;

        let handle = supervisor::start(config);
        let manager = ConfigManager::load(
            handle.client(),
            handle.active_config(),
            ConfigPaths::under(dir.path()),
            mock_bin(),
            preflight_timeout,
            kanatad::events::DaemonEvents::default(),
        );
        Self {
            handle,
            manager,
            dir,
            uid: nix::unistd::geteuid().as_raw(),
        }
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    async fn wait_state(&self, want: S) {
        for _ in 0..400 {
            if self.handle.snapshot().state == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("state never became {want:?}");
    }

    async fn wait_running_pid(&self) -> u32 {
        self.wait_state(S::Running).await;
        self.handle.snapshot().kanata_pid.expect("running pid")
    }

    /// Apply a config and wait for the resulting restart to fully settle to a
    /// new running pid (apply sends `Restart`: Running→Starting→Running).
    async fn apply_settled(&self, path: &std::path::Path) -> u32 {
        let before = self.handle.snapshot().kanata_pid;
        self.manager
            .apply_path(path, self.uid)
            .await
            .expect("apply");
        for _ in 0..400 {
            let snap = self.handle.snapshot();
            if snap.state == S::Running {
                if let Some(pid) = snap.kanata_pid {
                    if snap.kanata_pid != before {
                        return pid;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("apply never settled to a new running pid");
    }
}

/// [HARD] A config that fails `--check` is refused and the running child is
/// left untouched (SPEC §6.4). Gate-defining.
#[tokio::test]
async fn invalid_config_refused_while_old_keeps_running() {
    let fx = Fixture::new();
    // The seed config is applied at startup (no ConfigManager restart), so the
    // running pid is stable before we probe it.
    fx.handle.send(Command::Start).await.unwrap();
    let pid_before = fx.wait_running_pid().await;

    // Applying a broken config is refused with ConfigInvalid…
    let broken = fx.write("broken.kbd", BROKEN);
    let err = fx.manager.apply_path(&broken, fx.uid).await.unwrap_err();
    assert!(matches!(err, ConfigError::ConfigInvalid(_)), "got {err:?}");

    // …and the previously-running child is untouched (same pid, still Running):
    // validation fails before the supervisor is ever signaled (SPEC §6.4 [HARD]).
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fx.handle.snapshot().state, S::Running);
    assert_eq!(fx.handle.snapshot().kanata_pid, Some(pid_before));

    // A subsequent good apply does restart onto the new config.
    let good = fx.write("good.kbd", GOOD);
    let pid_after = fx.apply_settled(&good).await;
    assert_ne!(pid_after, pid_before);

    fx.handle.shutdown().await.unwrap();
}

/// [HARD] Last-known-good rollback restores a working config even after the
/// active file is edited to something broken on disk (SPEC §6.4). Gate-defining.
#[tokio::test]
async fn rollback_restores_last_known_good() {
    let fx = Fixture::new();
    let active = fx.write("active.kbd", GOOD);
    fx.handle.send(Command::Start).await.unwrap();
    fx.wait_state(S::Running).await;

    // Apply the good config → snapshot saved as last-known-good; wait for the
    // restart to settle so the later reload sees a stable Running child.
    fx.apply_settled(&active).await;

    // The user edits the active file into something broken on disk.
    std::fs::write(&active, BROKEN).unwrap();
    // A reload keeps the old process (SPEC §16): returns false, no error.
    assert!(!fx.manager.reload_active().await.unwrap());
    assert_eq!(fx.handle.snapshot().state, S::Running);

    // Roll back → the backup snapshot (still good) is re-applied.
    let outcome = fx.manager.rollback(fx.uid).await.expect("rollback");
    assert_eq!(outcome, Rollback::LastKnownGood);
    fx.wait_state(S::Running).await;

    fx.handle.shutdown().await.unwrap();
}

/// With no backup, rollback falls back to the built-in safe config (SPEC §6.4).
#[tokio::test]
async fn rollback_without_backup_uses_safe_config() {
    let fx = Fixture::new();
    fx.handle.send(Command::Start).await.unwrap();
    fx.wait_state(S::Running).await;

    let outcome = fx.manager.rollback(fx.uid).await.expect("rollback");
    assert_eq!(outcome, Rollback::SafeConfig);
    fx.wait_state(S::Running).await;
    // The safe config was materialized to disk.
    assert!(fx.dir.path().join("safe.kbd").exists());

    fx.handle.shutdown().await.unwrap();
}

/// [HARD] Path-safety rejections (SPEC §6.4): world-writable file and a
/// foreign-owned file are refused with PathRejected. Gate-defining.
#[tokio::test]
async fn path_safety_rejects_unsafe_configs() {
    let fx = Fixture::new();

    // World-writable → rejected.
    let writable = fx.write("writable.kbd", GOOD);
    std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o666)).unwrap();
    let err = fx
        .manager
        .validate(&writable, fx.uid, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConfigError::PathRejected(_)),
        "writable: {err:?}"
    );

    // A missing path is rejected (cannot resolve).
    let missing = fx.dir.path().join("does-not-exist.kbd");
    let err = fx
        .manager
        .validate(&missing, fx.uid, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConfigError::PathRejected(_)),
        "missing: {err:?}"
    );

    // A foreign-owned file is rejected: simulate by requesting as a different
    // uid than the file's owner (the file is owned by us).
    let owned = fx.write("owned.kbd", GOOD);
    std::fs::set_permissions(&owned, std::fs::Permissions::from_mode(0o644)).unwrap();
    let foreign_uid = fx.uid.wrapping_add(1);
    let err = fx
        .manager
        .validate(&owned, foreign_uid, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConfigError::PathRejected(_)),
        "foreign: {err:?}"
    );

    // The same file, requested by its real owner, validates.
    fx.manager
        .validate(&owned, fx.uid, None)
        .await
        .expect("owner ok");

    fx.handle.shutdown().await.unwrap();
}

/// A symlink whose target is a regular safe file is accepted (canonicalized),
/// but the safety verdict applies to the resolved inode.
#[tokio::test]
async fn symlink_resolves_to_target_facts() {
    let fx = Fixture::new();
    let real = fx.write("real.kbd", GOOD);
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o644)).unwrap();
    let link = fx.dir.path().join("link.kbd");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    // The symlink resolves to a safe, owner-matching target → accepted.
    let canonical = fx
        .manager
        .validate(&link, fx.uid, None)
        .await
        .expect("symlink ok");
    assert_eq!(canonical, std::fs::canonicalize(&real).unwrap());

    fx.handle.shutdown().await.unwrap();
}

/// Preset switching: SwitchPreset applies a preset's config and marks it active.
#[tokio::test]
async fn switch_preset_applies_and_lists_active() {
    let fx = Fixture::new();
    let main_kbd = fx.write("main.kbd", GOOD);
    fx.handle.send(Command::Start).await.unwrap();
    fx.wait_state(S::Running).await;

    // Register a preset via SetPresetList, then switch to it.
    let mut presets = std::collections::BTreeMap::new();
    presets.insert(
        "main".to_string(),
        kanatabar_core::config::PresetDef {
            config: main_kbd.to_string_lossy().into_owned(),
            autostart: false,
            kanata_bin: None,
            extra_args: vec![],
        },
    );
    fx.manager
        .set_preset_list(presets)
        .await
        .expect("set presets");

    fx.manager
        .switch_preset("main", fx.uid)
        .await
        .expect("switch");
    fx.wait_state(S::Running).await;

    let listed = fx.manager.list_presets().await;
    let main = listed
        .iter()
        .find(|p| p.name == "main")
        .expect("main listed");
    assert!(main.active, "main should be active after switch");

    // Switching to an unknown preset is an InvalidRequest, child untouched.
    let err = fx.manager.switch_preset("nope", fx.uid).await.unwrap_err();
    assert!(matches!(err, ConfigError::UnknownPreset(_)), "got {err:?}");
    assert_eq!(fx.handle.snapshot().state, S::Running);

    fx.handle.shutdown().await.unwrap();
}

/// `preset add` upserts into config.toml and `preset remove` deletes — the CLI
/// path that replaces hand-editing (v0.1.1). Add requires the .kbd to exist.
#[tokio::test]
async fn add_and_remove_preset_persist_and_validate() {
    let fx = Fixture::new();
    let kbd = fx.write("game.kbd", GOOD);
    let kbd = kbd.display().to_string();

    // Add is refused when the .kbd doesn't exist (no silent dangling preset).
    let err = fx
        .manager
        .add_preset("gaming", "/no/such.kbd", false)
        .await
        .unwrap_err();
    assert!(matches!(err, ConfigError::PathRejected(_)), "got {err:?}");

    // A real path adds it, and it shows up in the list.
    fx.manager
        .add_preset("gaming", &kbd, true)
        .await
        .expect("add");
    let listed = fx.manager.list_presets().await;
    let gaming = listed.iter().find(|p| p.name == "gaming").expect("listed");
    assert!(gaming.autostart, "autostart flag persisted");

    // Remove deletes it; removing again errors (not silent).
    fx.manager.remove_preset("gaming").await.expect("remove");
    assert!(fx.manager.list_presets().await.is_empty());
    let err = fx.manager.remove_preset("gaming").await.unwrap_err();
    assert!(matches!(err, ConfigError::UnknownPreset(_)), "got {err:?}");

    fx.handle.shutdown().await.unwrap();
}

/// `config reload` re-reads a hand-edited config.toml so presets appear without
/// a daemon restart — the gap the early user hit (v0.1.1).
#[tokio::test]
async fn reload_picks_up_hand_edited_presets() {
    use kanatabar_core::config::ConfigStatus;
    let fx = Fixture::new();
    assert!(fx.manager.list_presets().await.is_empty());

    // Hand-write config.toml the way a user would, then reload.
    let kbd = fx.write("main.kbd", GOOD);
    let toml = format!(
        "schema = 1\n[presets.main]\nconfig = \"{}\"\n",
        kbd.display()
    );
    std::fs::write(fx.dir.path().join("config.toml"), toml).unwrap();

    let status = fx.manager.reload().await;
    assert_eq!(status, ConfigStatus::Loaded { presets: 1 });
    assert_eq!(fx.manager.list_presets().await.len(), 1);

    // A broken edit is reported Invalid and the previous presets are kept.
    std::fs::write(
        fx.dir.path().join("config.toml"),
        "schema = 1\n[presets.main",
    )
    .unwrap();
    let status = fx.manager.reload().await;
    assert!(status.is_invalid(), "broken reload reports Invalid");
    assert_eq!(
        fx.manager.list_presets().await.len(),
        1,
        "previous good presets kept on a broken reload"
    );

    fx.handle.shutdown().await.unwrap();
}

/// `active_is_passthrough` is true only when the built-in safe config is
/// running — not merely when no preset is set (v0.1.2 labeling).
#[tokio::test]
async fn passthrough_is_detected_only_for_the_safe_config() {
    let fx = Fixture::new();
    // Seed config is a real .kbd, not the safe passthrough.
    fx.handle.send(Command::Start).await.unwrap();
    fx.wait_state(S::Running).await;
    assert!(
        !fx.manager.active_is_passthrough(),
        "a real seed config is not passthrough"
    );

    // Roll back to the built-in safe config → now passthrough.
    fx.manager.apply_safe_config(fx.uid).await.expect("safe");
    fx.wait_state(S::Running).await;
    assert!(
        fx.manager.active_is_passthrough(),
        "safe config should read as passthrough"
    );

    fx.handle.shutdown().await.unwrap();
}
