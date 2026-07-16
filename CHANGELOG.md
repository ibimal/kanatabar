# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3] - 2026-07-16

### Added

- The tray's **Run Doctor** now opens the full report (all checks, details, and
  fix hints) in the default text viewer and names the failing checks in the
  notification — mirroring `kanatactl doctor` instead of a bare failure count.

### Fixed

- The tray **Presets** menu stayed disabled after `kanatactl preset add` until a
  reconnect. Preset changes now emit a `PresetsChanged` event and the tray
  refreshes the list live.
- Cleaned up the empty `preset list` guidance (removed a redundant line; a
  `config.kbd` now suggests the preset name `main`).

## [0.1.2] - 2026-07-16

### Added

- The first-run wizard, once setup is complete, scans `~/.config/kanata` and —
  if no preset is configured — offers the exact `preset add` command for an
  existing config, so onboarding ends ready to remap.

### Changed

- Passthrough (no preset selected) is now labelled as such in `status` and
  `doctor` instead of surfacing the internal `safe.kbd` path. New additive
  `Status.passthrough` field.

## [0.1.1] - 2026-07-16

First-release UX fixes from early-user feedback: the daemon no longer fails
silently, and managing presets and kanata no longer requires hand-editing files.

### Added

- `kanatactl preset add <name> <path.kbd> [--autostart]` and `preset remove
  <name>` — manage presets without hand-editing `config.toml`; populates the
  previously-empty tray Presets menu. Adding refuses a nonexistent `.kbd`.
- `kanatactl config reload` — re-read `config.toml` so hand edits to presets
  take effect without restarting the daemon (`[defaults]` still need a restart).
- `doctor` gains a `config file` check that **fails** on a present-but-broken
  `config.toml`, naming the parse error and the fix.
- Empty `preset list` scans `~/.config/kanata` and prints the exact `preset
  add` command for each existing `.kbd`, so an existing kanata config is
  copy-paste to import.

### Fixed

- A broken `config.toml` is no longer silently discarded (which left presets
  mysteriously empty). It is logged at ERROR and surfaced by `doctor`; the
  daemon still runs on defaults so a typo can't take the keyboard down.
- kanata installed via `cargo install` (`~/.cargo/bin`) or MacPorts
  (`/opt/local/bin`) is now detected: `doctor` names the location and the exact
  `kanata_bin` line to set, instead of an unhelpful "not found". (The daemon
  still never auto-runs a user-writable path as root — the user opts in.)

## [0.1.0] - 2026-07-16

Initial public release. Hardware-verified end-to-end on macOS 26.5 with kanata 1.12.0
(see docs/HW-TESTS.md).

### Added

- `kanatad`: root LaunchDaemon supervising kanata — full state machine
  (Running/Paused/Degraded/…), bounded exponential backoff with crash budget,
  healthy-window reset, graceful shutdown.
- Hotplug re-sync: IOKit device watcher with debounce; a hotplugged keyboard triggers
  exactly one kanata restart.
- Health checks: Karabiner driver preflight, VHID-daemon liveness with 15 s grace,
  sleep/wake re-sync, orphan sweep, kanata TCP layer-change relay.
- Actionable Degraded states with fix hints: driver not activated, Input Monitoring
  denied, device grab conflict, VHID daemon down, kanata binary missing, and more.
- VHID-daemon management: installs a LaunchDaemon for the Karabiner virtual-HID daemon
  when nothing else manages it; detects and defers to Karabiner-Elements or an existing
  community plist.
- Config presets (`config.toml`): validate-before-apply (`kanata --check`),
  last-known-good rollback, autostart preset, path-safety checks.
- `kanatactl` CLI: status/start/stop/restart/pause/resume, watch, preset
  list/switch, config validate/apply, logs, devices, autostart, doctor (`--json` as a
  bug-report bundle), install/uninstall (with `--daemon-only`/`--agent-only`).
- KanataBar menu-bar app: live state + active layer, start/stop/pause/resume, preset
  picker, notifications (crash, degraded, recovery), first-run setup wizard
  (driver install/activation, permissions, service install), single-instance,
  launch-at-login.
- Control plane: UNIX-domain socket with peer-credential auth (uid 0 + console user),
  versioned JSON protocol.
- Clean uninstall: removes every installed path and launchd job, leaves shared
  directories and Karabiner files untouched (audited).

[Unreleased]: https://github.com/ibimal/kanatabar/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ibimal/kanatabar/releases/tag/v0.1.0
