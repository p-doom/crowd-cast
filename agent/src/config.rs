//! Configuration management for crowd-cast Agent

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Main configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// OBS WebSocket configuration
    #[serde(default)]
    pub obs: ObsConfig,

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
pub struct ObsConfig {
    /// OBS WebSocket host
    #[serde(default = "default_obs_host")]
    pub host: String,

    /// OBS WebSocket port
    #[serde(default = "default_obs_port")]
    pub port: u16,

    /// OBS WebSocket password (optional)
    pub password: Option<String>,

    /// Polling interval for hooked state (ms)
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
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

    /// Session ID (auto-generated if not set)
    pub session_id: Option<String>,
}

// Default value functions
fn default_obs_host() -> String {
    "localhost".to_string()
}

fn default_obs_port() -> u16 {
    4455
}

fn default_poll_interval() -> u64 {
    150 // 150ms for responsive capture state changes
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

impl Default for ObsConfig {
    fn default() -> Self {
        Self {
            host: default_obs_host(),
            port: default_obs_port(),
            password: None,
            poll_interval_ms: default_poll_interval(),
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
            session_id: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            obs: ObsConfig::default(),
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
}
