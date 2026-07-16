//! Config & preset manager (SPEC §6.4).
//!
//! Owns `config.toml` (presets + defaults), validates user-supplied `.kbd`
//! paths before applying them, keeps a last-known-good backup on every
//! successful apply, and can roll back to it or to the built-in safe config.
//!
//! Invariants [HARD]:
//! - **Validate before apply**: a config that fails path-safety or
//!   `kanata --check` is refused and the running child is never touched — a
//!   bad config can never take the keyboard down (SPEC §6.4).
//! - **Path safety**: user paths are canonicalized, opened once, and the
//!   *opened inode* is checked for regular-file/ownership/permissions before
//!   use (SPEC §6.4, §14); the pure rules live in `kanatabar_core::pathsafety`.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kanatabar_core::config::{ConfigFile, ConfigStatus, PresetDef, SAFE_CONFIG};
use kanatabar_core::ipc::{Event, PresetInfo};
use kanatabar_core::pathsafety::{self, PathFacts, PathReject};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::child::{self, Preflight};
use crate::config::SpawnTarget;
use crate::events::DaemonEvents;
use crate::supervisor::{Command, SupervisorClient};

/// Filesystem locations under the support dir (SPEC §3.2). All derived from a
/// single base so `KANATABAR_STATE` injects them for tests.
#[derive(Debug, Clone)]
pub struct ConfigPaths {
    /// `config.toml`.
    pub config_toml: PathBuf,
    /// `backups/` directory for last-known-good `.kbd` copies.
    pub backups_dir: PathBuf,
    /// Materialized built-in safe config.
    pub safe_kbd: PathBuf,
}

impl ConfigPaths {
    /// Standard layout under `base` (the support dir).
    pub fn under(base: &Path) -> Self {
        Self {
            config_toml: base.join("config.toml"),
            backups_dir: base.join("backups"),
            safe_kbd: base.join("safe.kbd"),
        }
    }
}

/// Why a validate/apply was refused (maps onto IPC `ErrorKind`, SPEC §7.2).
#[derive(Debug)]
pub enum ConfigError {
    /// Path failed the §6.4 safety checks → `ErrorKind::PathRejected`.
    PathRejected(String),
    /// `kanata --check` rejected the config → `ErrorKind::ConfigInvalid`.
    ConfigInvalid(String),
    /// The named preset does not exist → `ErrorKind::InvalidRequest`.
    UnknownPreset(String),
    /// An internal failure → `ErrorKind::Internal`.
    Internal(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PathRejected(m)
            | Self::ConfigInvalid(m)
            | Self::UnknownPreset(m)
            | Self::Internal(m) => f.write_str(m),
        }
    }
}

/// What a rollback restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rollback {
    /// Re-applied the last-known-good backup.
    LastKnownGood,
    /// Fell back to the built-in safe config.
    SafeConfig,
}

/// The config manager. Cheap to clone (shares the supervisor client, the
/// active-config handle, and the parsed `config.toml` behind a mutex).
#[derive(Clone)]
pub struct ConfigManager {
    supervisor: SupervisorClient,
    active: crate::config::ActiveConfig,
    paths: ConfigPaths,
    default_bin: PathBuf,
    preflight_timeout: Duration,
    file: std::sync::Arc<Mutex<ConfigFile>>,
    /// Load/parse outcome of the on-disk `config.toml`, surfaced by `doctor`
    /// (SPEC §9) so a broken file never fails silently. Updated on reload.
    status: std::sync::Arc<Mutex<ConfigStatus>>,
    events: DaemonEvents,
}

impl ConfigManager {
    /// Create a manager, loading `config.toml` if present (else defaults).
    /// `events` receives `ConfigApplied` pushes on every successful commit
    /// (SPEC §7.2).
    pub fn load(
        supervisor: SupervisorClient,
        active: crate::config::ActiveConfig,
        paths: ConfigPaths,
        default_bin: PathBuf,
        preflight_timeout: Duration,
        events: DaemonEvents,
    ) -> Self {
        let (file, status) = load_with_status(&paths.config_toml);
        Self {
            supervisor,
            active,
            paths,
            default_bin,
            preflight_timeout,
            file: std::sync::Arc::new(Mutex::new(file)),
            status: std::sync::Arc::new(Mutex::new(status)),
            events,
        }
    }

    /// The load/parse status of `config.toml` (SPEC §9 doctor). A broken file
    /// is reported, never silently ignored.
    pub async fn config_status(&self) -> ConfigStatus {
        self.status.lock().await.clone()
    }

    /// The active spawn target (kanata bin + `.kbd`), for diagnostics
    /// (`doctor`, SPEC §9).
    pub fn active_target(&self) -> crate::config::SpawnTarget {
        self.active.spawn_target()
    }

    /// The active preset name, if one is selected (`doctor`, SPEC §9).
    pub fn active_preset(&self) -> Option<String> {
        self.active.snapshot().preset
    }

    /// The configured presets, flagged with which one is active (SPEC §7.2).
    pub async fn list_presets(&self) -> Vec<PresetInfo> {
        let active_preset = self.active.snapshot().preset;
        let file = self.file.lock().await;
        file.presets
            .iter()
            .map(|(name, def)| PresetInfo {
                name: name.clone(),
                config: def.config.clone(),
                autostart: def.autostart,
                active: Some(name) == active_preset.as_ref(),
            })
            .collect()
    }

    /// Validate a `.kbd` path without applying it: path safety then
    /// `kanata --check` (SPEC §6.4). Returns the canonical path on success.
    pub async fn validate(
        &self,
        path: &Path,
        requesting_uid: u32,
        preset_bin: Option<&str>,
    ) -> Result<PathBuf, ConfigError> {
        let canonical = self.safe_canonicalize(path, requesting_uid)?;
        let target = SpawnTarget {
            kanata_bin: self.resolve_bin(preset_bin),
            kanata_cfg: canonical.clone(),
            extra_args: Vec::new(),
            tcp_port: None, // not needed for `--check`
            vetted_uid: Some(requesting_uid),
        };
        match child::preflight_config_check(&target, self.preflight_timeout).await {
            Preflight::Ok => Ok(canonical),
            Preflight::ConfigBroken { detail } => Err(ConfigError::ConfigInvalid(detail)),
            Preflight::BinMissing => Err(ConfigError::Internal(format!(
                "kanata binary not found: {}",
                target.kanata_bin.display()
            ))),
        }
    }

    /// Validate then apply a raw `.kbd` path: back it up as last-known-good,
    /// point the supervisor at it, and restart (SPEC §6.4). On any failure the
    /// running child is untouched.
    pub async fn apply_path(&self, path: &Path, requesting_uid: u32) -> Result<(), ConfigError> {
        let canonical = self.validate(path, requesting_uid, None).await?;
        self.commit(&canonical, None, None, None, requesting_uid)
            .await
    }

    /// Switch to a named preset (SPEC §7.2 `SwitchPreset`).
    pub async fn switch_preset(&self, name: &str, requesting_uid: u32) -> Result<(), ConfigError> {
        let def = {
            let file = self.file.lock().await;
            file.presets
                .get(name)
                .cloned()
                .ok_or_else(|| ConfigError::UnknownPreset(format!("no such preset: {name}")))?
        };
        let path = PathBuf::from(&def.config);
        let canonical = self
            .validate(&path, requesting_uid, def.kanata_bin.as_deref())
            .await?;
        let bin = def.kanata_bin.as_ref().map(PathBuf::from);
        self.commit(
            &canonical,
            Some(name.to_string()),
            bin,
            Some(def.extra_args),
            requesting_uid,
        )
        .await
    }

    /// Replace the preset list and persist `config.toml` (SPEC §7.2
    /// `SetPresetList`). Does not touch the running child.
    pub async fn set_preset_list(
        &self,
        presets: std::collections::BTreeMap<String, PresetDef>,
    ) -> Result<(), ConfigError> {
        let mut file = self.file.lock().await;
        file.presets = presets;
        write_config_file(&self.paths.config_toml, &file)
            .map_err(|err| ConfigError::Internal(format!("writing config.toml: {err}")))
    }

    /// Add or update one preset (`kanatactl preset add`), preserving every
    /// other preset and this one's advanced fields (`kanata_bin`,
    /// `extra_args`) on update. The `.kbd` must exist — a preset pointing at
    /// nothing is a silent trap, exactly the class of bug this batch removes.
    /// Persists `config.toml`; does not touch the running child (switch to it
    /// to apply). Upsert semantics.
    pub async fn add_preset(
        &self,
        name: &str,
        config: &str,
        autostart: bool,
    ) -> Result<(), ConfigError> {
        if name.trim().is_empty() {
            return Err(ConfigError::Internal("preset name is empty".to_string()));
        }
        if !Path::new(config).is_file() {
            return Err(ConfigError::PathRejected(format!(
                "no such .kbd file: {config} — create it first, or check the path"
            )));
        }
        let mut file = self.file.lock().await;
        let entry = file.presets.entry(name.to_string()).or_insert(PresetDef {
            config: config.to_string(),
            autostart,
            kanata_bin: None,
            extra_args: Vec::new(),
        });
        // On update, refresh the two user-facing fields, keep the advanced ones.
        entry.config = config.to_string();
        entry.autostart = autostart;
        if autostart {
            for (other, def) in file.presets.iter_mut() {
                if other != name {
                    def.autostart = false;
                }
            }
        }
        write_config_file(&self.paths.config_toml, &file)
            .map_err(|err| ConfigError::Internal(format!("writing config.toml: {err}")))?;
        *self.status.lock().await = ConfigStatus::Loaded {
            presets: file.presets.len(),
        };
        info!(preset = %name, config, autostart, "preset added");
        Ok(())
    }

    /// Remove one preset by name (`kanatactl preset remove`). Errors if it is
    /// not configured. Persists `config.toml`; leaves the running child alone
    /// (removing the active preset's entry doesn't stop kanata).
    pub async fn remove_preset(&self, name: &str) -> Result<(), ConfigError> {
        let mut file = self.file.lock().await;
        if file.presets.remove(name).is_none() {
            return Err(ConfigError::UnknownPreset(format!(
                "no preset named `{name}`"
            )));
        }
        write_config_file(&self.paths.config_toml, &file)
            .map_err(|err| ConfigError::Internal(format!("writing config.toml: {err}")))?;
        *self.status.lock().await = ConfigStatus::Loaded {
            presets: file.presets.len(),
        };
        info!(preset = %name, "preset removed");
        Ok(())
    }

    /// Re-read `config.toml` from disk (`kanatactl config reload`) so hand
    /// edits to presets take effect without restarting the daemon. Returns the
    /// resulting [`ConfigStatus`] so the caller can report it. A broken file is
    /// reported (not applied) and the previous good preset list is kept. Note:
    /// `[defaults]` changes (e.g. `kanata_bin`, `tcp_port`) still need a daemon
    /// restart — only the preset list is hot-reloaded.
    pub async fn reload(&self) -> ConfigStatus {
        let (parsed, status) = load_with_status(&self.paths.config_toml);
        if !status.is_invalid() {
            let mut file = self.file.lock().await;
            file.presets = parsed.presets;
        }
        *self.status.lock().await = status.clone();
        status
    }

    /// Enable/disable autostart of the **active** preset (SPEC §7.2
    /// `SetAutostart`); enabling clears the flag on every other preset so the
    /// startup pick (`autostart_preset`) stays unambiguous. Persists
    /// `config.toml`; does not touch the running child.
    pub async fn set_autostart(&self, enabled: bool) -> Result<(), ConfigError> {
        let Some(active) = self.active.snapshot().preset else {
            return Err(ConfigError::UnknownPreset(
                "no active preset — switch to a preset first, then set autostart".to_string(),
            ));
        };
        let mut file = self.file.lock().await;
        let Some(def) = file.presets.get_mut(&active) else {
            return Err(ConfigError::UnknownPreset(format!(
                "active preset `{active}` is not in config.toml"
            )));
        };
        def.autostart = enabled;
        if enabled {
            for (name, def) in file.presets.iter_mut() {
                if name != &active {
                    def.autostart = false;
                }
            }
        }
        write_config_file(&self.paths.config_toml, &file)
            .map_err(|err| ConfigError::Internal(format!("writing config.toml: {err}")))?;
        info!(preset = %active, enabled, "autostart updated");
        Ok(())
    }

    /// Roll back to the last-known-good backup, or to the safe config if there
    /// is none or it no longer validates (SPEC §6.4).
    pub async fn rollback(&self, requesting_uid: u32) -> Result<Rollback, ConfigError> {
        if let Some(backup) = self.active.snapshot().last_known_good {
            if backup.exists() {
                match self.validate(&backup, requesting_uid, None).await {
                    Ok(canonical) => {
                        self.commit(&canonical, None, None, None, requesting_uid)
                            .await?;
                        info!(path = %canonical.display(), "rolled back to last-known-good");
                        return Ok(Rollback::LastKnownGood);
                    }
                    Err(err) => warn!(%err, "last-known-good no longer valid; using safe config"),
                }
            }
        }
        self.apply_safe_config(requesting_uid).await?;
        Ok(Rollback::SafeConfig)
    }

    /// Materialize and apply the built-in safe config (SPEC §6.4 fallback).
    pub async fn apply_safe_config(&self, requesting_uid: u32) -> Result<(), ConfigError> {
        write_safe_config(&self.paths.safe_kbd)
            .map_err(|err| ConfigError::Internal(format!("writing safe config: {err}")))?;
        let canonical = self
            .validate(&self.paths.safe_kbd, requesting_uid, None)
            .await?;
        self.commit(&canonical, None, None, None, requesting_uid)
            .await
    }

    /// Re-check the currently-active config on disk and restart kanata if it
    /// still passes `kanata --check` (SPEC §6.4 file watch). If the file was
    /// edited to something broken, the running child is kept and `Ok(false)`
    /// is returned — the old process keeps running (SPEC §16). For IPC-applied
    /// configs the path-safety check is re-run against the vetting uid too, so
    /// an edit that also flipped ownership/permissions is refused (§14).
    pub async fn reload_active(&self) -> Result<bool, ConfigError> {
        let target = self.active.spawn_target();
        if let Some(uid) = target.vetted_uid {
            if let Err(err) = vet_path(&target.kanata_cfg, uid) {
                warn!(
                    cfg = %target.kanata_cfg.display(),
                    %err,
                    "active config no longer passes path safety; keeping the running config"
                );
                return Ok(false);
            }
        }
        match child::preflight_config_check(&target, self.preflight_timeout).await {
            Preflight::Ok => {
                self.supervisor
                    .send(Command::Restart)
                    .await
                    .map_err(|err| ConfigError::Internal(err.to_string()))?;
                Ok(true)
            }
            Preflight::ConfigBroken { detail } => {
                warn!(
                    cfg = %target.kanata_cfg.display(),
                    %detail,
                    "active config edited to something invalid; keeping the running config"
                );
                Ok(false)
            }
            Preflight::BinMissing => Err(ConfigError::Internal(format!(
                "kanata binary not found: {}",
                target.kanata_bin.display()
            ))),
        }
    }

    /// Point the active config at `canonical`, back it up, and restart.
    async fn commit(
        &self,
        canonical: &Path,
        preset: Option<String>,
        bin: Option<PathBuf>,
        extra_args: Option<Vec<String>>,
        requesting_uid: u32,
    ) -> Result<(), ConfigError> {
        let backup = self
            .backup(canonical, preset.as_deref())
            .map_err(|err| ConfigError::Internal(format!("saving backup: {err}")))?;

        self.active.set_active(
            canonical.to_path_buf(),
            preset.clone(),
            bin,
            extra_args,
            Some(requesting_uid),
        );
        self.active.set_last_known_good(backup);

        self.supervisor
            .send(Command::Restart)
            .await
            .map_err(|err| ConfigError::Internal(err.to_string()))?;
        // Push to subscribers (SPEC §7.2) once the switch is committed.
        self.events.publish(Event::ConfigApplied {
            preset,
            path: canonical.display().to_string(),
        });
        Ok(())
    }

    /// Copy a validated `.kbd` into `backups/` as last-known-good (SPEC §6.4).
    fn backup(&self, canonical: &Path, preset: Option<&str>) -> io::Result<PathBuf> {
        fs::create_dir_all(&self.paths.backups_dir)?;
        let name = format!("{}.kbd", preset.unwrap_or("applied"));
        let dest = self.paths.backups_dir.join(name);
        fs::copy(canonical, &dest)?;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))?;
        Ok(dest)
    }

    /// Canonicalize, open once, and check the opened inode (SPEC §6.4 [HARD]).
    fn safe_canonicalize(&self, path: &Path, requesting_uid: u32) -> Result<PathBuf, ConfigError> {
        let canonical = fs::canonicalize(path)
            .map_err(|err| ConfigError::PathRejected(format!("cannot resolve path: {err}")))?;
        vet_path(&canonical, requesting_uid)?;
        Ok(canonical)
    }

    /// Resolve the kanata binary: preset override → configured default → the
    /// daemon default (SPEC §7.3: empty means the default).
    fn resolve_bin(&self, preset_bin: Option<&str>) -> PathBuf {
        preset_bin
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.default_bin.clone())
    }
}

fn describe_reject(reject: PathReject, path: &Path) -> String {
    format!("{}: {}", path.display(), reject.describe())
}

/// Open `path` once (`O_NOFOLLOW`) and check the *opened inode* against the
/// §6.4 [HARD] rules for `requesting_uid`. Shared by the apply-time validation
/// ([`ConfigManager::safe_canonicalize`]) and the spawn-time re-vet the
/// supervisor runs on IPC-applied targets (§14 TOCTOU hardening).
pub fn vet_path(path: &Path, requesting_uid: u32) -> Result<(), ConfigError> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|err| ConfigError::PathRejected(format!("cannot open path: {err}")))?;
    let meta = file
        .metadata()
        .map_err(|err| ConfigError::Internal(format!("stat failed: {err}")))?;
    let facts = PathFacts {
        is_regular_file: meta.file_type().is_file(),
        uid: meta.uid(),
        mode: meta.mode() & 0o7777,
    };
    pathsafety::check(&facts, requesting_uid)
        .map_err(|reject| ConfigError::PathRejected(describe_reject(reject, path)))
}

/// Read and parse `config.toml` (SPEC §7.3). Missing file → `Ok(None)`-like
/// default handled by the caller; parse/read errors propagate.
fn read_config_file(path: &Path) -> io::Result<ConfigFile> {
    let text = fs::read_to_string(path)?;
    toml::from_str(&text).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// Load `config.toml` and classify the outcome (SPEC §7.3, §9). A missing file
/// is normal (built-in defaults); a present-but-broken file is **never
/// silently discarded** — it is logged at ERROR and reported by `doctor` via
/// the returned [`ConfigStatus`], while the daemon keeps running on defaults so
/// a typo can't take the keyboard down (the §6.4 invariant).
fn load_with_status(path: &Path) -> (ConfigFile, ConfigStatus) {
    match read_config_file(path) {
        Ok(file) => {
            let presets = file.presets.len();
            (file, ConfigStatus::Loaded { presets })
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            (ConfigFile::default(), ConfigStatus::Missing)
        }
        Err(err) => {
            // The source `toml` error carries line/column; surface it verbatim.
            let error = err
                .get_ref()
                .map(|inner| inner.to_string())
                .unwrap_or_else(|| err.to_string());
            error!(
                path = %path.display(),
                %error,
                "config.toml is invalid — its presets and defaults are ignored; \
                 fix it and run `kanatactl config reload`"
            );
            (ConfigFile::default(), ConfigStatus::Invalid { error })
        }
    }
}

/// Serialize and atomically write `config.toml` at mode 0644 (SPEC §3.2).
fn write_config_file(path: &Path, file: &ConfigFile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(file)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
    fs::rename(&tmp, path)
}

/// Materialize the built-in safe config at mode 0644.
fn write_safe_config(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, SAFE_CONFIG)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))
}

/// Materialize the built-in safe config at `path` (mode 0644), creating parent
/// directories as needed. Public so `kanatad`'s `run` command can fall back to
/// it when started with no `--cfg` and no autostart preset (SPEC §10: the
/// launchd job invokes `kanatad run` with no arguments at all).
pub fn materialize_safe_config(path: &Path) -> io::Result<()> {
    write_safe_config(path)
}

/// Load `config.toml` for read-only inspection (used by the daemon at startup
/// to pick the autostart preset).
pub fn load_config_file(path: &Path) -> Option<ConfigFile> {
    match read_config_file(path) {
        Ok(file) => Some(file),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            warn!(%err, path = %path.display(), "failed to read config.toml");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing file is normal; a valid file loads with its preset count; a
    /// broken file is reported `Invalid` (not silently discarded) while the
    /// returned config falls back to defaults so the daemon keeps running.
    #[test]
    fn load_with_status_classifies_the_three_outcomes() {
        let dir = tempfile::tempdir().unwrap();

        let missing = dir.path().join("config.toml");
        let (file, status) = load_with_status(&missing);
        assert_eq!(status, ConfigStatus::Missing);
        assert!(file.presets.is_empty());

        let valid = dir.path().join("valid.toml");
        fs::write(
            &valid,
            "schema = 1\n[presets.main]\nconfig = \"/tmp/x.kbd\"\n",
        )
        .unwrap();
        let (file, status) = load_with_status(&valid);
        assert_eq!(status, ConfigStatus::Loaded { presets: 1 });
        assert_eq!(file.presets.len(), 1);

        let broken = dir.path().join("broken.toml");
        fs::write(&broken, "schema = 1\n[presets.main\nconfig = oops").unwrap();
        let (file, status) = load_with_status(&broken);
        assert!(status.is_invalid(), "broken file must report Invalid");
        assert!(
            file.presets.is_empty(),
            "broken file falls back to empty defaults"
        );
    }

    /// The §7.3 example `config.toml` parses into the expected model.
    #[test]
    fn parses_spec_example_toml() {
        let toml_src = r#"
schema = 1

[defaults]
kanata_bin  = ""
tcp_port    = 5829
extra_args  = ["--nodelay"]
debounce_ms = 500
backoff     = { base_ms = 1000, cap_ms = 30000, budget = 5, reset_after_s = 60 }

[presets.main]
config    = "/Users/alice/.config/kanata/main.kbd"
autostart = true

[presets.gaming]
config = "/Users/alice/.config/kanata/gaming.kbd"
"#;
        let file: ConfigFile = toml::from_str(toml_src).unwrap();
        assert_eq!(file.schema, 1);
        assert_eq!(file.defaults.tcp_port, 5829);
        assert_eq!(file.defaults.debounce_ms, 500);
        assert_eq!(file.defaults.backoff.budget, 5);
        assert_eq!(file.presets.len(), 2);
        assert_eq!(file.autostart_preset().map(|(n, _)| n), Some("main"));
        assert!(!file.presets["gaming"].autostart);
    }

    /// A minimal `config.toml` fills in defaults for everything omitted.
    #[test]
    fn minimal_toml_uses_defaults() {
        let file: ConfigFile = toml::from_str("schema = 1\n").unwrap();
        assert_eq!(file.defaults, kanatabar_core::config::Defaults::default());
        assert!(file.presets.is_empty());
    }

    /// Writing then reading a config round-trips through TOML on disk.
    #[test]
    fn config_file_survives_write_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut file = ConfigFile::default();
        file.presets.insert(
            "main".to_string(),
            PresetDef {
                config: "/main.kbd".into(),
                autostart: true,
                kanata_bin: None,
                extra_args: vec![],
            },
        );
        write_config_file(&path, &file).unwrap();
        // 0644 per SPEC §3.2.
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        let back = read_config_file(&path).unwrap();
        assert_eq!(file, back);
    }
}
