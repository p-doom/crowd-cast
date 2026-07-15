//! Frontmost application detection
//!
//! Provides cross-platform detection of which application is currently focused.
//! Used to filter input capture to only target applications.

use std::ffi::CStr;

/// Information about an application
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    /// Bundle identifier (macOS) or process name (Linux/Windows)
    pub bundle_id: String,
    /// Localized display name
    pub name: String,
    /// Process ID
    pub pid: u32,
}

/// Get information about the currently focused application
pub fn get_frontmost_app() -> Option<AppInfo> {
    #[cfg(target_os = "macos")]
    {
        get_frontmost_app_macos()
    }

    #[cfg(target_os = "linux")]
    {
        get_frontmost_app_linux()
    }

    #[cfg(target_os = "windows")]
    {
        get_frontmost_app_windows()
    }
}

// ============================================================================
// macOS Implementation
// ============================================================================

#[cfg(target_os = "macos")]
fn get_frontmost_app_macos() -> Option<AppInfo> {
    use std::ffi::c_void;
    use std::os::raw::c_char;

    // Objective-C runtime types
    type Id = *mut c_void;
    type Sel = *mut c_void;
    type Class = *mut c_void;

    #[link(name = "objc", kind = "dylib")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Class;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend(receiver: Id, selector: Sel, ...) -> Id;
    }

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    unsafe {
        // Get NSWorkspace class
        let ns_workspace_class = objc_getClass(b"NSWorkspace\0".as_ptr() as *const c_char);
        if ns_workspace_class.is_null() {
            return None;
        }

        // Get shared workspace: [NSWorkspace sharedWorkspace]
        let shared_workspace_sel = sel_registerName(b"sharedWorkspace\0".as_ptr() as *const c_char);
        let workspace: Id = objc_msgSend(ns_workspace_class, shared_workspace_sel);
        if workspace.is_null() {
            return None;
        }

        // Get frontmost application: [workspace frontmostApplication]
        let frontmost_app_sel =
            sel_registerName(b"frontmostApplication\0".as_ptr() as *const c_char);
        let app: Id = objc_msgSend(workspace, frontmost_app_sel);
        if app.is_null() {
            return None;
        }

        // Get bundle identifier: [app bundleIdentifier]
        let bundle_id_sel = sel_registerName(b"bundleIdentifier\0".as_ptr() as *const c_char);
        let bundle_id_nsstring: Id = objc_msgSend(app, bundle_id_sel);
        let bundle_id = nsstring_to_string(bundle_id_nsstring)?;

        // Get localized name: [app localizedName]
        let localized_name_sel = sel_registerName(b"localizedName\0".as_ptr() as *const c_char);
        let name_nsstring: Id = objc_msgSend(app, localized_name_sel);
        let name = nsstring_to_string(name_nsstring).unwrap_or_else(|| bundle_id.clone());

        // Get process identifier: [app processIdentifier]
        // processIdentifier returns pid_t (i32) but objc_msgSend returns Id
        // We need to call a version that returns i32
        #[link(name = "objc", kind = "dylib")]
        extern "C" {
            #[link_name = "objc_msgSend"]
            fn objc_msgSend_i32(receiver: Id, selector: Sel, ...) -> i32;
        }

        let pid_sel = sel_registerName(b"processIdentifier\0".as_ptr() as *const c_char);
        let pid: i32 = objc_msgSend_i32(app, pid_sel);

        Some(AppInfo {
            bundle_id,
            name,
            pid: pid as u32,
        })
    }
}

#[cfg(target_os = "macos")]
unsafe fn nsstring_to_string(nsstring: *mut std::ffi::c_void) -> Option<String> {
    use std::ffi::c_void;
    use std::os::raw::c_char;

    type Id = *mut c_void;
    type Sel = *mut c_void;

    extern "C" {
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend(receiver: Id, selector: Sel, ...) -> Id;
    }

    if nsstring.is_null() {
        return None;
    }

    // Get UTF8 string: [nsstring UTF8String]
    let utf8_sel = sel_registerName(b"UTF8String\0".as_ptr() as *const c_char);
    let utf8_ptr: *const c_char = objc_msgSend(nsstring, utf8_sel) as *const c_char;

    if utf8_ptr.is_null() {
        return None;
    }

    CStr::from_ptr(utf8_ptr)
        .to_str()
        .ok()
        .map(|s| s.to_string())
}

// ============================================================================
// Linux Implementation
// ============================================================================

#[cfg(target_os = "linux")]
fn is_wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|s| s.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn get_frontmost_app_linux() -> Option<AppInfo> {
    // Wayland: the GNOME focus extension maintains the focused app on its own thread. We
    // deliberately do NOT use the X11 path here — under XWayland it would only ever see
    // XWayland windows. (wlroots has no focus provider: it records full-screen, so the
    // snapshot is empty there, which is correct for its capture-all mode.)
    if is_wayland_session() {
        crate::capture::focus::ensure_started();
        // Canonical Wayland identity is the focus provider's `wm_class` (GNOME) — the SAME
        // key the wizard stores in `target_apps` (resolved from each app's
        // `.desktop` `StartupWMClass`/id; see `apps.rs::list_installed_apps_wayland`), and what
        // `should_capture_app` compares against to gate input.
        //
        // We deliberately do NOT translate via the PID to `/proc/<pid>/comm`. `Meta.Window`
        // `get_pid()` is unreliable (XWayland windows that omit `_NET_WM_PID` report no PID), and
        // even when present, comm is a *different namespace* than the app_id the wizard stores —
        // e.g. comm `evince` vs wm_class `org.gnome.Evince`, or `alacritty` vs `Alacritty`. Any
        // such translation can silently disagree with `target_apps` and drop a focused target's
        // capture, which is exactly the silent-incorrectness the no-fallback design law forbids.
        // An empty app_id means the provider cannot identify the window → fail closed (`None`,
        // treated as non-target), never guess.
        return crate::capture::focus::snapshot().and_then(|f| {
            let identity = f.app_id.trim().to_string();
            if identity.is_empty() {
                return None;
            }
            Some(AppInfo {
                bundle_id: identity.clone(),
                name: identity,
                pid: f.pid.unwrap_or(0),
            })
        });
    }

    // Pure X11 session: native EWMH query (_NET_ACTIVE_WINDOW -> _NET_WM_PID).
    get_frontmost_app_x11()
}

/// Resolve the focused application on X11 / XWayland by reading EWMH properties
/// (`_NET_ACTIVE_WINDOW` -> `_NET_WM_PID`) directly over the X11 socket via x11rb's
/// pure-Rust connection. This replaces shelling out to the `xdotool` binary, so the agent
/// no longer requires `xdotool` (or any X client library) to be installed.
///
/// Returns `None` when there is no reachable X server, no active X11 window (e.g. a
/// Wayland-native window is focused — its handle never appears here), or the focused
/// window advertises no `_NET_WM_PID` (some clients omit it; `xdotool getwindowpid`
/// relied on the same hint, so this is at parity).
#[cfg(target_os = "linux")]
fn get_frontmost_app_x11() -> Option<AppInfo> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

    // RustConnection speaks the X11 wire protocol over the socket — no libX11/libxcb.
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;

    // only_if_exists=true: if no WM ever published these atoms, there is nothing to read.
    let net_active_window = conn
        .intern_atom(true, b"_NET_ACTIVE_WINDOW")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let net_wm_pid = conn
        .intern_atom(true, b"_NET_WM_PID")
        .ok()?
        .reply()
        .ok()?
        .atom;
    if net_active_window == 0 || net_wm_pid == 0 {
        return None;
    }

    // _NET_ACTIVE_WINDOW (a WINDOW on the root) identifies the focused toplevel.
    let active_window = conn
        .get_property(false, root, net_active_window, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()
        .and_then(|mut it| it.next())
        .filter(|&w| w != 0)?;

    // _NET_WM_PID (a CARDINAL on the window) gives the owning process.
    let pid = conn
        .get_property(false, active_window, net_wm_pid, AtomEnum::CARDINAL, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()
        .and_then(|mut it| it.next())?;

    // Identity key: `/proc/<pid>/comm` — the SAME key app enumeration (apps.rs) produces and
    // the wizard persists in `target_apps`, so `should_capture_app` matching agrees on X11.
    // (Both sides are kernel-truncated to 15 chars identically; the XComposite window
    // resolver additionally matches on the exe basename and WM_CLASS, see x11_windows.)
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .ok()?
        .trim()
        .to_string();
    if comm.is_empty() {
        return None;
    }

    Some(AppInfo {
        bundle_id: comm.clone(),
        name: comm,
        pid,
    })
}

// ============================================================================
// Windows Implementation
// ============================================================================

/// Last app reported for a foreground window not owned by this process.
///
/// Clicking the crowd-cast tray icon or opening its menu makes THIS process the
/// foreground window on Windows (unlike macOS status items and Linux SNI menus,
/// which don't take focus). Reporting ourselves reads downstream as the user
/// leaving their tracked app: the status flips to "no capture sources" and the
/// video blanks, so the act of checking the status corrupts the status
/// (issue #118). Remembering the previous app lets the provider report
/// "no change" instead. A Mutex rather than a thread_local because the engine
/// poll and the startup/context paths call in from different threads and must
/// share this memory.
#[cfg(target_os = "windows")]
static LAST_NON_SELF: std::sync::Mutex<Option<AppInfo>> = std::sync::Mutex::new(None);

/// Window classes of the shell's taskbar / tray surfaces. Focusing these is
/// part of interacting with the tray, not leaving the tracked app: on Windows
/// 11 a NEW tray icon lives in the hidden-icons overflow by default, and
/// opening that flyout foregrounds explorer's
/// `TopLevelWindowForOverflowXamlIsland` before our icon can even be clicked.
/// That flipped the status to "no capture sources" ahead of the menu, so the
/// self-masking alone still showed the issue #118 symptom whenever the icon
/// sat in the overflow. Dismissing the flyout lands focus on `Shell_TrayWnd`
/// (measured), `NotifyIconOverflowWindow` is the Windows 10 overflow, and
/// `Shell_SecondaryTrayWnd` is the taskbar on secondary monitors.
#[cfg(target_os = "windows")]
const TRAY_SHELL_CLASSES: [&str; 4] = [
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
];

/// Traits of the current foreground window that decide masking, resolved
/// alongside the owning app.
#[cfg(target_os = "windows")]
struct ForegroundTraits {
    /// IsWindowVisible: separates our hidden tray event window from our real
    /// windows (Settings panel, wizard).
    visible: bool,
    /// The window is one of the shell's taskbar / tray-overflow surfaces
    /// (TRAY_SHELL_CLASSES).
    tray_shell: bool,
}

#[cfg(target_os = "windows")]
fn get_frontmost_app_windows() -> Option<AppInfo> {
    let (current, traits) = match resolve_foreground_app() {
        Some((app, traits)) => (Some(app), traits),
        None => (
            None,
            ForegroundTraits {
                visible: false,
                tray_shell: false,
            },
        ),
    };
    let mut last = LAST_NON_SELF
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    filter_self(current, std::process::id(), &traits, &mut last)
}

/// Mask tray interactions as "no change" so status, capture target, and keylog
/// context hold steady, while the agent's real windows still report truthfully.
///
/// Three foreground cases are masked to the remembered last non-tray app:
///
/// - Our PID + an INVISIBLE window: the user is on our tray icon or its menu.
///   tray-icon creates its event window without WS_VISIBLE and muda calls
///   SetForegroundWindow on exactly that hidden window to display the menu
///   (the standard TrackPopupMenu pattern).
/// - The shell's taskbar / tray-overflow surfaces (TRAY_SHELL_CLASSES): these
///   belong to explorer, and reaching an overflow-hidden tray icon (the
///   Windows 11 default placement for new icons) foregrounds them before and
///   after our menu. Without masking them, checking the status still flips the
///   status whenever the icon is in the overflow.
///
/// Not masked:
///
/// - Our PID + a VISIBLE window (Settings panel, setup wizard, update dialog):
///   report ourselves. These can hold focus for minutes, and the config-level
///   self-exclusion deliberately keeps the agent's own UI out of the dataset
///   (capture off, UNCAPTURED context). Masking here would silently attribute
///   minutes of our own UI's input to the previous app.
///
/// Details:
///
/// - Self is matched on PID, not exe name: an exe-name match would also swallow
///   other crowd-cast processes (e.g. a second instance) and is spoofable by
///   any unrelated binary with the same name.
/// - Reports `None` when masked before any other app was ever resolved (tray
///   clicked right after launch): the engine already treats a `None` frontmost
///   as unknown, same as a failed resolution today.
/// - A failed resolution (`current == None`) keeps the memory: a transient
///   provider failure shouldn't erase what we knew.
/// - Accepted trade-off: input made while the menu / overflow is open is
///   attributed to the previous app. That is a few seconds of tray clicks;
///   attributing it to crowd-cast or explorer is what caused the false flip.
#[cfg(target_os = "windows")]
fn filter_self(
    current: Option<AppInfo>,
    own_pid: u32,
    traits: &ForegroundTraits,
    last: &mut Option<AppInfo>,
) -> Option<AppInfo> {
    match current {
        Some(app) if app.pid == own_pid && !traits.visible => last.clone(),
        // A visible window of ours: report self; deliberately do NOT store it in
        // `last`, which must only ever hold non-self, non-tray apps.
        Some(app) if app.pid == own_pid => Some(app),
        // Taskbar / tray-overflow surface: transient tray interaction, hold the
        // previous app (and keep it out of `last`).
        Some(_) if traits.tray_shell => last.clone(),
        Some(app) => {
            *last = Some(app.clone());
            Some(app)
        }
        None => None,
    }
}

/// Resolve the foreground window's owning application (this process included)
/// plus the traits `filter_self` masks on: visibility (our hidden tray/menu
/// window vs. our real windows) and whether the window is one of the shell's
/// tray surfaces.
#[cfg(target_os = "windows")]
fn resolve_foreground_app() -> Option<(AppInfo, ForegroundTraits)> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> *mut std::ffi::c_void;
        fn GetWindowThreadProcessId(hwnd: *mut std::ffi::c_void, process_id: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: *mut std::ffi::c_void) -> i32;
        fn GetClassNameW(hwnd: *mut std::ffi::c_void, buffer: *mut u16, max_count: i32) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn QueryFullProcessImageNameW(
            process: *mut std::ffi::c_void,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }

        let visible = IsWindowVisible(hwnd) != 0;

        let mut class_buf = [0u16; 128];
        let class_len = GetClassNameW(hwnd, class_buf.as_mut_ptr(), class_buf.len() as i32);
        let tray_shell = class_len > 0 && {
            let class = String::from_utf16_lossy(&class_buf[..class_len as usize]);
            TRAY_SHELL_CLASSES.iter().any(|c| class == *c)
        };

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return None;
        }

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }

        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;

        let result = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(process);

        if result == 0 {
            return None;
        }

        let path = OsString::from_wide(&buffer[..size as usize]);
        let path_str = path.to_string_lossy();

        let name = std::path::Path::new(path_str.as_ref())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string();

        // Use a lowercase executable stem as the bundle_id so app matching is
        // case-insensitive: Windows reports the on-disk case (e.g. "Notepad")
        // which users can't reliably predict when configuring target_apps.
        // `name` keeps the original case for display.
        Some((
            AppInfo {
                bundle_id: name.to_ascii_lowercase(),
                name,
                pid,
            },
            ForegroundTraits {
                visible,
                tray_shell,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_frontmost_app() {
        // This test will only pass when run interactively
        if let Some(app) = get_frontmost_app() {
            println!("Frontmost app: {:?}", app);
            assert!(!app.bundle_id.is_empty());
            assert!(!app.name.is_empty());
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod filter_self_tests {
    use super::*;

    const OWN_PID: u32 = 4242;
    /// Our hidden tray event window (tray icon / menu focus).
    const OWN_HIDDEN: ForegroundTraits = ForegroundTraits {
        visible: false,
        tray_shell: false,
    };
    /// A normal visible window (ours or another app's).
    const PLAIN_VISIBLE: ForegroundTraits = ForegroundTraits {
        visible: true,
        tray_shell: false,
    };
    /// The shell's taskbar / tray-overflow surface (explorer's, visible).
    const TRAY_SHELL: ForegroundTraits = ForegroundTraits {
        visible: true,
        tray_shell: true,
    };

    fn app(stem: &str, pid: u32) -> AppInfo {
        AppInfo {
            bundle_id: stem.to_ascii_lowercase(),
            name: stem.to_string(),
            pid,
        }
    }

    #[test]
    fn non_self_app_passes_through_and_is_remembered() {
        let mut last = None;
        let firefox = app("firefox", 100);
        assert_eq!(
            filter_self(Some(firefox.clone()), OWN_PID, &PLAIN_VISIBLE, &mut last),
            Some(firefox.clone())
        );
        assert_eq!(last, Some(firefox));
    }

    #[test]
    fn self_hidden_window_reports_the_previous_app() {
        // The tray/menu case: our hidden event window has focus.
        let firefox = app("firefox", 100);
        let mut last = Some(firefox.clone());
        let ours = app("crowd-cast-agent", OWN_PID);
        assert_eq!(
            filter_self(Some(ours), OWN_PID, &OWN_HIDDEN, &mut last),
            Some(firefox.clone())
        );
        // The memory must survive the masked read so a held-open menu keeps
        // reporting the same app across many polls.
        assert_eq!(last, Some(firefox));
    }

    #[test]
    fn self_visible_window_reports_self_and_keeps_memory() {
        // The Settings panel / wizard case: a real window of ours has focus.
        // Reporting self lets the config-level self-exclusion keep our own UI's
        // input out of the dataset instead of misattributing it.
        let firefox = app("firefox", 100);
        let mut last = Some(firefox.clone());
        let ours = app("crowd-cast-agent", OWN_PID);
        assert_eq!(
            filter_self(Some(ours.clone()), OWN_PID, &PLAIN_VISIBLE, &mut last),
            Some(ours)
        );
        // `last` must never hold a self entry.
        assert_eq!(last, Some(firefox));
    }

    #[test]
    fn tray_shell_surface_reports_the_previous_app() {
        // The overflow flyout / taskbar case: explorer's tray surface has
        // focus (the default route to a fresh install's tray icon).
        let firefox = app("firefox", 100);
        let mut last = Some(firefox.clone());
        let shell = app("explorer", 616);
        assert_eq!(
            filter_self(Some(shell), OWN_PID, &TRAY_SHELL, &mut last),
            Some(firefox.clone())
        );
        // explorer's tray surface must not overwrite the remembered app.
        assert_eq!(last, Some(firefox));
    }

    #[test]
    fn tray_shell_before_any_app_reports_none() {
        let mut last = None;
        let shell = app("explorer", 616);
        assert_eq!(filter_self(Some(shell), OWN_PID, &TRAY_SHELL, &mut last), None);
        assert_eq!(last, None);
    }

    #[test]
    fn explorer_file_window_is_not_masked() {
        // A real File Explorer window (class CabinetWClass, not a tray
        // surface) is a normal untracked app and must pass through.
        let mut last = Some(app("firefox", 100));
        let explorer = app("explorer", 616);
        assert_eq!(
            filter_self(Some(explorer.clone()), OWN_PID, &PLAIN_VISIBLE, &mut last),
            Some(explorer.clone())
        );
        assert_eq!(last, Some(explorer));
    }

    #[test]
    fn self_hidden_window_before_any_app_reports_none() {
        let mut last = None;
        let ours = app("crowd-cast-agent", OWN_PID);
        assert_eq!(filter_self(Some(ours), OWN_PID, &OWN_HIDDEN, &mut last), None);
        assert_eq!(last, None);
    }

    #[test]
    fn failed_resolution_reports_none_but_keeps_memory() {
        let firefox = app("firefox", 100);
        let mut last = Some(firefox.clone());
        assert_eq!(filter_self(None, OWN_PID, &OWN_HIDDEN, &mut last), None);
        assert_eq!(last, Some(firefox));
    }

    #[test]
    fn other_process_with_our_exe_name_is_not_masked() {
        // Identity is the PID: a second crowd-cast instance (or an unrelated
        // binary named like our exe) must be reported, not swallowed.
        let mut last = Some(app("firefox", 100));
        let other = app("crowd-cast-agent", 9999);
        assert_eq!(
            filter_self(Some(other.clone()), OWN_PID, &PLAIN_VISIBLE, &mut last),
            Some(other.clone())
        );
        assert_eq!(last, Some(other));
    }
}
