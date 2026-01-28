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
use libobs_wrapper::utils::StartupInfo;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::{convert::Infallible};
use tracing::{debug, info, warn};

use super::recording::{calculate_output_dimensions, RecordingConfig, RecordingOutput};
use super::sources::{get_main_display_uuid, ScreenCaptureSource};
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
        let options = ObsBootstrapperOptions::default();
        let notify_download = is_running_in_app_bundle();

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

        // Build default video info to get native display dimensions
        let default_video_info = ObsVideoInfoBuilder::new().build();
        let base_width = default_video_info.get_base_width();
        let base_height = default_video_info.get_base_height();

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
            .fps_num(self.recording_config.fps)
            .fps_den(1)
            .output_width(output_width)
            .output_height(output_height)
            .build();

        let startup_info = StartupInfo::default().set_video_info(video_info);
        let context =
            ObsContext::new(startup_info).context("Failed to create OBS context")?;

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

    /// Set up capture sources for display capture (legacy method)
    ///
    /// Creates the main scene and adds a screen capture source for the primary display.
    /// Must be called after `initialize()`.
    pub fn setup_display_capture(&mut self) -> Result<()> {
        self.setup_capture(&[])
    }

    /// Refresh capture sources after a display configuration change (in-place update)
    ///
    /// This is needed on macOS when displays are disconnected and reconnected,
    /// as ScreenCaptureKit caches display IDs that become stale.
    ///
    /// This method updates the display UUID on existing sources in-place,
    /// which is more efficient than destroying and recreating them.
    /// Returns the number of sources successfully refreshed.
    /// 
    /// NOTE: This may not work reliably when transitioning from clamshell mode
    /// because SCK may not properly reinitialize the stream. Use 
    /// `fully_recreate_sources()` for more reliable recovery.
    pub fn recreate_sources(&mut self) -> Result<usize> {
        if !self.is_initialized() {
            anyhow::bail!("OBS context not initialized");
        }

        // Get the new display UUID
        let display_uuid = get_main_display_uuid()
            .context("Failed to get main display UUID for refresh")?;

        info!("Refreshing capture sources with display UUID: {}", display_uuid);

        // Update each source's display UUID in-place
        let mut success_count = 0;
        for source in &mut self.capture_sources {
            match source.update_display_uuid(&display_uuid) {
                Ok(()) => {
                    success_count += 1;
                }
                Err(e) => {
                    warn!("Failed to refresh source '{}': {}", source.name(), e);
                }
            }
        }

        if success_count > 0 {
            info!("Refreshed {} capture source(s)", success_count);
        }

        Ok(success_count)
    }

    /// Fully destroy and recreate capture sources after a display configuration change
    ///
    /// Unlike `recreate_sources()` which updates settings in-place, this method
    /// completely destroys all existing capture sources and creates new ones.
    /// This forces ScreenCaptureKit to do a fresh initialization, which is more
    /// reliable when transitioning between displays (e.g., clamshell mode).
    ///
    /// Returns the number of sources successfully created.
    pub fn fully_recreate_sources(&mut self) -> Result<usize> {
        if !self.is_initialized() {
            anyhow::bail!("OBS context not initialized");
        }

        let target_apps = self.target_apps.clone();
        
        info!(
            "Fully recreating capture sources for {} target app(s)",
            target_apps.len()
        );

        // Drop all existing sources - this removes them from OBS
        let old_count = self.capture_sources.len();
        self.capture_sources.clear();
        debug!("Cleared {} existing capture source(s)", old_count);

        // Get the context and scene
        let context = self
            .context
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("OBS context not initialized"))?;

        // Get the existing scene (don't create a new one to avoid disrupting recording)
        let mut scene = match &mut self.scene {
            Some(s) => s.clone(),
            None => anyhow::bail!("Scene not initialized - call setup_capture first"),
        };

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

            debug!("Recreated display capture source (audio: {})", capture_audio);
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

        let new_count = self.capture_sources.len();
        info!("Fully recreated {} capture source(s)", new_count);

        // Update state
        if let Ok(mut state) = self.state.write() {
            state.any_source_active = !self.capture_sources.is_empty();
        }

        Ok(new_count)
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
            std::fs::create_dir_all(parent)
                .context("Failed to create output directory")?;
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
            state.should_capture =
                state.recording.is_recording && !state.recording.is_paused && state.any_source_active;
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
        // Stop any active recording first
        if self.recording.is_some() {
            info!("Stopping recording during shutdown...");
            if let Err(e) = self.stop_recording() {
                warn!("Error stopping recording during shutdown: {}", e);
            }
        }

        if self.context.is_some() {
            info!("Shutting down libobs context...");
        }
        // ObsContext handles cleanup in its own Drop implementation
    }
}
