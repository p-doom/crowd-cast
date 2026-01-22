//! System tray application using tray-icon
//!
//! Provides a system tray UI for controlling the CrowdCast agent.

use anyhow::Result;
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tokio::sync::{broadcast, mpsc};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tracing::{debug, error, info, warn};

use crate::sync::{EngineCommand, EngineStatus};

/// System tray application
pub struct TrayApp {
    _tray: TrayIcon,
    cmd_tx: mpsc::Sender<EngineCommand>,
    status_rx: broadcast::Receiver<EngineStatus>,
    status_item: MenuItem,
}

impl TrayApp {
    /// Create a new tray application with channels for engine communication
    pub fn new(
        cmd_tx: mpsc::Sender<EngineCommand>,
        status_rx: broadcast::Receiver<EngineStatus>,
    ) -> Result<Self> {
        info!("Initializing system tray UI");
        
        // Build the tray menu
        let menu = Menu::new();
        
        let status_item = MenuItem::new("Status: Initializing...", false, None);
        let separator1 = PredefinedMenuItem::separator();
        let start_item = MenuItem::with_id("start", "Start Capture", true, None);
        let stop_item = MenuItem::with_id("stop", "Stop Capture", true, None);
        let separator2 = PredefinedMenuItem::separator();
        let config_item = MenuItem::with_id("config", "Open Config", true, None);
        let separator3 = PredefinedMenuItem::separator();
        let quit_item = MenuItem::with_id("quit", "Quit", true, None);
        
        menu.append(&status_item)?;
        menu.append(&separator1)?;
        menu.append(&start_item)?;
        menu.append(&stop_item)?;
        menu.append(&separator2)?;
        menu.append(&config_item)?;
        menu.append(&separator3)?;
        menu.append(&quit_item)?;
        
        // Create tray icon
        let icon = load_icon()?;
        
        let _tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("CrowdCast Agent")
            .with_icon(icon)
            .build()?;
        
        info!("System tray created");
        
        Ok(Self {
            _tray,
            cmd_tx,
            status_rx,
            status_item,
        })
    }
    
    /// Run the tray application event loop (blocks until quit)
    pub fn run(mut self) -> Result<()> {
        info!("Starting system tray event loop");
        
        let menu_channel = MenuEvent::receiver();
        let tray_channel = TrayIconEvent::receiver();
        
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
            
            // Check for menu events
            if let Ok(event) = menu_channel.try_recv() {
                match event.id.0.as_str() {
                    "start" => {
                        info!("Start capture requested via tray");
                        if let Err(e) = self.cmd_tx.blocking_send(EngineCommand::StartCapture) {
                            error!("Failed to send start command: {}", e);
                        }
                    }
                    "stop" => {
                        info!("Stop capture requested via tray");
                        if let Err(e) = self.cmd_tx.blocking_send(EngineCommand::StopCapture) {
                            error!("Failed to send stop command: {}", e);
                        }
                    }
                    "config" => {
                        info!("Open config requested");
                        if let Err(e) = open_config() {
                            error!("Failed to open config: {}", e);
                        }
                    }
                    "quit" => {
                        info!("Quit requested via tray");
                        // Send shutdown command to engine
                        let _ = self.cmd_tx.blocking_send(EngineCommand::Shutdown);
                        break;
                    }
                    _ => {}
                }
            }
            
            // Check for tray icon events (click, etc.)
            if let Ok(event) = tray_channel.try_recv() {
                match event {
                    TrayIconEvent::Click { .. } => {
                        debug!("Tray icon clicked");
                        // Request current status
                        let _ = self.cmd_tx.blocking_send(EngineCommand::GetStatus);
                    }
                    _ => {}
                }
            }
            
            // Small sleep to prevent busy loop
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        
        info!("Tray event loop exited");
        Ok(())
    }
    
    /// Update the status display based on engine status
    fn update_status(&self, status: &EngineStatus) {
        let status_text = match status {
            EngineStatus::Idle => "Status: Idle".to_string(),
            EngineStatus::Capturing { event_count, .. } => {
                format!("Status: Capturing ({} events)", event_count)
            }
            EngineStatus::Uploading { chunk_id } => {
                format!("Status: Uploading {}", chunk_id)
            }
            EngineStatus::Error(msg) => {
                format!("Status: Error - {}", truncate_str(msg, 30))
            }
        };
        
        self.status_item.set_text(&status_text);
        debug!("Tray status updated: {}", status_text);
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

/// Load the tray icon
fn load_icon() -> Result<Icon> {
    // Create a simple colored icon programmatically
    // In production, you'd load this from a file
    let size = 32u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    
    for y in 0..size {
        for x in 0..size {
            // Create a simple gradient circle icon
            let dx = x as f32 - size as f32 / 2.0;
            let dy = y as f32 - size as f32 / 2.0;
            let dist = (dx * dx + dy * dy).sqrt();
            let radius = size as f32 / 2.0 - 2.0;
            
            if dist < radius {
                // Inside circle - green for "recording" feel
                rgba.push(76);   // R
                rgba.push(175);  // G
                rgba.push(80);   // B
                rgba.push(255);  // A
            } else {
                // Outside circle - transparent
                rgba.push(0);
                rgba.push(0);
                rgba.push(0);
                rgba.push(0);
            }
        }
    }
    
    Ok(Icon::from_rgba(rgba, size, size)?)
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
