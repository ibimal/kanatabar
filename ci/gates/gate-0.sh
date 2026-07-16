#!/usr/bin/env bash
# Gate 0 [AUTO] — scaffold: build+test+lint green; mock-kanata --help works. SPEC §19.
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
cargo run -q -p mock-kanata -- --help >/dev/null

echo "gate-0: PASS"
