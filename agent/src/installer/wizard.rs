//! First-run setup wizard for crowd-cast
//!
//! The wizard follows this flow:
//! 1. Detect/Install OBS
//! 2. Install Plugin
//! 3. Create Profile
//! 4. Configure OBS WebSocket
//! 5. Launch OBS (so plugin loads)
//! 6. Select Applications (requires OBS running)
//! 7. Request Permissions
//! 8. Setup Autostart

use anyhow::Result;
use obws::Client;
use std::io::{self, Write};
use std::process::Command;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use super::{
    app_selector::{
        create_capture_sources, display_selection_ui, get_available_windows, select_suggested_apps,
        AvailableWindowsResponse, CreateSourceWindow,
    },
    autostart::{enable_autostart, is_autostart_enabled, AutostartConfig},
    obs_detector::{detect_obs, is_obs_running, open_obs_download_page, OBSInstallation},
    permissions::{check_permissions, request_permissions, PermissionState},
    plugin_install::{check_plugin_installed, install_plugin_async},
    profile::{create_profile, create_scene_collection, detect_best_encoder, profile_exists},
};
use crate::config::Config;
use crate::obs::{OBSManager, OBSManagerConfig};

/// Result of running the setup wizard
#[derive(Debug)]
pub struct SetupResult {
    /// Whether setup completed successfully
    pub success: bool,
    /// OBS installation found/configured
    pub obs_installation: Option<OBSInstallation>,
    /// Whether the plugin was installed
    pub plugin_installed: bool,
    /// Whether the profile was created
    pub profile_created: bool,
    /// Number of capture sources created
    pub sources_created: usize,
    /// Whether all permissions are granted
    pub permissions_granted: bool,
    /// Whether autostart was enabled
    pub autostart_enabled: bool,
    /// Any warnings or notes
    pub notes: Vec<String>,
}

/// Configuration for the setup wizard
#[derive(Debug, Clone)]
pub struct WizardConfig {
    /// Run in non-interactive mode (use defaults)
    pub non_interactive: bool,
    /// Skip permission requests
    pub skip_permissions: bool,
    /// Skip autostart setup
    pub skip_autostart: bool,
    /// Skip application selection
    pub skip_app_selection: bool,
    /// Force reinstall of plugin
    pub force_plugin_reinstall: bool,
    /// Force recreate profile
    pub force_profile_recreate: bool,
    /// Timeout for waiting for OBS WebSocket
    pub websocket_timeout: Duration,
}

impl Default for WizardConfig {
    fn default() -> Self {
        Self {
            non_interactive: false,
            skip_permissions: false,
            skip_autostart: false,
            skip_app_selection: false,
            force_plugin_reinstall: false,
            force_profile_recreate: false,
            websocket_timeout: Duration::from_secs(30),
        }
    }
}

/// Run the setup wizard (async version)
pub async fn run_setup_wizard_async(config: WizardConfig) -> Result<SetupResult> {
    let mut result = SetupResult {
        success: false,
        obs_installation: None,
        plugin_installed: false,
        profile_created: false,
        sources_created: 0,
        permissions_granted: false,
        autostart_enabled: false,
        notes: Vec::new(),
    };

    println!();
    println!("=== crowd-cast Setup Wizard ===");
    println!();

    // Step 1: Check for OBS
    println!("Step 1/8: Checking for OBS Studio...");
    let obs = match detect_obs() {
        Some(obs) => {
            println!("  [✓] OBS Studio found at {:?}", obs.executable);
            obs
        }
        None => {
            println!("  [✗] OBS Studio not found");
            println!();
            
            if config.non_interactive {
                result.notes.push("OBS Studio not installed".to_string());
                return Ok(result);
            }
            
            println!("  OBS Studio is required for crowd-cast to function.");
            println!("  Would you like to open the OBS download page? (y/n)");
            
            if prompt_yes_no()? {
                open_obs_download_page()?;
                println!("  Please install OBS and run this setup again.");
            }
            
            result.notes.push("OBS Studio installation required".to_string());
            return Ok(result);
        }
    };
    
    result.obs_installation = Some(obs.clone());

    // Step 2: Install plugin
    println!();
    println!("Step 2/8: Installing crowd-cast plugin...");
    
    let plugin_status = check_plugin_installed(&obs);
    let mut plugin_installed_now = false;
    
    if plugin_status.installed && !config.force_plugin_reinstall {
        println!("  [✓] Plugin already installed at {:?}", plugin_status.path);
        result.plugin_installed = true;
    } else {
        match install_plugin_async(&obs).await {
            Ok(path) => {
                println!("  [✓] Plugin installed to {:?}", path);
                result.plugin_installed = true;
                plugin_installed_now = true;
            }
            Err(e) => {
                println!("  [✗] Failed to install plugin: {}", e);
                result.notes.push(format!("Plugin installation failed: {}", e));
                warn!("Plugin installation failed: {}", e);
            }
        }
    }

    // Step 3: Create/configure profile
    println!();
    println!("Step 3/8: Configuring OBS profile...");
    
    if profile_exists(&obs) && !config.force_profile_recreate {
        println!("  [✓] crowd-cast profile already exists");
        result.profile_created = true;
    } else {
        let encoder = detect_best_encoder();
        println!("  Detected best encoder: {}", encoder.display_name());
        
        match create_profile(&obs, encoder) {
            Ok(_) => {
                println!("  [✓] Created crowd-cast profile with {} encoding", encoder.display_name());
                result.profile_created = true;
            }
            Err(e) => {
                println!("  [✗] Failed to create profile: {}", e);
                result.notes.push(format!("Profile creation failed: {}", e));
            }
        }
        
        // Also create scene collection
        match create_scene_collection(&obs) {
            Ok(_) => {
                println!("  [✓] Created crowd-cast scene collection");
            }
            Err(e) => {
                println!("  [!] Failed to create scene collection: {}", e);
                result.notes.push(format!("Scene collection creation failed: {}", e));
            }
        }
    }

    // Step 4: Configure OBS WebSocket
    println!();
    println!("Step 4/8: Configuring OBS WebSocket...");
    
    let mut obs_manager: Option<OBSManager> = None;
    let obs_was_running = is_obs_running();
    let mut needs_obs_restart = false;
    let mut restart_reasons = Vec::new();
    
    let mut agent_config = Config::load().unwrap_or_default();
    match super::obs_websocket::ensure_obs_websocket_config(&obs, &mut agent_config) {
        Ok(config_result) => {
            if config_result.updated {
                println!("  [✓] WebSocket server enabled with authentication");
                if obs_was_running {
                    needs_obs_restart = true;
                    restart_reasons.push("WebSocket settings updated".to_string());
                }
            } else {
                println!("  [✓] WebSocket configuration already set");
            }
        }
        Err(e) => {
            println!("  [✗] Failed to configure OBS WebSocket: {}", e);
            result.notes.push(format!("WebSocket config failed: {}", e));
        }
    }

    // Step 5: Launch OBS
    println!();
    println!("Step 5/8: Launching OBS Studio...");
    
    // Helper function to launch OBS (returns error message if failed)
    fn try_launch_obs() -> Result<OBSManager, String> {
        println!("  Starting OBS minimized...");
        
        let manager_config = OBSManagerConfig {
            use_crowd_cast_profile: true,
            auto_start_recording: false,
            ..Default::default()
        };
        
        let mut manager = OBSManager::new(manager_config)
            .map_err(|e| format!("Failed to initialize OBS manager: {}", e))?;
        
        manager.launch_hidden()
            .map_err(|e| format!("Failed to launch OBS: {}", e))?;
        
        println!("  [✓] OBS launched");
        Ok(manager)
    }
    
    if plugin_installed_now && obs_was_running {
        needs_obs_restart = true;
        restart_reasons.push("crowd-cast plugin installed".to_string());
    }
    
    if obs_was_running {
        println!("  [✓] OBS is already running");
        if needs_obs_restart {
            println!("  [!] OBS restart required: {}", restart_reasons.join(", "));
            result.notes
                .push(format!("OBS restart required: {}", restart_reasons.join(", ")));
            if !config.non_interactive {
                println!("      Attempting to close OBS...");
                if let Some(ref mut manager) = obs_manager {
                    if let Err(e) = manager.stop() {
                        println!("      [!] Failed to stop OBS we launched: {}", e);
                    }
                } else if let Err(e) = request_obs_close() {
                    println!("      [!] Could not close OBS automatically: {}", e);
                    println!("      Please close OBS manually to continue...");
                }

                if let Err(e) = wait_for_obs_close(config.websocket_timeout).await {
                    println!("  [✗] {}", e);
                    result.notes.push(e.to_string());
                }

                match try_launch_obs() {
                    Ok(manager) => obs_manager = Some(manager),
                    Err(e) => {
                        println!("  [✗] {}", e);
                        result.notes.push(e);
                    }
                }
            }
        }
    } else {
        match try_launch_obs() {
            Ok(manager) => obs_manager = Some(manager),
            Err(e) => {
                println!("  [✗] {}", e);
                result.notes.push(e);
            }
        }
    }
    
    // Wait for WebSocket connection
    println!("  Waiting for OBS WebSocket...");
    
    let client = match wait_for_obs_websocket(&agent_config, config.websocket_timeout).await {
        Ok(client) => {
            println!("  [✓] Connected to OBS WebSocket");
            Some(client)
        }
        Err(e) => {
            println!("  [✗] Failed to connect to OBS WebSocket: {}", e);
            println!("      Make sure OBS WebSocket server is enabled (Tools > WebSocket Server Settings)");
            result.notes.push(format!("WebSocket connection failed: {}", e));
            None
        }
    };

    // Step 5: Select applications
    println!();
    println!("Step 6/8: Selecting applications to capture...");
    
    if config.skip_app_selection {
        println!("  [!] Skipping application selection");
    } else if let Some(ref client) = client {
        match get_available_windows(client).await {
            Ok(windows) => {
                println!("  Found {} windows ({} suggested)", 
                         windows.windows.len(), windows.suggested.len());
                
                let selected: Vec<CreateSourceWindow> = if config.non_interactive {
                    // Auto-select suggested apps
                    let selected = select_suggested_apps(&windows);
                    println!("  Auto-selecting {} suggested applications", selected.len());
                    selected
                } else {
                    // Interactive selection
                    display_selection_ui(&windows)?
                };
                
                if selected.is_empty() {
                    println!("  [!] No applications selected");
                    result.notes.push("No capture sources created".to_string());
                } else {
                    println!();
                    println!("  Creating {} capture sources...", selected.len());
                    
                    match create_capture_sources(client, selected).await {
                        Ok(response) => {
                            result.sources_created = response.created_count as usize;
                            
                            if response.success {
                                println!("  [✓] Created {} window capture sources", response.created_count);
                            } else {
                                println!("  [!] Created {} sources, {} failed", 
                                         response.created_count, response.failed_count);
                                for failed in &response.failed {
                                    result.notes.push(format!("Failed to create source '{}': {}", 
                                                              failed.name, failed.error));
                                }
                            }
                        }
                        Err(e) => {
                            println!("  [✗] Failed to create capture sources: {}", e);
                            result.notes.push(format!("Source creation failed: {}", e));
                        }
                    }
                }
            }
            Err(e) => {
                println!("  [✗] Failed to get available windows: {}", e);
                println!("      The crowd-cast plugin may not be loaded yet.");
                println!("      If OBS was already running, restart OBS to load the plugin.");
                if !config.non_interactive {
                    println!("      Restart OBS and retry automatically? (y/n)");
                    if prompt_yes_no()? {
                        match wait_for_obs_restart(config.websocket_timeout).await {
                            Ok(_) => {
                                println!("      Reconnecting to OBS WebSocket...");
                                let agent_config = Config::load().unwrap_or_default();
                                match wait_for_obs_websocket(&agent_config, config.websocket_timeout).await {
                                    Ok(new_client) => {
                                        println!("      [✓] Reconnected to OBS WebSocket");
                                        match get_available_windows_with_retry(
                                            &new_client,
                                            10,
                                            Duration::from_secs(1),
                                        ).await {
                                            Ok(windows) => {
                                                println!("  Found {} windows ({} suggested)", 
                                                         windows.windows.len(), windows.suggested.len());
                                                
                                                let selected: Vec<CreateSourceWindow> = if config.non_interactive {
                                                    let selected = select_suggested_apps(&windows);
                                                    println!("  Auto-selecting {} suggested applications", selected.len());
                                                    selected
                                                } else {
                                                    display_selection_ui(&windows)?
                                                };
                                                
                                                if selected.is_empty() {
                                                    println!("  [!] No applications selected");
                                                    result.notes.push("No capture sources created".to_string());
                                                } else {
                                                    println!();
                                                    println!("  Creating {} capture sources...", selected.len());
                                                    
                                                    match create_capture_sources(&new_client, selected).await {
                                                        Ok(response) => {
                                                            result.sources_created = response.created_count as usize;
                                                            
                                                            if response.success {
                                                                println!("  [✓] Created {} window capture sources", response.created_count);
                                                            } else {
                                                                println!("  [!] Created {} sources, {} failed", 
                                                                         response.created_count, response.failed_count);
                                                                for failed in &response.failed {
                                                                    result.notes.push(format!("Failed to create source '{}': {}", 
                                                                                              failed.name, failed.error));
                                                                }
                                                            }
                                                        }
                                                        Err(e) => {
                                                            println!("  [✗] Failed to create capture sources: {}", e);
                                                            result.notes.push(format!("Source creation failed: {}", e));
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                println!("  [✗] Retry failed: {}", e);
                                                println!("      You can add window capture sources manually in OBS.");
                                                result.notes.push(format!("Window enumeration retry failed: {}", e));
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        println!("  [✗] Failed to reconnect to OBS WebSocket: {}", e);
                                        println!("      You can add window capture sources manually in OBS.");
                                        result.notes.push(format!("WebSocket reconnect failed: {}", e));
                                    }
                                }
                            }
                            Err(e) => {
                                println!("  [✗] OBS restart not detected: {}", e);
                                println!("      You can add window capture sources manually in OBS.");
                                result.notes.push(format!("OBS restart not detected: {}", e));
                            }
                        }
                    } else {
                        println!("      You can add window capture sources manually in OBS.");
                        result.notes.push(format!("Window enumeration failed: {}", e));
                    }
                } else {
                    println!("      You can add window capture sources manually in OBS.");
                    result.notes.push(format!("Window enumeration failed: {}", e));
                }
            }
        }
    } else {
        println!("  [!] Skipping - no WebSocket connection");
        result.notes.push("Application selection skipped (no WebSocket)".to_string());
    }

    // Step 6: Request permissions
    println!();
    println!("Step 7/8: Checking permissions...");
    
    if config.skip_permissions {
        println!("  [!] Skipping permission checks");
        result.permissions_granted = true;
    } else {
        let perm_status = check_permissions();
        
        // Check accessibility
        match perm_status.accessibility {
            PermissionState::Granted => {
                println!("  [✓] Accessibility permission: Granted");
            }
            PermissionState::Denied => {
                println!("  [ ] Accessibility permission: Not granted");
                println!("      Requesting permission...");
            }
            PermissionState::NotApplicable => {
                println!("  [✓] Accessibility permission: Not required on this platform");
            }
            PermissionState::Unknown => {
                println!("  [?] Accessibility permission: Unknown status");
            }
        }
        
        // Check screen recording
        match perm_status.screen_recording {
            PermissionState::Granted => {
                println!("  [✓] Screen Recording permission: Granted");
            }
            PermissionState::Denied => {
                println!("  [ ] Screen Recording permission: Not granted");
                println!("      Requesting permission...");
            }
            PermissionState::NotApplicable => {
                println!("  [✓] Screen Recording permission: Not required on this platform");
            }
            PermissionState::Unknown => {
                println!("  [?] Screen Recording permission: Unknown status");
            }
        }
        
        // Check input group (Linux)
        match perm_status.input_group {
            PermissionState::Granted => {
                println!("  [✓] Input group membership: Granted");
            }
            PermissionState::Denied => {
                println!("  [✗] Input group membership: Not granted");
                println!("      Run: sudo usermod -aG input $USER");
                println!("      Then log out and log back in");
                result.notes.push("User needs to be added to 'input' group for Wayland".to_string());
            }
            PermissionState::NotApplicable => {
                // Don't print anything for not applicable
            }
            PermissionState::Unknown => {
                println!("  [?] Input group membership: Unknown status");
            }
        }
        
        // Request permissions if needed
        if perm_status.accessibility == PermissionState::Denied 
            || perm_status.screen_recording == PermissionState::Denied 
        {
            // First, trigger the permission dialogs
            match request_permissions() {
                Ok(_) => {
                    // Dialogs have been triggered, now wait for user to grant them
                    if !config.non_interactive {
                        println!();
                        println!("  Please grant the permissions in System Settings, then press Enter...");
                        wait_for_enter()?;
                    }
                    
                    // Re-check permissions after user has had time to grant them
                    let final_status = check_permissions();
                    result.permissions_granted = final_status.accessibility.is_granted()
                        && final_status.screen_recording.is_granted()
                        && final_status.input_group.is_granted();
                    
                    if result.permissions_granted {
                        println!("  [✓] All permissions granted");
                    } else {
                        println!("  [!] Some permissions not granted - you may need to grant them manually");
                        if !final_status.accessibility.is_granted() {
                            result.notes.push("Accessibility permission not granted".to_string());
                        }
                        if !final_status.screen_recording.is_granted() {
                            result.notes.push("Screen Recording permission not granted".to_string());
                        }
                    }
                }
                Err(e) => {
                    println!("  [✗] Error requesting permissions: {}", e);
                    result.notes.push(format!("Permission request failed: {}", e));
                }
            }
        } else {
            result.permissions_granted = perm_status.accessibility.is_granted()
                && perm_status.screen_recording.is_granted()
                && perm_status.input_group.is_granted();
        }
    }

    // Step 7: Setup autostart
    println!();
    println!("Step 8/8: Setting up autostart...");
    
    if config.skip_autostart {
        println!("  [!] Skipping autostart setup");
    } else if is_autostart_enabled() {
        println!("  [✓] Autostart already enabled");
        result.autostart_enabled = true;
    } else {
        let should_enable = if config.non_interactive {
            true
        } else {
            println!("  Would you like crowd-cast to start automatically on login? (y/n)");
            prompt_yes_no()?
        };
        
        if should_enable {
            let autostart_config = AutostartConfig::default();
            match enable_autostart(&autostart_config) {
                Ok(_) => {
                    println!("  [✓] Autostart enabled");
                    result.autostart_enabled = true;
                }
                Err(e) => {
                    println!("  [✗] Failed to enable autostart: {}", e);
                    result.notes.push(format!("Autostart setup failed: {}", e));
                }
            }
        } else {
            println!("  [!] Autostart not enabled (can be enabled later in settings)");
        }
    }

    // Summary
    println!();
    println!("=== Setup Complete ===");
    println!();
    
    result.success = result.obs_installation.is_some()
        && result.plugin_installed
        && result.permissions_granted;
    
    if result.success {
        println!("crowd-cast is ready to use!");
        println!();
        println!("Configuration:");
        println!("  • {} window capture sources created", result.sources_created);
        if result.autostart_enabled {
            println!("  • crowd-cast will start automatically on login");
        }
        println!();
        println!("The agent will:");
        println!("  • Keep OBS running minimized in the background");
        println!("  • Capture keyboard and mouse input when OBS is recording");
        println!("  • Upload paired data to your configured endpoint");
    } else {
        println!("Setup completed with some issues:");
        for note in &result.notes {
            println!("  • {}", note);
        }
        println!();
        println!("Please resolve the above issues and run setup again.");
    }
    
    println!();
    
    // Keep OBS running if we started it
    if let Some(manager) = obs_manager {
        info!("OBS will continue running in the background");
        // Don't drop the manager, let it keep OBS running
        std::mem::forget(manager);
    }
    
    Ok(result)
}

/// Wait for OBS WebSocket to become available
async fn wait_for_obs_websocket(config: &Config, timeout: Duration) -> Result<Client> {
    let start = Instant::now();
    
    loop {
        match Client::connect(
            &config.obs.host,
            config.obs.port,
            config.obs.password.as_deref(),
        ).await {
            Ok(client) => return Ok(client),
            Err(e) => {
                if start.elapsed() >= timeout {
                    println!(); // End the dots line
                    return Err(anyhow::anyhow!("Timeout waiting for OBS WebSocket: {}", e));
                }
                
                // Print progress dot
                print!(".");
                io::stdout().flush().ok();
                
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

async fn wait_for_obs_restart(timeout: Duration) -> Result<()> {
    if is_obs_running() {
        let start = Instant::now();
        println!("      Waiting for OBS to close...");
        loop {
            if !is_obs_running() {
                println!();
                break;
            }
            if start.elapsed() >= timeout {
                println!();
                return Err(anyhow::anyhow!("Timeout waiting for OBS to close"));
            }
            print!(".");
            io::stdout().flush().ok();
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    
    let start = Instant::now();
    println!("      Waiting for OBS to reopen...");
    loop {
        if is_obs_running() {
            println!();
            return Ok(());
        }
        if start.elapsed() >= timeout {
            println!();
            return Err(anyhow::anyhow!("Timeout waiting for OBS to reopen"));
        }
        print!(".");
        io::stdout().flush().ok();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_for_obs_close(timeout: Duration) -> Result<()> {
    if !is_obs_running() {
        return Ok(());
    }

    let start = Instant::now();
    println!("      Waiting for OBS to close...");
    loop {
        if !is_obs_running() {
            println!();
            return Ok(());
        }
        if start.elapsed() >= timeout {
            println!();
            return Err(anyhow::anyhow!("Timeout waiting for OBS to close"));
        }
        print!(".");
        io::stdout().flush().ok();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn request_obs_close() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("osascript")
            .args(["-e", "tell application \"OBS\" to quit"])
            .status()
            .map_err(|e| format!("Failed to run osascript: {}", e))?;
        if status.success() {
            return Ok(());
        }
        return Err("osascript returned a non-zero status".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        let status = Command::new("taskkill")
            .args(["/IM", "obs64.exe", "/T"])
            .status()
            .map_err(|e| format!("Failed to run taskkill: {}", e))?;
        if status.success() {
            return Ok(());
        }
        return Err("taskkill returned a non-zero status".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        let status = Command::new("pkill")
            .args(["-x", "obs"])
            .status()
            .map_err(|e| format!("Failed to run pkill: {}", e))?;
        if status.success() {
            return Ok(());
        }
        return Err("pkill returned a non-zero status".to_string());
    }
}

async fn get_available_windows_with_retry(
    client: &Client,
    attempts: usize,
    delay: Duration,
) -> Result<AvailableWindowsResponse> {
    let mut last_error = None;
    for _ in 0..attempts {
        match get_available_windows(client).await {
            Ok(windows) => return Ok(windows),
            Err(e) => {
                last_error = Some(e);
                tokio::time::sleep(delay).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Retry failed")))
}

/// Run the setup wizard (sync wrapper)
pub fn run_setup_wizard(config: WizardConfig) -> Result<SetupResult> {
    // Create a runtime for the async wizard
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_setup_wizard_async(config))
}

/// Prompt user for yes/no input
fn prompt_yes_no() -> Result<bool> {
    print!("> ");
    io::stdout().flush()?;
    
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    
    let input = input.trim().to_lowercase();
    Ok(input == "y" || input == "yes")
}

/// Wait for user to press Enter
fn wait_for_enter() -> Result<()> {
    print!("> ");
    io::stdout().flush()?;
    
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(())
}

/// Run setup in non-interactive mode
pub fn run_setup_non_interactive() -> Result<SetupResult> {
    run_setup_wizard(WizardConfig {
        non_interactive: true,
        ..Default::default()
    })
}

/// Check if first-run setup is needed
pub fn needs_setup() -> bool {
    // Check if OBS is installed
    let obs = match detect_obs() {
        Some(obs) => obs,
        None => return true,
    };
    
    // Check if plugin is installed
    if !check_plugin_installed(&obs).installed {
        return true;
    }
    
    // Check if profile exists
    if !profile_exists(&obs) {
        return true;
    }
    
    // Check permissions
    let perms = check_permissions();
    if !perms.accessibility.is_granted() 
        || !perms.screen_recording.is_granted()
        || !perms.input_group.is_granted() 
    {
        return true;
    }
    
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_needs_setup() {
        let needs = needs_setup();
        println!("Needs setup: {}", needs);
    }
    
    #[test]
    fn test_wizard_config_default() {
        let config = WizardConfig::default();
        assert!(!config.non_interactive);
        assert!(!config.skip_permissions);
        assert!(!config.skip_app_selection);
    }
}
