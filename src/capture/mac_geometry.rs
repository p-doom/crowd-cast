//! macOS multi-monitor capture geometry (the macOS analogue of `window_geometry.rs`
//! on Windows and `monitor_layout.rs` on Linux).
//!
//! Two jobs:
//! 1. [`capture_canvas_size`]: the recording canvas = the per-axis bounding envelope of every
//!    active display, each normalized so its SHORT EDGE is [`TARGET_SHORT_EDGE`] (FHD ×1.0,
//!    4K ×0.5, 1920×1200 ×0.9, portrait 1440×2560 ×0.75 → 1080×1920). Displays are overlaid at
//!    the canvas origin (only one app on one display is ever shown), never tiled — the same
//!    model and rule as Windows/Linux.
//! 2. [`norm_for_pixel_size`] / [`window_display_for_pid`]: the scale factor (and, for
//!    follow-focus, the display) to draw a captured display-sized frame at, so it lands
//!    normalized to its display's short edge.
//!
//! **INVARIANT: a normalized envelope canvas is NEVER output-capped** — the OBS output must
//! equal this canvas (`canvas_and_output_dimensions`), exactly like Windows. Short-edge
//! normalization means a portrait display legitimately makes the envelope TALLER than 1080
//! (1440×2560 → 1080×1920); applying `max_output_height` to that envelope downscales EVERY
//! display's content to fit (field case: an ultrawide recorded at 0.42× effective scale
//! because an idle portrait monitor inflated the envelope and the cap crushed the canvas).
//! The cap exists only for the raw main-display fallback path, which is not normalized.
//!
//! **macOS differs from Windows/Linux in the FIT, not the canvas.** ScreenCaptureKit
//! *Application* capture hands libobs a **full-display-sized frame** with the app composited in
//! place (rest transparent/black) — NOT a window-cropped buffer. This was confirmed empirically
//! (Step 0): the source's `obs_source_get_width/height` equalled the target display's dimensions
//! on both a scale-1.0 external (1920×1200) and a scale-2.0 Retina built-in (2940×1912). So the
//! per-display transform is `scale = norm, pos = (0,0)` with **no per-window offset** (the
//! offset is already baked into the display-sized frame), unlike Windows/Linux which offset a
//! window-cropped buffer by its on-monitor position.
//!
//! **Units.** SCK reports **backing PIXELS** (2940×1912 on the @2× Retina, not the 1470×956
//! points) — confirmed in Step 0. So the canvas and `norm` are computed from each display's
//! PIXEL dimensions (`CGDisplayModeGetPixelWidth/Height`, the same source as
//! `get_main_display_resolution`), matching the pixel-space frame the transform is applied to.
#![cfg(target_os = "macos")]

use core_graphics::display::CGDisplay;
use std::ffi::c_void;

/// Normalize every display (and its captured frame) so its SHORT edge maps to this many
/// pixels: every display keeps at least 1080p-class quality regardless of orientation. The
/// envelope this produces must never be output-capped (see the module docs).
pub const TARGET_SHORT_EDGE: f64 = 1080.0;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGDisplayCopyDisplayMode(display: u32) -> *const c_void;
    fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: *const c_void) -> usize;
    fn CGDisplayModeRelease(mode: *const c_void);
}

/// Backing-PIXEL dimensions of a display's current mode (the same API and pixel semantics as
/// [`super::get_main_display_resolution`]). `None` if the mode can't be read or is zero.
pub fn display_pixel_size(display_id: u32) -> Option<(u32, u32)> {
    unsafe {
        let mode = CGDisplayCopyDisplayMode(display_id);
        if mode.is_null() {
            return None;
        }
        let w = CGDisplayModeGetPixelWidth(mode) as u32;
        let h = CGDisplayModeGetPixelHeight(mode) as u32;
        CGDisplayModeRelease(mode);
        (w > 0 && h > 0).then_some((w, h))
    }
}

/// PIXEL sizes of all active displays (empty if enumeration fails — callers fall back).
fn active_display_pixel_sizes() -> Vec<(u32, u32)> {
    CGDisplay::active_displays()
        .unwrap_or_default()
        .into_iter()
        .filter_map(display_pixel_size)
        .collect()
}

/// Normalization factor for a display of the given PIXEL size: `TARGET_SHORT_EDGE / short edge`.
/// `None` if the size is degenerate.
pub fn norm_for_pixel_size(px_w: u32, px_h: u32) -> Option<f32> {
    let short = px_w.min(px_h);
    (short > 0).then(|| (TARGET_SHORT_EDGE / short as f64) as f32)
}

/// Recording canvas size: per-axis max over all displays of each display's PIXEL size normalized
/// so its SHORT edge is [`TARGET_SHORT_EDGE`]. A portrait display contributes 1080×(long/short
/// ×1080) — the envelope may exceed 1080 in height, which is fine because the output equals the
/// canvas (never capped; see module docs). Even dimensions for the encoder. `None` if no display
/// reports a usable size (caller fails closed — never a guessed canvas).
pub fn normalized_canvas(sizes: &[(u32, u32)]) -> Option<(u32, u32)> {
    let mut max_w = 0u32;
    let mut max_h = 0u32;
    for &(w, h) in sizes {
        if w == 0 || h == 0 {
            continue;
        }
        let scale = TARGET_SHORT_EDGE / w.min(h) as f64;
        // ceil, not round: rounding down (frac < 0.5) would size the canvas fractionally
        // smaller than the continuous scaled footprint and clip a sub-pixel edge column.
        max_w = max_w.max((w as f64 * scale).ceil() as u32);
        max_h = max_h.max((h as f64 * scale).ceil() as u32);
    }
    // Ceil to even (round UP to even, never down): the render transform scales the source by the
    // exact continuous `norm`, so flooring to even (`& !1`) could make the canvas up to ~1px
    // SMALLER than the scaled footprint and clip the right/bottom edge (e.g. a lone 2940×1912
    // Retina normalizes to 1660.67px wide → floor-even 1660 crops). Rounding up leaves at most a
    // sub-pixel black sliver, which is harmless.
    (max_w > 0 && max_h > 0).then(|| ((max_w + 1) & !1, (max_h + 1) & !1))
}

/// The multi-monitor recording canvas for the current active display set. `None` if no display
/// is enumerable (caller falls back to the main-display resolution).
pub fn capture_canvas_size() -> Option<(u32, u32)> {
    normalized_canvas(&active_display_pixel_sizes())
}

// ---------------------------------------------------------------------------
// Phase C: follow-focus across displays.
//
// Resolve which display the focused window of a given app (by pid) sits on, so the SCK source
// can be retargeted to that display and normalized by it. All CoreGraphics C functions — no
// Objective-C, so no arm64 objc_msgSend variadic hazard. Uses the real `kCGWindow*` CFString
// constants (extern statics) + CGRectMakeWithDictionaryRepresentation, so there are no per-poll
// CFString allocations and the dictionary-key match is guaranteed.
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *const c_void;
    fn CGGetDisplaysWithPoint(
        point: CGPoint,
        max_displays: u32,
        displays: *mut u32,
        matching_count: *mut u32,
    ) -> i32;
    // Returns CoreFoundation `Boolean` (unsigned char) — model as u8 (declaring `-> bool` is UB
    // for any byte other than 0/1) and compare `!= 0`.
    fn CGRectMakeWithDictionaryRepresentation(dict: *const c_void, rect: *mut CGRect) -> u8;
    // The real CoreGraphics CFString key constants (CFStringRef). Reading these avoids
    // fabricating/allocating CFStrings on every poll.
    static kCGWindowOwnerPID: *const c_void;
    static kCGWindowBounds: *const c_void;
    static kCGWindowLayer: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, idx: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    // `Boolean` (unsigned char) — see note above; model as u8.
    fn CFNumberGetValue(number: *const c_void, the_type: i32, value: *mut c_void) -> u8;
    fn CFRelease(cf: *const c_void);
}

const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;
const K_CF_NUMBER_SINT32: i32 = 3;

/// A display to retarget an SCK source to: its CGDirectDisplayID, UUID (for `set_display_uuid`),
/// and normalization factor (1080 / PIXEL short edge).
pub struct DisplayTarget {
    pub id: u32,
    pub uuid: String,
    pub norm: f32,
}

/// Bundle a display id into a `DisplayTarget` (uuid + norm). `None` if either is unreadable.
fn display_target(display_id: u32) -> Option<DisplayTarget> {
    let (w, h) = display_pixel_size(display_id)?;
    let norm = norm_for_pixel_size(w, h)?;
    let uuid = crate::capture::get_display_uuid(display_id)?;
    Some(DisplayTarget {
        id: display_id,
        uuid,
        norm,
    })
}

/// The main display as a retarget target (the default placement before a window is resolved).
pub fn main_display_target() -> Option<DisplayTarget> {
    display_target(CGDisplay::main().id)
}

/// The display whose bounds contain a global (points) coordinate.
fn display_for_point(x: f64, y: f64) -> Option<u32> {
    unsafe {
        let mut ids = [0u32; 8];
        let mut count = 0u32;
        let err = CGGetDisplaysWithPoint(CGPoint { x, y }, 8, ids.as_mut_ptr(), &mut count);
        (err == 0 && count > 0).then_some(ids[0])
    }
}

fn read_i32(dict: *const c_void, key: *const c_void) -> Option<i32> {
    unsafe {
        let v = CFDictionaryGetValue(dict, key);
        if v.is_null() {
            return None;
        }
        let mut out: i32 = 0;
        (CFNumberGetValue(v, K_CF_NUMBER_SINT32, &mut out as *mut i32 as *mut c_void) != 0)
            .then_some(out)
    }
}

/// The display the focused window of process `pid` sits on, as a retarget target. Picks the
/// app's FRONTMOST on-screen, layer-0 (non-menubar/overlay) window — CGWindowList is ordered
/// front-to-back, so the first pid match is the focused window (good for follow-focus). `None`
/// if the process has no such window right now (caller keeps the current placement).
pub fn window_display_for_pid(pid: u32) -> Option<DisplayTarget> {
    unsafe {
        let arr = CGWindowListCopyWindowInfo(
            K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS,
            0,
        );
        if arr.is_null() {
            return None;
        }
        let count = CFArrayGetCount(arr);
        let mut center: Option<(f64, f64)> = None;
        for i in 0..count {
            let dict = CFArrayGetValueAtIndex(arr, i);
            if dict.is_null() {
                continue;
            }
            match read_i32(dict, kCGWindowOwnerPID) {
                Some(p) if p as u32 == pid => {}
                _ => continue,
            }
            // Only layer-0 (normal app windows); skip menubar/overlay layers. Fail CLOSED — an
            // unreadable layer is skipped rather than assumed to be a real window.
            if !matches!(read_i32(dict, kCGWindowLayer), Some(0)) {
                continue;
            }
            let bounds = CFDictionaryGetValue(dict, kCGWindowBounds);
            if bounds.is_null() {
                continue;
            }
            let mut rect = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: 0.0,
                    height: 0.0,
                },
            };
            if CGRectMakeWithDictionaryRepresentation(bounds, &mut rect) == 0 {
                continue;
            }
            if rect.size.width < 40.0 || rect.size.height < 40.0 {
                continue; // ignore tiny helper windows
            }
            center = Some((
                rect.origin.x + rect.size.width / 2.0,
                rect.origin.y + rect.size.height / 2.0,
            ));
            break; // frontmost matching window
        }
        CFRelease(arr);
        let (cx, cy) = center?;
        display_target(display_for_point(cx, cy)?)
    }
}

/// Describe a display for the recording metadata: UUID + name + global POINT bounds
/// (`CGDisplayBounds`) + backing pixel size + is_main. `None` if its UUID or pixel size is
/// unreadable.
pub fn describe_display(display_id: u32) -> Option<crate::data::MonitorInfo> {
    let (px_width, px_height) = display_pixel_size(display_id)?;
    let uuid = crate::capture::get_display_uuid(display_id)?;
    let cg = CGDisplay::new(display_id);
    let b = cg.bounds();
    Some(crate::data::MonitorInfo {
        uuid,
        name: crate::capture::get_display_name(display_id),
        x: b.origin.x as i32,
        y: b.origin.y as i32,
        width: b.size.width as i32,
        height: b.size.height as i32,
        px_width,
        px_height,
        is_main: cg.is_main(),
    })
}

/// The full monitor layout: describe every active display. Empty if enumeration fails.
pub fn describe_all_displays() -> Vec<crate::data::MonitorInfo> {
    CGDisplay::active_displays()
        .unwrap_or_default()
        .into_iter()
        .filter_map(describe_display)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_fhd_canvas_is_native() {
        assert_eq!(normalized_canvas(&[(1920, 1080)]), Some((1920, 1080)));
    }

    #[test]
    fn fourk_normalizes_to_1080_short_edge() {
        // 3840x2160 short edge 2160 -> ×0.5 -> 1920x1080.
        assert_eq!(normalized_canvas(&[(3840, 2160)]), Some((1920, 1080)));
    }

    #[test]
    fn envelope_takes_per_axis_max_not_area() {
        // 4K (normalizes to 1920x1080) + 32:9 ultrawide 5120x1440 (height 1440 ×0.75 ->
        // 3840x1080). Canvas must be wide enough for the ultrawide AND tall enough for the 4K.
        assert_eq!(
            normalized_canvas(&[(3840, 2160), (5120, 1440)]),
            Some((3840, 1080))
        );
    }

    #[test]
    fn portrait_normalizes_by_short_edge_keeping_quality() {
        // A portrait-rotated 1440x2560: SHORT edge is its width -> ×0.75 -> 1080x1920. The
        // envelope legitimately exceeds 1080 tall; the output equals the canvas (never capped),
        // so the portrait display keeps 1080p-class quality instead of being squeezed to 608
        // wide. (The historical bug was capping this envelope's output, which crushed every
        // display's content — the fix is in canvas_and_output_dimensions, not here.)
        assert_eq!(normalized_canvas(&[(1440, 2560)]), Some((1080, 1920)));
    }

    #[test]
    fn rotating_a_display_keeps_its_norm() {
        // The same physical monitor, landscape vs portrait: same short edge, same 0.75 norm,
        // transposed canvas contribution.
        assert_eq!(normalized_canvas(&[(2560, 1440)]), Some((1920, 1080)));
        assert_eq!(normalized_canvas(&[(1440, 2560)]), Some((1080, 1920)));
        assert_eq!(norm_for_pixel_size(1440, 2560), Some(0.75));
        assert_eq!(norm_for_pixel_size(2560, 1440), Some(0.75));
    }

    #[test]
    fn portrait_retina_short_edge_and_ceil() {
        // A rotated Retina: 2338x3600 PIXELS, short edge 2338 -> ×0.46193 -> 1080x1662.96;
        // the fractional HEIGHT must ceil (1663) then ceil-to-even (1664), never floor-crop.
        assert_eq!(normalized_canvas(&[(2338, 3600)]), Some((1080, 1664)));
    }

    #[test]
    fn mixed_orientation_field_rig_envelope() {
        // The field case (three displays): 3440x1440 ultrawide (×0.75 -> 2580x1080) +
        // 3600x2338 Retina (×0.4619 -> 1663x1080) + 1440x2560 portrait (×0.75 -> 1080x1920).
        // Envelope 2580x1920: every display keeps full short-edge quality; the output must
        // equal this canvas — capping it to 1080 tall is what produced the 0.42× recordings.
        assert_eq!(
            normalized_canvas(&[(3440, 1440), (3600, 2338), (1440, 2560)]),
            Some((2580, 1920))
        );
    }

    #[test]
    fn rig_dell_plus_retina_matches_plan() {
        // The dev rig (Step 0): external DELL 1920x1200 (short 1200 ×0.9 -> 1728x1080) + built-in
        // Retina 2940x1912 PIXELS (short 1912 ×0.5649 -> 1661x1080). Per-axis max = 1728x1080.
        assert_eq!(
            normalized_canvas(&[(1920, 1200), (2940, 1912)]),
            Some((1728, 1080))
        );
    }

    #[test]
    fn lone_retina_ceils_to_even_no_crop() {
        // Built-in Retina 2940x1912 PIXELS alone: short 1912 ×(1080/1912) -> 1660.67 wide,
        // 1080.0 tall. Must ceil the odd 1661 UP to even 1662 (>= footprint), never floor to
        // 1660 (which would clip the right edge of the scaled source).
        assert_eq!(normalized_canvas(&[(2940, 1912)]), Some((1662, 1080)));
    }

    #[test]
    fn empty_fails_closed() {
        assert_eq!(normalized_canvas(&[]), None);
        assert_eq!(normalized_canvas(&[(0, 0)]), None);
    }

    #[test]
    fn norm_factors() {
        // FHD ×1.0, 4K ×0.5, DELL 1920x1200 ×0.9, Retina 2940x1912 ×~0.5649.
        assert!((norm_for_pixel_size(1920, 1080).unwrap() - 1.0).abs() < 1e-4);
        assert!((norm_for_pixel_size(3840, 2160).unwrap() - 0.5).abs() < 1e-4);
        assert!((norm_for_pixel_size(1920, 1200).unwrap() - 0.9).abs() < 1e-4);
        assert!((norm_for_pixel_size(2940, 1912).unwrap() - (1080.0 / 1912.0) as f32).abs() < 1e-4);
        assert_eq!(norm_for_pixel_size(0, 0), None);
    }
}
