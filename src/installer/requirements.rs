//! Host requirement detection for the Linux setup wizard.
//!
//! General across distros and compositors: detects the GPU render node, screen-capture
//! capability for the current session, input-group membership, and VAAPI hardware encode.
//! Each unmet requirement carries an exact, copy-pasteable install `command` tailored to
//! the detected package manager (pacman/apt/dnf/zypper/apk/emerge/nix) and the detected
//! compositor (which xdg-desktop-portal backend). Mirrors the macOS wizard's permission
//! gating; the GTK wizard renders these and hard-gates "Finish" on unmet Required items.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

/// How strongly a requirement is enforced in the wizard.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Must be satisfied to finish setup (hard-gates the Finish button).
    Required = 0,
    /// Strongly suggested; shown but does not block.
    Recommended = 1,
    /// Informational; shown but does not block.
    Optional = 2,
}

/// A single host requirement and whether it is currently met.
pub struct Requirement {
    pub label: String,
    /// Prose explanation / why it matters.
    pub detail: String,
    /// Exact shell command to fix it (empty if none / not applicable).
    pub command: String,
    pub severity: Severity,
    pub satisfied: bool,
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

// ===========================================================================
// Distribution / package-manager detection
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pkg {
    Arch,
    Debian,
    Fedora,
    Suse,
    Alpine,
    Gentoo,
    Nix,
    Unknown,
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').trim_matches('\'').to_string()
}

/// `ID` and `ID_LIKE` tokens from /etc/os-release, lowercased.
fn os_release_ids() -> Vec<String> {
    let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let mut ids = Vec::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            ids.push(unquote(v));
        } else if let Some(v) = line.strip_prefix("ID_LIKE=") {
            for t in unquote(v).split_whitespace() {
                ids.push(t.to_string());
            }
        }
    }
    ids.into_iter().map(|s| s.to_lowercase()).collect()
}

fn detect_pkg() -> Pkg {
    let ids = os_release_ids();
    let has = |k: &str| ids.iter().any(|i| i == k);
    if has("nixos") {
        Pkg::Nix
    } else if has("arch") || has("manjaro") || has("endeavouros") || has("garuda")
        || has("artix") || has("arcolinux") || has("cachyos")
    {
        Pkg::Arch
    } else if has("debian") || has("ubuntu") || has("linuxmint") || has("pop")
        || has("elementary") || has("raspbian") || has("kali") || has("zorin") || has("neon")
    {
        Pkg::Debian
    } else if has("fedora") || has("rhel") || has("centos") || has("rocky")
        || has("almalinux") || has("nobara")
    {
        Pkg::Fedora
    } else if ids.iter().any(|i| i.starts_with("opensuse")) || has("suse") || has("sles") {
        Pkg::Suse
    } else if has("alpine") {
        Pkg::Alpine
    } else if has("gentoo") {
        Pkg::Gentoo
    } else {
        Pkg::Unknown
    }
}

/// Build an exact install command for `packages` on the detected distro.
fn install_cmd(pkg: Pkg, packages: &[String]) -> String {
    let pkgs = packages.join(" ");
    match pkg {
        Pkg::Arch => format!("sudo pacman -S --needed {pkgs}"),
        Pkg::Debian => format!("sudo apt install {pkgs}"),
        Pkg::Fedora => format!("sudo dnf install {pkgs}"),
        Pkg::Suse => format!("sudo zypper install {pkgs}"),
        Pkg::Alpine => format!("sudo apk add {pkgs}"),
        Pkg::Gentoo => format!("sudo emerge {pkgs}"),
        Pkg::Nix => format!("nix-env -iA {} (or add to configuration.nix)",
            packages.iter().map(|p| format!("nixpkgs.{p}")).collect::<Vec<_>>().join(" ")),
        Pkg::Unknown => format!("install with your package manager: {pkgs}"),
    }
}

// ===========================================================================
// Compositor / portal-backend detection
// ===========================================================================

fn desktop() -> String {
    let d = env("XDG_CURRENT_DESKTOP").to_lowercase();
    if !d.is_empty() {
        return d;
    }
    if !env("SWAYSOCK").is_empty() {
        return "sway".into();
    }
    if !env("HYPRLAND_INSTANCE_SIGNATURE").is_empty() {
        return "hyprland".into();
    }
    String::new()
}

fn current_desktops() -> Vec<String> {
    let mut v: Vec<String> = env("XDG_CURRENT_DESKTOP")
        .split(':')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if !env("SWAYSOCK").is_empty() && !v.iter().any(|d| d == "sway") {
        v.push("sway".into());
    }
    if !env("HYPRLAND_INSTANCE_SIGNATURE").is_empty() && !v.iter().any(|d| d.contains("hyprland")) {
        v.push("hyprland".into());
    }
    v
}

fn portal_config_dirs() -> Vec<String> {
    let mut dirs = Vec::new();
    let cfg_home = {
        let h = env("XDG_CONFIG_HOME");
        if !h.is_empty() {
            h
        } else {
            let home = env("HOME");
            if home.is_empty() { String::new() } else { format!("{home}/.config") }
        }
    };
    if !cfg_home.is_empty() {
        dirs.push(format!("{cfg_home}/xdg-desktop-portal"));
    }
    dirs.push("/etc/xdg/xdg-desktop-portal".into());
    for base in env("XDG_DATA_DIRS").split(':').filter(|s| !s.is_empty()) {
        dirs.push(format!("{base}/xdg-desktop-portal"));
    }
    dirs.push("/usr/share/xdg-desktop-portal".into());
    dirs.dedup();
    dirs
}

/// The xdg-desktop-portal backend short-name expected for the current compositor.
fn compositor_backend_shortname() -> String {
    let d = desktop();
    if d.contains("gnome") {
        "gnome".into()
    } else if d.contains("kde") || d.contains("plasma") {
        "kde".into()
    } else if d.contains("hyprland") {
        "hyprland".into()
    } else if d.contains("sway") || d.contains("wlroots") || d.contains("river")
        || d.contains("labwc") || d.contains("wayfire")
    {
        "wlr".into()
    } else {
        String::new()
    }
}

/// Preferred ScreenCast backend short-names from portals.conf (modern selection).
fn configured_screencast_backends() -> Vec<String> {
    let desktops = current_desktops();
    for dir in portal_config_dirs() {
        let mut candidates: Vec<String> = desktops
            .iter()
            .map(|d| format!("{dir}/{d}-portals.conf"))
            .collect();
        candidates.push(format!("{dir}/portals.conf"));
        for path in candidates {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let mut in_pref = false;
            let mut screencast: Option<String> = None;
            let mut default: Option<String> = None;
            for line in text.lines() {
                let l = line.trim();
                if l.starts_with('[') {
                    in_pref = l.eq_ignore_ascii_case("[preferred]");
                    continue;
                }
                if !in_pref || l.is_empty() || l.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = l.split_once('=') {
                    match k.trim() {
                        "org.freedesktop.impl.portal.ScreenCast" => screencast = Some(v.trim().into()),
                        "default" => default = Some(v.trim().into()),
                        _ => {}
                    }
                }
            }
            if let Some(val) = screencast.or(default) {
                return val
                    .split(';')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && s != "none" && s != "*")
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Installed backends (`.portal` stem, UseIn list) that declare the ScreenCast impl.
fn installed_screencast_backends() -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for dir in portal_config_dirs() {
        let portals = format!("{dir}/portals");
        let Ok(entries) = std::fs::read_dir(&portals) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("portal") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let declares = text.lines().any(|l| {
                let l = l.trim_start();
                l.starts_with("Interfaces=") && l.contains("org.freedesktop.impl.portal.ScreenCast")
            });
            if !declares {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
            let useins: Vec<String> = text
                .lines()
                .find_map(|l| {
                    l.trim().strip_prefix("UseIn=").map(|v| {
                        v.split(';').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect()
                    })
                })
                .unwrap_or_default();
            out.push((stem, useins));
        }
    }
    out
}

/// (satisfied, missing-backend-short-name). Respects portals.conf selection, falls
/// back to UseIn matching; the short-name maps to `xdg-desktop-portal-<name>`.
fn screencast_status() -> (bool, Option<String>) {
    let installed = installed_screencast_backends();
    let configured = configured_screencast_backends();

    if !configured.is_empty() {
        if configured.iter().any(|want| installed.iter().any(|(stem, _)| stem == want)) {
            return (true, None);
        }
        return (false, Some(configured[0].clone()));
    }

    let desktops = current_desktops();
    for (_stem, useins) in &installed {
        if useins.iter().any(|u| desktops.iter().any(|d| d == u)) {
            return (true, None);
        }
    }
    let sn = compositor_backend_shortname();
    (false, Some(if sn.is_empty() { "wlr".into() } else { sn }))
}

// ===========================================================================
// Other host checks
// ===========================================================================

fn which_exists(prog: &str) -> bool {
    Command::new("which").arg(prog).output().map(|o| o.status.success()).unwrap_or(false)
}

/// Whether the ScreenCast portal interface is actually exposed on the session bus
/// (a backend is installed AND the portal is running/serving it). Returns None if we
/// cannot query D-Bus (caller then falls back to the file check). Bounded by `timeout`
/// so a non-responsive portal cannot hang the preflight.
fn screencast_dbus_available() -> Option<bool> {
    fn introspect(prog: &str, args: &[&str]) -> Option<bool> {
        let out = Command::new("timeout").arg("5").arg(prog).args(args).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).contains("org.freedesktop.portal.ScreenCast"))
    }
    if which_exists("busctl") {
        if let Some(r) = introspect(
            "busctl",
            &["--user", "introspect", "org.freedesktop.portal.Desktop",
              "/org/freedesktop/portal/desktop"],
        ) {
            return Some(r);
        }
    }
    if which_exists("gdbus") {
        if let Some(r) = introspect(
            "gdbus",
            &["introspect", "--session", "--dest", "org.freedesktop.portal.Desktop",
              "--object-path", "/org/freedesktop/portal/desktop"],
        ) {
            return Some(r);
        }
    }
    None
}

/// Whether crowd-cast can do per-app (per-window) capture on this host.
///
/// Two backends:
/// - **Wayland**: xdg-desktop-portal ScreenCast advertising WINDOW capture (bit 2 of
///   AvailableSourceTypes). GNOME/KDE do; wlroots/sway report MONITOR only.
/// - **Pure X11**: XComposite per-window capture, gated on an EWMH-capable WM and the X
///   Composite extension (see `capture::x11_windows`).
///
/// Returns false on non-Linux. When false the wizard greys out the per-app picker and an
/// existing per-app config re-triggers the (gated) wizard, so the agent never runs an
/// unsatisfiable config.
pub fn per_app_capture_available() -> bool {
    window_capture_supported()
}

/// Whether the active session can capture individual windows — via the Wayland ScreenCast
/// portal (WINDOW bit) or, on a pure X11 session, via XComposite.
pub fn window_capture_supported() -> bool {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|s| s.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false);
    if !wayland {
        // Pure X11: XComposite per-window capture (EWMH + Composite extension required).
        #[cfg(target_os = "linux")]
        {
            return crate::capture::x11_windows::x11_per_app_capable();
        }
        #[cfg(not(target_os = "linux"))]
        {
            return false;
        }
    }
    // AvailableSourceTypes bitmask: 1=MONITOR, 2=WINDOW, 4=VIRTUAL.
    available_source_types().map(|t| t & 2 != 0).unwrap_or(false)
}

/// Query the ScreenCast portal's `AvailableSourceTypes` property over D-Bus (busctl or
/// gdbus). Returns None if the portal isn't reachable or neither tool is present.
fn available_source_types() -> Option<u32> {
    fn last_int(s: &str) -> Option<u32> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|t| !t.is_empty())
            .last()
            .and_then(|t| t.parse().ok())
    }
    if which_exists("busctl") {
        if let Ok(o) = Command::new("timeout")
            .arg("5")
            .arg("busctl")
            .args([
                "--user",
                "get-property",
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.ScreenCast",
                "AvailableSourceTypes",
            ])
            .output()
        {
            if o.status.success() {
                if let Some(v) = last_int(&String::from_utf8_lossy(&o.stdout)) {
                    return Some(v);
                }
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
                "org.freedesktop.portal.Desktop",
                "--object-path",
                "/org/freedesktop/portal/desktop",
                "--method",
                "org.freedesktop.DBus.Properties.Get",
                "org.freedesktop.portal.ScreenCast",
                "AvailableSourceTypes",
            ])
            .output()
        {
            if o.status.success() {
                if let Some(v) = last_int(&String::from_utf8_lossy(&o.stdout)) {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn pipewire_running() -> bool {
    let runtime = env("XDG_RUNTIME_DIR");
    if !runtime.is_empty() && Path::new(&format!("{runtime}/pipewire-0")).exists() {
        return true;
    }
    Command::new("pgrep").arg("-x").arg("pipewire").output().map(|o| o.status.success()).unwrap_or(false)
}

fn in_input_group() -> bool {
    Command::new("id")
        .arg("-nG")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().any(|g| g == "input"))
        .unwrap_or(false)
}

fn gpu_render_node() -> bool {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else { return false };
    entries.flatten().any(|e| e.file_name().to_str().map(|n| n.starts_with("renderD")).unwrap_or(false))
}

fn vaapi_ok() -> bool {
    Command::new("vainfo").output().map(|o| o.status.success()).unwrap_or(false)
}

/// GPU vendors present, from /sys/class/drm/card*/device/vendor.
fn gpu_vendors() -> Vec<&'static str> {
    let mut v = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with("card") && !name.contains('-')) {
                continue;
            }
            let vpath = e.path().join("device/vendor");
            if let Ok(id) = std::fs::read_to_string(&vpath) {
                let id = id.trim().to_lowercase();
                let vendor = match id.as_str() {
                    "0x8086" => "intel",
                    "0x1002" => "amd",
                    "0x10de" => "nvidia",
                    _ => continue,
                };
                if !v.contains(&vendor) {
                    v.push(vendor);
                }
            }
        }
    }
    v
}

/// Best-effort VAAPI driver + utils package list for the distro and GPU vendor(s).
fn vaapi_packages(pkg: Pkg) -> Vec<String> {
    let vendors = gpu_vendors();
    let mut pkgs: Vec<String> = Vec::new();
    // vainfo / libva-utils
    pkgs.push(match pkg {
        Pkg::Debian => "vainfo".into(),
        _ => "libva-utils".into(),
    });
    for vendor in &vendors {
        match (*vendor, pkg) {
            ("intel", Pkg::Debian) => pkgs.push("intel-media-va-driver".into()),
            ("intel", _) => pkgs.push("intel-media-driver".into()),
            ("amd", Pkg::Arch) => pkgs.push("libva-mesa-driver".into()),
            ("amd", _) => pkgs.push("mesa-va-drivers".into()),
            // NVIDIA VAAPI relies on the non-standard nvidia-vaapi-driver (often AUR /
            // third-party) and OBS drives NVIDIA via NVENC separately, so it is omitted
            // from the auto-generated install command rather than emit a failing one.
            ("nvidia", _) => {}
            _ => {}
        }
    }
    pkgs
}

// ===========================================================================
// Public API
// ===========================================================================

/// `<this binary> --install-focus-extension` — the copy-pasteable command that installs
/// and enables the bundled GNOME focus extension.
fn self_install_focus_cmd() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .map(|exe| format!("{exe} --install-focus-extension"))
        .unwrap_or_else(|| "crowd-cast --install-focus-extension".into())
}

/// Build the follow-focus provider requirement for the current session.
///
/// GNOME has no app-callable native focus API, so it requires the bundled focus extension
/// to be installed, enabled, and *loaded* (live on D-Bus). The other states map to exact
/// remediations. Other compositors (wlroots/KDE/X11) expose focus natively, so the host
/// requirement is considered met there (the matching in-process provider is wired
/// separately; see capture/focus).
fn focus_provider_requirement() -> Requirement {
    use super::gnome_focus::{self, State};
    let label = "Follow-focus (detect which window is focused)".to_string();

    if gnome_focus::is_gnome() {
        let (satisfied, detail, command) = match gnome_focus::state() {
            State::Live => (true, String::new(), String::new()),
            State::PendingRelogin => (
                false,
                "The crowd-cast focus extension is installed but GNOME hasn't loaded it yet. \
                 Log out and back in once to activate it."
                    .into(),
                String::new(),
            ),
            State::NotInstalled => (
                false,
                "GNOME needs the bundled crowd-cast focus extension to detect the focused \
                 window (GNOME exposes no other reliable way). Install it, then log out and \
                 back in once."
                    .into(),
                self_install_focus_cmd(),
            ),
            State::NotEnabled => (
                false,
                "The crowd-cast focus extension is installed but not enabled. Enable it, then \
                 log out and back in once."
                    .into(),
                self_install_focus_cmd(),
            ),
            State::Blocked => (
                false,
                "GNOME user extensions are disabled on this system (disable-user-extensions / \
                 org policy), so the focus extension cannot load. crowd-cast cannot record on \
                 GNOME until extensions are permitted."
                    .into(),
                String::new(),
            ),
            State::VersionUnsupported(v) => (
                false,
                format!(
                    "crowd-cast's GNOME focus extension requires GNOME Shell 45 or newer \
                     (this session is GNOME {v}). Newer GNOME versions are supported \
                     automatically."
                ),
                String::new(),
            ),
        };
        return Requirement { label, detail, command, severity: Severity::Required, satisfied };
    }

    // wlroots (wlr-foreign-toplevel) / KDE (KWin) / X11 (_NET_ACTIVE_WINDOW) expose focus
    // natively and completely, so the host requirement is met. The in-process provider that
    // consumes them is wired separately; this gate only asserts host capability.
    Requirement {
        label,
        detail: String::new(),
        command: String::new(),
        severity: Severity::Required,
        satisfied: true,
    }
}

/// Collect the host requirements for the current Linux session.
pub fn collect() -> Vec<Requirement> {
    let pkg = detect_pkg();
    let wayland = !env("WAYLAND_DISPLAY").is_empty() || env("XDG_SESSION_TYPE") == "wayland";
    let x11 = !env("DISPLAY").is_empty();

    let mut reqs = Vec::new();

    // 1. GPU render node.
    let gpu = gpu_render_node();
    reqs.push(Requirement {
        label: "GPU render device".into(),
        detail: if gpu {
            String::new()
        } else {
            "No /dev/dri/renderD* found. Install your GPU's Mesa/NVIDIA userspace drivers.".into()
        },
        command: String::new(),
        severity: Severity::Required,
        satisfied: gpu,
    });

    // 2. Screen-capture capability for this session.
    if wayland {
        let pw = pipewire_running();
        let (configured_ok, missing) = screencast_status();
        let dbus = screencast_dbus_available();
        // Functional truth if we can query D-Bus; otherwise trust the file/config check.
        let live = dbus.unwrap_or(configured_ok);
        let satisfied = pw && live;
        let mut detail = String::new();
        let mut command = String::new();
        if !satisfied {
            if !configured_ok {
                let sn = missing.unwrap_or_else(|| "wlr".into());
                detail = "Your compositor has no ScreenCast portal backend installed.".into();
                command = install_cmd(pkg, &[format!("xdg-desktop-portal-{sn}")]);
            } else if dbus == Some(false) {
                detail = "A ScreenCast portal backend is installed but the portal is not serving it -- restart the portal. On wlroots compositors (sway/Hyprland), also export WAYLAND_DISPLAY to the D-Bus activation environment (run dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP in your compositor config).".into();
                command = "systemctl --user restart xdg-desktop-portal".into();
            }
            if !pw {
                if detail.is_empty() {
                    detail = "The PipeWire daemon is not running.".into();
                } else {
                    detail.push_str(" PipeWire is also not running.");
                }
                if command.is_empty() {
                    command = "systemctl --user enable --now pipewire pipewire-pulse".into();
                }
            }
        }
        reqs.push(Requirement {
            label: "Screen capture (Wayland: PipeWire + a ScreenCast portal backend)".into(),
            detail,
            command,
            severity: Severity::Required,
            satisfied,
        });
    } else if x11 {
        reqs.push(Requirement {
            label: "Screen capture (X11)".into(),
            detail: String::new(),
            command: String::new(),
            severity: Severity::Required,
            satisfied: true,
        });
    } else {
        reqs.push(Requirement {
            label: "Graphical session".into(),
            detail: "No Wayland or X11 session detected (WAYLAND_DISPLAY / DISPLAY unset).".into(),
            command: String::new(),
            severity: Severity::Required,
            satisfied: false,
        });
    }

    // 2b. Follow-focus provider: crowd-cast records input only while a configured target
    // app is focused, so it needs a reliable, complete-coverage "which window is focused"
    // source for this session. There is no fallback by design — recording is gated on this.
    // GNOME (no app-callable native focus API) requires the bundled focus extension; other
    // compositors expose it natively (wlr-foreign-toplevel / KWin) or via EWMH on X11.
    reqs.push(focus_provider_requirement());

    // 3. input group (evdev input capture).
    let input = in_input_group();
    reqs.push(Requirement {
        label: "Input capture ('input' group)".into(),
        detail: if input {
            String::new()
        } else {
            "Add yourself to the 'input' group, then log out and back in.".into()
        },
        command: if input { String::new() } else { "sudo usermod -aG input $USER".into() },
        severity: Severity::Recommended,
        satisfied: input,
    });

    // 4. VAAPI hardware encode (optional; x264 fallback otherwise).
    let va = vaapi_ok();
    reqs.push(Requirement {
        label: "Hardware video encoding (VAAPI)".into(),
        detail: if va {
            String::new()
        } else {
            "Optional — enables GPU video encoding (otherwise software x264 is used).".into()
        },
        command: if va { String::new() } else { install_cmd(pkg, &vaapi_packages(pkg)) },
        severity: Severity::Optional,
        satisfied: va,
    });

    reqs
}

/// True if any Required host requirement is currently unmet. Used to re-gate the
/// wizard on every launch (not just first run), so a later-broken/uninstalled
/// component (e.g. the ScreenCast portal) re-surfaces the gated wizard.
pub fn has_unmet_required() -> bool {
    collect()
        .iter()
        .any(|r| r.severity == Severity::Required && !r.satisfied)
}
