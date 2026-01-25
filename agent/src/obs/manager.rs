//! OBS Process Manager
//!
//! Handles launching, monitoring, and managing the OBS process lifecycle.

use anyhow::{Context, Result};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
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
}

/// Configuration for the OBS manager
#[derive(Debug, Clone)]
pub struct OBSManagerConfig {
    /// Whether to auto-start recording when OBS launches
    pub auto_start_recording: bool,
    /// Whether to auto-start streaming when OBS launches
    pub auto_start_streaming: bool,
    /// Use the CrowdCast profile
    pub use_crowdcast_profile: bool,
}

impl Default for OBSManagerConfig {
    fn default() -> Self {
        Self {
            auto_start_recording: false,
            auto_start_streaming: false,
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
}

impl OBSManager {
    /// Create a new OBS manager
    pub fn new(config: OBSManagerConfig) -> Result<Self> {
        let installation = detect_obs()
            .context("OBS Studio not found. Please install OBS first.")?;

        Ok(Self {
            installation,
            process: None,
            state: OBSState::Stopped,
            config,
        })
    }
    
    /// Launch OBS minimized to system tray
    pub fn launch_hidden(&mut self) -> Result<()> {
        self.refresh_process_state();
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
    
    /// Set state and notify subscribers
    fn set_state(&mut self, state: OBSState) {
        self.state = state;
    }

    fn refresh_process_state(&mut self) {
        if self.state != OBSState::Running {
            return;
        }

        let mut exited = false;
        if let Some(process) = self.process.as_mut() {
            match process.try_wait() {
                Ok(Some(status)) => {
                    debug!("OBS process exited: {:?}", status);
                    exited = true;
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("Failed to check OBS process status: {}", e);
                }
            }
        } else {
            exited = true;
        }

        if exited {
            self.process = None;
            self.state = OBSState::Stopped;
        }
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
        assert!(!config.auto_start_streaming);
    }
}
