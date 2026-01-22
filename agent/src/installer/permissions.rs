//! OS Permission handling for input capture and screen recording

#[allow(unused_imports)]
use anyhow::{Context, Result};
use std::process::Command;
use tracing::{debug, info};

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
        matches!(self, PermissionState::Granted | PermissionState::NotApplicable)
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
    
    // Opaque type for CFDictionary callbacks
    #[repr(C)]
    struct CFDictionaryCallBacks {
        _data: [u8; 0],
        _marker: std::marker::PhantomData<(*mut u8, std::marker::PhantomPinned)>,
    }
    
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    }
    
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const CFDictionaryCallBacks,
            value_callbacks: *const CFDictionaryCallBacks,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        static kCFBooleanTrue: *const c_void;
        static kAXTrustedCheckOptionPrompt: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: CFDictionaryCallBacks;
        static kCFTypeDictionaryValueCallBacks: CFDictionaryCallBacks;
    }
    
    // Request accessibility permission with prompt
    info!("Requesting Accessibility permission...");
    let accessibility = unsafe {
        let key = kAXTrustedCheckOptionPrompt;
        let value = kCFBooleanTrue;
        let dict = CFDictionaryCreate(
            std::ptr::null(),
            &key as *const *const c_void,
            &value as *const *const c_void,
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        
        let trusted = if !dict.is_null() {
            let result = AXIsProcessTrustedWithOptions(dict);
            CFRelease(dict);
            result
        } else {
            // Fallback: try without options (won't prompt but will check status)
            AXIsProcessTrustedWithOptions(std::ptr::null())
        };
        
        if trusted {
            PermissionState::Granted
        } else {
            // On macOS, accessibility permission requires manual intervention
            // Open System Settings to the Accessibility pane
            info!("Opening Accessibility settings for manual permission grant...");
            let _ = open_accessibility_settings();
            PermissionState::Denied
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
            debug!("User groups: {}, in_input_group={}", groups.trim(), in_group);
            
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
    
    info!("Adding user '{}' to input group (requires sudo)...", username);
    
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
        missing.push("Accessibility permission is required for keyboard and mouse capture".to_string());
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
