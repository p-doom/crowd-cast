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
    /// Stop recording cleanly so a pending auto-update can install
    PrepareForUpdate,
    /// Recreate the active capture source
    RefreshCaptureSource,
    /// Reload target apps (user changed settings via UI)
    ReloadTargetApps {
        target_apps: Vec<String>,
        capture_all: bool,
    },
    /// Pause uploads (segments accumulate on disk)
    PauseUploads,
    /// Resume uploads (drain the pending queue)
    ResumeUploads,
    /// Panic: delete current + buffered recordings
    Panic,
    /// User requested switch to a specific display (from notification action)
    SwitchToDisplay { display_id: u32 },
    /// Restart the process (exec) for fresh capture sources after unlock
    RestartProcess,
    /// System resumed from a suspend (Windows/Linux): restart the recording fresh so the keylog
    /// and video re-zero together. Sent by the OS power-event listeners (the primary, duration-
    /// independent resume signal); the engine's wall-clock-gap check is the fallback. macOS uses
    /// `RestartProcess` via its restart-on-unlock path instead, so it never sends this.
    ResumeFromSuspend,
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
