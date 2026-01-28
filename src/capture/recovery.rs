//! Display hotplug monitoring for ScreenCaptureKit recovery
//!
//! This module monitors display connection changes on macOS and signals
//! when capture sources need to be refreshed due to display ID changes.
//! It tracks the "original" display that capture started with and distinguishes
//! between the original display returning vs switching to a new display.

use tracing::{debug, info};

/// Events that can occur when display configuration changes
#[derive(Debug, Clone)]
pub enum DisplayChangeEvent {
    /// Original display returned - auto-recover is safe
    OriginalReturned {
        display_id: u32,
        uuid: String,
        display_name: String,
    },
    /// Switched to a different display - needs user confirmation
    SwitchedToNew {
        from_id: u32,
        from_name: String,
        to_id: u32,
        to_name: String,
        to_uuid: String,
    },
    /// All displays disconnected
    AllDisconnected,
}

/// Monitor for display connection changes
#[cfg(target_os = "macos")]
pub struct DisplayMonitor {
    /// Last known display IDs
    last_display_ids: Vec<u32>,
    /// Whether displays were disconnected
    displays_were_disconnected: bool,
    /// The display ID that was active when recording started
    original_display_id: Option<u32>,
    /// UUID of the original display
    original_display_uuid: Option<String>,
}

#[cfg(target_os = "macos")]
impl DisplayMonitor {
    pub fn new() -> Self {
        let ids = Self::get_display_ids();
        debug!("DisplayMonitor initialized with displays: {:?}", ids);
        Self {
            last_display_ids: ids,
            displays_were_disconnected: false,
            original_display_id: None,
            original_display_uuid: None,
        }
    }

    /// Set the original display when recording starts
    ///
    /// This should be called when recording begins to remember which display
    /// was active. When this display returns after disconnection, auto-recovery
    /// will be triggered without user intervention.
    pub fn set_original_display(&mut self, display_id: u32, uuid: String) {
        info!(
            "Setting original display: id={}, uuid={}",
            display_id, uuid
        );
        self.original_display_id = Some(display_id);
        self.original_display_uuid = Some(uuid);
    }

    /// Clear the original display (e.g., when recording stops)
    pub fn clear_original_display(&mut self) {
        self.original_display_id = None;
        self.original_display_uuid = None;
    }

    /// Get current display IDs
    fn get_display_ids() -> Vec<u32> {
        use core_graphics::display::CGDisplay;
        CGDisplay::active_displays()
            .map(|displays| displays.into_iter().collect())
            .unwrap_or_default()
    }

    /// Get the current display IDs (public accessor)
    pub fn current_display_ids(&self) -> &[u32] {
        &self.last_display_ids
    }

    /// Check for display changes and return what kind of change occurred
    pub fn check_for_changes(&mut self) -> Option<DisplayChangeEvent> {
        let current_ids = Self::get_display_ids();

        // No change
        if current_ids == self.last_display_ids {
            return None;
        }

        let old_ids = std::mem::replace(&mut self.last_display_ids, current_ids.clone());

        // All displays disconnected
        if current_ids.is_empty() {
            if !self.displays_were_disconnected {
                info!("All displays disconnected");
                self.displays_were_disconnected = true;
            }
            return Some(DisplayChangeEvent::AllDisconnected);
        }

        // Displays reconnected after being fully disconnected
        if self.displays_were_disconnected {
            info!("Displays reconnected: {:?}", current_ids);
            self.displays_were_disconnected = false;
        }

        // Check if original display returned
        if let Some(orig_id) = self.original_display_id {
            if current_ids.contains(&orig_id) && !old_ids.contains(&orig_id) {
                // Original display came back
                let uuid = get_display_uuid(orig_id).unwrap_or_default();
                let name = get_display_name(orig_id);
                info!("Original display {} ({}) returned", name, orig_id);
                return Some(DisplayChangeEvent::OriginalReturned {
                    display_id: orig_id,
                    uuid,
                    display_name: name,
                });
            }
        }

        // Otherwise, it's a switch to a new/different display
        let from_id = old_ids.first().copied().unwrap_or(0);
        let to_id = current_ids.first().copied().unwrap_or(0);
        let from_name = get_display_name(from_id);
        let to_name = get_display_name(to_id);
        let to_uuid = get_display_uuid(to_id).unwrap_or_default();

        info!(
            "Display IDs changed: {:?} -> {:?} ({} -> {})",
            old_ids, current_ids, from_name, to_name
        );

        Some(DisplayChangeEvent::SwitchedToNew {
            from_id,
            from_name,
            to_id,
            to_name,
            to_uuid,
        })
    }

    /// Legacy method for backward compatibility - returns true if any change detected
    pub fn has_changes(&mut self) -> bool {
        self.check_for_changes().is_some()
    }
}

#[cfg(target_os = "macos")]
impl Default for DisplayMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Get a human-readable name for a display
#[cfg(target_os = "macos")]
pub fn get_display_name(display_id: u32) -> String {
    use core_graphics::display::CGDisplay;

    if display_id == 0 {
        return "Unknown Display".to_string();
    }

    // Check if this is the main display
    let main_id = CGDisplay::main().id;
    if display_id == main_id {
        // Try to determine if it's built-in or external
        // Built-in displays typically have ID 1 on MacBooks
        if display_id == 1 {
            return "Built-in Display".to_string();
        }
    }

    // For external displays, try to get more info
    // Unfortunately, getting the actual display name requires IOKit which is more complex
    // Fall back to a descriptive name based on the ID
    if display_id == 1 {
        "Built-in Display".to_string()
    } else if display_id < 10 {
        format!("External Display {}", display_id)
    } else {
        // Higher IDs are often virtual displays
        format!("Display {}", display_id)
    }
}

/// Get the UUID for a display
#[cfg(target_os = "macos")]
pub fn get_display_uuid(display_id: u32) -> Option<String> {
    use std::ffi::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFUUIDCreateString(allocator: *const c_void, uuid: *const c_void) -> *const c_void;
        fn CFStringGetCStringPtr(string: *const c_void, encoding: u32) -> *const i8;
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut i8,
            buffer_size: i64,
            encoding: u32,
        ) -> bool;
        fn CFRelease(cf: *const c_void);
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;

    unsafe {
        let uuid_ref = CGDisplayCreateUUIDFromDisplayID(display_id);
        if uuid_ref.is_null() {
            return None;
        }

        let uuid_string = CFUUIDCreateString(std::ptr::null(), uuid_ref);
        CFRelease(uuid_ref);

        if uuid_string.is_null() {
            return None;
        }

        // Try to get the C string pointer directly first
        let c_str_ptr = CFStringGetCStringPtr(uuid_string, K_CF_STRING_ENCODING_UTF8);
        let result = if !c_str_ptr.is_null() {
            std::ffi::CStr::from_ptr(c_str_ptr)
                .to_str()
                .ok()
                .map(|s| s.to_string())
        } else {
            // Fallback: copy to buffer
            let mut buffer = [0i8; 128];
            if CFStringGetCString(
                uuid_string,
                buffer.as_mut_ptr(),
                buffer.len() as i64,
                K_CF_STRING_ENCODING_UTF8,
            ) {
                std::ffi::CStr::from_ptr(buffer.as_ptr())
                    .to_str()
                    .ok()
                    .map(|s| s.to_string())
            } else {
                None
            }
        };

        CFRelease(uuid_string);
        result
    }
}

// Non-macOS stubs
#[cfg(not(target_os = "macos"))]
pub struct DisplayMonitor;

#[cfg(not(target_os = "macos"))]
impl DisplayMonitor {
    pub fn new() -> Self {
        Self
    }

    pub fn set_original_display(&mut self, _display_id: u32, _uuid: String) {}

    pub fn clear_original_display(&mut self) {}

    pub fn current_display_ids(&self) -> &[u32] {
        &[]
    }

    pub fn check_for_changes(&mut self) -> Option<DisplayChangeEvent> {
        None
    }

    pub fn has_changes(&mut self) -> bool {
        false
    }
}

#[cfg(not(target_os = "macos"))]
impl Default for DisplayMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_os = "macos"))]
pub fn get_display_name(_display_id: u32) -> String {
    "Unknown Display".to_string()
}

#[cfg(not(target_os = "macos"))]
pub fn get_display_uuid(_display_id: u32) -> Option<String> {
    None
}
