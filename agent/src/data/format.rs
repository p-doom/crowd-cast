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
    
    /// Start timestamp (Unix epoch microseconds)
    pub start_time_us: u64,
    
    /// End timestamp (Unix epoch microseconds)
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
    
    /// Add an event to the chunk
    pub fn add_event(&mut self, event: InputEvent) {
        if self.events.is_empty() {
            self.start_time_us = event.timestamp_us;
        }
        self.end_time_us = event.timestamp_us;
        self.events.push(event);
    }
    
    /// Serialize to MessagePack bytes
    pub fn to_msgpack(&self) -> Result<Vec<u8>> {
        Ok(rmp_serde::to_vec(self)?)
    }
    
    /// Deserialize from MessagePack bytes
    pub fn from_msgpack(data: &[u8]) -> Result<Self> {
        Ok(rmp_serde::from_slice(data)?)
    }
    
    /// Serialize to JSON string
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
    
    /// Deserialize from JSON string
    pub fn from_json(data: &str) -> Result<Self> {
        Ok(serde_json::from_str(data)?)
    }
}

/// Information about a completed recording chunk ready for upload
#[derive(Debug, Clone)]
pub struct CompletedChunk {
    /// Session ID
    pub session_id: String,
    
    /// Chunk ID (usually derived from filename)
    pub chunk_id: String,
    
    /// Path to video file
    pub video_path: std::path::PathBuf,
    
    /// Input log data
    pub input_chunk: InputChunk,
}
