//! System tray UI and notifications

pub mod notifications;
mod tray;
pub mod tray_ffi;
mod updater;

pub use notifications::{
    init_notifications, is_authorized as notifications_authorized,
    show_capture_resumed_notification, show_display_change_notification,
    show_obs_download_started_notification, show_permissions_missing_notification,
    show_recording_started_notification, show_recording_stopped_notification,
    show_setup_configuring_notification, show_sources_refreshed_notification,
    show_update_completed_notification, show_update_installing_notification, NotificationAction,
};
pub use tray::*;
pub use updater::UpdaterController;

#[cfg(target_os = "macos")]
pub fn current_app_bundle_path() -> Option<std::path::PathBuf> {
    use std::ffi::OsStr;

    let exe = std::env::current_exe().ok()?;
    let macos_dir = exe.parent()?;
    if macos_dir.file_name() != Some(OsStr::new("MacOS")) {
        return None;
    }

    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name() != Some(OsStr::new("Contents")) {
        return None;
    }

    let app_dir = contents_dir.parent()?;
    if app_dir.extension() != Some(OsStr::new("app")) {
        return None;
    }

    Some(app_dir.to_path_buf())
}

/// Returns true when running from a macOS .app bundle.
#[cfg(target_os = "macos")]
pub fn is_running_in_app_bundle() -> bool {
    current_app_bundle_path().is_some()
}

/// Non-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn is_running_in_app_bundle() -> bool {
    false
}

/// Non-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn current_app_bundle_path() -> Option<std::path::PathBuf> {
    None
}
