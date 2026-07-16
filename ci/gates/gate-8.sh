#!/usr/bin/env bash
# Gate 8 — wizard + doctor (SPEC §19).
# [AUTO] doctor JSON schema stable: the core schema anchor + pass/fail
#        aggregation, the wizard step model (shared check names), and the
#        `kanatactl doctor [--json]` report driven end-to-end against the real
#        kanatad + mock-kanata over a temp socket (schema on the real path,
#        all-green + offline). No root, driver, or real kanata (SPEC §17).
# [HW]   clean-machine onboarding via the tray Setup Wizard: see
#        docs/HW-TESTS.md (Phase 8).
set -euo pipefail
cd "$(dirname "$0")/../.."

# The doctor end-to-end test drives the real daemon + mock binaries.
cargo build --workspace
cargo test --workspace

# The stable-schema anchor + aggregation (kanatabar_core::doctor).
cargo test -q -p kanatabar-core doctor::tests

# The wizard step model (shares the doctor check names; single source of truth).
cargo test -q -p kanatabar-tray --lib wizard::tests

# The gate-defining end-to-end doctor tests, by exact name.
for t in \
    doctor_reports_a_stable_all_green_report_against_the_mock \
    doctor_is_useful_offline_with_the_daemon_check_failing; do
    cargo test -q -p kanatactl --test doctor "$t" -- --exact "$t"
done

echo "gate-8 [AUTO]: PASS"
echo
echo "gate-8 [HW]: clean-machine onboarding needs a human — see docs/HW-TESTS.md"
echo "  (Phase 8): the tray Setup Wizard walks driver install → extension"
echo "  approval → Input Monitoring → service install → green doctor, opening"
echo "  the right System Settings pane at each step. Also verify \`kanatactl"
echo "  doctor\` and \`--json\` on a real machine with Karabiner installed."
