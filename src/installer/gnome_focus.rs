//! GNOME follow-focus prerequisite: detection, state, and install of the bundled
//! `crowd-cast-focus` GNOME Shell extension.
//!
//! On GNOME (Wayland) there is no app-callable native focused-window API
//! (`org.gnome.Shell.Introspect.GetWindows` is allowlisted to the compositor / portal),
//! so crowd-cast ships a tiny read-only extension that exposes the focused window over a
//! private D-Bus name. This module detects whether that capability is *live* and, if not,
//! classifies why so the wizard can give an exact remediation (install / log out and back
//! in / blocked). The extension can only be *loaded* by gnome-shell at session start, so a
//! freshly installed one needs one logout — but installing it needs no crowd-cast restart.
#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;

/// Extension UUID — must match resources/gnome-extension/<uuid>/metadata.json.
pub const UUID: &str = "crowd-cast-focus@p-doom.org";
/// Private session-bus name the extension owns once gnome-shell has loaded it.
pub const BUS_NAME: &str = "org.crowdcast.FocusProvider";
/// Minimum GNOME Shell major the extension supports: 45 is the ESM cutover (the extension
/// is written in ESM, so it cannot load on older shells). There is intentionally **no upper
/// bound** — the APIs it uses (`Extension`, `Gio.DBusExportedObject`, `global.display`,
/// `Meta.Window.get_pid/get_wm_class/get_title`) are stable across 45+, so we stay
/// forward-compatible rather than hard-failing on each new GNOME release. If a future GNOME
/// genuinely breaks it, the liveness probe catches it (the extension just won't go live).
/// The host's running major is injected into the installed metadata's `shell-version` (see
/// [`render_metadata`]), so GNOME Shell's own version validation always accepts it.
const MIN_SUPPORTED_MAJOR: u32 = 45;

// Extension source, embedded so install needs no external files at runtime.
const METADATA_JSON: &str =
    include_str!("../../resources/gnome-extension/crowd-cast-focus@p-doom.org/metadata.json");
const EXTENSION_JS: &str =
    include_str!("../../resources/gnome-extension/crowd-cast-focus@p-doom.org/extension.js");

/// Why follow-focus is or isn't available on GNOME right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Extension loaded and serving on the bus — prerequisite met.
    Live,
    /// Installed + enabled, but gnome-shell hasn't loaded it yet (needs one logout/login).
    PendingRelogin,
    /// Files not present in any extensions dir.
    NotInstalled,
    /// Installed but not enabled in gsettings.
    NotEnabled,
    /// User extensions are disabled session-wide (`disable-user-extensions`), or an org
    /// policy locks them — the extension can never load here.
    Blocked,
    /// The running GNOME Shell major isn't in the extension's supported set.
    VersionUnsupported(u32),
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn which_exists(prog: &str) -> bool {
    Command::new("which")
        .arg(prog)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Whether the current desktop is GNOME.
pub fn is_gnome() -> bool {
    env("XDG_CURRENT_DESKTOP").to_lowercase().contains("gnome")
        || env("XDG_SESSION_DESKTOP").to_lowercase().contains("gnome")
}

fn user_extensions_dir() -> PathBuf {
    let base = {
        let d = env("XDG_DATA_HOME");
        if !d.is_empty() {
            PathBuf::from(d)
        } else {
            PathBuf::from(env("HOME")).join(".local/share")
        }
    };
    base.join("gnome-shell/extensions").join(UUID)
}

/// metadata.json present in the per-user or a system extensions dir.
fn is_installed() -> bool {
    if user_extensions_dir().join("metadata.json").exists() {
        return true;
    }
    for base in env("XDG_DATA_DIRS").split(':').filter(|s| !s.is_empty()) {
        if PathBuf::from(base)
            .join("gnome-shell/extensions")
            .join(UUID)
            .join("metadata.json")
            .exists()
        {
            return true;
        }
    }
    PathBuf::from("/usr/share/gnome-shell/extensions")
        .join(UUID)
        .join("metadata.json")
        .exists()
}

fn gsettings_get(schema: &str, key: &str) -> Option<String> {
    let out = Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn is_enabled() -> bool {
    gsettings_get("org.gnome.shell", "enabled-extensions")
        .map(|v| v.contains(UUID))
        .unwrap_or(false)
}

/// True when user extensions are globally disabled (the extension can never load).
fn extensions_blocked() -> bool {
    gsettings_get("org.gnome.shell", "disable-user-extensions")
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Running GNOME Shell major version, if detectable (`gnome-shell --version`).
fn gnome_shell_major() -> Option<u32> {
    let out = Command::new("gnome-shell").arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // e.g. "GNOME Shell 46.2"
    text.split_whitespace()
        .find_map(|t| t.split('.').next().and_then(|n| n.parse::<u32>().ok()))
}

/// One-shot D-Bus probe: does anyone own `BUS_NAME` on the session bus right now?
/// Retried briefly because, right after login, gnome-shell may still be bringing
/// extensions up when crowd-cast (autostart) checks. (Event-driven NameOwnerChanged
/// watching belongs in the runtime focus consumer; this is the wizard/preflight probe.)
pub fn is_live() -> bool {
    for attempt in 0..3 {
        if name_has_owner(BUS_NAME) {
            return true;
        }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    }
    false
}

fn name_has_owner(name: &str) -> bool {
    fn truthy(s: &str) -> bool {
        s.contains("true")
    }
    if which_exists("busctl") {
        if let Ok(o) = Command::new("timeout")
            .arg("5")
            .arg("busctl")
            .args([
                "--user",
                "call",
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameHasOwner",
                "s",
                name,
            ])
            .output()
        {
            if o.status.success() {
                return truthy(&String::from_utf8_lossy(&o.stdout));
            }
        }
    }
    if which_exists("gdbus") {
        if let Ok(o) = Command::new("timeout")
            .arg("5")
            .arg("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.freedesktop.DBus",
                "--object-path",
                "/org/freedesktop/DBus",
                "--method",
                "org.freedesktop.DBus.NameHasOwner",
                name,
            ])
            .output()
        {
            if o.status.success() {
                return truthy(&String::from_utf8_lossy(&o.stdout));
            }
        }
    }
    false
}

/// Classify the current GNOME follow-focus prerequisite state. Callers should only invoke
/// this on a GNOME session (see [`is_gnome`]).
pub fn state() -> State {
    if is_live() {
        return State::Live;
    }
    if extensions_blocked() {
        return State::Blocked;
    }
    if let Some(major) = gnome_shell_major() {
        if major < MIN_SUPPORTED_MAJOR {
            return State::VersionUnsupported(major);
        }
    }
    if !is_installed() {
        return State::NotInstalled;
    }
    if !is_enabled() {
        return State::NotEnabled;
    }
    // Installed + enabled + supported, but the bus name isn't owned yet → not loaded.
    State::PendingRelogin
}

/// Render the extension's metadata.json, injecting the host's running GNOME major into
/// `shell-version`. GNOME Shell refuses to load an extension whose metadata doesn't list the
/// running major, so a fixed list would hard-fail on every new GNOME release; instead we
/// always include the detected major (plus a base span for older shells / detection misses).
fn render_metadata() -> String {
    let mut value: serde_json::Value = match serde_json::from_str(METADATA_JSON) {
        Ok(v) => v,
        Err(_) => return METADATA_JSON.to_string(),
    };
    let mut majors: Vec<u32> = (MIN_SUPPORTED_MAJOR..=50).collect();
    if let Some(m) = gnome_shell_major() {
        if m >= MIN_SUPPORTED_MAJOR {
            majors.push(m);
        }
    }
    majors.sort_unstable();
    majors.dedup();
    value["shell-version"] = serde_json::Value::Array(
        majors
            .into_iter()
            .map(|m| serde_json::Value::String(m.to_string()))
            .collect(),
    );
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| METADATA_JSON.to_string())
}

/// Install the bundled extension into the per-user extensions dir and enable it. Does NOT
/// restart anything: gnome-shell loads it at the next login. Returns a human-readable
/// status (Ok) or error message (Err) suitable for showing to the user.
pub fn install_and_enable() -> Result<String, String> {
    if extensions_blocked() {
        return Err(
            "GNOME user extensions are disabled on this system (disable-user-extensions). \
             crowd-cast cannot enable follow-focus here."
                .into(),
        );
    }
    let dir = user_extensions_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    std::fs::write(dir.join("metadata.json"), render_metadata())
        .map_err(|e| format!("Could not write metadata.json: {e}"))?;
    std::fs::write(dir.join("extension.js"), EXTENSION_JS)
        .map_err(|e| format!("Could not write extension.js: {e}"))?;

    // Enable it. `gnome-extensions enable` is preferred; fall back to appending to the
    // gsettings key directly. Either way it only takes effect once gnome-shell reloads.
    let enabled = if which_exists("gnome-extensions") {
        Command::new("gnome-extensions")
            .args(["enable", UUID])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        enable_via_gsettings()
    };
    if !enabled && !is_enabled() {
        // Last resort: try the gsettings path even if the CLI was present but failed.
        enable_via_gsettings();
    }

    Ok(format!(
        "Installed the crowd-cast focus extension to {}. Log out and back in once to \
         activate it; crowd-cast will then be able to record on GNOME.",
        dir.display()
    ))
}

/// Append UUID to `org.gnome.shell enabled-extensions` if not already present.
fn enable_via_gsettings() -> bool {
    let Some(current) = gsettings_get("org.gnome.shell", "enabled-extensions") else {
        return false;
    };
    if current.contains(UUID) {
        return true;
    }
    // current is a GVariant array literal like "['a@b']" or "@as []".
    let inner = current.trim().trim_start_matches("@as").trim();
    let items = inner.trim_start_matches('[').trim_end_matches(']').trim();
    let new_value = if items.is_empty() {
        format!("['{UUID}']")
    } else {
        format!("[{items}, '{UUID}']")
    };
    Command::new("gsettings")
        .args(["set", "org.gnome.shell", "enabled-extensions", &new_value])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
