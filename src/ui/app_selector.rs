//! Standalone app selection panel — shown from the tray "Settings" menu.

use anyhow::Result;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use tracing::info;

/// Result of showing the app selection panel.
pub struct AppSelectionResult {
    pub saved: bool,
    pub capture_all: bool,
    pub selected_apps: Vec<String>,
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct FfiAppSelectionResult {
    capture_all: bool,
    selected_apps: *const *const c_char,
    selected_apps_count: usize,
    saved: bool,
}

#[cfg(target_os = "macos")]
extern "C" {
    fn show_app_selection_panel(
        current_apps: *const *const c_char,
        current_count: usize,
        capture_all: bool,
        result: *mut FfiAppSelectionResult,
    );
    fn app_selection_free_result(result: *mut FfiAppSelectionResult);
}

/// Show the app selection panel. Blocks until the user closes it.
/// Returns the new selection, or None if cancelled.
#[cfg(target_os = "macos")]
pub fn show_panel(
    current_apps: &[String],
    capture_all: bool,
) -> Result<AppSelectionResult> {
    let c_apps: Vec<CString> = current_apps
        .iter()
        .filter_map(|s| CString::new(s.as_str()).ok())
        .collect();
    let c_ptrs: Vec<*const c_char> = c_apps.iter().map(|s| s.as_ptr()).collect();

    let mut result = FfiAppSelectionResult {
        capture_all,
        selected_apps: std::ptr::null(),
        selected_apps_count: 0,
        saved: false,
    };

    unsafe {
        show_app_selection_panel(
            c_ptrs.as_ptr(),
            c_ptrs.len(),
            capture_all,
            &mut result,
        );
    }

    if !result.saved {
        return Ok(AppSelectionResult {
            saved: false,
            capture_all,
            selected_apps: current_apps.to_vec(),
        });
    }

    let mut selected = Vec::new();
    if !result.selected_apps.is_null() {
        for i in 0..result.selected_apps_count {
            let ptr = unsafe { *result.selected_apps.add(i) };
            if !ptr.is_null() {
                if let Ok(s) = unsafe { CStr::from_ptr(ptr) }.to_str() {
                    selected.push(s.to_string());
                }
            }
        }
    }

    let capture_all = result.capture_all;
    unsafe { app_selection_free_result(&mut result) };

    info!(
        "App selection saved: capture_all={}, apps={:?}",
        capture_all, selected
    );

    Ok(AppSelectionResult {
        saved: true,
        capture_all,
        selected_apps: selected,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn show_panel(
    current_apps: &[String],
    capture_all: bool,
) -> Result<AppSelectionResult> {
    Ok(AppSelectionResult {
        saved: false,
        capture_all,
        selected_apps: current_apps.to_vec(),
    })
}
