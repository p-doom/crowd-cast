//! Windows tray implementation using the `tray-icon` + `muda` crates.
//!
//! Pure Rust (no C/FFI like the macOS tray). The shared `TrayApp` drives the
//! event loop and business logic; this type only renders the native tray icon
//! and menu and reports menu clicks as `TrayAction`s.
//!
//! `tray-icon` needs a Win32 message loop on the thread that owns the icon.
//! `TrayApp::run()` runs on the main thread, so we build the icon in `init()`
//! and pump the thread's message queue in `poll()`.

use anyhow::{Context, Result};
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use super::platform_tray::{
    PlatformTray, PlatformTrayPoll, TrayAction, TrayDisplayState, TrayIconPaths, TrayIconState,
};

// Stable menu-item ids so menu clicks map deterministically to actions.
const ID_START: &str = "cc.start";
const ID_STOP: &str = "cc.stop";
const ID_PANIC: &str = "cc.panic";
const ID_UPLOADS: &str = "cc.uploads";
const ID_SIGN: &str = "cc.sign";
const ID_SETTINGS: &str = "cc.settings";
const ID_UPDATES: &str = "cc.updates";
const ID_REPORT_BUG: &str = "cc.reportbug";
const ID_QUIT: &str = "cc.quit";

pub struct WindowsTray {
    idle_icon_path: std::path::PathBuf,
    recording_icon_path: std::path::PathBuf,
    blocked_icon_path: std::path::PathBuf,
    /// Built in `new()`, moved into the tray in `init()`.
    pending_menu: Option<Menu>,
    tray: Option<TrayIcon>,
    // Item handles we mutate on update(). Items we never change (panic, settings,
    // report bug, quit, separators) are owned by the menu and don't need handles here.
    status_item: MenuItem,
    account_item: MenuItem,
    start_item: MenuItem,
    stop_item: MenuItem,
    uploads_item: MenuItem,
    sign_item: MenuItem,
    updates_item: MenuItem,
    last_icon_state: Option<TrayIconState>,
}

impl WindowsTray {
    pub fn new(icon_paths: &TrayIconPaths) -> Result<Self> {
        let menu = Menu::new();

        // Disabled label rows.
        let status_item = MenuItem::new("Status: Idle", false, None);
        let account_item = MenuItem::new("", false, None);

        // Actionable rows (stable ids).
        let start_item = MenuItem::with_id(ID_START, "Start Recording", true, None);
        let stop_item = MenuItem::with_id(ID_STOP, "Stop Recording", false, None);
        let panic_item = MenuItem::with_id(ID_PANIC, "Delete last 10 minutes", true, None);
        let uploads_item = MenuItem::with_id(ID_UPLOADS, "Pause Uploads", true, None);
        let sign_item = MenuItem::with_id(ID_SIGN, "Sign in with Google", true, None);
        let settings_item = MenuItem::with_id(ID_SETTINGS, "Settings", true, None);
        let updates_item = MenuItem::with_id(ID_UPDATES, "Check for Updates", false, None);
        let report_bug_item = MenuItem::with_id(ID_REPORT_BUG, "Report Bug…", true, None);
        let quit_item = MenuItem::with_id(ID_QUIT, "Quit", true, None);

        let sep1 = PredefinedMenuItem::separator();
        let sep2 = PredefinedMenuItem::separator();
        let sep3 = PredefinedMenuItem::separator();

        menu.append_items(&[
            &status_item,
            &account_item,
            &sep1,
            &start_item,
            &stop_item,
            &panic_item,
            &sep2,
            &uploads_item,
            &sign_item,
            &settings_item,
            &updates_item,
            &report_bug_item,
            &sep3,
            &quit_item,
        ])
        .context("Failed to build Windows tray menu")?;

        Ok(Self {
            idle_icon_path: icon_paths.idle.clone(),
            recording_icon_path: icon_paths.recording.clone(),
            blocked_icon_path: icon_paths.blocked.clone(),
            pending_menu: Some(menu),
            tray: None,
            status_item,
            account_item,
            start_item,
            stop_item,
            uploads_item,
            sign_item,
            updates_item,
            last_icon_state: None,
        })
    }

    fn icon_path(&self, state: TrayIconState) -> &std::path::Path {
        match state {
            TrayIconState::Idle => &self.idle_icon_path,
            TrayIconState::Recording => &self.recording_icon_path,
            TrayIconState::Blocked => &self.blocked_icon_path,
        }
    }

    fn load_icon(&self, state: TrayIconState) -> Result<Icon> {
        let path = self.icon_path(state);
        Icon::from_path(path, None)
            .map_err(|e| anyhow::anyhow!("Failed to load tray icon {:?}: {}", path, e))
    }
}

impl PlatformTray for WindowsTray {
    fn init(&mut self) -> Result<()> {
        let menu = self
            .pending_menu
            .take()
            .context("WindowsTray::init called twice")?;

        let icon = self.load_icon(TrayIconState::Idle)?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("crowd-cast Agent")
            .with_icon(icon)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to create Windows tray icon: {}", e))?;

        self.tray = Some(tray);
        self.last_icon_state = Some(TrayIconState::Idle);
        Ok(())
    }

    fn poll(&mut self) -> PlatformTrayPoll {
        // Pump pending Win32 messages so tray-icon/muda can process clicks.
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_QUIT,
            };
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return PlatformTrayPoll::Exit;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // Translate one queued menu click into an action (the loop drains the rest).
        if let Ok(event) = MenuEvent::receiver().try_recv() {
            return match event.id.0.as_str() {
                ID_START => PlatformTrayPoll::Action(TrayAction::StartRecording),
                ID_STOP => PlatformTrayPoll::Action(TrayAction::StopRecording),
                ID_PANIC => PlatformTrayPoll::Action(TrayAction::Panic),
                ID_UPLOADS => PlatformTrayPoll::Action(TrayAction::ToggleUploads),
                ID_SIGN => PlatformTrayPoll::Action(TrayAction::SignIn),
                ID_SETTINGS => PlatformTrayPoll::Action(TrayAction::Settings),
                ID_UPDATES => PlatformTrayPoll::Action(TrayAction::CheckForUpdates),
                ID_REPORT_BUG => PlatformTrayPoll::Action(TrayAction::ReportBug),
                ID_QUIT => PlatformTrayPoll::Action(TrayAction::Quit),
                _ => PlatformTrayPoll::None,
            };
        }

        PlatformTrayPoll::None
    }

    fn update(&mut self, state: &TrayDisplayState) {
        self.status_item.set_text(&state.status_text);
        self.account_item.set_text(&state.account_text);
        self.start_item.set_enabled(state.can_start);
        self.stop_item.set_enabled(state.can_stop);
        self.uploads_item.set_text(&state.uploads_text);
        self.sign_item.set_text(&state.sign_action_text);
        self.sign_item.set_enabled(state.auth_action_enabled);
        self.updates_item.set_enabled(state.can_check_updates);

        if self.last_icon_state != Some(state.icon_state) {
            if let Some(tray) = self.tray.as_ref() {
                match self.load_icon(state.icon_state) {
                    Ok(icon) => {
                        if tray.set_icon(Some(icon)).is_ok() {
                            self.last_icon_state = Some(state.icon_state);
                        }
                    }
                    Err(e) => tracing::warn!("Failed to update tray icon: {}", e),
                }
            }
        }
    }

    fn prepare_for_restart(&mut self) {
        // Drop the icon so it disappears before the process is replaced.
        self.tray = None;
    }

    fn exit(&mut self) {
        self.tray = None;
    }
}
