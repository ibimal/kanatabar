#!/usr/bin/env bash
# Gate 2 [AUTO] — UDS control server + kanatactl (SPEC §19): CLI round-trips vs
# daemon+mock in a temp dir; wrong-uid peer rejected (simulated); serde
# round-trip of 100% of IPC types.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace
cargo test --workspace

# Gate-defining tests, by exact name, so a rename/deletion breaks the gate.
cargo test -q -p kanatabar-core --lib ipc::tests -- --exact \
    ipc::tests::every_request_round_trips \
    ipc::tests::every_response_round_trips \
    ipc::tests::status_wire_matches_spec_example \
    ipc::tests::negotiate_accepts_only_supported_version

for t in \
    hello_then_status_round_trips \
    lifecycle_commands_round_trip \
    subscribe_receives_state_changes \
    unauthorized_peer_is_rejected \
    request_before_hello_is_invalid; do
    cargo test -q -p kanatad --test control "$t" -- --exact "$t"
done

cargo test -q -p kanatactl --test e2e cli_round_trips_against_daemon_and_mock \
    -- --exact cli_round_trips_against_daemon_and_mock

echo "gate-2: PASS"
