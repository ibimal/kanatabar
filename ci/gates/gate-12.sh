#!/usr/bin/env bash
# Gate 12 — wizard, doctor & devices windows (SPEC §19, §11.2–§11.3, §8;
#           docs/design/phase12-ui-layer.md).
# [AUTO] window view-models and the page↔shell ipc protocol are display-free
#        lib code: devwin/healthwin/wizardwin (pinned render-JSON shapes),
#        pages (action validation against the static step table), the §11.1
#        check classification in core, and the doctor JSON schema staying
#        stable.
# [HW]   window behaviour needs a human on a real Mac: see docs/HW-TESTS.md
#        (Phase 12) — accessory-app focus, hotplug flips the open window,
#        wizard auto-open + live re-check, doctor→wizard delegation,
#        dark/light, unbundled dev run.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace

# The Phase 12 view-model + protocol tests (the [AUTO] half of the gate).
cargo test -q -p kanatabar-tray --lib devwin::
cargo test -q -p kanatabar-tray --lib healthwin::
cargo test -q -p kanatabar-tray --lib wizardwin::
cargo test -q -p kanatabar-tray --lib pages::
cargo test -q -p kanatabar-tray --lib wizard::

# The §11.1 classification (single source in core) and the doctor JSON wire
# shape must not drift while windows are added on top.
cargo test -q -p kanatabar-core --lib doctor::

echo "gate-12 [AUTO]: PASS"
echo
echo "gate-12 [HW]: window behaviour needs a human on a real Mac"
echo "  — see docs/HW-TESTS.md (Phase 12, Runs 10-11): windows open/focus from"
echo "  the accessory-policy tray and float in tiling WMs; devices refreshes on"
echo "  hotplug; the wizard auto-opens while setup is incomplete, live-rechecks"
echo "  every ~2s, and never elevates (sudo steps are copyable text); the"
echo "  Health Check delegates setup-class failures to the Setup Assistant;"
echo "  Copy report round-trips the doctor --json bundle; dark & light render."
echo "  Run it: cargo run -p kanatabar-tray  (with the daemon up via just run-dev)."
