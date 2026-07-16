#!/usr/bin/env bash
# Gate 7 — tray app (SPEC §19).
# [AUTO] UI-less client logic: the toolkit-free library (model/session/menu/
#        icons/notify/reconnect/login/single-instance) unit tests, plus the
#        connect → seed → subscribe → reconnect → command loop driven
#        end-to-end against the real kanatad + mock-kanata over a temp socket
#        (no display, no root, no real kanata — SPEC §17).
# [HW]   visual/menu checklist on a real Mac: see docs/HW-TESTS.md (Phase 7).
set -euo pipefail
cd "$(dirname "$0")/../.."

# The end-to-end client test drives the real daemon + mock binaries.
cargo build --workspace
cargo test --workspace

# The tray's pure client logic (the [AUTO] half of the gate).
cargo test -q -p kanatabar-tray --lib

# The gate-defining end-to-end client-logic test, by exact name.
cargo test -q -p kanatabar-tray --test client \
    tray_client_tracks_daemon_state_and_delivers_commands -- --exact \
    tray_client_tracks_daemon_state_and_delivers_commands

echo "gate-7 [AUTO]: PASS"
echo
echo "gate-7 [HW]: the tray's visual/menu behaviour needs a human on a real Mac"
echo "  — see docs/HW-TESTS.md (Phase 7): status icon reflects state, menu"
echo "  enable/disable tracks the state machine, presets switch, notifications"
echo "  fire on crash/degraded/recovery, single-instance, Launch at Login."
echo "  Run it: cargo run -p kanatabar-tray  (with the daemon up via just run-dev)."
