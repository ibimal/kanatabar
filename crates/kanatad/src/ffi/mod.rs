//! FFI boundary for kanatad.
//!
//! [HARD] All `unsafe` in the daemon lives under this module (CLAUDE.md,
//! SPEC §14): each block carries a `// SAFETY:` comment and is wrapped in a
//! safe API. Phase 2 needs only socket peer credentials; Phases 4–5 add IOKit
//! device notifications and `IORegisterForSystemPower` here.

pub mod iokit;
pub mod peercred;
pub mod power;
pub mod proc;
