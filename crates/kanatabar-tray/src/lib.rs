//! kanatabar-tray library (SPEC §8).
//!
//! Everything here is UI-toolkit-free and unit-testable without a display —
//! the Phase 7 gate's `[AUTO]` half (SPEC §19). `main.rs` is the thin GUI shell
//! (tao event loop, `tray-icon`, `muda`) that drives these types; it is
//! exercised only by the `[HW]` visual/menu checklist.
//!
//! - [`model`] — pure state → menu view-model.
//! - [`session`] — folds the event stream into a live model (the reducer).
//! - [`conn`] — the async control-socket client loop (connect/subscribe/
//!   reconnect) that produces [`conn::Update`]s.
//! - [`menu`] — stable menu-item ids and the click → request mapping.
//! - [`icons`] — template-image status glyphs.
//! - [`notify`] — the crash/degraded/recovery notification decision + delivery.
//! - [`reconnect`] — exponential reconnect timing.
//! - [`login`] — the "Launch at Login" LaunchAgent toggle.
//! - [`single_instance`] — the per-user single-instance lock.
//! - [`wizard`] — the first-run setup-wizard step model (SPEC §11).
//! - [`devwin`] — the devices-window view-model (SPEC §8, Phase 12).

pub mod conn;
pub mod devwin;
pub mod ffi;
pub mod icons;
pub mod login;
pub mod menu;
pub mod model;
pub mod notify;
pub mod reconnect;
pub mod session;
pub mod single_instance;
pub mod wizard;
