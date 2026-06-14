//! Follow-focus provider (Linux): a single, complete-coverage "which window is focused"
//! source for the current session, consumed by the sync engine to gate input capture
//! (record input only while a configured target app is focused).
//!
//! There is no fallback by design — recording is gated on a *live* provider (see
//! `installer::requirements` for the wizard gate and the engine's `start_recording`
//! preflight). Per environment:
//! - **GNOME**: the bundled focus extension over D-Bus. See [`gnome`].
//! - **X11**: resolved synchronously by `frontmost::get_frontmost_app` via EWMH.
//! - **wlroots** (sway/Hyprland/river/...): no picker-free per-window capture source exists
//!   yet, so crowd-cast records full-screen (capture-all) there and does not gate by app — no
//!   focus provider is started, and recording is not gated on one.
//! - other Wayland compositors (KDE, ...): no provider yet → stays not-live (gated off).
#![cfg(target_os = "linux")]

mod gnome;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// The focused application as reported by the active provider.
#[derive(Clone, Debug, Default)]
pub struct FocusInfo {
    /// `wm_class` (GNOME) — the follow-focus identity key.
    pub app_id: String,
    /// Owning PID when the provider exposes it (GNOME extension).
    pub pid: Option<u32>,
    /// Focused window's Mutter id (GNOME extension) — the SAME id `RecordWindow` expects, so
    /// GNOME per-window capture can re-point to the exact focused window (not just the app)
    /// among an app's several toplevels. `None` when no window is focused.
    pub window_id: Option<u64>,
}

/// Shared, thread-safe focus state written by a provider thread and read by the engine.
pub struct FocusState {
    inner: Mutex<Option<FocusInfo>>,
    live: AtomicBool,
}

impl FocusState {
    fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            live: AtomicBool::new(false),
        }
    }
    /// Set the currently focused app (None = no/unknown focused window).
    pub fn set(&self, info: Option<FocusInfo>) {
        if let Ok(mut g) = self.inner.lock() {
            *g = info;
        }
    }
    fn snapshot(&self) -> Option<FocusInfo> {
        self.inner.lock().ok().and_then(|g| g.clone())
    }
    /// Mark whether the provider can currently report focus.
    pub fn set_live(&self, v: bool) {
        self.live.store(v, Ordering::SeqCst);
    }
    fn live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }
}

fn global() -> &'static Arc<FocusState> {
    static S: OnceLock<Arc<FocusState>> = OnceLock::new();
    S.get_or_init(|| Arc::new(FocusState::new()))
}

/// Current focused application, or `None` if unknown / no window focused.
pub fn snapshot() -> Option<FocusInfo> {
    global().snapshot()
}

/// Whether a focus provider for this session is live. `false` means follow-focus can't be
/// trusted, so recording must be gated off (no silent fallback).
pub fn is_live() -> bool {
    global().live()
}

/// All currently-open windows' `wm_class`, sourced from the GNOME focus extension — the SAME
/// identity the gate matches against, so the wizard's app list agrees by construction (no
/// `.desktop` heuristic). GNOME answers via the extension's `ListWindows` D-Bus method on
/// demand. Empty off GNOME: wlroots records full-screen (no per-app selection) and X11
/// enumeration uses `/proc/comm` instead. Deduplicated and sorted; empties filtered.
pub fn list_app_ids() -> Vec<String> {
    if !is_wayland() {
        return Vec::new();
    }
    ensure_started();
    let mut ids = if is_gnome() {
        gnome::list_app_ids()
    } else {
        Vec::new()
    };
    ids.retain(|s| !s.trim().is_empty());
    ids.sort();
    ids.dedup();
    ids
}

fn env(n: &str) -> String {
    std::env::var(n).unwrap_or_default()
}

fn is_wayland() -> bool {
    !env("WAYLAND_DISPLAY").is_empty() || env("XDG_SESSION_TYPE").eq_ignore_ascii_case("wayland")
}

fn desktop() -> String {
    format!(
        "{} {}",
        env("XDG_CURRENT_DESKTOP").to_lowercase(),
        env("XDG_SESSION_DESKTOP").to_lowercase()
    )
}

fn is_gnome() -> bool {
    desktop().contains("gnome")
}

fn is_wlroots() -> bool {
    // XDG_CURRENT_DESKTOP/XDG_SESSION_DESKTOP are authoritative when set: a full desktop
    // environment (GNOME/KDE/...) is never wlroots, even if SWAYSOCK/HYPRLAND_* leaked into
    // its session env (sway exports SWAYSOCK to the systemd/dbus user environment, which
    // persists into a later GNOME login). Only fall back to the compositor-specific sockets
    // when neither desktop variable is set.
    let d = desktop();
    let d = d.trim();
    if !d.is_empty() {
        return ["sway", "wlroots", "hyprland", "river", "wayfire", "labwc"]
            .iter()
            .any(|k| d.contains(k));
    }
    !env("SWAYSOCK").is_empty() || !env("HYPRLAND_INSTANCE_SIGNATURE").is_empty()
}

/// Start the focus provider for this session exactly once (idempotent). Safe to call from
/// any thread / context; the provider runs on its own thread.
pub fn ensure_started() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let st = global().clone();
        if !is_wayland() {
            // X11 / no Wayland: frontmost::get_frontmost_app resolves focus synchronously
            // via EWMH, so the provider is considered live without a watcher thread.
            st.set_live(true);
        } else if is_gnome() {
            gnome::spawn(st);
        } else if is_wlroots() {
            // wlroots (sway/Hyprland/...): no picker-free per-window capture source exists yet,
            // so crowd-cast records full-screen (capture-all) and does not gate by app. No
            // focus provider is needed or started; recording is not gated on one here.
        } else {
            tracing::warn!(
                "follow-focus: no provider for this Wayland compositor; recording will be \
                 gated off until a supported focus source is available"
            );
        }
    });
}
