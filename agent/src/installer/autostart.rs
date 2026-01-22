//! Cross-platform autostart / login item setup

use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing::info;

/// Autostart configuration
#[derive(Debug, Clone)]
pub struct AutostartConfig {
    /// Application name
    pub app_name: String,
    /// Path to the executable
    pub app_path: PathBuf,
    /// Command line arguments to pass
    pub args: Vec<String>,
    /// Whether to start minimized
    pub start_minimized: bool,
}

impl Default for AutostartConfig {
    fn default() -> Self {
        Self {
            app_name: "CrowdCast".to_string(),
            app_path: std::env::current_exe().unwrap_or_default(),
            args: vec![],
            start_minimized: true,
        }
    }
}

/// Check if autostart is enabled
pub fn is_autostart_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        is_autostart_enabled_windows()
    }
    
    #[cfg(target_os = "macos")]
    {
        is_autostart_enabled_macos()
    }
    
    #[cfg(target_os = "linux")]
    {
        is_autostart_enabled_linux()
    }
}

/// Enable autostart
pub fn enable_autostart(config: &AutostartConfig) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        enable_autostart_windows(config)
    }
    
    #[cfg(target_os = "macos")]
    {
        enable_autostart_macos(config)
    }
    
    #[cfg(target_os = "linux")]
    {
        enable_autostart_linux(config)
    }
}

/// Disable autostart
pub fn disable_autostart() -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        disable_autostart_windows()
    }
    
    #[cfg(target_os = "macos")]
    {
        disable_autostart_macos()
    }
    
    #[cfg(target_os = "linux")]
    {
        disable_autostart_linux()
    }
}

// ============================================================================
// Windows Implementation
// ============================================================================

#[cfg(target_os = "windows")]
fn is_autostart_enabled_windows() -> bool {
    use std::process::Command;
    
    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "CrowdCast",
        ])
        .output();
    
    match output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

#[cfg(target_os = "windows")]
fn enable_autostart_windows(config: &AutostartConfig) -> Result<()> {
    use std::process::Command;
    
    let exe_path = config.app_path.to_string_lossy();
    let args = if config.args.is_empty() {
        String::new()
    } else {
        format!(" {}", config.args.join(" "))
    };
    
    let value = format!("\"{}\"{}",  exe_path, args);
    
    let status = Command::new("reg")
        .args([
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            &config.app_name,
            "/t",
            "REG_SZ",
            "/d",
            &value,
            "/f",
        ])
        .status()
        .context("Failed to run reg command")?;
    
    if status.success() {
        info!("Enabled autostart for {}", config.app_name);
        Ok(())
    } else {
        anyhow::bail!("Failed to add registry entry for autostart")
    }
}

#[cfg(target_os = "windows")]
fn disable_autostart_windows() -> Result<()> {
    use std::process::Command;
    
    let status = Command::new("reg")
        .args([
            "delete",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "CrowdCast",
            "/f",
        ])
        .status()
        .context("Failed to run reg command")?;
    
    if status.success() {
        info!("Disabled autostart");
        Ok(())
    } else {
        // Not an error if the key doesn't exist
        debug!("Registry key may not have existed");
        Ok(())
    }
}

// ============================================================================
// macOS Implementation
// ============================================================================

#[cfg(target_os = "macos")]
fn get_launch_agent_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("Could not get HOME directory")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join("dev.crowdcast.agent.plist"))
}

#[cfg(target_os = "macos")]
fn is_autostart_enabled_macos() -> bool {
    get_launch_agent_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn enable_autostart_macos(config: &AutostartConfig) -> Result<()> {
    use std::fs;
    
    let plist_path = get_launch_agent_path()?;
    
    // Ensure LaunchAgents directory exists
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)?;
    }
    
    let exe_path = config.app_path.to_string_lossy();
    
    // Build program arguments
    let mut program_args = format!(
        "        <string>{}</string>\n",
        exe_path
    );
    for arg in &config.args {
        program_args.push_str(&format!("        <string>{}</string>\n", arg));
    }
    
    let plist_content = format!(
r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.crowdcast.agent</string>
    <key>ProgramArguments</key>
    <array>
{program_args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        program_args = program_args
    );
    
    fs::write(&plist_path, plist_content)
        .with_context(|| format!("Failed to write LaunchAgent plist to {:?}", plist_path))?;
    
    info!("Created LaunchAgent at {:?}", plist_path);
    
    // Load the launch agent
    let _ = std::process::Command::new("launchctl")
        .args(["load", plist_path.to_str().unwrap()])
        .output();
    
    Ok(())
}

#[cfg(target_os = "macos")]
fn disable_autostart_macos() -> Result<()> {
    use std::fs;
    
    let plist_path = get_launch_agent_path()?;
    
    if plist_path.exists() {
        // Unload the launch agent first
        let _ = std::process::Command::new("launchctl")
            .args(["unload", plist_path.to_str().unwrap()])
            .output();
        
        fs::remove_file(&plist_path)
            .with_context(|| format!("Failed to remove LaunchAgent at {:?}", plist_path))?;
        
        info!("Removed LaunchAgent");
    }
    
    Ok(())
}

// ============================================================================
// Linux Implementation
// ============================================================================

#[cfg(target_os = "linux")]
fn get_autostart_path() -> Result<PathBuf> {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{}/.config", home)
        });
    
    Ok(PathBuf::from(config_home)
        .join("autostart")
        .join("crowdcast.desktop"))
}

#[cfg(target_os = "linux")]
fn is_autostart_enabled_linux() -> bool {
    get_autostart_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn enable_autostart_linux(config: &AutostartConfig) -> Result<()> {
    use std::fs;
    
    let desktop_path = get_autostart_path()?;
    
    // Ensure autostart directory exists
    if let Some(parent) = desktop_path.parent() {
        fs::create_dir_all(parent)?;
    }
    
    let exe_path = config.app_path.to_string_lossy();
    let args = if config.args.is_empty() {
        String::new()
    } else {
        format!(" {}", config.args.join(" "))
    };
    
    let desktop_content = format!(
r#"[Desktop Entry]
Type=Application
Name={name}
Exec={exe}{args}
Hidden=false
NoDisplay=false
X-GNOME-Autostart-enabled=true
Comment=CrowdCast data collection agent
"#,
        name = config.app_name,
        exe = exe_path,
        args = args
    );
    
    fs::write(&desktop_path, desktop_content)
        .with_context(|| format!("Failed to write desktop file to {:?}", desktop_path))?;
    
    info!("Created autostart desktop file at {:?}", desktop_path);
    Ok(())
}

#[cfg(target_os = "linux")]
fn disable_autostart_linux() -> Result<()> {
    use std::fs;
    
    let desktop_path = get_autostart_path()?;
    
    if desktop_path.exists() {
        fs::remove_file(&desktop_path)
            .with_context(|| format!("Failed to remove autostart file at {:?}", desktop_path))?;
        
        info!("Removed autostart desktop file");
    }
    
    Ok(())
}

// ============================================================================
// Common Functions
// ============================================================================

/// Setup autostart with default configuration
pub fn setup_autostart_default() -> Result<()> {
    let config = AutostartConfig::default();
    enable_autostart(&config)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_autostart_config_default() {
        let config = AutostartConfig::default();
        assert_eq!(config.app_name, "CrowdCast");
        assert!(config.start_minimized);
    }
    
    #[test]
    fn test_is_autostart_enabled() {
        let enabled = is_autostart_enabled();
        println!("Autostart enabled: {}", enabled);
    }
}
