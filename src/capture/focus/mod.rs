//! Follow-focus provider (Linux): a single, complete-coverage "which window is focused"
//! source for the current session, consumed by the sync engine to gate input capture
//! (record input only while a configured target app is focused).
//!
//! There is no fallback by design — recording is gated on a *live* provider (see
//! `installer::requirements` for the wizard gate and the engine's `start_recording`
//! preflight). Per environment:
//! - **wlroots** (sway/Hyprland/river/...): `zwlr_foreign_toplevel_manager_v1` (all apps,
//!   no a11y). See [`wlr`].
//! - **GNOME**: the bundled focus extension over D-Bus. See [`gnome`].
//! - **X11**: resolved synchronously by `frontmost::get_frontmost_app` via EWMH.
//! - other Wayland compositors (KDE, ...): no provider yet → stays not-live (gated off).
#![cfg(target_os = "linux")]

mod gnome;
mod wlr;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// The focused application as reported by the active provider.
#[derive(Clone, Debug, Default)]
pub struct FocusInfo {
    /// Wayland `app_id` (wlroots) or `wm_class` (GNOME) — the follow-focus identity key.
    pub app_id: String,
    /// Owning PID when the provider exposes it (GNOME extension); `None` on wlroots, where
    /// `zwlr_foreign_toplevel` carries no PID.
    pub pid: Option<u32>,
    /// Focused window's Mutter id when the provider exposes it (GNOME extension) — the SAME
    /// id `RecordWindow` expects, so GNOME per-window capture can re-point to the exact focused
    /// window (not just the app) among an app's several toplevels. `None` on wlroots / when no
    /// window is focused.
    pub window_id: Option<u64>,
}

/// Shared, thread-safe focus state written by a provider thread and read by the engine.
pub struct FocusState {
    inner: Mutex<Option<FocusInfo>>,
    live: AtomicBool,
    /// All current toplevel `app_id`s, maintained by providers that can enumerate windows
    /// without a request/response round-trip (wlroots tracks every toplevel locally). Used by
    /// `list_app_ids` to source the wizard's app list from the SAME identity the gate uses.
    /// GNOME does not populate this (it answers `list_app_ids` with an on-demand D-Bus call).
    windows: Mutex<Vec<String>>,
}

impl FocusState {
    fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            live: AtomicBool::new(false),
            windows: Mutex::new(Vec::new()),
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
    /// Replace the tracked set of all toplevel `app_id`s (wlroots provider).
    pub fn set_windows(&self, ids: Vec<String>) {
        if let Ok(mut g) = self.windows.lock() {
            *g = ids;
        }
    }
    fn windows(&self) -> Vec<String> {
        self.windows.lock().map(|g| g.clone()).unwrap_or_default()
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

/// All currently-open windows' `app_id`/`wm_class`, sourced from the active compositor focus
/// provider — the SAME identity the gate matches against, so the wizard's app list agrees by
/// construction (no `.desktop` heuristic). GNOME answers via the extension's `ListWindows`
/// D-Bus method on demand; wlroots returns its locally-tracked toplevel set. Empty when no
/// provider is live or the session isn't Wayland (X11 enumeration uses `/proc/comm` instead).
/// Deduplicated and sorted; empties filtered.
pub fn list_app_ids() -> Vec<String> {
    if !is_wayland() {
        return Vec::new();
    }
    ensure_started();
    let mut ids = if is_gnome() {
        gnome::list_app_ids()
    } else if is_wlroots() {
        global().windows()
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
    !env("SWAYSOCK").is_empty()
        || !env("HYPRLAND_INSTANCE_SIGNATURE").is_empty()
        || ["sway", "wlroots", "hyprland", "river", "wayfire", "labwc"]
            .iter()
            .any(|k| desktop().contains(k))
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
        } else if is_wlroots() {
            wlr::spawn(st);
        } else if is_gnome() {
            gnome::spawn(st);
        } else {
            tracing::warn!(
                "follow-focus: no provider for this Wayland compositor; recording will be \
                 gated off until a supported focus source is available"
            );
        }
    });
}
