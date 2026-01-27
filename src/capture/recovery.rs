//! Display hotplug monitoring for ScreenCaptureKit recovery
//!
//! This module monitors display connection changes on macOS and signals
//! when capture sources need to be refreshed due to display ID changes.

use tracing::{debug, info};

/// Monitor for display connection changes
#[cfg(target_os = "macos")]
pub struct DisplayMonitor {
    last_display_ids: Vec<u32>,
    displays_were_disconnected: bool,
}

#[cfg(target_os = "macos")]
impl DisplayMonitor {
    pub fn new() -> Self {
        let ids = Self::get_display_ids();
        debug!("DisplayMonitor initialized with displays: {:?}", ids);
        Self {
            last_display_ids: ids,
            displays_were_disconnected: false,
        }
    }
    
    /// Get current display IDs
    fn get_display_ids() -> Vec<u32> {
        use core_graphics::display::CGDisplay;
        CGDisplay::active_displays()
            .map(|displays| displays.into_iter().collect())
            .unwrap_or_default()
    }
    
    /// Check for display changes and return true if recovery might be needed
    pub fn check_for_changes(&mut self) -> bool {
        let current_ids = Self::get_display_ids();
        
        if current_ids.is_empty() {
            // Displays disconnected
            if !self.displays_were_disconnected {
                info!("Display disconnected");
                self.displays_were_disconnected = true;
            }
            self.last_display_ids.clear();
            false
        } else if self.displays_were_disconnected {
            // Displays reconnected after being disconnected
            info!("Display reconnected: {:?}", current_ids);
            self.displays_were_disconnected = false;
            self.last_display_ids = current_ids;
            true // Recovery needed
        } else if current_ids != self.last_display_ids {
            // Display IDs changed
            info!("Display IDs changed: {:?} -> {:?}", self.last_display_ids, current_ids);
            self.last_display_ids = current_ids;
            true // Recovery needed
        } else {
            false
        }
    }
}

#[cfg(target_os = "macos")]
impl Default for DisplayMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_os = "macos"))]
pub struct DisplayMonitor;

#[cfg(not(target_os = "macos"))]
impl DisplayMonitor {
    pub fn new() -> Self {
        Self
    }
    
    pub fn check_for_changes(&mut self) -> bool {
        false
    }
}

#[cfg(not(target_os = "macos"))]
impl Default for DisplayMonitor {
    fn default() -> Self {
        Self::new()
    }
}
