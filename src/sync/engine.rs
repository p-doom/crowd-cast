//! Synchronization engine
//!
//! Coordinates input capture with recording state and filters input
//! based on the frontmost application. Manages libobs recording with
//! HEVC hardware encoding (VideoToolbox on macOS) when available.

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::capture::{get_frontmost_app, CaptureContext, DisplayMonitor, RecordingSession};
use crate::config::Config;
use crate::data::{CompletedChunk, InputEvent, InputEventBuffer};
use crate::input::{create_input_backend, InputBackend};

use super::{EngineCommand, EngineStatus};

/// The synchronization engine coordinates recording and input capture
pub struct SyncEngine {
    /// Configuration
    config: Config,
    /// Capture context for libobs operations
    capture_ctx: CaptureContext,
    /// Input backend
    input_backend: Box<dyn InputBackend>,
    /// Command receiver
    cmd_rx: mpsc::Receiver<EngineCommand>,
    /// Status broadcaster
    status_tx: broadcast::Sender<EngineStatus>,
    /// Input event buffer
    event_buffer: InputEventBuffer,
    /// Whether input capture is currently enabled
    capture_enabled: bool,
    /// Last known frontmost app
    last_frontmost_app: Option<String>,
    /// Current recording session
    current_session: Option<RecordingSession>,
    /// OBS timestamp at recording start (nanoseconds)
    recording_start_ns: Option<u64>,
    /// Output directory for chunks
    output_dir: PathBuf,
    /// Display monitor for detecting display hotplug events (macOS)
    display_monitor: DisplayMonitor,
}

impl SyncEngine {
    /// Create a new sync engine
    pub fn new(
        config: Config,
        capture_ctx: CaptureContext,
        cmd_rx: mpsc::Receiver<EngineCommand>,
        status_tx: broadcast::Sender<EngineStatus>,
    ) -> Self {
        let output_dir = config.recording.output_directory
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("crowd-cast-recordings"));
        
        Self {
            config,
            capture_ctx,
            input_backend: create_input_backend(),
            cmd_rx,
            status_tx,
            event_buffer: InputEventBuffer::new(),
            capture_enabled: false,
            last_frontmost_app: None,
            current_session: None,
            recording_start_ns: None,
            output_dir,
            display_monitor: DisplayMonitor::new(),
        }
    }

    /// Run the engine main loop
    pub async fn run(&mut self) -> Result<()> {
        let session_id = self.config.session_id();
        info!("Sync engine starting for session: {}", session_id);

        // Ensure output directory exists
        std::fs::create_dir_all(&self.output_dir)?;

        // Start input capture (events go to a channel)
        let (input_tx, mut input_rx) = mpsc::unbounded_channel();
        self.input_backend.start(input_tx)?;

        // Main polling interval
        let poll_interval = Duration::from_millis(self.config.capture.poll_interval_ms);
        let mut poll_timer = tokio::time::interval(poll_interval);

        // Broadcast initial status
        let _ = self.status_tx.send(EngineStatus::Idle);

        if self.config.recording.autostart_on_launch {
            info!("Autostart recording on launch enabled");
            if let Err(e) = self.start_recording().await {
                error!("Failed to autostart recording: {}", e);
                let _ = self
                    .status_tx
                    .send(EngineStatus::Error("Autostart recording failed".to_string()));
            }
        }

        loop {
            tokio::select! {
                // Handle commands
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        EngineCommand::StartRecording => {
                            self.start_recording().await?;
                        }
                        EngineCommand::StopRecording => {
                            self.stop_recording().await?;
                        }
                        EngineCommand::SetCaptureEnabled(enabled) => {
                            info!("Manual capture override: {}", enabled);
                            self.capture_enabled = enabled;
                        }
                        EngineCommand::Shutdown => {
                            info!("Shutdown command received");
                            self.stop_recording().await?;
                            break;
                        }
                    }
                }

                // Handle input events
                Some(event) = input_rx.recv() => {
                    self.handle_input_event(event).await;
                }

                // Poll frontmost app and check for display changes
                _ = poll_timer.tick() => {
                    self.poll_frontmost_app().await;
                    self.check_display_changes();
                }
            }
        }

        info!("Sync engine stopped");
        Ok(())
    }

    /// Start recording
    async fn start_recording(&mut self) -> Result<()> {
        if self.current_session.is_some() {
            warn!("Recording already in progress");
            return Ok(());
        }

        info!("Starting recording...");

        // Ensure capture sources are set up
        if !self.capture_ctx.is_capture_setup() {
            self.capture_ctx.setup_capture(&self.config.capture.target_apps)?;
        }

        // Generate a session ID
        let session_id = uuid::Uuid::new_v4().to_string();

        // Start libobs recording with HEVC hardware encoding
        let session = self.capture_ctx.start_recording(session_id)?;

        info!(
            "Recording started: session={}, output={:?}",
            session.session_id, session.output_path
        );

        // Store the OBS timestamp for event synchronization
        self.recording_start_ns = Some(session.start_time_ns);
        self.current_session = Some(session);
        self.event_buffer.clear();

        let _ = self.status_tx.send(EngineStatus::Capturing { event_count: 0 });

        Ok(())
    }

    /// Stop recording
    async fn stop_recording(&mut self) -> Result<()> {
        if self.current_session.is_none() {
            debug!("No recording in progress");
            return Ok(());
        }

        info!("Stopping recording...");

        // Save any buffered events with final video path
        let video_path = self.current_session.as_ref().map(|s| s.output_path.clone());
        if !self.event_buffer.is_empty() {
            self.flush_event_buffer_with_video(video_path).await?;
        }

        // Stop libobs recording - use block_in_place because libobs-wrapper
        // uses blocking_recv() internally which panics in async context
        let session = tokio::task::block_in_place(|| self.capture_ctx.stop_recording())?;
        if let Some(session) = session {
            info!(
                "Recording stopped: session={}, output={:?}",
                session.session_id, session.output_path
            );
        }

        self.current_session = None;
        self.recording_start_ns = None;
        let _ = self.status_tx.send(EngineStatus::Idle);

        Ok(())
    }

    /// Check for display configuration changes and recover if needed
    ///
    /// On macOS, when displays are disconnected and reconnected, ScreenCaptureKit
    /// caches stale display IDs. This method detects such changes and recreates
    /// the capture sources to get fresh display enumeration.
    fn check_display_changes(&mut self) {
        // Check if display configuration changed (macOS only, no-op on other platforms)
        if self.display_monitor.check_for_changes() {
            info!("Display configuration change detected, recreating capture sources...");

            // Recreate sources with fresh display enumeration
            // This runs synchronously because libobs operations must be on the OBS thread
            match self.capture_ctx.recreate_sources() {
                Ok(count) => {
                    info!(
                        "Successfully recovered {} capture source(s) after display change",
                        count
                    );
                }
                Err(e) => {
                    error!("Failed to recover capture sources after display change: {}", e);
                    // The capture may be broken, but we don't stop recording
                    // User might reconnect the display again
                }
            }
        }
    }

    /// Poll the frontmost application and update capture state
    async fn poll_frontmost_app(&mut self) {
        let frontmost = get_frontmost_app();

        let bundle_id = frontmost.as_ref().map(|a| a.bundle_id.as_str());
        let should_capture = match bundle_id {
            Some(id) => self.config.should_capture_app(id),
            None => {
                // Can't detect frontmost app (e.g., Wayland)
                // Fall back to capture_all setting
                self.config.capture.capture_all
            }
        };

        // Log state changes
        let new_bundle_id = bundle_id.map(|s| s.to_string());
        if new_bundle_id != self.last_frontmost_app {
            if let Some(ref id) = new_bundle_id {
                debug!(
                    "Frontmost app changed: {} (capture: {})",
                    id, should_capture
                );
            }
            self.last_frontmost_app = new_bundle_id;
        }

        // Update capture state (only capture if recording AND app is allowed)
        let is_recording = self.current_session.is_some();
        let was_capturing = self.capture_enabled;
        self.capture_enabled = should_capture && is_recording;

        if self.capture_enabled != was_capturing {
            if self.capture_enabled {
                debug!("Input capture enabled");
            } else {
                debug!("Input capture disabled");
            }
        }

        // Update status
        if is_recording {
            if self.capture_enabled {
                let _ = self.status_tx.send(EngineStatus::Capturing {
                    event_count: self.event_buffer.len(),
                });
            } else {
                let _ = self.status_tx.send(EngineStatus::RecordingBlocked);
            }
        }
    }

    /// Handle an input event
    async fn handle_input_event(&mut self, event: InputEvent) {
        // Only buffer events if capture is enabled
        if !self.capture_enabled {
            return;
        }

        // Adjust timestamp relative to OBS recording start for video sync
        // Convert from system microseconds to OBS-relative microseconds
        let adjusted_event = if let Some(start_ns) = self.recording_start_ns {
            // Get current OBS timestamp and compute relative offset
            let current_ns = self.capture_ctx.get_video_frame_time().unwrap_or(0);
            let elapsed_us = current_ns.saturating_sub(start_ns) / 1000;

            InputEvent {
                timestamp_us: elapsed_us,
                ..event
            }
        } else {
            event
        };

        self.event_buffer.push(adjusted_event);

        // Check if buffer should be flushed (e.g., every N events or time interval)
        if self.event_buffer.len() >= 10000 {
            if let Err(e) = self.flush_event_buffer().await {
                error!("Failed to flush event buffer: {}", e);
            }
        }
    }

    /// Flush the event buffer to disk
    async fn flush_event_buffer(&mut self) -> Result<()> {
        self.flush_event_buffer_with_video(None).await
    }

    /// Flush the event buffer to disk with optional video path
    async fn flush_event_buffer_with_video(&mut self, video_path: Option<PathBuf>) -> Result<()> {
        if self.event_buffer.is_empty() {
            return Ok(());
        }

        // Use session_id for both video and input files so they match
        let session_id = self
            .current_session
            .as_ref()
            .map(|s| s.session_id.clone())
            .unwrap_or_else(|| self.config.session_id());

        let events = self.event_buffer.drain();

        // Compute start/end times from events
        let start_time_us = events.first().map(|e| e.timestamp_us).unwrap_or(0);
        let end_time_us = events.last().map(|e| e.timestamp_us).unwrap_or(0);

        let chunk = CompletedChunk {
            chunk_id: session_id.clone(),
            session_id: session_id.clone(),
            events,
            video_path,
            start_time_us,
            end_time_us,
        };

        // Save to disk with same session_id as video (recording_{session_id}.mp4 -> input_{session_id}.msgpack)
        let path = self.output_dir.join(format!("input_{}.msgpack", session_id));
        let bytes = rmp_serde::to_vec(&chunk.events)?;
        tokio::fs::write(&path, bytes).await?;

        info!("Flushed {} events to {:?}", chunk.events.len(), path);

        // Broadcast upload status
        let _ = self.status_tx.send(EngineStatus::Uploading { chunk_id: session_id });

        Ok(())
    }
}

/// Create command and status channels for the engine
pub fn create_engine_channels() -> (
    mpsc::Sender<EngineCommand>,
    mpsc::Receiver<EngineCommand>,
    broadcast::Sender<EngineStatus>,
    broadcast::Receiver<EngineStatus>,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let (status_tx, status_rx) = broadcast::channel(16);
    (cmd_tx, cmd_rx, status_tx, status_rx)
}
