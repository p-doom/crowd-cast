//! OBS Context management for embedded libobs
//!
//! Handles initialization, bootstrapping, and lifecycle of the libobs context.
//! Provides high-level API for screen capture and recording with automatic
//! encoder selection (HEVC preferred with hardware acceleration).
//!
//! Supports both display capture (entire screen) and application capture
//! (specific applications by bundle ID).

use anyhow::{Context as _, Result};
// The OBS bootstrapper downloads prebuilt OBS binaries at runtime. It is only used on
// macOS/Windows; on Linux libobs is provided by a system install or a relocatable bundle
// (located via CROWD_CAST_OBS_* env vars), so the crate (which hard-`compile_error!`s on
// Linux) is gated out entirely here and in Cargo.toml.
#[cfg(not(target_os = "linux"))]
use libobs_bootstrapper::{
    status_handler::ObsBootstrapStatusHandler, ObsBootstrapper, ObsBootstrapperOptions,
    ObsBootstrapperResult,
};
use libobs_wrapper::context::ObsContext;
use libobs_wrapper::data::video::ObsVideoInfoBuilder;
use libobs_wrapper::scenes::ObsSceneRef;
use libobs_wrapper::utils::StartupInfo;
// ObsPath/StartupPaths are only used to redirect OBS runtime paths on macOS/Linux.
#[cfg(any(target_os = "macos", target_os = "linux"))]
use libobs_wrapper::utils::{ObsPath, StartupPaths};
use std::collections::{HashMap, HashSet};
#[cfg(not(target_os = "linux"))]
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tracing::{debug, info, warn};

use crate::crash::log_critical_operation;

use super::frontmost::get_frontmost_app;
use super::recording::{calculate_output_dimensions, RecordingConfig, RecordingOutput};
use super::sources::{get_main_display_resolution, get_main_display_uuid, ScreenCaptureSource};
use super::CaptureState;

// Only used by the macOS/Windows bootstrap path below.
#[cfg(not(target_os = "linux"))]
use crate::ui::{is_running_in_app_bundle, show_obs_download_started_notification};

/// Session information for a recording
#[derive(Debug, Clone)]
pub struct RecordingSession {
    /// Unique session ID
    pub session_id: String,
    /// Output file path
    pub output_path: PathBuf,
    /// Start timestamp (monotonic nanoseconds from OBS)
    pub start_time_ns: u64,
}

/// Manages the embedded libobs context with screen capture and recording
pub struct CaptureContext {
    /// The libobs context (None if not yet initialized)
    context: Option<ObsContext>,
    /// Scene for display capture mode (no target apps) or legacy multi-source mode
    scene: Option<ObsSceneRef>,
    /// Capture sources for display capture / legacy mode
    capture_sources: Vec<ScreenCaptureSource>,
    /// Per-app scenes for single-active-app mode: bundle_id → (scene, source)
    /// All sources run simultaneously; switching apps = activating the target scene.
    app_scenes: HashMap<String, (ObsSceneRef, ScreenCaptureSource)>,
    /// Empty scene activated when no tracked app is frontmost
    blank_scene: Option<ObsSceneRef>,
    /// GNOME Wayland: owns the Mutter ScreenCast sessions that back the per-app PipeWire
    /// nodes (picker-free capture). Must outlive `app_scenes` — dropping it closes the
    /// sessions and kills the nodes. `None` off GNOME Wayland (display / XComposite paths).
    #[cfg(target_os = "linux")]
    gnome_screencast: Option<super::gnome_screencast::GnomeScreenCast>,
    /// GNOME Wayland follow-focus: which Mutter window-id each app's scene is currently bound
    /// to (bundle_id → window-id). The node source is re-pointed at the focused window as
    /// focus moves between an app's windows; this tracks the live binding so we only re-point
    /// on an actual change. Empty off GNOME Wayland.
    #[cfg(target_os = "linux")]
    gnome_bound_window: HashMap<String, u64>,
    /// GNOME Wayland follow-focus: the window-id whose bind last *failed* per app (bundle_id →
    /// window-id). Without this, a failing bind (e.g. the obs-pipewire capture source is not
    /// registered because the active portal advertises monitor-only) would be retried on every
    /// focus poll — recreating and leaking a scene + Mutter session ~10×/s. We record the failed
    /// window-id and skip re-attempting it; cleared on a successful bind and on capture
    /// reconfigure. Empty off GNOME Wayland.
    #[cfg(target_os = "linux")]
    gnome_bind_failed: HashMap<String, u64>,
    /// Multi-monitor per-app placement: the last MonitorFit applied to the active app's scene
    /// item, keyed (app, scale.to_bits, pos_x.to_bits, pos_y.to_bits) so an unchanged transform
    /// is a no-op rather than re-applied every focus poll. Linux only.
    #[cfg(target_os = "linux")]
    last_monitor_fit: Option<(String, u32, u32, u32)>,
    /// Recording output
    recording: Option<RecordingOutput>,
    /// Current recording session info
    current_session: Option<RecordingSession>,
    /// Current capture state
    state: Arc<RwLock<CaptureState>>,
    /// Recording output directory
    output_directory: PathBuf,
    /// Recording configuration
    recording_config: RecordingConfig,
    /// The canvas (base) dimensions in pixels that OBS is currently compositing into, captured
    /// whenever the video info is (re)built. Recorded in the segment metadata as the true frame
    /// size (with multi-monitor on this is the normalized envelope, not the main display).
    canvas_dims: (u32, u32),
    /// Target apps for capture (stored for recreation after display changes)
    target_apps: Vec<String>,
    /// Restore tokens for portal-backed display capture (Linux/Wayland), keyed by
    /// `DISPLAY_CAPTURE_KEY`.
    restore_tokens: HashMap<String, String>,
    /// Whether macOS should keep only one tracked application's source active at a time
    single_active_app_capture: bool,
    /// Currently active application capture target when single-active mode is enabled
    active_capture_app: Option<String>,
    /// Windows/macOS monitor-level fit last applied to the active source, used to skip
    /// re-applying an unchanged transform every poll: (app, scale, pos_x, pos_y) with the
    /// floats stored as bits so it derives Eq. (macOS pos is always (0,0) — SCK hands us a
    /// full-display frame; see `apply_monitor_fit_to_active`.)
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    last_monitor_fit: Option<(String, u32, u32, u32)>,
    /// macOS: whether the multi-monitor capture path (normalized canvas + per-display fit) is
    /// enabled. Set from `config.capture.mac_multi_monitor_capture` at startup. Kill-switch;
    /// when false, behaves exactly like the pre-feature main-display-only path.
    #[cfg(target_os = "macos")]
    mac_multi_monitor_capture: bool,
    /// macOS follow-focus: the display UUID each tracked app's SCK source is currently pointed
    /// at (bundle_id → uuid). PER-APP: single-active mode keeps a persistent source per target
    /// app (only the active scene is shown), so a single slot would be clobbered on every app
    /// switch and re-fire the (SCStream-restarting) retarget on a source that never moved. Keyed
    /// by app so each app's last-known-good display survives another app taking a turn as active.
    /// Reset with `last_monitor_fit` on every source rebuild.
    #[cfg(target_os = "macos")]
    last_display_uuid: HashMap<String, String>,
}

impl CaptureContext {
    /// Bootstrap OBS binaries if needed and create a new capture context
    pub async fn new(output_directory: PathBuf) -> Result<Self> {
        info!("Initializing embedded libobs capture context...");

        // Bootstrap OBS binaries (download if not present).
        // Linux does NOT use the bootstrapper: libobs is provided by a system OBS install
        // or a relocatable bundle located via CROWD_CAST_OBS_* env vars
        // (see `obs_startup_paths_from_env`).
        #[cfg(not(target_os = "linux"))]
        {
            let bootstrap_result = Self::bootstrap_obs().await?;

            match bootstrap_result {
                ObsBootstrapperResult::None => {
                    debug!("OBS binaries already present");
                }
                ObsBootstrapperResult::Restart => {
                    // On Windows, the bootstrapper downloads OBS and stages an updater
                    // that moves the new binaries into place and relaunches the app.
                    // We must exit cleanly so that updater can run; the relaunched
                    // process will find OBS already present and proceed normally.
                    #[cfg(target_os = "windows")]
                    {
                        info!(
                            "OBS binaries installed; exiting so the bootstrap updater can relaunch with OBS available"
                        );
                        std::process::exit(0);
                    }

                    // On macOS, bootstrap completes in place and returns None, so a
                    // Restart here is unexpected.
                    #[cfg(not(target_os = "windows"))]
                    {
                        warn!("OBS bootstrap requires restart - this shouldn't happen on this platform");
                        anyhow::bail!("OBS bootstrap requires application restart");
                    }
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            // No runtime bootstrapper *download*: the ~17 MB libobs bundle ships with the binary
            // and is located by compiled-in ABI under ~/.local/share/crowd-cast/obs/<abi>/. Here
            // we only validate + report; the actual StartupPaths wiring happens in initialize()
            // via obs_startup_paths_from_env(). Precedence there: CROWD_CAST_OBS_* env override ->
            // self-provisioned bundle -> system OBS install.
            match self_provisioned_bundle_root() {
                Some(root) if bundle_is_present(&root) => {
                    info!(
                        "libobs bundle present at {} (self-provisioned, ABI {})",
                        root.display(),
                        OBS_ABI
                    );
                }
                Some(root) => {
                    if std::env::var_os("CROWD_CAST_OBS_DATA_PATH").is_some() {
                        debug!(
                            "Self-provisioned libobs bundle absent at {}; using CROWD_CAST_OBS_* env override",
                            root.display()
                        );
                    } else {
                        warn!(
                            "Self-provisioned libobs bundle not found at {} (ABI {}); falling back to a system OBS install. \
                             Ship the bundle there, or set CROWD_CAST_OBS_* to override.",
                            root.display(),
                            OBS_ABI
                        );
                    }
                }
                None => {
                    debug!("HOME not set; relying on CROWD_CAST_OBS_* env or a system OBS install for libobs");
                }
            }
        }

        Ok(Self {
            context: None,
            scene: None,
            capture_sources: Vec::new(),
            app_scenes: HashMap::new(),
            blank_scene: None,
            #[cfg(target_os = "linux")]
            gnome_screencast: None,
            #[cfg(target_os = "linux")]
            gnome_bound_window: HashMap::new(),
            #[cfg(target_os = "linux")]
            gnome_bind_failed: HashMap::new(),
            #[cfg(target_os = "linux")]
            last_monitor_fit: None,
            recording: None,
            current_session: None,
            state: Arc::new(RwLock::new(CaptureState::default())),
            output_directory,
            recording_config: RecordingConfig::default(),
            canvas_dims: (0, 0),
            target_apps: Vec::new(),
            restore_tokens: HashMap::new(),
            single_active_app_capture: false,
            active_capture_app: None,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            last_monitor_fit: None,
            #[cfg(target_os = "macos")]
            mac_multi_monitor_capture: false,
            #[cfg(target_os = "macos")]
            last_display_uuid: HashMap::new(),
        })
    }

    /// Bootstrap OBS binaries (macOS/Windows only; Linux uses system or bundled libobs).
    #[cfg(not(target_os = "linux"))]
    async fn bootstrap_obs() -> Result<ObsBootstrapperResult> {
        // On Windows the release agent runs windowless (no console), so the
        // one-time first-launch OBS download is otherwise invisible — toast a
        // "downloading" notification so the user knows why startup is delayed.
        let notify_download = is_running_in_app_bundle() || cfg!(target_os = "windows");
        #[cfg(target_os = "macos")]
        let options = {
            let mut options = ObsBootstrapperOptions::default().set_update(false);
            if let Some(runtime_root) = obs_runtime_root() {
                info!(
                    "Using external OBS bootstrap install dir {}",
                    runtime_root.display()
                );
                options = options.set_install_dir(runtime_root);
            }
            options
        };
        #[cfg(not(target_os = "macos"))]
        let options = ObsBootstrapperOptions::default().set_update(false);

        // Do not auto-update OBS at runtime. Only install when missing.
        let obs_present = ObsBootstrapper::is_valid_installation_with_options(&options)
            .context("Failed to check OBS installation")?;
        if obs_present {
            debug!("OBS installation already present; skipping runtime update checks");
            return Ok(ObsBootstrapperResult::None);
        }

        ObsBootstrapper::bootstrap_with_handler(
            &options,
            Box::new(ObsBootstrapNotificationHandler::new(notify_download)),
        )
        .await
        .context("Failed to bootstrap OBS binaries")
    }

    /// Pre-populate the target app list before [`initialize`](Self::initialize) so the capture
    /// canvas can pick the multi-monitor per-app envelope vs the display-capture canvas (the
    /// mode depends on whether any app is targeted). [`setup_capture`](Self::setup_capture) sets
    /// the authoritative list later; this is idempotent with it.
    pub fn set_target_apps(&mut self, target_apps: &[String]) {
        self.target_apps = target_apps.to_vec();
    }

    /// Compute the recording canvas (base) and encoded output dimensions.
    ///
    /// On Windows the canvas is the bounding box of all monitors, each normalized
    /// to a 1080px shortest edge, and the output equals the canvas (uncapped — which is
    /// why short-edge is harmless there: a portrait monitor just makes a taller canvas).
    /// On Linux in single-active per-app mode the canvas is the multi-monitor
    /// 1080px-HEIGHT envelope (see [`super::monitor_layout`]) and the output is capped at
    /// `max_output_height` — height normalization keeps the cap from ever engaging.
    /// On macOS (multi-monitor mode) likewise; as a fallback the canvas is the main
    /// display and the output is downscaled to `max_output_height`.
    fn canvas_and_output_dimensions(&self) -> ((u32, u32), (u32, u32)) {
        #[cfg(target_os = "windows")]
        {
            if let Some((bw, bh)) = super::window_geometry::capture_canvas_size() {
                debug!("Monitor-normalized capture canvas: {}x{}", bw, bh);
                return ((bw, bh), (bw, bh));
            }
            warn!("Could not enumerate monitors for canvas sizing; falling back to primary display");
        }

        // Linux single-active per-app mode: the multi-monitor 1080-height envelope, so a
        // window on any monitor fits its normalized slot. Falls through to display resolution
        // if monitor enumeration fails.
        #[cfg(target_os = "linux")]
        if self.use_single_active_app_capture() {
            if let Some((w, h)) = super::monitor_layout::capture_canvas_size() {
                debug!("Multi-monitor capture canvas: {}x{}", w, h);
                let output =
                    calculate_output_dimensions(w, h, self.recording_config.max_output_height);
                return ((w, h), output);
            }
            warn!(
                "Could not enumerate monitors for the multi-monitor capture canvas. \
                 Falling back to display resolution."
            );
        }

        // macOS multi-monitor mode: the per-axis-max envelope of every display, each normalized
        // to a 1080px HEIGHT (PIXELS — SCK reports backing pixels; see mac_geometry). Gated on
        // the kill-switch flag AND single-active-app mode — matching the Linux gate and the
        // apply_monitor_fit_to_active gate — because only the single-active path applies the
        // compensating per-source transform (scale=norm). Display-capture / non-single-active
        // sources carry no such transform, so they must keep the pre-feature main-display canvas
        // (byte-identical) rather than a normalized canvas the source would overflow/crop. Falls
        // through to the main-display resolution if the flag is off or enumeration fails.
        #[cfg(target_os = "macos")]
        if self.mac_multi_monitor_enabled() && self.use_single_active_app_capture() {
            if let Some((w, h)) = super::mac_geometry::capture_canvas_size() {
                debug!("macOS multi-monitor capture canvas: {}x{}", w, h);
                let output =
                    calculate_output_dimensions(w, h, self.recording_config.max_output_height);
                return ((w, h), output);
            }
            warn!(
                "macOS multi-monitor: could not enumerate displays for the capture canvas. \
                 Falling back to the main display resolution."
            );
        }

        let (base_width, base_height) = match get_main_display_resolution() {
            Ok((w, h)) => {
                debug!("Detected display resolution: {}x{}", w, h);
                (w, h)
            }
            Err(e) => {
                warn!(
                    "Failed to detect display resolution: {}. Using OBS defaults.",
                    e
                );
                let default_video_info = ObsVideoInfoBuilder::new().build();
                (
                    default_video_info.get_base_width(),
                    default_video_info.get_base_height(),
                )
            }
        };
        let output = calculate_output_dimensions(
            base_width,
            base_height,
            self.recording_config.max_output_height,
        );
        ((base_width, base_height), output)
    }

    /// Initialize the libobs context (must be called from main thread on some platforms)
    ///
    /// This configures the video output based on `recording_config`:
    /// - Output resolution is downscaled to max_output_height while preserving aspect ratio
    /// - FPS is set from recording_config.fps
    pub fn initialize(&mut self) -> Result<()> {
        if self.context.is_some() {
            debug!("libobs context already initialized");
            return Ok(());
        }

        info!("Initializing libobs context...");

        // Canvas + output dimensions (Windows: monitor-normalized bounding box;
        // macOS/fallback: main display downscaled to max_output_height).
        let ((base_width, base_height), (output_width, output_height)) =
            self.canvas_and_output_dimensions();
        self.canvas_dims = (base_width, base_height);

        info!(
            "Video config: {}x{} (canvas) -> {}x{} (output), {} fps",
            base_width, base_height, output_width, output_height, self.recording_config.fps
        );

        // Build custom video info with configured settings
        let video_info = ObsVideoInfoBuilder::new()
            .base_width(base_width)
            .base_height(base_height)
            .fps_num(self.recording_config.fps)
            .fps_den(1)
            .output_width(output_width)
            .output_height(output_height)
            .build();

        let mut startup_info = StartupInfo::default().set_video_info(video_info);
        // On macOS the runtime OBS lives in the app bundle; on Linux it lives in a
        // downloaded/extracted bundle (or system install). Both can be redirected via
        // CROWD_CAST_OBS_* env vars. When none are set on Linux, `StartupInfo::default()`
        // already points libobs-wrapper at the system OBS paths.
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(paths) = obs_startup_paths_from_env() {
            startup_info = startup_info.set_startup_paths(paths);
        }
        log_critical_operation("initialize: calling ObsContext::new()");
        let context = ObsContext::new(startup_info).context("Failed to create OBS context")?;
        log_critical_operation("initialize: ObsContext::new() completed");

        info!("libobs context initialized successfully");
        self.context = Some(context);

        Ok(())
    }

    /// Enable or disable the macOS single-active-app capture strategy.
    /// Linux per-app capture uses the single-active path whenever per-app capture is
    /// supported; there is no portal-backed multi-source Wayland mode.
    pub fn set_single_active_app_capture(&mut self, enabled: bool) {
        self.single_active_app_capture = enabled;
    }

    /// Enable/disable the macOS multi-monitor capture path (normalized canvas + per-display
    /// fit). Set from `config.capture.mac_multi_monitor_capture` at startup. No-op off macOS.
    pub fn set_mac_multi_monitor_capture(&mut self, enabled: bool) {
        #[cfg(target_os = "macos")]
        {
            self.mac_multi_monitor_capture = enabled;
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = enabled;
        }
    }

    /// Whether the macOS multi-monitor capture path is active. Always false off macOS.
    #[cfg(target_os = "macos")]
    fn mac_multi_monitor_enabled(&self) -> bool {
        self.mac_multi_monitor_capture
    }

    fn use_single_active_app_capture(&self) -> bool {
        if self.target_apps.is_empty() {
            return false;
        }

        // macOS/Windows gate on the config flag (single-active is one of several modes).
        // Linux treats supported per-app capture as mandatory, so it ignores the flag.
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            crate::capture::is_single_active_capable() && self.single_active_app_capture
        }
        #[cfg(target_os = "linux")]
        {
            crate::capture::is_single_active_capable()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            false
        }
    }

    fn build_scene_name(prefix: &str) -> String {
        format!(
            "{}_{}",
            prefix,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        )
    }

    /// Canonical form of an application identifier for matching and as the
    /// `app_scenes` key. Windows executable names are case-insensitive, so we
    /// lower-case them; macOS bundle IDs and Linux process names are
    /// case-sensitive and pass through unchanged (so macOS behavior is identical).
    fn canonical_app_id(app: &str) -> String {
        #[cfg(target_os = "windows")]
        {
            app.to_ascii_lowercase()
        }
        #[cfg(not(target_os = "windows"))]
        {
            app.to_string()
        }
    }

    fn select_initial_active_app(&self) -> Option<String> {
        if !self.use_single_active_app_capture() {
            return None;
        }

        let frontmost = get_frontmost_app()?;
        if self
            .target_apps
            .iter()
            .any(|app| Self::canonical_app_id(app) == Self::canonical_app_id(&frontmost.bundle_id))
        {
            Some(Self::canonical_app_id(&frontmost.bundle_id))
        } else {
            None
        }
    }

    fn create_scene(&mut self, scene_name: &str) -> Result<ObsSceneRef> {
        let context = self
            .context
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;

        context.scene(scene_name).context("Failed to create scene")
    }

    fn activate_scene(scene: &mut ObsSceneRef) -> Result<()> {
        scene.set_to_channel(0).context("Failed to activate scene")
    }

    fn update_capture_state_flags(&self) {
        if let Ok(mut state) = self.state.write() {
            let has_active_source = if self.use_single_active_app_capture() {
                self.active_capture_app
                    .as_ref()
                    .and_then(|app| self.app_scenes.get(app))
                    .is_some()
            } else {
                !self.capture_sources.is_empty()
            };
            state.any_source_active = has_active_source;
            state.should_capture = state.recording.is_recording
                && !state.recording.is_paused
                && state.any_source_active;
        }
    }

    /// Create all per-app scenes and a blank scene for single-active-app mode.
    /// Each tracked app gets its own scene with one SCK source. All sources run
    /// simultaneously; switching apps just activates the target scene on channel 0.
    fn setup_app_scenes(&mut self, initial_active_app: Option<&str>) -> Result<()> {
        if !self.is_initialized() {
            anyhow::bail!("OBS context not initialized");
        }

        // Clean up all capture resources (both modes) to prevent cross-mode
        // leaks when switching between single-active and display/multi modes.
        self.app_scenes.clear();
        self.blank_scene = None;
        // The per-app monitor-fit transform is de-duped via `last_monitor_fit` (keyed on app +
        // scale + pos). Clearing app_scenes destroys the scene items the transform was applied
        // to, so that cache is now stale. Reset it here — every rebuild path (setup_capture,
        // fully_recreate_sources, reset_video_and_recreate_sources, reinitialize_for_display_change)
        // routes through setup_app_scenes — so a freshly recreated source always gets its
        // transform re-applied on the next poll, even when the recomputed key equals the
        // pre-rebuild value (e.g. wake-from-sleep on the same display, where scale is unchanged).
        // Without this, the dedup would match the stale key and skip re-applying, leaving the new
        // scene item at libobs's default scale 1.0 (overflowing a normalized canvas). All of
        // Linux/Windows/macOS carry this field.
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        {
            self.last_monitor_fit = None;
        }
        // macOS follow-focus: a rebuilt source starts pinned to the main display (see
        // new_application_capture), so the retarget cache must also be invalidated or the next
        // poll would skip re-pointing it to the focused window's display.
        #[cfg(target_os = "macos")]
        {
            self.last_display_uuid.clear();
        }
        // Drop any prior Mutter ScreenCast manager (closes its sessions / frees the old
        // nodes) before we rebuild.
        #[cfg(target_os = "linux")]
        {
            self.gnome_screencast = None;
            self.gnome_bound_window.clear();
            self.gnome_bind_failed.clear();
        }
        self.capture_sources.clear();
        self.scene = None;

        // Create blank scene (shown when no tracked app is frontmost)
        let blank_scene_name = Self::build_scene_name("blank");
        let mut blank_scene = self.create_scene(&blank_scene_name)?;
        if initial_active_app.is_none() {
            Self::activate_scene(&mut blank_scene)?;
        }
        self.blank_scene = Some(blank_scene);

        // GNOME Wayland: bring up the Mutter ScreenCast manager that produces picker-free
        // PipeWire nodes per target app. Held in `self` so its sessions (hence the nodes
        // backing the sources) outlive the scenes. This is the only supported per-app
        // Wayland backend, so init failure is fatal.
        #[cfg(target_os = "linux")]
        if crate::capture::is_gnome_wayland() {
            match super::gnome_screencast::GnomeScreenCast::new() {
                Ok(g) => {
                    info!(
                        "GNOME Wayland: using Mutter ScreenCast (picker-free) for per-app capture"
                    );
                    self.gnome_screencast = Some(g);
                }
                Err(e) => anyhow::bail!("GNOME ScreenCast manager init failed: {e}"),
            }
        }

        let display_uuid = get_main_display_uuid()
            .context("Failed to get main display UUID for application capture")?;
        let capture_audio = self.recording_config.enable_audio;
        let target_apps = self.target_apps.clone();
        let restore_tokens = self.restore_tokens.clone();

        let context = self
            .context
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;

        // Only create scenes for apps that are currently running.
        // Apps launched later get their scenes created lazily on first switch.
        // This avoids stale SCK filters for apps not running at startup.
        let running_bundles: HashSet<String> = {
            #[cfg(target_os = "macos")]
            {
                use objc::runtime::{Class, Object};
                use objc::{msg_send, sel, sel_impl};
                let mut bundles = HashSet::new();
                unsafe {
                    let cls = Class::get("NSWorkspace").unwrap();
                    let workspace: *mut Object = msg_send![cls, sharedWorkspace];
                    let apps: *mut Object = msg_send![workspace, runningApplications];
                    let count: usize = msg_send![apps, count];
                    for i in 0..count {
                        let app: *mut Object = msg_send![apps, objectAtIndex: i];
                        let bid: *mut Object = msg_send![app, bundleIdentifier];
                        if !bid.is_null() {
                            let cstr: *const std::os::raw::c_char = msg_send![bid, UTF8String];
                            if !cstr.is_null() {
                                if let Ok(s) = std::ffi::CStr::from_ptr(cstr).to_str() {
                                    bundles.insert(s.to_string());
                                }
                            }
                        }
                    }
                }
                bundles
            }
            // On Windows (and other non-macOS platforms) we don't pre-filter by
            // running process here. We attempt to create a window_capture source
            // for each target app below; apps without a capturable window simply
            // fail source creation and are skipped (see the match on the result).
            #[cfg(not(target_os = "macos"))]
            {
                target_apps.iter().cloned().collect()
            }
        };
        // Only consulted on macOS (see the per-app skip below).
        #[cfg(not(target_os = "macos"))]
        let _ = &running_bundles;

        for bundle_id in &target_apps {
            // macOS: ScreenCaptureKit sources for apps not running at startup must be created
            // in a fresh OBS context (the engine restarts the process to do so), so skip them
            // here and create them lazily. XComposite (X11) has no such constraint — we
            // pre-create a scene for every target app, so `needs_scene_for_app` stays false
            // (no restart) and the source binds its window on creation and each focus switch.
            #[cfg(target_os = "macos")]
            if !running_bundles.contains(bundle_id.as_str()) {
                debug!("Skipping scene for '{}' (not running)", bundle_id);
                continue;
            }

            // GNOME Wayland (Mutter ScreenCast): per-app scenes are created lazily on first
            // focus, and the node source is re-pointed to the *focused* window as focus moves
            // between an app's windows (see `gnome_ensure_focused_window`). We deliberately
            // create nothing at setup: the app generally isn't focused yet, so there is no
            // "focused window" to bind — picking one here is exactly the multi-window bug. The
            // app blanks until focused, then binds its focused window (and tracks it after).
            // This also means a target app launched *after* setup needs no process restart:
            // it has no scene, and gets one lazily the moment it's focused.
            #[cfg(target_os = "linux")]
            if self.gnome_screencast.is_some() {
                continue;
            }

            let scene_name = Self::build_scene_name(&format!("scene_{}", bundle_id));
            let mut scene = context
                .scene(scene_name.as_str())
                .context("Failed to create scene")?;

            let source_name = format!("app_capture_{}", bundle_id);
            match ScreenCaptureSource::new_application_capture(
                context,
                &mut scene,
                &source_name,
                bundle_id,
                &display_uuid,
                capture_audio,
                restore_tokens.get(bundle_id).map(|s| s.as_str()),
            ) {
                Ok(source) => {
                    // Key scenes by the canonical id so frontmost-derived lookups
                    // (also canonical) match regardless of how target_apps is cased.
                    // On macOS/Linux `canonical_app_id` is the identity, so this is the
                    // raw bundle id / process name there.
                    let canonical_id = Self::canonical_app_id(bundle_id);
                    if initial_active_app == Some(canonical_id.as_str()) {
                        Self::activate_scene(&mut scene)?;
                        self.active_capture_app = Some(canonical_id.clone());
                    }
                    info!("Created app scene for '{}'", bundle_id);
                    self.app_scenes.insert(canonical_id, (scene, source));
                }
                Err(e) => {
                    warn!(
                        "Failed to create capture source for '{}': {}. Skipping.",
                        bundle_id, e
                    );
                }
            }
        }

        // Assert the intended program scene: the initial active app's scene, or the blank
        // scene when no tracked app is frontmost.
        match self.active_capture_app.clone() {
            Some(app) => {
                if let Some((scene, _)) = self.app_scenes.get_mut(app.as_str()) {
                    Self::activate_scene(scene)?;
                } else if let Some(blank) = self.blank_scene.as_mut() {
                    Self::activate_scene(blank)?;
                    self.active_capture_app = None;
                }
            }
            None => {
                if let Some(blank) = self.blank_scene.as_mut() {
                    Self::activate_scene(blank)?;
                }
            }
        }

        info!(
            "Set up {} app scene(s) for single-active capture (active: {:?})",
            self.app_scenes.len(),
            self.active_capture_app
        );
        self.update_capture_state_flags();
        Ok(())
    }

    /// Set up capture for display capture mode or legacy multi-source mode.
    /// On Linux, per-app capture must use `setup_app_scenes`; this path is display-only.
    fn setup_display_or_multi_capture(&mut self) -> Result<usize> {
        if !self.is_initialized() {
            anyhow::bail!("OBS context not initialized");
        }
        #[cfg(target_os = "linux")]
        if !self.target_apps.is_empty() {
            anyhow::bail!(
                "Linux per-app capture must use the single-active GNOME/X11 path; \
                 portal-backed multi-source per-app capture is unsupported"
            );
        }

        // Clean up all capture resources (both modes) to prevent cross-mode
        // leaks when switching between single-active and display/multi modes.
        self.capture_sources.clear();
        self.scene = None;
        self.app_scenes.clear();
        self.blank_scene = None;
        // Leaving per-app mode: drop the Mutter ScreenCast manager (closes its sessions).
        #[cfg(target_os = "linux")]
        {
            self.gnome_screencast = None;
        }

        let scene_name = Self::build_scene_name("main_scene");
        let mut scene = self.create_scene(&scene_name)?;

        let capture_audio = self.recording_config.enable_audio;
        let target_apps = self.target_apps.clone();
        let restore_tokens = self.restore_tokens.clone();
        let mut capture_sources = Vec::new();

        let context = self
            .context
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;

        if target_apps.is_empty() {
            // Wayland: the portal "choose a screen" picker pops on create — on wlroots/sway
            // it's a bare slurp crosshair with no label — so cue the user what it's for, but
            // only when a picker is actually expected (no saved restore token; a valid token
            // restores the same output silently). Best-effort + synchronous so the cue lands
            // before the crosshair; a no-op when no notification daemon is running.
            #[cfg(target_os = "linux")]
            let source = {
                let display_token = restore_tokens
                    .get(super::sources::DISPLAY_CAPTURE_KEY)
                    .map(|s| s.as_str());
                if super::sources::is_wayland_session()
                    && display_token.map(|t| t.is_empty()).unwrap_or(true)
                {
                    crate::ui::notify_linux::notify_blocking(
                        "crowd-cast — choose a screen",
                        "Click the monitor you want to share in the selector that appears next.",
                    );
                    info!("Prompting portal monitor pick for display capture");
                }
                ScreenCaptureSource::new_display_capture(
                    context,
                    &mut scene,
                    "screen_capture",
                    capture_audio,
                    display_token,
                )
                .context("Failed to create screen capture source")?
            };
            #[cfg(not(target_os = "linux"))]
            let source = ScreenCaptureSource::new_display_capture(
                context,
                &mut scene,
                "screen_capture",
                capture_audio,
                None,
            )
            .context("Failed to create screen capture source")?;
            capture_sources.push(source);
        } else {
            let display_uuid = get_main_display_uuid()
                .context("Failed to get main display UUID for application capture")?;

            for (i, bundle_id) in target_apps.iter().enumerate() {
                let source_name = format!("app_capture_{}", i);
                match ScreenCaptureSource::new_application_capture(
                    context,
                    &mut scene,
                    &source_name,
                    bundle_id,
                    &display_uuid,
                    capture_audio,
                    restore_tokens.get(bundle_id).map(|s| s.as_str()),
                ) {
                    Ok(source) => {
                        debug!(
                            "Created capture source '{}' for '{}'",
                            source_name, bundle_id
                        );
                        capture_sources.push(source);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to create capture source for '{}': {}. Skipping.",
                            bundle_id, e
                        );
                    }
                }
            }

            if !target_apps.is_empty() && capture_sources.is_empty() {
                anyhow::bail!(
                    "Failed to create any capture sources for target apps: {:?}",
                    target_apps
                );
            }
        }

        let count = capture_sources.len();
        Self::activate_scene(&mut scene)?;
        self.capture_sources = capture_sources;
        self.scene = Some(scene);
        self.update_capture_state_flags();
        Ok(count)
    }

    /// Set up capture sources and scene for specific applications
    ///
    /// Creates per-application capture sources for each target app. If no target apps are
    /// specified, creates a display-capture source instead.
    /// Must be called after `initialize()`.
    ///
    /// # Arguments
    /// * `target_apps` - List of bundle identifiers to capture (e.g., ["com.apple.Safari", "com.microsoft.VSCode"])
    pub fn setup_capture(
        &mut self,
        target_apps: &[String],
        restore_tokens: &HashMap<String, String>,
    ) -> Result<()> {
        self.target_apps = target_apps.to_vec();
        self.restore_tokens = restore_tokens.clone();

        if self.use_single_active_app_capture() {
            let initial_active_app = self.select_initial_active_app();
            self.setup_app_scenes(initial_active_app.as_deref())?;
        } else {
            let count = self.setup_display_or_multi_capture()?;
            if target_apps.is_empty() {
                info!("Capture scene configured for display capture");
            } else {
                info!("Created {} application capture sources", count);
            }
        }

        Ok(())
    }

    /// Fully destroy and recreate capture sources after a display configuration change
    ///
    /// Unlike `recreate_sources()` which updates settings in-place, this method
    /// completely destroys all existing capture sources and creates new ones.
    /// This forces ScreenCaptureKit to do a fresh initialization, which is more
    /// reliable when transitioning between displays (e.g., clamshell mode).
    ///
    /// This method also recreates the scene to ensure old sources are fully removed
    /// from OBS. The new scene is activated on channel 0 to continue any ongoing
    /// recording seamlessly.
    ///
    /// Returns the number of sources successfully created.
    pub fn fully_recreate_sources(&mut self) -> Result<usize> {
        if !self.is_initialized() {
            anyhow::bail!("OBS context not initialized");
        }

        if self.use_single_active_app_capture() {
            let active_app = self.active_capture_app.clone();
            self.setup_app_scenes(active_app.as_deref())?;
            let count = self.app_scenes.len();
            info!("Fully recreated {} app scene(s)", count);
            Ok(count)
        } else {
            let count = self.setup_display_or_multi_capture()?;
            info!("Fully recreated {} capture source(s)", count);
            Ok(count)
        }
    }

    /// Reset video output resolution and recreate sources (safe reinit)
    ///
    /// This uses `obs_reset_video()` to change the video resolution WITHOUT dropping
    /// the entire OBS context. This avoids the SIGABRT crash that occurs when dropping
    /// the context.
    ///
    /// Requirements:
    /// - Recording must be stopped before calling this (outputs must not be active)
    /// - Graphics module cannot change (always true for same display type)
    pub fn reset_video_and_recreate_sources(&mut self) -> Result<()> {
        log_critical_operation("reset_video_and_recreate_sources: starting");

        if !self.is_initialized() {
            anyhow::bail!("OBS context not initialized");
        }

        // Recording must be stopped first (reset_video fails with active outputs)
        if self.recording.is_some() {
            log_critical_operation("reset_video_and_recreate_sources: stopping recording");
            self.stop_recording()
                .context("Failed to stop recording before video reset")?;
        }

        // Recompute canvas + output (monitors may have changed: hot-plug, rotation,
        // resolution). Same logic as initialize(): Windows monitor-normalized envelope,
        // Linux multi-monitor per-app envelope, else main display downscaled.
        let ((base_width, base_height), (output_width, output_height)) =
            self.canvas_and_output_dimensions();
        self.canvas_dims = (base_width, base_height);

        info!(
            "Resetting video: {}x{} (canvas) -> {}x{} (output), {} fps",
            base_width, base_height, output_width, output_height, self.recording_config.fps
        );

        // Clear all sources and scenes before reset
        log_critical_operation("reset_video_and_recreate_sources: clearing sources");
        self.capture_sources.clear();
        self.scene = None;
        self.app_scenes.clear();
        self.blank_scene = None;

        // Build new video info
        let video_info = ObsVideoInfoBuilder::new()
            .base_width(base_width)
            .base_height(base_height)
            .fps_num(self.recording_config.fps)
            .fps_den(1)
            .output_width(output_width)
            .output_height(output_height)
            .build();

        // Reset video (this is the safe way to change resolution)
        log_critical_operation("reset_video_and_recreate_sources: calling reset_video()");
        let context = self
            .context
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;

        context
            .reset_video(video_info)
            .context("Failed to reset video output")?;
        log_critical_operation("reset_video_and_recreate_sources: reset_video() completed");

        // Recreate capture sources
        self.fully_recreate_sources()
            .context("Failed to setup capture after video reset")?;

        log_critical_operation("reset_video_and_recreate_sources: completed successfully");
        Ok(())
    }

    /// Reinitialize the OBS context for a display change
    ///
    /// This drops the existing OBS context and recreates it, ensuring the
    /// base/output resolution matches the current main display. It also
    /// recreates capture sources for the stored target apps.
    pub fn reinitialize_for_display_change(&mut self) -> Result<()> {
        log_critical_operation("reinitialize_for_display_change: starting");
        if self.recording.is_some() {
            // Stop any active recording before resetting the context.
            log_critical_operation("reinitialize_for_display_change: stopping recording");
            self.stop_recording()
                .context("Failed to stop recording before reinit")?;
        }

        // Drop sources/scene/recording first to release OBS references.
        log_critical_operation("reinitialize_for_display_change: clearing capture_sources");
        self.capture_sources.clear();
        self.app_scenes.clear();
        self.blank_scene = None;
        log_critical_operation("reinitialize_for_display_change: dropping scene");
        self.scene = None;
        log_critical_operation("reinitialize_for_display_change: dropping recording");
        self.recording = None;
        self.current_session = None;

        // Drop the OBS context to allow a clean re-init.
        log_critical_operation("reinitialize_for_display_change: dropping OBS context (CRITICAL)");
        self.context = None;
        log_critical_operation("reinitialize_for_display_change: OBS context dropped successfully");

        if let Ok(mut state) = self.state.write() {
            state.any_source_active = false;
            state.recording.is_recording = false;
            state.recording.is_paused = false;
            state.recording.output_path = None;
            state.should_capture = false;
        }

        // Recreate OBS with fresh display resolution and restore sources.
        log_critical_operation("reinitialize_for_display_change: calling initialize()");
        self.initialize()
            .context("Failed to reinitialize OBS context")?;
        self.fully_recreate_sources()
            .context("Failed to re-setup capture after reinit")?;
        log_critical_operation("reinitialize_for_display_change: completed successfully");

        Ok(())
    }

    /// Set the recording configuration
    pub fn set_recording_config(&mut self, config: RecordingConfig) {
        self.recording_config = config;
    }

    /// The current recording canvas (base) dimensions in pixels — what OBS composites into,
    /// captured when the video info was last (re)built. With macOS multi-monitor on this is the
    /// normalized envelope; otherwise the display resolution. `(0, 0)` before initialize.
    pub fn canvas_dimensions(&self) -> (u32, u32) {
        self.canvas_dims
    }

    /// Layout metadata for the segment: the display currently captured (which physical monitor
    /// the video shows) and the full monitor arrangement. `(None, empty)` when the macOS
    /// multi-monitor path is inactive (flag off / non-macOS / not single-active).
    pub fn capture_layout_metadata(
        &self,
    ) -> (
        Option<crate::data::MonitorInfo>,
        Vec<crate::data::MonitorInfo>,
    ) {
        #[cfg(target_os = "macos")]
        {
            if self.mac_multi_monitor_enabled() && self.use_single_active_app_capture() {
                let all = super::mac_geometry::describe_all_displays();
                let active = self
                    .active_display_uuid()
                    .and_then(|uuid| all.iter().find(|m| m.uuid == uuid).cloned());
                return (active, all);
            }
        }
        (None, Vec::new())
    }

    /// UUID of the display the active app is currently captured on (macOS multi-monitor), for
    /// reporting the active display and detecting a follow-focus switch (even between two
    /// same-resolution monitors). Prefers the retarget cache; before the first poll retargets
    /// (and right after a source rebuild, which clears the cache) the source is pinned to the
    /// main display, so we report the main display's UUID rather than nothing — this is the
    /// single source of truth shared by `capture_layout_metadata` and the re-emit change check,
    /// so they can never disagree. `None` when the multi-monitor path is inactive or no app is
    /// active (blank scene).
    pub fn active_display_uuid(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            if self.mac_multi_monitor_enabled() && self.use_single_active_app_capture() {
                let app = self.active_capture_app.as_ref()?;
                return match self.last_display_uuid.get(app) {
                    Some(uuid) => Some(uuid.clone()),
                    // Source is created pinned to the main display (see new_application_capture);
                    // report that until the first poll's fit retargets it.
                    None => super::get_main_display_uuid().ok(),
                };
            }
        }
        None
    }

    /// Generate output path for a new recording session
    fn generate_output_path(&self, session_id: &str) -> PathBuf {
        let extension = match self.recording_config.format {
            libobs_simple::output::simple::OutputFormat::QuickTime
            | libobs_simple::output::simple::OutputFormat::HybridMov
            | libobs_simple::output::simple::OutputFormat::FragmentedMOV => "mov",
            libobs_simple::output::simple::OutputFormat::MatroskaVideo => "mkv",
            libobs_simple::output::simple::OutputFormat::FlashVideo => "flv",
            libobs_simple::output::simple::OutputFormat::MpegTs => "ts",
            _ => "mp4",
        };

        self.output_directory
            .join(format!("recording_{}.{}", session_id, extension))
    }

    /// Start recording a new session
    ///
    /// Returns the session ID and output path.
    pub fn start_recording(&mut self, session_id: String) -> Result<RecordingSession> {
        if self.recording.is_some() {
            anyhow::bail!("Recording already in progress");
        }

        let context = self
            .context
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?
            .clone();

        let output_path = self.generate_output_path(&session_id);

        // Ensure output directory exists
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create output directory")?;
        }

        info!(
            "Starting recording session {} to {:?}",
            session_id, output_path
        );

        // Create and start recording
        let mut recording =
            RecordingOutput::new(context.clone(), output_path.clone(), &self.recording_config)
                .context("Failed to create recording output")?;

        recording.start().context("Failed to start recording")?;

        // Get the start timestamp from OBS
        let start_time_ns = context
            .get_video_frame_time()
            .context("Failed to get video frame time")?;

        let session = RecordingSession {
            session_id,
            output_path,
            start_time_ns,
        };

        self.recording = Some(recording);
        self.current_session = Some(session.clone());

        // Update state
        if let Ok(mut state) = self.state.write() {
            state.recording.is_recording = true;
            state.recording.is_paused = false;
            state.recording.output_path = Some(session.output_path.clone());
            state.should_capture = state.any_source_active;
        }

        Ok(session)
    }

    /// Stop the current recording session
    ///
    /// Returns the completed session info with output path.
    pub fn stop_recording(&mut self) -> Result<Option<RecordingSession>> {
        let recording = match self.recording.take() {
            Some(r) => r,
            None => {
                debug!("No recording in progress");
                return Ok(None);
            }
        };

        let session = self.current_session.take();

        info!("Stopping recording...");

        let mut recording = recording;
        let output_path = recording.stop().context("Failed to stop recording")?;

        info!("Recording stopped: {:?}", output_path);

        // Update state
        if let Ok(mut state) = self.state.write() {
            state.recording.is_recording = false;
            state.recording.is_paused = false;
            state.recording.output_path = None;
            state.should_capture = false;
        }

        Ok(session)
    }

    /// Check if currently recording
    pub fn is_recording(&self) -> bool {
        self.recording.as_ref().map_or(false, |r| r.is_recording())
    }

    /// Pause recording
    pub fn pause_recording(&mut self) -> Result<()> {
        let recording = match self.recording.as_mut() {
            Some(r) => r,
            None => {
                debug!("No recording in progress to pause");
                return Ok(());
            }
        };

        recording.pause()?;

        // Update state
        if let Ok(mut state) = self.state.write() {
            state.recording.is_paused = true;
            state.should_capture = false;
        }

        Ok(())
    }

    /// Resume recording
    pub fn resume_recording(&mut self) -> Result<()> {
        let recording = match self.recording.as_mut() {
            Some(r) => r,
            None => {
                debug!("No recording in progress to resume");
                return Ok(());
            }
        };

        recording.resume()?;

        // Update state
        if let Ok(mut state) = self.state.write() {
            state.recording.is_paused = false;
            state.should_capture = state.recording.is_recording && state.any_source_active;
        }

        Ok(())
    }

    /// Check if recording is paused
    pub fn is_paused(&self) -> bool {
        self.recording.as_ref().map_or(false, |r| r.is_paused())
    }

    /// Get the current recording session info
    pub fn current_session(&self) -> Option<&RecordingSession> {
        self.current_session.as_ref()
    }

    /// Return the active application capture target in single-active mode.
    pub fn active_capture_app(&self) -> Option<&str> {
        self.active_capture_app.as_deref()
    }

    /// The capture mode this context routes to, mirroring `setup_capture` exactly:
    /// "single_active_app" (follow-focus per-app), "display" (full-screen display
    /// capture; no target apps), or "multi_source_app" (legacy multi-source per-app).
    /// Uses `use_single_active_app_capture()` rather than raw config because that
    /// method includes the platform-capability gate.
    pub fn capture_mode(&self) -> &'static str {
        if self.use_single_active_app_capture() {
            "single_active_app"
        } else if self.target_apps.is_empty() {
            "display"
        } else {
            "multi_source_app"
        }
    }

    /// Check if an app needs a scene created (wasn't running at startup).
    pub fn needs_scene_for_app(&self, bundle_id: &str) -> bool {
        let canonical = Self::canonical_app_id(bundle_id);
        self.use_single_active_app_capture()
            && self
                .target_apps
                .iter()
                .any(|a| Self::canonical_app_id(a) == canonical)
            && !self.app_scenes.contains_key(&canonical)
    }

    /// True on a GNOME-Wayland session driving picker-free per-window capture via Mutter
    /// ScreenCast. In this mode capture follows the *focused window* dynamically (scenes are
    /// created lazily and the node source is re-pointed on focus changes), so the engine must
    /// NOT process-restart for an app that appears after startup — it binds lazily instead.
    /// Always false off Linux, so macOS/Windows behaviour is unchanged.
    pub fn is_gnome_dynamic(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.gnome_screencast.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// GNOME Wayland follow-focus: ensure the capture for `bundle_id` is bound to the exact
    /// window the user currently has focused (`window_id`, the Mutter id reported by the focus
    /// extension). Creates the app's scene+source on first focus (lazy) and re-points the node
    /// source to the focused window as focus moves between the app's windows (and to brand-new
    /// windows). The re-point is **in place**: a fresh Mutter session/node is brought up, the
    /// existing source's `ConnectNode` is updated (`obs_source_update` → patched obs-pipewire
    /// reconnects the stream, no source/scene-item churn), then the old Mutter session is
    /// stopped (make-before-break). Idempotent: a no-op (returns `Ok(false)`) when already bound
    /// to `window_id` or when not in GNOME dynamic mode. Returns `Ok(true)` when it created or
    /// re-pointed a source.
    #[cfg(target_os = "linux")]
    pub fn gnome_ensure_focused_window(&mut self, bundle_id: &str, window_id: u64) -> Result<bool> {
        if self.gnome_screencast.is_none() {
            return Ok(false);
        }
        let prev_window = self.gnome_bound_window.get(bundle_id).copied();
        if prev_window == Some(window_id) {
            return Ok(false); // already showing the focused window
        }

        // Don't re-attempt a window-id we already failed to bind this session. A failure is
        // usually permanent for the session (e.g. the obs-pipewire capture source isn't
        // registered because the active portal advertises monitor-only), so without this guard
        // the caller — which runs every focus poll — would recreate and leak a scene + Mutter
        // session ~10×/s. The marker is cleared on a successful bind and on capture reconfigure,
        // and is keyed by window-id so focusing a *different* window still retries.
        if self.gnome_bind_failed.get(bundle_id) == Some(&window_id) {
            return Ok(false);
        }

        // Fresh Mutter node for the focused window, brought up before the old session is
        // stopped (make-before-break — the old node stays alive until the re-point lands).
        let node = match self
            .gnome_screencast
            .as_ref()
            .unwrap()
            .record_window(window_id)
        {
            Ok(node) => node,
            Err(e) => {
                self.gnome_bind_failed
                    .insert(bundle_id.to_string(), window_id);
                return Err(anyhow::anyhow!(
                    "Mutter RecordWindow(id={window_id}) for '{bundle_id}': {e}"
                ));
            }
        };

        if self.app_scenes.contains_key(bundle_id) {
            // Re-point IN PLACE: update the existing source's ConnectNode. The patched
            // obs-pipewire reconnects the stream to the new node without recreating the source
            // or its scene item (preserves z-order/transform; matches the update-in-place
            // convention used on macOS/X11).
            let (_, source) = self
                .app_scenes
                .get_mut(bundle_id)
                .expect("app scene present");
            source.update_connect_node(node)?;
            if let Some(old_window) = prev_window {
                self.gnome_screencast
                    .as_ref()
                    .unwrap()
                    .stop_window(old_window);
            }
            info!(
                "GNOME follow-focus: re-pointed '{}' to window {} (node {})",
                bundle_id, window_id, node
            );
        } else {
            // Lazy create: a fresh scene with the focused window's node source.
            let scene_name = Self::build_scene_name(&format!("scene_{}", bundle_id));
            let mut scene = self.create_scene(&scene_name)?;
            let context = self
                .context
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;
            let source_name = format!("app_capture_{}", bundle_id);
            let source = match ScreenCaptureSource::new_window_node_capture(
                context,
                &mut scene,
                &source_name,
                bundle_id,
                node,
            ) {
                Ok(source) => source,
                Err(e) => {
                    // Bind failed: drop the just-created scene and stop the Mutter session we
                    // brought up so a failure doesn't leak a scene + node, and remember the
                    // window-id so the caller stops re-attempting it every poll (fail closed).
                    drop(scene);
                    self.gnome_screencast
                        .as_ref()
                        .unwrap()
                        .stop_window(window_id);
                    self.gnome_bind_failed
                        .insert(bundle_id.to_string(), window_id);
                    return Err(e.context(format!(
                        "GNOME per-app capture: binding window {window_id} for '{bundle_id}' \
                         failed (is the obs-pipewire capture source available?)"
                    )));
                }
            };
            // If this app is already the active capture target (e.g. a start-path switch put
            // it active while it had no scene), activate the freshly-created scene now so it
            // doesn't stay blank — the app-switch machinery would short-circuit otherwise.
            if self.active_capture_app.as_deref() == Some(bundle_id) {
                Self::activate_scene(&mut scene)?;
            }
            self.app_scenes
                .insert(bundle_id.to_string(), (scene, source));
            info!(
                "GNOME follow-focus: created scene for '{}' bound to focused window {} (node {})",
                bundle_id, window_id, node
            );
        }

        self.gnome_bound_window
            .insert(bundle_id.to_string(), window_id);
        // Bound successfully (created or re-pointed) — clear any prior failure marker so a later
        // genuine failure on this app isn't suppressed.
        self.gnome_bind_failed.remove(bundle_id);
        // Re-point changes which window (and possibly which monitor) is captured, so the prior
        // fit no longer applies; force a recompute on the next apply.
        self.last_monitor_fit = None;
        self.update_capture_state_flags();
        Ok(true)
    }

    /// Multi-monitor per-app placement: draw the active app's captured window at its real
    /// on-monitor position, scaled by its monitor's 1080-HEIGHT normalization (4K window
    /// 0.5×, FHD 1.0×, ultrawide 0.75×, …; see monitor_layout.rs — Windows uses short-edge
    /// because its output is uncapped). Window + monitor
    /// geometry comes from the GNOME focus extension (logical coords) or X11/RandR (physical);
    /// the scene scale is derived from the captured buffer size, so HiDPI / fractional scaling
    /// needs no trusted scale factor. De-duped via `last_monitor_fit`; a no-op until the source
    /// has non-zero dimensions, and off single-active per-app mode. Safe to call every poll.
    #[cfg(target_os = "linux")]
    pub fn apply_monitor_fit_to_active(&mut self) {
        use libobs_wrapper::enums::{obs_alignment, ObsBoundsType};
        use libobs_wrapper::graphics::Vec2;
        use libobs_wrapper::scenes::ObsTransformInfoBuilder;

        if !self.use_single_active_app_capture() {
            return;
        }
        let Some(app) = self.active_capture_app.clone() else {
            return;
        };
        let Some((win, mon, monitor_scale)) = self.active_window_monitor_rects(&app) else {
            return;
        };
        let Some(fit) = super::monitor_layout::fit_for_window(win, mon, monitor_scale) else {
            return;
        };

        let key = (
            app.clone(),
            fit.scale.to_bits(),
            fit.pos_x.to_bits(),
            fit.pos_y.to_bits(),
        );
        if self.last_monitor_fit.as_ref() == Some(&key) {
            return;
        }

        // Explicit transform: scaled by the monitor factor, positioned at the window's real
        // on-monitor offset (canvas px), top-left aligned, no bounds.
        let info = ObsTransformInfoBuilder::new()
            .set_pos(Vec2::new(fit.pos_x, fit.pos_y))
            .set_scale(Vec2::new(fit.scale, fit.scale))
            .set_alignment(obs_alignment::LEFT | obs_alignment::TOP)
            .set_bounds_type(ObsBoundsType::None)
            .build(0, 0);

        let applied = match self.app_scenes.get(&app) {
            Some((scene, source)) => scene.set_transform_info(source.source(), &info).is_ok(),
            None => false,
        };
        if applied {
            debug!(
                "monitor-fit '{}': scale {:.3} pos ({:.0},{:.0}) [mon {}x{} @{}x]",
                app, fit.scale, fit.pos_x, fit.pos_y, mon.w, mon.h, monitor_scale
            );
            self.last_monitor_fit = Some(key);
        }
    }

    /// The focused window's frame rect + its monitor's rect for `app`, in one coordinate space
    /// (GNOME logical via the focus extension; X11 physical via RandR + window geometry). `None`
    /// if geometry isn't resolvable right now — caller skips the fit and retries next poll.
    #[cfg(target_os = "linux")]
    fn active_window_monitor_rects(
        &self,
        app: &str,
    ) -> Option<(
        super::monitor_layout::Rect,
        super::monitor_layout::Rect,
        f64,
    )> {
        use super::monitor_layout::Rect;
        if let Some(gsc) = self.gnome_screencast.as_ref() {
            let window_id = *self.gnome_bound_window.get(app)?;
            let g = gsc.window_geometry(window_id)?;
            if !g.found {
                return None;
            }
            let win = Rect::new(g.win.0, g.win.1, g.win.2, g.win.3);
            let mon = Rect::new(g.mon.0, g.mon.1, g.mon.2, g.mon.3);
            return (mon.w > 0 && mon.h > 0).then_some((win, mon, g.scale));
        }
        // Pure X11: resolve the app's focused window and the RandR monitor it sits on. X11 has
        // no per-monitor scaling, so the scale factor is 1.0 (logical == physical pixels).
        if super::x11_windows::is_pure_x11_session() {
            let wid: u32 = super::x11_windows::resolve_capture_window(app)?
                .parse()
                .ok()?;
            let (wx, wy, ww, wh) = super::x11_windows::x11_window_rect(wid)?;
            let win = Rect::new(wx, wy, ww, wh);
            let monitors: Vec<Rect> = super::x11_windows::x11_monitor_rects()?
                .into_iter()
                .map(|(x, y, w, h)| Rect::new(x, y, w, h))
                .collect();
            let mon = super::monitor_layout::monitor_containing(win, &monitors)?;
            return Some((win, mon, 1.0));
        }
        None
    }

    /// Switch to a different app's pre-created scene.
    /// Instant — just activates the target scene on channel 0.
    pub fn switch_active_app_capture(&mut self, active_app: Option<&str>) -> Result<bool> {
        if !self.use_single_active_app_capture() {
            return Ok(false);
        }

        let next_app = active_app.map(|app| app.to_string());
        if self.active_capture_app == next_app {
            return Ok(false);
        }

        match &next_app {
            Some(bundle_id) => {
                if let Some((scene, source)) = self.app_scenes.get_mut(bundle_id.as_str()) {
                    // X11: the app's window id is ephemeral and the focused window may have
                    // changed since this scene was created — re-resolve and re-point the
                    // XComposite source before showing it. No-op on macOS (scene switch is
                    // sufficient there) and on Wayland.
                    #[cfg(target_os = "linux")]
                    if let Err(e) = source.update_application(bundle_id) {
                        debug!(
                            "X11 re-resolve of capture window for '{}' failed: {}",
                            bundle_id, e
                        );
                    }
                    #[cfg(not(target_os = "linux"))]
                    let _ = source;
                    Self::activate_scene(scene)?;
                    info!(
                        "Switched active application capture to '{}' (scene switch)",
                        bundle_id
                    );
                } else {
                    if let Some(blank) = self.blank_scene.as_mut() {
                        Self::activate_scene(blank)?;
                    }
                    warn!("No scene for '{}'; showing blank", bundle_id);
                }
            }
            None => {
                if let Some(blank) = self.blank_scene.as_mut() {
                    Self::activate_scene(blank)?;
                }
                info!("Cleared active application capture source");
            }
        }

        self.active_capture_app = next_app;
        self.update_capture_state_flags();
        Ok(true)
    }

    /// Force a refresh of the current application capture source.
    /// In multi-scene mode, re-applies the same app via obs_source_update()
    /// to trigger an internal SCStream reset without creating a new source.
    pub fn refresh_active_capture_source(&mut self) -> Result<bool> {
        if !self.use_single_active_app_capture() {
            return Ok(false);
        }

        let Some(bundle_id) = self.active_capture_app.clone() else {
            return Ok(false);
        };

        if let Some((_, source)) = self.app_scenes.get_mut(bundle_id.as_str()) {
            source.update_application(&bundle_id)?;
            info!(
                "Refreshed active application capture for '{}' (in-place update)",
                bundle_id
            );
            return Ok(true);
        }

        warn!(
            "Cannot refresh capture for '{}': no pre-created scene",
            bundle_id
        );
        Ok(false)
    }

    /// Report the dimensions of the currently active source, if any.
    pub fn active_source_dimensions(&self) -> Result<Option<(u32, u32)>> {
        let source = if self.use_single_active_app_capture() {
            self.active_capture_app
                .as_ref()
                .and_then(|app| self.app_scenes.get(app))
                .map(|(_, source)| source)
        } else {
            self.capture_sources.first()
        };

        match source {
            Some(s) => Ok(Some(s.dimensions()?)),
            None => Ok(None),
        }
    }

    /// Apply the monitor-level fit to the active app's capture source: scale the
    /// window by its monitor's 1080-shortest-edge factor and place it at its real
    /// on-monitor position. Re-applied each poll so it tracks the window as it
    /// moves/resizes; de-duplicated so an unchanged transform is a no-op.
    /// Windows-only; a no-op elsewhere (macOS captures the main display only).
    #[cfg(target_os = "windows")]
    pub fn apply_monitor_fit_to_active(&mut self) {
        use libobs_wrapper::enums::{obs_alignment, ObsBoundsType};
        use libobs_wrapper::graphics::Vec2;
        use libobs_wrapper::scenes::ObsTransformInfoBuilder;

        let Some(app) = self.active_capture_app.clone() else {
            return;
        };
        let Some(fit) = super::window_geometry::monitor_fit_for_app(&app) else {
            return;
        };
        let key = (
            app.clone(),
            fit.scale.to_bits(),
            fit.pos_x.to_bits(),
            fit.pos_y.to_bits(),
        );
        if self.last_monitor_fit.as_ref() == Some(&key) {
            return;
        }

        // Explicit transform: no bounds, top-left aligned, scaled by the monitor
        // factor, positioned at the window's real on-monitor offset (in canvas px).
        let info = ObsTransformInfoBuilder::new()
            .set_pos(Vec2::new(fit.pos_x, fit.pos_y))
            .set_scale(Vec2::new(fit.scale, fit.scale))
            .set_alignment(obs_alignment::LEFT | obs_alignment::TOP)
            .set_bounds_type(ObsBoundsType::None)
            .build(0, 0);

        let applied = {
            let Some((scene, source)) = self.app_scenes.get(&app) else {
                return;
            };
            scene.set_transform_info(source.source(), &info).is_ok()
        };
        if applied {
            self.last_monitor_fit = Some(key);
        }
    }

    /// macOS multi-monitor per-app fit + follow-focus. ScreenCaptureKit *Application* capture
    /// hands us a full-DISPLAY-sized frame with the app composited in place (Step 0: the source's
    /// width/height equalled the target display's PIXELS on both a scale-1.0 external and a
    /// scale-2.0 Retina display), so the fit is `scale = norm, pos = (0,0)` with NO per-window
    /// offset — unlike Windows/Linux, which offset a window-cropped buffer by its on-monitor
    /// position.
    ///
    /// Follow-focus: when the active app's focused window is on a different display than the
    /// source is currently pointed at, retarget the source to that display (`update_display_uuid`,
    /// which restarts the SCStream — deduped via `last_display_uuid` so it only fires on an actual
    /// display change, not every poll) and normalize by that display. The active app's window is
    /// resolved via CGWindowList only when the app is the frontmost app (single-active tracks
    /// frontmost); when it isn't, the current placement is kept to avoid churning as focus flicks
    /// to non-target apps, and the main display is the default only for the first placement.
    ///
    /// De-duped via `last_monitor_fit`; gated on the kill-switch flag AND single-active mode; a
    /// no-op until a scene exists. Safe to call every poll.
    #[cfg(target_os = "macos")]
    pub fn apply_monitor_fit_to_active(&mut self) {
        use libobs_wrapper::enums::{obs_alignment, ObsBoundsType};
        use libobs_wrapper::graphics::Vec2;
        use libobs_wrapper::scenes::ObsTransformInfoBuilder;

        if !self.mac_multi_monitor_enabled() || !self.use_single_active_app_capture() {
            return;
        }
        let Some(app) = self.active_capture_app.clone() else {
            return;
        };

        // Which display is the active app's focused window on? Only trust CGWindowList when the
        // active app is the frontmost app; otherwise keep the current placement (no churn). Fall
        // back to the main display only for the very first placement of this app.
        let resolved = get_frontmost_app()
            .filter(|f| f.bundle_id == app)
            .and_then(|f| super::mac_geometry::window_display_for_pid(f.pid));
        let target = match resolved {
            Some(t) => t,
            None => {
                // Can't resolve the window now (active app not frontmost, or a transient
                // window/Space gap). If this app already has a placement, keep it (avoid churn
                // and a wrong reset to main); only default to main for its very first placement.
                if self.last_display_uuid.contains_key(&app) {
                    return;
                }
                match super::mac_geometry::main_display_target() {
                    Some(t) => t,
                    None => return,
                }
            }
        };

        // Retarget the SCK source to the focused window's display when it changed for THIS app
        // (deduped per-app; update_display_uuid is itself idempotent, so a no-op target never
        // restarts the stream). A display change also changes `norm`, so invalidate the fit cache
        // — but ONLY after a successful retarget, so a failed retarget doesn't apply the new
        // display's norm to a source still pointed at the old display.
        if self.last_display_uuid.get(&app).map(String::as_str) != Some(target.uuid.as_str()) {
            match self.app_scenes.get_mut(&app) {
                Some((_, source)) => match source.update_display_uuid(&target.uuid) {
                    Ok(()) => {
                        debug!(
                            "macOS follow-focus: retargeted '{}' to display {} ({})",
                            app, target.id, target.uuid
                        );
                        self.last_display_uuid.insert(app.clone(), target.uuid.clone());
                        self.last_monitor_fit = None;
                    }
                    Err(e) => {
                        // Leave caches unchanged so the next poll retries; do NOT fall through to
                        // apply target.norm — the source is still on its previous display, whose
                        // (still-correct) transform remains in effect.
                        debug!("macOS follow-focus retarget failed for '{}': {}", app, e);
                        return;
                    }
                },
                None => return, // no scene for this app yet
            }
        }

        // Apply the transform: normalized by the target display, positioned at the canvas origin.
        let norm = target.norm;
        let key = (app.clone(), norm.to_bits(), 0f32.to_bits(), 0f32.to_bits());
        if self.last_monitor_fit.as_ref() == Some(&key) {
            return;
        }
        let info = ObsTransformInfoBuilder::new()
            .set_pos(Vec2::new(0.0, 0.0))
            .set_scale(Vec2::new(norm, norm))
            .set_alignment(obs_alignment::LEFT | obs_alignment::TOP)
            .set_bounds_type(ObsBoundsType::None)
            .build(0, 0);
        let applied = match self.app_scenes.get(&app) {
            Some((scene, source)) => scene.set_transform_info(source.source(), &info).is_ok(),
            None => false,
        };
        if applied {
            debug!(
                "macOS monitor-fit '{}': scale {:.3} pos (0,0) [display {}]",
                app, norm, target.id
            );
            self.last_monitor_fit = Some(key);
        }
    }

    /// No-op on platforms without per-monitor fit (Windows, Linux, and macOS all have real
    /// implementations above).
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    pub fn apply_monitor_fit_to_active(&mut self) {}

    /// Return whether the active source has started producing non-zero-sized frames.
    pub fn active_source_is_ready(&self) -> Result<bool> {
        let Some((width, height)) = self.active_source_dimensions()? else {
            return Ok(false);
        };

        Ok(width > 0 && height > 0)
    }

    /// Get the current video frame time from OBS (in nanoseconds)
    ///
    /// This is the monotonic timestamp used by OBS for video frames.
    /// Use this to synchronize input events with video.
    pub fn get_video_frame_time(&self) -> Result<u64> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;

        context
            .get_video_frame_time()
            .context("Failed to get video frame time")
    }

    /// Get a reference to the libobs context
    pub fn context(&self) -> Option<&ObsContext> {
        self.context.as_ref()
    }

    /// Get a mutable reference to the libobs context
    pub fn context_mut(&mut self) -> Option<&mut ObsContext> {
        self.context.as_mut()
    }

    /// Get the current capture state
    pub fn get_state(&self) -> CaptureState {
        self.state.read().unwrap().clone()
    }

    /// Check if we should be capturing input
    pub fn should_capture(&self) -> bool {
        self.state.read().map(|s| s.should_capture).unwrap_or(false)
    }

    /// Update the capture state
    pub fn update_state<F>(&self, f: F)
    where
        F: FnOnce(&mut CaptureState),
    {
        if let Ok(mut state) = self.state.write() {
            f(&mut state);

            // Recompute should_capture
            state.should_capture = state.recording.is_recording
                && !state.recording.is_paused
                && state.any_source_active;
        }
    }

    /// Get the output directory for recordings
    pub fn output_directory(&self) -> &PathBuf {
        &self.output_directory
    }

    /// Check if the context is initialized
    pub fn is_initialized(&self) -> bool {
        self.context.is_some()
    }

    /// Check if capture sources are set up
    pub fn is_capture_setup(&self) -> bool {
        if self.use_single_active_app_capture() {
            self.blank_scene.is_some() || !self.app_scenes.is_empty()
        } else {
            self.scene.is_some() && !self.capture_sources.is_empty()
        }
    }

    /// Tear down all capture scenes/sources so `is_capture_setup()` becomes false. Used when
    /// a capture source dies; the dead source must not be silently reused, so the next
    /// `setup_capture()` establishes a fresh display portal session or Mutter per-app node.
    /// Does not touch the OBS context.
    pub fn teardown_capture(&mut self) {
        self.capture_sources.clear();
        self.app_scenes.clear();
        self.scene = None;
        self.blank_scene = None;
        // Closes the Mutter ScreenCast sessions backing any picker-free per-app nodes.
        #[cfg(target_os = "linux")]
        {
            self.gnome_screencast = None;
        }
        self.active_capture_app = None;
        self.update_capture_state_flags();
    }

    /// Get the number of active capture sources
    pub fn capture_source_count(&self) -> usize {
        self.capture_sources.len()
    }

    /// Get information about capture sources
    pub fn capture_source_names(&self) -> Vec<&str> {
        self.capture_sources.iter().map(|s| s.name()).collect()
    }

    /// Collect the current xdg-desktop-portal restore token from the active Wayland display
    /// source. Supported Linux per-app capture does not use portal restore tokens.
    pub fn collect_restore_tokens(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        #[cfg(target_os = "linux")]
        for source in &self.capture_sources {
            if let Some(app) = source.app_id() {
                if app != super::sources::DISPLAY_CAPTURE_KEY {
                    continue;
                }
                if let Some(token) = source.restore_token() {
                    if !token.is_empty() {
                        out.insert(app.to_string(), token);
                    }
                }
            }
        }
        out
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
struct ObsBootstrapNotificationHandler {
    notify_download: bool,
    download_notified: bool,
}

#[cfg(not(target_os = "linux"))]
impl ObsBootstrapNotificationHandler {
    fn new(notify_download: bool) -> Self {
        Self {
            notify_download,
            download_notified: false,
        }
    }
}

#[cfg(target_os = "macos")]
fn obs_runtime_root() -> Option<PathBuf> {
    if let Ok(runtime_dir) = std::env::var("CROWD_CAST_OBS_RUNTIME_DIR") {
        return Some(PathBuf::from(runtime_dir));
    }

    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/dev.crowd-cast.agent/obs/current"))
}

#[cfg(target_os = "macos")]
fn obs_startup_paths_from_env() -> Option<StartupPaths> {
    let runtime_root = obs_runtime_root()?;

    if !runtime_root.exists() {
        return None;
    }

    let libobs_data = runtime_root.join("data/libobs");
    let plugin_bin = runtime_root.join("obs-plugins/%module%.plugin/Contents/MacOS");
    let plugin_data = runtime_root.join("data/obs-plugins/%module%");

    let paths = StartupPaths::new(
        ObsPath::new(libobs_data.to_string_lossy().as_ref()),
        ObsPath::new(plugin_bin.to_string_lossy().as_ref()),
        ObsPath::new(plugin_data.to_string_lossy().as_ref()),
    );

    info!(
        "Using external OBS runtime paths from {}",
        runtime_root.display()
    );
    Some(paths)
}

/// Compile-time OBS ABI this binary's libobs bindings target (e.g. "32.0.2"), baked by build.rs.
/// The self-provisioned bundle lives under `~/.local/share/crowd-cast/obs/<abi>/` (rooted at `usr/`).
#[cfg(target_os = "linux")]
const OBS_ABI: &str = env!("CROWD_CAST_OBS_ABI");

/// Root of the libobs bundle shipped with / provisioned for this binary's ABI.
#[cfg(target_os = "linux")]
fn self_provisioned_bundle_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".local/share/crowd-cast/obs")
            .join(OBS_ABI),
    )
}

/// A bundle is considered present/intact when libobs' effects data dir is there.
#[cfg(target_os = "linux")]
fn bundle_is_present(root: &std::path::Path) -> bool {
    root.join("usr/share/obs/libobs/default.effect").exists()
}

/// StartupPaths for the self-provisioned bundle (tree rooted at `usr/`). Matches the layout
/// produced by packaging/linux/build-bundle.sh and the legacy CROWD_CAST_OBS_* vars.
#[cfg(target_os = "linux")]
fn self_provisioned_startup_paths() -> Option<StartupPaths> {
    let root = self_provisioned_bundle_root()?;
    if !bundle_is_present(&root) {
        return None;
    }
    let data = root.join("usr/share/obs/libobs");
    let plugin_bin = root.join("usr/lib/obs-plugins");
    let plugin_data = root.join("usr/share/obs/obs-plugins/%module%");
    info!("Using self-provisioned libobs bundle at {}", root.display());
    Some(StartupPaths::new(
        ObsPath::new(data.to_string_lossy().as_ref()),
        ObsPath::new(plugin_bin.to_string_lossy().as_ref()),
        ObsPath::new(plugin_data.to_string_lossy().as_ref()),
    ))
}

/// Resolve libobs runtime paths for Linux, in precedence order:
///   1. Explicit `CROWD_CAST_OBS_*` env overrides (dev / system-install escape hatch). All three
///      must be set: `CROWD_CAST_OBS_DATA_PATH`, `CROWD_CAST_OBS_PLUGIN_BIN_PATH`,
///      `CROWD_CAST_OBS_PLUGIN_DATA_PATH`.
///   2. The self-provisioned bundle shipped with the binary
///      (`~/.local/share/crowd-cast/obs/<abi>/usr/...`, ABI baked at build time) — the default,
///      so the bare binary needs no env/wrapper.
///   3. `None` -> `StartupInfo::default()`, which points libobs-wrapper at a system OBS install
///      (`/usr/share/obs/libobs` + `/usr/lib/<arch>/obs-plugins`).
#[cfg(target_os = "linux")]
fn obs_startup_paths_from_env() -> Option<StartupPaths> {
    if let (Ok(data), Ok(plugin_bin), Ok(plugin_data)) = (
        std::env::var("CROWD_CAST_OBS_DATA_PATH"),
        std::env::var("CROWD_CAST_OBS_PLUGIN_BIN_PATH"),
        std::env::var("CROWD_CAST_OBS_PLUGIN_DATA_PATH"),
    ) {
        info!(
            "Using OBS runtime paths from env overrides (data={}, plugin_bin={}, plugin_data={})",
            data, plugin_bin, plugin_data
        );
        return Some(StartupPaths::new(
            ObsPath::new(data.as_str()),
            ObsPath::new(plugin_bin.as_str()),
            ObsPath::new(plugin_data.as_str()),
        ));
    }

    self_provisioned_startup_paths()
}

#[cfg(not(target_os = "linux"))]
impl ObsBootstrapStatusHandler for ObsBootstrapNotificationHandler {
    type Error = Infallible;

    fn handle_downloading(&mut self, _progress: f32, _message: String) -> Result<(), Self::Error> {
        if self.notify_download && !self.download_notified {
            self.download_notified = true;
            show_obs_download_started_notification();
        }
        Ok(())
    }

    fn handle_extraction(&mut self, _progress: f32, _message: String) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Drop for CaptureContext {
    fn drop(&mut self) {
        log_critical_operation("CaptureContext::drop: starting");

        // Stop any active recording first
        if self.recording.is_some() {
            info!("Stopping recording during shutdown...");
            log_critical_operation("CaptureContext::drop: stopping recording");
            if let Err(e) = self.stop_recording() {
                warn!("Error stopping recording during shutdown: {}", e);
            }
        }

        if self.context.is_some() {
            info!("Shutting down libobs context...");
            log_critical_operation("CaptureContext::drop: dropping ObsContext (CRITICAL)");
        }
        // ObsContext handles cleanup in its own Drop implementation
        log_critical_operation("CaptureContext::drop: completed");
    }
}
