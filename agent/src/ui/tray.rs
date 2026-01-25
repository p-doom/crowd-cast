//! System tray application using dmikushin/tray FFI
//!
//! Provides a system tray UI for controlling the CrowdCast agent.

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

        let tooltip = CString::new("CrowdCast Agent")?;

        // Create menu items
        // Menu strings must be kept alive
        let status_text = CString::new("Status: Idle")?;
        let separator = CString::new("-")?;
        let start_text = CString::new("Start Recording")?;
        let stop_text = CString::new("Stop Recording")?;
        let pause_capture_text = CString::new("Pause Capture")?;
        let resume_capture_text = CString::new("Resume Capture")?;
        let config_text = CString::new("Open Config")?;
        let quit_text = CString::new("Quit")?;

        let menu_strings = vec![
            status_text,
            separator.clone(),
            start_text,
            stop_text,
            separator.clone(),
            pause_capture_text,
            resume_capture_text,
            separator.clone(),
            config_text,
            separator,
            quit_text,
        ];

        // Build menu items array (NULL-terminated)
        // Indices: 0=status, 1=sep, 2=start, 3=stop, 4=sep, 5=pause, 6=resume, 7=sep, 8=config, 9=sep, 10=quit
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
                text: menu_strings[2].as_ptr(), // Start Recording
                disabled: 0,
                checked: 0,
                cb: Some(on_start_capture),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[3].as_ptr(), // Stop Recording
                disabled: 0,
                checked: 0,
                cb: Some(on_stop_capture),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[4].as_ptr(), // separator
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[5].as_ptr(), // Pause Capture (manual mode)
                disabled: 0,
                checked: 0,
                cb: Some(on_pause_capture),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[6].as_ptr(), // Resume Capture (manual mode)
                disabled: 0,
                checked: 0,
                cb: Some(on_resume_capture),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[7].as_ptr(), // separator
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[8].as_ptr(), // Open Config
                disabled: 0,
                checked: 0,
                cb: Some(on_open_config),
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[9].as_ptr(), // separator
                disabled: 0,
                checked: 0,
                cb: None,
                submenu: std::ptr::null_mut(),
            },
            TrayMenuItem {
                text: menu_strings[10].as_ptr(), // Quit
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
        let (status_text, icon_state) = match status {
            EngineStatus::Idle => ("Status: Idle".to_string(), TrayIconState::Idle),
            EngineStatus::Capturing { event_count } => {
                (
                    format!("Status: Capturing ({} events)", event_count),
                    TrayIconState::Recording,
                )
            }
            EngineStatus::RecordingBlocked => {
                (
                    "Status: Recording (no capture sources)".to_string(),
                    TrayIconState::Blocked,
                )
            }
            EngineStatus::WaitingForOBS => {
                ("Status: Waiting for OBS...".to_string(), TrayIconState::Blocked)
            }
            EngineStatus::Uploading { chunk_id } => (
                format!("Status: Uploading {}", chunk_id),
                TrayIconState::Idle,
            ),
            EngineStatus::Error(msg) => {
                (
                    format!("Status: Error - {}", truncate_str(msg, 30)),
                    TrayIconState::Idle,
                )
            }
        };

        // Update the status menu item text
        if let Ok(new_text) = CString::new(status_text.as_bytes()) {
            // We need to update the menu string and refresh
            // For simplicity, we store the new string and update the pointer
            if !self._menu_strings.is_empty() {
                self._menu_strings[0] = new_text;
                self._menu_items[0].text = self._menu_strings[0].as_ptr();
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

unsafe extern "C" fn on_pause_capture(_item: *mut TrayMenuItem) {
    info!("Pause capture requested via tray (manual mode)");
    if let Some(sender) = CMD_SENDER.lock().unwrap().as_ref() {
        if let Err(e) = sender.try_send(EngineCommand::SetCaptureEnabled(false)) {
            error!("Failed to send pause capture command: {}", e);
        }
    }
}

unsafe extern "C" fn on_resume_capture(_item: *mut TrayMenuItem) {
    info!("Resume capture requested via tray (manual mode)");
    if let Some(sender) = CMD_SENDER.lock().unwrap().as_ref() {
        if let Err(e) = sender.try_send(EngineCommand::SetCaptureEnabled(true)) {
            error!("Failed to send resume capture command: {}", e);
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
    let icon_dir = directories::ProjectDirs::from("dev", "crowdcast", "agent")
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
    let logo_bytes = include_bytes!("../../../assets/logo.png");
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
