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

// Shared C ABI implemented by the native wizard on each platform
// (src/ui/wizard_darwin.m on macOS, src/ui/wizard_linux.c on Linux).
#[cfg(any(target_os = "macos", target_os = "linux"))]
extern "C" {
    /// Set the list of available apps for selection
    fn wizard_set_apps(apps: *const WizardAppInfo, count: usize);

    /// Run the setup wizard (blocks until wizard closes)
    fn wizard_run(config: *mut WizardConfig) -> i32;

    /// Free memory allocated by the wizard for selected_apps
    fn wizard_free_result(config: *mut WizardConfig);
}

// macOS-only permission helpers (TCC). No Linux equivalent in the wizard ABI.
#[cfg(target_os = "macos")]
extern "C" {
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
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn set_available_apps(apps: &[AppInfoWrapper]) {
    let ffi_apps: Vec<WizardAppInfo> = apps.iter().map(|a| a.as_ffi()).collect();
    unsafe {
        wizard_set_apps(ffi_apps.as_ptr(), ffi_apps.len());
    }
}

/// Run the native wizard and return the result
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn run_native_wizard(autostart_default: bool) -> NativeWizardResult {
    let mut config = WizardConfig::default();
    // Seed the "Start on login" checkbox with the saved preference so a re-opened wizard
    // shows the user's actual state (the native side reads this as the initial value).
    config.enable_autostart = autostart_default;

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

// Stubs for platforms without a native wizard (e.g. Windows)
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn set_available_apps(_apps: &[AppInfoWrapper]) {}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn run_native_wizard(_autostart_default: bool) -> NativeWizardResult {
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

// ---- Linux: host requirement checklist (rendered + gated by the GTK wizard) ----

/// Matches the C `WizardRequirement` in src/ui/wizard_linux.c.
/// `severity`: 0 = Required, 1 = Recommended, 2 = Optional.
#[repr(C)]
pub struct WizardRequirement {
    pub label: *const c_char,
    pub detail: *const c_char,
    pub command: *const c_char,
    pub severity: u32,
    pub satisfied: bool,
}

#[cfg(target_os = "linux")]
extern "C" {
    fn wizard_set_requirements(reqs: *const WizardRequirement, count: usize);
    fn wizard_set_per_app_available(available: bool);
}

/// Pass the detected host requirements to the GTK wizard for display + gating.
#[cfg(target_os = "linux")]
pub fn set_requirements(reqs: &[crate::installer::requirements::Requirement]) {
    let labels: Vec<CString> = reqs
        .iter()
        .map(|r| CString::new(r.label.as_str()).unwrap_or_default())
        .collect();
    let details: Vec<CString> = reqs
        .iter()
        .map(|r| CString::new(r.detail.as_str()).unwrap_or_default())
        .collect();
    let commands: Vec<CString> = reqs
        .iter()
        .map(|r| CString::new(r.command.as_str()).unwrap_or_default())
        .collect();
    let ffi: Vec<WizardRequirement> = reqs
        .iter()
        .enumerate()
        .map(|(i, r)| WizardRequirement {
            label: labels[i].as_ptr(),
            detail: details[i].as_ptr(),
            command: commands[i].as_ptr(),
            severity: r.severity as u32,
            satisfied: r.satisfied,
        })
        .collect();
    unsafe {
        wizard_set_requirements(ffi.as_ptr(), ffi.len());
    }
    // `labels`/`details`/`commands` must outlive the call; the C side strdup's them.
    drop(labels);
    drop(details);
    drop(commands);
}

/// Tell the wizard whether per-app capture is available; when false it greys out the
/// per-app picker and forces full-screen capture.
#[cfg(target_os = "linux")]
pub fn set_per_app_available(available: bool) {
    unsafe { wizard_set_per_app_available(available) }
}
