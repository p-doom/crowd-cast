//! macOS notification support using UNUserNotificationCenter
//!
//! Provides informational notifications for display changes and recording state.
//! Since display switching is automatic, notifications are purely informational.

use std::ffi::CString;
use std::sync::OnceLock;
use tokio::sync::mpsc;
// Not every macro is used on every platform (the macOS arms use them all; Linux routes through
// `notify_linux`, other platforms only `debug!`/`info!`).
#[allow(unused_imports)]
use tracing::{debug, error, info, warn};

/// Actions that can be triggered from notifications
#[derive(Debug, Clone)]
pub enum NotificationAction {
    /// User dismissed or tapped the notification
    Dismissed,
}

/// Channel sender for notification actions (set once during init)
static ACTION_SENDER: OnceLock<mpsc::UnboundedSender<NotificationAction>> = OnceLock::new();

// FFI declarations for the Objective-C implementation
#[cfg(target_os = "macos")]
mod ffi {
    use std::ffi::c_char;

    /// Callback type for notification actions
    pub type NotificationActionCallback = extern "C" fn(action_id: *const c_char, display_id: u32);

    #[link(name = "notifications_darwin", kind = "static")]
    extern "C" {
        pub fn notifications_init(callback: NotificationActionCallback) -> i32;
        pub fn notifications_show_display_change(
            from_display: *const c_char,
            to_display: *const c_char,
            to_display_id: u32,
        );
        pub fn notifications_show_capture_resumed(display_name: *const c_char);
        pub fn notifications_show_recording_started();
        pub fn notifications_show_recording_stopped();
        pub fn notifications_show_recording_paused();
        pub fn notifications_show_recording_resumed();
        pub fn notifications_show_permissions_missing(message: *const c_char);
        pub fn notifications_show_obs_download_started();
        pub fn notifications_show_setup_configuring();
        pub fn notifications_show_sources_refreshed();
        pub fn notifications_show_idle_paused();
        pub fn notifications_show_idle_resumed();
        pub fn notifications_show_update_installing();
        pub fn notifications_show_update_completed(version: *const c_char, build: *const c_char);
        pub fn notifications_is_authorized() -> i32;
    }
}

/// Callback function called from Objective-C when user interacts with notification
#[cfg(target_os = "macos")]
extern "C" fn notification_action_callback(action_id: *const std::ffi::c_char, display_id: u32) {
    let action_str = if action_id.is_null() {
        ""
    } else {
        unsafe { std::ffi::CStr::from_ptr(action_id).to_str().unwrap_or("") }
    };

    debug!(
        "Notification action received: action={}, display_id={}",
        action_str, display_id
    );

    let action = match action_str {
        "dismiss" | "default" => NotificationAction::Dismissed,
        _ => {
            warn!("Unknown notification action: {}", action_str);
            NotificationAction::Dismissed
        }
    };

    if let Some(sender) = ACTION_SENDER.get() {
        if let Err(e) = sender.send(action) {
            error!("Failed to send notification action: {}", e);
        }
    }
}

/// Initialize the notification system and request permissions
///
/// Must be called before showing any notifications. The provided sender
/// will receive notification actions when the user interacts with them.
///
/// Returns Ok(()) if initialization succeeded, Err if it failed.
#[cfg(target_os = "macos")]
pub fn init_notifications(
    action_sender: mpsc::UnboundedSender<NotificationAction>,
) -> Result<(), String> {
    // Store the sender for the callback
    ACTION_SENDER
        .set(action_sender)
        .map_err(|_| "Notification system already initialized")?;

    let result = unsafe { ffi::notifications_init(notification_action_callback) };

    if result == 0 {
        info!("Notification system initialized");
        Ok(())
    } else {
        Err("Failed to initialize notification system".to_string())
    }
}

/// Non-macOS notification dispatch. On Linux this surfaces a real desktop notification through
/// the freedesktop D-Bus service (`notify_linux`), mirroring the macOS UNUserNotificationCenter
/// path; on other platforms it's a no-op. Centralized so every `show_*` function below stays a
/// one-liner carrying the same copy as its macOS counterpart.
#[cfg(not(target_os = "macos"))]
fn emit(summary: &str, body: &str) {
    #[cfg(target_os = "linux")]
    {
        // Waits for the D-Bus send to flush (bounded). The agent restarts often, so a
        // fire-and-forget dispatch would be killed by process exit before the daemon got it.
        super::notify_linux::notify_blocking(summary, body);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (summary, body);
        debug!("Notifications not supported on this platform");
    }
}

/// Initialize notifications (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn init_notifications(
    _action_sender: mpsc::UnboundedSender<NotificationAction>,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if is_authorized() {
            info!("Linux desktop notifications enabled (org.freedesktop.Notifications)");
        } else {
            info!(
                "No desktop notification daemon is running; notifications are unavailable \
                 until one is started (e.g. mako/dunst on bare wlroots)"
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    info!("Notifications not supported on this platform");
    Ok(())
}

/// Show notification when display changes
///
/// Displays a notification with "Switch Display" and "Ignore" action buttons.
/// The `to_display_id` is passed back in the callback when user clicks "Switch".
#[cfg(target_os = "macos")]
pub fn show_display_change_notification(from_display: &str, to_display: &str, to_display_id: u32) {
    let from_c = match CString::new(from_display) {
        Ok(s) => s,
        Err(e) => {
            error!("Invalid from_display string: {}", e);
            return;
        }
    };
    let to_c = match CString::new(to_display) {
        Ok(s) => s,
        Err(e) => {
            error!("Invalid to_display string: {}", e);
            return;
        }
    };

    unsafe {
        ffi::notifications_show_display_change(from_c.as_ptr(), to_c.as_ptr(), to_display_id);
    }

    debug!(
        "Showed display change notification: {} -> {} (id: {})",
        from_display, to_display, to_display_id
    );
}

/// Show display change notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_display_change_notification(from_display: &str, to_display: &str, _to_display_id: u32) {
    emit(
        "Display Changed",
        &format!(
            "Display changed: {to_display} (was {from_display}). Finalizing and restarting \
             recording to match the new display."
        ),
    );
}

/// Show notification when capture resumes on original display
#[cfg(target_os = "macos")]
pub fn show_capture_resumed_notification(display_name: &str) {
    let name_c = match CString::new(display_name) {
        Ok(s) => s,
        Err(e) => {
            error!("Invalid display_name string: {}", e);
            return;
        }
    };

    unsafe {
        ffi::notifications_show_capture_resumed(name_c.as_ptr());
    }

    debug!("Showed capture resumed notification: {}", display_name);
}

/// Show capture resumed notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_capture_resumed_notification(display_name: &str) {
    emit(
        "Capture Resumed",
        &format!("Recording restarted on {display_name}"),
    );
}

/// Show notification when recording starts
#[cfg(target_os = "macos")]
pub fn show_recording_started_notification() {
    unsafe {
        ffi::notifications_show_recording_started();
    }

    debug!("Showed recording started notification");
}

/// Show recording started notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_recording_started_notification() {
    emit("Recording started", "");
}

/// Show notification when recording stops
#[cfg(target_os = "macos")]
pub fn show_recording_stopped_notification() {
    unsafe {
        ffi::notifications_show_recording_stopped();
    }

    debug!("Showed recording stopped notification");
}

/// Show recording stopped notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_recording_stopped_notification() {
    emit("Recording stopped", "");
}

/// Show notification when recording is paused
#[cfg(target_os = "macos")]
pub fn show_recording_paused_notification() {
    unsafe {
        ffi::notifications_show_recording_paused();
    }

    debug!("Showed recording paused notification");
}

/// Show recording paused notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_recording_paused_notification() {
    emit("Recording paused", "");
}

/// Show notification when recording is resumed
#[cfg(target_os = "macos")]
pub fn show_recording_resumed_notification() {
    unsafe {
        ffi::notifications_show_recording_resumed();
    }

    debug!("Showed recording resumed notification");
}

/// Show recording resumed notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_recording_resumed_notification() {
    emit("Recording resumed", "");
}

/// Show notification when recording is blocked by missing permissions
#[cfg(target_os = "macos")]
pub fn show_permissions_missing_notification(message: &str) {
    let msg_c = match CString::new(message) {
        Ok(s) => s,
        Err(e) => {
            error!("Invalid permissions message string: {}", e);
            return;
        }
    };

    unsafe {
        ffi::notifications_show_permissions_missing(msg_c.as_ptr());
    }

    debug!("Showed permissions missing notification");
}

/// Show permissions missing notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_permissions_missing_notification(message: &str) {
    let body = if message.is_empty() {
        "Recording not started. Required permissions are missing."
    } else {
        message
    };
    emit("Permissions Required", body);
}

/// Show notification when OBS download starts
#[cfg(target_os = "macos")]
pub fn show_obs_download_started_notification() {
    unsafe {
        ffi::notifications_show_obs_download_started();
    }

    debug!("Showed OBS download started notification");
}

/// Show OBS download started notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_obs_download_started_notification() {
    emit(
        "Downloading OBS",
        "Preparing capture components. This may take a minute.",
    );
}

/// Show notification when post-wizard setup starts
#[cfg(target_os = "macos")]
pub fn show_setup_configuring_notification() {
    unsafe {
        ffi::notifications_show_setup_configuring();
    }

    debug!("Showed setup configuring notification");
}

/// Show setup configuring notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_setup_configuring_notification() {
    emit(
        "Setting up Crowd-Cast",
        "Configuring components in the background. OBS installation will start shortly.",
    );
}

/// Show notification when capture sources are refreshed
#[cfg(target_os = "macos")]
pub fn show_sources_refreshed_notification() {
    unsafe {
        ffi::notifications_show_sources_refreshed();
    }

    debug!("Showed sources refreshed notification");
}

/// Show sources refreshed notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_sources_refreshed_notification() {
    emit("Sources refreshed", "Capture sources updated.");
}

/// Show notification when recording is paused due to user inactivity
#[cfg(target_os = "macos")]
pub fn show_idle_paused_notification() {
    unsafe {
        ffi::notifications_show_idle_paused();
    }

    debug!("Showed idle paused notification");
}

/// Show idle paused notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_idle_paused_notification() {
    emit("Recording paused (idle)", "");
}

/// Show notification when recording resumes after user activity detected
#[cfg(target_os = "macos")]
pub fn show_idle_resumed_notification() {
    unsafe {
        ffi::notifications_show_idle_resumed();
    }

    debug!("Showed idle resumed notification");
}

/// Show idle resumed notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_idle_resumed_notification() {
    emit("Recording resumed", "");
}

/// Show notification when an update is being installed
#[cfg(target_os = "macos")]
pub fn show_update_installing_notification() {
    unsafe {
        ffi::notifications_show_update_installing();
    }

    debug!("Showed update installing notification");
}

/// Show update installing notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_update_installing_notification() {
    emit(
        "Update Available",
        "A new version of CrowdCast is being installed. The app will restart shortly.",
    );
}

/// Show notification after a background update completed
#[cfg(target_os = "macos")]
pub fn show_update_completed_notification(version: &str, build: &str) {
    let version_c = CString::new(version).unwrap_or_default();
    let build_c = CString::new(build).unwrap_or_default();
    unsafe {
        ffi::notifications_show_update_completed(version_c.as_ptr(), build_c.as_ptr());
    }
    debug!(
        "Showed update completed notification: {} ({})",
        version, build
    );
}

/// Show update completed notification (non-macOS).
#[cfg(not(target_os = "macos"))]
pub fn show_update_completed_notification(version: &str, build: &str) {
    // Omit "(build ...)" when the caller has no platform build string, rather than rendering
    // an ugly empty build suffix.
    let body = match (version.is_empty(), build.is_empty()) {
        (true, _) => "A new version was installed in the background.".to_string(),
        (false, true) => format!("Updated to version {version}."),
        (false, false) => format!("Updated to version {version} (build {build})."),
    };
    emit("CrowdCast Updated", &body);
}

/// Check if notifications are authorized
///
/// Returns true if the user has granted notification permission.
#[cfg(target_os = "macos")]
pub fn is_authorized() -> bool {
    let result = unsafe { ffi::notifications_is_authorized() };
    result == 1
}

/// Check notification availability (non-macOS). On Linux this reflects whether a desktop
/// notification daemon is present (the analog of macOS authorization); elsewhere it's false.
#[cfg(not(target_os = "macos"))]
pub fn is_authorized() -> bool {
    #[cfg(target_os = "linux")]
    {
        super::notify_linux::service_available()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}
