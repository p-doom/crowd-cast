//! Notification center (macOS) — computes the set of glanceable notifications
//! shown as a badge count on the tray icon and listed in a "Notifications"
//! submenu.
//!
//! # Phase 1: derived, not an inbox
//!
//! These notifications are *derived from live app state* rather than stored
//! events. "Uploads are paused" exists exactly while uploads are paused; the
//! moment the user resumes (by clicking the notification, whose action is the
//! existing uploads toggle), the condition clears and the entry vanishes on the
//! next refresh. That means:
//!
//! - No persistence, no read/unread store, no dismiss button.
//! - The badge count is simply the number of currently-active conditions.
//! - Every notification is actionable and self-clearing.
//!
//! Phase 2 (account messages like "You've been paid for July") will need a
//! server `/notifications` endpoint and real server-side read state; that is
//! deliberately out of scope here and does not touch this derived set.

#![cfg(target_os = "macos")]

use super::platform_tray::{TrayAction, TrayNotification};

/// Live inputs the notification set is derived from. Kept as a plain value type
/// (not a reference to `TrayApp`) so the decision logic is pure and unit-testable.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotificationInputs {
    /// Uploads are currently paused by the user.
    pub uploads_paused: bool,
    /// Google sign-in is configured for this build but the user is not signed in.
    pub signed_out: bool,
    // TODO(capture-health): a "screen recording isn't working" notification.
    // Do NOT wire this to `EngineStatus::RecordingBlocked` — that fires routinely
    // whenever a non-captured app is frontmost (e.g. focusing WhatsApp), so it's
    // a false positive. The genuine signal is the dead-source escalation in
    // engine.rs (~:1754, `show_capture_stuck_notification` / the "system-level
    // wedge" path), which is currently a transient event, not a persistent tray
    // status. Wiring this needs a persistent "capture wedged" state broadcast to
    // TrayApp first.
    //
    // TODO(update-pending): needs a "newer version is downloadable" signal from
    // the Sparkle updater. `UpdaterController` only exposes whether the
    // *mechanism* is available, not whether an update is waiting.
}

/// Compute the active notification set from live state, highest-priority first.
///
/// Order matters: the first entries are the most actionable / most likely to be
/// silently losing data, so they sort to the top of the submenu.
pub fn compute(inputs: NotificationInputs) -> Vec<TrayNotification> {
    let mut out = Vec::new();

    // Uploads paused: data is being recorded but not shipped. Clicking resumes.
    if inputs.uploads_paused {
        out.push(TrayNotification {
            title: "Uploads are paused".to_string(),
            action: TrayAction::ToggleUploads,
        });
    }

    // Signed out: recordings upload under an anonymous id and the user misses
    // account features. Clicking starts sign-in.
    if inputs.signed_out {
        out.push(TrayNotification {
            title: "You're not signed in".to_string(),
            action: TrayAction::SignIn,
        });
    }

    out
}

/// Badge text for a notification count: "" for 0, "1".."9", then "9+".
/// (menu-bar badges have room for at most ~2 glyphs.)
pub fn badge_text(count: usize) -> String {
    match count {
        0 => String::new(),
        1..=9 => count.to_string(),
        _ => "9+".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_when_all_healthy() {
        assert!(compute(NotificationInputs::default()).is_empty());
    }

    #[test]
    fn each_condition_yields_one_notification() {
        let n = compute(NotificationInputs {
            uploads_paused: true,
            signed_out: false,
        });
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].action, TrayAction::ToggleUploads);
    }

    #[test]
    fn both_conditions_present_uploads_first() {
        let n = compute(NotificationInputs {
            uploads_paused: true,
            signed_out: true,
        });
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].action, TrayAction::ToggleUploads);
        assert_eq!(n[1].action, TrayAction::SignIn);
    }

    #[test]
    fn badge_text_caps_at_nine_plus() {
        assert_eq!(badge_text(0), "");
        assert_eq!(badge_text(1), "1");
        assert_eq!(badge_text(9), "9");
        assert_eq!(badge_text(10), "9+");
        assert_eq!(badge_text(999), "9+");
    }
}
