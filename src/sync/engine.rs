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
use std::time::Duration;
use tokio::time::Instant;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::capture::{
    get_frontmost_app, CaptureContext, DisplayChangeEvent, DisplayMonitor, RecordingSession,
    get_display_uuid,
};
use crate::config::Config;
use crate::data::{CompletedChunk, InputEvent, InputEventBuffer};
use crate::input::{create_input_backend, InputBackend};
use crate::ui::notifications::{
    init_notifications, show_capture_resumed_notification, show_display_change_notification,
    NotificationAction,
};
use crate::upload::Uploader;

use super::{EngineCommand, EngineStatus};

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
    /// Uploader instance
    uploader: Uploader,
    /// Segment duration in seconds (cached from config)
    segment_duration_secs: u64,
    /// Whether to delete files after upload
    delete_after_upload: bool,
    /// Upload receiver (taken once when run() starts)
    upload_rx: Option<mpsc::UnboundedReceiver<UploadMessage>>,
    /// Notification action receiver (taken once when run() starts)
    notification_rx: Option<mpsc::UnboundedReceiver<NotificationAction>>,
}

impl SyncEngine {
    /// Create a new sync engine
    pub fn new(
        config: Config,
        capture_ctx: CaptureContext,
        cmd_rx: mpsc::Receiver<EngineCommand>,
        status_tx: broadcast::Sender<EngineStatus>,
    ) -> Self {
        let output_dir = config.recording.output_directory
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("crowd-cast-recordings"));

        let (upload_tx, upload_rx) = mpsc::unbounded_channel();
        let uploader = Uploader::new(&config);
        let segment_duration_secs = config.recording.segment_duration_secs;
        let delete_after_upload = config.upload.delete_after_upload;

        // Create notification action channel
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();
        
        // Initialize notifications (best effort - non-fatal if it fails)
        if let Err(e) = init_notifications(notification_tx) {
            warn!("Failed to initialize notifications: {}. Display change alerts will not be shown.", e);
        }

        Self {
            config,
            capture_ctx,
            input_backend: create_input_backend(),
            cmd_rx,
            status_tx,
            event_buffer: InputEventBuffer::new(),
            capture_enabled: false,
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
            upload_rx: Some(upload_rx),
            notification_rx: Some(notification_rx),
        }
    }

    /// Spawn background task for uploading completed segments
    fn spawn_upload_task(
        mut upload_rx: mpsc::UnboundedReceiver<UploadMessage>,
        uploader: Uploader,
        delete_after_upload: bool,
    ) {
        const BASE_RETRY_BACKOFF: Duration = Duration::from_secs(30);
        const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(2 * 60 * 60);
        const MAX_RETRY_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);

        tokio::spawn(async move {
            let mut retry_queue: BinaryHeap<RetryEntry> = BinaryHeap::new();
            let mut sequence: u64 = 0;
            let mut active_session_id: Option<String> = None;

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
                        warn!("Failed to delete input file {:?}: {}", segment.input_path, e);
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
                                if active_session_id.as_ref() != Some(&session_id) {
                                    if !retry_queue.is_empty() {
                                        warn!(
                                            "Clearing {} queued retries due to new session {}",
                                            retry_queue.len(),
                                            session_id
                                        );
                                    }
                                    retry_queue.clear();
                                }
                                active_session_id = Some(session_id);
                            }
                            UploadMessage::Segment(segment) => {
                                let chunk_id = segment.chunk.chunk_id.clone();
                                let segment_session_id = segment.chunk.session_id.clone();
                                if let Some(active) = active_session_id.as_ref() {
                                    if active != &segment_session_id {
                                        warn!(
                                            "Dropping segment {} from session {} (active session {})",
                                            chunk_id, segment_session_id, active
                                        );
                                        continue;
                                    }
                                } else {
                                    active_session_id = Some(segment_session_id);
                                }

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
                        while retry_queue.peek().map(|entry| entry.next_attempt_at <= now).unwrap_or(false) {
                            let entry = retry_queue.pop().expect("retry queue peeked");
                            let mut item = entry.item;
                            let chunk_id = item.segment.chunk.chunk_id.clone();

                            if let Some(active) = active_session_id.as_ref() {
                                if active != &item.segment.chunk.session_id {
                                    warn!(
                                        "Dropping retry for segment {} from session {} (active session {})",
                                        chunk_id,
                                        item.segment.chunk.session_id,
                                        active
                                    );
                                    continue;
                                }
                            }

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
            Self::spawn_upload_task(upload_rx, self.uploader.clone(), self.delete_after_upload);
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

        // Segment rotation timer (created when recording starts, not at engine start)
        // This ensures the first segment is the full duration
        let mut segment_timer: Option<tokio::time::Interval> = None;

        // Broadcast initial status
        let _ = self.status_tx.send(EngineStatus::Idle);

        if self.config.recording.autostart_on_launch {
            info!("Autostart recording on launch enabled");
            if let Err(e) = self.start_recording().await {
                error!("Failed to autostart recording: {}", e);
                let _ = self
                    .status_tx
                    .send(EngineStatus::Error("Autostart recording failed".to_string()));
            } else {
                // Initialize segment timer after successful autostart
                // Use interval_at to delay first tick (interval() ticks immediately)
                if self.segment_duration_secs > 0 {
                    let duration = Duration::from_secs(self.segment_duration_secs);
                    segment_timer = Some(tokio::time::interval_at(Instant::now() + duration, duration));
                }
            }
        }

        loop {
            tokio::select! {
                // Handle commands
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        EngineCommand::StartRecording => {
                            self.start_recording().await?;
                            // Reset segment timer when recording starts to ensure full-length first segment
                            // Use interval_at to delay first tick (interval() ticks immediately)
                            if self.segment_duration_secs > 0 {
                                let duration = Duration::from_secs(self.segment_duration_secs);
                                segment_timer = Some(tokio::time::interval_at(Instant::now() + duration, duration));
                            }
                        }
                        EngineCommand::StopRecording => {
                            self.stop_recording().await?;
                            segment_timer = None;
                        }
                        EngineCommand::SetCaptureEnabled(enabled) => {
                            info!("Manual capture override: {}", enabled);
                            self.capture_enabled = enabled;
                        }
                        EngineCommand::SwitchToDisplay { display_id } => {
                            info!("User requested switch to display {}", display_id);
                            self.switch_to_display(display_id);
                        }
                        EngineCommand::Shutdown => {
                            info!("Shutdown command received");
                            self.stop_recording().await?;
                            break;
                        }
                    }
                }

                // Handle notification actions (user clicked on notification button)
                Some(action) = async {
                    match notification_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match action {
                        NotificationAction::SwitchToDisplay { display_id } => {
                            info!("Notification action: switch to display {}", display_id);
                            self.switch_to_display(display_id);
                        }
                        NotificationAction::Dismissed => {
                            debug!("User dismissed display change notification");
                        }
                    }
                }

                // Handle input events
                Some(event) = input_rx.recv() => {
                    self.handle_input_event(event).await;
                }

                // Poll frontmost app and check for display changes
                _ = poll_timer.tick() => {
                    self.poll_frontmost_app().await;
                    self.check_display_changes();
                }

                // Handle segment rotation (if enabled)
                _ = async {
                    match segment_timer.as_mut() {
                        Some(timer) => timer.tick().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if self.current_session.is_some() {
                        info!("Segment duration reached, rotating to new segment...");
                        if let Err(e) = self.rotate_segment().await {
                            error!("Failed to rotate segment: {}", e);
                        }
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

        let main_session_id = self.main_session_id.clone()
            .ok_or_else(|| anyhow::anyhow!("No main session ID set"))?;

        info!(
            "Rotating segment {} for session {}",
            self.segment_index, main_session_id
        );

        // Disable input capture during rotation to prevent events without corresponding video
        let was_capturing = self.capture_enabled;
        self.capture_enabled = false;

        // Flush current events and get video path
        let video_path = self.current_session.as_ref().map(|s| s.output_path.clone());
        let segment_id = self.current_segment_id();

        // Collect all events: partial flush files + remaining buffer
        let events = self.collect_segment_events(&segment_id).await?;
        let start_time_us = events.first().map(|e| e.timestamp_us).unwrap_or(0);
        let end_time_us = events.last().map(|e| e.timestamp_us).unwrap_or(0);

        // Save combined input events to disk
        let input_path = self.output_dir.join(format!("input_{}.msgpack", segment_id));
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

        // Queue for background upload (if uploader is configured)
        if self.uploader.is_configured() {
            let segment = CompletedSegment {
                chunk,
                input_path,
            };

            if let Err(e) = self.upload_tx.send(UploadMessage::Segment(segment)) {
                error!("Failed to queue segment for upload: {}", e);
            } else {
                let _ = self.status_tx.send(EngineStatus::Uploading { chunk_id: segment_id });
            }
        }

        // Clear recording state before starting new segment
        // This ensures we're in a consistent non-recording state if start fails
        self.current_session = None;
        self.recording_start_ns = None;

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
                let _ = self.status_tx.send(EngineStatus::Error(
                    format!("Segment rotation failed: {}", e)
                ));
                return Err(e);
            }
        };

        info!(
            "Started new segment {}: session={}, output={:?}",
            self.segment_index, session.session_id, session.output_path
        );

        self.recording_start_ns = Some(session.start_time_ns);
        self.current_session = Some(session);

        // Re-enable input capture if it was enabled before rotation
        self.capture_enabled = was_capturing;

        let _ = self.status_tx.send(EngineStatus::Capturing {
            event_count: 0,
        });

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
                if file_name_str.starts_with(&partial_prefix) && file_name_str.ends_with(".msgpack") {
                    partial_files.push(entry.path());
                }
            }
        }

        // Sort partial files by name (which includes timestamp) to maintain order
        partial_files.sort();

        // Read and combine events from partial files
        for partial_path in &partial_files {
            match tokio::fs::read(partial_path).await {
                Ok(bytes) => {
                    match rmp_serde::from_slice::<Vec<InputEvent>>(&bytes) {
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
                    }
                }
                Err(e) => {
                    warn!("Failed to read partial file {:?}: {}", partial_path, e);
                }
            }
        }

        // Add remaining events from buffer
        let buffer_events = self.event_buffer.drain();
        debug!("Adding {} events from buffer", buffer_events.len());
        all_events.extend(buffer_events);

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

        info!("Starting recording...");

        // Ensure capture sources are set up
        if !self.capture_ctx.is_capture_setup() {
            self.capture_ctx.setup_capture(&self.config.capture.target_apps)?;
        }

        // Generate a main session ID (persists across all segments)
        let main_session_id = uuid::Uuid::new_v4().to_string();
        self.main_session_id = Some(main_session_id.clone());
        self.segment_index = 0;
        let _ = self.upload_tx.send(UploadMessage::StartSession(main_session_id.clone()));

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
        self.event_buffer.clear();

        let _ = self.status_tx.send(EngineStatus::Capturing { event_count: 0 });

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
            let input_path = self.output_dir.join(format!("input_{}.msgpack", segment_id));
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

                let segment = CompletedSegment {
                    chunk,
                    input_path,
                };

                if let Err(e) = self.upload_tx.send(UploadMessage::Segment(segment)) {
                    error!("Failed to queue final segment for upload: {}", e);
                } else {
                    let _ = self.status_tx.send(EngineStatus::Uploading { chunk_id: segment_id });
                }
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
        
        // Clear the original display since we're no longer recording
        self.display_monitor.clear_original_display();
        
        let _ = self.status_tx.send(EngineStatus::Idle);

        Ok(())
    }

    /// Switch to a specific display (called from notification action or command)
    fn switch_to_display(&mut self, display_id: u32) {
        // Update the original display to the new one
        if let Some(uuid) = get_display_uuid(display_id) {
            self.display_monitor.set_original_display(display_id, uuid);
            
            // Recreate sources for the new display
            match self.capture_ctx.recreate_sources() {
                Ok(count) => {
                    info!(
                        "Successfully switched to display {} ({} sources updated)",
                        display_id, count
                    );
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
    fn check_display_changes(&mut self) {
        // Check if display configuration changed (macOS only, no-op on other platforms)
        let Some(event) = self.display_monitor.check_for_changes() else {
            return;
        };

        match event {
            DisplayChangeEvent::OriginalReturned { display_id, uuid, display_name } => {
                // Original display came back - auto-recover
                info!("Original display '{}' (id={}) returned, auto-recovering...", display_name, display_id);
                
                match self.capture_ctx.recreate_sources() {
                    Ok(count) => {
                        info!(
                            "Successfully recovered {} capture source(s) after display return",
                            count
                        );
                        // Show a notification that capture resumed
                        show_capture_resumed_notification(&display_name);
                    }
                    Err(e) => {
                        error!("Failed to recover capture sources: {}", e);
                        // User might reconnect the display again
                    }
                }
            }
            
            DisplayChangeEvent::SwitchedToNew { from_id, from_name, to_id, to_name, to_uuid } => {
                // Switched to a different display - show notification to let user decide
                info!(
                    "Display changed: '{}' (id={}) -> '{}' (id={})",
                    from_name, from_id, to_name, to_id
                );
                
                // Don't auto-switch - show notification with action buttons
                show_display_change_notification(&from_name, &to_name, to_id);
                
                // Note: capture may be broken until user clicks "Switch" or original returns
            }
            
            DisplayChangeEvent::AllDisconnected => {
                // All displays disconnected - just log and wait
                info!("All displays disconnected, waiting for reconnection...");
                // Don't spam notifications - just wait quietly
            }
        }
    }

    /// Poll the frontmost application and update capture state
    async fn poll_frontmost_app(&mut self) {
        let frontmost = get_frontmost_app();

        let bundle_id = frontmost.as_ref().map(|a| a.bundle_id.as_str());
        let should_capture = match bundle_id {
            Some(id) => self.config.should_capture_app(id),
            None => {
                // Can't detect frontmost app (e.g., Wayland)
                // Fall back to capture_all setting
                self.config.capture.capture_all
            }
        };

        // Log state changes
        let new_bundle_id = bundle_id.map(|s| s.to_string());
        if new_bundle_id != self.last_frontmost_app {
            if let Some(ref id) = new_bundle_id {
                debug!(
                    "Frontmost app changed: {} (capture: {})",
                    id, should_capture
                );
            }
            self.last_frontmost_app = new_bundle_id;
        }

        // Update capture state (only capture if recording AND app is allowed)
        let is_recording = self.current_session.is_some();
        let was_capturing = self.capture_enabled;
        self.capture_enabled = should_capture && is_recording;

        if self.capture_enabled != was_capturing {
            if self.capture_enabled {
                debug!("Input capture enabled");
            } else {
                debug!("Input capture disabled");
            }
        }

        // Update status
        if is_recording {
            if self.capture_enabled {
                let _ = self.status_tx.send(EngineStatus::Capturing {
                    event_count: self.event_buffer.len(),
                });
            } else {
                let _ = self.status_tx.send(EngineStatus::RecordingBlocked);
            }
        }
    }

    /// Handle an input event
    async fn handle_input_event(&mut self, event: InputEvent) {
        // Only buffer events if capture is enabled
        if !self.capture_enabled {
            return;
        }

        // Adjust timestamp relative to OBS recording start for video sync
        // Convert from system microseconds to OBS-relative microseconds
        let adjusted_event = if let Some(start_ns) = self.recording_start_ns {
            // Get current OBS timestamp and compute relative offset
            let current_ns = self.capture_ctx.get_video_frame_time().unwrap_or(0);
            let elapsed_us = current_ns.saturating_sub(start_ns) / 1000;

            InputEvent {
                timestamp_us: elapsed_us,
                ..event
            }
        } else {
            event
        };

        self.event_buffer.push(adjusted_event);

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
        let events = self.event_buffer.drain();
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
