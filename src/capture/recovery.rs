//! Capture recovery for ScreenCaptureKit display ID issues
//!
//! This module handles the recovery of stale captures when displays are
//! disconnected and reconnected on macOS. The key issue is that ScreenCaptureKit
//! caches display IDs, and when a display is reconnected, the IDs may change.
//!
//! The fix involves rebuilding the shareable content list before reinitializing
//! the capture stream. Since we have direct access to libobs, we can implement
//! this fix properly.

use anyhow::{Context as _, Result};
use std::time::Duration;
use tracing::{debug, info};

#[cfg(target_os = "macos")]
use libobs_wrapper::context::ObsContext;
#[cfg(target_os = "macos")]
use libobs_wrapper::sources::ObsSourceRef;
#[cfg(target_os = "macos")]
use libobs_wrapper::data::ObsDataGetters;

/// Handles recovery of stale screen captures
pub struct CaptureRecovery {
    /// Minimum time to wait after display detection before recovery
    display_stabilization_delay: Duration,
    /// Whether recovery is currently in progress
    recovery_in_progress: bool,
}

impl CaptureRecovery {
    pub fn new() -> Self {
        Self {
            // Wait 2 seconds for display to stabilize after reconnect
            display_stabilization_delay: Duration::from_secs(2),
            recovery_in_progress: false,
        }
    }
    
    /// Check if any displays are currently connected
    #[cfg(target_os = "macos")]
    pub fn displays_connected() -> bool {
        use core_graphics::display::CGDisplay;
        
        let displays = CGDisplay::active_displays();
        match displays {
            Ok(list) => !list.is_empty(),
            Err(_) => false,
        }
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn displays_connected() -> bool {
        // Assume displays are connected on non-macOS
        true
    }
    
    /// Get the number of connected displays
    #[cfg(target_os = "macos")]
    pub fn display_count() -> usize {
        use core_graphics::display::CGDisplay;
        
        CGDisplay::active_displays()
            .map(|list| list.len())
            .unwrap_or(0)
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn display_count() -> usize {
        1
    }
    
    /// Get the main display ID
    #[cfg(target_os = "macos")]
    pub fn main_display_id() -> Option<u32> {
        use core_graphics::display::CGDisplay;
        
        Some(CGDisplay::main().id)
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn main_display_id() -> Option<u32> {
        Some(0)
    }
    
    /// Attempt to recover a stale screen capture source
    /// 
    /// This works by:
    /// 1. Waiting for displays to be connected
    /// 2. Waiting for display configuration to stabilize
    /// 3. Toggling settings to force ScreenCaptureKit to re-enumerate displays
    /// 4. Updating the source with the new display configuration
    #[cfg(target_os = "macos")]
    pub fn recover_source(
        &mut self,
        context: &mut ObsContext,
        source: &mut ObsSourceRef,
        source_name: &str,
    ) -> Result<bool> {
        if self.recovery_in_progress {
            debug!("Recovery already in progress for {}", source_name);
            return Ok(false);
        }
        
        // Check if displays are connected
        if !Self::displays_connected() {
            debug!("No displays connected, skipping recovery for {}", source_name);
            return Ok(false);
        }
        
        self.recovery_in_progress = true;
        let result = self.do_recovery(context, source, source_name);
        self.recovery_in_progress = false;
        
        result
    }
    
    #[cfg(target_os = "macos")]
    fn do_recovery(
        &self,
        context: &mut ObsContext,
        source: &mut ObsSourceRef,
        source_name: &str,
    ) -> Result<bool> {
        info!(
            "Attempting recovery for source '{}' (displays: {})",
            source_name,
            Self::display_count()
        );
        
        // Wait for display configuration to stabilize
        std::thread::sleep(self.display_stabilization_delay);
        
        // Get current settings
        let settings = source.get_settings()
            .context("Failed to get source settings")?;
        
        // Get the current show_cursor value
        let show_cursor = settings.get_bool("show_cursor")
            .ok()
            .flatten()
            .unwrap_or(true);
        
        // Create new settings with toggled cursor (forces re-init)
        let mut new_settings = context.data()
            .context("Failed to create new settings")?;
        
        // Toggle show_cursor off
        new_settings.set_bool("show_cursor", !show_cursor)?;
        
        // Apply the toggled settings
        use libobs_wrapper::utils::traits::ObsUpdatable;
        source.update_raw(new_settings)
            .context("Failed to update source with toggled settings")?;
        
        // Wait for SCK to process
        std::thread::sleep(Duration::from_millis(500));
        
        // Toggle show_cursor back
        let mut restore_settings = context.data()
            .context("Failed to create restore settings")?;
        restore_settings.set_bool("show_cursor", show_cursor)?;
        
        // Get main display ID and set it
        if let Some(display_id) = Self::main_display_id() {
            restore_settings.set_int("display", display_id as i64)?;
        }
        
        // Apply restored settings
        source.update_raw(restore_settings)
            .context("Failed to restore source settings")?;
        
        info!("Recovery completed for source '{}'", source_name);
        Ok(true)
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn recover_source(
        &mut self,
        _context: &mut ObsContext,
        _source: &mut ObsSourceRef,
        source_name: &str,
    ) -> Result<bool> {
        debug!("Recovery not needed on this platform for {}", source_name);
        Ok(false)
    }
    
    /// Check if a source appears to be stale (not producing frames)
    /// 
    /// This is a heuristic based on the source dimensions being zero
    pub fn is_source_stale(_source: &ObsSourceRef) -> bool {
        // A source with 0 dimensions is likely stale
        // Note: This requires accessing the source's width/height
        // For now, we rely on external detection (e.g., frame callbacks)
        false
    }
    
    /// Set the display stabilization delay
    pub fn set_stabilization_delay(&mut self, delay: Duration) {
        self.display_stabilization_delay = delay;
    }
}

impl Default for CaptureRecovery {
    fn default() -> Self {
        Self::new()
    }
}

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
            // Display IDs changed (catches quick disconnect/reconnect)
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
