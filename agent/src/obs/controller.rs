//! OBS WebSocket controller implementation

use anyhow::{Context, Result};
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
    /// Hooked sources state changed (vendor event)
    HookedSourcesChanged { any_hooked: bool },
}

/// State of window capture sources from the crowd-cast OBS plugin
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

/// Vendor event payload for hooked source changes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookedSourcesChangedEvent {
    /// Source name
    pub name: String,
    /// Whether the source is hooked to a window
    pub hooked: bool,
    /// Whether the source is active (in the current scene)
    pub active: bool,
    /// Whether any source is currently hooked and active
    pub any_hooked: bool,
}

/// OBS recording state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    Stopped,
    Recording,
    Paused,
}

/// OBS streaming state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingState {
    Stopped,
    Streaming,
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
    client: Arc<RwLock<Client>>,
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
            client: Arc::new(RwLock::new(client)),
            state: Arc::new(RwLock::new(CaptureState::default())),
            config: config.clone(),
        };

        if let Some(output_directory) = config.recording.output_directory.as_ref() {
            controller
                .ensure_recording_directory(output_directory)
                .await
                .context("Failed to configure OBS recording directory")?;
        }

        // Get initial state
        controller.refresh_state().await?;

        Ok(controller)
    }

    /// Reconnect to OBS and refresh internal state
    pub async fn reconnect(&self) -> Result<()> {
        let client = Client::connect(
            &self.config.obs.host,
            self.config.obs.port,
            self.config.obs.password.as_deref(),
        )
        .await
        .context("Failed to reconnect to OBS WebSocket")?;

        {
            let mut guard = self.client.write().await;
            *guard = client;
        }

        self.refresh_state().await?;
        Ok(())
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
        let record_status = {
            let client = self.client.read().await;
            client.recording().status().await?
        };
        state.recording = if record_status.paused {
            RecordingState::Paused
        } else if record_status.active {
            RecordingState::Recording
        } else {
            RecordingState::Stopped
        };

        // Get streaming state
        let stream_status = {
            let client = self.client.read().await;
            client.streaming().status().await?
        };
        state.streaming = if stream_status.active {
            StreamingState::Streaming
        } else {
            StreamingState::Stopped
        };

        // Get current scene
        let scene = {
            let client = self.client.read().await;
            client.scenes().current_program_scene().await?
        };
        state.current_scene = scene.id.name.clone();

        // Get hooked sources from our plugin (via vendor request)
        state.hooked_sources = self.get_hooked_sources().await.ok();

        // Determine if we should capture
        let is_recording = matches!(state.recording, RecordingState::Recording);

        let any_hooked = state
            .hooked_sources
            .as_ref()
            .map(|h| h.any_hooked)
            .unwrap_or(true); // Default to true if plugin not available

        state.should_capture = is_recording && any_hooked;

        debug!(
            "OBS state: recording={:?}, streaming={:?}, any_hooked={}, should_capture={}",
            state.recording, state.streaming, any_hooked, state.should_capture
        );

        Ok(())
    }

    /// Query the crowd-cast plugin for hooked sources state
    async fn get_hooked_sources(&self) -> Result<HookedSourcesResponse> {
        let client = self.client.read().await;

        // Use vendor request to query our plugin
        // obws supports vendor requests via call_vendor_request
        let empty_data = serde_json::json!({});
        let response = client
            .general()
            .call_vendor_request(obws::requests::general::CallVendorRequest {
                vendor_name: "crowd-cast",
                request_type: "GetHookedSources",
                request_data: &empty_data,
            })
            .await
            .context("Failed to call crowd-cast.GetHookedSources vendor request")?;

        let hooked_response: HookedSourcesResponse = serde_json::from_value(response.response_data)
            .context("Failed to parse hooked sources response")?;

        Ok(hooked_response)
    }

    /// Set capture enabled state (for Wayland manual toggle fallback)
    pub async fn set_capture_enabled(&self, enabled: bool) -> Result<()> {
        let client = self.client.read().await;

        let request_data = serde_json::json!({
            "enabled": enabled
        });
        
        let response: obws::responses::general::VendorResponse<serde_json::Value> = client
            .general()
            .call_vendor_request(obws::requests::general::CallVendorRequest {
                vendor_name: "crowd-cast",
                request_type: "SetCaptureEnabled",
                request_data: &request_data,
            })
            .await
            .context("Failed to call crowd-cast.SetCaptureEnabled vendor request")?;

        // Log the response for debugging
        debug!("SetCaptureEnabled response: {:?}", response.response_data);

        Ok(())
    }

    /// Start recording
    pub async fn start_recording(&self) -> Result<()> {
        let client = self.client.read().await;
        client.recording().start().await?;
        info!("Started OBS recording");
        Ok(())
    }

    /// Stop recording
    pub async fn stop_recording(&self) -> Result<()> {
        let client = self.client.read().await;
        client.recording().stop().await?;
        info!("Stopped OBS recording");
        Ok(())
    }

    /// Get the current scene name
    pub async fn current_scene(&self) -> Result<String> {
        let client = self.client.read().await;
        let scene = client.scenes().current_program_scene().await?;
        Ok(scene.id.name)
    }

    async fn ensure_recording_directory(&self, output_directory: &PathBuf) -> Result<()> {
        tokio::fs::create_dir_all(output_directory)
            .await
            .with_context(|| {
                format!(
                    "Failed to create recording output directory: {:?}",
                    output_directory
                )
            })?;

        let directory = output_directory.to_string_lossy().to_string();
        let client = self.client.read().await;
        client
            .config()
            .set_record_directory(&directory)
            .await
            .with_context(|| format!("Failed to set OBS record directory to {}", directory))?;

        info!("Set OBS record directory to {}", directory);
        Ok(())
    }
    
    /// Subscribe to OBS events and forward them to a channel
    /// 
    /// Spawns a background task that listens for OBS events and sends
    /// relevant ones (recording/streaming state changes) to the returned receiver.
    pub async fn subscribe_events(&self) -> Result<mpsc::UnboundedReceiver<OBSEvent>> {
        let raw_events = {
            let client = self.client.read().await;
            client
                .events()
                .context("Failed to subscribe to OBS events")?
        };
        
        let (tx, rx) = mpsc::unbounded_channel();
        let state = self.state.clone();
        
        // Spawn a task to forward events
        tokio::spawn(async move {
            // Pin the stream to allow polling
            tokio::pin!(raw_events);
            
            while let Some(event) = raw_events.next().await {
                let obs_event = match event {
                    Event::RecordStateChanged { active, state: output_state, path } => {
                        let mut capture = state.write().await;
                        match output_state {
                            OutputState::Started if active => {
                                capture.recording = RecordingState::Recording;
                                capture.should_capture = should_capture(&capture);
                                Some(OBSEvent::RecordingStarted)
                            }
                            OutputState::Stopped if !active => {
                                capture.recording = RecordingState::Stopped;
                                capture.should_capture = should_capture(&capture);
                                Some(OBSEvent::RecordingStopped {
                                    path: path.map(PathBuf::from),
                                })
                            }
                            _ => None,
                        }
                    }
                    Event::StreamStateChanged { active, state: output_state } => {
                        let mut capture = state.write().await;
                        match output_state {
                            OutputState::Started if active => {
                                capture.streaming = StreamingState::Streaming;
                                capture.should_capture = should_capture(&capture);
                                Some(OBSEvent::StreamingStarted)
                            }
                            OutputState::Stopped if !active => {
                                capture.streaming = StreamingState::Stopped;
                                capture.should_capture = should_capture(&capture);
                                Some(OBSEvent::StreamingStopped)
                            }
                            _ => None,
                        }
                    }
                    Event::VendorEvent {
                        vendor_name,
                        event_type,
                        event_data,
                    } => {
                        if vendor_name == "crowd-cast" && event_type == "HookedSourcesChanged" {
                            match serde_json::from_value::<HookedSourcesChangedEvent>(event_data) {
                                Ok(payload) => {
                                    let mut capture = state.write().await;
                                    update_hooked_sources(&mut capture, &payload);
                                    capture.should_capture = should_capture(&capture);
                                    Some(OBSEvent::HookedSourcesChanged {
                                        any_hooked: payload.any_hooked,
                                    })
                                }
                                Err(e) => {
                                    debug!("Failed to parse HookedSourcesChanged event: {}", e);
                                    None
                                }
                            }
                        } else {
                            None
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

fn should_capture(state: &CaptureState) -> bool {
    let is_recording_or_streaming = matches!(state.recording, RecordingState::Recording)
        || matches!(state.streaming, StreamingState::Streaming);
    let any_hooked = state
        .hooked_sources
        .as_ref()
        .map(|h| h.any_hooked)
        .unwrap_or(true);
    is_recording_or_streaming && any_hooked
}

fn update_hooked_sources(state: &mut CaptureState, payload: &HookedSourcesChangedEvent) {
    let hooked = state.hooked_sources.get_or_insert(HookedSourcesResponse {
        sources: Vec::new(),
        any_hooked: payload.any_hooked,
    });

    if let Some(source) = hooked.sources.iter_mut().find(|s| s.name == payload.name) {
        source.hooked = payload.hooked;
        source.active = payload.active;
    } else {
        hooked.sources.push(SourceState {
            name: payload.name.clone(),
            hooked: payload.hooked,
            active: payload.active,
        });
    }

    hooked.any_hooked = payload.any_hooked;
}
