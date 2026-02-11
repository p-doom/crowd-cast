//! OS Permission handling for input capture and screen recording

#[allow(unused_imports)]
use anyhow::{Context, Result};
use std::process::Command;
use tracing::{debug, info, warn};

/// Permission status for all required permissions
#[derive(Debug, Clone)]
pub struct PermissionStatus {
    /// Accessibility permission (for keyboard/mouse capture)
    pub accessibility: PermissionState,
    /// Screen recording permission (for window capture)
    pub screen_recording: PermissionState,
    /// Input group membership (Linux Wayland only)
    pub input_group: PermissionState,
}

/// State of a single permission
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionState {
    /// Permission is granted
    Granted,
    /// Permission is denied
    Denied,
    /// Permission status is unknown or not applicable
    Unknown,
    /// Permission is not needed on this platform
    NotApplicable,
}

impl PermissionState {
    pub fn is_granted(&self) -> bool {
        matches!(
            self,
            PermissionState::Granted | PermissionState::NotApplicable
        )
    }
}

/// Check all required permissions for the current platform
pub fn check_permissions() -> PermissionStatus {
    #[cfg(target_os = "macos")]
    {
        PermissionStatus {
            accessibility: check_accessibility_macos(),
            screen_recording: check_screen_recording_macos(),
            input_group: PermissionState::NotApplicable,
        }
    }

    #[cfg(target_os = "linux")]
    {
        PermissionStatus {
            accessibility: PermissionState::NotApplicable,
            screen_recording: PermissionState::NotApplicable,
            input_group: check_input_group_linux(),
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Windows generally doesn't require special permissions for input capture
        PermissionStatus {
            accessibility: PermissionState::NotApplicable,
            screen_recording: PermissionState::NotApplicable,
            input_group: PermissionState::NotApplicable,
        }
    }
}

/// Request all required permissions
pub fn request_permissions() -> Result<PermissionStatus> {
    #[cfg(target_os = "macos")]
    {
        request_permissions_macos()
    }

    #[cfg(target_os = "linux")]
    {
        request_permissions_linux()
    }

    #[cfg(target_os = "windows")]
    {
        Ok(check_permissions())
    }
}

// ============================================================================
// macOS Implementation
// ============================================================================

#[cfg(target_os = "macos")]
fn check_accessibility_macos() -> PermissionState {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    let trusted = unsafe { AXIsProcessTrusted() };
    debug!("macOS Accessibility: trusted={}", trusted);

    if trusted {
        PermissionState::Granted
    } else {
        PermissionState::Denied
    }
}

#[cfg(target_os = "macos")]
fn check_screen_recording_macos() -> PermissionState {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    let has_access = unsafe { CGPreflightScreenCaptureAccess() };
    debug!("macOS Screen Recording: has_access={}", has_access);

    if has_access {
        PermissionState::Granted
    } else {
        PermissionState::Denied
    }
}

#[cfg(target_os = "macos")]
fn request_permissions_macos() -> Result<PermissionStatus> {
    use std::ffi::c_void;

    // CoreFoundation types
    type CFAllocatorRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFBooleanRef = *const c_void;
    type CFIndex = isize;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFAllocatorDefault: CFAllocatorRef;
        static kCFBooleanTrue: CFBooleanRef;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;

        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const i8,
            encoding: u32,
        ) -> CFStringRef;

        fn CFDictionaryCreate(
            allocator: CFAllocatorRef,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: CFIndex,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFDictionaryRef;

        fn CFRelease(cf: *const c_void);
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;

    // Request accessibility permission with prompt
    info!("Requesting Accessibility permission...");
    let accessibility = unsafe {
        // Create the key string "AXTrustedCheckOptionPrompt"
        let key_cstr = b"AXTrustedCheckOptionPrompt\0".as_ptr() as *const i8;
        let key =
            CFStringCreateWithCString(kCFAllocatorDefault, key_cstr, K_CF_STRING_ENCODING_UTF8);

        if key.is_null() {
            warn!("Failed to create CFString for AXTrustedCheckOptionPrompt");
            PermissionState::Denied
        } else {
            let keys: [*const c_void; 1] = [key];
            let values: [*const c_void; 1] = [kCFBooleanTrue];

            let dict = CFDictionaryCreate(
                kCFAllocatorDefault,
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks as *const _ as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const _ as *const c_void,
            );

            let trusted = if !dict.is_null() {
                let result = AXIsProcessTrustedWithOptions(dict);
                CFRelease(dict);
                result
            } else {
                warn!("Failed to create options dictionary");
                false
            };

            CFRelease(key);

            if trusted {
                PermissionState::Granted
            } else {
                // Open System Preferences to the Accessibility pane
                let _ = open_accessibility_settings();
                PermissionState::Denied
            }
        }
    };

    // Request screen recording permission
    info!("Requesting Screen Recording permission...");
    let screen_recording = unsafe {
        let granted = CGRequestScreenCaptureAccess();
        if granted {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        }
    };

    Ok(PermissionStatus {
        accessibility,
        screen_recording,
        input_group: PermissionState::NotApplicable,
    })
}

#[cfg(target_os = "macos")]
pub fn open_accessibility_settings() -> Result<()> {
    Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn()
        .context("Failed to open Accessibility settings")?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn open_screen_recording_settings() -> Result<()> {
    Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn()
        .context("Failed to open Screen Recording settings")?;
    Ok(())
}

/// Prompt for accessibility permission only (shows system dialog)
/// Returns true if already granted, false otherwise
#[cfg(target_os = "macos")]
pub fn prompt_accessibility_permission() -> bool {
    use std::ffi::c_void;

    type CFAllocatorRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFBooleanRef = *const c_void;
    type CFIndex = isize;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFAllocatorDefault: CFAllocatorRef;
        static kCFBooleanTrue: CFBooleanRef;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;

        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const i8,
            encoding: u32,
        ) -> CFStringRef;

        fn CFDictionaryCreate(
            allocator: CFAllocatorRef,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: CFIndex,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFDictionaryRef;

        fn CFRelease(cf: *const c_void);
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;

    unsafe {
        let key_cstr = b"AXTrustedCheckOptionPrompt\0".as_ptr() as *const i8;
        let key =
            CFStringCreateWithCString(kCFAllocatorDefault, key_cstr, K_CF_STRING_ENCODING_UTF8);

        if key.is_null() {
            return false;
        }

        let keys: [*const c_void; 1] = [key];
        let values: [*const c_void; 1] = [kCFBooleanTrue];

        let dict = CFDictionaryCreate(
            kCFAllocatorDefault,
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks as *const _ as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const _ as *const c_void,
        );

        let trusted = if !dict.is_null() {
            let result = AXIsProcessTrustedWithOptions(dict);
            CFRelease(dict);
            result
        } else {
            false
        };

        CFRelease(key);
        trusted
    }
}

/// Prompt for screen recording permission only (shows system dialog)
/// Returns true if granted, false otherwise
#[cfg(target_os = "macos")]
pub fn prompt_screen_recording_permission() -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    unsafe { CGRequestScreenCaptureAccess() }
}

// ============================================================================
// Linux Implementation
// ============================================================================

#[cfg(target_os = "linux")]
fn check_input_group_linux() -> PermissionState {
    // Check if we're on Wayland
    let is_wayland = std::env::var("XDG_SESSION_TYPE")
        .map(|s| s == "wayland")
        .unwrap_or(false);

    if !is_wayland {
        // X11 doesn't need input group
        debug!("Not on Wayland, input group not required");
        return PermissionState::NotApplicable;
    }

    // Check if user is in the input group
    match Command::new("groups").output() {
        Ok(output) => {
            let groups = String::from_utf8_lossy(&output.stdout);
            let in_group = groups.split_whitespace().any(|g| g == "input");
            debug!(
                "User groups: {}, in_input_group={}",
                groups.trim(),
                in_group
            );

            if in_group {
                PermissionState::Granted
            } else {
                PermissionState::Denied
            }
        }
        Err(e) => {
            warn!("Failed to check groups: {}", e);
            PermissionState::Unknown
        }
    }
}

#[cfg(target_os = "linux")]
fn request_permissions_linux() -> Result<PermissionStatus> {
    let input_group = check_input_group_linux();

    if input_group == PermissionState::Denied {
        warn!("User is not in the 'input' group. For Wayland input capture, run:");
        warn!("  sudo usermod -aG input $USER");
        warn!("Then log out and log back in.");
    }

    Ok(PermissionStatus {
        accessibility: PermissionState::NotApplicable,
        screen_recording: PermissionState::NotApplicable,
        input_group,
    })
}

#[cfg(target_os = "linux")]
pub fn add_user_to_input_group() -> Result<()> {
    let username = std::env::var("USER").context("Could not get current username")?;

    info!(
        "Adding user '{}' to input group (requires sudo)...",
        username
    );

    let status = Command::new("sudo")
        .args(["usermod", "-aG", "input", &username])
        .status()
        .context("Failed to run usermod")?;

    if status.success() {
        info!("Successfully added user to input group. Please log out and log back in.");
        Ok(())
    } else {
        anyhow::bail!("Failed to add user to input group")
    }
}

// ============================================================================
// Windows Implementation (stubs)
// ============================================================================

#[cfg(target_os = "windows")]
pub fn open_accessibility_settings() -> Result<()> {
    // Windows doesn't have a direct equivalent
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn open_screen_recording_settings() -> Result<()> {
    // Windows doesn't have a direct equivalent
    Ok(())
}

// ============================================================================
// Common Functions
// ============================================================================

/// Check if all required permissions are granted
pub fn all_permissions_granted() -> bool {
    let status = check_permissions();
    status.accessibility.is_granted()
        && status.screen_recording.is_granted()
        && status.input_group.is_granted()
}

/// Get a human-readable description of missing permissions
pub fn describe_missing_permissions() -> Vec<String> {
    let status = check_permissions();
    let mut missing = Vec::new();

    if !status.accessibility.is_granted() {
        missing.push(
            "Accessibility permission is required for keyboard and mouse capture".to_string(),
        );
    }

    if !status.screen_recording.is_granted() {
        missing.push("Screen Recording permission is required for window capture".to_string());
    }

    if !status.input_group.is_granted() {
        missing.push("User must be in 'input' group for Wayland input capture".to_string());
    }

    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_permissions() {
        let status = check_permissions();
        println!("Permission status: {:?}", status);
    }

    #[test]
    fn test_describe_missing() {
        let missing = describe_missing_permissions();
        println!("Missing permissions: {:?}", missing);
    }
}
