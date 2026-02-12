//! GUI-based setup wizard
//!
//! On macOS, this uses native Cocoa UI via FFI.
//! On other platforms, a fallback implementation may be used.

use anyhow::Result;
use tracing::info;

use crate::capture::list_capturable_apps;
use crate::config::Config;
use crate::installer::autostart::{enable_autostart, AutostartConfig};

#[cfg(target_os = "macos")]
use super::wizard_ffi::{self, AppInfoWrapper};

/// Result of running the GUI wizard
#[derive(Debug, Clone)]
pub struct WizardResult {
    /// Whether setup completed successfully
    pub completed: bool,
    /// Selected applications for capture
    pub selected_apps: Vec<String>,
    /// Whether to capture all apps
    pub capture_all: bool,
    /// Whether autostart was enabled
    pub autostart_enabled: bool,
}

impl Default for WizardResult {
    fn default() -> Self {
        Self {
            completed: false,
            selected_apps: vec![],
            capture_all: false,
            autostart_enabled: false,
        }
    }
}

/// Run the GUI setup wizard
///
/// On macOS, this launches a native Cocoa window.
/// On other platforms, returns an error indicating native wizard is not available.
pub fn run_wizard_gui(config: &mut Config) -> Result<WizardResult> {
    info!("Starting native setup wizard");

    #[cfg(target_os = "macos")]
    {
        run_wizard_macos(config)
    }

    #[cfg(not(target_os = "macos"))]
    {
        // For non-macOS, we could fall back to CLI wizard or return error
        anyhow::bail!("Native GUI wizard is only available on macOS. Use --setup for CLI wizard.");
    }
}

#[cfg(target_os = "macos")]
fn run_wizard_macos(config: &mut Config) -> Result<WizardResult> {
    // Get list of available apps
    info!("Loading available applications...");
    let apps = list_capturable_apps();

    // Convert to FFI format
    let app_wrappers: Vec<AppInfoWrapper> = apps
        .iter()
        .map(|a| AppInfoWrapper::new(&a.bundle_id, &a.name, a.pid))
        .collect();

    // Set apps in the native wizard
    wizard_ffi::set_available_apps(&app_wrappers);

    // Run the native wizard (blocks until closed)
    info!("Launching native wizard window...");
    let native_result = wizard_ffi::run_native_wizard();

    // Convert result
    let result = WizardResult {
        completed: native_result.completed,
        selected_apps: native_result.selected_apps.clone(),
        capture_all: native_result.capture_all,
        autostart_enabled: native_result.enable_autostart,
    };

    // If wizard completed, update and save config
    if result.completed {
        info!("Wizard completed successfully");

        // Update config
        config.capture.capture_all = result.capture_all;
        config.capture.target_apps = result.selected_apps.clone();
        config.capture.setup_completed = true;

        // Enable autostart if requested
        if result.autostart_enabled {
            let autostart_config = AutostartConfig::default();
            if let Err(e) = enable_autostart(&autostart_config) {
                info!("Failed to enable autostart: {}", e);
            } else {
                info!("Autostart enabled");
            }
        }

        // Save config
        config.save()?;
        info!("Configuration saved");
    } else {
        info!("Wizard was cancelled");
    }

    Ok(result)
}
