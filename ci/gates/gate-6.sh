#!/usr/bin/env bash
# Gate 6 — install/uninstall (SPEC §19).
# [AUTO] install/uninstall against a temp-dir prefix (no root, no real
#        launchd): exact file audit, --daemon-only/--agent-only scoping,
#        pre-existing config/state cleanup, usage errors.
# [HW]   reboot persistence; kill -9 revival; clean uninstall audit on a real
#        machine: see docs/HW-TESTS.md (manual).
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace
cargo test --workspace

# Pure plist/path rendering (kanatactl::install).
cargo test -q -p kanatactl --lib install::tests

# Gate-defining install/uninstall integration tests, by exact name.
for t in \
    install_creates_exactly_the_expected_files \
    uninstall_removes_preexisting_config_and_state \
    daemon_only_skips_the_agent \
    mutually_exclusive_flags_are_a_usage_error; do
    cargo test -q -p kanatactl --test install "$t" -- --exact "$t"
done

echo "gate-6 [AUTO]: PASS"
echo
echo "gate-6 [HW]: reboot persistence, kill -9 revival, and a clean-uninstall"
echo "  audit on real hardware need a human — see docs/HW-TESTS.md (Phase 6)."
echo "  Try it: sudo kanatactl install && sudo reboot; then sudo kill -9 \$(pgrep kanatad)."
echo "  ./ci/clean-install-audit.sh before/after 'sudo kanatactl uninstall' diffs the file list."
