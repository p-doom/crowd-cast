//! Setup wizard for crowd-cast
//!
//! Guides users through initial configuration:
//! 1. Permission requests (Screen Recording, Accessibility)
//! 2. Application selection for capture
//! 3. Optional autostart setup

use anyhow::{Context, Result};
use std::io::{self, Write};
use tracing::info;

use crate::capture::{list_capturable_apps, AppInfo};
use crate::config::Config;
use crate::installer::permissions::{
    check_permissions, request_permissions, PermissionState,
};

/// Result of running the setup wizard
#[derive(Debug)]
pub struct WizardResult {
    /// Whether setup completed successfully
    pub success: bool,
    /// Selected applications for capture
    pub selected_apps: Vec<String>,
    /// Whether to capture all apps
    pub capture_all: bool,
    /// Whether autostart was enabled
    pub autostart_enabled: bool,
}

/// Run the setup wizard
pub fn run_wizard(config: &mut Config) -> Result<WizardResult> {
    println!("\n=================================================");
    println!("  crowd-cast Setup Wizard");
    println!("=================================================\n");

    // Step 1: Check and request permissions
    println!("Step 1: Checking permissions...\n");
    
    let perms = check_permissions();
    let mut all_granted = true;
    
    if !perms.accessibility.is_granted() {
        println!("  [!] Accessibility permission required for input capture");
        all_granted = false;
    } else {
        println!("  [OK] Accessibility permission granted");
    }
    
    if !perms.screen_recording.is_granted() {
        println!("  [!] Screen Recording permission required for capture");
        all_granted = false;
    } else {
        println!("  [OK] Screen Recording permission granted");
    }
    
    if !perms.input_group.is_granted() && perms.input_group != PermissionState::NotApplicable {
        println!("  [!] User must be in 'input' group for Wayland capture");
        all_granted = false;
    }
    
    if !all_granted {
        println!("\nRequesting missing permissions...");
        let new_perms = request_permissions()?;
        
        // Check again
        if !new_perms.accessibility.is_granted() {
            println!("\n[Warning] Accessibility permission not granted.");
            println!("Please grant permission in System Settings > Privacy & Security > Accessibility");
            println!("Then restart crowd-cast.\n");
            
            if !prompt_continue("Continue anyway?")? {
                return Ok(WizardResult {
                    success: false,
                    selected_apps: vec![],
                    capture_all: false,
                    autostart_enabled: false,
                });
            }
        }
        
        if !new_perms.screen_recording.is_granted() {
            println!("\n[Warning] Screen Recording permission not granted.");
            println!("Please grant permission in System Settings > Privacy & Security > Screen Recording");
            println!("Then restart crowd-cast.\n");
            
            if !prompt_continue("Continue anyway?")? {
                return Ok(WizardResult {
                    success: false,
                    selected_apps: vec![],
                    capture_all: false,
                    autostart_enabled: false,
                });
            }
        }
    }
    
    println!();

    // Step 2: Application selection
    println!("Step 2: Select applications to capture\n");
    println!("Input will only be captured when one of the selected");
    println!("applications is in the foreground.\n");
    
    let capture_all = prompt_yes_no("Capture input for ALL applications?")?;
    
    let selected_apps = if capture_all {
        println!("\nAll applications will be captured.\n");
        vec![]
    } else {
        println!("\nLoading running applications...\n");
        let apps = list_capturable_apps();
        
        if apps.is_empty() {
            println!("[Warning] No capturable applications found.");
            println!("You can add applications manually in the config file later.\n");
            vec![]
        } else {
            select_applications(&apps)?
        }
    };

    // Step 3: Autostart
    println!("Step 3: Autostart configuration\n");
    
    let autostart_enabled = prompt_yes_no("Start crowd-cast automatically on login?")?;
    
    if autostart_enabled {
        let autostart_config = crate::installer::autostart::AutostartConfig::default();
        match crate::installer::autostart::enable_autostart(&autostart_config) {
            Ok(_) => println!("\n[OK] Autostart enabled.\n"),
            Err(e) => println!("\n[Warning] Failed to enable autostart: {}\n", e),
        }
    }

    // Save configuration
    println!("Saving configuration...\n");
    
    config.capture.capture_all = capture_all;
    config.capture.target_apps = selected_apps.clone();
    config.complete_setup()?;
    
    println!("=================================================");
    println!("  Setup Complete!");
    println!("=================================================\n");
    
    if capture_all {
        println!("Input capture: ALL applications");
    } else if selected_apps.is_empty() {
        println!("Input capture: No applications selected (disabled)");
    } else {
        println!("Input capture: {} application(s)", selected_apps.len());
        for app in &selected_apps {
            println!("  - {}", app);
        }
    }
    
    println!("\nConfiguration saved to: {:?}", config.config_path());
    println!();

    Ok(WizardResult {
        success: true,
        selected_apps,
        capture_all,
        autostart_enabled,
    })
}

/// Interactive application selection
fn select_applications(apps: &[AppInfo]) -> Result<Vec<String>> {
    let mut selected = Vec::new();
    
    println!("Available applications:\n");
    
    for (i, app) in apps.iter().enumerate() {
        println!("  {:3}. {} ({})", i + 1, app.name, app.bundle_id);
    }
    
    println!();
    println!("Enter application numbers to select (comma-separated)");
    println!("Example: 1,3,5 or 'all' for all apps, 'none' to skip");
    print!("\nSelection: ");
    io::stdout().flush()?;
    
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();
    
    if input == "all" {
        // Return all app bundle IDs
        return Ok(apps.iter().map(|a| a.bundle_id.clone()).collect());
    }
    
    if input == "none" || input.is_empty() {
        return Ok(vec![]);
    }
    
    // Parse comma-separated numbers
    for part in input.split(',') {
        let part = part.trim();
        if let Ok(num) = part.parse::<usize>() {
            if num >= 1 && num <= apps.len() {
                let app = &apps[num - 1];
                if !selected.contains(&app.bundle_id) {
                    selected.push(app.bundle_id.clone());
                    println!("  Selected: {} ({})", app.name, app.bundle_id);
                }
            } else {
                println!("  [!] Invalid number: {}", num);
            }
        } else {
            println!("  [!] Invalid input: {}", part);
        }
    }
    
    println!();
    
    // Confirm selection
    if selected.is_empty() {
        println!("No applications selected.");
        if prompt_yes_no("Would you like to select again?")? {
            return select_applications(apps);
        }
    } else {
        println!("Selected {} application(s).", selected.len());
        if !prompt_yes_no("Confirm selection?")? {
            return select_applications(apps);
        }
    }
    
    Ok(selected)
}

/// Prompt for yes/no input
fn prompt_yes_no(prompt: &str) -> Result<bool> {
    print!("{} [y/N]: ", prompt);
    io::stdout().flush()?;
    
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();
    
    Ok(input == "y" || input == "yes")
}

/// Prompt to continue
fn prompt_continue(prompt: &str) -> Result<bool> {
    prompt_yes_no(prompt)
}

/// Check if setup wizard should be run
pub fn needs_setup(config: &Config) -> bool {
    config.needs_setup()
}

/// Run setup wizard asynchronously (for use with tokio)
pub async fn run_wizard_async(config: &mut Config) -> Result<WizardResult> {
    // Run the blocking wizard in a spawn_blocking task
    let mut config_clone = config.clone();
    let result = tokio::task::spawn_blocking(move || {
        run_wizard(&mut config_clone)
    }).await.context("Wizard task panicked")??;
    
    // Update the original config if successful
    if result.success {
        config.capture.capture_all = result.capture_all;
        config.capture.target_apps = result.selected_apps.clone();
        config.capture.setup_completed = true;
        // Don't save here - run_wizard already saved
    }
    
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_needs_setup() {
        let config = Config::default();
        assert!(needs_setup(&config));
    }
}
