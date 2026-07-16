#!/usr/bin/env bash
# Gate 9 — dependency hardening: VHID-daemon management (SPEC §6.5a, §19).
# [AUTO] the pure detection/decision logic (launchctl-list parsing, management
#        classification, install decision), the driver-version parsing and the
#        kanata↔driver coupling rule, the doctor schema growing the two new
#        checks, the wizard steps mapping to them, and `kanatactl install`
#        writing/removing our vhidd LaunchDaemon plist against a temp prefix.
#        No root, driver, or real kanata (SPEC §17).
# [HW]   reboot with the driver installed but NO Karabiner-Elements: kanata
#        comes back alive (our vhidd job keeps the daemon running); with
#        Karabiner-Elements present, no second daemon. See docs/HW-TESTS.md.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace
cargo test --workspace

# The pure §6.5a rules: parsing, classification, install decision.
cargo test -q -p kanatabar-core vhidd::tests

# Driver-version parsing + the kanata↔driver coupling rule (SPEC §2).
cargo test -q -p kanatabar-core driver::tests

# The doctor schema anchor now includes the two new checks; wizard maps steps.
cargo test -q -p kanatabar-core doctor::tests
cargo test -q -p kanatabar-tray --lib wizard::tests

# The gate-defining install/uninstall behaviour, by exact name.
for t in \
    vhidd_plist_installed_when_daemon_binary_present_and_unmanaged \
    vhidd_plist_skipped_without_daemon_binary; do
    cargo test -q -p kanatactl --test install "$t" -- --exact "$t"
done

echo "gate-9 [AUTO]: PASS"
echo
echo "gate-9 [HW]: VHID-daemon supervision needs a human — see docs/HW-TESTS.md"
echo "  (Phase 9): on a Mac with the Karabiner driver but no Karabiner-Elements,"
echo "  'sudo kanatactl install' registers io.github.ibimal.kanatabar.vhidd;"
echo "  after a reboot kanata is alive with no manual action. With"
echo "  Karabiner-Elements installed, doctor reports its job and we do NOT"
echo "  start a second daemon."
