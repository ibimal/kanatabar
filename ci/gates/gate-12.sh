#!/usr/bin/env bash
# Gate 12 — wizard, doctor & devices windows (SPEC §19, §11.2–§11.3, §8;
#           docs/design/phase12-ui-layer.md).
# [AUTO] window view-models are display-free lib code: devwin (devices window;
#        healthwin/wizardwin join as they land), the pinned render-JSON shapes,
#        and the doctor JSON schema staying stable.
# [HW]   window behaviour needs a human on a real Mac: see docs/HW-TESTS.md
#        (Phase 12) — accessory-app focus, hotplug flips the open window,
#        dark/light, unbundled dev run.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace

# The Phase 12 view-model tests (the [AUTO] half of the gate), by module.
cargo test -q -p kanatabar-tray --lib devwin::

# The doctor JSON wire shape must not drift while windows are added on top.
cargo test -q -p kanatabar-core --lib doctor_check_json_schema_is_stable

echo "gate-12 [AUTO]: PASS (devices window; doctor/wizard windows pending)"
echo
echo "gate-12 [HW]: window behaviour needs a human on a real Mac"
echo "  — see docs/HW-TESTS.md (Phase 12): Devices… opens/focuses the window"
echo "  from the accessory-policy tray, hotplug refreshes it in place while"
echo "  open, close hides (instant re-open), dark & light render correctly,"
echo "  and the unbundled dev binary can create the WKWebView at all."
echo "  Run it: cargo run -p kanatabar-tray  (with the daemon up via just run-dev)."
