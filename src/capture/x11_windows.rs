//! X11 window resolution for per-app (per-window) capture.
//!
//! `xcomposite_input` captures a window by its X11 **window id**, but crowd-cast tracks apps
//! by identity. The single canonical identity key is `/proc/<pid>/comm` — the *same* key app
//! enumeration (`apps.rs`), the wizard's `target_apps`, and `frontmost::get_frontmost_app`
//! all use, so every layer agrees by construction.
//!
//! **No fallbacks (by design).** Follow-focus only ever captures the *focused* window, so we
//! bind exactly `_NET_ACTIVE_WINDOW` and nothing else — no client-list scan, no "topmost
//! window of the app" guess, no multi-key matching, and no reliance on OBS's internal
//! name+class re-find. We emit a bare decimal window id and re-resolve deterministically on
//! every focus switch and capture-watchdog refresh. If the focused window doesn't belong to
//! the app the engine asked us to capture (focus moved between the engine's decision and this
//! read) or carries no PID, we return `None`: the caller leaves the source blank and the
//! engine's readiness gate (`active_source_is_ready` → `should_enable_capture_for_target`)
//! keeps input capture off. Fail-closed, never a wrong-window capture.
//!
//! **Pure X11 only.** Under a Wayland session the X11/EWMH view sees only XWayland clients,
//! so callers gate on a non-Wayland session (see [`is_pure_x11_session`], mirrored by
//! `installer::requirements` and `capture::frontmost`).
#![cfg(target_os = "linux")]

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
use x11rb::rust_connection::RustConnection;

/// True for a *pure* X11 session: an X server is reachable and this is not a Wayland session
/// (where `DISPLAY` would only address XWayland). This gates the entire X11 per-app path; on
/// Wayland the portal/PipeWire path owns window capture.
pub fn is_pure_x11_session() -> bool {
    if std::env::var_os("DISPLAY").is_none() {
        return false;
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return false;
    }
    !std::env::var("XDG_SESSION_TYPE")
        .map(|s| s.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false)
}

/// Whether this host can do XComposite per-window capture: a pure X11 session whose WM
/// publishes `_NET_ACTIVE_WINDOW` (the focus hint we resolve against), with the X Composite
/// extension present (required for the off-screen window pixmaps `xcomposite_input` reads).
/// When false the wizard greys out the per-app picker (`requirements`), so we never enter the
/// capture path on a host that can't satisfy it.
pub fn x11_per_app_capable() -> bool {
    if !is_pure_x11_session() {
        return false;
    }
    let Ok((conn, _)) = x11rb::connect(None) else {
        return false;
    };
    // The focus hint the resolver depends on. Absent ⇒ we could never resolve a window.
    if !atom_exists(&conn, b"_NET_ACTIVE_WINDOW") {
        return false;
    }
    // X Composite extension — no extension means no redirected pixmap to capture.
    conn.query_extension(b"Composite")
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.present)
        .unwrap_or(false)
}

/// The X11 screen (root window) pixel dimensions for a pure-X11 session — used as the capture
/// canvas / recording-metadata resolution. Core-protocol only (no RANDR dependency), so it is
/// always available on a reachable X server. `None` only if the server is unreachable or
/// reports a zero-sized screen (caller fails closed rather than guessing a size).
pub fn x11_screen_size() -> Option<(u32, u32)> {
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let screen = conn.setup().roots.get(screen_num)?;
    let (w, h) = (screen.width_in_pixels as u32, screen.height_in_pixels as u32);
    (w > 0 && h > 0).then_some((w, h))
}

/// Resolve `app_identity` (a `/proc/comm`) to the decimal window id of the **currently
/// focused window**, but only if that window still belongs to `app_identity`. Returns `None`
/// otherwise (focus moved, or the focused window has no PID) — caller leaves the source
/// blank and input stays gated off. See the module docs: there is deliberately no fallback.
pub fn resolve_capture_window(app_identity: &str) -> Option<String> {
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen_num)?.root;

    let active = net_active_window(&conn, root)?;
    let focused_comm = net_wm_pid(&conn, active).and_then(proc_comm);

    // Single canonical key: comm-vs-comm is exact (both are kernel-truncated identically),
    // so no tolerance and no alternate keys are needed.
    focused_belongs_to(focused_comm.as_deref(), app_identity).then(|| active.to_string())
}

/// The focused window belongs to `app` iff its owning process `comm` equals `app` exactly.
fn focused_belongs_to(focused_comm: Option<&str>, app: &str) -> bool {
    focused_comm == Some(app)
}

// ---- thin x11rb helpers -------------------------------------------------------------

/// Intern an atom only if it already exists (a WM published it); `None`/0 otherwise.
fn intern(conn: &RustConnection, name: &str) -> Option<u32> {
    let atom = conn.intern_atom(true, name.as_bytes()).ok()?.reply().ok()?.atom;
    (atom != 0).then_some(atom)
}

fn atom_exists(conn: &RustConnection, name: &[u8]) -> bool {
    conn.intern_atom(true, name)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.atom != 0)
        .unwrap_or(false)
}

/// The focused toplevel window id (`_NET_ACTIVE_WINDOW` on the root), or `None` / filtered to
/// non-zero.
fn net_active_window(conn: &RustConnection, root: u32) -> Option<u32> {
    let atom = intern(conn, "_NET_ACTIVE_WINDOW")?;
    conn.get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()
        .filter(|&w| w != 0)
}

/// The owning process id of a window (`_NET_WM_PID`), if advertised.
fn net_wm_pid(conn: &RustConnection, win: u32) -> Option<u32> {
    let atom = intern(conn, "_NET_WM_PID")?;
    conn.get_property(false, win, atom, AtomEnum::CARDINAL, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()
        .filter(|&pid| pid != 0)
}

fn proc_comm(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focused_window_of_target_matches() {
        assert!(focused_belongs_to(Some("firefox"), "firefox"));
    }

    #[test]
    fn focused_window_of_other_app_does_not_match() {
        // Focus moved to a different app between the engine's decision and our read: must
        // NOT capture it (fail-closed → None → blank → input gated off).
        assert!(!focused_belongs_to(Some("code"), "firefox"));
    }

    #[test]
    fn unidentifiable_focused_window_does_not_match() {
        // No _NET_WM_PID ⇒ no comm ⇒ can't prove ownership ⇒ don't capture.
        assert!(!focused_belongs_to(None, "firefox"));
    }

    #[test]
    fn match_is_exact_not_substring() {
        assert!(!focused_belongs_to(Some("firefox-bin"), "firefox"));
        assert!(!focused_belongs_to(Some("fire"), "firefox"));
    }
}
