//! OBS WebSocket controller implementation

use anyhow::{Context, Result};
use base64::Engine;
use futures::StreamExt;
use obws::events::{Event, OutputState};
use obws::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info};

use crate::config::Config;

/// OBS recording event for the sync engine
#[derive(Debug, Clone)]
pub enum OBSEvent {
    /// Recording started
    RecordingStarted,
    /// Recording stopped with output file path
    RecordingStopped { path: Option<PathBuf> },
    /// Streaming started  
    StreamingStarted,
    /// Streaming stopped
    StreamingStopped,
}

/// State of window capture sources from the CrowdCast OBS plugin
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookedSourcesResponse {
    /// List of tracked sources
    pub sources: Vec<SourceState>,
    
    /// Whether any source is currently hooked and active
    pub any_hooked: bool,
}

/// State of a single window capture source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceState {
    /// Source name
    pub name: String,
    
    /// Whether the source is hooked to a window
    pub hooked: bool,
    
    /// Whether the source is active (in the current scene)
    pub active: bool,
}

/// OBS recording state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    Stopped,
    Starting,
    Recording,
    Stopping,
    Paused,
}

/// OBS streaming state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingState {
    Stopped,
    Starting,
    Streaming,
    Stopping,
}

/// Combined capture state
#[derive(Debug, Clone)]
pub struct CaptureState {
    /// Whether we should be logging input
    pub should_capture: bool,
    
    /// Recording state
    pub recording: RecordingState,
    
    /// Streaming state
    pub streaming: StreamingState,
    
    /// Hooked sources info
    pub hooked_sources: Option<HookedSourcesResponse>,
    
    /// Current scene name
    pub current_scene: String,
}

impl Default for CaptureState {
    fn default() -> Self {
        Self {
            should_capture: false,
            recording: RecordingState::Stopped,
            streaming: StreamingState::Stopped,
            hooked_sources: None,
            current_scene: String::new(),
        }
    }
}

/// Controller for OBS WebSocket communication
pub struct OBSController {
    client: Client,
    state: Arc<RwLock<CaptureState>>,
    #[allow(dead_code)]
    config: Config,
}

impl OBSController {
    /// Create a new OBS controller and connect to OBS
    pub async fn new(config: &Config) -> Result<Self> {
        let client = Client::connect(
            &config.obs.host,
            config.obs.port,
            config.obs.password.as_deref(),
        )
        .await
        .context("Failed to connect to OBS WebSocket")?;

        let controller = Self {
            client,
            state: Arc::new(RwLock::new(CaptureState::default())),
            config: config.clone(),
        };

        // Get initial state
        controller.refresh_state().await?;

        Ok(controller)
    }

    /// Get the current capture state
    pub async fn get_state(&self) -> CaptureState {
        self.state.read().await.clone()
    }

    /// Check if we should be capturing input
    pub async fn should_capture(&self) -> bool {
        self.state.read().await.should_capture
    }

    /// Refresh the capture state from OBS
    pub async fn refresh_state(&self) -> Result<()> {
        let mut state = self.state.write().await;

        // Get recording state
        let record_status = self.client.recording().status().await?;
        state.recording = if record_status.paused {
            RecordingState::Paused
        } else if record_status.active {
            RecordingState::Recording
        } else {
            RecordingState::Stopped
        };

        // Get streaming state
        let stream_status = self.client.streaming().status().await?;
        state.streaming = if stream_status.active {
            StreamingState::Streaming
        } else {
            StreamingState::Stopped
        };

        // Get current scene
        let scene = self.client.scenes().current_program_scene().await?;
        state.current_scene = scene.id.name.clone();

        // Get hooked sources from our plugin (via vendor request)
        state.hooked_sources = self.get_hooked_sources().await.ok();

        // Determine if we should capture
        let is_recording_or_streaming = matches!(state.recording, RecordingState::Recording)
            || matches!(state.streaming, StreamingState::Streaming);

        let any_hooked = state
            .hooked_sources
            .as_ref()
            .map(|h| h.any_hooked)
            .unwrap_or(true); // Default to true if plugin not available

        state.should_capture = is_recording_or_streaming && any_hooked;

        debug!(
            "OBS state: recording={:?}, streaming={:?}, any_hooked={}, should_capture={}",
            state.recording, state.streaming, any_hooked, state.should_capture
        );

        Ok(())
    }

    /// Query the CrowdCast plugin for hooked sources state
    async fn get_hooked_sources(&self) -> Result<HookedSourcesResponse> {
        // Use vendor request to query our plugin
        // obws supports vendor requests via call_vendor_request
        let empty_data = serde_json::json!({});
        let response = self
            .client
            .general()
            .call_vendor_request(obws::requests::general::CallVendorRequest {
                vendor_name: "crowdcast",
                request_type: "GetHookedSources",
                request_data: &empty_data,
            })
            .await
            .context("Failed to call crowdcast.GetHookedSources vendor request")?;

        let hooked_response: HookedSourcesResponse = serde_json::from_value(response.response_data)
            .context("Failed to parse hooked sources response")?;

        Ok(hooked_response)
    }

    /// Start recording
    pub async fn start_recording(&self) -> Result<()> {
        self.client.recording().start().await?;
        info!("Started OBS recording");
        Ok(())
    }

    /// Stop recording
    pub async fn stop_recording(&self) -> Result<()> {
        self.client.recording().stop().await?;
        info!("Stopped OBS recording");
        Ok(())
    }

    /// Start streaming
    pub async fn start_streaming(&self) -> Result<()> {
        self.client.streaming().start().await?;
        info!("Started OBS streaming");
        Ok(())
    }

    /// Stop streaming
    pub async fn stop_streaming(&self) -> Result<()> {
        self.client.streaming().stop().await?;
        info!("Stopped OBS streaming");
        Ok(())
    }

    /// Get a screenshot of the current program output (for sanity check)
    pub async fn get_screenshot(&self) -> Result<Vec<u8>> {
        let scene = self.client.scenes().current_program_scene().await?;
        
        let screenshot = self
            .client
            .sources()
            .take_screenshot(obws::requests::sources::TakeScreenshot {
                source: scene.id.name.as_str().into(),
                width: Some(320),  // Small size for quick analysis
                height: Some(180),
                format: "png",
                compression_quality: Some(50),
            })
            .await?;

        // Decode base64
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&screenshot)
            .context("Failed to decode screenshot")?;

        Ok(bytes)
    }

    /// Check if the current output is a "black screen"
    /// Returns true if the screenshot appears to be mostly black
    pub async fn is_black_screen(&self) -> Result<bool> {
        let screenshot_bytes = self.get_screenshot().await?;

        // Load image and check average brightness
        let img = image::load_from_memory(&screenshot_bytes)
            .context("Failed to load screenshot image")?;

        let gray = img.to_luma8();
        let total_brightness: u64 = gray.pixels().map(|p| p.0[0] as u64).sum();
        let pixel_count = gray.width() as u64 * gray.height() as u64;
        let average_brightness = total_brightness / pixel_count;

        // Consider it "black" if average brightness is below threshold
        let is_black = average_brightness < 10;

        debug!(
            "Screenshot analysis: avg_brightness={}, is_black={}",
            average_brightness, is_black
        );

        Ok(is_black)
    }

    /// Get the current scene name
    pub async fn current_scene(&self) -> Result<String> {
        let scene = self.client.scenes().current_program_scene().await?;
        Ok(scene.id.name)
    }

    /// Get recording output directory
    pub async fn get_record_directory(&self) -> Result<String> {
        let config = self.client.config().record_directory().await?;
        Ok(config)
    }
    
    /// Subscribe to OBS events and forward them to a channel
    /// 
    /// Spawns a background task that listens for OBS events and sends
    /// relevant ones (recording/streaming state changes) to the returned receiver.
    pub fn subscribe_events(&self) -> Result<mpsc::UnboundedReceiver<OBSEvent>> {
        let raw_events = self.client.events()
            .context("Failed to subscribe to OBS events")?;
        
        let (tx, rx) = mpsc::unbounded_channel();
        
        // Spawn a task to forward events
        tokio::spawn(async move {
            // Pin the stream to allow polling
            tokio::pin!(raw_events);
            
            while let Some(event) = raw_events.next().await {
                let obs_event = match event {
                    Event::RecordStateChanged { active, state, path } => {
                        match state {
                            OutputState::Started if active => Some(OBSEvent::RecordingStarted),
                            OutputState::Stopped if !active => Some(OBSEvent::RecordingStopped {
                                path: path.map(PathBuf::from),
                            }),
                            _ => None,
                        }
                    }
                    Event::StreamStateChanged { active, state } => {
                        match state {
                            OutputState::Started if active => Some(OBSEvent::StreamingStarted),
                            OutputState::Stopped if !active => Some(OBSEvent::StreamingStopped),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                
                if let Some(e) = obs_event {
                    if tx.send(e).is_err() {
                        // Receiver dropped, exit task
                        break;
                    }
                }
            }
        });
        
        Ok(rx)
    }
}
