//! macOS tray implementation using tray_ffi (dmikushin/tray).
//!
//! Wraps the C FFI tray library with atomic-bool-based callbacks.
//! All business logic lives in `tray.rs`; this file only handles
//! native menu rendering and event collection.

use anyhow::Result;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
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
static SETTINGS_REQUESTED: AtomicBool = AtomicBool::new(false);
static TOGGLE_UPLOADS_REQUESTED: AtomicBool = AtomicBool::new(false);
static SIGN_IN_REQUESTED: AtomicBool = AtomicBool::new(false);
static MACOS_QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

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
// 2 = separator
const MENU_START: usize = 3;
const MENU_STOP: usize = 4;
// 5 = panic (text never changes)
// 6 = separator
const MENU_UPLOADS: usize = 7;
const MENU_SIGN_ACTION: usize = 8;
// 9 = settings (text never changes)
const MENU_UPDATES: usize = 10;
// 11 = separator
// 12 = quit
// 13 = NULL terminator

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
}

impl MacOSTray {
    pub fn new(icon_paths: &TrayIconPaths) -> Result<Self> {
        let icons = TrayIconCStrings::new(icon_paths)?;
        let tooltip = CString::new("crowd-cast Agent")?;

        // Initial menu strings (overwritten by the first update() call)
        let menu_strings = vec![
            CString::new("Status: Idle")?,          // 0: status
            CString::new("")?,                       // 1: account
            CString::new("-")?,                      // 2: separator
            CString::new("Start Recording")?,        // 3
            CString::new("Stop Recording")?,         // 4
            CString::new("Delete last 10 minutes")?, // 5: panic
            CString::new("-")?,                      // 6: separator
            CString::new("Pause Uploads")?,          // 7
            CString::new("Sign in with Google")?,    // 8
            CString::new("Settings")?,               // 9
            CString::new("Check for Updates")?,      // 10
            CString::new("-")?,                      // 11: separator
            CString::new("Quit")?,                   // 12
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
            // 2: Separator
            TrayMenuItem {
                text: menu_strings[2].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            // 3: Start Recording
            TrayMenuItem {
                text: menu_strings[3].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_start_capture),
                submenu: std::ptr::null_mut(),
            },
            // 4: Stop Recording
            TrayMenuItem {
                text: menu_strings[4].as_ptr(),
                disabled: 1,
                checked: 0,
                cb: Some(on_stop_capture),
                submenu: std::ptr::null_mut(),
            },
            // 5: Panic
            TrayMenuItem {
                text: menu_strings[5].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_panic),
                submenu: std::ptr::null_mut(),
            },
            // 6: Separator
            TrayMenuItem {
                text: menu_strings[6].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            // 7: Pause/Resume Uploads
            TrayMenuItem {
                text: menu_strings[7].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_toggle_uploads),
                submenu: std::ptr::null_mut(),
            },
            // 8: Sign in / Sign out
            TrayMenuItem {
                text: menu_strings[8].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_sign_in),
                submenu: std::ptr::null_mut(),
            },
            // 9: Settings
            TrayMenuItem {
                text: menu_strings[9].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_settings),
                submenu: std::ptr::null_mut(),
            },
            // 10: Check for Updates
            TrayMenuItem {
                text: menu_strings[10].as_ptr(),
                disabled: 1,
                checked: 0,
                cb: Some(on_check_for_updates),
                submenu: std::ptr::null_mut(),
            },
            // 11: Separator
            TrayMenuItem {
                text: menu_strings[11].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            // 12: Quit
            TrayMenuItem {
                text: menu_strings[12].as_ptr(),
                disabled: 0,
                checked: 0,
                cb: Some(on_quit),
                submenu: std::ptr::null_mut(),
            },
            // 13: NULL terminator
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

        Ok(Self {
            tray,
            icons,
            _tooltip: tooltip,
            menu_items,
            menu_strings,
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
        SETTINGS_REQUESTED.store(false, Ordering::SeqCst);
        TOGGLE_UPLOADS_REQUESTED.store(false, Ordering::SeqCst);
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
            self.menu_items[MENU_SIGN_ACTION].text =
                self.menu_strings[MENU_SIGN_ACTION].as_ptr();
        }
        self.menu_items[MENU_SIGN_ACTION].disabled =
            if state.auth_action_enabled { 0 } else { 1 };

        // Check for Updates enabled state
        self.menu_items[MENU_UPDATES].disabled = if state.can_check_updates { 0 } else { 1 };

        // Icon
        self.tray.icon_filepath = self.icons.path_for(state.icon_state);

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
