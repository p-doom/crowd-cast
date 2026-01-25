//! OBS Plugin installation
//!
//! Handles plugin installation from:
//! 1. Bundled binary (if available)
//! 2. GitHub Releases download (fallback)
//!
//! On macOS, plugins are installed as .plugin bundles.
//! On Linux/Windows, plugins are installed as .so/.dll files.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use super::obs_detector::OBSInstallation;

/// Name of our OBS plugin
const PLUGIN_NAME: &str = "obs-crowdcast";

/// GitHub repository for plugin releases
const GITHUB_REPO: &str = "your-org/crowd-cast"; // TODO: Update with actual repo

/// Plugin file extension per platform
#[cfg(target_os = "windows")]
const PLUGIN_EXT: &str = "dll";

#[cfg(target_os = "macos")]
const PLUGIN_EXT: &str = "plugin"; // macOS uses .plugin bundles

#[cfg(target_os = "linux")]
const PLUGIN_EXT: &str = "so";

/// Platform-specific artifact name for download
#[cfg(target_os = "windows")]
const PLUGIN_ARTIFACT: &str = "obs-crowdcast-windows-x64.dll";

#[cfg(target_os = "macos")]
const PLUGIN_ARTIFACT: &str = "obs-crowdcast-macos-universal.zip"; // Zip containing .plugin bundle

#[cfg(target_os = "linux")]
const PLUGIN_ARTIFACT: &str = "obs-crowdcast-linux-x64.so";

/// Result of plugin installation check
#[derive(Debug)]
pub struct PluginStatus {
    /// Whether the plugin is installed
    pub installed: bool,
    /// Path where the plugin is/should be installed
    pub path: PathBuf,
    /// Installed version (if detectable)
    pub version: Option<String>,
}

/// Check if the CrowdCast plugin is installed
pub fn check_plugin_installed(obs: &OBSInstallation) -> PluginStatus {
    let plugin_path = get_plugin_install_path(obs);
    
    #[cfg(target_os = "macos")]
    let installed = {
        // On macOS, check that both the bundle and the binary inside exist
        let binary_path = plugin_path.join("Contents/MacOS").join(PLUGIN_NAME);
        plugin_path.exists() && binary_path.exists()
    };
    
    #[cfg(not(target_os = "macos"))]
    let installed = plugin_path.exists();
    
    debug!("Checking plugin at {:?}: installed={}", plugin_path, installed);
    
    PluginStatus {
        installed,
        path: plugin_path,
        version: None, // Could read from plugin metadata
    }
}

/// Get the path where the plugin should be installed
fn get_plugin_install_path(obs: &OBSInstallation) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        obs.plugins_dir.join("64bit").join(format!("{}.{}", PLUGIN_NAME, PLUGIN_EXT))
    }
    
    #[cfg(target_os = "macos")]
    {
        // macOS uses .plugin bundles directly in the plugins directory
        obs.plugins_dir.join(format!("{}.{}", PLUGIN_NAME, PLUGIN_EXT))
    }
    
    #[cfg(target_os = "linux")]
    {
        obs.plugins_dir
            .join(PLUGIN_NAME)
            .join("bin")
            .join("64bit")
            .join(format!("{}.{}", PLUGIN_NAME, PLUGIN_EXT))
    }
}

/// Get the path to a bundled plugin (binary or bundle directory)
fn get_bundled_plugin_path() -> Option<PathBuf> {
    let exe_path = std::env::current_exe().ok()?;
    let exe_dir = exe_path.parent()?;
    
    #[cfg(target_os = "macos")]
    {
        // On macOS, look for .plugin bundle directories
        let bundle_name = format!("{}.plugin", PLUGIN_NAME);
        let possible_paths = [
            // Resources/plugins directory (macOS app bundle)
            exe_dir.join("../Resources/plugins").join(&bundle_name),
            // plugins subdirectory
            exe_dir.join("plugins").join(&bundle_name),
            // Same directory as executable
            exe_dir.join(&bundle_name),
            // Development: build output (look for the .plugin bundle)
            exe_dir.join("../../obs-crowdcast-plugin/build/artifact").join(&bundle_name),
        ];
        
        for path in possible_paths {
            // Check that it's a valid bundle with the binary inside
            let binary_path = path.join("Contents/MacOS").join(PLUGIN_NAME);
            if path.exists() && path.is_dir() && binary_path.exists() {
                debug!("Found bundled plugin bundle at {:?}", path);
                return Some(path);
            }
        }
        
        None
    }
    
    #[cfg(not(target_os = "macos"))]
    {
        // On other platforms, look for single binary files
        let possible_paths = [
            // Same directory as executable
            exe_dir.join(PLUGIN_ARTIFACT),
            exe_dir.join(format!("{}.{}", PLUGIN_NAME, PLUGIN_EXT)),
            // Resources/plugins directory (macOS app bundle - alternative location)
            exe_dir.join("../Resources/plugins").join(PLUGIN_ARTIFACT),
            exe_dir.join("../Resources/plugins").join(format!("{}.{}", PLUGIN_NAME, PLUGIN_EXT)),
            // Resources directory
            exe_dir.join("../Resources").join(PLUGIN_ARTIFACT),
            // plugins subdirectory
            exe_dir.join("plugins").join(PLUGIN_ARTIFACT),
            // Development: build output
            exe_dir.join("../../obs-crowdcast-plugin/build").join(format!("{}.{}", PLUGIN_NAME, PLUGIN_EXT)),
        ];
        
        for path in possible_paths {
            if path.exists() && path.is_file() {
                debug!("Found bundled plugin at {:?}", path);
                return Some(path);
            }
        }
        
        None
    }
}

/// Install the CrowdCast plugin to OBS
pub fn install_plugin(obs: &OBSInstallation) -> Result<PathBuf> {
    // First try bundled plugin
    if let Some(bundled_path) = get_bundled_plugin_path() {
        info!("Installing plugin from bundled binary");
        return install_from_path(&bundled_path, obs);
    }
    
    // Fall back to downloading from GitHub
    warn!("Bundled plugin not found, downloading from GitHub Releases...");
    install_from_github(obs)
}

/// Install the CrowdCast plugin (async version with download support)
pub async fn install_plugin_async(obs: &OBSInstallation) -> Result<PathBuf> {
    // First try bundled plugin
    if let Some(bundled_path) = get_bundled_plugin_path() {
        info!("Installing plugin from bundled binary");
        return install_from_path(&bundled_path, obs);
    }
    
    // Fall back to downloading from GitHub
    warn!("Bundled plugin not found, downloading from GitHub Releases...");
    download_and_install_plugin(obs).await
}

/// Install plugin from a local path
fn install_from_path(source_path: &Path, obs: &OBSInstallation) -> Result<PathBuf> {
    let install_path = get_plugin_install_path(obs);
    
    #[cfg(target_os = "macos")]
    {
        // On macOS, source_path is a .plugin bundle directory
        // Copy the entire bundle
        install_macos_bundle(source_path, &install_path)?;
    }
    
    #[cfg(not(target_os = "macos"))]
    {
        // On other platforms, source_path is a single binary file
        // Create parent directories if they don't exist
        if let Some(parent) = install_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create plugin directory: {:?}", parent))?;
        }
        
        // Copy the plugin binary
        fs::copy(source_path, &install_path)
            .with_context(|| format!("Failed to copy plugin to {:?}", install_path))?;
        
        // Also copy locale files if they exist
        install_plugin_data(obs)?;
    }
    
    info!("Installed CrowdCast plugin to {:?}", install_path);
    
    Ok(install_path)
}

/// Install a macOS .plugin bundle
#[cfg(target_os = "macos")]
fn install_macos_bundle(source_bundle: &Path, install_path: &Path) -> Result<()> {
    // Create plugins directory if it doesn't exist
    if let Some(parent) = install_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create plugins directory: {:?}", parent))?;
    }
    
    // Remove existing bundle if present
    if install_path.exists() {
        fs::remove_dir_all(install_path)
            .with_context(|| format!("Failed to remove existing bundle at {:?}", install_path))?;
    }
    
    // Copy entire bundle directory
    copy_dir_recursive(source_bundle, install_path)?;
    
    // Ensure the binary is executable
    let binary_path = install_path.join("Contents/MacOS").join(PLUGIN_NAME);
    if binary_path.exists() {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&binary_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&binary_path, perms)?;
    }
    
    debug!("Installed macOS plugin bundle to {:?}", install_path);
    Ok(())
}

/// Install plugin by downloading from GitHub (sync wrapper)
fn install_from_github(obs: &OBSInstallation) -> Result<PathBuf> {
    // Create a runtime for the async download
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(download_and_install_plugin(obs))
}

/// Download and install plugin from GitHub Releases
async fn download_and_install_plugin(obs: &OBSInstallation) -> Result<PathBuf> {
    let install_path = get_plugin_install_path(obs);
    
    // Get the latest release download URL
    let download_url = get_latest_release_url().await?;
    
    info!("Downloading plugin from: {}", download_url);
    
    // Download the plugin
    let client = reqwest::Client::new();
    let response = client
        .get(&download_url)
        .header("User-Agent", "crowdcast-agent")
        .send()
        .await
        .context("Failed to download plugin")?;
    
    if !response.status().is_success() {
        anyhow::bail!("Download failed with status: {}", response.status());
    }
    
    let bytes = response.bytes().await.context("Failed to read response body")?;
    
    #[cfg(target_os = "macos")]
    {
        // On macOS, the artifact is a zip containing a .plugin bundle
        install_macos_plugin_from_zip(&bytes, obs)?;
    }
    
    #[cfg(not(target_os = "macos"))]
    {
        // On other platforms, write directly to install path
        if let Some(parent) = install_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create plugin directory: {:?}", parent))?;
        }
        
        fs::write(&install_path, &bytes)
            .with_context(|| format!("Failed to write plugin to {:?}", install_path))?;
        
        // Set executable permission on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&install_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&install_path, perms)?;
        }
        
        // Download and install locale data
        download_and_install_data(obs).await?;
    }
    
    info!("Downloaded and installed plugin to {:?}", install_path);
    
    Ok(install_path)
}

/// Install macOS plugin from a zip archive containing a .plugin bundle
#[cfg(target_os = "macos")]
fn install_macos_plugin_from_zip(zip_bytes: &[u8], obs: &OBSInstallation) -> Result<()> {
    use std::io::Cursor;
    
    let cursor = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .context("Failed to read zip archive")?;
    
    let install_path = get_plugin_install_path(obs);
    
    // Create plugins directory
    if let Some(parent) = install_path.parent() {
        fs::create_dir_all(parent)?;
    }
    
    // Remove existing bundle
    if install_path.exists() {
        fs::remove_dir_all(&install_path)?;
    }
    
    // Extract the .plugin bundle
    let bundle_name = format!("{}.plugin", PLUGIN_NAME);
    
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let file_path = file.mangled_name();
        
        // Only extract files that are inside our bundle
        if let Ok(relative) = file_path.strip_prefix(&bundle_name) {
            let dest_path = install_path.join(relative);
            
            if file.is_dir() {
                fs::create_dir_all(&dest_path)?;
            } else {
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut outfile = fs::File::create(&dest_path)?;
                std::io::copy(&mut file, &mut outfile)?;
                
                // Set executable permission for the binary
                #[cfg(unix)]
                if dest_path.ends_with(format!("Contents/MacOS/{}", PLUGIN_NAME)) {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = fs::metadata(&dest_path)?.permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&dest_path, perms)?;
                }
            }
        }
    }
    
    debug!("Extracted macOS plugin bundle to {:?}", install_path);
    Ok(())
}

/// Get the download URL for the latest release
async fn get_latest_release_url() -> Result<String> {
    let api_url = format!("https://api.github.com/repos/{}/releases/latest", GITHUB_REPO);
    
    let client = reqwest::Client::new();
    let response = client
        .get(&api_url)
        .header("User-Agent", "crowdcast-agent")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("Failed to fetch release info")?;
    
    if !response.status().is_success() {
        // If no releases yet, provide instructions
        anyhow::bail!(
            "Could not find plugin releases. Please either:\n\
             1. Build the plugin locally (see README)\n\
             2. Wait for a release to be published at https://github.com/{}/releases",
            GITHUB_REPO
        );
    }
    
    let release: serde_json::Value = response.json().await?;
    
    // Find the asset matching our platform
    let assets = release["assets"]
        .as_array()
        .context("No assets in release")?;
    
    for asset in assets {
        let name = asset["name"].as_str().unwrap_or("");
        if name == PLUGIN_ARTIFACT {
            let url = asset["browser_download_url"]
                .as_str()
                .context("No download URL for asset")?;
            return Ok(url.to_string());
        }
    }
    
    anyhow::bail!(
        "Could not find {} in release assets. Available assets: {:?}",
        PLUGIN_ARTIFACT,
        assets.iter().filter_map(|a| a["name"].as_str()).collect::<Vec<_>>()
    )
}

/// Download and install locale data files
#[cfg(not(target_os = "macos"))]
async fn download_and_install_data(obs: &OBSInstallation) -> Result<()> {
    // For now, just try to install from local paths
    // In the future, we could download data files from the release too
    let _ = install_plugin_data(obs);
    Ok(())
}

/// Install plugin data files (locale, etc.) - only needed for non-macOS
#[cfg(not(target_os = "macos"))]
fn install_plugin_data(obs: &OBSInstallation) -> Result<()> {
    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path.parent().context("No parent directory")?;
    
    // Look for locale files
    let locale_sources = [
        exe_dir.join("data/locale"),
        exe_dir.join("../Resources/data/locale"),
        exe_dir.join("../../obs-crowdcast-plugin/data/locale"),
    ];
    
    let locale_dest = get_plugin_data_path(obs);
    
    for source in &locale_sources {
        if source.exists() && source.is_dir() {
            fs::create_dir_all(&locale_dest)?;
            copy_dir_contents(source, &locale_dest)?;
            debug!("Copied locale files to {:?}", locale_dest);
            break;
        }
    }
    
    Ok(())
}

/// Get the path for plugin data files
#[cfg(not(target_os = "macos"))]
fn get_plugin_data_path(obs: &OBSInstallation) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        obs.data_dir.join("obs-plugins").join(PLUGIN_NAME).join("locale")
    }
    
    #[cfg(target_os = "macos")]
    {
        // On macOS, data is inside the .plugin bundle
        obs.plugins_dir
            .join(format!("{}.plugin", PLUGIN_NAME))
            .join("Contents/Resources/locale")
    }
    
    #[cfg(target_os = "linux")]
    {
        obs.plugins_dir.join(PLUGIN_NAME).join("data").join("locale")
    }
}

/// Copy directory contents recursively (for non-bundle files)
#[cfg(not(target_os = "macos"))]
fn copy_dir_contents(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }
    
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        
        if src_path.is_dir() {
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    
    Ok(())
}

/// Copy directory recursively (for macOS bundles)
#[cfg(target_os = "macos")]
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    
    Ok(())
}

/// Uninstall the CrowdCast plugin from OBS
pub fn uninstall_plugin(obs: &OBSInstallation) -> Result<()> {
    let install_path = get_plugin_install_path(obs);
    
    if install_path.exists() {
        #[cfg(target_os = "macos")]
        {
            // On macOS, remove the entire bundle directory
            fs::remove_dir_all(&install_path)
                .with_context(|| format!("Failed to remove plugin bundle at {:?}", install_path))?;
        }
        
        #[cfg(not(target_os = "macos"))]
        {
            // On other platforms, remove the plugin file
            fs::remove_file(&install_path)
                .with_context(|| format!("Failed to remove plugin at {:?}", install_path))?;
            
            // Try to remove parent directories if empty
            if let Some(parent) = install_path.parent() {
                let _ = fs::remove_dir(parent); // Ignore error if not empty
            }
        }
        
        info!("Uninstalled CrowdCast plugin from {:?}", install_path);
    } else {
        debug!("Plugin not installed, nothing to uninstall");
    }
    
    // Also remove plugin data (only relevant for non-macOS, as macOS data is in the bundle)
    #[cfg(not(target_os = "macos"))]
    {
        let data_path = get_plugin_data_path(obs);
        if let Some(plugin_dir) = data_path.parent().and_then(|p| p.parent()) {
            if plugin_dir.exists() {
                let _ = fs::remove_dir_all(plugin_dir);
                debug!("Removed plugin data directory");
            }
        }
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installer::detect_obs;
    
    #[test]
    fn test_check_plugin_installed() {
        if let Some(obs) = detect_obs() {
            let status = check_plugin_installed(&obs);
            println!("Plugin status: {:?}", status);
        }
    }
    
    #[test]
    fn test_get_bundled_plugin_path() {
        let path = get_bundled_plugin_path();
        println!("Bundled plugin path: {:?}", path);
    }
}
