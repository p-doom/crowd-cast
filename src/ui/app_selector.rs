//! Standalone app selection panel — shown from the tray "Settings" menu.

#[cfg(any(target_os = "macos", target_os = "linux"))]
use anyhow::Context as _;
use anyhow::Result;
#[cfg(target_os = "macos")]
use std::ffi::{CStr, CString};
#[cfg(target_os = "macos")]
use std::os::raw::c_char;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use tracing::info;

/// Result of showing the app selection panel.
pub struct AppSelectionResult {
    pub saved: bool,
    pub capture_all: bool,
    pub selected_apps: Vec<String>,
}

// Matches the C `AppSelectionResult` implemented natively per platform
// (src/ui/wizard_darwin.m on macOS, src/ui/wizard_linux.c on Linux).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[repr(C)]
struct FfiAppSelectionResult {
    capture_all: bool,
    selected_apps: *const *const c_char,
    selected_apps_count: usize,
    saved: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
extern "C" {
    fn show_app_selection_panel(
        current_apps: *const *const c_char,
        current_count: usize,
        capture_all: bool,
        result: *mut FfiAppSelectionResult,
    );
    fn app_selection_free_result(result: *mut FfiAppSelectionResult);
}

/// Show the native (in-process) app-selection dialog. Blocks until the user closes it.
/// Returns the new selection, with `saved = false` if the user cancelled.
///
/// THREADING / LINUX SAFETY: this drives the platform GUI toolkit (Cocoa on macOS, GTK on
/// Linux) on the *current* thread. On Linux it is only safe to call from a process that has
/// NOT initialized libobs. libobs's Wayland support runs a `glib::MainLoop` on the default
/// `GMainContext` from a background thread (see libobs-rs `LinuxGlibLoop`); once GTK is
/// initialized here, GDK attaches its event source to that same default context, so the
/// libobs background thread and this thread both end up inside non-thread-safe GTK and
/// corrupt GDK's per-screen state (the `GDK_IS_SCREEN` warnings, then a NULL deref / SIGSEGV).
/// The agent process owns libobs, so it must reach the panel via [`show_panel`], which
/// re-execs a clean child that calls this. macOS has no such background loop, so its
/// [`show_panel`] calls this directly.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn show_panel_native(current_apps: &[String], capture_all: bool) -> Result<AppSelectionResult> {
    // Linux: the GTK dialog renders whatever candidate list we stage via the shared
    // wizard FFI (on macOS the Cocoa panel enumerates running apps itself). Stage the
    // current candidates and the per-app-capture availability gate before showing.
    #[cfg(target_os = "linux")]
    {
        use crate::installer::wizard_ffi::{self, AppInfoWrapper};
        let apps = crate::capture::list_capturable_apps();
        let wrappers: Vec<AppInfoWrapper> = apps
            .iter()
            .map(|a| AppInfoWrapper::new(&a.bundle_id, &a.name, a.pid))
            .collect();
        wizard_ffi::set_available_apps(&wrappers);
        wizard_ffi::set_per_app_available(
            crate::installer::requirements::per_app_capture_available(),
        );
    }

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
        show_app_selection_panel(c_ptrs.as_ptr(), c_ptrs.len(), capture_all, &mut result);
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

/// macOS: the Cocoa panel is safe to run in-process on the tray's main thread.
#[cfg(target_os = "macos")]
pub fn show_panel(current_apps: &[String], capture_all: bool) -> Result<AppSelectionResult> {
    show_panel_native(current_apps, capture_all)
}

/// Windows: reuse the native app-picker (the setup wizard's checklist) as a
/// standalone Settings panel. Blocks on its event loop until Save/Cancel.
#[cfg(target_os = "windows")]
pub fn show_panel(current_apps: &[String], capture_all: bool) -> Result<AppSelectionResult> {
    let r: crate::installer::AppPickerResult =
        crate::installer::run_settings_panel(current_apps, capture_all)?;
    Ok(AppSelectionResult {
        saved: r.saved,
        capture_all: r.capture_all,
        selected_apps: r.selected_apps,
    })
}

// ---- Linux: render the panel in a separate, libobs-free process -------------------------
//
// The agent process has libobs initialized, whose Wayland `glib::MainLoop` owns the default
// `GMainContext` on a background thread, so it can't show GTK itself (see the threading note
// on `show_panel_native`). Instead we re-exec ourselves with `--settings-panel-out <file>`:
// the child has no libobs, so GTK owns the default context, renders the dialog, and writes
// the result back as JSON. This mirrors how the setup wizard runs in its own process before
// libobs init (see `main.rs`).

#[cfg(target_os = "linux")]
#[derive(serde::Serialize, serde::Deserialize)]
struct PanelWire {
    saved: bool,
    capture_all: bool,
    selected_apps: Vec<String>,
}

#[cfg(target_os = "linux")]
pub fn show_panel(current_apps: &[String], capture_all: bool) -> Result<AppSelectionResult> {
    // The child re-reads the same on-disk config to render the current selection, so the
    // caller's values don't need threading through argv. Kept in the signature for parity
    // with macOS / the no-op stub, and so callers document intent.
    let _ = (current_apps, capture_all);

    let exe = std::env::current_exe().context("locating current executable")?;
    let out = std::env::temp_dir().join(format!("crowd-cast-settings-{}.json", std::process::id()));
    // Stale file from a previous (crashed) run would otherwise be read as this run's result.
    let _ = std::fs::remove_file(&out);

    let status = std::process::Command::new(&exe)
        .arg("--settings-panel-out")
        .arg(&out)
        .status()
        .context("spawning the settings-panel subprocess")?;

    if !status.success() {
        let _ = std::fs::remove_file(&out);
        // Fail closed: a crashed/failed dialog must not change the saved config.
        anyhow::bail!("settings-panel subprocess exited unsuccessfully: {status}");
    }

    let data = std::fs::read_to_string(&out)
        .context("settings-panel subprocess produced no result file")?;
    let _ = std::fs::remove_file(&out);

    let wire: PanelWire = serde_json::from_str(&data).context("parsing settings-panel result")?;
    Ok(AppSelectionResult {
        saved: wire.saved,
        capture_all: wire.capture_all,
        selected_apps: wire.selected_apps,
    })
}

/// Subprocess entry point for `--settings-panel-out <file>` (Linux). Runs in a clean process
/// that has NOT initialized libobs, so GTK has the default `GMainContext` to itself. Renders
/// the panel against the current on-disk config and writes the result as JSON to `out_path`
/// for the parent ([`show_panel`]) to read back.
#[cfg(target_os = "linux")]
pub fn run_settings_panel_subprocess(out_path: &std::path::Path) -> Result<()> {
    let config = crate::config::Config::load().context("loading config for settings panel")?;
    let result = show_panel_native(&config.capture.target_apps, config.capture.capture_all)?;
    let wire = PanelWire {
        saved: result.saved,
        capture_all: result.capture_all,
        selected_apps: result.selected_apps,
    };
    let json = serde_json::to_string(&wire).context("serializing settings-panel result")?;
    std::fs::write(out_path, json)
        .with_context(|| format!("writing settings-panel result to {out_path:?}"))?;
    Ok(())
}

// Platforms without a native settings panel: no-op.
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn show_panel(current_apps: &[String], capture_all: bool) -> Result<AppSelectionResult> {
    Ok(AppSelectionResult {
        saved: false,
        capture_all,
        selected_apps: current_apps.to_vec(),
    })
}
