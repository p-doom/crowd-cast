//! Running application enumeration
//!
//! Lists running GUI applications for the setup wizard to let users
//! select which applications to capture.

use super::AppInfo;

/// List all running GUI applications
pub fn list_running_apps() -> Vec<AppInfo> {
    #[cfg(target_os = "macos")]
    {
        list_running_apps_macos()
    }

    #[cfg(target_os = "linux")]
    {
        list_running_apps_linux()
    }

    #[cfg(target_os = "windows")]
    {
        list_running_apps_windows()
    }
}

/// List applications that can be captured (GUI apps with windows)
/// This filters out background processes and system services
pub fn list_capturable_apps() -> Vec<AppInfo> {
    let apps = list_running_apps();

    // Filter to only apps that are likely to have visible windows
    apps.into_iter()
        .filter(|app| !is_system_app(&app.bundle_id))
        .collect()
}

/// Check if an app is a system/background app that shouldn't be captured
fn is_system_app(bundle_id: &str) -> bool {
    // macOS system apps
    let system_prefixes = [
        "com.apple.loginwindow",
        "com.apple.dock",
        "com.apple.finder", // Often want to exclude Finder
        "com.apple.SystemUIServer",
        "com.apple.WindowServer",
        "com.apple.CoreServices",
        "com.apple.notificationcenterui",
        "com.apple.controlcenter",
        "com.apple.Spotlight",
    ];

    for prefix in &system_prefixes {
        if bundle_id.starts_with(prefix) {
            return true;
        }
    }

    false
}

// ============================================================================
// macOS Implementation
// ============================================================================

#[cfg(target_os = "macos")]
fn list_running_apps_macos() -> Vec<AppInfo> {
    use std::process::Command;

    // Use AppleScript to get list of running apps with bundle identifiers
    // The bundle identifier is required for ScreenCaptureKit application capture
    let script = r#"
        set appList to ""
        tell application "System Events"
            set allApps to every process whose background only is false
            repeat with anApp in allApps
                set appName to name of anApp
                set appPID to unix id of anApp
                set bundleID to bundle identifier of anApp
                if bundleID is not missing value then
                    set appList to appList & appName & "|||" & appPID & "|||" & bundleID & "\n"
                end if
            end repeat
        end tell
        return appList
    "#;

    let output = match Command::new("osascript").arg("-e").arg(script).output() {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    let mut apps = Vec::new();

    for line in output_str.lines() {
        let parts: Vec<&str> = line.split("|||").collect();
        if parts.len() >= 3 {
            let name = parts[0].trim().to_string();
            let pid: u32 = parts[1].trim().parse().unwrap_or(0);
            let bundle_id = parts[2].trim().to_string();

            // Skip apps without a valid bundle ID
            if bundle_id.is_empty() {
                continue;
            }

            apps.push(AppInfo {
                bundle_id,
                name,
                pid,
            });
        }
    }

    // Sort by name for consistent display
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

// ============================================================================
// Linux Implementation
// ============================================================================

#[cfg(target_os = "linux")]
fn list_running_apps_linux() -> Vec<AppInfo> {
    use std::collections::HashSet;
    use std::fs;
    use std::path::Path;

    let mut apps = Vec::new();
    let mut seen_names = HashSet::new();

    // Read /proc to find all processes
    let proc_dir = Path::new("/proc");
    if let Ok(entries) = fs::read_dir(proc_dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            // Check if this is a PID directory
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if let Ok(pid) = name.parse::<u32>() {
                    // Check if this process has a display (DISPLAY env or is X11 client)
                    let environ_path = path.join("environ");
                    if let Ok(environ) = fs::read_to_string(&environ_path) {
                        // Check for DISPLAY variable (indicates X11 app)
                        if !environ.contains("DISPLAY=") {
                            continue;
                        }
                    } else {
                        continue;
                    }

                    // Get process name
                    let comm_path = path.join("comm");
                    if let Ok(comm) = fs::read_to_string(&comm_path) {
                        let name = comm.trim().to_string();

                        // Skip if we've already seen this name
                        if seen_names.contains(&name) {
                            continue;
                        }
                        seen_names.insert(name.clone());

                        apps.push(AppInfo {
                            bundle_id: name.clone(),
                            name,
                            pid,
                        });
                    }
                }
            }
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

// ============================================================================
// Windows Implementation
// ============================================================================

#[cfg(target_os = "windows")]
fn list_running_apps_windows() -> Vec<AppInfo> {
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut std::ffi::c_void;
        fn Process32FirstW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }

    const TH32CS_SNAPPROCESS: u32 = 0x00000002;
    const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;

    let mut apps = Vec::new();
    let mut seen_names = HashSet::new();

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return apps;
        }

        let mut entry: ProcessEntry32W = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;

        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                // Find null terminator
                let len = entry
                    .sz_exe_file
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(260);

                let name = OsString::from_wide(&entry.sz_exe_file[..len])
                    .to_string_lossy()
                    .to_string();

                // Remove .exe extension
                let name = name.strip_suffix(".exe").unwrap_or(&name).to_string();

                if !seen_names.contains(&name) {
                    seen_names.insert(name.clone());
                    apps.push(AppInfo {
                        bundle_id: name.clone(),
                        name,
                        pid: entry.th32_process_id,
                    });
                }

                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }

        CloseHandle(snapshot);
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_running_apps() {
        let apps = list_running_apps();
        println!("Found {} running apps:", apps.len());
        for app in &apps {
            println!("  - {} ({})", app.name, app.bundle_id);
        }
        // Should find at least a few apps
        assert!(!apps.is_empty());
    }

    #[test]
    fn test_list_capturable_apps() {
        let apps = list_capturable_apps();
        println!("Found {} capturable apps:", apps.len());
        for app in &apps {
            println!("  - {} ({})", app.name, app.bundle_id);
        }
    }
}
