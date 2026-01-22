//! OBS Process Manager
//!
//! Handles launching, monitoring, and managing the OBS process lifecycle.

use anyhow::{Context, Result};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::installer::{detect_obs, get_profile_name, get_scene_collection_name, OBSInstallation};

/// OBS process state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OBSState {
    /// OBS is not running
    Stopped,
    /// OBS is starting up
    Starting,
    /// OBS is running
    Running,
    /// OBS is stopping
    Stopping,
    /// OBS crashed or exited unexpectedly
    Crashed,
}

/// Configuration for the OBS manager
#[derive(Debug, Clone)]
pub struct OBSManagerConfig {
    /// Whether to auto-start recording when OBS launches
    pub auto_start_recording: bool,
    /// Whether to auto-start streaming when OBS launches
    pub auto_start_streaming: bool,
    /// Whether to restart OBS if it crashes
    pub auto_restart: bool,
    /// Maximum number of restart attempts
    pub max_restart_attempts: u32,
    /// Delay between restart attempts
    pub restart_delay: Duration,
    /// Use the CrowdCast profile
    pub use_crowdcast_profile: bool,
}

impl Default for OBSManagerConfig {
    fn default() -> Self {
        Self {
            auto_start_recording: false,
            auto_start_streaming: false,
            auto_restart: true,
            max_restart_attempts: 3,
            restart_delay: Duration::from_secs(5),
            use_crowdcast_profile: true,
        }
    }
}

/// Manages the OBS process lifecycle
pub struct OBSManager {
    /// OBS installation info
    installation: OBSInstallation,
    /// Child process handle
    process: Option<Child>,
    /// Current state
    state: OBSState,
    /// Configuration
    config: OBSManagerConfig,
    /// Number of restart attempts since last successful start
    restart_attempts: u32,
    /// Time of last crash
    last_crash: Option<Instant>,
    /// State change notifier
    state_tx: watch::Sender<OBSState>,
    /// State change receiver (for cloning)
    state_rx: watch::Receiver<OBSState>,
}

impl OBSManager {
    /// Create a new OBS manager
    pub fn new(config: OBSManagerConfig) -> Result<Self> {
        let installation = detect_obs()
            .context("OBS Studio not found. Please install OBS first.")?;
        
        let (state_tx, state_rx) = watch::channel(OBSState::Stopped);
        
        Ok(Self {
            installation,
            process: None,
            state: OBSState::Stopped,
            config,
            restart_attempts: 0,
            last_crash: None,
            state_tx,
            state_rx,
        })
    }
    
    /// Create with specific OBS installation
    pub fn with_installation(installation: OBSInstallation, config: OBSManagerConfig) -> Self {
        let (state_tx, state_rx) = watch::channel(OBSState::Stopped);
        
        Self {
            installation,
            process: None,
            state: OBSState::Stopped,
            config,
            restart_attempts: 0,
            last_crash: None,
            state_tx,
            state_rx,
        }
    }
    
    /// Get the current OBS state
    pub fn state(&self) -> OBSState {
        self.state
    }
    
    /// Subscribe to state changes
    pub fn subscribe(&self) -> watch::Receiver<OBSState> {
        self.state_rx.clone()
    }
    
    /// Launch OBS minimized to system tray
    pub fn launch_hidden(&mut self) -> Result<()> {
        if self.state == OBSState::Running {
            debug!("OBS is already running");
            return Ok(());
        }
        
        self.set_state(OBSState::Starting);
        
        let mut args = vec!["--minimize-to-tray".to_string()];
        
        // Use CrowdCast profile if configured
        if self.config.use_crowdcast_profile {
            args.push("--profile".to_string());
            args.push(get_profile_name().to_string());
            args.push("--collection".to_string());
            args.push(get_scene_collection_name().to_string());
        }
        
        // Auto-start recording if configured
        if self.config.auto_start_recording {
            args.push("--startrecording".to_string());
        }
        
        // Auto-start streaming if configured
        if self.config.auto_start_streaming {
            args.push("--startstreaming".to_string());
        }
        
        info!("Launching OBS with args: {:?}", args);
        
        let process = Command::new(&self.installation.executable)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to launch OBS from {:?}", self.installation.executable))?;
        
        self.process = Some(process);
        self.restart_attempts = 0;
        self.set_state(OBSState::Running);
        
        info!("OBS launched successfully");
        Ok(())
    }
    
    /// Stop OBS gracefully
    pub fn stop(&mut self) -> Result<()> {
        if self.state == OBSState::Stopped {
            return Ok(());
        }
        
        self.set_state(OBSState::Stopping);
        
        if let Some(mut process) = self.process.take() {
            // Try graceful shutdown first
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(process.id() as i32, libc::SIGTERM);
                }
            }
            
            #[cfg(windows)]
            {
                // On Windows, try to close gracefully via taskkill
                let _ = Command::new("taskkill")
                    .args(["/PID", &process.id().to_string()])
                    .output();
            }
            
            // Wait a bit for graceful shutdown
            std::thread::sleep(Duration::from_secs(2));
            
            // Force kill if still running
            match process.try_wait() {
                Ok(None) => {
                    warn!("OBS did not stop gracefully, killing...");
                    let _ = process.kill();
                }
                Ok(Some(status)) => {
                    debug!("OBS exited with status: {:?}", status);
                }
                Err(e) => {
                    error!("Error checking OBS status: {}", e);
                }
            }
        }
        
        self.set_state(OBSState::Stopped);
        info!("OBS stopped");
        Ok(())
    }
    
    /// Check if OBS is still running and handle crashes
    pub fn check_health(&mut self) -> Result<OBSState> {
        if let Some(ref mut process) = self.process {
            match process.try_wait() {
                Ok(None) => {
                    // Still running
                    if self.state != OBSState::Running {
                        self.set_state(OBSState::Running);
                    }
                }
                Ok(Some(status)) => {
                    // Process exited
                    if status.success() {
                        info!("OBS exited normally");
                        self.set_state(OBSState::Stopped);
                    } else {
                        warn!("OBS crashed with status: {:?}", status);
                        self.set_state(OBSState::Crashed);
                        self.last_crash = Some(Instant::now());
                        self.process = None;
                        
                        // Attempt auto-restart if configured
                        if self.config.auto_restart {
                            self.attempt_restart()?;
                        }
                    }
                }
                Err(e) => {
                    error!("Error checking OBS process: {}", e);
                }
            }
        }
        
        Ok(self.state)
    }
    
    /// Attempt to restart OBS after a crash
    fn attempt_restart(&mut self) -> Result<()> {
        if self.restart_attempts >= self.config.max_restart_attempts {
            error!(
                "OBS has crashed {} times, giving up on auto-restart",
                self.restart_attempts
            );
            return Ok(());
        }
        
        self.restart_attempts += 1;
        info!(
            "Attempting OBS restart ({}/{})",
            self.restart_attempts, self.config.max_restart_attempts
        );
        
        std::thread::sleep(self.config.restart_delay);
        self.launch_hidden()
    }
    
    /// Set state and notify subscribers
    fn set_state(&mut self, state: OBSState) {
        self.state = state;
        let _ = self.state_tx.send(state);
    }
    
    /// Get OBS installation info
    pub fn installation(&self) -> &OBSInstallation {
        &self.installation
    }
    
    /// Check if OBS process is responding
    pub fn is_responsive(&self) -> bool {
        self.state == OBSState::Running
    }
}

impl Drop for OBSManager {
    fn drop(&mut self) {
        if self.state == OBSState::Running {
            if let Err(e) = self.stop() {
                error!("Failed to stop OBS on drop: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_obs_manager_config_default() {
        let config = OBSManagerConfig::default();
        assert!(!config.auto_start_recording);
        assert!(config.auto_restart);
        assert_eq!(config.max_restart_attempts, 3);
    }
}
