#!/usr/bin/env bash
# Gate 3 [AUTO] — config/presets (SPEC §19): invalid config refused while the
# old child keeps running; last-known-good rollback works; path-safety cases
# rejected. Plus TOML load and the pure path-safety rules.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace
cargo test --workspace

# Pure path-safety rules (core).
cargo test -q -p kanatabar-core --lib pathsafety::tests

# Gate-defining integration tests, by exact name.
for t in \
    invalid_config_refused_while_old_keeps_running \
    rollback_restores_last_known_good \
    rollback_without_backup_uses_safe_config \
    path_safety_rejects_unsafe_configs \
    switch_preset_applies_and_lists_active; do
    cargo test -q -p kanatad --test config "$t" -- --exact "$t"
done

# TOML config.toml parsing (§7.3).
cargo test -q -p kanatad --lib configmgr::tests

echo "gate-3: PASS"
