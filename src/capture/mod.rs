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

mod apps;
mod context;
#[cfg(target_os = "linux")]
pub(crate) mod focus;
mod frontmost;
mod recording;
mod recovery;
mod sources;
#[cfg(target_os = "linux")]
pub(crate) mod wayland_output;
#[cfg(target_os = "linux")]
pub(crate) mod x11_windows;

/// Whether this platform/session can drive the single-active-app capture model (capture
/// only the frontmost tracked app, switching on focus). macOS always can (ScreenCaptureKit
/// per-app); Linux can only on a **pure X11 session** (XComposite per-window capture) — on
/// Wayland the portal/PipeWire multi-source path owns window capture instead.
pub fn is_single_active_capable() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(target_os = "linux")]
    {
        x11_windows::is_pure_x11_session()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

pub use apps::{list_capturable_apps, list_running_apps};
pub use context::{CaptureContext, RecordingSession};
pub use frontmost::{get_frontmost_app, AppInfo};
pub use recording::{
    calculate_output_dimensions, RecordingConfig, RecordingOutput, RecordingOutputBuilder,
    RecordingState, VideoCodecPreference,
};
pub use recovery::{get_display_name, get_display_uuid, DisplayChangeEvent, DisplayMonitor};
pub use sources::{get_main_display_resolution, get_main_display_uuid, ScreenCaptureSource};

/// Events emitted by the capture system
#[derive(Debug, Clone)]
pub enum CaptureEvent {
    /// Recording started
    RecordingStarted,
    /// Recording stopped with output file path
    RecordingStopped { path: Option<std::path::PathBuf> },
    /// Capture source state changed
    SourceStateChanged { name: String, active: bool },
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
