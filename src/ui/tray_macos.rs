//! macOS tray implementation using tray_ffi (dmikushin/tray).
//!
//! Wraps the C FFI tray library with atomic-bool-based callbacks.
//! All business logic lives in `tray.rs`; this file only handles
//! native menu rendering and event collection.

use anyhow::Result;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use tracing::info;

use super::platform_tray::{
    PlatformTray, PlatformTrayPoll, TrayAction, TrayDisplayState, TrayIconPaths, TrayIconState,
};
use super::tray_ffi::{self, Tray, TrayMenuItem};

// ---------------------------------------------------------------------------
// Atomic flags set by C callbacks, read by poll()
// ---------------------------------------------------------------------------

static START_REQUESTED: AtomicBool = AtomicBool::new(false);
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static PANIC_REQUESTED: AtomicBool = AtomicBool::new(false);
static CHECK_FOR_UPDATES_REQUESTED: AtomicBool = AtomicBool::new(false);
static REPORT_BUG_REQUESTED: AtomicBool = AtomicBool::new(false);
static SETTINGS_REQUESTED: AtomicBool = AtomicBool::new(false);
static TOGGLE_UPLOADS_REQUESTED: AtomicBool = AtomicBool::new(false);
static SIGN_IN_REQUESTED: AtomicBool = AtomicBool::new(false);
static MACOS_QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static OPEN_DASHBOARD_REQUESTED: AtomicBool = AtomicBool::new(false);

// Last status-item health verdict seen by poll(), so transitions are logged
// exactly once. -1 = nothing observed yet (distinct from the C layer's
// 0 = "not yet checked").
static LAST_HEALTH_STATE: AtomicI32 = AtomicI32::new(-1);

/// Human-readable name for a tray.h status-item health value.
fn health_state_name(state: i32) -> &'static str {
    match state {
        -1 => "startup",
        0 => "unchecked",
        1 => "attached",
        2 => "detached",
        3 => "never-attached",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// C callbacks — set atomic flags, nothing else
// ---------------------------------------------------------------------------

unsafe extern "C" fn on_start_capture(_item: *mut TrayMenuItem) {
    START_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_stop_capture(_item: *mut TrayMenuItem) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_panic(_item: *mut TrayMenuItem) {
    PANIC_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_check_for_updates(_item: *mut TrayMenuItem) {
    CHECK_FOR_UPDATES_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_report_bug(_item: *mut TrayMenuItem) {
    REPORT_BUG_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_toggle_uploads(_item: *mut TrayMenuItem) {
    TOGGLE_UPLOADS_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_sign_in(_item: *mut TrayMenuItem) {
    SIGN_IN_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_settings(_item: *mut TrayMenuItem) {
    SETTINGS_REQUESTED.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn on_quit(_item: *mut TrayMenuItem) {
    MACOS_QUIT_REQUESTED.store(true, Ordering::SeqCst);
    unsafe {
        tray_ffi::tray_exit();
    }
}

unsafe extern "C" fn on_open_dashboard(_item: *mut TrayMenuItem) {
    OPEN_DASHBOARD_REQUESTED.store(true, Ordering::SeqCst);
}

/// Map a notification's action to the existing menu callback that performs it,
/// so notification rows need no new engine plumbing.
fn notification_callback(
    action: &TrayAction,
) -> Option<unsafe extern "C" fn(*mut TrayMenuItem)> {
    match action {
        TrayAction::ToggleUploads => Some(on_toggle_uploads),
        TrayAction::Settings => Some(on_settings),
        TrayAction::SignIn => Some(on_sign_in),
        TrayAction::CheckForUpdates => Some(on_check_for_updates),
        TrayAction::OpenDashboard => Some(on_open_dashboard),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Icon CString wrappers
// ---------------------------------------------------------------------------

struct TrayIconCStrings {
    idle: CString,
    recording: CString,
    blocked: CString,
}

impl TrayIconCStrings {
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
            TrayIconState::Blocked => self.blocked.as_ptr(),
        }
    }
}

// ---------------------------------------------------------------------------
// Menu item indices (must match the order in MacOSTray::new)
// ---------------------------------------------------------------------------

const MENU_STATUS: usize = 0;
const MENU_ACCOUNT: usize = 1;
const MENU_NOTIFICATIONS: usize = 2; // parent of the dynamic notifications submenu
// 3 = separator
const MENU_START: usize = 4;
const MENU_STOP: usize = 5;
// 6 = panic (text never changes)
// 7 = separator
const MENU_UPLOADS: usize = 8;
const MENU_SIGN_ACTION: usize = 9;
// 10 = settings (text never changes)
const MENU_UPDATES: usize = 11;
// 12 = report bug (text never changes)
// 13 = separator
// 14 = quit
// 15 = NULL terminator

// ---------------------------------------------------------------------------
// MacOSTray
// ---------------------------------------------------------------------------

pub struct MacOSTray {
    tray: Tray,
    // Owned data that must live as long as the tray
    icons: TrayIconCStrings,
    _tooltip: CString,
    menu_items: Vec<TrayMenuItem>,
    menu_strings: Vec<CString>,
    // Dynamic notifications submenu (children of MENU_NOTIFICATIONS). Rebuilt each
    // update(); the C side copies the strings during tray_update, so replacing
    // these vecs afterward is safe (same lifetime contract as menu_strings).
    notif_items: Vec<TrayMenuItem>,
    notif_strings: Vec<CString>,
    // Holds the current badged-icon path CString alive for the FFI call when a
    // badge is shown; `None` falls back to the plain per-state icon.
    badged_icon: Option<CString>,
}

impl MacOSTray {
    pub fn new(icon_paths: &TrayIconPaths) -> Result<Self> {
        let icons = TrayIconCStrings::new(icon_paths)?;
        let tooltip = CString::new("crowd-cast Agent")?;

        // Initial menu strings (overwritten by the first update() call)
        let menu_strings = vec![
            CString::new("Status: Idle")?,           // 0: status
            CString::new("")?,                       // 1: account
            CString::new("No notifications")?,       // 2: notifications (submenu parent)
            CString::new("-")?,                      // 3: separator
            CString::new("Start Recording")?,        // 4
            CString::new("Stop Recording")?,         // 5
            CString::new("Delete last 10 minutes")?, // 6: panic
            CString::new("-")?,                      // 7: separator
            CString::new("Pause Uploads")?,          // 8
            CString::new("Sign in with Google")?,    // 9
            CString::new("Settings")?,               // 10
            CString::new("Check for Updates")?,      // 11
            CString::new("Report Bug…")?,            // 12
            CString::new("-")?,                      // 13: separator
            CString::new("Quit")?,                   // 14
        ];

        let mut menu_items = vec![
            // 0: Status (disabled label)
            TrayMenuItem {
                text: menu_strings[0].as_ptr(),
                disabled: 1,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            // 1: Account (disabled label)
            TrayMenuItem {
                text: menu_strings[1].as_ptr(),
                disabled: 1,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            // 2: Notifications (submenu parent; text + submenu set in update())
            TrayMenuItem {
                text: menu_strings[2].as_ptr(),
                disabled: 1,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            // 3: Separator
            TrayMenuItem {
                text: menu_strings[3].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            // 4: Start Recording
            TrayMenuItem {
                text: menu_strings[4].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_start_capture),
                submenu: std::ptr::null_mut(),
            },
            // 5: Stop Recording
            TrayMenuItem {
                text: menu_strings[5].as_ptr(),
                disabled: 1,
                checked: 0,
                cb: Some(on_stop_capture),
                submenu: std::ptr::null_mut(),
            },
            // 6: Panic
            TrayMenuItem {
                text: menu_strings[6].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_panic),
                submenu: std::ptr::null_mut(),
            },
            // 7: Separator
            TrayMenuItem {
                text: menu_strings[7].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            // 8: Pause/Resume Uploads
            TrayMenuItem {
                text: menu_strings[8].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_toggle_uploads),
                submenu: std::ptr::null_mut(),
            },
            // 9: Sign in / Sign out
            TrayMenuItem {
                text: menu_strings[9].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_sign_in),
                submenu: std::ptr::null_mut(),
            },
            // 10: Settings
            TrayMenuItem {
                text: menu_strings[10].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_settings),
                submenu: std::ptr::null_mut(),
            },
            // 11: Check for Updates
            TrayMenuItem {
                text: menu_strings[11].as_ptr(),
                disabled: 1,
                checked: 0,
                cb: Some(on_check_for_updates),
                submenu: std::ptr::null_mut(),
            },
            // 12: Report Bug
            TrayMenuItem {
                text: menu_strings[12].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_report_bug),
                submenu: std::ptr::null_mut(),
            },
            // 13: Separator
            TrayMenuItem {
                text: menu_strings[13].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            // 14: Quit
            TrayMenuItem {
                text: menu_strings[14].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_quit),
                submenu: std::ptr::null_mut(),
            },
            // 15: NULL terminator
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
            cb: None,
            menu: menu_items.as_mut_ptr(),
        };

        // Submenu starts empty (just its NULL terminator); populated in update().
        let notif_items = vec![TrayMenuItem {
            text: std::ptr::null(),
            disabled: 0,
            checked: 0,
            cb: None,
            submenu: std::ptr::null_mut(),
        }];

        Ok(Self {
            tray,
            icons,
            _tooltip: tooltip,
            menu_items,
            menu_strings,
            notif_items,
            notif_strings: Vec::new(),
            badged_icon: None,
        })
    }
}

impl PlatformTray for MacOSTray {
    fn init(&mut self) -> Result<()> {
        // Clear any stale flags
        START_REQUESTED.store(false, Ordering::SeqCst);
        STOP_REQUESTED.store(false, Ordering::SeqCst);
        PANIC_REQUESTED.store(false, Ordering::SeqCst);
        CHECK_FOR_UPDATES_REQUESTED.store(false, Ordering::SeqCst);
        REPORT_BUG_REQUESTED.store(false, Ordering::SeqCst);
        SETTINGS_REQUESTED.store(false, Ordering::SeqCst);
        TOGGLE_UPLOADS_REQUESTED.store(false, Ordering::SeqCst);
        OPEN_DASHBOARD_REQUESTED.store(false, Ordering::SeqCst);
        SIGN_IN_REQUESTED.store(false, Ordering::SeqCst);
        MACOS_QUIT_REQUESTED.store(false, Ordering::SeqCst);

        let result = unsafe { tray_ffi::tray_init(&mut self.tray) };
        if result != 0 {
            anyhow::bail!("Failed to initialize system tray");
        }
        Ok(())
    }

    fn poll(&mut self) -> PlatformTrayPoll {
        // Process native events (callbacks fire during this call)
        let loop_result = unsafe { tray_ffi::tray_loop(0) };

        // Quit has highest priority. The on_quit callback sets the flag AND calls
        // tray_exit(), which makes tray_loop return -1. We must check the flag
        // before the loop_result so TrayApp can distinguish quit from other exits.
        if MACOS_QUIT_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Action(TrayAction::Quit);
        }

        if loop_result < 0 {
            return PlatformTrayPoll::Exit;
        }

        // Log status-item health transitions into the app log file, so a
        // missing-icon report can be classified from the participant's log
        // alone: "never-attached" persisting = the item allocated but no
        // menu-bar window ever appeared; "attached" while the icon is
        // visually absent = AppKit thinks it's in the bar but ControlCenter
        // isn't rendering it (a different failure needing a different fix).
        let health = unsafe { tray_ffi::tray_status_item_health_state() } as i32;
        let prev = LAST_HEALTH_STATE.swap(health, Ordering::Relaxed);
        if health != prev && !(prev == -1 && health == 0) {
            info!(
                "Status item health: {} -> {}",
                health_state_name(prev),
                health_state_name(health)
            );
        }

        // Platform-specific restart triggers
        if unsafe { tray_ffi::tray_screen_was_unlocked() } {
            info!("Screen unlocked — requesting restart for fresh capture sources");
            return PlatformTrayPoll::RequestRestart;
        }

        if unsafe { tray_ffi::tray_needs_restart() } {
            info!("Native tray requested process restart");
            return PlatformTrayPoll::RequestRestart;
        }

        // Regular user actions
        if START_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Action(TrayAction::StartRecording);
        }
        if STOP_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Action(TrayAction::StopRecording);
        }
        if PANIC_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Action(TrayAction::Panic);
        }
        if SIGN_IN_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Action(TrayAction::SignIn);
        }
        if SETTINGS_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Action(TrayAction::Settings);
        }
        if TOGGLE_UPLOADS_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Action(TrayAction::ToggleUploads);
        }
        if CHECK_FOR_UPDATES_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Action(TrayAction::CheckForUpdates);
        }
        if REPORT_BUG_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Action(TrayAction::ReportBug);
        }
        if OPEN_DASHBOARD_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Action(TrayAction::OpenDashboard);
        }

        PlatformTrayPoll::None
    }

    fn update(&mut self, state: &TrayDisplayState) {
        // Status text
        if let Ok(text) = CString::new(state.status_text.as_bytes()) {
            self.menu_strings[MENU_STATUS] = text;
            self.menu_items[MENU_STATUS].text = self.menu_strings[MENU_STATUS].as_ptr();
        }

        // Account text
        if let Ok(text) = CString::new(state.account_text.as_bytes()) {
            self.menu_strings[MENU_ACCOUNT] = text;
            self.menu_items[MENU_ACCOUNT].text = self.menu_strings[MENU_ACCOUNT].as_ptr();
        }

        // Start / Stop enabled state
        self.menu_items[MENU_START].disabled = if state.can_start { 0 } else { 1 };
        self.menu_items[MENU_STOP].disabled = if state.can_stop { 0 } else { 1 };

        // Uploads toggle text
        if let Ok(text) = CString::new(state.uploads_text.as_bytes()) {
            self.menu_strings[MENU_UPLOADS] = text;
            self.menu_items[MENU_UPLOADS].text = self.menu_strings[MENU_UPLOADS].as_ptr();
        }

        // Sign action text + enabled state
        if let Ok(text) = CString::new(state.sign_action_text.as_bytes()) {
            self.menu_strings[MENU_SIGN_ACTION] = text;
            self.menu_items[MENU_SIGN_ACTION].text = self.menu_strings[MENU_SIGN_ACTION].as_ptr();
        }
        self.menu_items[MENU_SIGN_ACTION].disabled = if state.auth_action_enabled { 0 } else { 1 };

        // Check for Updates enabled state
        self.menu_items[MENU_UPDATES].disabled = if state.can_check_updates { 0 } else { 1 };

        // Notifications submenu (dynamic). Rebuilt from the derived set each
        // refresh; the C side copies these strings during tray_update, so
        // replacing the vecs here is safe.
        let count = state.notifications.len();
        let parent_label = if count == 0 {
            "No notifications".to_string()
        } else {
            format!("Notifications ({})", count)
        };
        if let Ok(text) = CString::new(parent_label.as_bytes()) {
            self.menu_strings[MENU_NOTIFICATIONS] = text;
            self.menu_items[MENU_NOTIFICATIONS].text =
                self.menu_strings[MENU_NOTIFICATIONS].as_ptr();
        }
        self.menu_items[MENU_NOTIFICATIONS].disabled = if count == 0 { 1 } else { 0 };

        // Child strings: one per notification, plus a "View on dashboard" footer.
        let mut notif_strings: Vec<CString> = Vec::with_capacity(count + 1);
        for n in &state.notifications {
            notif_strings
                .push(CString::new(n.title.as_bytes()).unwrap_or_else(|_| CString::default()));
        }
        if count > 0 {
            notif_strings.push(
                CString::new("View on dashboard…").unwrap_or_else(|_| CString::default()),
            );
        }
        self.notif_strings = notif_strings;

        // Child items reuse existing action callbacks; NULL-terminated.
        let mut notif_items: Vec<TrayMenuItem> = Vec::with_capacity(count + 2);
        for (i, n) in state.notifications.iter().enumerate() {
            notif_items.push(TrayMenuItem {
                text: self.notif_strings[i].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: notification_callback(&n.action),
                submenu: std::ptr::null_mut(),
            });
        }
        if count > 0 {
            notif_items.push(TrayMenuItem {
                text: self.notif_strings[count].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_open_dashboard),
                submenu: std::ptr::null_mut(),
            });
        }
        notif_items.push(TrayMenuItem {
            text: std::ptr::null(),
            disabled: 0,
            checked: 0,
            cb: None,
            submenu: std::ptr::null_mut(),
        });
        self.notif_items = notif_items;
        self.menu_items[MENU_NOTIFICATIONS].submenu = if count == 0 {
            std::ptr::null_mut()
        } else {
            self.notif_items.as_mut_ptr()
        };

        // Icon — badge it with the notification count when there are any.
        self.badged_icon = if count > 0 {
            super::tray::render_badged_tray_icon(state.icon_state, count)
                .and_then(|p| CString::new(p.to_string_lossy().as_bytes()).ok())
        } else {
            None
        };
        self.tray.icon_filepath = match &self.badged_icon {
            Some(c) => c.as_ptr(),
            None => self.icons.path_for(state.icon_state),
        };

        // Apply
        self.tray.menu = self.menu_items.as_mut_ptr();
        unsafe {
            tray_ffi::tray_update(&mut self.tray);
        }
    }

    fn prepare_for_restart(&mut self) {
        unsafe {
            tray_ffi::tray_prepare_for_restart();
        }
    }

    fn exit(&mut self) {
        unsafe {
            tray_ffi::tray_exit();
        }
    }
}
