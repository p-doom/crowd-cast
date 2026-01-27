//! Raw FFI bindings to dmikushin/tray C library
//!
//! These are low-level bindings. Use the safe wrapper in `tray.rs` instead.

use std::os::raw::{c_char, c_int};

/// Tray icon and menu configuration
#[repr(C)]
pub struct Tray {
    /// Path to icon file (PNG on macOS/Linux, ICO on Windows)
    pub icon_filepath: *const c_char,
    /// Tooltip text shown on hover
    pub tooltip: *const c_char,
    /// Callback for left-click on tray icon (NULL to just open menu)
    pub cb: Option<unsafe extern "C" fn(*mut Tray)>,
    /// NULL-terminated array of menu items
    pub menu: *mut TrayMenuItem,
}

/// Menu item configuration
#[repr(C)]
pub struct TrayMenuItem {
    /// Menu item text (use "-" for separator, NULL to terminate array)
    pub text: *const c_char,
    /// Whether item is disabled/grayed out (0 = enabled, 1 = disabled)
    pub disabled: c_int,
    /// Whether item is checked (0 = unchecked, 1 = checked)
    pub checked: c_int,
    /// Callback when item is selected
    pub cb: Option<unsafe extern "C" fn(*mut TrayMenuItem)>,
    /// NULL-terminated array of submenu items (NULL if no submenu)
    pub submenu: *mut TrayMenuItem,
}

// Only include FFI functions when tray is available
#[cfg(not(no_tray))]
extern "C" {
    /// Initialize the tray icon and menu
    /// Returns 0 on success, -1 on failure
    pub fn tray_init(tray: *mut Tray) -> c_int;

    /// Run one iteration of the event loop
    /// If blocking is non-zero, blocks until an event occurs
    /// Returns 0 normally, -1 if tray_exit() was called
    pub fn tray_loop(blocking: c_int) -> c_int;

    /// Update the tray icon, tooltip, and menu
    pub fn tray_update(tray: *mut Tray);

    /// Signal the event loop to exit
    pub fn tray_exit();
}

// Stub implementations when tray is not available
#[cfg(no_tray)]
pub unsafe fn tray_init(_tray: *mut Tray) -> c_int {
    -1
}

#[cfg(no_tray)]
pub unsafe fn tray_loop(_blocking: c_int) -> c_int {
    std::thread::sleep(std::time::Duration::from_millis(100));
    0
}

#[cfg(no_tray)]
pub unsafe fn tray_update(_tray: *mut Tray) {}

#[cfg(no_tray)]
pub unsafe fn tray_exit() {}

impl Default for Tray {
    fn default() -> Self {
        Self {
            icon_filepath: std::ptr::null(),
            tooltip: std::ptr::null(),
            cb: None,
            menu: std::ptr::null_mut(),
        }
    }
}

impl Default for TrayMenuItem {
    fn default() -> Self {
        Self {
            text: std::ptr::null(),
            disabled: 0,
            checked: 0,
            cb: None,
            submenu: std::ptr::null_mut(),
        }
    }
}
