//! Pure logic for KanataBar: supervisor state machine, IPC protocol types,
//! config model, and backoff/debounce computation.
//!
//! [HARD] This crate performs **no I/O and no FFI** (SPEC §5): every state
//! transition, backoff computation, IPC (de)serialization, and config
//! validation rule must be unit-testable on any OS.

#![warn(missing_docs)]

pub mod backoff;
pub mod config;
pub mod debounce;
pub mod device;
pub mod doctor;
pub mod driver;
pub mod ipc;
pub mod kanata;
pub mod logring;
pub mod machine;
pub mod pathsafety;
pub mod state;
pub mod vhidd;

/// Control-protocol version carried in every response/event (SPEC §7.1).
pub const PROTOCOL_VERSION: u32 = 1;
