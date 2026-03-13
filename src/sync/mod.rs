//! Synchronization engine - coordinates input capture with recording state

mod engine;

pub use engine::{create_engine_channels, SyncEngine};

/// Commands that can be sent to the sync engine
#[derive(Debug, Clone)]
pub enum EngineCommand {
    /// Manually start recording
    StartRecording,
    /// Manually stop recording
    StopRecording,
    /// Pause recording (both video and keylog)
    PauseRecording,
    /// Resume recording (both video and keylog)
    ResumeRecording,
    /// User requested switch to a specific display (from notification action)
    SwitchToDisplay { display_id: u32 },
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
    /// Recording is paused (both video and keylog)
    Paused,
    /// Recording is active but sources are not working
    RecordingBlocked,
    /// Waiting for libobs to be ready
    WaitingForOBS,
    /// Engine is uploading a chunk
    Uploading {
        /// Chunk ID being uploaded
        chunk_id: String,
    },
    /// An error occurred
    Error(String),
}
