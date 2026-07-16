//! kanatad — KanataBar root daemon library (SPEC §6).
//!
//! Split bin/lib so the supervisor and control server can be driven in-process
//! by integration tests against mock-kanata, without root, devices, or real
//! kanata (SPEC §17). All `unsafe` FFI lives in `src/ffi/` only (CLAUDE.md).

#![deny(unsafe_op_in_unsafe_fn)]

pub mod child;
pub mod config;
pub mod configmgr;
pub mod control;
pub mod device;
pub mod doctor;
pub mod events;
mod ffi;
pub mod health;
pub mod logbuf;
pub mod statefile;
pub mod supervisor;
pub mod watch;
