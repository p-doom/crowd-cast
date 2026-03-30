//! Synchronization engine
//!
//! Coordinates input capture with recording state and filters input
//! based on the frontmost application. Manages libobs recording with
//! HEVC hardware encoding (VideoToolbox on macOS) when available.
//!
//! Supports segmented recording for progressive upload - recordings are
//! split into fixed-duration segments that are uploaded and deleted
//! immediately to minimize storage overhead.

use anyhow::Result;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::capture::{
    get_display_uuid, get_frontmost_app, CaptureContext, DisplayChangeEvent, DisplayMonitor,
    RecordingSession,
};
use crate::config::Config;
use crate::data::{
    CompletedChunk, ContextEvent, EventType, InputEvent, InputEventBuffer, UNCAPTURED_APP_ID,
    UNKNOWN_APP_ID,
};
use crate::input::{create_input_backend, InputBackend};
use crate::installer::permissions::describe_missing_permissions;
use crate::ui::notifications::{
    is_authorized as notifications_authorized, show_capture_resumed_notification,
    show_display_change_notification, show_idle_paused_notification,
    show_idle_resumed_notification, show_permissions_missing_notification,
    show_recording_paused_notification, show_recording_resumed_notification,
    show_recording_started_notification, show_recording_stopped_notification, NotificationAction,
};
use crate::upload::Uploader;

use super::{EngineCommand, EngineStatus};

/// Restart the current process with a clean OBS context.
/// Uses Unix exec to replace this process with a fresh one.
fn restart_process() -> ! {
    info!("Restarting process for fresh OBS context...");
    let exe = std::env::current_exe().expect("Failed to get current executable path");
    let args: Vec<String> = std::env::args().skip(1).collect();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe).args(&args).exec();
        // exec() only returns on error
        error!("exec failed: {}", err);
        std::process::exit(1);
    }

    #[cfg(not(unix))]
    {
        std::process::Command::new(&exe)
            .args(&args)
            .spawn()
            .expect("Failed to restart process");
        std::process::exit(0);
    }
}

/// Persisted recording state — survives restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistedRecordingState {
    Recording,
    Paused,
    Stopped,
}

fn recording_state_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "crowd-cast", "agent")
        .map(|p| p.data_dir().join("recording_state"))
}

fn read_recording_state() -> Option<PersistedRecordingState> {
    let path = recording_state_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    match content.trim() {
        "recording" => Some(PersistedRecordingState::Recording),
        "paused" => Some(PersistedRecordingState::Paused),
        "stopped" => Some(PersistedRecordingState::Stopped),
        _ => None,
    }
}

fn write_recording_state(state: PersistedRecordingState) {
    let Some(path) = recording_state_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let s = match state {
        PersistedRecordingState::Recording => "recording",
        PersistedRecordingState::Paused => "paused",
        PersistedRecordingState::Stopped => "stopped",
    };
    if let Err(e) = std::fs::write(&path, s) {
        warn!("Failed to persist recording state: {}", e);
    }
}

fn uploads_paused_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "crowd-cast", "agent")
        .map(|p| p.data_dir().join("uploads_paused"))
}

fn read_uploads_paused() -> bool {
    uploads_paused_path()
        .and_then(|p| std::fs::read_to_string(&p).ok())
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

fn write_uploads_paused(paused: bool) {
    let Some(path) = uploads_paused_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, if paused { "true" } else { "false" });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusKind {
    Idle,
    Capturing,
    Paused,
    RecordingBlocked,
    WaitingForOBS,
    Uploading,
    Error,
}

impl StatusKind {
    fn from_status(status: &EngineStatus) -> Self {
        match status {
            EngineStatus::Idle => Self::Idle,
            EngineStatus::Capturing { .. } => Self::Capturing,
            EngineStatus::Paused => Self::Paused,
            EngineStatus::RecordingBlocked => Self::RecordingBlocked,
            EngineStatus::WaitingForOBS => Self::WaitingForOBS,
            EngineStatus::Uploading { .. } => Self::Uploading,
            EngineStatus::Error(_) => Self::Error,
        }
    }
}

/// A completed segment ready for upload
#[derive(Debug)]
struct CompletedSegment {
    /// The completed chunk with video path and events
    chunk: CompletedChunk,
    /// Path to the input events file
    input_path: PathBuf,
}

#[derive(Debug)]
enum UploadMessage {
    StartSession(String),
    Segment(CompletedSegment),
}

#[derive(Debug)]
struct RetryItem {
    segment: CompletedSegment,
    attempts: u32,
    first_failed_at: Instant,
    next_attempt_at: Instant,
}

#[derive(Debug)]
struct RetryEntry {
    next_attempt_at: Instant,
    sequence: u64,
    item: RetryItem,
}

impl Ord for RetryEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse for min-heap behavior.
        other
            .next_attempt_at
            .cmp(&self.next_attempt_at)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for RetryEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for RetryEntry {
    fn eq(&self, other: &Self) -> bool {
        self.next_attempt_at == other.next_attempt_at && self.sequence == other.sequence
    }
}

impl Eq for RetryEntry {}

/// Pending display refresh retry state
#[derive(Debug)]
struct PendingDisplayRefresh {
    /// Display name for logging
    display_name: String,
    /// Number of retry attempts made
    attempt: u32,
    /// When to attempt the next retry
    next_retry_at: Instant,
    /// Whether to restart recording after reinit
    restart_recording: bool,
    /// Whether to show a capture resumed notification on success
    show_resumed_notification: bool,
    /// Whether to stop recording before reinit (needed if previous stop failed)
    stop_recording_first: bool,
    /// Whether to do a full libobs context reinitialization (true) or just recreate sources (false)
    full_reinit: bool,
}

#[derive(Debug, Clone)]
struct PendingAppSwitch {
    target_app: Option<String>,
    scheduled_at: Instant,
}

#[derive(Debug, Clone)]
struct PendingCaptureWatchdog {
    expected_app: String,
    deadline: Instant,
    attempt: u32,
}

#[derive(Debug)]
struct PendingInputTransition {
    target_app: String,
    events: Vec<InputEvent>,
}

/// Maximum number of display refresh retry attempts
const MAX_DISPLAY_REFRESH_RETRIES: u32 = 5;

/// Base delay between display refresh retries (doubles each attempt)
const DISPLAY_REFRESH_RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
const CAPTURING_STATUS_INTERVAL: Duration = Duration::from_secs(1);
const MAX_TRANSITION_INPUT_EVENTS: usize = 512;

/// The synchronization engine coordinates recording and input capture
pub struct SyncEngine {
    /// Configuration
    config: Config,
    /// Capture context for libobs operations
    capture_ctx: CaptureContext,
    /// Input backend
    input_backend: Box<dyn InputBackend>,
    /// Command receiver
    cmd_rx: mpsc::Receiver<EngineCommand>,
    /// Status broadcaster
    status_tx: broadcast::Sender<EngineStatus>,
    /// Input event buffer
    event_buffer: InputEventBuffer,
    /// Whether input capture is currently enabled
    capture_enabled: bool,
    /// Whether recording is currently paused (both video and keylog)
    is_paused: bool,
    /// Last known frontmost app
    last_frontmost_app: Option<String>,
    /// Current recording session
    current_session: Option<RecordingSession>,
    /// OBS timestamp at recording start (nanoseconds)
    recording_start_ns: Option<u64>,
    /// Output directory for chunks
    output_dir: PathBuf,
    /// Display monitor for detecting display hotplug events (macOS)
    display_monitor: DisplayMonitor,
    /// Main session ID (persists across all segments)
    main_session_id: Option<String>,
    /// Current segment index (0-based)
    segment_index: u32,
    /// Channel for completed segments to upload
    upload_tx: mpsc::UnboundedSender<UploadMessage>,
    /// Buffer for completed segments — held for 10 minutes before uploading
    /// so the panic button can delete recent recordings without a backend call.
    upload_buffer: std::collections::VecDeque<(Instant, CompletedSegment)>,
    /// Uploader instance
    uploader: Uploader,
    /// Segment duration in seconds (cached from config)
    segment_duration_secs: u64,
    /// Whether to delete files after upload
    delete_after_upload: bool,
    /// Shared flag to pause/resume uploads from the tray
    uploads_paused: Arc<AtomicBool>,
    /// Upload receiver (taken once when run() starts)
    upload_rx: Option<mpsc::UnboundedReceiver<UploadMessage>>,
    /// Notification action receiver (taken once when run() starts)
    notification_rx: Option<mpsc::UnboundedReceiver<NotificationAction>>,
    /// Pending display refresh retry (for when SCK isn't ready immediately)
    pending_display_refresh: Option<PendingDisplayRefresh>,
    /// Last time an input event was recorded (buffered for upload)
    last_recorded_action_time: Instant,
    /// Whether we're currently auto-paused due to idle (vs user-initiated pause)
    idle_paused: bool,
    /// Idle timeout duration (cached from config, Duration::ZERO means disabled)
    idle_timeout: Duration,
    /// Whether to pause uploads during idle
    pause_uploads_on_idle: bool,
    /// Last broadcast status kind (used to dedupe noisy status broadcasts)
    last_status_kind: Option<StatusKind>,
    /// Last time a capturing status was broadcast (for throttling)
    last_capturing_status_at: Option<Instant>,
    /// Whether the macOS single-active-app capture strategy is enabled
    single_active_app_capture: bool,
    /// Whether to blank the video when a non-target app is frontmost
    blank_video_on_untracked_app: bool,
    /// Timeout for the active-source readiness watchdog
    capture_watchdog_timeout: Duration,
    /// Number of automatic source-refresh retries before giving up
    capture_watchdog_max_retries: u32,
    /// Pending app source switch awaiting application
    pending_app_switch: Option<PendingAppSwitch>,
    /// Pending active-source readiness watchdog
    pending_capture_watchdog: Option<PendingCaptureWatchdog>,
    /// Bundle ID of the app whose capture source failed to become ready after
    /// all watchdog retries. Prevents infinite create/destroy cycles.
    /// Cleared on app switch, recording stop/start, or display change.
    capture_watchdog_exhausted_app: Option<String>,
    /// Segment rotation timer — fires every `segment_duration_secs` to split
    /// the recording into manageable chunks. Stored as a struct field so that
    /// every code path that starts/stops recording (including display recovery)
    /// automatically gets the timer in the right state.
    segment_timer: Option<tokio::time::Interval>,
    /// Input events buffered while waiting for a tracked app's video to become ready
    pending_input_transition: Option<PendingInputTransition>,
    /// Last application context emitted into the raw event stream
    last_emitted_context: Option<String>,
    /// Number of buffered non-context input events for O(1) status updates
    buffered_non_context_event_count: usize,
}

impl SyncEngine {
    /// Create a new sync engine
    pub fn new(
        config: Config,
        capture_ctx: CaptureContext,
        cmd_rx: mpsc::Receiver<EngineCommand>,
        status_tx: broadcast::Sender<EngineStatus>,
        notification_rx: mpsc::UnboundedReceiver<NotificationAction>,
    ) -> Self {
        let output_dir = config
            .recording
            .output_directory
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("crowd-cast-recordings"));

        let (upload_tx, upload_rx) = mpsc::unbounded_channel();
        let uploader = Uploader::new(&config);
        let segment_duration_secs = config.recording.segment_duration_secs;
        let delete_after_upload = config.upload.delete_after_upload;

        // Activity-gated capture settings
        let idle_timeout_secs = config.capture.idle_timeout_secs;
        let idle_timeout = if idle_timeout_secs > 0 {
            Duration::from_secs(idle_timeout_secs)
        } else {
            Duration::ZERO // Disabled
        };
        let pause_uploads_on_idle = config.capture.pause_uploads_on_idle;
        let single_active_app_capture = config.capture.single_active_app_capture
            && cfg!(target_os = "macos")
            && !config.capture.target_apps.is_empty();
        let blank_video_on_untracked_app = config.capture.blank_video_on_untracked_app;
        let capture_watchdog_timeout =
            Duration::from_millis(config.capture.capture_watchdog_timeout_ms);
        let capture_watchdog_max_retries = config.capture.capture_watchdog_max_retries;

        if config.capture.capture_all && single_active_app_capture {
            warn!(
                "capture.capture_all=true with non-empty target_apps and single_active_app_capture enabled \
will still drive video switching from the frontmost app. This configuration is ambiguous and may capture \
unintended app video."
            );
        }

        Self {
            config,
            capture_ctx,
            input_backend: create_input_backend(),
            cmd_rx,
            status_tx,
            event_buffer: InputEventBuffer::new(),
            capture_enabled: false,
            is_paused: false,
            last_frontmost_app: None,
            current_session: None,
            recording_start_ns: None,
            output_dir,
            display_monitor: DisplayMonitor::new(),
            main_session_id: None,
            segment_index: 0,
            upload_tx,
            uploader,
            segment_duration_secs,
            delete_after_upload,
            uploads_paused: Arc::new(AtomicBool::new(read_uploads_paused())),
            upload_buffer: std::collections::VecDeque::new(),
            upload_rx: Some(upload_rx),
            notification_rx: Some(notification_rx),
            pending_display_refresh: None,
            last_recorded_action_time: Instant::now(),
            idle_paused: false,
            idle_timeout,
            pause_uploads_on_idle,
            last_status_kind: None,
            last_capturing_status_at: None,
            single_active_app_capture,
            blank_video_on_untracked_app,
            capture_watchdog_timeout,
            capture_watchdog_max_retries,
            pending_app_switch: None,
            pending_capture_watchdog: None,
            capture_watchdog_exhausted_app: None,
            segment_timer: None,
            pending_input_transition: None,
            last_emitted_context: None,
            buffered_non_context_event_count: 0,
        }
    }

    fn send_status(&mut self, status: EngineStatus) {
        self.send_status_internal(status, false);
    }

    fn send_status_force(&mut self, status: EngineStatus) {
        self.send_status_internal(status, true);
    }

    fn send_status_internal(&mut self, status: EngineStatus, force: bool) {
        let status_kind = StatusKind::from_status(&status);
        let now = Instant::now();

        let should_send = if force {
            true
        } else {
            match status_kind {
                // Capturing can be noisy from polling; dedupe and throttle it.
                StatusKind::Capturing => {
                    self.last_status_kind != Some(StatusKind::Capturing)
                        || self.last_capturing_status_at.map_or(true, |last| {
                            now.duration_since(last) >= CAPTURING_STATUS_INTERVAL
                        })
                }
                // RecordingBlocked can also spam while a non-target app is frontmost.
                StatusKind::RecordingBlocked => {
                    self.last_status_kind != Some(StatusKind::RecordingBlocked)
                }
                _ => true,
            }
        };

        if !should_send {
            return;
        }

        if status_kind == StatusKind::Capturing {
            self.last_capturing_status_at = Some(now);
        }
        self.last_status_kind = Some(status_kind);
        let _ = self.status_tx.send(status);
    }

    fn reset_segment_timer(&mut self) {
        if self.segment_duration_secs > 0 && self.current_session.is_some() && !self.is_paused {
            let duration = Duration::from_secs(self.segment_duration_secs);
            self.segment_timer = Some(tokio::time::interval_at(
                Instant::now() + duration,
                duration,
            ));
        } else {
            self.segment_timer = None;
        }
    }

    /// Buffer a completed segment for delayed upload (10-minute hold).
    fn buffer_segment_for_upload(&mut self, segment: CompletedSegment, segment_id: String) {
        if self.uploader.is_configured() {
            info!("Buffering segment {} for delayed upload", segment_id);
            self.upload_buffer.push_back((Instant::now(), segment));
        }
    }

    /// Graduate buffered segments older than 10 minutes to the upload task.
    fn graduate_upload_buffer(&mut self) {
        const UPLOAD_BUFFER_DELAY: Duration = Duration::from_secs(600);
        let now = Instant::now();

        while let Some((created_at, _)) = self.upload_buffer.front() {
            if now.duration_since(*created_at) >= UPLOAD_BUFFER_DELAY {
                let (_, segment) = self.upload_buffer.pop_front().unwrap();
                let chunk_id = segment.chunk.chunk_id.clone();
                info!("Graduating segment {} from upload buffer", chunk_id);
                if let Err(e) = self.upload_tx.send(UploadMessage::Segment(segment)) {
                    error!("Failed to send graduated segment: {}", e);
                }
            } else {
                break;
            }
        }
    }

    /// Flush all buffered segments to the upload task immediately (for graceful shutdown).
    fn flush_upload_buffer(&mut self) {
        let count = self.upload_buffer.len();
        if count > 0 {
            info!("Flushing {} buffered segment(s) to upload queue", count);
        }
        while let Some((_, segment)) = self.upload_buffer.pop_front() {
            let chunk_id = segment.chunk.chunk_id.clone();
            if let Err(e) = self.upload_tx.send(UploadMessage::Segment(segment)) {
                error!("Failed to flush segment {}: {}", chunk_id, e);
            }
        }
    }

    /// Panic: delete all buffered segments from disk.
    fn purge_upload_buffer(&mut self) {
        let count = self.upload_buffer.len();
        if count > 0 {
            info!("Panic: deleting {} buffered segment(s)", count);
        }
        while let Some((_, segment)) = self.upload_buffer.pop_front() {
            if let Some(ref video_path) = segment.chunk.video_path {
                if let Err(e) = std::fs::remove_file(video_path) {
                    warn!("Failed to delete video {:?}: {}", video_path, e);
                } else {
                    debug!("Deleted video: {:?}", video_path);
                }
            }
            if let Err(e) = std::fs::remove_file(&segment.input_path) {
                warn!("Failed to delete input {:?}: {}", segment.input_path, e);
            } else {
                debug!("Deleted input: {:?}", segment.input_path);
            }
        }
    }

    fn active_video_target(&self) -> Option<&str> {
        self.capture_ctx.active_capture_app()
    }

    fn current_recording_elapsed_us(&self) -> Option<u64> {
        let start_ns = self.recording_start_ns?;
        let current_ns = self.capture_ctx.get_video_frame_time().ok()?;
        Some(current_ns.saturating_sub(start_ns) / 1000)
    }

    fn clear_pending_input_transition(&mut self) {
        self.pending_input_transition = None;
    }

    fn reset_pending_input_transition(&mut self, target_app: &str) {
        if let Some(pending) = &self.pending_input_transition {
            if pending.target_app != target_app && !pending.events.is_empty() {
                debug!(
                    "Discarding {} buffered transition input event(s) for stale target '{}'",
                    pending.events.len(),
                    pending.target_app
                );
            }
        }

        self.pending_input_transition = Some(PendingInputTransition {
            target_app: target_app.to_string(),
            events: Vec::new(),
        });
    }

    fn should_buffer_transition_input(
        &self,
        should_capture: bool,
        desired_target: Option<&str>,
    ) -> bool {
        self.single_active_app_capture
            && should_capture
            && self.current_session.is_some()
            && !self.is_paused
            && desired_target.is_some()
            && !self.capture_enabled
    }

    fn buffer_transition_input_event(&mut self, target_app: &str, event: InputEvent) {
        let needs_reset = self
            .pending_input_transition
            .as_ref()
            .map(|pending| pending.target_app != target_app)
            .unwrap_or(true);
        if needs_reset {
            self.reset_pending_input_transition(target_app);
        }

        if let Some(pending) = self.pending_input_transition.as_mut() {
            if pending.events.len() >= MAX_TRANSITION_INPUT_EVENTS {
                if pending.events.len() == MAX_TRANSITION_INPUT_EVENTS {
                    warn!(
                        "Transition input buffer for '{}' reached {} events; dropping additional events until video is ready",
                        pending.target_app, MAX_TRANSITION_INPUT_EVENTS
                    );
                }
                return;
            }

            pending.events.push(event);
        }
    }

    fn flush_pending_input_transition(&mut self, desired_target: Option<&str>) {
        if !self.capture_enabled {
            return;
        }

        let Some(target_app) = desired_target else {
            self.clear_pending_input_transition();
            return;
        };

        let matches_target = self
            .pending_input_transition
            .as_ref()
            .map(|pending| pending.target_app == target_app)
            .unwrap_or(false);
        if !matches_target {
            if self.pending_input_transition.is_some() {
                self.clear_pending_input_transition();
            }
            return;
        }

        let Some(flush_elapsed_us) = self.current_recording_elapsed_us() else {
            debug!(
                "Deferring flush of buffered transition input for '{}' because recording time is unavailable",
                target_app
            );
            return;
        };

        let Some(pending) = self.pending_input_transition.take() else {
            return;
        };

        if pending.events.is_empty() {
            return;
        }

        let last_raw_timestamp = pending
            .events
            .last()
            .map(|event| event.timestamp_us)
            .unwrap_or(0);
        let buffered_count = pending.events.len();

        for event in pending.events {
            let delta_us = last_raw_timestamp.saturating_sub(event.timestamp_us);
            self.buffer_input_event(InputEvent {
                timestamp_us: flush_elapsed_us.saturating_sub(delta_us),
                event: event.event,
            });
        }

        self.last_recorded_action_time = Instant::now();
        debug!(
            "Flushed {} buffered transition input event(s) for '{}'",
            buffered_count, target_app
        );
    }

    fn rearm_capture_recovery_if_needed(
        &mut self,
        should_capture: bool,
        desired_target: Option<&str>,
    ) {
        let Some(target_app) = desired_target else {
            return;
        };

        if !(self.single_active_app_capture
            && should_capture
            && self.current_session.is_some()
            && !self.is_paused)
        {
            return;
        }

        if self.pending_app_switch.is_some() {
            return;
        }

        if self.capture_ctx.active_capture_app() != Some(target_app) {
            return;
        }

        if self
            .pending_capture_watchdog
            .as_ref()
            .map(|pending| pending.expected_app == target_app)
            .unwrap_or(false)
        {
            return;
        }

        // Don't restart the watchdog if we already exhausted retries for this
        // exact app — prevents infinite create/destroy cycles when a source
        // never becomes ready (e.g., certain Electron apps).
        if self.capture_watchdog_exhausted_app.as_deref() == Some(target_app) {
            return;
        }

        match self.capture_ctx.active_source_is_ready() {
            Ok(true) => {}
            Ok(false) => self.schedule_capture_watchdog(target_app, 0),
            Err(e) => {
                debug!(
                    "Unable to inspect readiness for '{}'; deferring recovery re-arm: {}",
                    target_app, e
                );
            }
        }
    }

    async fn apply_due_app_switch(&mut self) {
        let should_apply = self
            .pending_app_switch
            .as_ref()
            .map(|pending| pending.scheduled_at <= Instant::now())
            .unwrap_or(false);

        if should_apply {
            self.apply_pending_app_switch().await;
        }
    }

    fn schedule_app_switch(&mut self, target_app: Option<String>) {
        if !self.single_active_app_capture {
            return;
        }

        if self.active_video_target() == target_app.as_deref() {
            self.pending_app_switch = None;
            return;
        }

        if self
            .pending_app_switch
            .as_ref()
            .map(|pending| pending.target_app.as_deref() == target_app.as_deref())
            .unwrap_or(false)
        {
            return;
        }

        let scheduled_at = Instant::now();
        info!("Queueing active capture switch to {:?}", target_app);
        self.pending_app_switch = Some(PendingAppSwitch {
            target_app,
            scheduled_at,
        });
    }

    fn schedule_capture_watchdog(&mut self, expected_app: &str, attempt: u32) {
        if !self.single_active_app_capture || self.capture_watchdog_timeout.is_zero() {
            self.pending_capture_watchdog = None;
            return;
        }

        self.pending_capture_watchdog = Some(PendingCaptureWatchdog {
            expected_app: expected_app.to_string(),
            deadline: Instant::now() + self.capture_watchdog_timeout,
            attempt,
        });
    }

    fn clear_capture_watchdog(&mut self) {
        self.pending_capture_watchdog = None;
    }

    fn desired_video_target_for_frontmost(
        &self,
        frontmost_app: Option<&str>,
        should_capture: bool,
    ) -> Option<String> {
        if !self.single_active_app_capture {
            return None;
        }

        match frontmost_app {
            Some(app) if should_capture => Some(app.to_string()),
            Some(_) if self.blank_video_on_untracked_app => None,
            Some(_) => self.active_video_target().map(|app| app.to_string()),
            None => self.active_video_target().map(|app| app.to_string()),
        }
    }

    fn should_enable_capture_for_target(
        &self,
        should_capture: bool,
        desired_target: Option<&str>,
    ) -> bool {
        if !(should_capture && self.current_session.is_some() && !self.is_paused) {
            return false;
        }

        if !self.single_active_app_capture {
            return true;
        }

        if self
            .pending_app_switch
            .as_ref()
            .map(|pending| pending.target_app.as_deref() != desired_target)
            .unwrap_or(false)
        {
            return false;
        }

        if self.capture_ctx.active_capture_app() != desired_target {
            return false;
        }

        let Some(app) = desired_target else {
            return false;
        };

        match self.capture_ctx.active_source_is_ready() {
            Ok(ready) => ready,
            Err(e) => {
                debug!(
                    "Treating active capture source for '{}' as not ready while gating input capture: {}",
                    app, e
                );
                false
            }
        }
    }

    fn update_capture_enabled(&mut self, should_capture: bool, desired_target: Option<&str>) {
        let was_capturing = self.capture_enabled;
        self.capture_enabled =
            self.should_enable_capture_for_target(should_capture, desired_target);

        if self.capture_enabled {
            self.flush_pending_input_transition(desired_target);
        } else if !self.should_buffer_transition_input(should_capture, desired_target) {
            self.clear_pending_input_transition();
        }

        if self.capture_enabled != was_capturing {
            if self.capture_enabled {
                debug!("Input capture enabled");
            } else {
                debug!("Input capture disabled");
            }
        }
    }

    fn refresh_capture_enabled_from_frontmost(&mut self) {
        let (frontmost_app, should_capture) = self.frontmost_capture_state();
        let desired_target =
            self.desired_video_target_for_frontmost(frontmost_app.as_deref(), should_capture);
        self.update_capture_enabled(should_capture, desired_target.as_deref());
    }

    async fn sync_single_active_capture_state_for_input(&mut self) -> (bool, Option<String>) {
        let (frontmost_app, should_capture) = self.frontmost_capture_state();
        let desired_target =
            self.desired_video_target_for_frontmost(frontmost_app.as_deref(), should_capture);

        if self.single_active_app_capture && self.current_session.is_some() {
            self.schedule_app_switch(desired_target.clone());
            self.apply_due_app_switch().await;
            self.rearm_capture_recovery_if_needed(should_capture, desired_target.as_deref());
        }

        self.update_capture_enabled(should_capture, desired_target.as_deref());
        (should_capture, desired_target)
    }

    fn adjust_input_event_timestamp(&self, event: InputEvent) -> InputEvent {
        if let Some(elapsed_us) = self.current_recording_elapsed_us() {
            InputEvent {
                timestamp_us: elapsed_us,
                ..event
            }
        } else {
            event
        }
    }

    fn drain_pending_transition_events_for_persistence(&mut self) -> Vec<InputEvent> {
        let Some(flush_elapsed_us) = self.current_recording_elapsed_us() else {
            if let Some(pending) = self.pending_input_transition.take() {
                warn!(
                    "Dropping {} buffered transition input event(s) for '{}' because recording time is unavailable",
                    pending.events.len(),
                    pending.target_app
                );
            }
            return Vec::new();
        };

        let Some(pending) = self.pending_input_transition.take() else {
            return Vec::new();
        };

        if pending.events.is_empty() {
            return Vec::new();
        }

        let last_raw_timestamp = pending
            .events
            .last()
            .map(|event| event.timestamp_us)
            .unwrap_or(0);
        let mut remapped = Vec::with_capacity(pending.events.len());

        for event in pending.events {
            let delta_us = last_raw_timestamp.saturating_sub(event.timestamp_us);
            remapped.push(InputEvent {
                timestamp_us: flush_elapsed_us.saturating_sub(delta_us),
                event: event.event,
            });
        }

        remapped
    }

    fn prepare_active_capture_target(
        &mut self,
        frontmost_app: Option<&str>,
        should_capture: bool,
        reason: &str,
    ) -> Result<Option<String>> {
        if !self.single_active_app_capture {
            return Ok(None);
        }

        let desired_target = self.desired_video_target_for_frontmost(frontmost_app, should_capture);

        self.capture_ctx
            .switch_active_app_capture(desired_target.as_deref())
            .map_err(|e| anyhow::anyhow!("{}: {}", reason, e))?;

        if desired_target.is_none() {
            self.clear_capture_watchdog();
        }

        Ok(desired_target)
    }

    fn frontmost_capture_state(&mut self) -> (Option<String>, bool) {
        let frontmost = get_frontmost_app();
        let bundle_id = frontmost.as_ref().map(|a| a.bundle_id.clone());
        let should_capture = match bundle_id.as_deref() {
            Some(id) => self.config.should_capture_app(id),
            None => self.config.capture.capture_all,
        };

        if bundle_id != self.last_frontmost_app {
            debug!(
                "Frontmost app changed: {:?} (capture: {})",
                bundle_id, should_capture
            );
            self.last_frontmost_app = bundle_id.clone();
        }

        (bundle_id, should_capture)
    }

    async fn apply_pending_app_switch(&mut self) {
        let Some(pending) = self.pending_app_switch.take() else {
            return;
        };

        let (frontmost_app, should_capture) = self.frontmost_capture_state();
        let desired_target =
            self.desired_video_target_for_frontmost(frontmost_app.as_deref(), should_capture);
        if desired_target != pending.target_app {
            info!(
                "Skipping stale active capture switch to {:?}; current desired target is {:?}",
                pending.target_app, desired_target
            );
            self.schedule_app_switch(desired_target.clone());
            self.update_capture_enabled(should_capture, desired_target.as_deref());
            return;
        }

        let target_app = desired_target;

        // SCK sources only work when created before the OBS context's first
        // recording. If a tracked app wasn't running at startup, it has no
        // scene. The only reliable fix is to restart the process so all
        // currently-running apps get scenes in a fresh OBS context.
        if let Some(app) = target_app.as_deref() {
            if self.capture_ctx.needs_scene_for_app(app) {
                info!(
                    "App '{}' wasn't running at startup — restarting to create its capture source",
                    app
                );
                self.stop_recording().await.ok();
                restart_process();
            }
        }

        match self
            .capture_ctx
            .switch_active_app_capture(target_app.as_deref())
        {
            Ok(switched) => {
                if switched {
                    info!("Applied active capture switch to {:?}", target_app);
                    self.capture_watchdog_exhausted_app = None;
                }
                if let Some(app) = target_app.as_deref() {
                    self.schedule_capture_watchdog(app, 0);
                } else {
                    self.clear_capture_watchdog();
                }
                self.update_capture_enabled(should_capture, target_app.as_deref());
                self.rearm_capture_recovery_if_needed(should_capture, target_app.as_deref());
            }
            Err(e) => {
                error!(
                    "Failed to apply active capture switch to {:?}: {}",
                    target_app, e
                );
                self.update_capture_enabled(should_capture, target_app.as_deref());
                self.send_status_force(EngineStatus::Error(format!(
                    "Capture source switch failed: {}",
                    e
                )));
            }
        }
    }

    async fn run_capture_watchdog(&mut self) {
        let Some(watchdog) = self.pending_capture_watchdog.clone() else {
            return;
        };
        let surface_failure = self.current_session.is_some() && !self.is_paused;

        if self.capture_ctx.active_capture_app() != Some(watchdog.expected_app.as_str()) {
            debug!(
                "Skipping stale capture watchdog for '{}' (active target is {:?})",
                watchdog.expected_app,
                self.capture_ctx.active_capture_app()
            );
            self.clear_capture_watchdog();
            self.refresh_capture_enabled_from_frontmost();
            return;
        }

        match self.capture_ctx.active_source_is_ready() {
            Ok(true) => {
                if let Ok(Some((width, height))) = self.capture_ctx.active_source_dimensions() {
                    debug!(
                        "Active capture source for '{}' is ready at {}x{}",
                        watchdog.expected_app, width, height
                    );
                }
                self.clear_capture_watchdog();
                self.refresh_capture_enabled_from_frontmost();
            }
            Ok(false) => {
                if watchdog.attempt >= self.capture_watchdog_max_retries {
                    warn!(
                        "Active capture source for '{}' did not become ready after {} retry attempt(s); \
                         suppressing further retries until app switch",
                        watchdog.expected_app, watchdog.attempt
                    );
                    self.capture_watchdog_exhausted_app =
                        Some(watchdog.expected_app.clone());
                    self.clear_capture_watchdog();
                    if surface_failure {
                        self.send_status_force(EngineStatus::RecordingBlocked);
                    }
                    self.refresh_capture_enabled_from_frontmost();
                    return;
                }

                warn!(
                    "Active capture source for '{}' is not ready yet; refreshing (attempt {}/{})",
                    watchdog.expected_app,
                    watchdog.attempt + 1,
                    self.capture_watchdog_max_retries + 1
                );

                match self.capture_ctx.refresh_active_capture_source() {
                    Ok(_) => {
                        self.schedule_capture_watchdog(
                            &watchdog.expected_app,
                            watchdog.attempt + 1,
                        );
                        self.refresh_capture_enabled_from_frontmost();
                    }
                    Err(e) => {
                        self.clear_capture_watchdog();
                        error!(
                            "Failed to refresh active capture source for '{}': {}",
                            watchdog.expected_app, e
                        );
                        if surface_failure {
                            self.send_status_force(EngineStatus::Error(format!(
                                "Capture source refresh failed: {}",
                                e
                            )));
                        }
                        self.refresh_capture_enabled_from_frontmost();
                    }
                }
            }
            Err(e) => {
                self.clear_capture_watchdog();
                error!("Failed to inspect active capture source readiness: {}", e);
                if surface_failure {
                    self.send_status_force(EngineStatus::Error(format!(
                        "Capture source watchdog failed: {}",
                        e
                    )));
                }
                self.refresh_capture_enabled_from_frontmost();
            }
        }
    }

    fn buffered_input_event_count(&self) -> usize {
        self.buffered_non_context_event_count
    }

    fn buffer_input_event(&mut self, event: InputEvent) {
        self.event_buffer.push(event);
        self.buffered_non_context_event_count += 1;
    }

    fn clear_event_buffer(&mut self) {
        self.event_buffer.clear();
        self.buffered_non_context_event_count = 0;
    }

    fn drain_event_buffer(&mut self) -> Vec<InputEvent> {
        let events = self.event_buffer.drain();
        self.buffered_non_context_event_count = 0;
        events
    }

    fn current_context_app_id(&self, should_capture: bool) -> &str {
        match self.last_frontmost_app.as_deref() {
            Some(app_id) if should_capture => app_id,
            Some(_) => UNCAPTURED_APP_ID,
            None => UNKNOWN_APP_ID,
        }
    }

    fn current_capture_timestamp_us(&self) -> u64 {
        match self.recording_start_ns {
            Some(start_ns) => {
                self.capture_ctx
                    .get_video_frame_time()
                    .unwrap_or(start_ns)
                    .saturating_sub(start_ns)
                    / 1000
            }
            None => 0,
        }
    }

    fn push_context_event(&mut self, app_id: String, timestamp_us: u64) {
        self.event_buffer.push(InputEvent {
            timestamp_us,
            event: EventType::ContextChanged(ContextEvent {
                app_id: app_id.clone(),
            }),
        });
        self.last_emitted_context = Some(app_id);
    }

    fn emit_context_snapshot(&mut self, should_capture: bool, timestamp_us: u64) {
        let app_id = self.current_context_app_id(should_capture).to_string();
        self.push_context_event(app_id, timestamp_us);
    }

    fn maybe_emit_context_transition(&mut self, should_capture: bool) {
        if self.current_session.is_none() {
            return;
        }

        if self.last_emitted_context.as_deref() == Some(self.current_context_app_id(should_capture))
        {
            return;
        }

        let app_id = self.current_context_app_id(should_capture).to_string();
        self.push_context_event(app_id, self.current_capture_timestamp_us());
    }

    /// Spawn background task for uploading completed segments
    fn spawn_upload_task(
        mut upload_rx: mpsc::UnboundedReceiver<UploadMessage>,
        uploader: Uploader,
        delete_after_upload: bool,
        uploads_paused: Arc<AtomicBool>,
    ) {
        const BASE_RETRY_BACKOFF: Duration = Duration::from_secs(30);
        const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(2 * 60 * 60);
        const MAX_RETRY_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);
        const UPLOAD_PAUSE_NOTIFY_THRESHOLD: usize = 50;

        tokio::spawn(async move {
            let mut retry_queue: BinaryHeap<RetryEntry> = BinaryHeap::new();
            let mut sequence: u64 = 0;
            let mut active_session_id: Option<String> = None;
            let mut upload_pause_notified = false;

            fn jitter_multiplier(chunk_id: &str, attempts: u32) -> f64 {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                chunk_id.hash(&mut hasher);
                attempts.hash(&mut hasher);
                let hash = hasher.finish();
                let bucket = (hash % 401) as f64;
                0.8 + (bucket / 1000.0)
            }

            fn backoff_for_attempt(attempt: u32) -> Duration {
                let exp = 1u32
                    .checked_shl(attempt.saturating_sub(1))
                    .unwrap_or(u32::MAX);
                BASE_RETRY_BACKOFF
                    .checked_mul(exp)
                    .unwrap_or(MAX_RETRY_BACKOFF)
                    .min(MAX_RETRY_BACKOFF)
            }

            async fn upload_and_cleanup(
                uploader: &Uploader,
                segment: &CompletedSegment,
                delete_after_upload: bool,
            ) -> Result<()> {
                uploader.upload(&segment.chunk).await?;

                if delete_after_upload {
                    if let Some(ref video_path) = segment.chunk.video_path {
                        if let Err(e) = tokio::fs::remove_file(video_path).await {
                            warn!("Failed to delete video file {:?}: {}", video_path, e);
                        } else {
                            debug!("Deleted video file: {:?}", video_path);
                        }
                    }

                    if let Err(e) = tokio::fs::remove_file(&segment.input_path).await {
                        warn!(
                            "Failed to delete input file {:?}: {}",
                            segment.input_path, e
                        );
                    } else {
                        debug!("Deleted input file: {:?}", segment.input_path);
                    }
                }

                Ok(())
            }

            loop {
                let next_retry_at = retry_queue.peek().map(|entry| entry.next_attempt_at);

                tokio::select! {
                    Some(msg) = upload_rx.recv() => {
                        match msg {
                            UploadMessage::StartSession(session_id) => {
                                active_session_id = Some(session_id);
                            }
                            UploadMessage::Segment(segment) => {
                                let chunk_id = segment.chunk.chunk_id.clone();
                                let segment_session_id = segment.chunk.session_id.clone();
                                // Always accept segments — the upload buffer means
                                // segments from older sessions are delayed, not stale.
                                if active_session_id.as_ref() != Some(&segment_session_id) {
                                    active_session_id = Some(segment_session_id);
                                }

                                // If uploads are paused, queue the segment for later
                                if uploads_paused.load(AtomicOrdering::SeqCst) {
                                    info!("Uploads paused, queuing segment {} for later", chunk_id);
                                    let now = Instant::now();
                                    sequence = sequence.wrapping_add(1);
                                    retry_queue.push(RetryEntry {
                                        next_attempt_at: now,
                                        sequence,
                                        item: RetryItem {
                                            segment,
                                            attempts: 0,
                                            first_failed_at: now,
                                            next_attempt_at: now,
                                        },
                                    });
                                    if retry_queue.len() >= UPLOAD_PAUSE_NOTIFY_THRESHOLD && !upload_pause_notified {
                                        upload_pause_notified = true;
                                        warn!("{} segments waiting to upload. Resume uploads from the tray menu.", UPLOAD_PAUSE_NOTIFY_THRESHOLD);
                                        extern "C" {
                                            fn notifications_show_upload_queue_warning();
                                        }
                                        unsafe { notifications_show_upload_queue_warning(); }
                                    }
                                    continue;
                                }

                                upload_pause_notified = false;
                                info!("Background upload starting for segment {}", chunk_id);
                                match upload_and_cleanup(&uploader, &segment, delete_after_upload).await {
                                    Ok(()) => {
                                        info!("Successfully uploaded segment {}", chunk_id);
                                    }
                                    Err(e) => {
                                        error!("Failed to upload segment {}: {}", chunk_id, e);
                                        let now = Instant::now();
                                        let attempt = 1;
                                        let mut delay = backoff_for_attempt(attempt);
                                        delay = delay.mul_f64(jitter_multiplier(&chunk_id, attempt));
                                        if delay > MAX_RETRY_BACKOFF {
                                            delay = MAX_RETRY_BACKOFF;
                                        }
                                        let retry_item = RetryItem {
                                            segment,
                                            attempts: attempt,
                                            first_failed_at: now,
                                            next_attempt_at: now + delay,
                                        };
                                        sequence = sequence.wrapping_add(1);
                                        retry_queue.push(RetryEntry {
                                            next_attempt_at: retry_item.next_attempt_at,
                                            sequence,
                                            item: retry_item,
                                        });
                                    }
                                }
                            }
                        }
                    }

                    _ = async {
                        match next_retry_at {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        let now = Instant::now();
                        // Don't process retries while uploads are paused
                        if uploads_paused.load(AtomicOrdering::SeqCst) {
                            continue;
                        }
                        while retry_queue.peek().map(|entry| entry.next_attempt_at <= now).unwrap_or(false) {
                            let entry = retry_queue.pop().expect("retry queue peeked");
                            let mut item = entry.item;
                            let chunk_id = item.segment.chunk.chunk_id.clone();

                            if now.duration_since(item.first_failed_at) >= MAX_RETRY_WINDOW {
                                warn!(
                                    "Giving up on segment {} after {} attempts (retry window exceeded)",
                                    chunk_id, item.attempts
                                );
                                continue;
                            }

                            info!(
                                "Retrying upload for segment {} (attempt {})",
                                chunk_id,
                                item.attempts + 1
                            );

                            match upload_and_cleanup(&uploader, &item.segment, delete_after_upload).await {
                                Ok(()) => {
                                    info!("Successfully uploaded segment {}", chunk_id);
                                }
                                Err(e) => {
                                    error!("Retry failed for segment {}: {}", chunk_id, e);
                                    let attempt = item.attempts + 1;
                                    let mut delay = backoff_for_attempt(attempt);
                                    delay = delay.mul_f64(jitter_multiplier(&chunk_id, attempt));
                                    if delay > MAX_RETRY_BACKOFF {
                                        delay = MAX_RETRY_BACKOFF;
                                    }
                                    item.attempts = attempt;
                                    item.next_attempt_at = Instant::now() + delay;
                                    sequence = sequence.wrapping_add(1);
                                    retry_queue.push(RetryEntry {
                                        next_attempt_at: item.next_attempt_at,
                                        sequence,
                                        item,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    /// Run the engine main loop
    pub async fn run(&mut self) -> Result<()> {
        let session_id = self.config.session_id();
        info!("Sync engine starting for session: {}", session_id);

        // Spawn background upload task (must be done inside async context)
        if let Some(upload_rx) = self.upload_rx.take() {
            Self::spawn_upload_task(upload_rx, self.uploader.clone(), self.delete_after_upload, self.uploads_paused.clone());
        }

        // Take notification receiver for the main loop
        let mut notification_rx = self.notification_rx.take();

        // Ensure output directory exists
        std::fs::create_dir_all(&self.output_dir)?;

        // Start input capture (events go to a channel)
        let (input_tx, mut input_rx) = mpsc::unbounded_channel();
        self.input_backend.start(input_tx)?;

        // Main polling interval
        let poll_interval = Duration::from_millis(self.config.capture.poll_interval_ms);
        let mut poll_timer = tokio::time::interval(poll_interval);

        // Broadcast initial status
        self.send_status_force(EngineStatus::Idle);

        // Restore recording state from previous session, or fall back to
        // autostart_on_launch for fresh installs (no persisted state).
        let desired_state = read_recording_state().unwrap_or_else(|| {
            if self.config.recording.autostart_on_launch {
                PersistedRecordingState::Recording
            } else {
                PersistedRecordingState::Stopped
            }
        });

        match desired_state {
            PersistedRecordingState::Recording | PersistedRecordingState::Paused => {
                info!("Restoring recording state: {:?}", desired_state);
                if let Err(e) = self.start_recording().await {
                    error!("Failed to start recording: {}", e);
                    self.send_status_force(EngineStatus::Error(format!(
                        "Recording start failed: {}",
                        e
                    )));
                } else {
                    self.reset_segment_timer();
                    if desired_state == PersistedRecordingState::Paused {
                        self.pause_recording();
                    }
                }
            }
            PersistedRecordingState::Stopped => {
                info!("Recording state: stopped (not auto-starting)");
            }
        }

        loop {
            tokio::select! {
                // Handle commands
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        EngineCommand::StartRecording => {
                            if let Err(e) = self.start_recording().await {
                                error!("Failed to start recording: {}", e);
                                self.send_status_force(EngineStatus::Error(format!(
                                    "Start recording failed: {}",
                                    e
                                )));
                            } else {
                                write_recording_state(PersistedRecordingState::Recording);
                                self.reset_segment_timer();
                            }
                        }
                        EngineCommand::StopRecording => {
                            self.stop_recording().await?;
                            write_recording_state(PersistedRecordingState::Stopped);
                            self.reset_segment_timer();
                        }
                        EngineCommand::PauseRecording => {
                            let was_paused = self.is_paused;
                            self.pause_recording();
                            if !was_paused && self.is_paused {
                                write_recording_state(PersistedRecordingState::Paused);
                                self.reset_segment_timer();
                            }
                        }
                        EngineCommand::ResumeRecording => {
                            let was_paused = self.is_paused;
                            self.resume_recording();
                            if was_paused && !self.is_paused {
                                write_recording_state(PersistedRecordingState::Recording);
                                self.reset_segment_timer();
                            }
                        }
                        EngineCommand::PrepareForUpdate => {
                            info!("Preparing for update install");
                            self.stop_recording().await?;
                            self.reset_segment_timer();
                        }
                        EngineCommand::RefreshCaptureSource => {
                            if let Err(e) = self.capture_ctx.refresh_active_capture_source() {
                                error!("Failed to refresh active capture source: {}", e);
                                self.send_status_force(EngineStatus::Error(format!(
                                    "Capture source refresh failed: {}",
                                    e
                                )));
                            } else if let Some(app) =
                                self.capture_ctx.active_capture_app().map(|app| app.to_string())
                            {
                                self.schedule_capture_watchdog(&app, 0);
                                self.refresh_capture_enabled_from_frontmost();
                            } else {
                                self.clear_capture_watchdog();
                                self.refresh_capture_enabled_from_frontmost();
                            }
                        }
                        EngineCommand::ReloadTargetApps { target_apps, capture_all } => {
                            info!(
                                "Reloading target apps: capture_all={}, apps={:?}",
                                capture_all, target_apps
                            );
                            let was_recording = self.current_session.is_some();
                            if was_recording {
                                self.stop_recording().await?;
                            }
                            self.config.capture.target_apps = target_apps;
                            self.config.capture.capture_all = capture_all;
                            self.single_active_app_capture = !self.config.capture.capture_all
                                && !self.config.capture.target_apps.is_empty()
                                && cfg!(target_os = "macos")
                                && self.config.capture.single_active_app_capture;
                            self.capture_ctx
                                .set_single_active_app_capture(self.single_active_app_capture);
                            self.capture_ctx
                                .setup_capture(&self.config.capture.target_apps)?;
                            if was_recording {
                                self.start_recording().await?;
                            }
                        }
                        EngineCommand::PauseUploads => {
                            info!("Uploads paused");
                            self.uploads_paused.store(true, AtomicOrdering::SeqCst);
                            write_uploads_paused(true);
                        }
                        EngineCommand::ResumeUploads => {
                            info!("Uploads resumed");
                            self.uploads_paused.store(false, AtomicOrdering::SeqCst);
                            write_uploads_paused(false);
                        }
                        EngineCommand::Panic => {
                            warn!("PANIC: deleting recent recordings");
                            if self.current_session.is_some() {
                                let session = tokio::task::block_in_place(|| self.capture_ctx.stop_recording())?;
                                if let Some(session) = session {
                                    if let Err(e) = std::fs::remove_file(&session.output_path) {
                                        warn!("Failed to delete video {:?}: {}", session.output_path, e);
                                    }
                                    let seg_id = self.current_segment_id();
                                    let prefix = format!("input_{}", seg_id);
                                    if let Ok(entries) = std::fs::read_dir(&self.output_dir) {
                                        for entry in entries.flatten() {
                                            let name = entry.file_name();
                                            if name.to_string_lossy().starts_with(&prefix) {
                                                let _ = std::fs::remove_file(entry.path());
                                            }
                                        }
                                    }
                                }
                                self.current_session = None;
                                self.recording_start_ns = None;
                                self.segment_timer = None;
                                self.clear_event_buffer();
                            }
                            self.purge_upload_buffer();
                            write_recording_state(PersistedRecordingState::Recording);
                            if let Err(e) = self.start_recording().await {
                                error!("Failed to restart recording after panic: {}", e);
                            }
                        }
                        EngineCommand::SwitchToDisplay { display_id } => {
                            info!("User requested switch to display {}", display_id);
                            self.switch_to_display(display_id);
                        }
                        EngineCommand::Shutdown => {
                            info!("Shutdown command received");
                            self.stop_recording().await?;
                            self.flush_upload_buffer();
                            break;
                        }
                    }
                }

                // Handle notification actions (informational only - display switch is automatic)
                Some(action) = async {
                    match notification_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match action {
                        NotificationAction::Dismissed => {
                            debug!("User acknowledged display change notification");
                        }
                    }
                }

                // Handle input events
                Some(event) = input_rx.recv() => {
                    let was_paused = self.is_paused;
                    self.handle_input_event(event).await;
                    if was_paused && !self.is_paused {
                        self.reset_segment_timer();
                    }
                }

                // Poll frontmost app and check for display changes
                _ = poll_timer.tick() => {
                    self.poll_frontmost_app().await;
                    self.check_display_changes().await;
                    self.graduate_upload_buffer();
                }

                // Apply a queued app-driven capture source switch
                _ = async {
                    match &self.pending_app_switch {
                        Some(pending) => tokio::time::sleep_until(pending.scheduled_at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.apply_pending_app_switch().await;
                }

                // Handle segment rotation (if enabled)
                _ = async {
                    match self.segment_timer.as_mut() {
                        Some(timer) => timer.tick().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if self.current_session.is_some() && !self.is_paused {
                        info!("Segment duration reached, rotating to new segment...");
                        if let Err(e) = self.rotate_segment().await {
                            error!("Failed to rotate segment: {}", e);
                        }
                    } else if self.is_paused {
                        // Keep timer disarmed while paused so pause state is stable.
                        self.reset_segment_timer();
                    }
                }

                // Handle pending display refresh retries (for when SCK isn't ready immediately)
                _ = async {
                    match &self.pending_display_refresh {
                        Some(pending) => tokio::time::sleep_until(pending.next_retry_at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.retry_display_refresh().await;
                }

                // Verify that a newly switched active app source has started producing frames
                _ = async {
                    match &self.pending_capture_watchdog {
                        Some(pending) => tokio::time::sleep_until(pending.deadline).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.run_capture_watchdog().await;
                }

                // Handle idle timeout (recorded-action-gated capture)
                _ = async {
                    // Only run idle timer if:
                    // - Idle timeout is enabled (non-zero)
                    // - Not already paused (either manually or idle-paused)
                    // - Recording is active
                    if self.idle_timeout.is_zero() || self.is_paused || self.current_session.is_none() {
                        std::future::pending::<()>().await
                    } else {
                        let deadline = self.last_recorded_action_time + self.idle_timeout;
                        tokio::time::sleep_until(deadline).await
                    }
                } => {
                    let was_paused = self.is_paused;
                    self.handle_idle_timeout();
                    if !was_paused && self.is_paused {
                        self.reset_segment_timer();
                    }
                }
            }
        }

        info!("Sync engine stopped");
        Ok(())
    }

    /// Rotate to a new recording segment
    ///
    /// This stops the current recording, queues it for upload, and starts
    /// a new recording segment. Used for progressive upload to minimize
    /// storage overhead.
    async fn rotate_segment(&mut self) -> Result<()> {
        if self.current_session.is_none() {
            debug!("No recording in progress, skipping segment rotation");
            return Ok(());
        }
        if self.is_paused {
            debug!("Skipping segment rotation while paused");
            return Ok(());
        }

        let main_session_id = self
            .main_session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No main session ID set"))?;

        info!(
            "Rotating segment {} for session {}",
            self.segment_index, main_session_id
        );

        // Disable input capture during rotation to prevent events without corresponding video
        self.capture_enabled = false;

        // Flush current events and get video path
        let video_path = self.current_session.as_ref().map(|s| s.output_path.clone());
        let segment_id = self.current_segment_id();

        // Collect all events: partial flush files + remaining buffer
        let events = self.collect_segment_events(&segment_id).await?;
        let start_time_us = events.first().map(|e| e.timestamp_us).unwrap_or(0);
        let end_time_us = events.last().map(|e| e.timestamp_us).unwrap_or(0);

        // Save combined input events to disk
        let input_path = self
            .output_dir
            .join(format!("input_{}.msgpack", segment_id));
        let bytes = rmp_serde::to_vec(&events)?;
        tokio::fs::write(&input_path, bytes).await?;

        info!("Saved {} events to {:?}", events.len(), input_path);

        // Stop the current recording
        let _session = tokio::task::block_in_place(|| self.capture_ctx.stop_recording())?;

        // Create completed segment for upload
        let chunk = CompletedChunk {
            chunk_id: segment_id.clone(),
            session_id: main_session_id.clone(),
            events,
            video_path: video_path.clone(),
            start_time_us,
            end_time_us,
        };

        // Buffer for delayed upload (10-minute hold for panic button)
        let segment = CompletedSegment { chunk, input_path };
        self.buffer_segment_for_upload(segment, segment_id);

        // Clear recording state before starting new segment
        // This ensures we're in a consistent non-recording state if start fails
        self.current_session = None;
        self.recording_start_ns = None;

        let (frontmost_app, should_capture) = self.frontmost_capture_state();
        let desired_target = self.prepare_active_capture_target(
            frontmost_app.as_deref(),
            should_capture,
            "Failed to switch active capture source before segment rotation",
        )?;

        // Increment segment index
        self.segment_index += 1;

        // Start new recording segment
        let new_segment_id = self.current_segment_id();
        let session = match self.capture_ctx.start_recording(new_segment_id) {
            Ok(session) => session,
            Err(e) => {
                // Failed to start new segment - leave capture disabled and in non-recording state
                error!("Failed to start new segment after rotation: {}", e);
                self.main_session_id = None;
                self.segment_index = 0;
                self.send_status_force(EngineStatus::Error(format!(
                    "Segment rotation failed: {}",
                    e
                )));
                return Err(e);
            }
        };

        info!(
            "Started new segment {}: session={}, output={:?}",
            self.segment_index, session.session_id, session.output_path
        );

        self.recording_start_ns = Some(session.start_time_ns);
        self.current_session = Some(session);

        self.emit_context_snapshot(should_capture, 0);
        if let Some(app) = desired_target.as_deref() {
            self.schedule_capture_watchdog(app, 0);
        }
        self.update_capture_enabled(should_capture, desired_target.as_deref());
        if self.capture_enabled {
            self.send_status_force(EngineStatus::Capturing { event_count: 0 });
        } else {
            self.send_status_force(EngineStatus::RecordingBlocked);
        }

        Ok(())
    }

    /// Get the current segment ID (main_session_id + segment_index)
    fn current_segment_id(&self) -> String {
        match &self.main_session_id {
            Some(id) => format!("{}_seg{:04}", id, self.segment_index),
            None => format!("unknown_seg{:04}", self.segment_index),
        }
    }

    /// Collect all events for a segment, including partial flush files and buffer
    ///
    /// This reads any partial flush files for the segment, combines them with
    /// the remaining buffer events, and cleans up the partial files.
    async fn collect_segment_events(&mut self, segment_id: &str) -> Result<Vec<InputEvent>> {
        let mut all_events = Vec::new();

        // Find and read all partial flush files for this segment
        let partial_prefix = format!("input_{}_partial_", segment_id);
        let mut partial_files = Vec::new();

        if let Ok(mut entries) = tokio::fs::read_dir(&self.output_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let file_name = entry.file_name();
                let file_name_str = file_name.to_string_lossy();
                if file_name_str.starts_with(&partial_prefix) && file_name_str.ends_with(".msgpack")
                {
                    partial_files.push(entry.path());
                }
            }
        }

        // Sort partial files by name (which includes timestamp) to maintain order
        partial_files.sort();

        // Read and combine events from partial files
        for partial_path in &partial_files {
            match tokio::fs::read(partial_path).await {
                Ok(bytes) => match rmp_serde::from_slice::<Vec<InputEvent>>(&bytes) {
                    Ok(events) => {
                        debug!(
                            "Loaded {} events from partial file {:?}",
                            events.len(),
                            partial_path
                        );
                        all_events.extend(events);
                    }
                    Err(e) => {
                        warn!("Failed to parse partial file {:?}: {}", partial_path, e);
                    }
                },
                Err(e) => {
                    warn!("Failed to read partial file {:?}: {}", partial_path, e);
                }
            }
        }

        // Add remaining events from buffer
        let buffer_events = self.drain_event_buffer();
        debug!("Adding {} events from buffer", buffer_events.len());
        all_events.extend(buffer_events);

        let transition_events = self.drain_pending_transition_events_for_persistence();
        debug!(
            "Adding {} events from transition buffer",
            transition_events.len()
        );
        all_events.extend(transition_events);

        // Clean up partial files
        for partial_path in partial_files {
            if let Err(e) = tokio::fs::remove_file(&partial_path).await {
                warn!("Failed to delete partial file {:?}: {}", partial_path, e);
            }
        }

        // Sort all events by timestamp to ensure proper order
        all_events.sort_by_key(|e| e.timestamp_us);

        Ok(all_events)
    }

    /// Start recording
    async fn start_recording(&mut self) -> Result<()> {
        if self.current_session.is_some() {
            warn!("Recording already in progress");
            return Ok(());
        }

        let missing = describe_missing_permissions();
        if !missing.is_empty() {
            let details = missing.join(" ");
            let message = format!("Recording not started. {}", details);
            warn!("{}", message);
            self.send_status_force(EngineStatus::Error(message.clone()));
            if self.config.recording.notify_on_start_stop && notifications_authorized() {
                show_permissions_missing_notification(&message);
            }
            return Err(anyhow::anyhow!("{}", message));
        }

        info!("Starting recording...");

        // Ensure capture sources are set up
        if !self.capture_ctx.is_capture_setup() {
            self.capture_ctx
                .setup_capture(&self.config.capture.target_apps)?;
        }

        let (frontmost_app, should_capture) = self.frontmost_capture_state();
        let desired_target = self.prepare_active_capture_target(
            frontmost_app.as_deref(),
            should_capture,
            "Failed to initialize active capture source before recording start",
        )?;

        // Generate a main session ID (persists across all segments)
        let main_session_id = uuid::Uuid::new_v4().to_string();
        self.main_session_id = Some(main_session_id.clone());
        self.segment_index = 0;
        let _ = self
            .upload_tx
            .send(UploadMessage::StartSession(main_session_id.clone()));

        // Record the current display as the "original" display for recovery purposes
        let current_displays = self.display_monitor.current_display_ids();
        if let Some(&display_id) = current_displays.first() {
            if let Some(uuid) = get_display_uuid(display_id) {
                self.display_monitor.set_original_display(display_id, uuid);
            }
        }

        // Generate segment ID for first segment
        let segment_id = self.current_segment_id();

        // Start libobs recording with HEVC hardware encoding
        let session = self.capture_ctx.start_recording(segment_id)?;

        let segment_info = if self.config.recording.segment_duration_secs > 0 {
            format!(
                " (segmented, {}s per segment)",
                self.config.recording.segment_duration_secs
            )
        } else {
            String::new()
        };

        info!(
            "Recording started: main_session={}, segment={}, output={:?}{}",
            main_session_id, self.segment_index, session.output_path, segment_info
        );

        // Store the OBS timestamp for event synchronization
        self.recording_start_ns = Some(session.start_time_ns);
        self.current_session = Some(session);
        self.clear_event_buffer();
        self.clear_pending_input_transition();
        self.is_paused = false; // Ensure not paused when starting
        self.idle_paused = false; // Ensure not idle-paused when starting
        self.last_recorded_action_time = Instant::now(); // Reset recorded-action timer

        self.emit_context_snapshot(should_capture, 0);
        if let Some(app) = desired_target.as_deref() {
            self.schedule_capture_watchdog(app, 0);
        }
        self.update_capture_enabled(should_capture, desired_target.as_deref());
        if self.capture_enabled {
            self.send_status_force(EngineStatus::Capturing { event_count: 0 });
        } else {
            self.send_status_force(EngineStatus::RecordingBlocked);
        }

        self.reset_segment_timer();

        if self.config.recording.notify_on_start_stop && notifications_authorized() {
            show_recording_started_notification();
        }

        Ok(())
    }

    /// Stop recording
    async fn stop_recording(&mut self) -> Result<()> {
        if self.current_session.is_none() {
            debug!("No recording in progress");
            return Ok(());
        }

        info!("Stopping recording...");

        // Save any buffered events with final video path
        let video_path = self.current_session.as_ref().map(|s| s.output_path.clone());
        let segment_id = self.current_segment_id();

        // Collect all events: partial flush files + remaining buffer
        let events = self.collect_segment_events(&segment_id).await?;

        if !events.is_empty() || video_path.is_some() {
            let start_time_us = events.first().map(|e| e.timestamp_us).unwrap_or(0);
            let end_time_us = events.last().map(|e| e.timestamp_us).unwrap_or(0);

            // Save combined input events to disk
            let input_path = self
                .output_dir
                .join(format!("input_{}.msgpack", segment_id));
            let bytes = rmp_serde::to_vec(&events)?;
            tokio::fs::write(&input_path, bytes).await?;

            info!("Saved {} events to {:?}", events.len(), input_path);

            // Stop libobs recording - use block_in_place because libobs-wrapper
            // uses blocking_recv() internally which panics in async context
            let session = tokio::task::block_in_place(|| self.capture_ctx.stop_recording())?;
            if let Some(session) = session {
                info!(
                    "Recording stopped: session={}, output={:?}",
                    session.session_id, session.output_path
                );
            }

            // Queue final segment for upload
            if self.uploader.is_configured() {
                let main_session_id = self.main_session_id.clone().unwrap_or_default();
                let chunk = CompletedChunk {
                    chunk_id: segment_id.clone(),
                    session_id: main_session_id,
                    events,
                    video_path,
                    start_time_us,
                    end_time_us,
                };

                let segment = CompletedSegment { chunk, input_path };
                self.buffer_segment_for_upload(segment, segment_id);
            }
        } else {
            // Just stop recording without upload
            let session = tokio::task::block_in_place(|| self.capture_ctx.stop_recording())?;
            if let Some(session) = session {
                info!(
                    "Recording stopped: session={}, output={:?}",
                    session.session_id, session.output_path
                );
            }
        }

        self.current_session = None;
        self.recording_start_ns = None;
        self.main_session_id = None;
        self.segment_index = 0;
        self.is_paused = false;
        self.idle_paused = false;
        self.pending_app_switch = None;
        self.capture_watchdog_exhausted_app = None;
        self.segment_timer = None;
        self.clear_capture_watchdog();
        self.clear_pending_input_transition();
        self.last_emitted_context = None;

        // Clear the original display since we're no longer recording
        self.display_monitor.clear_original_display();

        self.send_status_force(EngineStatus::Idle);

        if self.config.recording.notify_on_start_stop && notifications_authorized() {
            show_recording_stopped_notification();
        }

        Ok(())
    }

    /// Pause recording (both video capture and keylog)
    ///
    /// Pauses the OBS video output and disables input event capture.
    fn pause_recording(&mut self) {
        if self.current_session.is_none() {
            warn!("Cannot pause - no recording in progress");
            return;
        }

        info!("Pausing recording (video and keylog)...");

        // Pause the video recording - use block_in_place because libobs-wrapper
        // uses blocking_recv() internally which panics in async context
        let result = tokio::task::block_in_place(|| self.capture_ctx.pause_recording());
        if let Err(e) = result {
            error!("Failed to pause video recording: {}", e);
            // Still mark as paused to prevent infinite retry loops —
            // the intent to pause should stick even if OBS is degraded.
        }

        self.is_paused = true;
        self.capture_enabled = false;

        self.send_status_force(EngineStatus::Paused);

        if self.config.recording.notify_on_start_stop && notifications_authorized() {
            show_recording_paused_notification();
        }

        info!("Recording paused");
    }

    /// Resume recording (both video capture and keylog)
    fn resume_recording(&mut self) {
        if self.current_session.is_none() {
            warn!("Cannot resume - no recording in progress");
            return;
        }

        if !self.is_paused {
            debug!("Recording not paused");
            return;
        }

        info!("Resuming recording (video and keylog)...");

        let (frontmost_app, should_capture) = self.frontmost_capture_state();
        let desired_target = match self.prepare_active_capture_target(
            frontmost_app.as_deref(),
            should_capture,
            "Failed to switch active capture source before resuming recording",
        ) {
            Ok(target) => target,
            Err(e) => {
                error!("{}", e);
                self.send_status_force(EngineStatus::Error(e.to_string()));
                return;
            }
        };

        // Resume the video recording - use block_in_place because libobs-wrapper
        // uses blocking_recv() internally which panics in async context
        let result = tokio::task::block_in_place(|| self.capture_ctx.resume_recording());
        if let Err(e) = result {
            error!("Failed to resume video recording: {}", e);
            return;
        }

        self.is_paused = false;
        self.emit_context_snapshot(should_capture, self.current_capture_timestamp_us());
        if let Some(app) = desired_target.as_deref() {
            self.schedule_capture_watchdog(app, 0);
        }
        self.update_capture_enabled(should_capture, desired_target.as_deref());

        if self.capture_enabled {
            self.send_status_force(EngineStatus::Capturing {
                event_count: self.buffered_input_event_count(),
            });
        } else {
            self.send_status_force(EngineStatus::RecordingBlocked);
        }

        if self.config.recording.notify_on_start_stop && notifications_authorized() {
            show_recording_resumed_notification();
        }

        info!("Recording resumed");
    }

    /// Switch to a specific display (called from notification action or command)
    fn switch_to_display(&mut self, display_id: u32) {
        // Update the original display to the new one
        if let Some(uuid) = get_display_uuid(display_id) {
            self.display_monitor.set_original_display(display_id, uuid);

            // Fully recreate sources for the new display (more reliable than in-place update)
            match self.capture_ctx.fully_recreate_sources() {
                Ok(count) => {
                    info!(
                        "Successfully switched to display {} ({} sources recreated)",
                        display_id, count
                    );
                    if let Some(app) = self
                        .capture_ctx
                        .active_capture_app()
                        .map(|app| app.to_string())
                    {
                        self.schedule_capture_watchdog(&app, 0);
                    } else {
                        self.clear_capture_watchdog();
                    }
                    self.refresh_capture_enabled_from_frontmost();
                }
                Err(e) => {
                    error!("Failed to switch to display {}: {}", display_id, e);
                }
            }
        } else {
            error!("Failed to get UUID for display {}", display_id);
        }
    }

    /// Check for display configuration changes and handle appropriately
    ///
    /// On macOS, when displays are disconnected and reconnected, ScreenCaptureKit
    /// caches stale display IDs. This method detects such changes and either:
    /// - Auto-recovers if the original display returns
    /// - Shows a notification to let the user decide if switching to a new display
    async fn check_display_changes(&mut self) {
        // Check if display configuration changed (macOS only, no-op on other platforms)
        let Some(event) = self.display_monitor.check_for_changes() else {
            return;
        };

        // Cancel any pending retry since display configuration changed again
        if self.pending_display_refresh.is_some() {
            debug!("Cancelling pending display refresh retry due to new display change");
            self.pending_display_refresh = None;
        }

        let restart_recording = self.current_session.is_some();

        match event {
            DisplayChangeEvent::OriginalReturned {
                display_id,
                uuid: _,
                display_name,
            } => {
                // Original display came back - auto-recover with light reinit
                // (same resolution expected, just recreate sources)
                info!(
                    "Original display '{}' (id={}) returned, auto-recovering...",
                    display_name, display_id
                );
                self.try_reinitialize_with_retry(
                    &display_name,
                    restart_recording,
                    true,
                    true,
                    false,
                )
                .await;
            }

            DisplayChangeEvent::SwitchedToNew {
                from_id,
                from_name,
                to_id,
                to_name,
                to_uuid: _,
            } => {
                // Switched to a different display - use reset_video() to update resolution
                // This is safe (uses obs_reset_video, doesn't drop context, no SIGABRT)
                info!(
                    "Display changed: '{}' (id={}) -> '{}' (id={})",
                    from_name, from_id, to_name, to_id
                );

                if restart_recording && notifications_authorized() {
                    show_display_change_notification(&from_name, &to_name, to_id);
                }

                // Use full_reinit: true to reset video resolution for new display
                self.try_reinitialize_with_retry(&to_name, restart_recording, false, true, true)
                    .await;
            }

            DisplayChangeEvent::AllDisconnected => {
                // All displays disconnected - just log and wait
                info!("All displays disconnected, waiting for reconnection...");
                // Don't spam notifications - just wait quietly
            }
        }
    }

    /// Reinitialize OBS and optionally restart recording for display changes
    ///
    /// When `full_reinit` is false, only recreates capture sources without dropping the
    /// libobs context. This is safer when the display resolution hasn't changed
    /// (e.g., when the original display returns after being disconnected).
    async fn reinitialize_after_display_change(
        &mut self,
        display_name: &str,
        restart_recording: bool,
        show_resumed_notification: bool,
        stop_recording_first: bool,
        full_reinit: bool,
    ) -> Result<()> {
        let mut restore_capture = None;
        if stop_recording_first {
            restore_capture = Some(self.capture_enabled);
            self.capture_enabled = false;

            if restart_recording {
                self.stop_recording().await?;
            }
        }

        if full_reinit {
            // Resolution may have changed - use reset_video() which is safe
            // (doesn't drop the context, avoids SIGABRT crash)
            self.capture_ctx.reset_video_and_recreate_sources()?;
        } else {
            // Light reinitialization - just recreate capture sources without
            // touching the libobs context or video settings. This is fastest
            // and safest when the same display returns at the same resolution.
            self.capture_ctx.fully_recreate_sources()?;
        }

        if restart_recording {
            self.start_recording().await?;
        }

        if show_resumed_notification && notifications_authorized() {
            show_capture_resumed_notification(display_name);
        }

        if let Some(prev) = restore_capture {
            if !restart_recording && self.current_session.is_some() {
                self.capture_enabled = prev;
            }
        }

        Ok(())
    }

    /// Try to reinitialize capture after a display change, scheduling a retry if it fails
    ///
    /// SCK sometimes isn't ready immediately after display changes (especially
    /// when coming out of clamshell mode). This method handles that by scheduling
    /// retries with exponential backoff.
    ///
    /// When `full_reinit` is false, only recreates capture sources (faster, safer).
    /// When `full_reinit` is true, drops and recreates the entire libobs context.
    async fn try_reinitialize_with_retry(
        &mut self,
        display_name: &str,
        restart_recording: bool,
        show_resumed_notification: bool,
        stop_recording_first: bool,
        full_reinit: bool,
    ) {
        match self
            .reinitialize_after_display_change(
                display_name,
                restart_recording,
                show_resumed_notification,
                stop_recording_first,
                full_reinit,
            )
            .await
        {
            Ok(()) => {
                info!(
                    "Successfully reinitialized capture for display '{}'",
                    display_name
                );
                self.pending_display_refresh = None;
            }
            Err(e) => {
                warn!(
                    "Failed to reinitialize capture for display '{}': {} (will retry)",
                    display_name, e
                );

                // Schedule a retry
                self.pending_display_refresh = Some(PendingDisplayRefresh {
                    display_name: display_name.to_string(),
                    attempt: 1,
                    next_retry_at: Instant::now() + DISPLAY_REFRESH_RETRY_BASE_DELAY,
                    restart_recording,
                    show_resumed_notification,
                    stop_recording_first,
                    full_reinit,
                });
            }
        }
    }

    /// Retry a pending display refresh
    async fn retry_display_refresh(&mut self) {
        let Some(pending) = self.pending_display_refresh.take() else {
            return;
        };

        info!(
            "Retrying display refresh for '{}' (attempt {}/{})",
            pending.display_name,
            pending.attempt + 1,
            MAX_DISPLAY_REFRESH_RETRIES
        );

        match self
            .reinitialize_after_display_change(
                &pending.display_name,
                pending.restart_recording,
                pending.show_resumed_notification,
                pending.stop_recording_first,
                pending.full_reinit,
            )
            .await
        {
            Ok(()) => {
                info!(
                    "Successfully reinitialized capture for display '{}' on retry",
                    pending.display_name
                );
                // Success - don't reschedule
            }
            Err(e) => {
                if pending.attempt >= MAX_DISPLAY_REFRESH_RETRIES {
                    error!(
                        "Failed to reinitialize capture for display '{}' after {} attempts: {}. Giving up.",
                        pending.display_name, MAX_DISPLAY_REFRESH_RETRIES, e
                    );
                    // Don't reschedule - we've exhausted retries
                } else {
                    // Schedule another retry with exponential backoff
                    let delay = DISPLAY_REFRESH_RETRY_BASE_DELAY * (1 << pending.attempt);
                    warn!(
                        "Retry failed for display '{}': {}. Will retry in {:?}",
                        pending.display_name, e, delay
                    );
                    self.pending_display_refresh = Some(PendingDisplayRefresh {
                        display_name: pending.display_name,
                        attempt: pending.attempt + 1,
                        next_retry_at: Instant::now() + delay,
                        restart_recording: pending.restart_recording,
                        show_resumed_notification: pending.show_resumed_notification,
                        stop_recording_first: pending.stop_recording_first,
                        full_reinit: pending.full_reinit,
                    });
                }
            }
        }
    }

    /// Handle idle timeout - pause recording when user is inactive
    ///
    /// Called when no input event has been recorded
    /// for the configured idle_timeout duration. Pauses both video recording
    /// and input capture to save resources and storage.
    fn handle_idle_timeout(&mut self) {
        // Don't pause if already paused or no recording in progress
        if self.current_session.is_none() || self.is_paused {
            return;
        }

        info!(
            "No recorded actions for {:?}, pausing capture...",
            self.idle_timeout
        );

        self.idle_paused = true;
        self.pause_recording();

        // Show notification if enabled
        if self.config.recording.notify_on_start_stop && notifications_authorized() {
            show_idle_paused_notification();
        }
    }

    /// Resume recording after idle-pause when user activity is detected
    ///
    /// Called when any user input is detected while in idle-paused state.
    /// Resumes video recording and input capture.
    fn resume_from_idle(&mut self) {
        if !self.idle_paused {
            return;
        }

        info!("User activity detected, resuming capture from idle...");

        self.resume_recording();
        if self.is_paused {
            return;
        }
        self.idle_paused = false;
        self.last_recorded_action_time = Instant::now();

        // Show notification if enabled
        if self.config.recording.notify_on_start_stop && notifications_authorized() {
            show_idle_resumed_notification();
        }
    }

    /// Poll the frontmost application and update capture state
    async fn poll_frontmost_app(&mut self) {
        // Keep app tracking fresh even while paused so state is accurate on resume.
        let (frontmost_app, should_capture) = self.frontmost_capture_state();
        let desired_target =
            self.desired_video_target_for_frontmost(frontmost_app.as_deref(), should_capture);
        if self.single_active_app_capture && self.current_session.is_some() {
            self.schedule_app_switch(desired_target.clone());
            self.apply_due_app_switch().await;
            self.rearm_capture_recovery_if_needed(should_capture, desired_target.as_deref());
        }

        // Don't update capture state if paused.
        if self.is_paused {
            return;
        }

        // Update capture state (only capture if recording, the app is allowed, and video is ready)
        let is_recording = self.current_session.is_some();
        if is_recording {
            self.maybe_emit_context_transition(should_capture);
        }
        self.update_capture_enabled(should_capture, desired_target.as_deref());

        // Update status
        if is_recording {
            if self.capture_enabled {
                self.send_status(EngineStatus::Capturing {
                    event_count: self.buffered_input_event_count(),
                });
            } else if !self.is_paused {
                self.send_status(EngineStatus::RecordingBlocked);
            }
            // If paused, don't change status - keep showing Paused
        }
    }

    /// Handle an input event
    async fn handle_input_event(&mut self, event: InputEvent) {
        let mut transition_target = None;

        // Auto-resume from idle only when frontmost app is capturable
        if self.idle_paused {
            let (should_capture, desired_target) =
                self.sync_single_active_capture_state_for_input().await;
            if should_capture {
                self.resume_from_idle();
                self.update_capture_enabled(should_capture, desired_target.as_deref());
                if self.should_buffer_transition_input(should_capture, desired_target.as_deref()) {
                    transition_target = desired_target;
                }
            } else {
                return;
            }
        } else if self.single_active_app_capture
            && self.current_session.is_some()
            && (self.pending_app_switch.is_some()
                || self.pending_capture_watchdog.is_some()
                || !self.capture_enabled
                || !matches!(&event.event, EventType::MouseMove(_)))
        {
            let (should_capture, desired_target) =
                self.sync_single_active_capture_state_for_input().await;
            if self.should_buffer_transition_input(should_capture, desired_target.as_deref()) {
                transition_target = desired_target;
            }
        }

        // Track user activity for idle detection when on a tracked app.
        // Uses should_capture (frontmost app is tracked) rather than capture_enabled
        // (which also requires source readiness). This way:
        // - On a tracked app with source not ready: timestamp updates, no spurious idle
        // - On an untracked app: timestamp goes stale, idle fires after timeout
        if self.current_session.is_some() && self.config.should_capture_app(
            self.last_frontmost_app.as_deref().unwrap_or(""),
        ) {
            self.last_recorded_action_time = Instant::now();
        }

        // Only buffer events if capture is enabled
        if !self.capture_enabled {
            if let Some(target_app) = transition_target.as_deref() {
                self.buffer_transition_input_event(target_app, event);
            }
            return;
        }

        let adjusted_event = self.adjust_input_event_timestamp(event);

        self.buffer_input_event(adjusted_event);

        // Check if buffer should be flushed (e.g., every N events or time interval)
        if self.event_buffer.len() >= 10000 {
            if let Err(e) = self.flush_event_buffer().await {
                error!("Failed to flush event buffer: {}", e);
            }
        }
    }

    /// Flush the event buffer to disk (for periodic flushing during long segments)
    ///
    /// This drains the buffer to bound memory usage. Events are saved to numbered
    /// partial files that will be combined with the main segment file at rotation.
    async fn flush_event_buffer(&mut self) -> Result<()> {
        if self.event_buffer.is_empty() {
            return Ok(());
        }

        // Generate a unique partial file name using timestamp to allow multiple flushes
        let segment_id = self.current_segment_id();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let flush_path = self.output_dir.join(format!(
            "input_{}_partial_{}.msgpack",
            segment_id, timestamp
        ));

        // Drain the buffer to bound memory usage
        let events = self.drain_event_buffer();
        let event_count = events.len();
        let bytes = rmp_serde::to_vec(&events)?;
        tokio::fs::write(&flush_path, bytes).await?;

        debug!(
            "Partial flush: {} events to {:?} (buffer cleared)",
            event_count, flush_path
        );

        Ok(())
    }
}

/// Create command and status channels for the engine
pub fn create_engine_channels() -> (
    mpsc::Sender<EngineCommand>,
    mpsc::Receiver<EngineCommand>,
    broadcast::Sender<EngineStatus>,
    broadcast::Receiver<EngineStatus>,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let (status_tx, status_rx) = broadcast::channel(16);
    (cmd_tx, cmd_rx, status_tx, status_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("crowd-cast-test-{}-{}", name, std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn make_test_segment(dir: &PathBuf, name: &str) -> CompletedSegment {
        let video_path = dir.join(format!("{}.mp4", name));
        let input_path = dir.join(format!("{}.msgpack", name));
        std::fs::write(&video_path, b"fake video").unwrap();
        std::fs::write(&input_path, b"fake input").unwrap();

        CompletedSegment {
            chunk: crate::data::CompletedChunk {
                session_id: "test-session".to_string(),
                chunk_id: name.to_string(),
                video_path: Some(video_path),
                events: vec![],
                start_time_us: 0,
                end_time_us: 1000,
            },
            input_path,
        }
    }

    #[test]
    fn purge_upload_buffer_deletes_files() {
        let dir = test_dir("purge");
        let seg1 = make_test_segment(&dir, "seg1");
        let seg2 = make_test_segment(&dir, "seg2");

        let video1 = seg1.chunk.video_path.clone().unwrap();
        let input1 = seg1.input_path.clone();
        let video2 = seg2.chunk.video_path.clone().unwrap();
        let input2 = seg2.input_path.clone();

        assert!(video1.exists());
        assert!(input1.exists());
        assert!(video2.exists());
        assert!(input2.exists());

        let mut buffer: std::collections::VecDeque<(Instant, CompletedSegment)> =
            std::collections::VecDeque::new();
        buffer.push_back((Instant::now(), seg1));
        buffer.push_back((Instant::now(), seg2));

        // Purge
        while let Some((_, segment)) = buffer.pop_front() {
            if let Some(ref video_path) = segment.chunk.video_path {
                let _ = std::fs::remove_file(video_path);
            }
            let _ = std::fs::remove_file(&segment.input_path);
        }

        assert!(!video1.exists());
        assert!(!input1.exists());
        assert!(!video2.exists());
        assert!(!input2.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upload_pause_persistence_roundtrip() {
        let dir = test_dir("pause-persist");
        let path = dir.join("uploads_paused");

        assert!(!path.exists());

        std::fs::write(&path, "true").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "true");

        std::fs::write(&path, "false").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim() == "true",
            false
        );

        std::fs::write(&path, "true").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim() == "true",
            true
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn graduate_buffer_respects_delay() {
        let dir = test_dir("graduate");
        let seg_old = make_test_segment(&dir, "old");
        let seg_new = make_test_segment(&dir, "new");

        let mut buffer: std::collections::VecDeque<(Instant, CompletedSegment)> =
            std::collections::VecDeque::new();

        // Old segment: created 20 minutes ago
        buffer.push_back((Instant::now() - Duration::from_secs(1200), seg_old));
        // New segment: created just now
        buffer.push_back((Instant::now(), seg_new));

        let delay = Duration::from_secs(600);
        let now = Instant::now();
        let mut graduated = vec![];

        while let Some((created_at, _)) = buffer.front() {
            if now.duration_since(*created_at) >= delay {
                let (_, segment) = buffer.pop_front().unwrap();
                graduated.push(segment.chunk.chunk_id.clone());
            } else {
                break;
            }
        }

        assert_eq!(graduated.len(), 1);
        assert_eq!(graduated[0], "old");
        assert_eq!(buffer.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
