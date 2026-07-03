//! Configuration management for crowd-cast Agent

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

    /// Secure-input gating (withholding secrets such as passwords from capture)
    #[serde(default)]
    pub security: SecurityConfig,

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

    /// Whether the app should be configured to start automatically at OS login.
    #[serde(default)]
    pub start_on_login: bool,

    /// Idle timeout in seconds before pausing capture (0 = disabled)
    /// When no keyboard/mouse activity is detected for this duration, recording pauses automatically.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,

    /// Whether to pause uploads during idle (in addition to recording)
    #[serde(default = "default_true")]
    pub pause_uploads_on_idle: bool,

    /// On macOS, keep only the frontmost tracked application's capture source active.
    /// This avoids running multiple ScreenCaptureKit application sources at once. Linux
    /// per-app capture uses the single-active path whenever it is supported.
    #[serde(default = "default_single_active_app_capture")]
    pub single_active_app_capture: bool,

    /// macOS multi-monitor / multi-Space capture: place the focused app/display at its real
    /// spatial position on a multi-monitor–normalized canvas (parity with Windows/Linux).
    /// Kill-switch — set false to fall back to today's main-display-only capture. No effect
    /// off macOS.
    #[serde(default = "default_mac_multi_monitor_capture")]
    pub mac_multi_monitor_capture: bool,

    /// When a non-target app is frontmost, blank the video instead of keeping the last target app.
    #[serde(default = "default_true")]
    pub blank_video_on_untracked_app: bool,

    /// How long to wait for a newly switched capture source to become ready.
    #[serde(default = "default_capture_watchdog_timeout_ms")]
    pub capture_watchdog_timeout_ms: u64,

    /// Number of automatic retries before declaring the active capture source unhealthy.
    #[serde(default = "default_capture_watchdog_max_retries")]
    pub capture_watchdog_max_retries: u32,

    /// xdg-desktop-portal ScreenCast restore tokens for supported Wayland display capture,
    /// keyed by reserved identifiers such as `__display__`.
    #[serde(default)]
    pub restore_tokens: HashMap<String, String>,
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

    /// Whether to show notifications on recording start/stop
    #[serde(default = "default_true")]
    pub notify_on_start_stop: bool,

    /// Segment duration in seconds (0 = no segmentation)
    /// Recordings will be split into segments of this duration for progressive upload
    #[serde(default = "default_segment_duration_secs")]
    pub segment_duration_secs: u64,
}

fn default_segment_duration_secs() -> u64 {
    300 // 5 minutes
}

fn default_idle_timeout_secs() -> u64 {
    120 // 2 minutes of inactivity before pausing capture
}

fn default_single_active_app_capture() -> bool {
    // On by default where follow-focus per-app capture exists. On Linux this is mandatory
    // for supported per-app capture rather than a portal-backed multi-source option.
    cfg!(any(target_os = "macos", target_os = "windows", target_os = "linux"))
}

fn default_mac_multi_monitor_capture() -> bool {
    // Default on: parity with Windows/Linux, which normalize the multi-monitor canvas
    // unconditionally (no flag). Kept as a kill-switch. No effect off macOS.
    true
}

fn default_capture_watchdog_timeout_ms() -> u64 {
    1500
}

fn default_capture_watchdog_max_retries() -> u32 {
    1
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
            start_on_login: false,
            idle_timeout_secs: default_idle_timeout_secs(),
            pause_uploads_on_idle: true,
            single_active_app_capture: default_single_active_app_capture(),
            mac_multi_monitor_capture: default_mac_multi_monitor_capture(),
            blank_video_on_untracked_app: true,
            capture_watchdog_timeout_ms: default_capture_watchdog_timeout_ms(),
            capture_watchdog_max_retries: default_capture_watchdog_max_retries(),
            restore_tokens: HashMap::new(),
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
            notify_on_start_stop: true,
            segment_duration_secs: default_segment_duration_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Withhold keystrokes from capture while a secure context (e.g. a focused password
    /// field) is detected. Best-effort; the server-side scrub remains the authoritative
    /// backstop. Default: true.
    #[serde(default = "default_true")]
    pub gating_enabled: bool,

    /// On Linux, enable system accessibility (org.a11y.Status IsEnabled) at startup so
    /// applications expose their UI tree to the password-field detector. This is a
    /// system-wide, session-scoped change and should be disclosed to the user in the
    /// setup wizard. Without it, only already-accessible apps are covered. Default: true.
    #[serde(default = "default_true")]
    pub enable_accessibility: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            gating_enabled: true,
            enable_accessibility: true,
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
            security: SecurityConfig::default(),
            config_path: None,
        }
    }
}

/// The agent's own app identifier: the value `get_frontmost_app()` reports when
/// the agent itself is in the foreground. Computed once and cached.
///
/// On Windows/Linux this is the lowercased executable stem (e.g.
/// `crowd-cast-agent`), matching how frontmost detection reports apps; on macOS
/// it's the bundle identifier.
pub fn agent_self_identifier() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        #[cfg(target_os = "macos")]
        {
            "dev.crowd-cast.agent".to_string()
        }
        #[cfg(not(target_os = "macos"))]
        {
            std::env::current_exe()
                .ok()
                .and_then(|p| {
                    p.file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_ascii_lowercase())
                })
                .unwrap_or_default()
        }
    })
}

/// Whether `bundle_id` refers to the crowd-cast agent itself.
pub fn is_agent_self(bundle_id: &str) -> bool {
    let me = agent_self_identifier();
    !me.is_empty() && bundle_id.eq_ignore_ascii_case(me)
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
        let config_path = self
            .config_path
            .clone()
            .unwrap_or_else(|| Self::default_config_path().expect("Failed to get config path"));

        // Ensure parent directory exists
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {:?}", parent))?;
        }

        let contents = toml::to_string_pretty(self).context("Failed to serialize config")?;

        std::fs::write(&config_path, contents)
            .with_context(|| format!("Failed to write config file: {:?}", config_path))?;

        Ok(())
    }

    /// Get the config file path
    pub fn config_path(&self) -> PathBuf {
        self.config_path
            .clone()
            .unwrap_or_else(|| Self::default_config_path().expect("Failed to get config path"))
    }

    /// Get default config path
    fn default_config_path() -> Result<PathBuf> {
        let proj_dirs = directories::ProjectDirs::from("dev", "crowd-cast", "agent")
            .context("Failed to determine config directory")?;

        Ok(proj_dirs.config_dir().join("config.toml"))
    }

    /// Get or generate session ID
    pub fn session_id(&self) -> String {
        self.recording
            .session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    }

    /// Check if setup wizard needs to be run
    pub fn needs_setup(&self) -> bool {
        !self.capture.setup_completed
    }

    /// Check if input should be captured for the given app
    pub fn should_capture_app(&self, bundle_id: &str) -> bool {
        // Never capture the agent itself. On Windows the user can pick it from the
        // window list, and because clicking the tray brings the agent to the
        // foreground, that becomes a capture-switch + process-restart loop with no
        // way out via the UI. Exclude it before capture_all/target_apps so nothing
        // can trigger it (and any config that already lists it is effectively healed).
        if is_agent_self(bundle_id) {
            return false;
        }
        if self.capture.capture_all {
            return true;
        }

        if self.capture.target_apps.is_empty() {
            // No apps configured - don't capture anything until setup is done
            return false;
        }

        // On Windows, app identifiers are executable names whose case the user
        // can't reliably predict, so match case-insensitively. On macOS/Linux the
        // identifiers (bundle IDs / process names) are case-sensitive.
        #[cfg(target_os = "windows")]
        {
            self.capture
                .target_apps
                .iter()
                .any(|app| app.eq_ignore_ascii_case(bundle_id))
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.capture.target_apps.iter().any(|app| app == bundle_id)
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_never_captures_itself() {
        let me = agent_self_identifier();
        // In tests this is the test binary's stem; we only need it non-empty to
        // exercise the exclusion logic.
        assert!(!me.is_empty(), "agent self-identifier should resolve");

        // Excluded even when capture_all is on.
        let mut cfg = Config::default();
        cfg.capture.capture_all = true;
        assert!(
            !cfg.should_capture_app(me),
            "agent must never capture itself, even with capture_all"
        );

        // Excluded even if explicitly listed; other apps are still captured.
        cfg.capture.capture_all = false;
        cfg.capture.target_apps = vec![me.to_string(), "firefox".to_string()];
        assert!(!cfg.should_capture_app(me));
        assert!(cfg.should_capture_app("firefox"));

        // Self-exclusion is case-insensitive.
        assert!(!cfg.should_capture_app(&me.to_ascii_uppercase()));
    }
}
