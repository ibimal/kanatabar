#!/usr/bin/env bash
# Run kanatad in the foreground against mock-kanata with a temp state dir &
# socket (SPEC §20.2). Ctrl-C exercises the graceful-shutdown path.
set -euo pipefail
cd "$(dirname "$0")/.."

DEV_DIR="$(mktemp -d -t kanatabar-dev)"
trap 'rm -rf "$DEV_DIR"' EXIT

cargo build -p mock-kanata -p kanatad
printf ';; kanatabar dev config (mock)\n' > "$DEV_DIR/dev.kbd"

KANATABAR_SOCK="$DEV_DIR/kanatabar.sock" \
KANATABAR_STATE="$DEV_DIR/state" \
KANATABAR_KANATA_BIN="$(pwd)/target/debug/mock-kanata" \
KANATABAR_CFG="$DEV_DIR/dev.kbd" \
KANATABAR_SKIP_DRIVER_CHECK=true \
    cargo run -p kanatad -- run
