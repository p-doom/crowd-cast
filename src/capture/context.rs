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
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tracing::{debug, info, warn};

use crate::crash::log_critical_operation;

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
    /// The main scene for capture sources
    scene: Option<ObsSceneRef>,
    /// Capture sources (one per target application, or single display capture)
    capture_sources: Vec<ScreenCaptureSource>,
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
            recording: None,
            current_session: None,
            state: Arc::new(RwLock::new(CaptureState::default())),
            output_directory,
            recording_config: RecordingConfig::default(),
            target_apps: Vec::new(),
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

    /// Set up capture sources and scene for specific applications
    ///
    /// Creates the main scene and adds application capture sources for each target app.
    /// If no target apps are specified, falls back to display capture.
    /// Must be called after `initialize()`.
    ///
    /// # Arguments
    /// * `target_apps` - List of bundle identifiers to capture (e.g., ["com.apple.Safari", "com.microsoft.VSCode"])
    pub fn setup_capture(&mut self, target_apps: &[String]) -> Result<()> {
        // Store target apps for potential recreation after display changes
        self.target_apps = target_apps.to_vec();

        let context = self
            .context
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;

        // Create the main scene
        let mut scene = context
            .scene("main_scene")
            .context("Failed to create main scene")?;

        // Activate the scene on channel 0
        scene
            .set_to_channel(0)
            .context("Failed to activate scene")?;

        // Audio capture is controlled by recording_config.enable_audio
        let capture_audio = self.recording_config.enable_audio;

        // Clear any existing sources
        self.capture_sources.clear();

        if target_apps.is_empty() {
            // Fallback to display capture if no apps specified
            let capture_source = ScreenCaptureSource::new_display_capture(
                context,
                &mut scene,
                "screen_capture",
                capture_audio,
            )
            .context("Failed to create screen capture source")?;

            debug!("Display capture source created (audio: {})", capture_audio);

            self.capture_sources.push(capture_source);
        } else {
            // Get display UUID for application capture
            let display_uuid = get_main_display_uuid()
                .context("Failed to get main display UUID for application capture")?;

            debug!(
                "Setting up app capture for {} apps (display: {})",
                target_apps.len(),
                display_uuid
            );

            // Create application capture source for each target app
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
                        debug!(
                            "Created capture source '{}' for '{}'",
                            source_name, bundle_id
                        );
                        self.capture_sources.push(source);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to create capture source for '{}': {}. Skipping.",
                            bundle_id, e
                        );
                        // Continue with other apps rather than failing completely
                    }
                }
            }

            if self.capture_sources.is_empty() {
                anyhow::bail!(
                    "Failed to create any capture sources for target apps: {:?}",
                    target_apps
                );
            }

            info!(
                "Created {} application capture sources (audio: {})",
                self.capture_sources.len(),
                capture_audio
            );
        }

        self.scene = Some(scene);

        // Update state
        if let Ok(mut state) = self.state.write() {
            state.any_source_active = !self.capture_sources.is_empty();
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
        log_critical_operation("fully_recreate_sources: starting");
        if !self.is_initialized() {
            anyhow::bail!("OBS context not initialized");
        }

        let target_apps = self.target_apps.clone();

        info!(
            "Fully recreating capture sources for {} target app(s)",
            target_apps.len()
        );

        // Clear Rust-side source references
        let old_count = self.capture_sources.len();
        self.capture_sources.clear();
        debug!(
            "Cleared {} existing capture source(s) from Rust Vec",
            old_count
        );

        // Drop the old scene reference - this allows OBS to clean up scene items
        // when we create a new scene
        if self.scene.is_some() {
            debug!("Dropping old scene reference");
            self.scene = None;
        }

        // Get the context
        let context = self
            .context
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;

        // Create a fresh scene with a unique name to avoid conflicts with the old one
        // (The old scene will be cleaned up when all references to it are dropped)
        let scene_name = format!(
            "main_scene_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );

        debug!("Creating new scene: {}", scene_name);
        let mut scene = context
            .scene(scene_name.as_str())
            .context("Failed to create new scene")?;

        // Activate the new scene on channel 0 (replaces any existing scene)
        scene
            .set_to_channel(0)
            .context("Failed to activate new scene")?;

        let capture_audio = self.recording_config.enable_audio;

        if target_apps.is_empty() {
            // Fallback to display capture if no apps specified
            let capture_source = ScreenCaptureSource::new_display_capture(
                context,
                &mut scene,
                "screen_capture",
                capture_audio,
            )
            .context("Failed to create screen capture source")?;

            debug!(
                "Recreated display capture source (audio: {})",
                capture_audio
            );
            self.capture_sources.push(capture_source);
        } else {
            // Get fresh display UUID
            let display_uuid = get_main_display_uuid()
                .context("Failed to get main display UUID for application capture")?;

            debug!(
                "Recreating app capture for {} apps (display: {})",
                target_apps.len(),
                display_uuid
            );

            // Create application capture source for each target app
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
                        debug!(
                            "Recreated capture source '{}' for '{}'",
                            source_name, bundle_id
                        );
                        self.capture_sources.push(source);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to recreate capture source for '{}': {}. Skipping.",
                            bundle_id, e
                        );
                    }
                }
            }

            if self.capture_sources.is_empty() {
                anyhow::bail!(
                    "Failed to recreate any capture sources for target apps: {:?}",
                    target_apps
                );
            }
        }

        // Store the new scene
        self.scene = Some(scene);

        let new_count = self.capture_sources.len();
        info!(
            "Fully recreated {} capture source(s) with new scene",
            new_count
        );

        // Update state
        if let Ok(mut state) = self.state.write() {
            state.any_source_active = !self.capture_sources.is_empty();
        }

        log_critical_operation("fully_recreate_sources: completed successfully");
        Ok(new_count)
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

        let target_apps = self.target_apps.clone();

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

        // Clear sources and scene before reset
        log_critical_operation("reset_video_and_recreate_sources: clearing sources");
        self.capture_sources.clear();
        self.scene = None;

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
        log_critical_operation("reset_video_and_recreate_sources: calling setup_capture()");
        self.setup_capture(&target_apps)
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
        let target_apps = self.target_apps.clone();

        if self.recording.is_some() {
            // Stop any active recording before resetting the context.
            log_critical_operation("reinitialize_for_display_change: stopping recording");
            self.stop_recording()
                .context("Failed to stop recording before reinit")?;
        }

        // Drop sources/scene/recording first to release OBS references.
        log_critical_operation("reinitialize_for_display_change: clearing capture_sources");
        self.capture_sources.clear();
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
        log_critical_operation("reinitialize_for_display_change: calling setup_capture()");
        self.setup_capture(&target_apps)
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
        self.scene.is_some() && !self.capture_sources.is_empty()
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
