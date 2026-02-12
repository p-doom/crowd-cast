//! Screen capture source management for embedded libobs
//!
//! Handles creating and managing screen/application capture sources.
//! Supports both display capture (entire screen) and application capture
//! (specific applications by bundle ID).

use anyhow::{Context as _, Result};
use libobs_wrapper::context::ObsContext;
use libobs_wrapper::scenes::ObsSceneRef;
use libobs_wrapper::sources::{ObsSourceBuilder, ObsSourceRef};
use tracing::{debug, info};

#[cfg(target_os = "macos")]
use libobs_simple::sources::macos::{
    ScreenCaptureSourceBuilder, ScreenCaptureSourceUpdater, ScreenCaptureType,
};
#[cfg(target_os = "macos")]
use libobs_wrapper::data::ObsObjectUpdater;
#[cfg(target_os = "macos")]
use libobs_wrapper::utils::traits::ObsUpdatable;

/// Wrapper around a screen capture source
pub struct ScreenCaptureSource {
    source: ObsSourceRef,
    name: String,
    is_active: bool,
}

impl ScreenCaptureSource {
    /// Create a new screen capture source for the main display
    ///
    /// # Arguments
    /// * `context` - The OBS context
    /// * `scene` - The scene to add the source to
    /// * `name` - Name for the source
    /// * `capture_audio` - Whether to capture system audio (macOS 13+)
    #[cfg(target_os = "macos")]
    pub fn new_display_capture(
        context: &mut ObsContext,
        scene: &mut ObsSceneRef,
        name: &str,
        capture_audio: bool,
    ) -> Result<Self> {
        // Get the current main display UUID - this is refreshed each time,
        // so it will be correct even after display reconnection
        let display_uuid = get_main_display_uuid()
            .context("Failed to get main display UUID for display capture")?;

        info!(
            "Creating macOS screen capture source: {} (display_uuid: {}, audio: {})",
            name, display_uuid, capture_audio
        );

        let source = context
            .source_builder::<ScreenCaptureSourceBuilder, _>(name)?
            .set_display_uuid(display_uuid)
            .set_show_cursor(true)
            .set_audio_capture(capture_audio)
            .add_to_scene(scene)
            .context("Failed to add screen capture source to scene")?;

        debug!("Screen capture source '{}' created successfully", name);

        Ok(Self {
            source,
            name: name.to_string(),
            is_active: true,
        })
    }

    /// Create a new screen capture source (fallback for non-macOS)
    #[cfg(not(target_os = "macos"))]
    pub fn new_display_capture(
        _context: &mut ObsContext,
        _scene: &mut ObsSceneRef,
        _name: &str,
        _capture_audio: bool,
    ) -> Result<Self> {
        anyhow::bail!("Screen capture not yet implemented for this platform");
    }

    /// Create a new application capture source for a specific application
    ///
    /// This captures all visible windows of the specified application using
    /// ScreenCaptureKit's application capture mode.
    ///
    /// # Arguments
    /// * `context` - The OBS context
    /// * `scene` - The scene to add the source to
    /// * `name` - Name for the source (should be unique)
    /// * `bundle_id` - Bundle identifier of the application (e.g., "com.apple.Safari")
    /// * `display_uuid` - UUID of the display (required for application capture filter)
    /// * `capture_audio` - Whether to capture application audio (macOS 13+)
    #[cfg(target_os = "macos")]
    pub fn new_application_capture(
        context: &mut ObsContext,
        scene: &mut ObsSceneRef,
        name: &str,
        bundle_id: &str,
        display_uuid: &str,
        capture_audio: bool,
    ) -> Result<Self> {
        info!(
            "Creating macOS application capture source: {} (app: {}, audio: {})",
            name, bundle_id, capture_audio
        );

        let source = context
            .source_builder::<ScreenCaptureSourceBuilder, _>(name)?
            .set_capture_type(ScreenCaptureType::Application as i64)
            .set_application(bundle_id)
            .set_display_uuid(display_uuid)
            .set_show_cursor(true)
            .set_audio_capture(capture_audio)
            .set_hide_obs(true) // Don't capture OBS/ourselves
            .add_to_scene(scene)
            .context("Failed to add application capture source to scene")?;

        debug!(
            "Application capture source '{}' for '{}' created successfully",
            name, bundle_id
        );

        Ok(Self {
            source,
            name: name.to_string(),
            is_active: true,
        })
    }

    /// Create a new application capture source (fallback for non-macOS)
    #[cfg(not(target_os = "macos"))]
    pub fn new_application_capture(
        _context: &mut ObsContext,
        _scene: &mut ObsSceneRef,
        name: &str,
        _bundle_id: &str,
        _display_uuid: &str,
        _capture_audio: bool,
    ) -> Result<Self> {
        anyhow::bail!("Application capture not yet implemented for this platform");
    }

    /// Get the source name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Check if the source is active (producing frames)
    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// Get a reference to the underlying OBS source
    pub fn source(&self) -> &ObsSourceRef {
        &self.source
    }

    /// Get a mutable reference to the underlying OBS source
    pub fn source_mut(&mut self) -> &mut ObsSourceRef {
        &mut self.source
    }

    /// Update the active state based on source dimensions
    /// A source with 0 width/height is considered inactive (stale capture)
    pub fn update_active_state(&mut self) -> bool {
        // In libobs, we can check if frames are being produced by checking dimensions
        // This is a simplified check - the actual implementation would need to
        // access the source's internal state
        let was_active = self.is_active;

        // TODO: Implement proper frame detection
        // For now, assume active unless explicitly marked otherwise
        self.is_active = true;

        was_active != self.is_active
    }

    /// Mark the source as inactive (stale)
    pub fn mark_inactive(&mut self) {
        self.is_active = false;
    }

    /// Mark the source as active
    pub fn mark_active(&mut self) {
        self.is_active = true;
    }

    /// Update the display UUID for this source
    ///
    /// This updates the source settings in-place without destroying/recreating it.
    /// Used after display reconnection to point the source at the new display.
    #[cfg(target_os = "macos")]
    pub fn update_display_uuid(&mut self, display_uuid: &str) -> Result<()> {
        let runtime = self.source.runtime();

        ScreenCaptureSourceUpdater::create_update(runtime, &mut self.source)
            .context("Failed to create source updater")?
            .set_display_uuid(display_uuid)
            .update()
            .context("Failed to update display UUID")?;

        debug!(
            "Updated display UUID for source '{}' to {}",
            self.name, display_uuid
        );
        Ok(())
    }

    /// Update the display UUID (non-macOS stub)
    #[cfg(not(target_os = "macos"))]
    pub fn update_display_uuid(&mut self, _display_uuid: &str) -> Result<()> {
        Ok(())
    }
}

/// Collection of capture sources
pub struct CaptureSourceManager {
    sources: Vec<ScreenCaptureSource>,
}

impl CaptureSourceManager {
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// Add a source to the manager
    pub fn add(&mut self, source: ScreenCaptureSource) {
        self.sources.push(source);
    }

    /// Get all sources
    pub fn sources(&self) -> &[ScreenCaptureSource] {
        &self.sources
    }

    /// Get mutable access to all sources
    pub fn sources_mut(&mut self) -> &mut [ScreenCaptureSource] {
        &mut self.sources
    }

    /// Check if any source is active
    pub fn any_active(&self) -> bool {
        self.sources.iter().any(|s| s.is_active())
    }

    /// Get the number of sources
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

impl Default for CaptureSourceManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Get the UUID string for the main display
///
/// This is required for application capture mode, which needs a display
/// to define the capture region.
#[cfg(target_os = "macos")]
pub fn get_main_display_uuid() -> Result<String> {
    use core_graphics::display::CGDisplay;
    use std::ffi::c_void;

    // FFI declarations for CoreGraphics UUID functions
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

    let main_display_id = CGDisplay::main().id;

    unsafe {
        let uuid_ref = CGDisplayCreateUUIDFromDisplayID(main_display_id);
        if uuid_ref.is_null() {
            anyhow::bail!(
                "Failed to get UUID for main display (ID: {})",
                main_display_id
            );
        }

        let uuid_string = CFUUIDCreateString(std::ptr::null(), uuid_ref);
        CFRelease(uuid_ref);

        if uuid_string.is_null() {
            anyhow::bail!("Failed to create UUID string for main display");
        }

        // Try to get the C string pointer directly first
        let c_str_ptr = CFStringGetCStringPtr(uuid_string, K_CF_STRING_ENCODING_UTF8);
        let result = if !c_str_ptr.is_null() {
            std::ffi::CStr::from_ptr(c_str_ptr)
                .to_str()
                .map(|s| s.to_string())
                .map_err(|e| anyhow::anyhow!("Invalid UTF-8 in display UUID: {}", e))
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
                    .map(|s| s.to_string())
                    .map_err(|e| anyhow::anyhow!("Invalid UTF-8 in display UUID: {}", e))
            } else {
                Err(anyhow::anyhow!("Failed to get display UUID string"))
            }
        };

        CFRelease(uuid_string);
        result
    }
}

/// Get the UUID string for the main display (non-macOS fallback)
#[cfg(not(target_os = "macos"))]
pub fn get_main_display_uuid() -> Result<String> {
    anyhow::bail!("Display UUID not available on this platform")
}

/// Get the actual resolution of the main display
///
/// On macOS, this returns the pixel dimensions of the current display mode,
/// which correctly handles Retina displays and different scaling settings.
/// This is more accurate than OBS's default video info which may not reflect
/// the actual display configuration.
#[cfg(target_os = "macos")]
pub fn get_main_display_resolution() -> Result<(u32, u32)> {
    use core_graphics::display::CGDisplay;
    use std::ffi::c_void;

    // FFI declarations for CGDisplayMode functions
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGDisplayCopyDisplayMode(display: u32) -> *const c_void;
        fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize;
        fn CGDisplayModeGetPixelHeight(mode: *const c_void) -> usize;
        fn CGDisplayModeRelease(mode: *const c_void);
    }

    let main_display_id = CGDisplay::main().id;

    unsafe {
        let mode = CGDisplayCopyDisplayMode(main_display_id);
        if mode.is_null() {
            anyhow::bail!(
                "Failed to get display mode for main display (ID: {})",
                main_display_id
            );
        }

        let width = CGDisplayModeGetPixelWidth(mode) as u32;
        let height = CGDisplayModeGetPixelHeight(mode) as u32;

        CGDisplayModeRelease(mode);

        if width == 0 || height == 0 {
            anyhow::bail!("Invalid display dimensions: {}x{}", width, height);
        }

        Ok((width, height))
    }
}

/// Get the actual resolution of the main display (non-macOS fallback)
#[cfg(not(target_os = "macos"))]
pub fn get_main_display_resolution() -> Result<(u32, u32)> {
    anyhow::bail!("Display resolution detection not available on this platform")
}
