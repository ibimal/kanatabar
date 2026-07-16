#!/usr/bin/env bash
# Gate 5 — health (SPEC §19).
# [AUTO] preflight parser tests on captured systemextensionsctl outputs, plus
#        the driver-probe wiring, orphan sweep, and TCP layer relay.
# [HW]   driver-off → Degraded; wake recovery: see docs/HW-TESTS.md (manual).
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace
cargo test --workspace

# Pure parsers (core): systemextensionsctl, kanata version + layer messages.
cargo test -q -p kanatabar-core --lib driver::tests
cargo test -q -p kanatabar-core --lib kanata::tests

# Gate-defining health integration tests, by exact name.
for t in \
    driver_not_activated_degrades_without_spawn \
    vhid_daemon_down_degrades \
    healthy_driver_allows_start \
    orphan_sweep_kills_recorded_kanata \
    tcp_relay_reflects_layer_changes; do
    cargo test -q -p kanatad --test health "$t" -- --exact "$t"
done

echo "gate-5 [AUTO]: PASS"
echo
echo "gate-5 [HW]: driver-off → Degraded and wake recovery need a human —"
echo "  see docs/HW-TESTS.md (Phase 5)."
