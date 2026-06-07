//! System tray application
//!
//! Business logic for the tray UI: engine command dispatch, updater scheduling,
//! auth flow, settings panel. Platform-specific rendering is handled by
//! implementations of `PlatformTray`.

use anyhow::Result;
use image::imageops::FilterType;
use image::RgbaImage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use super::platform_tray::{
    PlatformTray, PlatformTrayPoll, TrayAction, TrayDisplayState, TrayIconPaths, TrayIconState,
};
use super::UpdaterController;
use crate::sync::{EngineCommand, EngineStatus};

// ---------------------------------------------------------------------------
// Globals shared with main.rs
// ---------------------------------------------------------------------------

/// Set when the user explicitly quits via the tray menu.
/// Read by `main.rs` after the tray loop exits to decide the process exit code.
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Set by the background sign-in thread when OAuth completes.
/// Read by the tray loop to refresh the auth display.
static SIGN_IN_COMPLETED: AtomicBool = AtomicBool::new(false);

/// Check if the user explicitly quit via the tray menu.
pub fn was_quit_requested() -> bool {
    QUIT_REQUESTED.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// Prepare-for-update logic (pure business logic, no platform dependency)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrepareForUpdateAction {
    Wait,
    SendCommand,
    ClearRequest,
}

fn status_blocks_immediate_update(status: &EngineStatus) -> bool {
    matches!(
        status,
        EngineStatus::Capturing { .. }
            | EngineStatus::RecordingBlocked
            | EngineStatus::Uploading { .. }
    )
}

fn status_needs_prepare_for_update(status: &EngineStatus) -> bool {
    matches!(
        status,
        EngineStatus::Capturing { .. } | EngineStatus::Paused | EngineStatus::RecordingBlocked
    )
}

fn next_prepare_for_update_action(
    request_pending: bool,
    last_status: Option<&EngineStatus>,
) -> PrepareForUpdateAction {
    if !request_pending {
        return PrepareForUpdateAction::Wait;
    }

    match last_status {
        Some(status) if status_needs_prepare_for_update(status) => {
            PrepareForUpdateAction::SendCommand
        }
        Some(_) => PrepareForUpdateAction::ClearRequest,
        None => PrepareForUpdateAction::Wait,
    }
}

// ---------------------------------------------------------------------------
// Auth display helper
// ---------------------------------------------------------------------------

/// Compute the display strings for the auth section of the tray menu.
/// Returns (account_text, sign_action_text, auth_configured).
fn compute_auth_display(
    auth: &Option<std::sync::Arc<tokio::sync::Mutex<crate::auth::AuthManager>>>,
) -> (String, String, bool) {
    if let Some(ref auth) = auth {
        if let Ok(mgr) = auth.try_lock() {
            if let Some(email) = mgr.email() {
                return (
                    format!("Signed in as {}", email),
                    "Sign out".to_string(),
                    true,
                );
            }
            return (String::new(), "Sign in with Google".to_string(), true);
        }
        return (String::new(), "Sign in with Google".to_string(), true);
    }
    (
        String::new(),
        "Sign in (not configured)".to_string(),
        false,
    )
}

// ---------------------------------------------------------------------------
// TrayApp
// ---------------------------------------------------------------------------

/// System tray application — owns a platform tray and drives the event loop.
pub struct TrayApp {
    cmd_tx: mpsc::Sender<EngineCommand>,
    status_rx: broadcast::Receiver<EngineStatus>,
    platform_tray: Box<dyn PlatformTray>,
    updater: UpdaterController,
    last_updater_can_check: Option<bool>,
    last_status: Option<EngineStatus>,
    pending_prepare_for_update: bool,
    last_update_check: std::time::Instant,
    uploads_paused: bool,
    auth: Option<std::sync::Arc<tokio::sync::Mutex<crate::auth::AuthManager>>>,
    auth_runtime: Option<std::sync::Arc<tokio::runtime::Runtime>>,
    // Cached display state for auth
    account_display_text: String,
    sign_action_display_text: String,
    auth_configured: bool,
}

impl TrayApp {
    /// Create a new tray application with channels for engine communication
    pub fn new(
        cmd_tx: mpsc::Sender<EngineCommand>,
        status_rx: broadcast::Receiver<EngineStatus>,
        auth: Option<std::sync::Arc<tokio::sync::Mutex<crate::auth::AuthManager>>>,
        auth_runtime: Option<std::sync::Arc<tokio::runtime::Runtime>>,
    ) -> Result<Self> {
        info!("Initializing system tray UI");

        let icon_paths = get_icon_paths()?;
        let platform_tray = create_platform_tray(&icon_paths)?;
        let updater = UpdaterController::new();

        // Compute initial auth display state
        let (account_display_text, sign_action_display_text, auth_configured) =
            compute_auth_display(&auth);

        let uploads_paused = directories::ProjectDirs::from("dev", "crowd-cast", "agent")
            .and_then(|p| std::fs::read_to_string(p.data_dir().join("uploads_paused")).ok())
            .map(|s| s.trim() == "true")
            .unwrap_or(false);

        info!("System tray created");

        Ok(Self {
            cmd_tx,
            status_rx,
            platform_tray,
            updater,
            last_updater_can_check: None,
            last_status: None,
            pending_prepare_for_update: false,
            last_update_check: std::time::Instant::now(),
            uploads_paused,
            auth,
            auth_runtime,
            account_display_text,
            sign_action_display_text,
            auth_configured,
        })
    }

    /// Compute the full display state from current TrayApp state.
    fn compute_display_state(&self) -> TrayDisplayState {
        let (status_text, icon_state, can_start, can_stop) = match &self.last_status {
            Some(EngineStatus::Idle) => (
                "Status: Idle".to_string(),
                TrayIconState::Idle,
                true,
                false,
            ),
            Some(EngineStatus::Capturing { event_count }) => (
                format!("Status: Capturing ({} events)", event_count),
                TrayIconState::Recording,
                false,
                true,
            ),
            Some(EngineStatus::Paused) => (
                "Status: Idle (paused)".to_string(),
                TrayIconState::Idle,
                false,
                true,
            ),
            Some(EngineStatus::RecordingBlocked) => (
                "Status: Recording (no capture sources)".to_string(),
                TrayIconState::Blocked,
                false,
                true,
            ),
            Some(EngineStatus::WaitingForOBS) => (
                "Status: Waiting for OBS...".to_string(),
                TrayIconState::Blocked,
                true,
                false,
            ),
            Some(EngineStatus::Uploading { chunk_id }) => (
                format!("Status: Uploading {}", chunk_id),
                TrayIconState::Idle,
                true,
                false,
            ),
            Some(EngineStatus::Error(msg)) => (
                format!("Status: Error - {}", truncate_str(msg, 30)),
                TrayIconState::Idle,
                true,
                false,
            ),
            None => (
                "Status: Idle".to_string(),
                TrayIconState::Idle,
                true,
                false,
            ),
        };

        TrayDisplayState {
            icon_state,
            status_text,
            account_text: self.account_display_text.clone(),
            sign_action_text: self.sign_action_display_text.clone(),
            auth_action_enabled: self.auth_configured,
            can_start,
            can_stop,
            uploads_text: if self.uploads_paused {
                "Resume Uploads".to_string()
            } else {
                "Pause Uploads".to_string()
            },
            can_check_updates: self.updater.can_check_for_updates(),
        }
    }

    /// Push current display state to the platform tray.
    fn refresh_display(&mut self) {
        let state = self.compute_display_state();
        self.platform_tray.update(&state);
    }

    /// Initialize and run the tray application event loop (blocks until quit)
    pub fn run(mut self) -> Result<()> {
        info!("Starting system tray event loop");

        self.platform_tray.init()?;

        QUIT_REQUESTED.store(false, Ordering::SeqCst);
        SIGN_IN_COMPLETED.store(false, Ordering::SeqCst);

        self.updater.start();
        if let Some(reason) = self.updater.reason() {
            info!("Updater unavailable: {}", reason);
        }

        // Push initial display state (includes uploads_paused, auth, updater)
        self.refresh_display();

        loop {
            // Check for engine status updates (non-blocking)
            match self.status_rx.try_recv() {
                Ok(status) => {
                    self.update_status(&status);
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    warn!("Missed {} status updates", n);
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    // No updates, that's fine
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    info!("Status channel closed, exiting tray");
                    break;
                }
            }

            // Run one iteration of the platform event loop
            match self.platform_tray.poll() {
                PlatformTrayPoll::None => {}

                PlatformTrayPoll::Exit => {
                    info!("Tray loop signaled exit");
                    break;
                }

                PlatformTrayPoll::RequestRestart => {
                    self.platform_tray.prepare_for_restart();
                    let _ = self.cmd_tx.try_send(EngineCommand::RestartProcess);
                    break;
                }

                PlatformTrayPoll::Action(action) => match action {
                    TrayAction::Quit => {
                        info!("Quit requested via tray menu");
                        QUIT_REQUESTED.store(true, Ordering::SeqCst);
                        let _ = self.cmd_tx.try_send(EngineCommand::Shutdown);
                        break;
                    }
                    TrayAction::StartRecording => {
                        info!("Start recording requested via tray");
                        if let Err(e) = self.cmd_tx.try_send(EngineCommand::StartRecording) {
                            error!("Failed to send start recording command: {}", e);
                        }
                    }
                    TrayAction::StopRecording => {
                        info!("Stop recording requested via tray");
                        if let Err(e) = self.cmd_tx.try_send(EngineCommand::StopRecording) {
                            error!("Failed to send stop recording command: {}", e);
                        }
                    }
                    TrayAction::Panic => {
                        warn!("Panic button pressed via tray");
                        if let Err(e) = self.cmd_tx.try_send(EngineCommand::Panic) {
                            error!("Failed to send panic command: {}", e);
                        }
                    }
                    TrayAction::ToggleUploads => {
                        self.uploads_paused = !self.uploads_paused;
                        if self.uploads_paused {
                            info!("Uploads paused by user");
                            let _ = self.cmd_tx.try_send(EngineCommand::PauseUploads);
                        } else {
                            info!("Uploads resumed by user");
                            let _ = self.cmd_tx.try_send(EngineCommand::ResumeUploads);
                        }
                        self.refresh_display();
                    }
                    TrayAction::SignIn => {
                        self.handle_sign_in();
                    }
                    TrayAction::Settings => {
                        self.show_settings_panel();
                    }
                    TrayAction::CheckForUpdates => {
                        if let Err(e) = self.updater.check_for_updates() {
                            warn!("Failed to check for updates: {}", e);
                        }
                        self.last_update_check = std::time::Instant::now();
                    }
                },
            }

            // Check if sign-in completed on the background thread
            if SIGN_IN_COMPLETED.swap(false, Ordering::SeqCst) {
                self.update_auth_display();
                self.refresh_display();
            }

            // Periodic background update check (bypasses Sparkle's scheduler
            // which relies on NSUserDefaults that may not persist).
            const UPDATE_CHECK_INTERVAL: std::time::Duration =
                std::time::Duration::from_secs(600);
            if self.last_update_check.elapsed() >= UPDATE_CHECK_INTERVAL {
                if self.updater.can_check_for_updates() {
                    info!("Scheduled background update check");
                    self.updater.check_for_updates_in_background();
                }
                self.last_update_check = std::time::Instant::now();
            }

            // Handle deferred auto-update install requests.
            self.pending_prepare_for_update |= self.updater.take_prepare_for_update_request();
            let prepare_action = next_prepare_for_update_action(
                self.pending_prepare_for_update,
                self.last_status.as_ref(),
            );

            // On Windows, WinSparkle launches the installer *first* and only then
            // asks the app to quit — and, unlike macOS Sparkle, it never terminates
            // the process itself. The installer is already running and waiting to
            // replace this exe, so we must clean-stop and exit promptly or its
            // Restart Manager step can't close us ("Setup was unable to close all
            // applications"). Any actionable state takes the same path as a tray
            // Quit: send Shutdown (flush segment + stop OBS), then break and exit.
            #[cfg(target_os = "windows")]
            match prepare_action {
                PrepareForUpdateAction::SendCommand | PrepareForUpdateAction::ClearRequest => {
                    info!("Auto-update staged an installer; shutting down to apply it");
                    crate::INTENTIONAL_EXIT.store(true, Ordering::SeqCst);
                    let _ = self.cmd_tx.try_send(EngineCommand::Shutdown);
                    break;
                }
                PrepareForUpdateAction::Wait => {}
            }

            // On macOS, Sparkle terminates (and relaunches) the app for us, so we
            // only need a clean stop while recording; otherwise just clear it.
            #[cfg(not(target_os = "windows"))]
            match prepare_action {
                PrepareForUpdateAction::SendCommand => {
                    info!("Auto-update requested a clean stop before install");
                    // Mark as intentional so main exits with 0 (no KeepAlive restart)
                    crate::INTENTIONAL_EXIT.store(true, Ordering::SeqCst);
                    match self.cmd_tx.try_send(EngineCommand::PrepareForUpdate) {
                        Ok(()) => {
                            self.pending_prepare_for_update = false;
                        }
                        Err(e) => {
                            warn!("Failed to queue prepare-for-update command: {}", e);
                        }
                    }
                }
                PrepareForUpdateAction::ClearRequest => {
                    self.pending_prepare_for_update = false;
                }
                PrepareForUpdateAction::Wait => {}
            }

            // Refresh display if updater availability changed
            let can_check = self.updater.can_check_for_updates();
            if self.last_updater_can_check != Some(can_check) {
                self.last_updater_can_check = Some(can_check);
                self.refresh_display();
            }

            // Small sleep to prevent busy loop when no events
            std::thread::sleep(std::time::Duration::from_millis(16));
        }

        info!("Tray event loop exited");
        Ok(())
    }

    /// Process a new engine status: update internal state and refresh the display.
    fn update_status(&mut self, status: &EngineStatus) {
        self.last_status = Some(status.clone());

        self.updater
            .set_busy(status_blocks_immediate_update(status));

        self.refresh_display();

        debug!(
            "Tray status updated: {}",
            match status {
                EngineStatus::Idle => "Idle".to_string(),
                EngineStatus::Capturing { event_count } =>
                    format!("Capturing ({} events)", event_count),
                EngineStatus::Paused => "Paused".to_string(),
                EngineStatus::RecordingBlocked => "RecordingBlocked".to_string(),
                EngineStatus::WaitingForOBS => "WaitingForOBS".to_string(),
                EngineStatus::Uploading { chunk_id } => format!("Uploading {}", chunk_id),
                EngineStatus::Error(msg) => format!("Error: {}", msg),
            }
        );
    }

    /// Refresh cached auth display strings from the auth manager.
    fn update_auth_display(&mut self) {
        let (account, sign_action, configured) = compute_auth_display(&self.auth);
        self.account_display_text = account;
        self.sign_action_display_text = sign_action;
        self.auth_configured = configured;
    }

    fn handle_sign_in(&mut self) {
        let (Some(auth), Some(rt)) = (self.auth.clone(), self.auth_runtime.clone()) else {
            warn!("Auth not configured — cannot sign in");
            return;
        };

        // Check if already authenticated — if so, sign out
        let is_authenticated = rt.block_on(async {
            let mgr = auth.lock().await;
            mgr.is_authenticated()
        });

        if is_authenticated {
            info!("Signing out...");
            rt.block_on(async {
                let mut mgr = auth.lock().await;
                mgr.logout();
            });
            self.update_auth_display();
            self.refresh_display();
            return;
        }

        info!("Starting Google sign-in flow...");

        // Run the OAuth flow on a background thread (can't block the main/tray thread)
        let auth_clone = auth.clone();
        std::thread::spawn(move || {
            let result = rt.block_on(async {
                let mut mgr = auth_clone.lock().await;
                mgr.login().await
            });
            match result {
                Ok(state) => {
                    info!("Sign-in successful: {}", state.email);
                    SIGN_IN_COMPLETED.store(true, Ordering::SeqCst);
                }
                Err(e) => {
                    error!("Sign-in failed: {}", e);
                }
            }
        });
    }

    fn show_settings_panel(&self) {
        let config = match crate::config::Config::load() {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to load config for settings panel: {}", e);
                return;
            }
        };

        let result = match super::app_selector::show_panel(
            &config.capture.target_apps,
            config.capture.capture_all,
        ) {
            Ok(r) => r,
            Err(e) => {
                error!("Settings panel error: {}", e);
                return;
            }
        };

        if !result.saved {
            return;
        }

        // Save to config file
        let mut config = config;
        config.capture.target_apps = result.selected_apps.clone();
        config.capture.capture_all = result.capture_all;
        if let Err(e) = config.save() {
            error!("Failed to save config: {}", e);
            return;
        }

        // Tell the engine to reload
        if let Err(e) = self.cmd_tx.try_send(EngineCommand::ReloadTargetApps {
            target_apps: result.selected_apps,
            capture_all: result.capture_all,
        }) {
            error!("Failed to send reload command: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// Platform tray factory
// ---------------------------------------------------------------------------

fn create_platform_tray(icon_paths: &TrayIconPaths) -> Result<Box<dyn PlatformTray>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(super::tray_macos::MacOSTray::new(icon_paths)?))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(super::tray_windows::WindowsTray::new(icon_paths)?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = icon_paths;
        Ok(Box::new(super::platform_tray::StubTray))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        next_prepare_for_update_action, status_blocks_immediate_update,
        status_needs_prepare_for_update, PrepareForUpdateAction,
    };
    use crate::sync::EngineStatus;

    #[test]
    fn update_blocking_statuses_match_policy() {
        assert!(status_blocks_immediate_update(&EngineStatus::Capturing {
            event_count: 1
        }));
        assert!(status_blocks_immediate_update(
            &EngineStatus::RecordingBlocked
        ));
        assert!(status_blocks_immediate_update(&EngineStatus::Uploading {
            chunk_id: "chunk".into(),
        }));

        assert!(!status_blocks_immediate_update(&EngineStatus::Idle));
        assert!(!status_blocks_immediate_update(
            &EngineStatus::WaitingForOBS
        ));
        assert!(!status_blocks_immediate_update(&EngineStatus::Error(
            "boom".into()
        )));
    }

    #[test]
    fn prepare_for_update_only_targets_active_recording_states() {
        assert!(status_needs_prepare_for_update(&EngineStatus::Capturing {
            event_count: 1
        }));
        assert!(status_needs_prepare_for_update(&EngineStatus::Paused));
        assert!(status_needs_prepare_for_update(
            &EngineStatus::RecordingBlocked
        ));

        assert!(!status_needs_prepare_for_update(&EngineStatus::Idle));
        assert!(!status_needs_prepare_for_update(
            &EngineStatus::WaitingForOBS
        ));
        assert!(!status_needs_prepare_for_update(&EngineStatus::Uploading {
            chunk_id: "chunk".into(),
        }));
    }

    #[test]
    fn prepare_for_update_action_is_one_shot_and_status_driven() {
        assert_eq!(
            next_prepare_for_update_action(true, Some(&EngineStatus::Capturing { event_count: 1 })),
            PrepareForUpdateAction::SendCommand
        );
        assert_eq!(
            next_prepare_for_update_action(true, Some(&EngineStatus::Paused)),
            PrepareForUpdateAction::SendCommand
        );
        assert_eq!(
            next_prepare_for_update_action(true, Some(&EngineStatus::Idle)),
            PrepareForUpdateAction::ClearRequest
        );
        assert_eq!(
            next_prepare_for_update_action(
                true,
                Some(&EngineStatus::Uploading {
                    chunk_id: "chunk".into(),
                })
            ),
            PrepareForUpdateAction::ClearRequest
        );
        assert_eq!(
            next_prepare_for_update_action(true, None),
            PrepareForUpdateAction::Wait
        );
        assert_eq!(
            next_prepare_for_update_action(false, Some(&EngineStatus::Idle)),
            PrepareForUpdateAction::Wait
        );
    }
}

// ---------------------------------------------------------------------------
// Icon generation (cross-platform)
// ---------------------------------------------------------------------------

/// Truncate a string to a maximum length, adding ellipsis if needed
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Get the paths to the tray icons for each capture state
fn get_icon_paths() -> Result<TrayIconPaths> {
    let icon_dir = directories::ProjectDirs::from("dev", "crowd-cast", "agent")
        .map(|p| p.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir());

    std::fs::create_dir_all(&icon_dir)?;
    let ext = if cfg!(target_os = "windows") {
        "ico"
    } else {
        "png"
    };

    let paths = TrayIconPaths {
        idle: icon_dir.join(format!("tray_idle.{}", ext)),
        recording: icon_dir.join(format!("tray_recording.{}", ext)),
        blocked: icon_dir.join(format!("tray_blocked.{}", ext)),
    };

    // Regenerate icons if any are missing or if the app version changed
    let version_file = icon_dir.join("tray_version");
    let current_version = env!("CARGO_PKG_VERSION");
    let cached_version = std::fs::read_to_string(&version_file).unwrap_or_default();
    let needs_create = cached_version.trim() != current_version
        || !paths.idle.exists()
        || !paths.recording.exists()
        || !paths.blocked.exists();

    if needs_create {
        create_tray_icons(&paths)?;
        let _ = std::fs::write(&version_file, current_version);
        info!("Created tray icons in {:?}", icon_dir);
    }

    Ok(paths)
}

fn create_tray_icons(paths: &TrayIconPaths) -> Result<()> {
    let size = 32u32;
    let base = load_base_icon(size);
    let variants: [(TrayIconState, [u8; 4], &PathBuf); 3] = [
        (TrayIconState::Idle, [158, 158, 158, 255], &paths.idle),
        (
            TrayIconState::Recording,
            [76, 175, 80, 255],
            &paths.recording,
        ),
        (TrayIconState::Blocked, [255, 152, 0, 255], &paths.blocked),
    ];

    for (_state, color, path) in variants {
        let mut img = base.clone();
        apply_status_dot(&mut img, color);
        img.save(path)?;
        debug!("Tray icon generated: {:?}", path);
    }

    Ok(())
}

fn load_base_icon(size: u32) -> RgbaImage {
    let logo_bytes = include_bytes!("../../assets/logo.png");
    if let Ok(image) = image::load_from_memory(logo_bytes) {
        image
            .resize_exact(size, size, FilterType::Lanczos3)
            .to_rgba8()
    } else {
        create_fallback_icon(size)
    }
}

fn create_fallback_icon(size: u32) -> RgbaImage {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - size as f32 / 2.0;
            let dy = y as f32 - size as f32 / 2.0;
            let dist = (dx * dx + dy * dy).sqrt();
            let radius = size as f32 / 2.0 - 2.0;

            if dist < radius {
                rgba.push(68);
                rgba.push(68);
                rgba.push(68);
                rgba.push(255);
            } else {
                rgba.push(0);
                rgba.push(0);
                rgba.push(0);
                rgba.push(0);
            }
        }
    }

    image::RgbaImage::from_raw(size, size, rgba)
        .unwrap_or_else(|| image::RgbaImage::new(size, size))
}

fn apply_status_dot(img: &mut RgbaImage, color: [u8; 4]) {
    let size = img.width().min(img.height());
    if size == 0 {
        return;
    }

    let radius = size as f32 * 0.18;
    let cx = size as f32 - radius - 2.0;
    let cy = size as f32 - radius - 2.0;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            if (dx * dx + dy * dy).sqrt() <= radius {
                img.put_pixel(x, y, image::Rgba(color));
            }
        }
    }
}
