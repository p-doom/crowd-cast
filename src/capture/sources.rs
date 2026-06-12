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

#[cfg(target_os = "linux")]
use libobs_simple::sources::linux::{
    PipeWireDesktopCaptureSourceBuilder, PipeWireWindowCaptureSourceBuilder,
    X11CaptureSourceBuilder, XCompositeInputSourceBuilder, XCompositeInputSourceUpdater,
};
#[cfg(target_os = "linux")]
use libobs_wrapper::data::{ObsData, ObsObjectUpdater};

/// Returns true when running under a Wayland session (vs X11), used to choose the right
/// Linux capture backend (PipeWire/portal on Wayland, XSHM/XComposite on X11).
#[cfg(target_os = "linux")]
pub(crate) fn is_wayland_session() -> bool {
    std::env::var("XDG_SESSION_TYPE")
        .map(|s| s.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false)
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Wrapper around a screen capture source
pub struct ScreenCaptureSource {
    source: ObsSourceRef,
    name: String,
    is_active: bool,
    /// Target app/window identifier this source captures (None for display capture).
    /// Used to key persisted portal restore tokens on Linux/Wayland.
    app_id: Option<String>,
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
            app_id: None,
        })
    }

    /// Create a new display (full-screen) capture source on Linux.
    ///
    /// - **Wayland**: captures via xdg-desktop-portal ScreenCast + PipeWire (the GPU /
    ///   zero-copy path). The first run shows the portal picker; a restore token can be
    ///   persisted later to avoid re-prompting (see docs/LINUX_LIBOBS_PROVISIONING.md).
    /// - **X11**: captures the whole primary screen via XSHM (`xshm_input`).
    ///
    /// Audio is configured at the output level, so `_capture_audio` is unused here.
    #[cfg(target_os = "linux")]
    pub fn new_display_capture(
        context: &mut ObsContext,
        scene: &mut ObsSceneRef,
        name: &str,
        _capture_audio: bool,
    ) -> Result<Self> {
        let source = if is_wayland_session() {
            info!(
                "Creating Linux PipeWire (Wayland) display capture source: {}",
                name
            );
            context
                .source_builder::<PipeWireDesktopCaptureSourceBuilder, _>(name)?
                .set_show_cursor(true)
                .add_to_scene(scene)
                .context("Failed to add PipeWire desktop capture source to scene")?
        } else {
            info!("Creating Linux X11 (xshm) display capture source: {}", name);
            context
                .source_builder::<X11CaptureSourceBuilder, _>(name)?
                .set_screen(0)
                .set_show_cursor(true)
                .add_to_scene(scene)
                .context("Failed to add X11 screen capture source to scene")?
        };

        debug!("Linux display capture source '{}' created", name);
        Ok(Self {
            source,
            name: name.to_string(),
            is_active: true,
            app_id: None,
        })
    }

    /// Create a new display capture source on Windows.
    ///
    /// WINDOWS (for whoever adds Windows support): mirror the Linux/macOS implementations
    /// using `libobs_simple::sources::windows::MonitorCaptureSourceBuilder` (the
    /// `monitor_capture` source), selecting the WGC capture method. The structure is
    /// identical: `context.source_builder::<MonitorCaptureSourceBuilder, _>(name)? ...
    /// .add_to_scene(scene)`.
    #[cfg(target_os = "windows")]
    pub fn new_display_capture(
        _context: &mut ObsContext,
        _scene: &mut ObsSceneRef,
        _name: &str,
        _capture_audio: bool,
    ) -> Result<Self> {
        anyhow::bail!("Display capture not yet implemented for Windows (use monitor_capture)")
    }

    /// Fallback for unsupported platforms.
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    pub fn new_display_capture(
        _context: &mut ObsContext,
        _scene: &mut ObsSceneRef,
        _name: &str,
        _capture_audio: bool,
    ) -> Result<Self> {
        anyhow::bail!("Screen capture not supported on this platform");
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
        _restore_token: Option<&str>,
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
            app_id: Some(bundle_id.to_string()),
        })
    }

    /// Create a per-application / per-window capture source on Linux.
    ///
    /// Privacy-preserving: only the target window's own buffer is captured, so an
    /// overlapping non-selected window cannot leak (no monitor cropping involved).
    /// - **Wayland**: per-window capture via the xdg-desktop-portal ScreenCast WINDOW
    ///   source (PipeWire). Selection is user-driven through the system picker — Wayland
    ///   does not permit selecting a window by app id programmatically — so `bundle_id` is
    ///   used only for logging; persist the restore token to avoid re-prompting.
    /// - **X11**: per-window capture via XComposite. `bundle_id` MUST be the X11 window id
    ///   (decimal string); mapping an application to its window id is the caller's job (see
    ///   the frontmost/app-enumeration code) and is intentionally not done here.
    ///
    /// `_display_uuid` is unused on Linux (kept for signature parity with macOS).
    ///
    /// NOTE: the end-to-end per-app / follow-focus flow on Linux (portal session lifecycle,
    /// restore-token persistence, window-id resolution) requires validation on a Linux
    /// machine. See docs/LINUX_PORTING_PLAN.md.
    #[cfg(target_os = "linux")]
    pub fn new_application_capture(
        context: &mut ObsContext,
        scene: &mut ObsSceneRef,
        name: &str,
        bundle_id: &str,
        _display_uuid: &str,
        _capture_audio: bool,
        restore_token: Option<&str>,
    ) -> Result<Self> {
        let source = if is_wayland_session() {
            info!(
                "Creating Linux PipeWire (Wayland) window capture source: {} (app hint: {}, has_token: {})",
                name,
                bundle_id,
                restore_token.map(|t| !t.is_empty()).unwrap_or(false)
            );
            // First launch (no token): the portal asks the user to pick a window, and the
            // token is read back afterwards (see CaptureContext::collect_restore_tokens).
            // Later launches pass the saved token so the same window is restored silently.
            let mut builder = context
                .source_builder::<PipeWireWindowCaptureSourceBuilder, _>(name)?
                .set_show_cursor(true);
            if let Some(token) = restore_token {
                if !token.is_empty() {
                    builder = builder.set_restore_token(token.to_string());
                }
            }
            builder
                .add_to_scene(scene)
                .context("Failed to add PipeWire window capture source to scene")?
        } else {
            // X11: `bundle_id` is the app identity (`/proc/comm`), not a window id. Bind the
            // app's window id *only if it is the focused window right now* (no fallback — see
            // x11_windows). Empty otherwise (e.g. created at setup before the app is focused);
            // the source stays blank and not-ready, so the engine's readiness gate keeps input
            // capture off until a focus switch re-resolves it. Fail-closed, never a wrong window.
            let capture_window =
                crate::capture::x11_windows::resolve_capture_window(bundle_id).unwrap_or_default();
            info!(
                "Creating Linux X11 (xcomposite) window capture source: {} (app: {}, resolved: {})",
                name,
                bundle_id,
                if capture_window.is_empty() { "<no window yet>" } else { &capture_window }
            );
            context
                .source_builder::<XCompositeInputSourceBuilder, _>(name)?
                .set_capture_window(capture_window)
                .set_show_cursor(true)
                .add_to_scene(scene)
                .context("Failed to add XComposite window capture source to scene")?
        };

        debug!("Linux application capture source '{}' created", name);
        Ok(Self {
            source,
            name: name.to_string(),
            is_active: true,
            app_id: Some(bundle_id.to_string()),
        })
    }

    /// Create a per-app capture source on Linux that binds an already-existing PipeWire node
    /// directly (no portal, no picker). The node is produced out-of-band by
    /// `gnome_screencast` via Mutter `RecordWindow`; OBS connects to it through the
    /// obs-pipewire `ConnectNode` setting (bundled-OBS patch). The node must stay alive (its
    /// Mutter session is held by the `GnomeScreenCast` manager) for this source's lifetime.
    #[cfg(target_os = "linux")]
    pub fn new_window_node_capture(
        context: &mut ObsContext,
        scene: &mut ObsSceneRef,
        name: &str,
        app_id: &str,
        node_id: u32,
    ) -> Result<Self> {
        info!(
            "Creating Linux PipeWire node capture source: {} (app: {}, node: {})",
            name, app_id, node_id
        );
        let source = context
            .source_builder::<PipeWireWindowCaptureSourceBuilder, _>(name)?
            .set_connect_node(node_id as i64)
            .set_show_cursor(true)
            .add_to_scene(scene)
            .context("Failed to add PipeWire node capture source to scene")?;
        Ok(Self {
            source,
            name: name.to_string(),
            is_active: true,
            app_id: Some(app_id.to_string()),
        })
    }

    /// Create a per-application/window capture source on Windows.
    ///
    /// WINDOWS (for whoever adds Windows support): implement using
    /// `libobs_simple::sources::windows::WindowCaptureSourceBuilder` (the `window_capture`
    /// source, WGC method) to capture a single HWND. Map the target app to its foreground
    /// HWND in the caller. Same builder -> `add_to_scene` structure as Linux/macOS.
    #[cfg(target_os = "windows")]
    pub fn new_application_capture(
        _context: &mut ObsContext,
        _scene: &mut ObsSceneRef,
        _name: &str,
        _bundle_id: &str,
        _display_uuid: &str,
        _capture_audio: bool,
        _restore_token: Option<&str>,
    ) -> Result<Self> {
        anyhow::bail!("Application capture not yet implemented for Windows (use window_capture)")
    }

    /// Fallback for unsupported platforms.
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    pub fn new_application_capture(
        _context: &mut ObsContext,
        _scene: &mut ObsSceneRef,
        _name: &str,
        _bundle_id: &str,
        _display_uuid: &str,
        _capture_audio: bool,
        _restore_token: Option<&str>,
    ) -> Result<Self> {
        anyhow::bail!("Application capture not supported on this platform");
    }

    /// Get the source name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The target app/window identifier this source captures (None for display capture).
    pub fn app_id(&self) -> Option<&str> {
        self.app_id.as_deref()
    }

    /// Read the xdg-desktop-portal restore token from a Wayland window-capture source.
    /// Returns None for other source types/platforms or before the user has selected a
    /// window (the token only becomes available once the portal session is established).
    #[cfg(target_os = "linux")]
    pub fn restore_token(&self) -> Option<String> {
        use libobs_simple::sources::linux::PipeWireSourceExtTrait;
        match self.source.get_restore_token() {
            Ok(token) => token,
            Err(e) => {
                debug!("get_restore_token('{}') unavailable: {}", self.name, e);
                None
            }
        }
    }

    /// Wayland: block until this per-app PipeWire source is actually producing frames
    /// (non-zero dimensions). Frames only flow once its xdg-desktop-portal ScreenCast
    /// session is fully established — after the user picks a window on first run, or
    /// after the silent restore handshake on later runs. We key on frame production
    /// rather than the restore token because a token passed in up front reads as "ready"
    /// immediately while the portal handshake is still in flight.
    ///
    /// This lets the caller create per-app sources ONE AT A TIME: two portal sessions
    /// negotiating concurrently abort the OBS pipewire plugin (`free(): invalid pointer`).
    /// On X11 (XComposite) there is no portal handshake, so this returns immediately.
    ///
    /// Returns `true` once the source is streaming, `false` on timeout (e.g. the user
    /// dismissed the portal picker).
    #[cfg(target_os = "linux")]
    pub fn wait_until_capturing(&self, timeout: std::time::Duration) -> bool {
        if !is_wayland_session() {
            return true;
        }
        let start = std::time::Instant::now();
        loop {
            if let Ok((w, h)) = self.dimensions() {
                if w > 0 && h > 0 {
                    debug!(
                        "Portal session for '{}' is streaming ({}x{}) after {:?}",
                        self.name,
                        w,
                        h,
                        start.elapsed()
                    );
                    return true;
                }
            }
            if start.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }

    /// Restore token (non-Linux stub: portal restore tokens are Wayland-only).
    #[cfg(not(target_os = "linux"))]
    pub fn restore_token(&self) -> Option<String> {
        None
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

    /// Re-resolve and re-point this source at the app's focused window, in-place.
    ///
    /// On X11 the window id is ephemeral (app restart / new window), so on every focus switch
    /// and capture-watchdog refresh we re-resolve `bundle_id` deterministically to the *focused
    /// window* (only if it still belongs to this app — no fallback) and update `capture_window`
    /// via `obs_source_update()` (no source recreation). Binds empty when this app isn't the
    /// focused one, leaving the source blank/not-ready so input capture stays gated off (the
    /// engine never switches the active capture to a non-frontmost app, so in practice this
    /// updates the just-focused app to its focused window). No-op on Wayland, where the
    /// portal/PipeWire selection is user-driven and persisted via a restore token.
    #[cfg(target_os = "linux")]
    pub fn update_application(&mut self, bundle_id: &str) -> Result<()> {
        if is_wayland_session() {
            return Ok(());
        }
        let capture_window =
            crate::capture::x11_windows::resolve_capture_window(bundle_id).unwrap_or_default();
        let runtime = self.source.runtime();
        XCompositeInputSourceUpdater::create_update(runtime, &mut self.source)
            .context("Failed to create XComposite source updater")?
            .set_capture_window(capture_window.clone())
            .update()
            .context("Failed to update XComposite capture window")?;
        self.app_id = Some(bundle_id.to_string());
        debug!(
            "Re-resolved XComposite capture for '{}' (window: {})",
            bundle_id,
            if capture_window.is_empty() { "<none>" } else { &capture_window }
        );
        Ok(())
    }

    /// Re-point a GNOME Wayland direct-node capture (created by `new_window_node_capture`) to a
    /// different PipeWire node, in place. Sets the `ConnectNode` setting and calls
    /// `obs_source_update`; the bundled obs-pipewire patch sees the changed node in its update
    /// handler and reconnects the stream to it — no source recreation, the scene item (and its
    /// transform/z-order) is preserved. Used by GNOME follow-focus to track the focused window.
    #[cfg(target_os = "linux")]
    pub fn update_connect_node(&mut self, node_id: u32) -> Result<()> {
        let mut data =
            ObsData::new(self.source.runtime()).context("Failed to allocate ObsData for ConnectNode")?;
        data.set_int("ConnectNode", node_id as i64)
            .context("Failed to set ConnectNode")?;
        self.source
            .update_raw(data)
            .context("Failed to obs_source_update ConnectNode")?;
        debug!("Re-pointed node capture '{}' to node {}", self.name, node_id);
        Ok(())
    }

    /// Update the target application (other non-macOS platforms: no-op stub).
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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

/// Get the UUID string for the main display.
///
/// On Linux the capture sources do not need a display UUID (unlike macOS ScreenCaptureKit
/// application capture), so this returns an empty string to keep the cross-platform call
/// sites (e.g. `setup_display_or_multi_capture`) working without special-casing.
#[cfg(target_os = "linux")]
pub fn get_main_display_uuid() -> Result<String> {
    Ok(String::new())
}

/// Get the UUID string for the main display (Windows).
///
/// WINDOWS: not needed for `monitor_capture`/`window_capture`; return empty unless a
/// monitor identifier is later required.
#[cfg(target_os = "windows")]
pub fn get_main_display_uuid() -> Result<String> {
    Ok(String::new())
}

/// Get the UUID string for the main display (unsupported-platform fallback)
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
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

/// Get the actual resolution of the main display (Linux).
///
/// Detects per session type — pure X11 reads the root-window geometry, Wayland reads the
/// largest `wl_output` current mode — with no cross-backend fallback: the chosen backend
/// either reports a size or this returns `Err`, and callers fail closed (never a guessed
/// default), so the capture canvas and recording metadata always reflect the real display.
#[cfg(target_os = "linux")]
pub fn get_main_display_resolution() -> Result<(u32, u32)> {
    if crate::capture::x11_windows::is_pure_x11_session() {
        crate::capture::x11_windows::x11_screen_size()
            .context("X11 root window reported no usable screen geometry")
    } else if is_wayland_session() {
        crate::capture::wayland_output::wayland_output_size()
            .context("no Wayland output reported a current mode")
    } else {
        anyhow::bail!("no X11 or Wayland session detected for display resolution detection")
    }
}

/// Get the actual resolution of the main display (Windows).
///
/// WINDOWS: implement via `GetSystemMetrics`/`EnumDisplayMonitors`; until then callers fall
/// back to OBS defaults.
#[cfg(target_os = "windows")]
pub fn get_main_display_resolution() -> Result<(u32, u32)> {
    anyhow::bail!("Display resolution detection not yet implemented on Windows (using OBS defaults)")
}

/// Get the actual resolution of the main display (unsupported-platform fallback)
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn get_main_display_resolution() -> Result<(u32, u32)> {
    anyhow::bail!("Display resolution detection not available on this platform")
}
