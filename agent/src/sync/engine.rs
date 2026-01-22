//! Sync engine implementation
//!
//! Coordinates input capture with OBS recording/streaming state.

use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::data::{CompletedChunk, InputChunk, InputEvent};
use crate::input::{create_input_backend, InputBackend};
use crate::obs::{OBSController, OBSEvent};
use crate::upload::Uploader;

/// Commands that can be sent to the sync engine
#[derive(Debug, Clone)]
pub enum EngineCommand {
    /// Manually start input capture
    StartCapture,
    /// Manually stop input capture
    StopCapture,
    /// Request current status
    GetStatus,
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
        /// Current session ID
        session_id: String,
    },
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
}

/// The sync engine coordinates input capture with OBS state
pub struct SyncEngine {
    config: Config,
    obs: OBSController,
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
        });
        
        let input_backend = create_input_backend();
        let uploader = Uploader::new(&config);
        
        Ok(Self {
            config,
            obs,
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
    
    /// Run the sync engine main loop
    pub async fn run(mut self) -> Result<()> {
        info!("Sync engine starting for session: {}", self.state.session_id);
        self.send_status(EngineStatus::Idle);
        
        // Create channel for input events
        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<InputEvent>();
        
        // Start input capture backend
        self.input_backend.start(input_tx)?;
        
        // Subscribe to OBS events via channel
        let mut obs_events = match self.obs.subscribe_events() {
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
        let mut last_sanity_check = Instant::now();
        let mut poll_timer = tokio::time::interval(poll_interval);
        
        loop {
            tokio::select! {
                // Handle OBS events (recording started/stopped, etc.)
                Some(obs_event) = async {
                    match &mut obs_events {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.handle_obs_event(obs_event).await;
                }
                
                // Handle commands from tray UI
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        EngineCommand::StartCapture => {
                            info!("Manual capture start requested");
                            self.start_capture().await;
                        }
                        EngineCommand::StopCapture => {
                            info!("Manual capture stop requested");
                            self.stop_capture().await;
                        }
                        EngineCommand::GetStatus => {
                            self.send_current_status().await;
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
                        debug!("Failed to refresh OBS state: {}", e);
                    }
                    
                    // Periodic sanity check
                    if last_sanity_check.elapsed() >= sanity_interval {
                        last_sanity_check = Instant::now();
                        self.run_sanity_check().await;
                    }
                }
            }
        }
        
        info!("Sync engine shutting down");
        Ok(())
    }
    
    /// Handle an OBS event
    async fn handle_obs_event(&mut self, event: OBSEvent) {
        match event {
            OBSEvent::RecordingStarted => {
                info!("OBS recording started");
                self.start_capture().await;
            }
            OBSEvent::RecordingStopped { path } => {
                info!("OBS recording stopped, output: {:?}", path);
                self.stop_capture().await;
                
                // Finalize chunk and upload
                if let Some(video_path) = path {
                    self.finalize_and_upload(Some(video_path)).await;
                }
            }
            OBSEvent::StreamingStarted => {
                info!("OBS streaming started");
                self.start_capture().await;
            }
            OBSEvent::StreamingStopped => {
                info!("OBS streaming stopped");
                self.stop_capture().await;
            }
        }
    }
    
    /// Start input capture
    async fn start_capture(&self) {
        if !self.state.capture_enabled.swap(true, Ordering::SeqCst) {
            info!("Input capture enabled");
            self.send_status(EngineStatus::Capturing {
                event_count: self.state.event_count.load(Ordering::SeqCst),
                session_id: self.state.session_id.clone(),
            });
        }
    }
    
    /// Stop input capture
    async fn stop_capture(&self) {
        if self.state.capture_enabled.swap(false, Ordering::SeqCst) {
            info!("Input capture disabled");
            
            // Increment pause count
            let mut chunk = self.state.current_chunk.write().await;
            if let Some(ref mut c) = *chunk {
                c.metadata.pause_count += 1;
            }
            
            self.send_status(EngineStatus::Idle);
        }
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
                
                self.send_status(EngineStatus::Idle);
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
    
    /// Send current status
    async fn send_current_status(&self) {
        let is_capturing = self.state.capture_enabled.load(Ordering::SeqCst);
        
        if is_capturing {
            self.send_status(EngineStatus::Capturing {
                event_count: self.state.event_count.load(Ordering::SeqCst),
                session_id: self.state.session_id.clone(),
            });
        } else {
            self.send_status(EngineStatus::Idle);
        }
    }
    
    /// Run periodic sanity check
    async fn run_sanity_check(&self) {
        if self.state.capture_enabled.load(Ordering::SeqCst) {
            match self.obs.is_black_screen().await {
                Ok(is_black) => {
                    if is_black {
                        warn!("Sanity check: OBS output appears to be black screen");
                    }
                }
                Err(e) => {
                    debug!("Sanity check failed: {}", e);
                }
            }
        }
    }
}
