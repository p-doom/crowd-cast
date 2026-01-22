//! OBS Studio detection and installation helper

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;
use tracing::{debug, info, warn};

/// Information about an OBS installation
#[derive(Debug, Clone)]
pub struct OBSInstallation {
    /// Path to the OBS executable
    pub executable: PathBuf,
    /// Path to the OBS data directory (for plugins, profiles, etc.)
    pub data_dir: PathBuf,
    /// Path to the plugins directory
    pub plugins_dir: PathBuf,
    /// Detected OBS version (if available)
    pub version: Option<String>,
}

/// Detect OBS Studio installation on the current system
pub fn detect_obs() -> Option<OBSInstallation> {
    #[cfg(target_os = "windows")]
    {
        detect_obs_windows()
    }
    
    #[cfg(target_os = "macos")]
    {
        detect_obs_macos()
    }
    
    #[cfg(target_os = "linux")]
    {
        detect_obs_linux()
    }
}

#[cfg(target_os = "windows")]
fn detect_obs_windows() -> Option<OBSInstallation> {
    use std::env;
    
    let program_files = env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
    let program_files_x86 = env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".to_string());
    let appdata = env::var("APPDATA").ok()?;
    
    let possible_paths = [
        format!(r"{}\obs-studio\bin\64bit\obs64.exe", program_files),
        format!(r"{}\obs-studio\bin\64bit\obs64.exe", program_files_x86),
    ];
    
    for path in &possible_paths {
        let exe_path = PathBuf::from(path);
        if exe_path.exists() {
            let data_dir = PathBuf::from(&appdata).join("obs-studio");
            let plugins_dir = data_dir.join("obs-plugins").join("64bit");
            
            info!("Found OBS at: {:?}", exe_path);
            return Some(OBSInstallation {
                executable: exe_path,
                data_dir,
                plugins_dir,
                version: None, // Could parse from file version
            });
        }
    }
    
    debug!("OBS not found in standard Windows locations");
    None
}

#[cfg(target_os = "macos")]
fn detect_obs_macos() -> Option<OBSInstallation> {
    let app_path = PathBuf::from("/Applications/OBS.app");
    
    if app_path.exists() {
        let exe_path = app_path.join("Contents/MacOS/OBS");
        let home = std::env::var("HOME").ok()?;
        let data_dir = PathBuf::from(&home).join("Library/Application Support/obs-studio");
        let plugins_dir = data_dir.join("plugins");
        
        info!("Found OBS at: {:?}", app_path);
        return Some(OBSInstallation {
            executable: exe_path,
            data_dir,
            plugins_dir,
            version: None,
        });
    }
    
    debug!("OBS not found at /Applications/OBS.app");
    None
}

#[cfg(target_os = "linux")]
fn detect_obs_linux() -> Option<OBSInstallation> {
    let possible_paths = [
        "/usr/bin/obs",
        "/usr/local/bin/obs",
        "/snap/bin/obs",
        "/var/lib/flatpak/exports/bin/com.obsproject.Studio",
    ];
    
    for path in &possible_paths {
        let exe_path = PathBuf::from(path);
        if exe_path.exists() {
            let home = std::env::var("HOME").ok()?;
            let data_dir = PathBuf::from(&home).join(".config/obs-studio");
            let plugins_dir = data_dir.join("plugins");
            
            info!("Found OBS at: {:?}", exe_path);
            return Some(OBSInstallation {
                executable: exe_path,
                data_dir,
                plugins_dir,
                version: None,
            });
        }
    }
    
    // Try using `which` command
    if let Ok(output) = Command::new("which").arg("obs").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                let exe_path = PathBuf::from(&path);
                let home = std::env::var("HOME").ok()?;
                let data_dir = PathBuf::from(&home).join(".config/obs-studio");
                let plugins_dir = data_dir.join("plugins");
                
                info!("Found OBS via which: {:?}", exe_path);
                return Some(OBSInstallation {
                    executable: exe_path,
                    data_dir,
                    plugins_dir,
                    version: None,
                });
            }
        }
    }
    
    debug!("OBS not found on Linux");
    None
}

/// Get the download URL for OBS based on the current platform
pub fn get_obs_download_url() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "https://obsproject.com/download"
    }
    
    #[cfg(target_os = "macos")]
    {
        "https://obsproject.com/download"
    }
    
    #[cfg(target_os = "linux")]
    {
        "https://obsproject.com/download"
    }
}

/// Open the OBS download page in the default browser
pub fn open_obs_download_page() -> Result<()> {
    let url = get_obs_download_url();
    
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/c", "start", url])
            .spawn()
            .context("Failed to open browser")?;
    }
    
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .spawn()
            .context("Failed to open browser")?;
    }
    
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(url)
            .spawn()
            .context("Failed to open browser")?;
    }
    
    info!("Opened OBS download page: {}", url);
    Ok(())
}

/// Check if OBS is currently running
pub fn is_obs_running() -> bool {
    #[cfg(target_os = "windows")]
    {
        Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq obs64.exe"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("obs64.exe"))
            .unwrap_or(false)
    }
    
    #[cfg(target_os = "macos")]
    {
        Command::new("pgrep")
            .args(["-x", "OBS"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    
    #[cfg(target_os = "linux")]
    {
        Command::new("pgrep")
            .args(["-x", "obs"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// Kill any running OBS processes
pub fn kill_obs() -> Result<()> {
    if !is_obs_running() {
        return Ok(());
    }
    
    warn!("Killing running OBS process");
    
    #[cfg(target_os = "windows")]
    {
        Command::new("taskkill")
            .args(["/IM", "obs64.exe", "/F"])
            .output()
            .context("Failed to kill OBS")?;
    }
    
    #[cfg(target_os = "macos")]
    {
        Command::new("pkill")
            .args(["-x", "OBS"])
            .output()
            .context("Failed to kill OBS")?;
    }
    
    #[cfg(target_os = "linux")]
    {
        Command::new("pkill")
            .args(["-x", "obs"])
            .output()
            .context("Failed to kill OBS")?;
    }
    
    // Wait a moment for the process to terminate
    std::thread::sleep(std::time::Duration::from_millis(500));
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_detect_obs() {
        // This test will pass or fail depending on whether OBS is installed
        let result = detect_obs();
        println!("OBS detection result: {:?}", result);
    }
    
    #[test]
    fn test_is_obs_running() {
        let running = is_obs_running();
        println!("OBS running: {}", running);
    }
}
