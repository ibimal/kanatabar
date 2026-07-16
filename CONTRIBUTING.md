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

## Releasing (maintainers)

Releases are tag-driven. Pushing a `vX.Y.Z` tag runs
[`.github/workflows/release.yml`](.github/workflows/release.yml), which builds the
universal artifacts, publishes a GitHub Release, and bumps the Homebrew tap.

1. **Bump the version** in `Cargo.toml` (`[workspace.package] version`) and run
   `cargo build` so `Cargo.lock` updates.
2. **Update `CHANGELOG.md`** — move the `[Unreleased]` notes into a new
   `[X.Y.Z] - <date>` section.
3. **Verify:** `just check` and `just gate-10` must both be green.
4. **Commit, then tag and push:**
   ```sh
   git commit -am "chore(release): vX.Y.Z — <summary>"
   git push origin main
   git tag -a vX.Y.Z -m "KanataBar vX.Y.Z"
   git push origin vX.Y.Z
   ```
   The workflow verifies the tag matches the workspace version, re-runs `gate-10`,
   creates the Release (notes from the matching CHANGELOG section), and pushes the
   updated cask to `ibimal/homebrew-tap`.

The `[HW]` brew items (clean install, `brew upgrade`, cask uninstall) are verified by
hand against [docs/HW-TESTS.md](docs/HW-TESTS.md) Run 9 after publishing.

### Homebrew tap

All KanataBar (and any future ibimal apps') casks live in the single
[`ibimal/homebrew-tap`](https://github.com/ibimal/homebrew-tap) repo under `Casks/`.
The cask is generated from [`packaging/homebrew/kanatabar.rb`](packaging/homebrew/kanatabar.rb)
(the `__VERSION__`/`__SHA256__` placeholders are substituted at release time) — **edit
the template in this repo, never the tap directly**, or the next release overwrites it.

The tap bump needs a token, because a repo's default `GITHUB_TOKEN` can't push to a
*different* repo:

- Create a **fine-grained PAT** named `kanatabar-tap-bump`, scoped to **only**
  `ibimal/homebrew-tap` with **Contents: Read and write**.
- Add it as the `TAP_PUSH_TOKEN` repository secret:
  `gh secret set TAP_PUSH_TOKEN --repo ibimal/kanatabar`.

Without the secret the release still succeeds; the tap-bump step just prints the manual
`version`/`sha256` to apply. Fine-grained PATs expire (max one year) — GitHub emails a
reminder before expiry; rotate by generating a new token and re-running `gh secret set`.

## Reporting bugs

Please attach `kanatactl doctor --json` output — it's designed as a bug-report bundle
(it contains check results and versions, never key data). Include your macOS, kanata,
and Karabiner driver versions if doctor can't run.
