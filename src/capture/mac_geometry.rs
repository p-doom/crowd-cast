//! macOS multi-monitor capture geometry (the macOS analogue of `window_geometry.rs`
//! on Windows and `monitor_layout.rs` on Linux).
//!
//! Two jobs:
//! 1. [`capture_canvas_size`]: the recording canvas = the per-axis bounding envelope of every
//!    active display, each normalized so its shorter edge is [`TARGET_SHORT_EDGE`] (FHD ×1.0,
//!    4K ×0.5, 1920×1200 ×0.9, …). Displays are overlaid at the canvas origin (only one app on
//!    one display is ever shown), never tiled — matching the Windows/Linux model.
//! 2. [`main_display_norm`] / [`norm_for_pixel_size`]: the scale factor to draw a captured
//!    display-sized frame at, so it lands normalized to its display's short edge.
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

/// Normalize every display (and its captured frame) so its shorter edge maps to this many pixels.
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
/// so its short edge is [`TARGET_SHORT_EDGE`]. Even dimensions for the encoder. `None` if no
/// display reports a usable size (caller fails closed — never a guessed canvas).
pub fn normalized_canvas(sizes: &[(u32, u32)]) -> Option<(u32, u32)> {
    let mut max_w = 0u32;
    let mut max_h = 0u32;
    for &(w, h) in sizes {
        let short = w.min(h);
        if short == 0 {
            continue;
        }
        let scale = TARGET_SHORT_EDGE / short as f64;
        max_w = max_w.max((w as f64 * scale).round() as u32);
        max_h = max_h.max((h as f64 * scale).round() as u32);
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

/// Normalization factor for the MAIN display. Phase B pins the SCK source to the main display,
/// so the active frame is normalized by the main display's short edge. `None` if unreadable.
pub fn main_display_norm() -> Option<f32> {
    let (w, h) = display_pixel_size(CGDisplay::main().id)?;
    norm_for_pixel_size(w, h)
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
        // 4K (normalizes to 1920x1080) + 32:9 ultrawide 5120x1440 (short edge 1440 ×0.75 ->
        // 3840x1080). Canvas must be wide enough for the ultrawide AND tall enough for the 4K.
        assert_eq!(
            normalized_canvas(&[(3840, 2160), (5120, 1440)]),
            Some((3840, 1080))
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
