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
            app_name: "crowd-cast".to_string(),
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
            "crowd-cast",
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

    let value = format!("\"{}\"{}", exe_path, args);

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
            "crowd-cast",
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
const MACOS_AUTOSTART_LABEL: &str = "dev.crowd-cast.agent";

#[cfg(target_os = "macos")]
fn get_launch_agent_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("Could not get HOME directory")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", MACOS_AUTOSTART_LABEL)))
}

#[cfg(target_os = "macos")]
fn is_autostart_enabled_macos() -> bool {
    get_launch_agent_path().map(|p| p.exists()).unwrap_or(false)
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
    let mut program_args = format!("        <string>{}</string>\n", exe_path);
    for arg in &config.args {
        program_args.push_str(&format!("        <string>{}</string>\n", arg));
    }

    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{program_args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        label = MACOS_AUTOSTART_LABEL,
        program_args = program_args
    );

    fs::write(&plist_path, plist_content)
        .with_context(|| format!("Failed to write LaunchAgent plist to {:?}", plist_path))?;

    info!("Configured LaunchAgent at {:?}", plist_path);

    let service = macos_launch_agent_service_target();

    // Ensure launchd state is not disabled for this service.
    if is_current_macos_launch_agent_disabled()? {
        let status = std::process::Command::new("launchctl")
            .args(["enable", &service])
            .status()
            .with_context(|| format!("Failed to run launchctl enable for {}", service))?;

        if status.success() {
            info!("Re-enabled LaunchAgent service {}", service);
        } else {
            anyhow::bail!("launchctl enable failed for {}", service);
        }
    }

    // Note: plist changes (e.g., KeepAlive) take effect on next launchd service
    // load (next login or next service restart). We intentionally do NOT
    // bootout+bootstrap here because that would kill the currently running
    // process and spawn a duplicate.

    Ok(())
}

#[cfg(target_os = "macos")]
fn disable_autostart_macos() -> Result<()> {
    remove_macos_launch_agent(&get_launch_agent_path()?)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_macos_launch_agent(plist_path: &PathBuf) -> Result<()> {
    use std::fs;

    let service = macos_launch_agent_service_target();
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &service])
        .output();
    let _ = std::process::Command::new("launchctl")
        .args(["disable", &service])
        .output();

    if plist_path.exists() {
        fs::remove_file(plist_path)
            .with_context(|| format!("Failed to remove LaunchAgent at {:?}", plist_path))?;
        info!("Removed LaunchAgent at {:?}", plist_path);
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn is_current_macos_launch_agent_healthy(config: &AutostartConfig) -> Result<bool> {
    let plist_path = get_launch_agent_path()?;
    if !plist_path.exists() {
        return Ok(false);
    }

    let expected_exe = config.app_path.to_string_lossy();
    let contents = std::fs::read_to_string(&plist_path)
        .with_context(|| format!("Failed to read LaunchAgent plist at {:?}", plist_path))?;

    // Check label, exe path, and that KeepAlive is unconditional (not the
    // old dict form with Crashed/SuccessfulExit which doesn't work correctly).
    Ok(contents.contains(MACOS_AUTOSTART_LABEL)
        && contents.contains(&format!("<string>{}</string>", expected_exe))
        && !contents.contains("<key>Crashed</key>"))
}

#[cfg(target_os = "macos")]
fn macos_launch_agent_domain_target() -> String {
    let uid = unsafe { libc::getuid() };
    format!("gui/{}", uid)
}

#[cfg(target_os = "macos")]
fn macos_launch_agent_service_target() -> String {
    format!(
        "{}/{}",
        macos_launch_agent_domain_target(),
        MACOS_AUTOSTART_LABEL
    )
}

#[cfg(target_os = "macos")]
fn is_current_macos_launch_agent_disabled() -> Result<bool> {
    let domain = macos_launch_agent_domain_target();
    let output = std::process::Command::new("launchctl")
        .args(["print-disabled", &domain])
        .output()
        .with_context(|| format!("Failed to run launchctl print-disabled for {}", domain))?;

    if !output.status.success() {
        anyhow::bail!("launchctl print-disabled failed for {}", domain);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let disabled_markers = [
        format!("\"{}\" => disabled", MACOS_AUTOSTART_LABEL),
        format!("{} => disabled", MACOS_AUTOSTART_LABEL),
        format!("\"{}\" => true", MACOS_AUTOSTART_LABEL),
        format!("{} => true", MACOS_AUTOSTART_LABEL),
    ];

    Ok(disabled_markers
        .iter()
        .any(|marker| stdout.contains(marker)))
}

// ============================================================================
// Linux Implementation
// ============================================================================

#[cfg(target_os = "linux")]
fn get_autostart_path() -> Result<PathBuf> {
    let config_home = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{}/.config", home)
    });

    Ok(PathBuf::from(config_home)
        .join("autostart")
        .join("crowd-cast.desktop"))
}

#[cfg(target_os = "linux")]
fn is_autostart_enabled_linux() -> bool {
    get_autostart_path().map(|p| p.exists()).unwrap_or(false)
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
Comment=crowd-cast data collection agent
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

/// Returns true if any autostart artifact currently exists.
pub fn has_any_autostart_entry() -> bool {
    is_autostart_enabled()
}

/// What to do when a quit marker file is found on startup.
#[derive(Debug, PartialEq, Eq)]
enum QuitMarkerAction {
    /// This is a launchd auto-restart after quit/update — exit immediately.
    /// Keep the marker so subsequent launchd restart attempts also exit
    /// (launchd throttles with 10s delays between restarts).
    ExitKeepMarker,
    /// This is a Sparkle/user launch, or a stale marker — delete it and continue.
    ContinueDeleteMarker,
}

/// Decide what to do with a quit marker.
///
/// - `started_by_launchd`: true if XPC_SERVICE_NAME env var is set
/// - `marker_age_secs`: age of the marker file in seconds (None if unknown)
///
//// Exit only if ALL: started by launchd AND marker is recent (<120s).
// 120s gives 12 launchd throttle cycles (10s each) for disable to take effect.
const QUIT_MARKER_MAX_AGE_SECS: u64 = 120;

fn check_quit_marker(started_by_launchd: bool, marker_age_secs: Option<u64>) -> QuitMarkerAction {
    let is_recent = marker_age_secs
        .map(|age| age < QUIT_MARKER_MAX_AGE_SECS)
        .unwrap_or(false);

    if started_by_launchd && is_recent {
        QuitMarkerAction::ExitKeepMarker
    } else {
        QuitMarkerAction::ContinueDeleteMarker
    }
}

/// Reconciles OS autostart state with desired configuration.
/// This is safe to call on every application startup.
pub fn reconcile_autostart(config: &AutostartConfig, should_enable: bool) -> Result<()> {
    if should_enable {
        #[cfg(target_os = "macos")]
        {
            let quit_marker = directories::ProjectDirs::from("dev", "crowd-cast", "agent")
                .map(|p| p.data_dir().join("quit_requested"));
            if let Some(ref path) = quit_marker {
                if path.exists() {
                    let started_by_launchd =
                        std::env::var("XPC_SERVICE_NAME").is_ok();
                    let marker_age_secs = std::fs::metadata(path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .map(|age| age.as_secs());

                    match check_quit_marker(started_by_launchd, marker_age_secs) {
                        QuitMarkerAction::ExitKeepMarker => {
                            info!("Launchd auto-restart after quit/update — exiting");
                            std::process::exit(0);
                        }
                        QuitMarkerAction::ContinueDeleteMarker => {
                            let _ = std::fs::remove_file(path);
                            info!("Quit marker cleaned up — continuing normally");
                        }
                    }
                }
            }

            let healthy = is_current_macos_launch_agent_healthy(config).unwrap_or(false);
            let disabled = is_current_macos_launch_agent_disabled().unwrap_or(false);

            if !healthy || disabled {
                info!(
                    "Autostart needs repair (healthy={}, disabled={}) - reconfiguring",
                    healthy, disabled
                );
                enable_autostart(config)?;
            }

            return Ok(());
        }

        #[cfg(not(target_os = "macos"))]
        {
            if !is_autostart_enabled() {
                info!("Autostart missing - enabling");
                enable_autostart(config)?;
            }
            return Ok(());
        }
    }

    if has_any_autostart_entry() {
        info!("Autostart disabled by preference - removing entry");
        disable_autostart()?;
    }

    Ok(())
}

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
        assert_eq!(config.app_name, "crowd-cast");
        assert!(config.start_minimized);
    }

    #[test]
    fn test_is_autostart_enabled() {
        let enabled = is_autostart_enabled();
        println!("Autostart enabled: {}", enabled);
    }

    // === Quit marker tests ===

    #[test]
    fn quit_marker_launchd_recent_exits() {
        // launchd auto-restart within 120s of quit → should exit
        assert_eq!(
            check_quit_marker(true, Some(0)),
            QuitMarkerAction::ExitKeepMarker
        );
        assert_eq!(
            check_quit_marker(true, Some(5)),
            QuitMarkerAction::ExitKeepMarker
        );
        assert_eq!(
            check_quit_marker(true, Some(119)),
            QuitMarkerAction::ExitKeepMarker
        );
    }

    #[test]
    fn quit_marker_launchd_stale_continues() {
        // launchd start with stale marker (>120s, e.g. next login) → clean up, continue
        assert_eq!(
            check_quit_marker(true, Some(120)),
            QuitMarkerAction::ContinueDeleteMarker
        );
        assert_eq!(
            check_quit_marker(true, Some(3600)),
            QuitMarkerAction::ContinueDeleteMarker
        );
        assert_eq!(
            check_quit_marker(true, Some(86400)),
            QuitMarkerAction::ContinueDeleteMarker
        );
    }

    #[test]
    fn quit_marker_sparkle_continues() {
        // Sparkle/user launch (no XPC_SERVICE_NAME) → clean up, continue regardless of age
        assert_eq!(
            check_quit_marker(false, Some(0)),
            QuitMarkerAction::ContinueDeleteMarker
        );
        assert_eq!(
            check_quit_marker(false, Some(5)),
            QuitMarkerAction::ContinueDeleteMarker
        );
        assert_eq!(
            check_quit_marker(false, Some(3600)),
            QuitMarkerAction::ContinueDeleteMarker
        );
    }

    #[test]
    fn quit_marker_unknown_age_continues() {
        // Can't determine marker age → treat as stale, continue
        assert_eq!(
            check_quit_marker(true, None),
            QuitMarkerAction::ContinueDeleteMarker
        );
        assert_eq!(
            check_quit_marker(false, None),
            QuitMarkerAction::ContinueDeleteMarker
        );
    }

    #[test]
    fn quit_marker_launchd_throttle_retries_exit() {
        // launchd 10s throttle restarts (marker still <120s) → should still exit
        assert_eq!(
            check_quit_marker(true, Some(10)),
            QuitMarkerAction::ExitKeepMarker
        );
        assert_eq!(
            check_quit_marker(true, Some(20)),
            QuitMarkerAction::ExitKeepMarker
        );
        assert_eq!(
            check_quit_marker(true, Some(60)),
            QuitMarkerAction::ExitKeepMarker
        );
        assert_eq!(
            check_quit_marker(true, Some(110)),
            QuitMarkerAction::ExitKeepMarker
        );
    }

    #[test]
    fn quit_marker_boundary_at_120s() {
        // Exactly 120s → stale (not recent)
        assert_eq!(
            check_quit_marker(true, Some(120)),
            QuitMarkerAction::ContinueDeleteMarker
        );
        // 119s → still recent
        assert_eq!(
            check_quit_marker(true, Some(119)),
            QuitMarkerAction::ExitKeepMarker
        );
    }
}
