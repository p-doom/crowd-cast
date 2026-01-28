//! System tray UI and notifications

mod tray;
pub mod tray_ffi;
pub mod notifications;

pub use tray::*;
pub use notifications::{
    init_notifications, is_authorized as notifications_authorized,
    show_capture_resumed_notification, show_display_change_notification, NotificationAction,
};
