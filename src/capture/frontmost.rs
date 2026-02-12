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
    // Try X11 first, then fall back to reading /proc for Wayland
    if let Some(app) = get_frontmost_app_x11() {
        return Some(app);
    }

    // On Wayland, we can't reliably get the focused window from outside
    // Return None and let the sync engine handle this (capture all or use manual mode)
    None
}

#[cfg(target_os = "linux")]
fn get_frontmost_app_x11() -> Option<AppInfo> {
    use std::process::Command;

    // Use xdotool to get the active window
    let output = Command::new("xdotool")
        .args(["getactivewindow", "getwindowpid"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let pid_str = String::from_utf8_lossy(&output.stdout);
    let pid: u32 = pid_str.trim().parse().ok()?;

    // Get the process name from /proc
    let comm_path = format!("/proc/{}/comm", pid);
    let name = std::fs::read_to_string(&comm_path).ok()?.trim().to_string();

    // Get the command line for a more complete name
    let cmdline_path = format!("/proc/{}/cmdline", pid);
    let cmdline = std::fs::read_to_string(&cmdline_path)
        .ok()
        .and_then(|s| s.split('\0').next().map(|s| s.to_string()))
        .unwrap_or_else(|| name.clone());

    // Use the executable name as bundle_id equivalent
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
