//! Config data model (SPEC §6.4, §7.3).
//!
//! Phase 2 defines the preset model as serde types so they can cross the
//! control socket (`SetPresetList`, `ListPresets`). Phase 3 adds TOML loading,
//! validation, path safety, and last-known-good handling on top of these.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::backoff::BackoffConfig;

/// Built-in transparent-passthrough config: the final fallback when even the
/// last-known-good backup is broken (SPEC §6.4). Materialized to disk by the
/// daemon and loaded like any other preset.
///
/// [VERIFY] the exact syntax against the installed kanata version before
/// relying on it as a real fallback.
pub const SAFE_CONFIG: &str = "\
;; KanataBar safe fallback config — transparent passthrough, remaps nothing.
;; [VERIFY] against the installed kanata version.
(defcfg process-unmapped-keys no)
(defsrc)
(deflayer base)
";

/// Current `config.toml` schema version (SPEC §7.3 `schema = 1`).
pub const CONFIG_SCHEMA: u32 = 1;

/// Daemon-wide defaults from `[defaults]` in `config.toml` (SPEC §7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    /// kanata binary; empty string means `/usr/local/bin/kanata` then `PATH`.
    pub kanata_bin: String,
    /// kanata TCP port for the layer-event client (Phase 5).
    pub tcp_port: u16,
    /// Extra arguments appended to every kanata invocation.
    pub extra_args: Vec<String>,
    /// Coalescing window for device/config events (SPEC §6.2 [HARD]).
    pub debounce_ms: u64,
    /// Backoff tuning (SPEC §6.2).
    pub backoff: BackoffConfig,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            kanata_bin: String::new(),
            tcp_port: 5829,
            extra_args: vec!["--nodelay".to_string()],
            debounce_ms: 500,
            backoff: BackoffConfig::default(),
        }
    }
}

/// Whether `config.toml` was loaded, absent, or present-but-broken. The daemon
/// tracks this so a parse failure is *surfaced* (doctor, logs) instead of the
/// file being silently discarded — the original v0.1.0 behaviour, which left a
/// user's presets mysteriously empty with only a buried log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigStatus {
    /// No `config.toml` on disk — running on built-in defaults (normal).
    Missing,
    /// Parsed successfully; carries the preset count for a friendly detail.
    Loaded {
        /// Number of presets the file defines.
        presets: usize,
    },
    /// Present but failed to parse; its presets and defaults are ignored and
    /// the daemon runs on built-in defaults until it is fixed. Carries the
    /// human-readable parse error (e.g. the `toml` message with line/column).
    Invalid {
        /// The parse/read error, rendered for display.
        error: String,
    },
}

impl ConfigStatus {
    /// True when the file is present but could not be parsed (the actionable
    /// failure doctor should flag).
    pub fn is_invalid(&self) -> bool {
        matches!(self, ConfigStatus::Invalid { .. })
    }
}

/// The parsed `config.toml` (SPEC §3.2, §7.3): schema, defaults, presets.
///
/// Modified only via IPC (`SetPresetList`) or sudo (SPEC §6.4); preset `.kbd`
/// files themselves live wherever the user likes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFile {
    /// Schema version.
    #[serde(default = "default_schema")]
    pub schema: u32,
    /// Daemon-wide defaults.
    #[serde(default)]
    pub defaults: Defaults,
    /// Named presets.
    #[serde(default)]
    pub presets: BTreeMap<String, PresetDef>,
}

fn default_schema() -> u32 {
    CONFIG_SCHEMA
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            schema: CONFIG_SCHEMA,
            defaults: Defaults::default(),
            presets: BTreeMap::new(),
        }
    }
}

impl ConfigFile {
    /// The preset marked `autostart`, if any (first by name for determinism).
    pub fn autostart_preset(&self) -> Option<(&str, &PresetDef)> {
        self.presets
            .iter()
            .find(|(_, def)| def.autostart)
            .map(|(name, def)| (name.as_str(), def))
    }
}

/// One named preset: a `.kbd` config plus optional per-preset overrides
/// (SPEC §7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetDef {
    /// Path to the preset's `.kbd` file.
    pub config: String,
    /// Start this preset automatically when the daemon boots.
    #[serde(default)]
    pub autostart: bool,
    /// Override the kanata binary for this preset; `None` uses the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kanata_bin: Option<String>,
    /// Extra arguments appended for this preset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

/// A full set of named presets — the payload of `SetPresetList` (SPEC §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PresetList {
    /// Presets keyed by name (ordered for stable output).
    pub presets: BTreeMap<String, PresetDef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_def_defaults_are_omitted() {
        let def = PresetDef {
            config: "/x.kbd".into(),
            autostart: false,
            kanata_bin: None,
            extra_args: vec![],
        };
        let json = serde_json::to_string(&def).unwrap();
        // Optional/empty fields are skipped; `autostart` keeps its explicit default.
        assert_eq!(json, r#"{"config":"/x.kbd","autostart":false}"#);
        let back: PresetDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn config_file_defaults_match_spec() {
        let defaults = Defaults::default();
        assert_eq!(defaults.tcp_port, 5829);
        assert_eq!(defaults.debounce_ms, 500);
        assert_eq!(defaults.extra_args, vec!["--nodelay".to_string()]);
        assert_eq!(defaults.backoff, BackoffConfig::default());
        assert_eq!(ConfigFile::default().schema, CONFIG_SCHEMA);
    }

    #[test]
    fn config_file_round_trips_via_serde() {
        let mut presets = BTreeMap::new();
        presets.insert(
            "main".to_string(),
            PresetDef {
                config: "/main.kbd".into(),
                autostart: true,
                kanata_bin: None,
                extra_args: vec![],
            },
        );
        let file = ConfigFile {
            schema: 1,
            defaults: Defaults::default(),
            presets,
        };
        let json = serde_json::to_string(&file).unwrap();
        let back: ConfigFile = serde_json::from_str(&json).unwrap();
        assert_eq!(file, back);
    }

    #[test]
    fn autostart_preset_is_found() {
        let mut presets = BTreeMap::new();
        presets.insert(
            "main".to_string(),
            PresetDef {
                config: "/main.kbd".into(),
                autostart: true,
                kanata_bin: None,
                extra_args: vec![],
            },
        );
        presets.insert(
            "gaming".to_string(),
            PresetDef {
                config: "/gaming.kbd".into(),
                autostart: false,
                kanata_bin: None,
                extra_args: vec![],
            },
        );
        let file = ConfigFile {
            schema: 1,
            defaults: Defaults::default(),
            presets,
        };
        assert_eq!(file.autostart_preset().map(|(n, _)| n), Some("main"));
    }

    #[test]
    fn preset_list_round_trips() {
        let mut presets = BTreeMap::new();
        presets.insert(
            "main".to_string(),
            PresetDef {
                config: "/main.kbd".into(),
                autostart: true,
                kanata_bin: Some("/usr/local/bin/kanata".into()),
                extra_args: vec!["--nodelay".into()],
            },
        );
        let list = PresetList { presets };
        let json = serde_json::to_string(&list).unwrap();
        let back: PresetList = serde_json::from_str(&json).unwrap();
        assert_eq!(list, back);
    }
}
