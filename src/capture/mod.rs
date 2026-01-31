//! Embedded libobs capture module
//!
//! This module provides screen capture and recording functionality using
//! libobs directly (embedded), rather than communicating with an external
//! OBS process via WebSocket.
//!
//! Key benefits:
//! - Single binary distribution (no external OBS dependency)
//! - Direct control over capture sources and recording
//! - Ability to fix ScreenCaptureKit issues directly

mod context;
mod recording;
mod sources;
mod recovery;
mod frontmost;
mod apps;

pub use context::{CaptureContext, RecordingSession};
pub use recording::{RecordingConfig, RecordingOutput, RecordingOutputBuilder, RecordingState, VideoCodecPreference};
pub use sources::{ScreenCaptureSource, get_main_display_uuid, get_main_display_resolution};
pub use recovery::{DisplayMonitor, DisplayChangeEvent, get_display_name, get_display_uuid};
pub use frontmost::{get_frontmost_app, AppInfo};
pub use apps::{list_running_apps, list_capturable_apps};

/// Events emitted by the capture system
#[derive(Debug, Clone)]
pub enum CaptureEvent {
    /// Recording started
    RecordingStarted,
    /// Recording stopped with output file path
    RecordingStopped { path: Option<std::path::PathBuf> },
    /// Capture source state changed
    SourceStateChanged { 
        name: String,
        active: bool,
    },
    /// All sources recovered after display reconnect
    SourcesRecovered,
}

/// Combined capture state
#[derive(Debug, Clone, Default)]
pub struct CaptureState {
    /// Whether we should be logging input (recording active + sources working)
    pub should_capture: bool,
    /// Current recording state
    pub recording: RecordingStateInfo,
    /// Whether any capture source is active
    pub any_source_active: bool,
}

/// Recording state information
#[derive(Debug, Clone, Default)]
pub struct RecordingStateInfo {
    pub is_recording: bool,
    pub is_paused: bool,
    pub output_path: Option<std::path::PathBuf>,
}
