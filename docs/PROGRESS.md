# Progress

One line per phase: date, status, deviations from spec.

| Phase | Date | Status | Deviations |
|---|---|---|---|
| 0–10 | 2026-07-08 → 2026-07-16 | done — every [AUTO] gate green (`just gate-0`…`gate-10`); the [HW] runbook (docs/HW-TESTS.md) verified end-to-end on macOS 26.5 / kanata 1.12.0, including Run 9's local half (pkg install, reinstall-over-live, bundled notifications, wizard) | Phase-by-phase development predates the public repo; this history begins at v0.1.0. Remaining [HW]: Run 9's brew items (cask install/upgrade/uninstall against the published release). |
| v0.1.1 | 2026-07-16 | released — config/preset UX batch from first-user feedback: config.toml parse errors now surface (doctor `config file` check, never silent); `kanatactl preset add/remove` + `config reload` (no hand-editing); kanata detected in `~/.cargo/bin`/MacPorts with guided `kanata_bin` fix; empty `preset list` discovers `~/.config/kanata` configs. `just check` + gate-10 green; published + tap bumped. | kanata-bin resolution kept the §14 allowlist (root never auto-runs a user-writable path); user opts in via `kanata_bin`. config.toml auto-watch deferred — explicit `config reload` + `preset add` is safer (defaults can't hot-swap under a running child). |
