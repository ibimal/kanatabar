# Contributing to KanataBar

Thanks for your interest! Issues and pull requests are welcome.

## Ground rules

- **The spec is authoritative.** [docs/SPEC.md](docs/SPEC.md) defines the architecture,
  invariants, and phase gates. Items marked **[HARD]** are non-negotiable design
  invariants; don't send PRs that weaken them. If you think the spec is wrong, open an
  issue first.
- **`just check` must stay green** — it runs `cargo fmt --check`, `clippy -D warnings`,
  the full test suite, and `cargo deny`. CI enforces the same.
- **Gates are sacred.** `just gate-N` scripts encode each phase's acceptance criteria.
  Never adjust a gate to make it pass; fix the code.

## Code invariants (enforced in review)

- `kanatabar-core` stays I/O-free and FFI-free (pure logic, unit-testable).
- All `unsafe` lives in `kanatad/src/ffi/` with `SAFETY:` comments and
  `deny(unsafe_op_in_unsafe_fn)`.
- No `unwrap`/`expect` outside tests.
- Never log keystrokes or `.kbd` file contents — anywhere, at any level.
- Spawn processes with `Command` and argument arrays; never shell-interpolate.
- Socket path and state dir must remain injectable via `KANATABAR_SOCK` /
  `KANATABAR_STATE` so tests run without root.

## Development

```sh
just check      # fmt + clippy + tests + cargo-deny
just run-dev    # daemon + mock-kanata in a temp sandbox (no root, no driver needed)
```

Almost everything is testable without hardware: `mock-kanata` simulates the child
process, and the daemon runs unprivileged in a temp dir. Changes that touch the driver,
launchd, TCC permissions, or real devices are **[HW]** territory — test against the
relevant section of [docs/HW-TESTS.md](docs/HW-TESTS.md) and say in the PR which items
you ran.

## Reporting bugs

Please attach `kanatactl doctor --json` output — it's designed as a bug-report bundle
(it contains check results and versions, never key data). Include your macOS, kanata,
and Karabiner driver versions if doctor can't run.
