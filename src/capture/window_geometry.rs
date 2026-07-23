//! Windows monitor / window geometry for the monitor-level capture fit.
//!
//! Two jobs:
//! 1. `capture_canvas_size`: the recording canvas = the bounding box of every
//!    monitor, each normalized so its shortest edge is 1080px (FHD x1.0,
//!    4K x0.5, ultrawide 3440x1440 x0.75). Square only when both a wide-landscape
//!    and a tall-portrait monitor are present.
//! 2. `monitor_fit_for_app`: for a captured app, the scale + top-left position to
//!    draw its window at, so it sits at its real on-monitor location, scaled by
//!    its monitor's normalization factor. A half-monitor window therefore appears
//!    as half the frame, where it actually is; the rest stays black.
//!
//! All coordinates are physical pixels: the process is Per-Monitor-V2 DPI-aware
//! (set in `main`), so `GetWindowRect` / `GetMonitorInfo` and the WGC capture
//! textures are all in the same physical-pixel space.

use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Rect {
    const ZERO: Rect = Rect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
}

#[repr(C)]
#[allow(dead_code)] // cb_size/rc_work/dw_flags are required for the FFI layout
struct MonitorInfo {
    cb_size: u32,
    rc_monitor: Rect,
    rc_work: Rect,
    dw_flags: u32,
}

#[link(name = "user32")]
extern "system" {
    fn EnumDisplayMonitors(
        hdc: *mut c_void,
        clip: *const Rect,
        callback: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut Rect, isize) -> i32,
        data: isize,
    ) -> i32;
    fn EnumWindows(
        callback: unsafe extern "system" fn(*mut c_void, isize) -> i32,
        data: isize,
    ) -> i32;
    fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
    fn GetWindowRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
    fn MonitorFromWindow(hwnd: *mut c_void, flags: u32) -> *mut c_void;
    fn GetMonitorInfoW(hmon: *mut c_void, info: *mut MonitorInfo) -> i32;
    fn IsWindowVisible(hwnd: *mut c_void) -> i32;
    fn IsIconic(hwnd: *mut c_void) -> i32;
    fn GetWindowTextLengthW(hwnd: *mut c_void) -> i32;
    fn GetForegroundWindow() -> *mut c_void;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn QueryFullProcessImageNameW(
        process: *mut c_void,
        flags: u32,
        name: *mut u16,
        size: *mut u32,
    ) -> i32;
}

const MONITOR_DEFAULTTONEAREST: u32 = 2;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
/// Normalize every monitor/window so its shortest edge maps to this many pixels.
const TARGET_SHORT_EDGE: f64 = 1080.0;

unsafe extern "system" fn collect_monitor(
    _hmon: *mut c_void,
    _hdc: *mut c_void,
    rc: *mut Rect,
    data: isize,
) -> i32 {
    if !rc.is_null() && data != 0 {
        let rects = &mut *(data as *mut Vec<Rect>);
        rects.push(*rc);
    }
    1 // TRUE: keep enumerating
}

unsafe extern "system" fn collect_window(hwnd: *mut c_void, data: isize) -> i32 {
    if data == 0 {
        return 1;
    }
    // Top-level, visible, not minimized, and titled: skips tool/helper windows so
    // we pick the app's real window (roughly matching what OBS's matcher picks).
    if IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 && GetWindowTextLengthW(hwnd) > 0 {
        let wins = &mut *(data as *mut Vec<*mut c_void>);
        wins.push(hwnd);
    }
    1
}

/// Every monitor's rectangle (virtual-screen, physical pixels).
fn all_monitor_rects() -> Vec<Rect> {
    let mut rects: Vec<Rect> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            collect_monitor,
            &mut rects as *mut Vec<Rect> as isize,
        );
    }
    rects
}

/// Stable signature of the current monitor layout (sorted per-monitor rectangles).
/// Used to detect when monitors are added/removed/moved/resized (e.g. plugging in
/// an ultrawide) so the canvas can be recomputed. Empty if enumeration fails.
pub fn monitor_signature() -> Vec<(i32, i32, i32, i32)> {
    let mut sig: Vec<(i32, i32, i32, i32)> = all_monitor_rects()
        .into_iter()
        .map(|r| (r.left, r.top, r.right, r.bottom))
        .collect();
    sig.sort_unstable();
    sig
}

/// Recording canvas size: bounding box of all monitors, each normalized so its
/// shortest edge is `TARGET_SHORT_EDGE`. Returns `None` if no monitors are found.
pub fn capture_canvas_size() -> Option<(u32, u32)> {
    let rects = all_monitor_rects();

    let mut max_w = 0u32;
    let mut max_h = 0u32;
    for r in &rects {
        let w = (r.right - r.left).max(0) as f64;
        let h = (r.bottom - r.top).max(0) as f64;
        let short = w.min(h);
        if short <= 0.0 {
            continue;
        }
        let scale = TARGET_SHORT_EDGE / short;
        max_w = max_w.max((w * scale).round() as u32);
        max_h = max_h.max((h * scale).round() as u32);
    }

    if max_w == 0 || max_h == 0 {
        return None;
    }
    // Even dimensions for the video encoder.
    Some((max_w & !1, max_h & !1))
}

/// Scale + top-left position (in canvas pixels) at which to draw `bundle_id`'s
/// window. `None` if no matching window is found right now.
#[derive(Clone, Copy, PartialEq)]
pub struct MonitorFit {
    pub scale: f32,
    pub pos_x: f32,
    pub pos_y: f32,
}

pub fn monitor_fit_for_app(bundle_id: &str) -> Option<MonitorFit> {
    let mut wins: Vec<*mut c_void> = Vec::new();
    unsafe {
        EnumWindows(collect_window, &mut wins as *mut Vec<*mut c_void> as isize);
    }

    // First match in Z-order (topmost) for this executable, mirroring OBS's
    // executable-priority matching.
    let hwnd = wins.into_iter().find(|&h| window_exe_matches(h, bundle_id))?;

    unsafe {
        let mut win = Rect::ZERO;
        if GetWindowRect(hwnd, &mut win) == 0 {
            return None;
        }
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if hmon.is_null() {
            return None;
        }
        let mut mi = MonitorInfo {
            cb_size: std::mem::size_of::<MonitorInfo>() as u32,
            rc_monitor: Rect::ZERO,
            rc_work: Rect::ZERO,
            dw_flags: 0,
        };
        if GetMonitorInfoW(hmon, &mut mi) == 0 {
            return None;
        }
        let m = mi.rc_monitor;
        let short = (m.right - m.left).min(m.bottom - m.top).max(1) as f32;
        let scale = TARGET_SHORT_EDGE as f32 / short;
        Some(MonitorFit {
            scale,
            pos_x: (win.left - m.left) as f32 * scale,
            pos_y: (win.top - m.top) as f32 * scale,
        })
    }
}

/// The current foreground window's HWND (as `isize`), but only if it belongs to `bundle_id`
/// (case-insensitive executable file-stem match, the same rule as `window_exe_matches` and
/// frontmost app resolution) AND is a real capturable window: visible, not minimized, and
/// titled (the same filter `collect_window` applies when building the monitor-fit candidate
/// list, which roughly matches OBS's own `window_capture` enumeration). Returns `None` for
/// every other case: no foreground window, a window of a different app, or a non-real window.
///
/// This is design decision 3(b)'s raw `GetForegroundWindow` query. It deliberately does NOT
/// reuse `frontmost::get_frontmost_app` (that provider is self-masked and goes deliberately
/// stale during our own tray / settings interactions to protect input-capture gating, so it
/// would miss legitimate focus changes here), and it deliberately does NOT consult OBS's own
/// window enumeration (that is the caller's job, via `sources::select_window_by_handle`, which
/// keeps this function's dependencies to raw Win32 only). The file-stem match against the active
/// app's exe is what filters out transient self-focus, so no extra self-mask is layered on.
///
/// For a UWP-hosted app, `GetForegroundWindow` returns the `ApplicationFrameHost` parent HWND
/// while libobs' enumeration surfaces the content child HWND, so the caller's
/// `select_window_by_handle` lookup will simply miss and no-op for that (uncommon here) class
/// of app; that is an accepted gap, not a correctness bug, since the fallback is "keep the
/// current binding".
///
/// The pass/fail decision is factored into the pure `foreground_qualifies` gate so it can be
/// unit-tested without live Win32 handles (see `foreground_gate_tests`); this function only
/// reads the raw Win32 traits and feeds them to that gate.
pub(crate) fn foreground_window_of_app(bundle_id: &str) -> Option<isize> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let traits = ForegroundWindowTraits {
            visible: IsWindowVisible(hwnd) != 0,
            minimized: IsIconic(hwnd) != 0,
            titled: GetWindowTextLengthW(hwnd) > 0,
        };
        // Resolve the owning exe only when the cheap real-window checks already pass:
        // `window_exe_matches` does an OpenProcess round-trip and this runs on every ~100ms
        // poll. The pure gate re-checks `is_real_window`, so it stays the single authority on
        // the combined decision (design decisions 2a + 2b).
        let exe_matches = traits.is_real_window() && window_exe_matches(hwnd, bundle_id);
        if foreground_qualifies(traits, exe_matches) {
            Some(hwnd as isize)
        } else {
            None
        }
    }
}

/// Traits of the foreground window that decide whether follow-focus may track it, read from raw
/// Win32 by `foreground_window_of_app`. Split out from the decision (`foreground_qualifies`) so
/// the gate is unit-testable without live Win32 handles, exactly as `frontmost::ForegroundTraits`
/// is split from `frontmost::filter_self`.
#[derive(Clone, Copy)]
struct ForegroundWindowTraits {
    /// `IsWindowVisible`: WGC has no surface to capture for an invisible window.
    visible: bool,
    /// `IsIconic`: a minimized window has no live content.
    minimized: bool,
    /// `GetWindowTextLengthW > 0`: OBS's `window_capture` enumeration skips untitled windows.
    titled: bool,
}

impl ForegroundWindowTraits {
    /// The "real capturable window" half of the gate (design decision 2b): visible, not
    /// minimized, titled — the same filter `collect_window` / OBS's own enumeration apply.
    fn is_real_window(self) -> bool {
        self.visible && !self.minimized && self.titled
    }
}

/// Pure follow-focus gate (design decisions 2a + 2b): the foreground window qualifies as a
/// re-point target only when it is a real capturable window (`is_real_window`) AND belongs to
/// the active captured app (`exe_matches`, the caller's case-insensitive exe file-stem check).
/// Kept free of Win32 so the foreign-exe rejection is unit-testable — binding the active app's
/// source to another app's window is the foreground-misattribution class behind PR #119 / #120.
fn foreground_qualifies(traits: ForegroundWindowTraits, exe_matches: bool) -> bool {
    traits.is_real_window() && exe_matches
}

/// Whether `hwnd`'s owning process executable file-stem matches `bundle_id`
/// (case-insensitive), reusing the same exe-resolution as frontmost detection.
fn window_exe_matches(hwnd: *mut c_void, bundle_id: &str) -> bool {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return false;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(process);
        if ok == 0 || size == 0 {
            return false;
        }
        let path = OsString::from_wide(&buffer[..size as usize]);
        std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|stem| stem.eq_ignore_ascii_case(bundle_id))
            .unwrap_or(false)
    }
}

// The whole module is `#[cfg(target_os = "windows")]`, so `#[cfg(test)]` alone already scopes
// these to Windows test builds.
#[cfg(test)]
mod foreground_gate_tests {
    use super::{foreground_qualifies, ForegroundWindowTraits};

    /// A normal, capturable window: visible, not minimized, titled.
    const REAL: ForegroundWindowTraits = ForegroundWindowTraits {
        visible: true,
        minimized: false,
        titled: true,
    };

    #[test]
    fn real_window_of_the_active_app_qualifies() {
        // Real capturable window owned by the active app's exe: track it.
        assert!(foreground_qualifies(REAL, true));
    }

    #[test]
    fn foreign_exe_is_rejected_even_when_real() {
        // A perfectly real, visible, titled foreground window that belongs to a DIFFERENT exe
        // than the active captured app must NOT be a re-point target: pointing the active app's
        // source at another app's window is exactly the foreground misattribution that caused
        // PR #119 / #120. This is the clause with no coverage before this test.
        assert!(!foreground_qualifies(REAL, false));
    }

    #[test]
    fn invisible_window_is_rejected() {
        // Even the right app's window is skipped when there is no live surface to capture.
        let traits = ForegroundWindowTraits {
            visible: false,
            ..REAL
        };
        assert!(!foreground_qualifies(traits, true));
    }

    #[test]
    fn minimized_window_is_rejected() {
        let traits = ForegroundWindowTraits {
            minimized: true,
            ..REAL
        };
        assert!(!foreground_qualifies(traits, true));
    }

    #[test]
    fn untitled_window_is_rejected() {
        let traits = ForegroundWindowTraits {
            titled: false,
            ..REAL
        };
        assert!(!foreground_qualifies(traits, true));
    }
}
