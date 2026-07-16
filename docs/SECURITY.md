# Security

## Reporting a vulnerability

Please report vulnerabilities privately via
[GitHub Security Advisories](https://github.com/ibimal/kanatabar/security/advisories/new)
(“Report a vulnerability”). Do not open a public issue for security problems. You should
get a response within a week.

## Threat model & mitigations

KanataBar runs a root daemon (`kanatad`) that supervises kanata and exposes a local
control socket. The design decisions below are invariants (SPEC §14), not incidental:

| Threat | Mitigation |
|---|---|
| Non-console local user drives the root daemon via the socket | peer-credential allowlist (uid 0 + the console user), socket mode 0660 |
| Malicious IPC payloads | strict serde types, message size caps, typed errors (no panics), fuzzed decoder |
| Symlink/TOCTOU via `ApplyConfig` paths | canonicalize + regular-file + ownership + not-world-writable checks; open once, validate the opened file |
| Command injection | processes are spawned with `Command` and argument arrays; nothing is shell-interpolated |
| Keystroke exposure | the daemon never subscribes to key events; keystrokes and `.kbd` contents are banned from logs; no telemetry of any kind |
| Supply chain | `cargo-deny` (advisories, bans, license checks) in CI; `Cargo.lock` committed |
| Update tampering | no in-app updater; installs and updates go through Homebrew or manual GitHub Releases downloads with published checksums |
| Unsafe FFI bugs | all `unsafe` confined to `kanatad/src/ffi/` with `deny(unsafe_op_in_unsafe_fn)`, documented invariants, and safe wrapper APIs |

## What KanataBar can and cannot see

KanataBar supervises the kanata *process*. It reads process exit status, kanata's
stderr (for fault classification), device attach/detach events, and kanata's TCP
layer-change notifications. It has no access to what you type — remapping happens
inside kanata and the Karabiner virtual-HID driver, and KanataBar deliberately never
requests key-event access for itself.
