//! FFI bindings for the native macOS setup wizard
//!
//! This module provides Rust bindings to the Objective-C wizard implementation.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

/// App info structure matching the C definition
#[repr(C)]
pub struct WizardAppInfo {
    pub bundle_id: *const c_char,
    pub name: *const c_char,
    pub pid: u32,
}

/// Configuration structure for wizard results
#[repr(C)]
pub struct WizardConfig {
    pub capture_all: bool,
    pub enable_autostart: bool,
    pub selected_apps: *const *const c_char,
    pub selected_apps_count: usize,
    pub completed: bool,
    pub cancelled: bool,
}

impl Default for WizardConfig {
    fn default() -> Self {
        Self {
            capture_all: false,
            enable_autostart: true,
            selected_apps: std::ptr::null(),
            selected_apps_count: 0,
            completed: false,
            cancelled: false,
        }
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    /// Set the list of available apps for selection
    fn wizard_set_apps(apps: *const WizardAppInfo, count: usize);

    /// Run the setup wizard (blocks until wizard closes)
    fn wizard_run(config: *mut WizardConfig) -> i32;

    /// Free memory allocated by the wizard for selected_apps
    fn wizard_free_result(config: *mut WizardConfig);

    /// Check accessibility permission status (1 = granted, 0 = denied)
    fn wizard_check_accessibility() -> i32;

    /// Check screen recording permission status (1 = granted, 0 = denied)
    fn wizard_check_screen_recording() -> i32;

    /// Check notification permission status (1 = granted, 0 = denied)
    fn wizard_check_notifications() -> i32;

    /// Request accessibility permission (shows system prompt)
    fn wizard_request_accessibility() -> i32;

    /// Request screen recording permission (shows system prompt)
    fn wizard_request_screen_recording() -> i32;

    /// Request notification permission (shows system prompt)
    fn wizard_request_notifications();

    /// Open System Preferences to Accessibility pane
    fn wizard_open_accessibility_settings();

    /// Open System Preferences to Screen Recording pane
    fn wizard_open_screen_recording_settings();

    /// Open System Preferences to Notifications pane
    fn wizard_open_notifications_settings();
}

/// Rust-friendly wrapper for wizard app info
pub struct AppInfoWrapper {
    bundle_id: CString,
    name: CString,
    pid: u32,
}

impl AppInfoWrapper {
    pub fn new(bundle_id: &str, name: &str, pid: u32) -> Self {
        Self {
            bundle_id: CString::new(bundle_id).unwrap_or_default(),
            name: CString::new(name).unwrap_or_default(),
            pid,
        }
    }

    fn as_ffi(&self) -> WizardAppInfo {
        WizardAppInfo {
            bundle_id: self.bundle_id.as_ptr(),
            name: self.name.as_ptr(),
            pid: self.pid,
        }
    }
}

/// Result of running the wizard
#[derive(Debug, Clone)]
pub struct NativeWizardResult {
    pub completed: bool,
    pub cancelled: bool,
    pub capture_all: bool,
    pub enable_autostart: bool,
    pub selected_apps: Vec<String>,
}

/// Set the available apps for the wizard to display
#[cfg(target_os = "macos")]
pub fn set_available_apps(apps: &[AppInfoWrapper]) {
    let ffi_apps: Vec<WizardAppInfo> = apps.iter().map(|a| a.as_ffi()).collect();
    unsafe {
        wizard_set_apps(ffi_apps.as_ptr(), ffi_apps.len());
    }
}

/// Run the native wizard and return the result
#[cfg(target_os = "macos")]
pub fn run_native_wizard() -> NativeWizardResult {
    let mut config = WizardConfig::default();

    let _result = unsafe { wizard_run(&mut config) };

    // Extract selected apps
    let mut selected_apps = Vec::new();
    if !config.selected_apps.is_null() && config.selected_apps_count > 0 {
        unsafe {
            for i in 0..config.selected_apps_count {
                let ptr = *config.selected_apps.add(i);
                if !ptr.is_null() {
                    if let Ok(s) = CStr::from_ptr(ptr).to_str() {
                        selected_apps.push(s.to_string());
                    }
                }
            }
            // Free the memory allocated by the wizard
            wizard_free_result(&mut config);
        }
    }

    NativeWizardResult {
        completed: config.completed,
        cancelled: config.cancelled,
        capture_all: config.capture_all,
        enable_autostart: config.enable_autostart,
        selected_apps,
    }
}

/// Check if accessibility permission is granted
#[cfg(target_os = "macos")]
pub fn check_accessibility() -> bool {
    unsafe { wizard_check_accessibility() == 1 }
}

/// Check if screen recording permission is granted
#[cfg(target_os = "macos")]
pub fn check_screen_recording() -> bool {
    unsafe { wizard_check_screen_recording() == 1 }
}

/// Request accessibility permission
#[cfg(target_os = "macos")]
pub fn request_accessibility() -> bool {
    unsafe { wizard_request_accessibility() == 1 }
}

/// Request screen recording permission
#[cfg(target_os = "macos")]
pub fn request_screen_recording() -> bool {
    unsafe { wizard_request_screen_recording() == 1 }
}

/// Open accessibility settings
#[cfg(target_os = "macos")]
pub fn open_accessibility_settings() {
    unsafe { wizard_open_accessibility_settings() }
}

/// Open screen recording settings
#[cfg(target_os = "macos")]
pub fn open_screen_recording_settings() {
    unsafe { wizard_open_screen_recording_settings() }
}

/// Check if notification permission is granted
#[cfg(target_os = "macos")]
pub fn check_notifications() -> bool {
    unsafe { wizard_check_notifications() == 1 }
}

/// Request notification permission
#[cfg(target_os = "macos")]
pub fn request_notifications() {
    unsafe { wizard_request_notifications() }
}

/// Open notification settings
#[cfg(target_os = "macos")]
pub fn open_notifications_settings() {
    unsafe { wizard_open_notifications_settings() }
}

// Non-macOS stubs
#[cfg(not(target_os = "macos"))]
pub fn set_available_apps(_apps: &[AppInfoWrapper]) {}

#[cfg(not(target_os = "macos"))]
pub fn run_native_wizard() -> NativeWizardResult {
    NativeWizardResult {
        completed: false,
        cancelled: true,
        capture_all: false,
        enable_autostart: false,
        selected_apps: vec![],
    }
}

#[cfg(not(target_os = "macos"))]
pub fn check_accessibility() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn check_screen_recording() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn check_notifications() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn request_accessibility() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn request_screen_recording() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn request_notifications() {}

#[cfg(not(target_os = "macos"))]
pub fn open_accessibility_settings() {}

#[cfg(not(target_os = "macos"))]
pub fn open_screen_recording_settings() {}

#[cfg(not(target_os = "macos"))]
pub fn open_notifications_settings() {}
