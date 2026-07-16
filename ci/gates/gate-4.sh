#!/usr/bin/env bash
# Gate 4 — device monitor (SPEC §19).
# [AUTO] fake-event debounce tests (must pass headless).
# [HW]   real hotplug → single restart: see docs/HW-TESTS.md (manual).
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace
cargo test --workspace

# Pure debounce + device-relevance rules (core), incl. proptests.
cargo test -q -p kanatabar-core --lib debounce::tests
cargo test -q -p kanatabar-core --lib device::tests

# Gate-defining fake-event pipeline tests, by exact name.
for t in \
    hotplug_storm_collapses_to_one_restart \
    spaced_changes_restart_each \
    irrelevant_changes_do_not_restart \
    no_restart_when_not_running; do
    cargo test -q -p kanatad --test device "$t" -- --exact "$t"
done

echo "gate-4 [AUTO]: PASS"
echo
echo "gate-4 [HW]: real hotplug requires a human — see docs/HW-TESTS.md (Phase 4)."
echo "  Implement + build are done; the IOKit path has an open [VERIFY] noted there."
