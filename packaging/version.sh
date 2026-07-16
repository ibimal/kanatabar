#!/usr/bin/env bash
# Print the workspace version (single source: [workspace.package] in Cargo.toml).
set -euo pipefail
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
if [[ -z "$version" ]]; then
    echo "could not read workspace.package.version from Cargo.toml" >&2
    exit 1
fi
echo "$version"
