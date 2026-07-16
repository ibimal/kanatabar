# KanataBar — agent instructions
Spec: docs/SPEC.md is authoritative. Build phase-by-phase (§19); never skip gates.
- Commands: `just check` (fmt+clippy+test+deny) · `just gate-N` · `just run-dev` (daemon+mock).
- [HARD] items in the spec are invariants. [VERIFY] items: check against installed versions.
- kanatabar-core stays I/O-free and FFI-free. All unsafe lives in kanatad/src/ffi with SAFETY comments.
- No unwrap/expect outside tests. clippy -D warnings must stay green.
- Never log keystrokes or .kbd contents. Never commit secrets; signing reads env vars.
- Socket path & state dir must be injectable via env for tests (KANATABAR_SOCK, KANATABAR_STATE).
- Each phase: commit `phase-N: summary`; append one line to docs/PROGRESS.md (status, deviations).
- If a gate seems wrong, don't weaken it — record in PROGRESS.md and ask.
- Hardware-dependent steps: implement, generate/refresh docs/HW-TESTS.md, print checklist, stop.
