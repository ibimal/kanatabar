#!/usr/bin/env bash
# Build kanatad/kanatactl/kanatabar-tray for both Apple architectures and
# lipo them into target/universal/, ad-hoc signed (SPEC §12).
set -euo pipefail
cd "$(dirname "$0")/.."

TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
BINS=(kanatad kanatactl kanatabar-tray)

for target in "${TARGETS[@]}"; do
    rustup target add "$target" >/dev/null
    cargo build --release --target "$target" -p kanatad -p kanatactl -p kanatabar-tray
done

mkdir -p target/universal
for bin in "${BINS[@]}"; do
    lipo -create -output "target/universal/$bin" \
        "target/${TARGETS[0]}/release/$bin" \
        "target/${TARGETS[1]}/release/$bin"
    # Ad-hoc signature (SPEC §12): required on Apple silicon, and keeps TCC
    # grants stable across copies of this artifact. No identity involved.
    codesign --force -s - "target/universal/$bin"
    echo "target/universal/$bin: $(lipo -archs "target/universal/$bin")"
done
