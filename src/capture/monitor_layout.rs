//! Linux multi-monitor capture canvas + per-app window fit, matching the Windows model
//! (`src/capture/window_geometry.rs` there).
//!
//! Two jobs:
//! 1. [`capture_canvas_size`]: the recording canvas = the per-axis bounding envelope of every
//!    monitor, each normalized so its HEIGHT is [`TARGET_HEIGHT`] (FHD ×1.0, 4K ×0.5,
//!    3440×1440 ×0.75). Sized so *any* single monitor, normalized, fits — monitors are overlaid
//!    at the canvas origin (only one app on one monitor is ever shown), never tiled.
//! 2. [`fit_for_window`]: for a captured window, the scale + top-left position to draw it at so
//!    it sits at its real on-monitor location, scaled by its monitor's normalization factor. A
//!    half-monitor window therefore fills half the frame, where it actually is; the rest is
//!    blank. The scene scale is derived from the *actual* captured source-buffer size rather
//!    than a reported scale factor, so HiDPI / fractional scaling is handled without trusting
//!    Mutter's scale convention.
//!
//! **Height, NOT short edge.** Unlike Windows (whose output equals its canvas, uncapped), the
//! Linux output is downscaled to `max_output_height`, so a portrait-rotated monitor normalized
//! by its short edge (its WIDTH) would inflate the envelope's height past 1080 and get the
//! whole canvas — every monitor's content — crushed to fit the cap. Height normalization keeps
//! the envelope at exactly 1080 tall, so the cap never engages and a portrait monitor costs
//! only its own (naturally narrow) width. Same rule and rationale as macOS (mac_geometry.rs);
//! the field case that surfaced it was a macOS ultrawide recorded at 0.42× effective scale
//! because an idle portrait monitor sat in the layout.
//!
//! Monitor rectangles come from `wl_output` (Wayland) or RandR (X11). The per-window monitor
//! rectangle used by [`fit_for_window`] is supplied by the caller — on GNOME from the focus
//! extension (logical coords), on X11 from RandR + the window geometry (physical coords) — and
//! must be in the same units as the window rectangle passed alongside it.
#![cfg(target_os = "linux")]

/// Normalize every monitor (and window) so its HEIGHT maps to this many pixels. Height rather
/// than short edge: see the module docs — short-edge normalization lets a portrait monitor
/// inflate the canvas envelope and (via the output cap) degrade every monitor's resolution.
pub const TARGET_HEIGHT: f64 = 1080.0;

/// A rectangle in a single coordinate space (Wayland logical or X11/physical pixels). For
/// monitors fed to the canvas envelope only `w`/`h` matter; `x`/`y` are used to place a window
/// relative to its monitor in [`fit_for_window`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    fn height(&self) -> f64 {
        self.h.max(0) as f64
    }
}

/// Scale + top-left position (in canvas pixels) at which to draw a captured window's source so
/// it lands at its real on-monitor location, normalized to its monitor's height. Mirrors the
/// Windows `MonitorFit` (which normalizes by short edge — safe there because the Windows output
/// is not capped; see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorFit {
    pub scale: f32,
    pub pos_x: f32,
    pub pos_y: f32,
}

/// Recording canvas size: per-axis max over all monitors of each monitor's size normalized so
/// its HEIGHT is [`TARGET_HEIGHT`] (every normalized height is exactly 1080, so the envelope
/// only ever grows in width). Even dimensions for the encoder. `None` if no monitor reports a
/// usable size (caller fails closed — never a guessed canvas).
pub fn normalized_canvas(monitors: &[Rect]) -> Option<(u32, u32)> {
    let mut max_w = 0u32;
    let mut max_h = 0u32;
    for m in monitors {
        let height = m.height();
        if height <= 0.0 || m.w <= 0 {
            continue;
        }
        let scale = TARGET_HEIGHT / height;
        max_w = max_w.max((m.w as f64 * scale).ceil() as u32);
        max_h = max_h.max((m.h as f64 * scale).ceil() as u32);
    }
    // Ceil to even (round UP, never down): the render transform scales sources by the exact
    // continuous norm, so a floored canvas can be up to ~1px smaller than the widest monitor's
    // scaled footprint and clip its right edge (a portrait monitor's normalized width is
    // rarely a whole number). Rounding up leaves at most a sub-pixel black sliver.
    (max_w > 0 && max_h > 0).then(|| ((max_w + 1) & !1, (max_h + 1) & !1))
}

/// Compute the fit for `window` on `monitor` given the monitor's `monitor_scale` factor
/// (logical-to-physical, e.g. 1.0 at 100%, 2.0 at 200%; X11 is always 1.0).
///
/// `window` and `monitor` are in the *same* coordinate space: logical pixels on GNOME, physical
/// on X11. `norm` (= `TARGET_HEIGHT / monitor height`) maps those pixels onto the
/// normalized canvas; the scene-item scale is `norm / monitor_scale`, the extra division
/// converting to the *physical* captured buffer the transform is applied to (buffer ≈
/// logical × monitor_scale).
///
/// We deliberately do NOT derive the scale from the captured buffer size. That size is reported
/// unreliably (a PipeWire source briefly reads an 800×600 placeholder before frames flow), which
/// made the scale flicker between correct and wildly wrong values frame-to-frame: the window
/// jumped size (encoding artifacts) and at the wrong scale overflowed the canvas (cropped). The
/// monitor scale is stable. `None` if the monitor geometry is unusable.
pub fn fit_for_window(window: Rect, monitor: Rect, monitor_scale: f64) -> Option<MonitorFit> {
    let height = monitor.height();
    if height <= 0.0 {
        return None;
    }
    let scale_factor = if monitor_scale > 0.0 {
        monitor_scale
    } else {
        1.0
    };
    let norm = TARGET_HEIGHT / height;
    let scale = (norm / scale_factor) as f32;
    let pos_x = ((window.x - monitor.x) as f64 * norm) as f32;
    let pos_y = ((window.y - monitor.y) as f64 * norm) as f32;
    Some(MonitorFit {
        scale,
        pos_x,
        pos_y,
    })
}

/// All monitor rectangles for the current session, or `None` if none can be enumerated. Wayland
/// reports per-output current modes (physical pixels, positionless — fine for the envelope); a
/// pure X11 session reports RandR monitor rects. The order is the compositor/RandR order.
pub fn monitor_rects() -> Option<Vec<Rect>> {
    if super::x11_windows::is_pure_x11_session() {
        let rects = super::x11_windows::x11_monitor_rects()?;
        return Some(
            rects
                .into_iter()
                .map(|(x, y, w, h)| Rect::new(x, y, w, h))
                .collect(),
        );
    }
    let sizes = super::wayland_output::wayland_output_sizes();
    if sizes.is_empty() {
        return None;
    }
    Some(
        sizes
            .into_iter()
            .map(|(w, h)| Rect::new(0, 0, w as i32, h as i32))
            .collect(),
    )
}

/// The multi-monitor recording canvas size for this session: the normalized envelope of every
/// monitor. `None` if no monitor is reported (caller fails closed).
pub fn capture_canvas_size() -> Option<(u32, u32)> {
    normalized_canvas(&monitor_rects()?)
}

/// The monitor `window` mostly sits on (largest overlap), falling back to the first monitor.
/// `None` only if there are no monitors. Used by the X11 fit, where the window rect and monitor
/// rects are both in root/physical coordinates. (GNOME gets the monitor straight from the
/// extension, so it doesn't need this.)
pub fn monitor_containing(window: Rect, monitors: &[Rect]) -> Option<Rect> {
    monitors
        .iter()
        .copied()
        .max_by_key(|m| overlap_area(window, *m))
        .filter(|m| overlap_area(window, *m) > 0)
        .or_else(|| monitors.first().copied())
}

fn overlap_area(a: Rect, b: Rect) -> i64 {
    let x = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let y = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    (x.max(0) as i64) * (y.max(0) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_fhd_canvas_is_native() {
        let c = normalized_canvas(&[Rect::new(0, 0, 1920, 1080)]).unwrap();
        assert_eq!(c, (1920, 1080));
    }

    #[test]
    fn fourk_normalizes_to_1080_height() {
        // 3840x2160 height 2160 -> ×0.5 -> 1920x1080.
        let c = normalized_canvas(&[Rect::new(0, 0, 3840, 2160)]).unwrap();
        assert_eq!(c, (1920, 1080));
    }

    #[test]
    fn portrait_normalizes_by_height_not_short_edge() {
        // A portrait-rotated 1440x2560: HEIGHT 2560 ×0.421875 -> 607.5 -> ceil-even 608x1080.
        // Under the old short-edge rule this was 1080x1920, inflating the envelope height and
        // getting the whole canvas crushed by the output cap (max_output_height).
        let c = normalized_canvas(&[Rect::new(0, 0, 1440, 2560)]).unwrap();
        assert_eq!(c, (608, 1080));
    }

    #[test]
    fn mixed_orientation_envelope_not_inflated() {
        // Ultrawide + portrait: the portrait monitor must not stretch the envelope past 1080
        // tall (which would trigger the output cap and downscale the ultrawide's content).
        let c =
            normalized_canvas(&[Rect::new(0, 0, 3440, 1440), Rect::new(0, 0, 1440, 2560)]).unwrap();
        assert_eq!(c, (2580, 1080));
    }

    #[test]
    fn fit_portrait_monitor_uses_height_norm() {
        // Window filling a portrait 1440x2560 monitor at scale 1: norm = 1080/2560, matching
        // the canvas rule so the footprint (607.5x1080) fits the 608x1080 canvas slot.
        let fit = fit_for_window(
            Rect::new(0, 0, 1440, 2560),
            Rect::new(0, 0, 1440, 2560),
            1.0,
        )
        .unwrap();
        assert!((fit.scale - 0.421875).abs() < 1e-4, "scale {}", fit.scale);
    }

    #[test]
    fn envelope_takes_per_axis_max_not_area() {
        // 4K (8.29 MP, normalizes to 1920x1080) + 32:9 ultrawide 5120x1440 (7.37 MP, smaller
        // area but wider: height 1440 -> ×0.75 -> 3840x1080). Canvas must be wide enough for
        // the ultrawide AND tall enough for the 4K — per-axis max, not the largest by area.
        let c =
            normalized_canvas(&[Rect::new(0, 0, 3840, 2160), Rect::new(0, 0, 5120, 1440)]).unwrap();
        assert_eq!(c, (3840, 1080));
    }

    #[test]
    fn empty_monitors_fail_closed() {
        assert_eq!(normalized_canvas(&[]), None);
    }

    #[test]
    fn fit_fhd_window_at_scale_1_is_identity() {
        // 1920x1080 window filling a 1920x1080 monitor at scale 1: scale 1.0, origin.
        let fit = fit_for_window(
            Rect::new(0, 0, 1920, 1080),
            Rect::new(0, 0, 1920, 1080),
            1.0,
        )
        .unwrap();
        assert!((fit.scale - 1.0).abs() < 1e-4, "scale {}", fit.scale);
        assert_eq!((fit.pos_x, fit.pos_y), (0.0, 0.0));
    }

    #[test]
    fn fit_4k_monitor_at_scale_1_halves() {
        // 4K monitor at scale 1: norm = 1080/2160 = 0.5, so the captured 4K buffer is drawn at
        // 0.5 -> 1920x1080 in the canvas.
        let fit = fit_for_window(
            Rect::new(0, 0, 3840, 2160),
            Rect::new(0, 0, 3840, 2160),
            1.0,
        )
        .unwrap();
        assert!((fit.scale - 0.5).abs() < 1e-4, "scale {}", fit.scale);
    }

    #[test]
    fn fit_hidpi_logical_window_uses_monitor_scale() {
        // GNOME HiDPI: window logical 1280x720 on a logical 1920x1080 monitor at scale 2 (Mutter
        // buffer physical 2560x1440). norm = 1080/1080 = 1.0; scene scale = 1.0/2 = 0.5 maps the
        // physical buffer onto the 1280x720 footprint. Position is logical*norm.
        let fit = fit_for_window(
            Rect::new(100, 50, 1280, 720),
            Rect::new(0, 0, 1920, 1080),
            2.0,
        )
        .unwrap();
        assert!((fit.scale - 0.5).abs() < 1e-4, "scale {}", fit.scale);
        assert!((fit.pos_x - 100.0).abs() < 1e-3, "pos_x {}", fit.pos_x);
        assert!((fit.pos_y - 50.0).abs() < 1e-3, "pos_y {}", fit.pos_y);
    }

    #[test]
    fn fit_2560x1440_maximized_real_case() {
        // The real bug case: a maximized window (below the 32px top bar) on a 2560x1440 monitor
        // at scale 1. norm = 1080/1440 = 0.75; scale must be a stable 0.75 (not derived from the
        // flickering source buffer), positioned at (0, 24). Buffer 2560x1408 * 0.75 = 1920x1056,
        // fitting the 1920x1080 canvas with no crop.
        let fit = fit_for_window(
            Rect::new(0, 32, 2560, 1408),
            Rect::new(0, 0, 2560, 1440),
            1.0,
        )
        .unwrap();
        assert!((fit.scale - 0.75).abs() < 1e-4, "scale {}", fit.scale);
        assert!((fit.pos_x - 0.0).abs() < 1e-3, "pos_x {}", fit.pos_x);
        assert!((fit.pos_y - 24.0).abs() < 1e-3, "pos_y {}", fit.pos_y);
    }

    #[test]
    fn fit_zero_monitor_is_none() {
        assert_eq!(
            fit_for_window(Rect::new(0, 0, 100, 100), Rect::new(0, 0, 0, 0), 1.0),
            None
        );
    }
}
