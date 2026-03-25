//! OBS Context management for embedded libobs
//!
//! Handles initialization, bootstrapping, and lifecycle of the libobs context.
//! Provides high-level API for screen capture and recording with automatic
//! encoder selection (HEVC preferred with hardware acceleration).
//!
//! Supports both display capture (entire screen) and application capture
//! (specific applications by bundle ID).

use anyhow::{Context as _, Result};
use libobs_bootstrapper::{
    status_handler::ObsBootstrapStatusHandler, ObsBootstrapper, ObsBootstrapperOptions,
    ObsBootstrapperResult,
};
use libobs_wrapper::context::ObsContext;
use libobs_wrapper::data::video::ObsVideoInfoBuilder;
use libobs_wrapper::scenes::ObsSceneRef;
use libobs_wrapper::utils::{ObsPath, StartupInfo, StartupPaths};
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tracing::{debug, info, warn};

use crate::crash::log_critical_operation;

use super::frontmost::get_frontmost_app;
use super::recording::{calculate_output_dimensions, RecordingConfig, RecordingOutput};
use super::sources::{get_main_display_resolution, get_main_display_uuid, ScreenCaptureSource};
use super::CaptureState;
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
    /// Target apps for capture (stored for recreation after display changes)
    target_apps: Vec<String>,
    /// Whether macOS should keep only one tracked application's source active at a time
    single_active_app_capture: bool,
    /// Currently active application capture target when single-active mode is enabled
    active_capture_app: Option<String>,
}

impl CaptureContext {
    /// Bootstrap OBS binaries if needed and create a new capture context
    pub async fn new(output_directory: PathBuf) -> Result<Self> {
        info!("Initializing embedded libobs capture context...");

        // Bootstrap OBS binaries (download if not present)
        let bootstrap_result = Self::bootstrap_obs().await?;

        match bootstrap_result {
            ObsBootstrapperResult::None => {
                debug!("OBS binaries already present");
            }
            ObsBootstrapperResult::Restart => {
                // On Windows this means we need to restart. On macOS, Done is returned instead.
                warn!("OBS bootstrap requires restart - this shouldn't happen on macOS");
                anyhow::bail!("OBS bootstrap requires application restart");
            }
        }

        Ok(Self {
            context: None,
            scene: None,
            capture_sources: Vec::new(),
            app_scenes: HashMap::new(),
            blank_scene: None,
            recording: None,
            current_session: None,
            state: Arc::new(RwLock::new(CaptureState::default())),
            output_directory,
            recording_config: RecordingConfig::default(),
            target_apps: Vec::new(),
            single_active_app_capture: false,
            active_capture_app: None,
        })
    }

    /// Bootstrap OBS binaries
    async fn bootstrap_obs() -> Result<ObsBootstrapperResult> {
        let notify_download = is_running_in_app_bundle();
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

        // Get actual display resolution from CoreGraphics (handles Retina correctly)
        // Fall back to OBS defaults if detection fails
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

        // Calculate output dimensions (aspect-preserving, max height from config)
        let (output_width, output_height) = calculate_output_dimensions(
            base_width,
            base_height,
            self.recording_config.max_output_height,
        );

        info!(
            "Video config: {}x{} (native) -> {}x{} (output), {} fps",
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
        #[cfg(target_os = "macos")]
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
    pub fn set_single_active_app_capture(&mut self, enabled: bool) {
        self.single_active_app_capture = enabled;
    }

    fn use_single_active_app_capture(&self) -> bool {
        cfg!(target_os = "macos") && self.single_active_app_capture && !self.target_apps.is_empty()
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

    fn select_initial_active_app(&self) -> Option<String> {
        if !self.use_single_active_app_capture() {
            return None;
        }

        let frontmost = get_frontmost_app()?;
        if self
            .target_apps
            .iter()
            .any(|app| app == &frontmost.bundle_id)
        {
            Some(frontmost.bundle_id)
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

        // Clean up any existing app scenes
        self.app_scenes.clear();
        self.blank_scene = None;

        // Create blank scene (shown when no tracked app is frontmost)
        let blank_scene_name = Self::build_scene_name("blank");
        let mut blank_scene = self.create_scene(&blank_scene_name)?;
        if initial_active_app.is_none() {
            Self::activate_scene(&mut blank_scene)?;
        }
        self.blank_scene = Some(blank_scene);

        let display_uuid = get_main_display_uuid()
            .context("Failed to get main display UUID for application capture")?;
        let capture_audio = self.recording_config.enable_audio;
        let target_apps = self.target_apps.clone();

        let context = self
            .context
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;

        for bundle_id in &target_apps {
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
            ) {
                Ok(source) => {
                    if initial_active_app == Some(bundle_id.as_str()) {
                        Self::activate_scene(&mut scene)?;
                        self.active_capture_app = Some(bundle_id.clone());
                    }
                    info!("Created app scene for '{}'", bundle_id);
                    self.app_scenes.insert(bundle_id.clone(), (scene, source));
                }
                Err(e) => {
                    warn!(
                        "Failed to create capture source for '{}': {}. Skipping.",
                        bundle_id, e
                    );
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
    fn setup_display_or_multi_capture(&mut self) -> Result<usize> {
        if !self.is_initialized() {
            anyhow::bail!("OBS context not initialized");
        }

        self.capture_sources.clear();
        self.scene = None;

        let scene_name = Self::build_scene_name("main_scene");
        let mut scene = self.create_scene(&scene_name)?;

        let capture_audio = self.recording_config.enable_audio;
        let target_apps = self.target_apps.clone();
        let mut capture_sources = Vec::new();

        let context = self
            .context
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;

        if target_apps.is_empty() {
            let source = ScreenCaptureSource::new_display_capture(
                context,
                &mut scene,
                "screen_capture",
                capture_audio,
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
                ) {
                    Ok(source) => {
                        debug!("Created capture source '{}' for '{}'", source_name, bundle_id);
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
    /// Creates the main scene and adds application capture sources for each target app.
    /// If no target apps are specified, falls back to display capture.
    /// Must be called after `initialize()`.
    ///
    /// # Arguments
    /// * `target_apps` - List of bundle identifiers to capture (e.g., ["com.apple.Safari", "com.microsoft.VSCode"])
    pub fn setup_capture(&mut self, target_apps: &[String]) -> Result<()> {
        self.target_apps = target_apps.to_vec();

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

        // Get new display resolution
        let (base_width, base_height) = match get_main_display_resolution() {
            Ok((w, h)) => {
                info!("New display resolution: {}x{}", w, h);
                (w, h)
            }
            Err(e) => {
                warn!(
                    "Failed to detect new display resolution: {}. Using defaults.",
                    e
                );
                let default_video_info = ObsVideoInfoBuilder::new().build();
                (
                    default_video_info.get_base_width(),
                    default_video_info.get_base_height(),
                )
            }
        };

        // Calculate output dimensions
        let (output_width, output_height) = calculate_output_dimensions(
            base_width,
            base_height,
            self.recording_config.max_output_height,
        );

        info!(
            "Resetting video: {}x{} (native) -> {}x{} (output), {} fps",
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

    /// Switch the single-active application capture source to a new target app.
    ///
    /// Passing `None` clears the current application source and leaves the scene blank.
    /// Switch to a different app's pre-created scene.
    /// Instant — just activates the target scene on channel 0, no source lifecycle churn.
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
                if let Some((scene, _)) = self.app_scenes.get_mut(bundle_id.as_str()) {
                    Self::activate_scene(scene)?;
                    info!(
                        "Switched active application capture to '{}' (scene switch)",
                        bundle_id
                    );
                } else {
                    // App not in our pre-created scenes — show blank
                    if let Some(blank) = self.blank_scene.as_mut() {
                        Self::activate_scene(blank)?;
                    }
                    warn!(
                        "No pre-created scene for '{}'; showing blank",
                        bundle_id
                    );
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

    /// Get the number of active capture sources
    pub fn capture_source_count(&self) -> usize {
        self.capture_sources.len()
    }

    /// Get information about capture sources
    pub fn capture_source_names(&self) -> Vec<&str> {
        self.capture_sources.iter().map(|s| s.name()).collect()
    }
}

#[derive(Debug)]
struct ObsBootstrapNotificationHandler {
    notify_download: bool,
    download_notified: bool,
}

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
