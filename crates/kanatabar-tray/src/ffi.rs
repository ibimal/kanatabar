//! The tray's macOS-framework calls: `UNUserNotificationCenter` delivery
//! (SPEC §8), kept in one module so the ObjC surface stays reviewable.
//!
//! The objc2 bindings used here are all safe wrappers — this crate still
//! contains **zero `unsafe`** (the CLAUDE.md confinement rule for unsafe
//! remains satisfied trivially). The framework requires the process to run
//! from an app bundle: `currentNotificationCenter` raises an ObjC exception
//! in a bundle-less process, so callers must gate on [`bundle_identifier`]
//! first (the reason unbundled dev builds use the osascript fallback).
#![deny(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNUserNotificationCenter,
};

/// The main bundle's identifier; `None` when running unbundled (a bare
/// `cargo build` binary launched outside an `.app`).
pub fn bundle_identifier() -> Option<String> {
    NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|s| s.to_string())
}

/// Ask the user to allow notifications (the one-time system prompt; macOS
/// remembers the answer per bundle id). Must only be called when
/// [`bundle_identifier`] returned `Some`.
pub fn request_notification_authorization() {
    // The completion handler receives a raw `*mut NSError`; we never
    // dereference it — granted/denied is all the log needs.
    let handler = RcBlock::new(|granted: Bool, _error: *mut NSError| {
        if granted.as_bool() {
            tracing::info!("notification authorization granted");
        } else {
            // Denied (or previously denied): the system drops notifications
            // silently; log so HW runs can tell this apart from a delivery
            // bug.
            tracing::warn!("notification authorization denied — notifications will not appear");
        }
    });
    current_center().requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &handler,
    );
}

/// Post a notification through Notification Center. Must only be called when
/// [`bundle_identifier`] returned `Some`. The center singleton is documented
/// thread-safe, so posting from a blocking-pool thread is fine.
pub fn post_notification(title: &str, body: &str) {
    // Distinct identifiers so successive notifications don't replace each
    // other (equal ids update-in-place per the framework contract).
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let id = format!("kanatabar-{}", SEQ.fetch_add(1, Ordering::Relaxed));

    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    // Trigger `None` = deliver immediately; completion handler `None` is
    // allowed by the API (fire and forget — delivery failures are the
    // system's to report).
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&id),
        &content,
        None,
    );
    current_center().addNotificationRequest_withCompletionHandler(&request, None);
}

/// The process's notification center. Raises an ObjC exception when the
/// process has no bundle — which is why every public entry point above
/// requires a prior [`bundle_identifier`] gate.
fn current_center() -> objc2::rc::Retained<UNUserNotificationCenter> {
    UNUserNotificationCenter::currentNotificationCenter()
}
