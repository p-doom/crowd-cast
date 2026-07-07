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
    PipeWireDesktopCaptureSourceBuilder, PipeWireScreenCaptureSourceBuilder,
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

/// Reserved `restore_tokens` map key for the full-screen display capture source on Wayland.
/// Display capture has no app/window identity, so its xdg-desktop-portal restore token is
/// persisted under this sentinel (kept distinct from any real bundle id) so the monitor pick
/// happens once and then restores silently on later launches. The leading underscores make a
/// collision with a real macOS bundle id or Linux process name impossible.
#[cfg(target_os = "linux")]
pub const DISPLAY_CAPTURE_KEY: &str = "__display__";

/// Wrapper around a screen capture source
pub struct ScreenCaptureSource {
    source: ObsSourceRef,
    name: String,
    is_active: bool,
    /// Target app/window identifier this source captures (None for display capture).
    /// Used to key persisted portal restore tokens on Linux/Wayland.
    app_id: Option<String>,
    /// macOS: the display UUID this SCK source is currently pinned to. `update_display_uuid`
    /// uses it to skip a redundant `obs_source_update` (which restarts the SCStream) when the
    /// requested UUID is unchanged. macOS-only — only ScreenCaptureKit capture is display-keyed.
    #[cfg(target_os = "macos")]
    display_uuid: Option<String>,
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
        _restore_token: Option<&str>,
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
            .set_display_uuid(display_uuid.clone())
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
            display_uuid: Some(display_uuid),
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
        _restore_token: Option<&str>,
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

        debug!("Monitor capture source '{}' created successfully", name);

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
    ///   zero-copy path). The first run shows the portal picker (on wlroots/sway this is a
    ///   bare `slurp` crosshair); the restore token read back afterwards is persisted under
    ///   [`DISPLAY_CAPTURE_KEY`] so the monitor pick happens once and later launches restore
    ///   the same output silently.
    /// - **X11**: captures the whole primary screen via XSHM (`xshm_input`) — no portal, no
    ///   picker, so `restore_token` is ignored.
    ///
    /// Audio is configured at the output level, so `_capture_audio` is unused here.
    #[cfg(target_os = "linux")]
    pub fn new_display_capture(
        context: &mut ObsContext,
        scene: &mut ObsSceneRef,
        name: &str,
        _capture_audio: bool,
        restore_token: Option<&str>,
    ) -> Result<Self> {
        let wayland = is_wayland_session();
        let source = if wayland {
            info!(
                "Creating Linux PipeWire (Wayland) display capture source: {} (has_token: {})",
                name,
                restore_token.map(|t| !t.is_empty()).unwrap_or(false)
            );
            // First launch (no token): the portal asks the user to pick a monitor and the
            // token is read back afterwards (see CaptureContext::collect_restore_tokens).
            // Later launches pass the saved token so the same output is restored silently.
            // Mirrors the per-window path: OBS requests persist mode itself, so an empty
            // token is simply not set rather than seeded.
            let mut builder = context
                .source_builder::<PipeWireDesktopCaptureSourceBuilder, _>(name)?
                .set_show_cursor(true);
            if let Some(token) = restore_token {
                if !token.is_empty() {
                    builder = builder.set_restore_token(token.to_string());
                }
            }
            builder
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
            // Wayland display capture persists its portal token under the reserved key;
            // X11 has no token, so it stays identity-less.
            app_id: wayland.then(|| DISPLAY_CAPTURE_KEY.to_string()),
        })
    }

    /// Create a new screen capture source (fallback for unsupported platforms)
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn new_display_capture(
        _context: &mut ObsContext,
        _scene: &mut ObsSceneRef,
        _name: &str,
        _capture_audio: bool,
        _restore_token: Option<&str>,
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
            display_uuid: Some(display_uuid.to_string()),
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
        _restore_token: Option<&str>,
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

        debug!(
            "Window capture source '{}' for '{}' created successfully",
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
    /// - **Wayland**: not supported here. GNOME Wayland per-app capture binds Mutter-owned
    ///   PipeWire nodes through [`Self::new_window_node_capture`]; sway is display capture
    ///   only. xdg-desktop-portal WINDOW capture is not a supported crowd-cast backend.
    /// - **X11**: per-window capture via XComposite. `bundle_id` MUST be the X11 window id
    ///   (decimal string); mapping an application to its window id is the caller's job (see
    ///   the frontmost/app-enumeration code) and is intentionally not done here.
    ///
    /// `_display_uuid` is unused on Linux (kept for signature parity with macOS).
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
        if is_wayland_session() {
            anyhow::bail!(
                "Linux Wayland per-app capture must use GNOME Mutter ScreenCast nodes; \
                 xdg-desktop-portal WINDOW capture is unsupported"
            );
        }
        let _ = restore_token;
        // X11: `bundle_id` is the app identity (`/proc/comm`), not a window id. Bind the
        // app's window id *only if it is the focused window right now* (no fallback; see
        // x11_windows). Empty otherwise (e.g. created at setup before the app is focused);
        // the source stays blank and not-ready, so the engine's readiness gate keeps input
        // capture off until a focus switch re-resolves it. Fail-closed, never a wrong window.
        let capture_window =
            crate::capture::x11_windows::resolve_capture_window(bundle_id).unwrap_or_default();
        info!(
            "Creating Linux X11 (xcomposite) window capture source: {} (app: {}, resolved: {})",
            name,
            bundle_id,
            if capture_window.is_empty() {
                "<no window yet>"
            } else {
                &capture_window
            }
        );
        let source = context
            .source_builder::<XCompositeInputSourceBuilder, _>(name)?
            .set_capture_window(capture_window)
            .set_show_cursor(true)
            .add_to_scene(scene)
            .context("Failed to add XComposite window capture source to scene")?;

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
    ///
    /// Uses the **unified** `pipewire-screen-capture-source` as the container, NOT the
    /// window-capture source. obs-pipewire only registers the window-capture source when the
    /// xdg-desktop-portal advertises WINDOW capture — but we never touch the portal here (the
    /// node comes from Mutter, and `ConnectNode` binds it on the default PipeWire daemon). The
    /// screen-capture source is registered whenever *any* screencast type is available, so this
    /// path no longer breaks when a non-GNOME portal backend (e.g. a stale xdg-desktop-portal-wlr
    /// serving the session) advertises monitor-only. `ConnectNode` behaves identically across
    /// the three obs-pipewire source types (see the bundled-OBS patch).
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
            .source_builder::<PipeWireScreenCaptureSourceBuilder, _>(name)?
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

    /// Unsupported-platform implementation.
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

    /// The target app/window identifier this source captures. Linux Wayland display capture
    /// uses [`DISPLAY_CAPTURE_KEY`] so its portal restore token can be persisted.
    pub fn app_id(&self) -> Option<&str> {
        self.app_id.as_deref()
    }

    /// Read the xdg-desktop-portal restore token from a Wayland PipeWire source.
    /// Returns None for other source types/platforms or before the user has selected a
    /// display (the token only becomes available once the portal session is established).
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

    /// Re-resolve and re-point this source at the app's focused window, in-place.
    ///
    /// On X11 the window id is ephemeral (app restart / new window), so on every focus switch
    /// and capture-watchdog refresh we re-resolve `bundle_id` deterministically to the *focused
    /// window* (only if it still belongs to this app — no fallback) and update `capture_window`
    /// via `obs_source_update()` (no source recreation). Binds empty when this app isn't the
    /// focused one, leaving the source blank/not-ready so input capture stays gated off (the
    /// engine never switches the active capture to a non-frontmost app, so in practice this
    /// updates the just-focused app to its focused window). No-op on Wayland: GNOME uses
    /// `update_connect_node`, and non-GNOME Wayland per-app capture is unsupported.
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
            if capture_window.is_empty() {
                "<none>"
            } else {
                &capture_window
            }
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
        let mut data = ObsData::new(self.source.runtime())
            .context("Failed to allocate ObsData for ConnectNode")?;
        data.set_int("ConnectNode", node_id as i64)
            .context("Failed to set ConnectNode")?;
        self.source
            .update_raw(data)
            .context("Failed to obs_source_update ConnectNode")?;
        debug!(
            "Re-pointed node capture '{}' to node {}",
            self.name, node_id
        );
        Ok(())
    }

    /// Update the target application (fallback for unsupported platforms: no-op stub).
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn update_application(&mut self, _bundle_id: &str) -> Result<()> {
        Ok(())
    }

    /// Update the display UUID for this source
    ///
    /// This updates the source settings in-place without destroying/recreating it.
    /// Used after display reconnection to point the source at the new display.
    #[cfg(target_os = "macos")]
    pub fn update_display_uuid(&mut self, display_uuid: &str) -> Result<()> {
        // Idempotent: obs_source_update on an SCK source restarts its SCStream (brief black
        // frame), so skip it when the source is already pinned to this display. Without this,
        // the follow-focus retarget would needlessly restart the stream on the first poll after
        // every (re)build (source is created pinned to main) and on app switches.
        if self.display_uuid.as_deref() == Some(display_uuid) {
            return Ok(());
        }

        let runtime = self.source.runtime();

        ScreenCaptureSourceUpdater::create_update(runtime, &mut self.source)
            .context("Failed to create source updater")?
            .set_display_uuid(display_uuid)
            .set_audio_capture(false)
            .update()
            .context("Failed to update display UUID")?;

        self.display_uuid = Some(display_uuid.to_string());
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
/// Windows `window_capture` targets a window, not a display, so it has no use
/// for a display UUID. Return an empty placeholder so the shared app-scene
/// setup (which passes a display UUID to `new_application_capture`) works
/// uniformly; the value is ignored on Windows.
#[cfg(target_os = "windows")]
pub fn get_main_display_uuid() -> Result<String> {
    Ok(String::new())
}

/// Get the UUID string for the main display (unsupported-platform fallback)
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
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

/// Get the actual resolution of the main display (unsupported-platform fallback)
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn get_main_display_resolution() -> Result<(u32, u32)> {
    anyhow::bail!("Display resolution detection not available on this platform")
}
