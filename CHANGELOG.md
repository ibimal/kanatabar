# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
