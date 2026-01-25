//! Sync engine implementation
//!
//! Coordinates input capture with OBS recording/streaming state.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant as StdInstant};
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::time::Instant as TokioInstant;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::data::{CompletedChunk, InputChunk, InputEvent};
use crate::input::{create_input_backend, InputBackend};
use crate::obs::{OBSController, OBSEvent, OBSManager, RecordingState, StreamingState};
use crate::installer::is_obs_running;
use crate::upload::Uploader;

/// Commands that can be sent to the sync engine
#[derive(Debug, Clone)]
pub enum EngineCommand {
    /// Manually start OBS recording
    StartRecording,
    /// Manually stop OBS recording
    StopRecording,
    /// Set capture enabled state (for Wayland manual toggle fallback)
    SetCaptureEnabled(bool),
    /// Shutdown the engine
    Shutdown,
}

/// Status updates from the sync engine
#[derive(Debug, Clone)]
pub enum EngineStatus {
    /// Engine is idle (not capturing)
    Idle,
    /// Engine is capturing input
    Capturing {
        /// Number of events captured in current chunk
        event_count: usize,
    },
    /// Recording or streaming is active, but no hooked sources are available
    RecordingBlocked,
    /// OBS is unavailable, reconnecting
    WaitingForOBS,
    /// Engine is uploading a chunk
    Uploading {
        /// Chunk ID being uploaded
        chunk_id: String,
    },
    /// An error occurred
    Error(String),
}

/// State shared between sync engine components
struct SharedState {
    /// Whether input capture is currently enabled
    capture_enabled: AtomicBool,
    
    /// Current input chunk being built
    current_chunk: RwLock<Option<InputChunk>>,
    
    /// Session ID
    session_id: String,
    
    /// Current chunk ID counter
    chunk_counter: RwLock<u32>,
    
    /// Event counter for status reporting
    event_count: AtomicUsize,
    
    /// Whether OBS is connected
    obs_connected: AtomicBool,

    /// Last screenshot hash used for stale-frame detection
    last_screenshot_hash: RwLock<Option<u64>>,

    /// Count of consecutive identical screenshots
    stale_screenshot_count: AtomicUsize,
}

/// The sync engine coordinates input capture with OBS state
pub struct SyncEngine {
    config: Config,
    obs: OBSController,
    obs_manager: OBSManager,
    input_backend: Box<dyn InputBackend>,
    uploader: Uploader,
    state: Arc<SharedState>,
    cmd_rx: mpsc::Receiver<EngineCommand>,
    status_tx: broadcast::Sender<EngineStatus>,
}

impl SyncEngine {
    /// Create a new sync engine with command/status channels
    pub async fn new(
        config: Config,
        obs: OBSController,
        obs_manager: OBSManager,
        cmd_rx: mpsc::Receiver<EngineCommand>,
        status_tx: broadcast::Sender<EngineStatus>,
    ) -> Result<Self> {
        let session_id = config.session_id();
        let current_scene = obs.current_scene().await.unwrap_or_default();
        
        let state = Arc::new(SharedState {
            capture_enabled: AtomicBool::new(false),
            current_chunk: RwLock::new(Some(InputChunk::new(
                session_id.clone(),
                "0".to_string(),
                current_scene,
            ))),
            session_id,
            chunk_counter: RwLock::new(0),
            event_count: AtomicUsize::new(0),
            obs_connected: AtomicBool::new(true),
            last_screenshot_hash: RwLock::new(None),
            stale_screenshot_count: AtomicUsize::new(0),
        });
        
        let input_backend = create_input_backend();
        let uploader = Uploader::new(&config);
        
        Ok(Self {
            config,
            obs,
            obs_manager,
            input_backend,
            uploader,
            state,
            cmd_rx,
            status_tx,
        })
    }
    
    /// Broadcast a status update
    fn send_status(&self, status: EngineStatus) {
        let _ = self.status_tx.send(status);
    }

    /// Broadcast capture status based on OBS state and capture_enabled
    async fn send_capture_status(&self) {
        if !self.state.obs_connected.load(Ordering::SeqCst) {
            self.send_status(EngineStatus::WaitingForOBS);
            return;
        }

        let capture_enabled = self.state.capture_enabled.load(Ordering::SeqCst);
        let obs_state = self.obs.get_state().await;
        let is_recording_or_streaming =
            matches!(obs_state.recording, RecordingState::Recording)
                || matches!(obs_state.streaming, StreamingState::Streaming);
        let any_hooked = obs_state
            .hooked_sources
            .as_ref()
            .map(|h| h.any_hooked)
            .unwrap_or(true);

        if capture_enabled {
            self.send_status(EngineStatus::Capturing {
                event_count: self.state.event_count.load(Ordering::SeqCst),
            });
        } else if is_recording_or_streaming && !any_hooked {
            self.send_status(EngineStatus::RecordingBlocked);
        } else {
            self.send_status(EngineStatus::Idle);
        }
    }
    
    /// Run the sync engine main loop
    pub async fn run(mut self) -> Result<()> {
        info!("Sync engine starting for session: {}", self.state.session_id);
        self.send_capture_status().await;
        
        // Create channel for input events
        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<InputEvent>();
        
        // Start input capture backend
        self.input_backend.start(input_tx)?;
        
        // Subscribe to OBS events via channel
        let mut obs_events = match self.obs.subscribe_events().await {
            Ok(rx) => Some(rx),
            Err(e) => {
                warn!("Failed to subscribe to OBS events: {}. Falling back to polling only.", e);
                None
            }
        };
        
        // Spawn task to handle incoming input events
        let state = self.state.clone();
        let config = self.config.clone();
        let _input_handler = tokio::spawn(async move {
            while let Some(event) = input_rx.recv().await {
                // Only record if capture is enabled
                if state.capture_enabled.load(Ordering::SeqCst) {
                    let mut chunk = state.current_chunk.write().await;
                    if let Some(ref mut c) = *chunk {
                        // Filter based on config
                        let should_record = match &event.event {
                            crate::data::EventType::KeyPress(_) | 
                            crate::data::EventType::KeyRelease(_) => config.input.capture_keyboard,
                            crate::data::EventType::MouseMove(_) => config.input.capture_mouse_move,
                            crate::data::EventType::MousePress(_) | 
                            crate::data::EventType::MouseRelease(_) => config.input.capture_mouse_click,
                            crate::data::EventType::MouseScroll(_) => config.input.capture_mouse_scroll,
                        };
                        
                        if should_record {
                            c.add_event(event);
                            state.event_count.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            }
        });
        
        // Main event loop using tokio::select!
        let poll_interval = Duration::from_millis(self.config.obs.poll_interval_ms);
        let sanity_interval = Duration::from_secs(self.config.obs.sanity_check_interval_secs);
        let mut last_sanity_check = StdInstant::now();
        let mut poll_timer = tokio::time::interval(poll_interval);
        let mut reconnect_backoff = Duration::from_secs(1);
        let max_reconnect_backoff = Duration::from_secs(10);
        let mut next_reconnect_at = TokioInstant::now();
        
        loop {
            tokio::select! {
                // Handle OBS events (recording started/stopped, etc.)
                Some(obs_event) = async {
                    match &mut obs_events {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending::<Option<_>>().await,
                    }
                } => {
                    let connected = self.handle_obs_event(obs_event).await;
                    if !connected {
                        obs_events = None;
                        reconnect_backoff = Duration::from_secs(1);
                        next_reconnect_at = TokioInstant::now() + reconnect_backoff;
                    }
                }
                
                // Handle commands from tray UI
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        EngineCommand::StartRecording => {
                            info!("Manual recording start requested");
                            if let Err(e) = self.obs.start_recording().await {
                                if self.handle_obs_disconnect(e).await {
                                    reconnect_backoff = Duration::from_secs(1);
                                    next_reconnect_at = TokioInstant::now() + reconnect_backoff;
                                }
                                obs_events = None;
                            }
                        }
                        EngineCommand::StopRecording => {
                            info!("Manual recording stop requested");
                            if let Err(e) = self.obs.stop_recording().await {
                                if self.handle_obs_disconnect(e).await {
                                    reconnect_backoff = Duration::from_secs(1);
                                    next_reconnect_at = TokioInstant::now() + reconnect_backoff;
                                }
                                obs_events = None;
                            }
                        }
                        EngineCommand::SetCaptureEnabled(enabled) => {
                            info!("Manual capture enabled set to: {}", enabled);
                            if let Err(e) = self.obs.set_capture_enabled(enabled).await {
                                warn!("Failed to set capture enabled: {}", e);
                            }
                        }
                        EngineCommand::Shutdown => {
                            info!("Shutdown requested");
                            break;
                        }
                    }
                }
                
                // Periodic state refresh (fallback for when events aren't working)
                _ = poll_timer.tick() => {
                    if let Err(e) = self.obs.refresh_state().await {
                        if self.handle_obs_disconnect(e).await {
                            reconnect_backoff = Duration::from_secs(1);
                            next_reconnect_at = TokioInstant::now() + reconnect_backoff;
                        }
                        obs_events = None;
                    } else {
                        if self.state.obs_connected.swap(true, Ordering::SeqCst) == false {
                            info!("OBS connection restored");
                        }
                        self.reconcile_capture_state().await;
                    }
                    
                    // Periodic sanity check
                    if last_sanity_check.elapsed() >= sanity_interval {
                        last_sanity_check = StdInstant::now();
                        self.run_sanity_check().await;
                    }
                }
                
                // Reconnect when OBS is unavailable
                _ = async {
                    if !self.state.obs_connected.load(Ordering::SeqCst) {
                        tokio::time::sleep_until(next_reconnect_at).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    info!("Attempting to reconnect to OBS...");
                    if !is_obs_running() {
                        info!("OBS is not running; attempting to relaunch...");
                        match self.obs_manager.launch_hidden() {
                            Ok(()) => {
                                tokio::time::sleep(Duration::from_secs(3)).await;
                            }
                            Err(e) => {
                                warn!("Failed to relaunch OBS: {}", e);
                            }
                        }
                    }
                    match self.obs.reconnect().await {
                        Ok(()) => {
                            self.state.obs_connected.store(true, Ordering::SeqCst);
                            reconnect_backoff = Duration::from_secs(1);
                            next_reconnect_at = TokioInstant::now() + reconnect_backoff;
                            obs_events = match self.obs.subscribe_events().await {
                                Ok(rx) => Some(rx),
                                Err(e) => {
                                    warn!("Failed to resubscribe to OBS events: {}", e);
                                    None
                                }
                            };
                            self.reconcile_capture_state().await;
                        }
                        Err(e) => {
                            warn!("OBS reconnect failed: {}", e);
                            self.send_status(EngineStatus::WaitingForOBS);
                            reconnect_backoff = (reconnect_backoff * 2).min(max_reconnect_backoff);
                            next_reconnect_at = TokioInstant::now() + reconnect_backoff;
                        }
                    }
                }
            }
        }
        
        info!("Sync engine shutting down");
        Ok(())
    }
    
    /// Handle an OBS event
    async fn handle_obs_event(&mut self, event: OBSEvent) -> bool {
        match event {
            OBSEvent::RecordingStarted => {
                info!("OBS recording started");
                if let Err(e) = self.obs.refresh_state().await {
                    self.handle_obs_disconnect(e).await;
                    return false;
                } else {
                    self.reconcile_capture_state().await;
                }
            }
            OBSEvent::RecordingStopped { path } => {
                info!("OBS recording stopped, output: {:?}", path);
                if let Err(e) = self.obs.refresh_state().await {
                    self.handle_obs_disconnect(e).await;
                    return false;
                } else {
                    self.reconcile_capture_state().await;
                }
                
                // Finalize chunk and upload
                if let Some(video_path) = path {
                    self.finalize_and_upload(Some(video_path)).await;
                }
            }
            OBSEvent::StreamingStarted => {
                info!("OBS streaming started");
                if let Err(e) = self.obs.refresh_state().await {
                    self.handle_obs_disconnect(e).await;
                    return false;
                } else {
                    self.reconcile_capture_state().await;
                }
            }
            OBSEvent::StreamingStopped => {
                info!("OBS streaming stopped");
                if let Err(e) = self.obs.refresh_state().await {
                    self.handle_obs_disconnect(e).await;
                    return false;
                } else {
                    self.reconcile_capture_state().await;
                }
            }
            OBSEvent::HookedSourcesChanged { any_hooked } => {
                debug!("Hooked sources changed (any_hooked={})", any_hooked);
                self.reconcile_capture_state().await;
            }
        }

        true
    }
    
    /// Start input capture
    async fn start_capture(&self) {
        // Early return if already capturing
        if self.state.capture_enabled.load(Ordering::SeqCst) {
            return;
        }

        // Set the recording start timestamp BEFORE enabling capture to avoid race condition.
        // If we enable capture first, events can arrive with timestamps before start_time_us
        // is set, causing timeline sync issues.
        // Only set on first start (when start_time_us is 0), not on resume after pause.
        if let Some(timestamp_us) = self.input_backend.current_timestamp() {
            let mut chunk = self.state.current_chunk.write().await;
            if let Some(ref mut c) = *chunk {
                if c.start_time_us == 0 {
                    c.set_recording_start(timestamp_us);
                    info!("Recording start timestamp set to {} us", timestamp_us);
                } else {
                    debug!("Resuming capture, keeping existing start_time_us = {} us", c.start_time_us);
                }
            }
        } else {
            warn!("Input backend not started, cannot set recording start timestamp");
        }

        // Now enable capture - events added after this point will have timestamps >= start_time_us
        if !self.state.capture_enabled.swap(true, Ordering::SeqCst) {
            info!("Input capture enabled");
        }
    }
    
    /// Stop input capture
    async fn stop_capture(&self) {
        if self.state.capture_enabled.swap(false, Ordering::SeqCst) {
            info!("Input capture disabled");

            self.state.stale_screenshot_count.store(0, Ordering::SeqCst);
            *self.state.last_screenshot_hash.write().await = None;
            
            // Increment pause count
            let mut chunk = self.state.current_chunk.write().await;
            if let Some(ref mut c) = *chunk {
                c.metadata.pause_count += 1;
            }
            
        }
    }

    /// Reconcile capture state with OBS should_capture (recording/streaming + hooked windows)
    async fn reconcile_capture_state(&self) {
        let should_capture = self.obs.should_capture().await;
        let is_capturing = self.state.capture_enabled.load(Ordering::SeqCst);

        if should_capture && !is_capturing {
            self.start_capture().await;
        } else if !should_capture && is_capturing {
            self.stop_capture().await;
        }

        self.send_capture_status().await;
    }
    
    /// Finalize current chunk and upload
    async fn finalize_and_upload(&mut self, video_path: Option<PathBuf>) {
        match self.finalize_chunk(video_path).await {
            Ok(Some(chunk)) => {
                let chunk_id = chunk.chunk_id.clone();
                self.send_status(EngineStatus::Uploading { chunk_id: chunk_id.clone() });
                
                info!("Uploading chunk {}", chunk_id);
                
                if let Err(e) = self.uploader.upload(&chunk).await {
                    error!("Failed to upload chunk {}: {}", chunk_id, e);
                    self.send_status(EngineStatus::Error(format!("Upload failed: {}", e)));
                } else {
                    info!("Successfully uploaded chunk {}", chunk_id);
                }

                self.send_capture_status().await;
            }
            Ok(None) => {
                debug!("No chunk to upload (no events recorded)");
            }
            Err(e) => {
                error!("Failed to finalize chunk: {}", e);
                self.send_status(EngineStatus::Error(format!("Finalize failed: {}", e)));
            }
        }
    }
    
    /// Finalize the current chunk and prepare for upload
    async fn finalize_chunk(&self, video_path: Option<PathBuf>) -> Result<Option<CompletedChunk>> {
        let mut chunk_guard = self.state.current_chunk.write().await;
        
        if let Some(chunk) = chunk_guard.take() {
            let event_count = chunk.events.len();
            info!("Finalizing chunk {} with {} events", chunk.chunk_id, event_count);
            
            // Reset event counter
            self.state.event_count.store(0, Ordering::SeqCst);
            
            if event_count == 0 {
                // No events, but still create new chunk for next recording
                let mut counter = self.state.chunk_counter.write().await;
                *counter += 1;
                let new_chunk_id = counter.to_string();
                
                let current_scene = self.obs.current_scene().await.unwrap_or_default();
                *chunk_guard = Some(InputChunk::new(
                    self.state.session_id.clone(),
                    new_chunk_id,
                    current_scene,
                ));
                
                return Ok(None);
            }
            
            // Create new chunk for next recording segment
            let mut counter = self.state.chunk_counter.write().await;
            *counter += 1;
            let new_chunk_id = counter.to_string();
            
            let current_scene = self.obs.current_scene().await.unwrap_or_default();
            *chunk_guard = Some(InputChunk::new(
                self.state.session_id.clone(),
                new_chunk_id,
                current_scene,
            ));
            
            if let Some(video_path) = video_path {
                self.save_input_chunk(&video_path, &chunk).await?;
                return Ok(Some(CompletedChunk {
                    session_id: chunk.session_id.clone(),
                    chunk_id: chunk.chunk_id.clone(),
                    video_path,
                    input_chunk: chunk,
                }));
            }
        }
        
        Ok(None)
    }

    async fn save_input_chunk(&self, video_path: &PathBuf, chunk: &InputChunk) -> Result<PathBuf> {
        let input_path = video_path.with_extension("msgpack");
        let input_bytes = chunk
            .to_msgpack()
            .context("Failed to serialize input chunk for local save")?;

        tokio::fs::write(&input_path, input_bytes)
            .await
            .with_context(|| format!("Failed to write input chunk to {:?}", input_path))?;

        info!("Saved input chunk to {:?}", input_path);
        Ok(input_path)
    }

    async fn handle_obs_disconnect(&self, error: anyhow::Error) -> bool {
        let was_connected = self.state.obs_connected.swap(false, Ordering::SeqCst);
        if was_connected {
            warn!("OBS connection lost: {}", error);
            self.stop_capture().await;
        } else {
            debug!("OBS still unavailable: {}", error);
        }

        self.send_status(EngineStatus::WaitingForOBS);
        was_connected
    }
    
    /// Run periodic sanity check
    async fn run_sanity_check(&self) {
        if self.state.capture_enabled.load(Ordering::SeqCst) {
            const STALE_SCREENSHOT_THRESHOLD: usize = 2;

            let screenshot_hash = match self.obs.screenshot_luma_hash().await {
                Ok(hash) => hash,
                Err(e) => {
                    debug!("Sanity check failed: {}", e);
                    return;
                }
            };

            let mut last_hash = self.state.last_screenshot_hash.write().await;
            if let Some(previous_hash) = *last_hash {
                if previous_hash == screenshot_hash {
                    let count = self
                        .state
                        .stale_screenshot_count
                        .fetch_add(1, Ordering::SeqCst)
                        + 1;
                    if count == STALE_SCREENSHOT_THRESHOLD {
                        warn!(
                            "Sanity check: OBS output appears frozen ({} identical frames)",
                            count
                        );
                    }
                    return;
                }
            }

            *last_hash = Some(screenshot_hash);
            self.state
                .stale_screenshot_count
                .store(0, Ordering::SeqCst);
        }
    }
}
