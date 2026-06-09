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
fn get_frontmost_app_linux() -> Option<AppInfo> {
    // Try the X11/XWayland path first (reads EWMH focus properties natively).
    if let Some(app) = get_frontmost_app_x11() {
        return Some(app);
    }

    // No reachable X server, or a Wayland-native window is focused (its handle never
    // appears in X11). Return None; the sync engine treats unknown focus conservatively
    // (capture only when capture_all is set). A Wayland-native focus signal (e.g.
    // wlr-foreign-toplevel on wlroots, or AT-SPI) is a separate, compositor-specific path.
    None
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
    let net_wm_pid = conn.intern_atom(true, b"_NET_WM_PID").ok()?.reply().ok()?.atom;
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

    // Resolve a display name and an executable-basename identity from /proc — the same
    // identity key produced by app enumeration in apps.rs, so target-app matching agrees.
    let comm_path = format!("/proc/{}/comm", pid);
    let name = std::fs::read_to_string(&comm_path).ok()?.trim().to_string();

    let cmdline_path = format!("/proc/{}/cmdline", pid);
    let cmdline = std::fs::read_to_string(&cmdline_path)
        .ok()
        .and_then(|s| s.split('\0').next().map(|s| s.to_string()))
        .unwrap_or_else(|| name.clone());

    let bundle_id = std::path::Path::new(&cmdline)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&name)
        .to_string();

    Some(AppInfo {
        bundle_id,
        name,
        pid,
    })
}

// ============================================================================
// Windows Implementation
// ============================================================================

#[cfg(target_os = "windows")]
fn get_frontmost_app_windows() -> Option<AppInfo> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> *mut std::ffi::c_void;
        fn GetWindowThreadProcessId(hwnd: *mut std::ffi::c_void, process_id: *mut u32) -> u32;
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

        Some(AppInfo {
            bundle_id: name.clone(),
            name,
            pid,
        })
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
