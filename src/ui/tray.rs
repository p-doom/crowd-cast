//! System tray application using dmikushin/tray FFI
//!
//! Provides a system tray UI for controlling the crowd-cast agent.

use anyhow::Result;
use image::imageops::FilterType;
use image::RgbaImage;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use super::{
    tray_ffi::{self, Tray, TrayMenuItem},
    UpdaterController,
};
use crate::sync::{EngineCommand, EngineStatus};

// Global state for callbacks (required because C callbacks can't capture Rust state)
static CHECK_FOR_UPDATES_REQUESTED: AtomicBool = AtomicBool::new(false);
static SETTINGS_REQUESTED: AtomicBool = AtomicBool::new(false);
static TOGGLE_UPLOADS_REQUESTED: AtomicBool = AtomicBool::new(false);
static SIGN_IN_REQUESTED: AtomicBool = AtomicBool::new(false);
static SIGN_IN_COMPLETED: AtomicBool = AtomicBool::new(false);
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static CMD_SENDER: Mutex<Option<mpsc::Sender<EngineCommand>>> = Mutex::new(None);

/// Check if the user explicitly quit via the tray menu.
pub fn was_quit_requested() -> bool {
    QUIT_REQUESTED.load(Ordering::SeqCst)
}

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
            | EngineStatus::Paused
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayIconState {
    Idle,
    Recording,
    Paused,
    Blocked,
}

struct TrayIconPaths {
    idle: PathBuf,
    recording: PathBuf,
    blocked: PathBuf,
}

struct TrayIconSet {
    idle: CString,
    recording: CString,
    blocked: CString,
}

impl TrayIconSet {
    fn new(paths: &TrayIconPaths) -> Result<Self> {
        Ok(Self {
            idle: CString::new(paths.idle.to_string_lossy().as_bytes())?,
            recording: CString::new(paths.recording.to_string_lossy().as_bytes())?,
            blocked: CString::new(paths.blocked.to_string_lossy().as_bytes())?,
        })
    }

    fn path_for(&self, state: TrayIconState) -> *const std::os::raw::c_char {
        match state {
            TrayIconState::Idle => self.idle.as_ptr(),
            TrayIconState::Recording => self.recording.as_ptr(),
            TrayIconState::Paused => self.idle.as_ptr(), // Use idle (grey) icon when paused
            TrayIconState::Blocked => self.blocked.as_ptr(),
        }
    }
}

/// System tray application
pub struct TrayApp {
    cmd_tx: mpsc::Sender<EngineCommand>,
    status_rx: broadcast::Receiver<EngineStatus>,
    // Owned data that must live as long as the tray
    _icons: TrayIconSet,
    _tooltip: CString,
    _menu_items: Vec<TrayMenuItem>,
    _menu_strings: Vec<CString>,
    tray: Tray,
    updater: UpdaterController,
    last_updater_can_check: Option<bool>,
    last_status: Option<EngineStatus>,
    pending_prepare_for_update: bool,
    last_update_check: std::time::Instant,
    uploads_paused: bool,
    auth: Option<std::sync::Arc<tokio::sync::Mutex<crate::auth::AuthManager>>>,
    auth_runtime: Option<std::sync::Arc<tokio::runtime::Runtime>>,
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

        // Store sender in global for callbacks
        {
            let mut sender = CMD_SENDER.lock().unwrap();
            *sender = Some(cmd_tx.clone());
        }

        // Get tray icon paths
        let icon_paths = get_icon_paths()?;
        let icons = TrayIconSet::new(&icon_paths)?;
        let updater = UpdaterController::new();

        let tooltip = CString::new("crowd-cast Agent")?;

        // Create menu items
        // Menu strings must be kept alive
        // Note: We use indices to update text dynamically based on state
        let status_text = CString::new("Status: Idle")?;
        let separator = CString::new("-")?;
        let start_text = CString::new("Start Recording")?;
        let pause_text = CString::new("Pause Recording")?;
        let resume_text = CString::new("Resume Recording")?;
        let stop_text = CString::new("Stop Recording")?;
        let panic_text = CString::new("Delete last 10 minutes")?;
        let pause_uploads_text = CString::new("Pause Uploads")?;
        let settings_text = CString::new("Settings")?;
        let updates_text = CString::new("Check for Updates")?;
        let quit_text = CString::new("Quit")?;

        // Determine auth display and action text based on auth state
        let is_authenticated = auth.as_ref()
            .and_then(|a| a.try_lock().ok())
            .map(|m| m.is_authenticated())
            .unwrap_or(false);
        let account_text = if let Some(ref auth) = auth {
            if let Ok(mgr) = auth.try_lock() {
                if let Some(email) = mgr.email() {
                    CString::new(format!("Signed in as {}", email))?
                } else {
                    CString::new("")?
                }
            } else {
                CString::new("")?
            }
        } else {
            CString::new("")?
        };
        let sign_action_text = if auth.is_none() {
            CString::new("Sign in (not configured)")?
        } else if is_authenticated {
            CString::new("Sign out")?
        } else {
            CString::new("Sign in with Google")?
        };

        let menu_strings = vec![
            status_text,          // 0
            account_text,         // 1  — "Signed in as X" (disabled display)
            separator.clone(),    // 2
            start_text,           // 3
            pause_text,           // 4
            resume_text,          // 5
            stop_text,            // 6
            panic_text,           // 7
            separator.clone(),    // 8
            pause_uploads_text,   // 9
            sign_action_text,     // 10 — "Sign in with Google" / "Sign out"
            settings_text,        // 11
            updates_text,         // 12
            separator.clone(),    // 13
            quit_text,            // 14
        ];

        // Build menu items array (NULL-terminated)
        // Menu indices: 0=status, 1=account, 2=sep, 3=start, 4=pause, 5=resume, 6=stop,
        //   7=panic, 8=sep, 9=pause_uploads, 10=sign_action, 11=settings, 12=updates, 13=sep, 14=quit
        let mut menu_items = vec![
            TrayMenuItem {
                text: menu_strings[0].as_ptr(), // Status
                disabled: 1,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[1].as_ptr(), // Signed in as X (display only)
                disabled: 1,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[2].as_ptr(), // separator
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[3].as_ptr(), // Start Recording
                disabled: 0,
                checked: 0,
                cb: Some(on_start_capture),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[4].as_ptr(), // Pause Recording
                disabled: 1,
                checked: 0,
                cb: Some(on_pause_recording),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[5].as_ptr(), // Resume Recording
                disabled: 1,
                checked: 0,
                cb: Some(on_resume_recording),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[6].as_ptr(), // Stop Recording
                disabled: 1,
                checked: 0,
                cb: Some(on_stop_capture),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[7].as_ptr(), // Panic
                disabled: 0,
                checked: 0,
                cb: Some(on_panic),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[8].as_ptr(), // separator
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[9].as_ptr(), // Pause Uploads
                disabled: 0,
                checked: 0,
                cb: Some(on_toggle_uploads),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[10].as_ptr(), // Sign in / Sign out
                disabled: if auth.is_none() { 1 } else { 0 },
                checked: 0,
                cb: Some(on_sign_in),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[11].as_ptr(), // Settings
                disabled: 0,
                checked: 0,
                cb: Some(on_settings),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[12].as_ptr(), // Check for Updates
                disabled: 1,
                checked: 0,
                cb: Some(on_check_for_updates),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[13].as_ptr(), // separator
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[14].as_ptr(), // Quit
                disabled: 0,
                checked: 0,
                cb: Some(on_quit),
                submenu: std::ptr::null_mut(),
            },
            // NULL terminator
            TrayMenuItem {
                text: std::ptr::null(),
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
        ];

        let tray = Tray {
            icon_filepath: icons.path_for(TrayIconState::Idle),
            tooltip: tooltip.as_ptr(),
            cb: None, // No left-click callback, just show menu
            menu: menu_items.as_mut_ptr(),
        };

        info!("System tray created");

        Ok(Self {
            cmd_tx,
            status_rx,
            _icons: icons,
            _tooltip: tooltip,
            _menu_items: menu_items,
            _menu_strings: menu_strings,
            tray,
            updater,
            last_updater_can_check: None,
            last_status: None,
            pending_prepare_for_update: false,
            last_update_check: std::time::Instant::now(),
            uploads_paused: directories::ProjectDirs::from("dev", "crowd-cast", "agent")
                .and_then(|p| std::fs::read_to_string(p.data_dir().join("uploads_paused")).ok())
                .map(|s| s.trim() == "true")
                .unwrap_or(false),
            auth,
            auth_runtime,
        })
    }

    /// Initialize and run the tray application event loop (blocks until quit)
    pub fn run(mut self) -> Result<()> {
        info!("Starting system tray event loop");

        // Initialize the tray
        let init_result = unsafe { tray_ffi::tray_init(&mut self.tray) };
        if init_result != 0 {
            return Err(anyhow::anyhow!("Failed to initialize system tray"));
        }

        QUIT_REQUESTED.store(false, Ordering::SeqCst);
        CHECK_FOR_UPDATES_REQUESTED.store(false, Ordering::SeqCst);
        SETTINGS_REQUESTED.store(false, Ordering::SeqCst);
        TOGGLE_UPLOADS_REQUESTED.store(false, Ordering::SeqCst);

        // Restore upload pause state from previous session
        if self.uploads_paused {
            info!("Uploads paused (restored from previous session)");
            let new_text = CString::new("Resume Uploads").unwrap_or_default();
            self._menu_strings[9] = new_text;
            self._menu_items[9].text = self._menu_strings[9].as_ptr();
            self.tray.menu = self._menu_items.as_mut_ptr();
            unsafe { tray_ffi::tray_update(&mut self.tray); }
        }

        self.updater.start();
        if let Some(reason) = self.updater.reason() {
            info!("Updater unavailable: {}", reason);
        }
        self.refresh_updater_menu_item();

        loop {
            // Check for status updates (non-blocking)
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

            // Run one iteration of the native event loop (non-blocking)
            let loop_result = unsafe { tray_ffi::tray_loop(0) };

            if loop_result < 0 {
                info!("Tray loop signaled exit");
                break;
            }

            // Check if quit was requested via callback
            if QUIT_REQUESTED.load(Ordering::SeqCst) {
                info!("Quit requested via tray menu");
                let _ = self.cmd_tx.try_send(EngineCommand::Shutdown);
                break;
            }

            if SIGN_IN_REQUESTED.swap(false, Ordering::SeqCst) {
                self.handle_sign_in();
            }

            if SIGN_IN_COMPLETED.swap(false, Ordering::SeqCst) {
                self.update_auth_menu();
            }

            if SETTINGS_REQUESTED.swap(false, Ordering::SeqCst) {
                self.show_settings_panel();
            }

            if TOGGLE_UPLOADS_REQUESTED.swap(false, Ordering::SeqCst) {
                self.uploads_paused = !self.uploads_paused;
                if self.uploads_paused {
                    info!("Uploads paused by user");
                    let _ = self.cmd_tx.try_send(EngineCommand::PauseUploads);
                } else {
                    info!("Uploads resumed by user");
                    let _ = self.cmd_tx.try_send(EngineCommand::ResumeUploads);
                }
                // Update menu text: index 7 is the uploads toggle
                let new_text = if self.uploads_paused {
                    CString::new("Resume Uploads").unwrap_or_default()
                } else {
                    CString::new("Pause Uploads").unwrap_or_default()
                };
                self._menu_strings[9] = new_text;
                self._menu_items[9].text = self._menu_strings[9].as_ptr();
                self.tray.menu = self._menu_items.as_mut_ptr();
                unsafe { tray_ffi::tray_update(&mut self.tray); }
            }

            if CHECK_FOR_UPDATES_REQUESTED.swap(false, Ordering::SeqCst) {
                if let Err(e) = self.updater.check_for_updates() {
                    warn!("Failed to check for updates: {}", e);
                }
                self.last_update_check = std::time::Instant::now();
            }

            // Periodic background update check (bypasses Sparkle's scheduler
            // which relies on NSUserDefaults that may not persist).
            // Interval matches SUScheduledCheckInterval in Info.plist (default 60s for testing, 86400 for production).
            const UPDATE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);
            if self.last_update_check.elapsed() >= UPDATE_CHECK_INTERVAL {
                if self.updater.can_check_for_updates() {
                    info!("Scheduled background update check");
                    self.updater.check_for_updates_in_background();
                }
                self.last_update_check = std::time::Instant::now();
            }

            self.pending_prepare_for_update |= self.updater.take_prepare_for_update_request();
            match next_prepare_for_update_action(
                self.pending_prepare_for_update,
                self.last_status.as_ref(),
            ) {
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

            self.refresh_updater_menu_item();

            // Small sleep to prevent busy loop when no events
            std::thread::sleep(std::time::Duration::from_millis(16));
        }

        info!("Tray event loop exited");
        Ok(())
    }

    /// Update the status display based on engine status
    fn update_status(&mut self, status: &EngineStatus) {
        self.last_status = Some(status.clone());

        // Determine status text, icon state, and menu state
        #[derive(Clone, Copy, PartialEq)]
        enum MenuState {
            Idle,      // Show: Start
            Recording, // Show: Pause, Stop
            Paused,    // Show: Resume, Stop
        }

        let (status_text, icon_state, menu_state) = match status {
            EngineStatus::Idle => (
                "Status: Idle".to_string(),
                TrayIconState::Idle,
                MenuState::Idle,
            ),
            EngineStatus::Capturing { event_count } => (
                format!("Status: Capturing ({} events)", event_count),
                TrayIconState::Recording,
                MenuState::Recording,
            ),
            EngineStatus::Paused => (
                "Status: Paused".to_string(),
                TrayIconState::Paused,
                MenuState::Paused,
            ),
            EngineStatus::RecordingBlocked => (
                "Status: Recording (no capture sources)".to_string(),
                TrayIconState::Blocked,
                MenuState::Recording,
            ),
            EngineStatus::WaitingForOBS => (
                "Status: Waiting for OBS...".to_string(),
                TrayIconState::Blocked,
                MenuState::Idle,
            ),
            EngineStatus::Uploading { chunk_id } => (
                format!("Status: Uploading {}", chunk_id),
                TrayIconState::Idle,
                MenuState::Idle,
            ),
            EngineStatus::Error(msg) => (
                format!("Status: Error - {}", truncate_str(msg, 30)),
                TrayIconState::Idle,
                MenuState::Idle,
            ),
        };

        // Update the status menu item text and menu item visibility
        if let Ok(new_text) = CString::new(status_text.as_bytes()) {
            if !self._menu_strings.is_empty() {
                // Update status text
                self._menu_strings[0] = new_text;
                self._menu_items[0].text = self._menu_strings[0].as_ptr();

                // Update menu item visibility based on state
                // Menu indices: 3=start, 4=pause, 5=resume, 6=stop
                match menu_state {
                    MenuState::Idle => {
                        // Show: Start, Hide: Pause, Resume, Stop
                        self._menu_items[3].disabled = 0; // Start - enabled
                        self._menu_items[4].disabled = 1; // Pause - disabled
                        self._menu_items[5].disabled = 1; // Resume - disabled
                        self._menu_items[6].disabled = 1; // Stop - disabled
                    }
                    MenuState::Recording => {
                        // Show: Pause, Stop, Hide: Start, Resume
                        self._menu_items[3].disabled = 1; // Start - disabled
                        self._menu_items[4].disabled = 0; // Pause - enabled
                        self._menu_items[5].disabled = 1; // Resume - disabled
                        self._menu_items[6].disabled = 0; // Stop - enabled
                    }
                    MenuState::Paused => {
                        // Show: Resume, Stop, Hide: Start, Pause
                        self._menu_items[3].disabled = 1; // Start - disabled
                        self._menu_items[4].disabled = 1; // Pause - disabled
                        self._menu_items[5].disabled = 0; // Resume - enabled
                        self._menu_items[6].disabled = 0; // Stop - enabled
                    }
                }

                self.tray.menu = self._menu_items.as_mut_ptr();
                self.tray.icon_filepath = self._icons.path_for(icon_state);
                unsafe {
                    tray_ffi::tray_update(&mut self.tray);
                }
            }
        }

        self.updater
            .set_busy(status_blocks_immediate_update(status));
        self.refresh_updater_menu_item();

        debug!("Tray status updated: {}", status_text);
    }

    fn refresh_updater_menu_item(&mut self) {
        if self._menu_items.len() <= 8 {
            return;
        }

        let can_check = self.updater.can_check_for_updates();
        if self.last_updater_can_check == Some(can_check) {
            return;
        }

        self.last_updater_can_check = Some(can_check);
        self._menu_items[12].disabled = if can_check { 0 } else { 1 };

        self.tray.menu = self._menu_items.as_mut_ptr();
        unsafe {
            tray_ffi::tray_update(&mut self.tray);
        }
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
            self.update_auth_menu();
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

    fn update_auth_menu(&mut self) {
        if let Some(ref auth) = self.auth {
            if let Ok(mgr) = auth.try_lock() {
                if let Some(email) = mgr.email() {
                    // Signed in: show account display + "Sign out" action
                    let account = CString::new(format!("Signed in as {}", email)).unwrap_or_default();
                    self._menu_strings[1] = account;
                    self._menu_items[1].text = self._menu_strings[1].as_ptr();
                    let sign_out = CString::new("Sign out").unwrap_or_default();
                    self._menu_strings[10] = sign_out;
                    self._menu_items[10].text = self._menu_strings[10].as_ptr();
                } else {
                    // Signed out: clear account display + "Sign in with Google" action
                    let empty = CString::new("").unwrap_or_default();
                    self._menu_strings[1] = empty;
                    self._menu_items[1].text = self._menu_strings[1].as_ptr();
                    let sign_in = CString::new("Sign in with Google").unwrap_or_default();
                    self._menu_strings[10] = sign_in;
                    self._menu_items[10].text = self._menu_strings[10].as_ptr();
                }
            }
        }
        self.tray.menu = self._menu_items.as_mut_ptr();
        unsafe { tray_ffi::tray_update(&mut self.tray); }
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
        assert!(status_blocks_immediate_update(&EngineStatus::Paused));
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
            next_prepare_for_update_action(false, Some(&EngineStatus::Paused)),
            PrepareForUpdateAction::Wait
        );
    }
}

impl Drop for TrayApp {
    fn drop(&mut self) {
        // Clean up global state
        let mut sender = CMD_SENDER.lock().unwrap();
        *sender = None;
    }
}

// C callbacks - these must be extern "C" functions

unsafe extern "C" fn on_start_capture(_item: *mut TrayMenuItem) {
    info!("Start recording requested via tray");
    if let Some(sender) = CMD_SENDER.lock().unwrap().as_ref() {
        // Use try_send to avoid blocking (can't use blocking_send inside tokio runtime)
        if let Err(e) = sender.try_send(EngineCommand::StartRecording) {
            error!("Failed to send start recording command: {}", e);
        }
    }
}

unsafe extern "C" fn on_stop_capture(_item: *mut TrayMenuItem) {
    info!("Stop recording requested via tray");
    if let Some(sender) = CMD_SENDER.lock().unwrap().as_ref() {
        // Use try_send to avoid blocking (can't use blocking_send inside tokio runtime)
        if let Err(e) = sender.try_send(EngineCommand::StopRecording) {
            error!("Failed to send stop recording command: {}", e);
        }
    }
}

unsafe extern "C" fn on_pause_recording(_item: *mut TrayMenuItem) {
    info!("Pause recording requested via tray");
    if let Some(sender) = CMD_SENDER.lock().unwrap().as_ref() {
        if let Err(e) = sender.try_send(EngineCommand::PauseRecording) {
            error!("Failed to send pause recording command: {}", e);
        }
    }
}

unsafe extern "C" fn on_resume_recording(_item: *mut TrayMenuItem) {
    info!("Resume recording requested via tray");
    if let Some(sender) = CMD_SENDER.lock().unwrap().as_ref() {
        if let Err(e) = sender.try_send(EngineCommand::ResumeRecording) {
            error!("Failed to send resume recording command: {}", e);
        }
    }
}

unsafe extern "C" fn on_check_for_updates(_item: *mut TrayMenuItem) {
    info!("Check for updates requested via tray");
    CHECK_FOR_UPDATES_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_panic(_item: *mut TrayMenuItem) {
    warn!("Panic button pressed via tray");
    if let Some(sender) = CMD_SENDER.lock().unwrap().as_ref() {
        if let Err(e) = sender.try_send(EngineCommand::Panic) {
            error!("Failed to send panic command: {}", e);
        }
    }
}

unsafe extern "C" fn on_toggle_uploads(_item: *mut TrayMenuItem) {
    info!("Toggle uploads requested via tray");
    TOGGLE_UPLOADS_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_sign_in(_item: *mut TrayMenuItem) {
    info!("Sign in requested via tray");
    SIGN_IN_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_settings(_item: *mut TrayMenuItem) {
    info!("Settings requested via tray");
    SETTINGS_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_quit(_item: *mut TrayMenuItem) {
    info!("Quit requested via tray");
    QUIT_REQUESTED.store(true, Ordering::SeqCst);
    unsafe {
        tray_ffi::tray_exit();
    }
}

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

    let needs_create = !paths.idle.exists() || !paths.recording.exists() || !paths.blocked.exists();

    if needs_create {
        create_tray_icons(&paths)?;
        info!("Created tray icons in {:?}", icon_dir);
    }

    Ok(paths)
}

fn create_tray_icons(paths: &TrayIconPaths) -> Result<()> {
    let size = 32u32;
    let base = load_base_icon(size);
    let variants = [
        (TrayIconState::Idle, [158, 158, 158, 255], &paths.idle),
        (
            TrayIconState::Recording,
            [76, 175, 80, 255],
            &paths.recording,
        ),
        (TrayIconState::Blocked, [255, 152, 0, 255], &paths.blocked),
    ];

    for (state, color, path) in variants {
        let mut img = base.clone();
        apply_status_dot(&mut img, color);
        img.save(path)?;
        debug!("Tray icon generated for {:?}: {:?}", state, path);
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

