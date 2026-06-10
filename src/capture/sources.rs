//! Screen capture source management for embedded libobs
//!
//! Handles creating and managing screen/application capture sources.
//! Supports both display capture (entire screen) and application capture
//! (specific applications by bundle ID).

use anyhow::{Context as _, Result};
use libobs_wrapper::context::ObsContext;
use libobs_wrapper::scenes::ObsSceneRef;
use libobs_wrapper::sources::{ObsSourceBuilder, ObsSourceRef};
use libobs_wrapper::unsafe_send::Sendable;
use libobs_wrapper::utils::traits::ObsUpdatable;
use tracing::{debug, info};

#[cfg(target_os = "macos")]
use libobs_simple::sources::macos::{
    ScreenCaptureSourceBuilder, ScreenCaptureSourceUpdater, ScreenCaptureType,
};
#[cfg(target_os = "macos")]
use libobs_wrapper::data::ObsObjectUpdater;

/// Wrapper around a screen capture source
pub struct ScreenCaptureSource {
    source: ObsSourceRef,
    name: String,
    is_active: bool,
}

/// Fit a freshly added scene item to the recording canvas.
///
/// Windows `window_capture` adds the scene item at the window's native pixel size
/// in the top-left corner, so a window larger than the canvas (for example a
/// maximized window on an external or ultrawide monitor) is clipped to the canvas
/// rectangle. Giving the item "scale to inner" bounds equal to the canvas makes
/// OBS scale it to fit inside the frame (preserving aspect, letterboxed) and keep
/// it fit every frame even as the window resizes. A failure here is non-fatal:
/// the source still records, just possibly cropped.
///
/// Windows-only: macOS ScreenCaptureKit sources are display-sized (already match
/// the canvas), so this would be a no-op there, and macOS is a shipped product we
/// keep untouched.
#[cfg(target_os = "windows")]
fn fit_source_to_canvas(scene: &ObsSceneRef, source: &ObsSourceRef, name: &str) {
    if let Err(e) = scene.fit_source_to_screen(source) {
        tracing::warn!(
            "Could not fit capture source '{}' to the canvas (capture may be cropped): {}",
            name, e
        );
    }
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

    /// Create a new full-screen capture source on Windows.
    ///
    /// Uses libobs `monitor_capture` via Windows Graphics Capture (WGC),
    /// targeting the primary monitor. `capture_audio` is ignored — the
    /// Windows monitor source has no audio track (audio is window-scoped only).
    #[cfg(target_os = "windows")]
    pub fn new_display_capture(
        context: &mut ObsContext,
        scene: &mut ObsSceneRef,
        name: &str,
        _capture_audio: bool,
    ) -> Result<Self> {
        use libobs_simple::sources::windows::{
            MonitorCaptureSourceBuilder, ObsDisplayCaptureMethod,
        };

        info!("Creating Windows monitor capture source: {}", name);

        // Pick the primary monitor, falling back to the first enumerated one.
        let monitors = MonitorCaptureSourceBuilder::get_monitors()
            .map_err(|e| anyhow::anyhow!("Failed to enumerate monitors: {}", e))?;
        let primary = monitors
            .iter()
            .find(|m| m.0.is_primary)
            .or_else(|| monitors.first());

        let mut builder = context
            .source_builder::<MonitorCaptureSourceBuilder, _>(name)?
            .set_capture_cursor(true)
            .set_capture_method(ObsDisplayCaptureMethod::MethodWgc);

        if let Some(monitor) = primary {
            info!("Capturing monitor '{}'", monitor.0.name);
            builder = builder.set_monitor(monitor);
        } else {
            tracing::warn!("No monitors enumerated; using default monitor capture settings");
        }

        let source = builder
            .add_to_scene(scene)
            .context("Failed to add monitor capture source to scene")?;

        fit_source_to_canvas(scene, &source, name);

        debug!("Monitor capture source '{}' created successfully", name);

        Ok(Self {
            source,
            name: name.to_string(),
            is_active: true,
        })
    }

    /// Create a new screen capture source (fallback for unsupported platforms)
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
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

    /// Create a new application capture source on Windows.
    ///
    /// Uses libobs `window_capture` (Windows Graphics Capture) bound to a
    /// top-level window of the target application, matched by executable name.
    /// Returns an error if the application has no capturable window right now;
    /// callers (app-scene setup) treat that as "skip this app for now".
    /// `display_uuid` and `capture_audio` are unused on Windows.
    #[cfg(target_os = "windows")]
    pub fn new_application_capture(
        context: &mut ObsContext,
        scene: &mut ObsSceneRef,
        name: &str,
        bundle_id: &str,
        _display_uuid: &str,
        _capture_audio: bool,
    ) -> Result<Self> {
        use libobs_simple::sources::windows::{
            ObsWindowCaptureMethod, ObsWindowPriority, WindowCaptureSourceBuilder,
        };

        let obs_id = find_window_obs_id_for_app(bundle_id)?;
        info!(
            "Creating Windows window capture source: {} (app: {}, window: {})",
            name, bundle_id, obs_id
        );

        let source = context
            .source_builder::<WindowCaptureSourceBuilder, _>(name)?
            .set_window_raw(obs_id.as_str())
            .set_priority(ObsWindowPriority::Executable)
            .set_cursor(true)
            .set_capture_method(ObsWindowCaptureMethod::MethodWgc)
            .add_to_scene(scene)
            .context("Failed to add window capture source to scene")?;

        fit_source_to_canvas(scene, &source, name);

        debug!(
            "Window capture source '{}' for '{}' created successfully",
            name, bundle_id
        );

        Ok(Self {
            source,
            name: name.to_string(),
            is_active: true,
        })
    }

    /// Create a new application capture source (fallback for unsupported platforms)
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub fn new_application_capture(
        _context: &mut ObsContext,
        _scene: &mut ObsSceneRef,
        _name: &str,
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
        let was_active = self.is_active;
        self.is_active = self
            .dimensions()
            .map(|(width, height)| width > 0 && height > 0)
            .unwrap_or(false);

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

    /// Update the target application for this source in-place via `obs_source_update()`.
    #[cfg(target_os = "macos")]
    pub fn update_application(&mut self, bundle_id: &str) -> Result<()> {
        ScreenCaptureSourceUpdater::create_update(self.source.runtime(), &mut self.source)
            .context("Failed to create source updater")?
            .set_application(bundle_id)
            .set_audio_capture(false)
            .update()
            .context("Failed to update application")?;

        info!(
            "Updated capture source '{}' to application '{}'",
            self.name, bundle_id
        );
        Ok(())
    }

    /// Re-resolve the target application's window and update this `window_capture`
    /// source in-place (Windows). Used by the readiness watchdog to recover when
    /// the captured window has changed (e.g. closed and reopened).
    #[cfg(target_os = "windows")]
    pub fn update_application(&mut self, bundle_id: &str) -> Result<()> {
        use libobs_simple::sources::windows::WindowCaptureSourceUpdater;
        use libobs_wrapper::data::ObsObjectUpdater;

        let obs_id = find_window_obs_id_for_app(bundle_id)?;
        WindowCaptureSourceUpdater::create_update(self.source.runtime(), &mut self.source)
            .context("Failed to create window capture updater")?
            .set_window_raw(obs_id.as_str())
            .update()
            .context("Failed to update window capture target")?;

        info!(
            "Updated window capture source '{}' to application '{}'",
            self.name, bundle_id
        );
        Ok(())
    }

    /// Update the target application (fallback for unsupported platforms)
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub fn update_application(&mut self, _bundle_id: &str) -> Result<()> {
        Ok(())
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
            .set_audio_capture(false)
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

    /// Return the current source dimensions reported by OBS.
    pub fn dimensions(&self) -> Result<(u32, u32)> {
        let runtime = self.source.runtime();
        let source_ptr = Sendable(self.source.as_ptr());

        let width = libobs_wrapper::run_with_obs!(runtime.clone(), (source_ptr), move || unsafe {
            libobs::obs_source_get_width(source_ptr)
        })?;
        let height = libobs_wrapper::run_with_obs!(runtime, (source_ptr), move || unsafe {
            libobs::obs_source_get_height(source_ptr)
        })?;

        Ok((width, height))
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

/// Get the UUID string for the main display (Windows).
///
/// Windows `window_capture` targets a window, not a display, so it has no use
/// for a display UUID. Return an empty placeholder so the shared app-scene
/// setup (which passes a display UUID to `new_application_capture`) works
/// uniformly; the value is ignored on Windows.
#[cfg(target_os = "windows")]
pub fn get_main_display_uuid() -> Result<String> {
    Ok(String::new())
}

/// Get the UUID string for the main display (fallback for unsupported platforms)
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn get_main_display_uuid() -> Result<String> {
    anyhow::bail!("Display UUID not available on this platform")
}

/// Find the OBS window id of a capturable top-level window belonging to the
/// given application (matched by executable file stem, case-insensitive).
#[cfg(target_os = "windows")]
fn find_window_obs_id_for_app(bundle_id: &str) -> Result<String> {
    use libobs_simple::sources::windows::{WindowCaptureSourceBuilder, WindowSearchMode};

    let windows = WindowCaptureSourceBuilder::get_windows(WindowSearchMode::ExcludeMinimized)
        .map_err(|e| anyhow::anyhow!("Failed to enumerate windows: {}", e))?;

    windows
        .iter()
        .find(|w| {
            std::path::Path::new(&w.0.full_exe)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|stem| stem.eq_ignore_ascii_case(bundle_id))
                .unwrap_or(false)
        })
        .map(|w| w.0.obs_id.clone())
        .ok_or_else(|| {
            anyhow::anyhow!("No capturable window found for application '{}'", bundle_id)
        })
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

/// Get the actual pixel resolution of the primary display on Windows.
///
/// Uses `EnumDisplaySettingsW(ENUM_CURRENT_SETTINGS)`, which reports the
/// current mode's true pixel dimensions independent of the process's
/// DPI-awareness (unlike `GetSystemMetrics`).
#[cfg(target_os = "windows")]
pub fn get_main_display_resolution() -> Result<(u32, u32)> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS};

    unsafe {
        let mut devmode = DEVMODEW::default();
        devmode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;

        let ok = EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut devmode);
        if !ok.as_bool() {
            anyhow::bail!("EnumDisplaySettingsW failed to read the primary display mode");
        }

        let width = devmode.dmPelsWidth;
        let height = devmode.dmPelsHeight;
        if width == 0 || height == 0 {
            anyhow::bail!("Invalid display dimensions: {}x{}", width, height);
        }

        Ok((width, height))
    }
}

/// Get the actual resolution of the main display (fallback for unsupported platforms)
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn get_main_display_resolution() -> Result<(u32, u32)> {
    anyhow::bail!("Display resolution detection not available on this platform")
}
