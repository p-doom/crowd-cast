//! Data format and serialization utilities

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::InputEvent;

/// A chunk of input events associated with a video chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputChunk {
    /// Session identifier
    pub session_id: String,
    
    /// Chunk identifier (matches video chunk filename)
    pub chunk_id: String,
    
    /// Start timestamp in microseconds since backend start.
    /// This represents when video recording started and is used as the
    /// reference point (t=0) for aligning input events with video.
    pub start_time_us: u64,
    
    /// End timestamp in microseconds since backend start.
    /// Set to the timestamp of the last event in the chunk.
    pub end_time_us: u64,
    
    /// Input events in this chunk
    pub events: Vec<InputEvent>,
    
    /// Metadata about the chunk
    pub metadata: ChunkMetadata,
}

/// Metadata associated with an input chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMetadata {
    /// OBS scene name at time of capture
    pub obs_scene: String,
    
    /// Number of times capture was paused during this chunk
    pub pause_count: u32,
    
    /// Total duration paused (microseconds)
    pub pause_duration_us: u64,
    
    /// Agent version
    pub agent_version: String,
    
    /// Platform (windows, macos, linux)
    pub platform: String,
}

impl InputChunk {
    /// Create a new input chunk
    pub fn new(session_id: String, chunk_id: String, obs_scene: String) -> Self {
        Self {
            session_id,
            chunk_id,
            start_time_us: 0,
            end_time_us: 0,
            events: Vec::new(),
            metadata: ChunkMetadata {
                obs_scene,
                pause_count: 0,
                pause_duration_us: 0,
                agent_version: env!("CARGO_PKG_VERSION").to_string(),
                platform: std::env::consts::OS.to_string(),
            },
        }
    }
    
    /// Set the recording start timestamp.
    /// This should be called when OBS recording starts to synchronize
    /// input events with the video timeline.
    /// Only sets the timestamp if it hasn't been set yet (is still 0),
    /// to avoid overwriting on resume after pause.
    pub fn set_recording_start(&mut self, timestamp_us: u64) {
        if self.start_time_us == 0 {
            self.start_time_us = timestamp_us;
        }
    }
    
    /// Add an event to the chunk
    pub fn add_event(&mut self, event: InputEvent) {
        self.end_time_us = event.timestamp_us;
        self.events.push(event);
    }
    
    /// Serialize to MessagePack bytes
    pub fn to_msgpack(&self) -> Result<Vec<u8>> {
        Ok(rmp_serde::to_vec(self)?)
    }
    
}

/// Information about a completed recording chunk ready for upload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedChunk {
    /// Session ID
    pub session_id: String,
    
    /// Chunk ID (usually derived from filename)
    pub chunk_id: String,
    
    /// Path to video file (optional, may not be set immediately)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_path: Option<std::path::PathBuf>,
    
    /// Input events in this chunk
    pub events: Vec<InputEvent>,
    
    /// Start timestamp (microseconds)
    pub start_time_us: u64,
    
    /// End timestamp (microseconds)
    pub end_time_us: u64,
}

/// Buffer for collecting input events during capture
#[derive(Debug, Default)]
pub struct InputEventBuffer {
    events: Vec<InputEvent>,
}

impl InputEventBuffer {
    /// Create a new empty buffer
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }
    
    /// Add an event to the buffer
    pub fn push(&mut self, event: InputEvent) {
        self.events.push(event);
    }
    
    /// Get the number of events in the buffer
    pub fn len(&self) -> usize {
        self.events.len()
    }
    
    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    
    /// Clear the buffer
    pub fn clear(&mut self) {
        self.events.clear();
    }
    
    /// Drain all events from the buffer
    pub fn drain(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.events)
    }
}
