#!/usr/bin/env bash
# Gate 1 [AUTO] — supervisor core (SPEC §19): crash→backoff→degraded,
# healthy-window reset, graceful SIGTERM, transitions logged.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace
cargo test --workspace

# Run the gate-defining integration tests by exact name so a rename or
# deletion breaks the gate instead of silently passing.
for t in \
    crash_backoff_then_degraded \
    healthy_window_resets_budget \
    budget_spent_without_healthy_reset \
    graceful_sigterm_persists_state_and_logs_transitions; do
    cargo test -q -p kanatad --test supervisor "$t" -- --exact "$t"
done

echo "gate-1: PASS"
