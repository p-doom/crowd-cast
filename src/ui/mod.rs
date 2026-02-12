//! System tray UI and notifications

pub mod notifications;
mod tray;
pub mod tray_ffi;

pub use notifications::{
    init_notifications, is_authorized as notifications_authorized,
    show_capture_resumed_notification, show_display_change_notification,
    show_obs_download_started_notification, show_permissions_missing_notification,
    show_recording_started_notification, show_recording_stopped_notification,
    show_setup_configuring_notification, show_sources_refreshed_notification, NotificationAction,
};
pub use tray::*;

/// Returns true when running from a macOS .app bundle.
#[cfg(target_os = "macos")]
pub fn is_running_in_app_bundle() -> bool {
    use std::ffi::OsStr;

    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return false,
    };

    let macos_dir = match exe.parent() {
        Some(path) => path,
        None => return false,
    };

    if macos_dir.file_name() != Some(OsStr::new("MacOS")) {
        return false;
    }

    let contents_dir = match macos_dir.parent() {
        Some(path) => path,
        None => return false,
    };

    if contents_dir.file_name() != Some(OsStr::new("Contents")) {
        return false;
    }

    let app_dir = match contents_dir.parent() {
        Some(path) => path,
        None => return false,
    };

    app_dir.extension() == Some(OsStr::new("app"))
}

/// Non-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn is_running_in_app_bundle() -> bool {
    false
}
