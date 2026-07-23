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

/// Among an app's candidate windows (their pixel sizes, topmost-first in Z-order),
/// choose the index of the one the capture source is actually rendering.
///
/// The capture source reports the size of the window OBS locked onto, so when that
/// size is known we pick the candidate whose bounds match it most closely (ties
/// resolve to the topmost). This is what stops the fit from latching onto a
/// transient dialog or floating panel that is momentarily topmost in Z-order:
/// positioning the captured main-window content at a dialog's location is what
/// makes the window jump to the wrong spot for a few seconds. Without a source
/// hint, fall back to the largest window (the main document window dwarfs
/// dialogs/palettes). `None` only when `sizes` is empty.
fn select_capture_window(sizes: &[(u32, u32)], source_size: Option<(u32, u32)>) -> Option<usize> {
    match source_size {
        Some((sw, sh)) if sw > 0 && sh > 0 => (0..sizes.len()).min_by_key(|&i| {
            let (w, h) = sizes[i];
            (w as i64 - sw as i64).unsigned_abs() + (h as i64 - sh as i64).unsigned_abs()
        }),
        _ => (0..sizes.len())
            .min_by_key(|&i| std::cmp::Reverse(sizes[i].0 as u64 * sizes[i].1 as u64)),
    }
}

pub fn monitor_fit_for_app(bundle_id: &str, source_size: Option<(u32, u32)>) -> Option<MonitorFit> {
    let mut wins: Vec<*mut c_void> = Vec::new();
    unsafe {
        EnumWindows(collect_window, &mut wins as *mut Vec<*mut c_void> as isize);
    }

    // Every window this executable owns (topmost first) paired with its bounds.
    let candidates: Vec<(*mut c_void, Rect)> = wins
        .into_iter()
        .filter(|&h| window_exe_matches(h, bundle_id))
        .filter_map(|h| {
            let mut r = Rect::ZERO;
            // SAFETY: `h` is a live top-level window handle from EnumWindows.
            if unsafe { GetWindowRect(h, &mut r) } != 0 {
                Some((h, r))
            } else {
                None
            }
        })
        .collect();

    // Pick the window OBS is actually capturing, not merely the topmost one, so a
    // transient dialog/panel does not hijack the transform.
    let sizes: Vec<(u32, u32)> = candidates
        .iter()
        .map(|(_, r)| {
            (
                (r.right - r.left).max(0) as u32,
                (r.bottom - r.top).max(0) as u32,
            )
        })
        .collect();
    let (hwnd, win) = candidates[select_capture_window(&sizes, source_size)?];

    unsafe {
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

#[cfg(test)]
mod select_capture_window_tests {
    use super::select_capture_window;

    #[test]
    fn matches_source_size_over_topmost_dialog() {
        // Topmost is a dialog; the captured (main) window is second in Z-order.
        // Martin's SpaceClaim glitch: a dialog was topmost while OBS captured the
        // 1927x1047 main window, so the fit must select index 1, not 0.
        let sizes = [(400, 300), (1927, 1047), (200, 150)];
        assert_eq!(select_capture_window(&sizes, Some((1927, 1047))), Some(1));
    }

    #[test]
    fn tolerates_small_drift_between_source_and_bounds() {
        // Reported source size and GetWindowRect can differ by a few px (DWM frame).
        let sizes = [(500, 500), (1920, 1040)];
        assert_eq!(select_capture_window(&sizes, Some((1927, 1047))), Some(1));
    }

    #[test]
    fn size_match_ties_resolve_to_topmost() {
        let sizes = [(1000, 1000), (1000, 1000)];
        assert_eq!(select_capture_window(&sizes, Some((1000, 1000))), Some(0));
    }

    #[test]
    fn falls_back_to_largest_without_source_hint() {
        let sizes = [(400, 300), (1927, 1047), (800, 600)];
        assert_eq!(select_capture_window(&sizes, None), Some(1));
        // A zero source size is treated as unknown.
        assert_eq!(select_capture_window(&sizes, Some((0, 0))), Some(1));
    }

    #[test]
    fn largest_fallback_ties_resolve_to_topmost() {
        let sizes = [(1000, 1000), (1000, 1000), (500, 500)];
        assert_eq!(select_capture_window(&sizes, None), Some(0));
    }

    #[test]
    fn empty_candidates_return_none() {
        assert_eq!(select_capture_window(&[], Some((100, 100))), None);
        assert_eq!(select_capture_window(&[], None), None);
    }
}
