//! Platform-specific tray abstraction.
//!
//! Defines the trait that each platform implements for system tray functionality.
//! Business logic (updater, auth, engine commands) lives in `tray.rs`; this module
//! contains only the cross-platform interface.

use anyhow::Result;
use std::path::PathBuf;

/// Actions that can be triggered by the user via the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    StartRecording,
    StopRecording,
    Panic,
    ToggleUploads,
    SignIn,
    Settings,
    CheckForUpdates,
    ReportBug,
    Quit,
}

/// A single glanceable notification shown in the tray's notification section.
///
/// Phase 1 notifications are *derived from live state* (e.g. "uploads are paused"
/// is present while uploads are paused), so they carry no read/unread state:
/// acting on one clears the underlying condition and the entry disappears on the
/// next refresh. Each notification reuses an existing `TrayAction` as its click
/// handler, so no new engine plumbing is needed per notification kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayNotification {
    /// One-line, glanceable text (e.g. "Uploads are paused").
    pub title: String,
    /// What clicking this notification does — reuses the existing action set.
    pub action: TrayAction,
}

/// Visual state of the tray icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayIconState {
    Idle,
    Recording,
    Blocked,
}

/// Paths to tray icon files for each state.
pub struct TrayIconPaths {
    pub idle: PathBuf,
    pub recording: PathBuf,
    pub blocked: PathBuf,
}

/// Full display state for the tray menu.
///
/// `TrayApp` maintains this state and passes it to the platform tray
/// whenever a refresh is needed.
pub struct TrayDisplayState {
    /// Which icon variant to show.
    pub icon_state: TrayIconState,
    /// Top-of-menu status line (e.g. "Status: Capturing (42 events)").
    pub status_text: String,
    /// Account display line (e.g. "Signed in as user@example.com"), empty when signed out.
    pub account_text: String,
    /// Label for the sign-in / sign-out menu item.
    pub sign_action_text: String,
    /// Whether the sign-in / sign-out menu item is clickable.
    pub auth_action_enabled: bool,
    /// Whether "Start Recording" should be enabled.
    pub can_start: bool,
    /// Whether "Stop Recording" should be enabled.
    pub can_stop: bool,
    /// Label for the uploads toggle (e.g. "Pause Uploads" / "Resume Uploads").
    pub uploads_text: String,
    /// Whether "Check for Updates" should be enabled.
    pub can_check_updates: bool,
    /// Active glanceable notifications (derived from live state). macOS-only for
    /// now; the badge count and the "Notifications" submenu render from this.
    /// Other platforms don't populate or render it yet.
    #[cfg(target_os = "macos")]
    pub notifications: Vec<TrayNotification>,
}

/// Result of polling the platform tray for events.
pub enum PlatformTrayPoll {
    /// Normal iteration, nothing happened.
    None,
    /// A user action was taken via the menu.
    Action(TrayAction),
    /// The native event loop signaled exit (e.g. app termination).
    Exit,
    /// The platform requests a process restart (e.g. macOS screen unlock,
    /// status-item detachment).
    RequestRestart,
}

/// Platform-specific system tray implementation.
///
/// Each platform provides a concrete type implementing this trait.
/// The shared `TrayApp` drives the event loop and business logic,
/// delegating native UI operations to this trait.
pub trait PlatformTray {
    /// Initialize the native tray. Called once before the event loop starts.
    fn init(&mut self) -> Result<()>;

    /// Run one non-blocking iteration of the native event loop.
    /// Processes pending OS events and checks for user actions.
    fn poll(&mut self) -> PlatformTrayPoll;

    /// Update the tray display to match the given state.
    fn update(&mut self, state: &TrayDisplayState);

    /// Clean up native resources before the process is replaced (e.g. via `exec`).
    fn prepare_for_restart(&mut self);

    /// Signal the native event loop to exit.
    fn exit(&mut self);
}

/// Stub tray for platforms without native tray support.
/// All operations are no-ops; `poll` always returns `None`.
pub struct StubTray;

impl PlatformTray for StubTray {
    fn init(&mut self) -> Result<()> {
        Ok(())
    }

    fn poll(&mut self) -> PlatformTrayPoll {
        PlatformTrayPoll::None
    }

    fn update(&mut self, _state: &TrayDisplayState) {}

    fn prepare_for_restart(&mut self) {}

    fn exit(&mut self) {}
}
