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
        tracing::debug!("Registry key may not have existed");
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

/// Upgrade an EXISTING LaunchAgent plist (written by an older build) in place so
/// launchd also relaunches the agent after a clean nonzero exit, not only after
/// a crash: `KeepAlive.Crashed` covers signal deaths ONLY, so a startup failure
/// that `exit(1)`s (e.g. relaunching into post-wake display flux) used to leave
/// the agent dead until the next login. Patches ONLY the KeepAlive dict via
/// exact-string replacement — ProgramArguments and everything else are left
/// untouched, so a dev build running from a different path can never repoint
/// the user's autostart. The wizard-written template already includes the key;
/// this heals installs from before it did. The patched plist takes effect at
/// the next launchd load (login/reboot) — no bootout of the running job.
/// Call at startup; a no-op when autostart is off or the plist is current.
#[cfg(target_os = "macos")]
pub fn refresh_launch_agent_keepalive() -> Result<()> {
    let plist_path = get_launch_agent_path()?;
    if !plist_path.exists() {
        return Ok(()); // autostart not enabled — nothing to refresh
    }
    let content = std::fs::read_to_string(&plist_path)
        .with_context(|| format!("Failed to read LaunchAgent plist at {:?}", plist_path))?;
    if content.contains("<key>SuccessfulExit</key>") {
        return Ok(()); // already current
    }
    let old_keepalive = "    <key>KeepAlive</key>\n    <dict>\n        <key>Crashed</key>\n        <true/>\n    </dict>";
    let new_keepalive = "    <key>KeepAlive</key>\n    <dict>\n        <key>Crashed</key>\n        <true/>\n        <key>SuccessfulExit</key>\n        <false/>\n    </dict>";
    if !content.contains(old_keepalive) {
        // Unexpected layout (hand-edited?) — leave it alone rather than guess.
        info!("LaunchAgent plist has an unexpected KeepAlive layout; not patching");
        return Ok(());
    }
    let patched = content.replace(old_keepalive, new_keepalive);
    std::fs::write(&plist_path, patched)
        .with_context(|| format!("Failed to write LaunchAgent plist at {:?}", plist_path))?;
    info!("Upgraded LaunchAgent KeepAlive to also relaunch on nonzero exit");
    Ok(())
}

/// Non-macOS stub (LaunchAgent plists are macOS-only).
#[cfg(not(target_os = "macos"))]
pub fn refresh_launch_agent_keepalive() -> Result<()> {
    Ok(())
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
    <dict>
        <key>Crashed</key>
        <true/>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
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
    // process ungracefully (losing in-flight recordings).

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

    // Check label, exe path, and KeepAlive/Crashed dict (not unconditional true).
    Ok(contents.contains(MACOS_AUTOSTART_LABEL)
        && contents.contains(&format!("<string>{}</string>", expected_exe))
        && contents.contains("<key>Crashed</key>"))
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

/// A wlroots-based compositor (sway/Hyprland/river/wayfire/labwc/...). Unlike full desktop
/// environments (GNOME/KDE/XFCE), these do NOT run XDG autostart entries, so a
/// `~/.config/autostart/*.desktop` file is inert there and must not be reported as "enabled".
#[cfg(target_os = "linux")]
fn is_wlroots_session() -> bool {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    // XDG_CURRENT_DESKTOP is authoritative when set. Full desktop environments (GNOME/KDE/
    // XFCE) honor XDG autostart and are NEVER wlroots — even though SWAYSOCK/HYPRLAND_* can
    // leak into their session env: sway exports SWAYSOCK to the systemd/dbus user environment,
    // which persists into a later GNOME login and would otherwise misclassify GNOME as sway.
    // Only fall back to the compositor sockets when XDG_CURRENT_DESKTOP is unset.
    let d = env("XDG_CURRENT_DESKTOP").to_lowercase();
    if !d.is_empty() {
        return ["sway", "wlroots", "hyprland", "river", "wayfire", "labwc"]
            .iter()
            .any(|c| d.contains(c));
    }
    !env("SWAYSOCK").is_empty() || !env("HYPRLAND_INSTANCE_SIGNATURE").is_empty()
}

#[cfg(target_os = "linux")]
fn config_home() -> PathBuf {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let cfg = env("XDG_CONFIG_HOME");
    if cfg.is_empty() {
        PathBuf::from(env("HOME")).join(".config")
    } else {
        PathBuf::from(cfg)
    }
}

/// Manual autostart instructions for a compositor that does not run XDG autostart entries.
/// Carries the exact line the user must paste and where it goes.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct ManualAutostart {
    /// Human-readable compositor name (e.g. "sway", "Hyprland").
    pub compositor: String,
    /// The config file the user should add the line to.
    pub config_path: PathBuf,
    /// The exact line to paste, in that compositor's syntax.
    pub exec_line: String,
    /// A ready-to-run shell command that appends `exec_line` to `config_path`, so the user
    /// can fix it with one copy-paste. Only surfaced while the line is absent, so no
    /// in-command idempotency guard is needed.
    pub command: String,
}

/// The manual autostart line for the current session, or `None` on desktops that honor XDG
/// autostart (GNOME/KDE/XFCE) and on X11 DEs — there autostart is fully automatic.
#[cfg(target_os = "linux")]
pub fn linux_manual_autostart() -> Option<ManualAutostart> {
    linux_manual_autostart_for(&AutostartConfig::default())
}

#[cfg(target_os = "linux")]
fn linux_manual_autostart_for(config: &AutostartConfig) -> Option<ManualAutostart> {
    if !is_wlroots_session() {
        return None;
    }
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let exe = config.app_path.to_string_lossy().to_string();
    let args = if config.args.is_empty() {
        String::new()
    } else {
        format!(" {}", config.args.join(" "))
    };
    let cfg = config_home();
    let desktop = env("XDG_CURRENT_DESKTOP").to_lowercase();

    // Per-compositor config path + exec-directive syntax. Giving sway's `exec` line on
    // Hyprland (which uses `exec-once`) would be wrong, so each is handled explicitly.
    let (compositor, config_path, exec_line) =
        if !env("HYPRLAND_INSTANCE_SIGNATURE").is_empty() || desktop.contains("hyprland") {
            (
                "Hyprland".to_string(),
                cfg.join("hypr").join("hyprland.conf"),
                format!("exec-once = {exe}{args}"),
            )
        } else if desktop.contains("river") {
            (
                "river".to_string(),
                cfg.join("river").join("init"),
                format!("riverctl spawn \"{exe}{args}\""),
            )
        } else {
            // sway, plus other sway-style wlroots compositors.
            let name = if !env("SWAYSOCK").is_empty() || desktop.contains("sway") {
                "sway".to_string()
            } else {
                "your wlroots compositor".to_string()
            };
            (
                name,
                cfg.join("sway").join("config"),
                format!("exec {exe}{args}"),
            )
        };

    // Append the line to the config. No idempotency guard needed: this is only ever surfaced
    // while the line is absent (the requirement is hidden once `is_autostart_enabled()` sees
    // it), so the "show only when not satisfied" check upstream already prevents a re-append.
    let cfg_str = config_path.to_string_lossy();
    let command = format!("echo '{exec_line}' >> {cfg_str}");

    Some(ManualAutostart {
        compositor,
        config_path,
        exec_line,
        command,
    })
}

/// True iff the compositor config (and any `config.d/` includes) contains an uncommented
/// line that launches our binary — i.e. the user actually pasted the autostart line. This
/// is the wlroots equivalent of "is the autostart entry present", and avoids the false
/// positive of trusting an inert `~/.config/autostart/*.desktop`.
#[cfg(target_os = "linux")]
fn compositor_config_has_exec(m: &ManualAutostart) -> bool {
    let exe = AutostartConfig::default()
        .app_path
        .to_string_lossy()
        .to_string();
    if exe.is_empty() {
        return false;
    }
    let mut files = vec![m.config_path.clone()];
    if let Some(parent) = m.config_path.parent() {
        // sway commonly splits its config across a `config.d/` directory.
        if let Ok(entries) = std::fs::read_dir(parent.join("config.d")) {
            files.extend(entries.flatten().map(|e| e.path()));
        }
    }
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with('#') {
                continue;
            }
            // Match on the binary path on an exec/spawn directive so prompt/formatting
            // differences don't cause a false negative.
            if l.contains(&exe) && (l.contains("exec") || l.contains("spawn")) {
                return true;
            }
        }
    }
    false
}

#[cfg(target_os = "linux")]
fn is_autostart_enabled_linux() -> bool {
    // wlroots: "enabled" iff the exec line is actually present in the compositor config.
    if let Some(m) = linux_manual_autostart() {
        return compositor_config_has_exec(&m);
    }
    // XDG-autostart desktops (GNOME/KDE/XFCE) / X11: the desktop file existing is sufficient.
    get_autostart_path().map(|p| p.exists()).unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn enable_autostart_linux(config: &AutostartConfig) -> Result<()> {
    use std::fs;

    // wlroots compositors don't run XDG autostart entries. Writing one would be an inert
    // artifact that falsely reports "enabled" (violates the no-fallback / fail-closed law).
    // Autostart there requires a manual config line, surfaced by the wizard via
    // `linux_manual_autostart()`; there is nothing to write here.
    if let Some(m) = linux_manual_autostart_for(config) {
        info!(
            "Autostart on {} is manual — user must add `{}` to {:?} (no XDG autostart support)",
            m.compositor, m.exec_line, m.config_path
        );
        return Ok(());
    }

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

/// Reconciles OS autostart state with desired configuration.
/// This is safe to call on every application startup.
pub fn reconcile_autostart(config: &AutostartConfig, should_enable: bool) -> Result<()> {
    if should_enable {
        #[cfg(target_os = "macos")]
        {
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
}
