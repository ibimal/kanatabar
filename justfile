# KanataBar build/test/gate commands — see docs/SPEC.md §20.2.
# Gate scripts live in ci/gates/; a missing script means the phase isn't implemented yet.

default: check

# fmt + clippy + tests + supply-chain audit — must be green at the end of every phase
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    cargo deny check

test:
    cargo test --workspace

# Run the daemon in the foreground against mock-kanata with temp state/socket
run-dev:
    ./ci/run-dev.sh

gate-0:
    ./ci/gates/gate-0.sh

gate-1:
    ./ci/gates/gate-1.sh

gate-2:
    ./ci/gates/gate-2.sh

gate-3:
    ./ci/gates/gate-3.sh

gate-4:
    ./ci/gates/gate-4.sh

gate-5:
    ./ci/gates/gate-5.sh

gate-6:
    ./ci/gates/gate-6.sh

gate-7:
    ./ci/gates/gate-7.sh

gate-8:
    ./ci/gates/gate-8.sh

gate-9:
    ./ci/gates/gate-9.sh

gate-10:
    ./ci/gates/gate-10.sh

gate-12:
    ./ci/gates/gate-12.sh

# Build both targets and lipo into a universal binary (Phase 10)
build-universal:
    ./packaging/build-universal.sh

# Componentize the installer pkg, ad-hoc signed (Phase 10, SPEC §12)
pkg:
    ./packaging/build-pkg.sh

# List every path an install touches, for the uninstall audit (Phase 6)
clean-install-audit:
    ./ci/clean-install-audit.sh
