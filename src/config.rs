//! Configuration management for crowd-cast Agent

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Main configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Capture configuration (which apps to capture)
    #[serde(default)]
    pub capture: CaptureConfig,

    /// Input capture configuration
    #[serde(default)]
    pub input: InputConfig,

    /// Upload configuration
    #[serde(default)]
    pub upload: UploadConfig,

    /// Recording configuration
    #[serde(default)]
    pub recording: RecordingConfig,

    /// Path to config file (not serialized)
    #[serde(skip)]
    config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConfig {
    /// List of app bundle IDs (macOS) or process names (Linux/Windows) to capture
    /// When empty, capture all apps (or use capture_all flag)
    #[serde(default)]
    pub target_apps: Vec<String>,

    /// If true, capture input for all applications (ignore target_apps)
    #[serde(default)]
    pub capture_all: bool,

    /// Polling interval for frontmost app detection (ms)
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,

    /// Whether setup wizard has been completed
    #[serde(default)]
    pub setup_completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputConfig {
    /// Whether to capture keyboard events
    #[serde(default = "default_true")]
    pub capture_keyboard: bool,

    /// Whether to capture mouse movement
    #[serde(default = "default_true")]
    pub capture_mouse_move: bool,

    /// Whether to capture mouse clicks
    #[serde(default = "default_true")]
    pub capture_mouse_click: bool,

    /// Whether to capture mouse scroll
    #[serde(default = "default_true")]
    pub capture_mouse_scroll: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadConfig {
    /// Lambda endpoint for getting pre-signed URLs
    pub lambda_endpoint: Option<String>,

    /// Whether to delete local files after successful upload
    #[serde(default = "default_true")]
    pub delete_after_upload: bool,

    /// Maximum concurrent uploads
    #[serde(default = "default_max_uploads")]
    pub max_concurrent_uploads: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingConfig {
    /// Directory to watch for OBS recording chunks
    #[serde(default = "default_recording_output_directory_option")]
    pub output_directory: Option<PathBuf>,

    /// Whether to start recording automatically on launch
    #[serde(default = "default_autostart_on_launch")]
    pub autostart_on_launch: bool,

    /// Session ID (auto-generated if not set)
    pub session_id: Option<String>,
}

// Default value functions
fn default_poll_interval() -> u64 {
    100 // 100ms for responsive frontmost app detection
}

fn default_true() -> bool {
    true
}

fn default_max_uploads() -> usize {
    2
}

fn default_recording_output_directory() -> PathBuf {
    std::env::temp_dir().join("crowd-cast-recordings")
}

fn default_recording_output_directory_option() -> Option<PathBuf> {
    Some(default_recording_output_directory())
}

fn default_autostart_on_launch() -> bool {
    true
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            target_apps: Vec::new(),
            capture_all: false,
            poll_interval_ms: default_poll_interval(),
            setup_completed: false,
        }
    }
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            capture_keyboard: true,
            capture_mouse_move: true,
            capture_mouse_click: true,
            capture_mouse_scroll: true,
        }
    }
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            lambda_endpoint: None,
            delete_after_upload: true,
            max_concurrent_uploads: default_max_uploads(),
        }
    }
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            output_directory: Some(default_recording_output_directory()),
            autostart_on_launch: default_autostart_on_launch(),
            session_id: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            capture: CaptureConfig::default(),
            input: InputConfig::default(),
            upload: UploadConfig::default(),
            recording: RecordingConfig::default(),
            config_path: None,
        }
    }
}

impl Config {
    /// Load configuration from default location or create default
    pub fn load() -> Result<Self> {
        let config_path = Self::default_config_path()?;

        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read config file: {:?}", config_path))?;

            let mut config: Config = toml::from_str(&contents)
                .with_context(|| format!("Failed to parse config file: {:?}", config_path))?;

            config.config_path = Some(config_path);
            Ok(config)
        } else {
            // Create default config
            let config = Config::default();
            config.save()?;
            Ok(config)
        }
    }

    /// Save configuration to file
    pub fn save(&self) -> Result<()> {
        let config_path = self.config_path.clone().unwrap_or_else(|| {
            Self::default_config_path().expect("Failed to get config path")
        });

        // Ensure parent directory exists
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {:?}", parent))?;
        }

        let contents = toml::to_string_pretty(self)
            .context("Failed to serialize config")?;

        std::fs::write(&config_path, contents)
            .with_context(|| format!("Failed to write config file: {:?}", config_path))?;

        Ok(())
    }

    /// Get the config file path
    pub fn config_path(&self) -> PathBuf {
        self.config_path.clone().unwrap_or_else(|| {
            Self::default_config_path().expect("Failed to get config path")
        })
    }

    /// Get default config path
    fn default_config_path() -> Result<PathBuf> {
        let proj_dirs = directories::ProjectDirs::from("dev", "crowd-cast", "agent")
            .context("Failed to determine config directory")?;

        Ok(proj_dirs.config_dir().join("config.toml"))
    }

    /// Get or generate session ID
    pub fn session_id(&self) -> String {
        self.recording.session_id.clone().unwrap_or_else(|| {
            uuid::Uuid::new_v4().to_string()
        })
    }

    /// Check if setup wizard needs to be run
    pub fn needs_setup(&self) -> bool {
        !self.capture.setup_completed
    }

    /// Check if input should be captured for the given app
    pub fn should_capture_app(&self, bundle_id: &str) -> bool {
        if self.capture.capture_all {
            return true;
        }
        
        if self.capture.target_apps.is_empty() {
            // No apps configured - don't capture anything until setup is done
            return false;
        }
        
        self.capture.target_apps.iter().any(|app| app == bundle_id)
    }

    /// Mark setup as completed and save
    pub fn complete_setup(&mut self) -> Result<()> {
        self.capture.setup_completed = true;
        self.save()
    }

    /// Add an app to the capture list
    pub fn add_target_app(&mut self, bundle_id: String) {
        if !self.capture.target_apps.contains(&bundle_id) {
            self.capture.target_apps.push(bundle_id);
        }
    }

    /// Remove an app from the capture list
    pub fn remove_target_app(&mut self, bundle_id: &str) {
        self.capture.target_apps.retain(|app| app != bundle_id);
    }

    /// Clear all target apps
    pub fn clear_target_apps(&mut self) {
        self.capture.target_apps.clear();
    }
}
