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

use super::tray_ffi::{self, Tray, TrayMenuItem};
use crate::sync::{EngineCommand, EngineStatus};

// Global state for callbacks (required because C callbacks can't capture Rust state)
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static CMD_SENDER: Mutex<Option<mpsc::Sender<EngineCommand>>> = Mutex::new(None);

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
}

impl TrayApp {
    /// Create a new tray application with channels for engine communication
    pub fn new(
        cmd_tx: mpsc::Sender<EngineCommand>,
        status_rx: broadcast::Receiver<EngineStatus>,
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

        let tooltip = CString::new("crowd-cast Agent")?;

        // Create menu items
        // Menu strings must be kept alive
        // Note: We use indices to update text dynamically based on state
        let status_text = CString::new("Status: Idle")?;
        let separator = CString::new("-")?;
        let start_text = CString::new("Start Recording")?;    // Index 2 - shown when idle
        let pause_text = CString::new("Pause Recording")?;    // Index 3 - shown when recording
        let resume_text = CString::new("Resume Recording")?;  // Index 4 - shown when paused
        let stop_text = CString::new("Stop Recording")?;      // Index 5 - shown when recording/paused
        let refresh_text = CString::new("Refresh Sources")?;  // Index 6 - always available
        let config_text = CString::new("Open Config")?;
        let quit_text = CString::new("Quit")?;

        let menu_strings = vec![
            status_text,      // 0
            separator.clone(), // 1
            start_text,       // 2
            pause_text,       // 3
            resume_text,      // 4
            stop_text,        // 5
            separator.clone(), // 6
            refresh_text,     // 7
            separator.clone(), // 8
            config_text,      // 9
            separator.clone(), // 10
            quit_text,        // 11
        ];

        // Build menu items array (NULL-terminated)
        // Menu indices: 0=status, 1=sep, 2=start, 3=pause, 4=resume, 5=stop, 6=sep, 7=refresh, 8=sep, 9=config, 10=sep, 11=quit
        // Initially: Start visible, Pause/Resume/Stop hidden (idle state)
        let mut menu_items = vec![
            TrayMenuItem {
                text: menu_strings[0].as_ptr(), // Status
                disabled: 1, // Status is not clickable
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[1].as_ptr(), // separator
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[2].as_ptr(), // Start Recording (visible when idle)
                disabled: 0,
                checked: 0,
                cb: Some(on_start_capture),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[3].as_ptr(), // Pause Recording (visible when recording)
                disabled: 1, // Initially hidden (disabled) - idle state
                checked: 0,
                cb: Some(on_pause_recording),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[4].as_ptr(), // Resume Recording (visible when paused)
                disabled: 1, // Initially hidden (disabled) - idle state
                checked: 0,
                cb: Some(on_resume_recording),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[5].as_ptr(), // Stop Recording (visible when recording/paused)
                disabled: 1, // Initially hidden (disabled) - idle state
                checked: 0,
                cb: Some(on_stop_capture),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[6].as_ptr(), // separator
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[7].as_ptr(), // Refresh Sources
                disabled: 0,
                checked: 0,
                cb: Some(on_refresh_sources),
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
                text: menu_strings[9].as_ptr(), // Open Config
                disabled: 0,
                checked: 0,
                cb: Some(on_open_config),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[10].as_ptr(), // separator
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[11].as_ptr(), // Quit
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
                // Send shutdown command to engine (use try_send to avoid blocking)
                let _ = self.cmd_tx.try_send(EngineCommand::Shutdown);
                break;
            }

            // Small sleep to prevent busy loop when no events
            std::thread::sleep(std::time::Duration::from_millis(16));
        }

        info!("Tray event loop exited");
        Ok(())
    }

    /// Update the status display based on engine status
    fn update_status(&mut self, status: &EngineStatus) {
        // Determine status text, icon state, and menu state
        #[derive(Clone, Copy, PartialEq)]
        enum MenuState {
            Idle,       // Show: Start
            Recording,  // Show: Pause, Stop
            Paused,     // Show: Resume, Stop
        }

        let (status_text, icon_state, menu_state) = match status {
            EngineStatus::Idle => ("Status: Idle".to_string(), TrayIconState::Idle, MenuState::Idle),
            EngineStatus::Capturing { event_count } => {
                (
                    format!("Status: Capturing ({} events)", event_count),
                    TrayIconState::Recording,
                    MenuState::Recording,
                )
            }
            EngineStatus::Paused => {
                ("Status: Paused".to_string(), TrayIconState::Paused, MenuState::Paused)
            }
            EngineStatus::RecordingBlocked => {
                (
                    "Status: Recording (no capture sources)".to_string(),
                    TrayIconState::Blocked,
                    MenuState::Recording,
                )
            }
            EngineStatus::WaitingForOBS => {
                ("Status: Waiting for OBS...".to_string(), TrayIconState::Blocked, MenuState::Idle)
            }
            EngineStatus::Uploading { chunk_id } => (
                format!("Status: Uploading {}", chunk_id),
                TrayIconState::Idle,
                MenuState::Idle,
            ),
            EngineStatus::Error(msg) => {
                (
                    format!("Status: Error - {}", truncate_str(msg, 30)),
                    TrayIconState::Idle,
                    MenuState::Idle,
                )
            }
        };

        // Update the status menu item text and menu item visibility
        if let Ok(new_text) = CString::new(status_text.as_bytes()) {
            if !self._menu_strings.is_empty() {
                // Update status text
                self._menu_strings[0] = new_text;
                self._menu_items[0].text = self._menu_strings[0].as_ptr();

                // Update menu item visibility based on state
                // Menu indices: 2=start, 3=pause, 4=resume, 5=stop
                match menu_state {
                    MenuState::Idle => {
                        // Show: Start, Hide: Pause, Resume, Stop
                        self._menu_items[2].disabled = 0; // Start - enabled
                        self._menu_items[3].disabled = 1; // Pause - disabled
                        self._menu_items[4].disabled = 1; // Resume - disabled
                        self._menu_items[5].disabled = 1; // Stop - disabled
                    }
                    MenuState::Recording => {
                        // Show: Pause, Stop, Hide: Start, Resume
                        self._menu_items[2].disabled = 1; // Start - disabled
                        self._menu_items[3].disabled = 0; // Pause - enabled
                        self._menu_items[4].disabled = 1; // Resume - disabled
                        self._menu_items[5].disabled = 0; // Stop - enabled
                    }
                    MenuState::Paused => {
                        // Show: Resume, Stop, Hide: Start, Pause
                        self._menu_items[2].disabled = 1; // Start - disabled
                        self._menu_items[3].disabled = 1; // Pause - disabled
                        self._menu_items[4].disabled = 0; // Resume - enabled
                        self._menu_items[5].disabled = 0; // Stop - enabled
                    }
                }

                self.tray.menu = self._menu_items.as_mut_ptr();
                self.tray.icon_filepath = self._icons.path_for(icon_state);
                unsafe {
                    tray_ffi::tray_update(&mut self.tray);
                }
            }
        }

        debug!("Tray status updated: {}", status_text);
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

unsafe extern "C" fn on_refresh_sources(_item: *mut TrayMenuItem) {
    info!("Refresh sources requested via tray");
    if let Some(sender) = CMD_SENDER.lock().unwrap().as_ref() {
        if let Err(e) = sender.try_send(EngineCommand::RefreshSources) {
            error!("Failed to send refresh sources command: {}", e);
        }
    }
}

unsafe extern "C" fn on_open_config(_item: *mut TrayMenuItem) {
    info!("Open config requested via tray");
    if let Err(e) = open_config() {
        error!("Failed to open config: {}", e);
    }
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
    let ext = if cfg!(target_os = "windows") { "ico" } else { "png" };

    let paths = TrayIconPaths {
        idle: icon_dir.join(format!("tray_idle.{}", ext)),
        recording: icon_dir.join(format!("tray_recording.{}", ext)),
        blocked: icon_dir.join(format!("tray_blocked.{}", ext)),
    };

    let needs_create = !paths.idle.exists()
        || !paths.recording.exists()
        || !paths.blocked.exists();

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
        (TrayIconState::Recording, [76, 175, 80, 255], &paths.recording),
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
        image.resize_exact(size, size, FilterType::Lanczos3).to_rgba8()
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

/// Open the config file in the default editor
fn open_config() -> Result<()> {
    let config = crate::config::Config::load()?;
    let config_path = config.config_path();

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&config_path)
            .spawn()?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&config_path)
            .spawn()?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("notepad")
            .arg(&config_path)
            .spawn()?;
    }

    Ok(())
}
