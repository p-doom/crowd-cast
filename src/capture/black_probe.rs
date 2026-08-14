//! Output-level blackness probe — the "silent black recording" detector (PDOOM-1298).
//!
//! WHY: when crowd-cast (re)creates an SCK capture source at a transitional moment (an app
//! that only just launched, a display-change/unlock restart), the source for a GPU-heavy app
//! (Cursor, Antigravity) can come up permanently BLACK while still reporting valid
//! dimensions. `active_source_is_ready()` is `width > 0 && height > 0`, so the dead-source
//! watchdog sees a healthy source and never re-binds it — hours of silent black video, only
//! discovered after the study session. A later process restart against the settled app
//! captures perfectly, so the cure is known; what was missing was a *signal*.
//!
//! Dimensions can't provide that signal, and neither can mean/max luma: app-capture black
//! frames still contain the real macOS menu-bar strip (bright pixels) and the mouse cursor.
//! What separates them is the black-pixel FRACTION — capture-black is limited-range Y<=16-17,
//! dark-theme IDE backgrounds sit at Y>=30, and menu bar + cursor are under 5% of the frame.
//! So we tap the composited OUTPUT (not a source) with a raw video callback, downscaled to a
//! thumbnail, and publish "what fraction of the frame is black" for the engine to act on.
//!
//! That fraction is evidence, not a verdict: genuinely sparse content (a full-screen dark
//! terminal at an empty prompt) also reads as almost entirely black. The engine corroborates
//! it with "this source has never once produced content" before acting — see
//! `blind_gates_met` in the sync engine.
//!
//! The callback runs on OBS's video thread, so it is `extern "C"`, allocation-free,
//! panic-free, touches only `'static` atomics, and scans just one frame in every
//! [`PROBE_EVERY_N_FRAMES`] (~2s at 30fps). Registration uses a null `param`, so there is
//! nothing for the callback to outlive and nothing to clean up before an exec()-based
//! restart.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use libobs_wrapper::runtime::ObsRuntime;
use tracing::{debug, warn};

/// Luma at or below this counts as capture-black. Capture-black arrives as limited-range
/// yuv420p Y=16-17; the darkest real UI backgrounds we care about (dark-theme editors) are
/// Y>=30, so 20 sits in the gap with headroom on both sides.
pub const BLACK_Y_MAX: u8 = 20;

/// Probe thumbnail size. Big enough that the menu-bar strip and the cursor stay a small
/// minority of pixels, small enough that a full scan is a rounding error on the video thread.
const PROBE_WIDTH: u32 = 480;
const PROBE_HEIGHT: u32 = 270;

/// Scan one frame in every N (~2s at the 30fps this records at). The bug we're detecting
/// lasts for hours; sampling any faster only burns video-thread time.
const PROBE_EVERY_N_FRAMES: u64 = 60;

/// Frames delivered to the callback since process start (drives the 1-in-N sampling).
static FRAMES_SEEN: AtomicU64 = AtomicU64::new(0);

/// Black-pixel fraction of the most recent scanned frame, scaled by 1000 (atomics can't hold
/// an f32). Only meaningful once [`SAMPLE_SEQ`] is non-zero.
static PCT_BLACK_MILLI: AtomicU32 = AtomicU32::new(0);

/// NON-black pixel count of the most recent scanned frame. The engine uses its GROWTH to
/// tell live-but-sparse content from a wedged capture: typing into a true-black-themed
/// full-screen terminal stays above the black-percentage threshold indefinitely (each glyph
/// is a handful of thumbnail pixels), but every keystroke ADDS ink — while a wedged frame's
/// non-black count barely moves (menu-bar clock, cursor). Growth is the one signal the
/// percentage can never provide.
static NONBLACK_COUNT: AtomicU32 = AtomicU32::new(0);

/// Monotonically increasing count of scanned frames. The engine uses it to tell a FRESH
/// sample from a stale one: if the video thread stops delivering frames entirely, the last
/// reading must not be re-counted as ongoing evidence.
static SAMPLE_SEQ: AtomicU64 = AtomicU64::new(0);

/// `(black, total)` pixel counts of the Y plane, black meaning at or below [`BLACK_Y_MAX`].
///
/// `linesize` is the plane stride, which OBS may pad beyond `width` — the padding bytes are
/// undefined and must never be counted. Total, panic-free and allocation-free: it is called
/// from the OBS video thread, where a panic would unwind across an FFI boundary (UB).
fn black_pixel_counts(y_plane: &[u8], width: usize, height: usize, linesize: usize) -> (usize, usize) {
    if width == 0 || height == 0 || linesize < width {
        return (0, 0);
    }
    let mut black = 0usize;
    let mut total = 0usize;
    for row in 0..height {
        let Some(start) = row.checked_mul(linesize) else {
            break;
        };
        let Some(end) = start.checked_add(width) else {
            break;
        };
        // A short plane (shouldn't happen — OBS sizes it from the stride) truncates the scan
        // rather than panicking; the counts are then over the rows we actually saw.
        let Some(line) = y_plane.get(start..end) else {
            break;
        };
        for &px in line {
            if px <= BLACK_Y_MAX {
                black += 1;
            }
        }
        total += width;
    }
    (black, total)
}

/// Fraction (0.0..=1.0) of the Y plane at or below [`BLACK_Y_MAX`].
fn black_pixel_fraction(y_plane: &[u8], width: usize, height: usize, linesize: usize) -> f32 {
    let (black, total) = black_pixel_counts(y_plane, width, height, linesize);
    if total == 0 {
        0.0
    } else {
        black as f32 / total as f32
    }
}

/// OBS video-thread callback. Must stay cheap, allocation-free and panic-free.
unsafe extern "C" fn on_raw_video(_param: *mut c_void, frame: *mut libobs::video_data) {
    if FRAMES_SEEN.fetch_add(1, Ordering::Relaxed) % PROBE_EVERY_N_FRAMES != 0 {
        return;
    }
    if frame.is_null() {
        return;
    }
    // I420: plane 0 is Y, one byte per pixel.
    let data = (*frame).data[0];
    let linesize = (*frame).linesize[0] as usize;
    let width = PROBE_WIDTH as usize;
    let height = PROBE_HEIGHT as usize;
    if data.is_null() || linesize < width {
        return;
    }
    // Only the bytes we actually read: full strides for every row but the last, then `width`
    // bytes of the final row. Anything past that may not belong to the plane.
    let Some(len) = linesize
        .checked_mul(height.saturating_sub(1))
        .and_then(|n| n.checked_add(width))
    else {
        return;
    };
    let plane = std::slice::from_raw_parts(data, len);
    let (black, total) = black_pixel_counts(plane, width, height, linesize);
    let pct = if total == 0 {
        0.0
    } else {
        black as f32 / total as f32
    };

    PCT_BLACK_MILLI.store((pct * 1000.0) as u32, Ordering::Relaxed);
    NONBLACK_COUNT.store(total.saturating_sub(black) as u32, Ordering::Relaxed);
    // Release-publish the reading: a reader that observes the new sequence number is
    // guaranteed to observe the percentage that goes with it.
    SAMPLE_SEQ.fetch_add(1, Ordering::Release);
}

fn probe_conversion() -> libobs::video_scale_info {
    libobs::video_scale_info {
        format: libobs::video_format_VIDEO_FORMAT_I420,
        width: PROBE_WIDTH,
        height: PROBE_HEIGHT,
        // Capture-black is limited-range Y=16-17, and BLACK_Y_MAX is calibrated for that, so
        // ask for limited range explicitly rather than inheriting whatever the output uses.
        range: libobs::video_range_type_VIDEO_RANGE_PARTIAL,
        colorspace: libobs::video_colorspace_VIDEO_CS_DEFAULT,
    }
}

/// Attach the probe to the composited video output.
///
/// All libobs calls are funnelled through the wrapper's OBS thread (`run_with_obs!`), like
/// every other raw-binding call in this crate. Failures are logged, never propagated: the
/// probe is a diagnostic tap, and losing it must never stop a recording from starting.
pub fn register(runtime: &ObsRuntime) {
    if let Err(e) = libobs_wrapper::run_with_obs!(runtime, move || unsafe {
        // OBS copies the conversion into its video input, so a local is fine.
        let conversion = probe_conversion();
        libobs::obs_add_raw_video_callback(
            &conversion as *const _,
            Some(on_raw_video),
            std::ptr::null_mut(),
        );
    }) {
        warn!(
            "black probe: could not attach output blackness probe: {}",
            e
        );
        return;
    }
    debug!(
        "black probe: attached at {}x{} (1 frame in {})",
        PROBE_WIDTH, PROBE_HEIGHT, PROBE_EVERY_N_FRAMES
    );
}

/// Detach the probe. Registering a raw-video consumer forces OBS to download the mix to CPU
/// on every frame, so the probe only stays attached while a recording is actually running —
/// when nothing is being written, black output costs nothing and proves nothing. Removing a
/// callback that isn't registered is a documented no-op (OBS searches its consumer list), so
/// this is safe to call unconditionally.
pub fn remove(runtime: &ObsRuntime) {
    if let Err(e) = libobs_wrapper::run_with_obs!(runtime, move || unsafe {
        libobs::obs_remove_raw_video_callback(Some(on_raw_video), std::ptr::null_mut());
    }) {
        debug!("black probe: detach failed: {}", e);
    }
}

/// Re-attach after a `reset_video`, which rebuilds the video output the callback was
/// connected to. Removing first (matched on the callback+param pair, hence the same null
/// `param` used at registration) keeps this idempotent if the connection did survive.
pub fn reregister(runtime: &ObsRuntime) {
    remove(runtime);
    register(runtime);
}

/// Latest reading as `(black_pixel_fraction, nonblack_pixel_count, sample_sequence)`, or
/// `None` until the probe has scanned its first frame. The sequence number only ever
/// increases; callers compare it against the last one they consumed so a frozen video thread
/// reads as "no new evidence" rather than as sustained blackness.
pub fn output_black_stats() -> Option<(f32, u32, u64)> {
    let seq = SAMPLE_SEQ.load(Ordering::Acquire);
    if seq == 0 {
        return None;
    }
    Some((
        PCT_BLACK_MILLI.load(Ordering::Relaxed) as f32 / 1000.0,
        NONBLACK_COUNT.load(Ordering::Relaxed),
        seq,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Y plane with `linesize > width` padding, filled row-major from `pixels`, with
    /// the padding set to 0xFF (bright) so a scan that wrongly includes it is obvious.
    fn padded_plane(width: usize, height: usize, linesize: usize, pixels: &[u8]) -> Vec<u8> {
        let mut plane = vec![0xFF_u8; linesize * height];
        for row in 0..height {
            for col in 0..width {
                plane[row * linesize + col] = pixels[row * width + col];
            }
        }
        plane
    }

    #[test]
    fn all_black_frame_is_fully_black() {
        // Pure capture-black (limited-range Y=16) over a padded plane → 1.0, proving the
        // stride padding (0xFF) is excluded.
        let plane = padded_plane(4, 3, 16, &[16; 12]);
        assert_eq!(black_pixel_fraction(&plane, 4, 3, 16), 1.0);
    }

    #[test]
    fn dark_ui_is_not_black() {
        // A dark-theme editor background (Y=30) must never read as black — that is the
        // false-positive this threshold exists to avoid.
        let plane = padded_plane(4, 3, 9, &[30; 12]);
        assert_eq!(black_pixel_fraction(&plane, 4, 3, 9), 0.0);
    }

    #[test]
    fn menu_bar_and_cursor_still_read_as_black() {
        // The incident's signature: a black capture that still shows the real menu-bar strip
        // and the mouse cursor, which is why mean/max luma can't detect it. The menu bar is
        // ~24pt of a ~2000pt-tall display, i.e. roughly one row of the 270-row probe frame —
        // modelled here as 1 bright row in 100 plus a cursor pixel. The fraction must still
        // clear the engine's 0.97 gate.
        let width = 10;
        let height = 100;
        let mut pixels = vec![16_u8; width * height];
        for px in pixels.iter_mut().take(width) {
            *px = 200; // menu bar
        }
        pixels[width * 40 + 3] = 220; // cursor
        let linesize = width + 7;
        let plane = padded_plane(width, height, linesize, &pixels);
        let pct = black_pixel_fraction(&plane, width, height, linesize);
        assert!(pct > 0.97, "expected >0.97 black, got {pct}");
    }

    #[test]
    fn sparse_real_content_also_reads_as_black() {
        // Documents the limit of this signal, and why the engine does NOT act on it alone: a
        // genuinely-alive full-screen dark terminal showing one short prompt line (no menu
        // bar — macOS hides it in full screen) is indistinguishable from a wedged capture by
        // pixel fraction. ~0.4% ink here, so it clears the engine's 0.97 gate exactly like a
        // black source would. The defense against acting on it lives in
        // `blind_gates_met`'s "this source has produced real content" gate, not here.
        let width = 100;
        let height = 100;
        let mut pixels = vec![16_u8; width * height];
        for px in pixels[width * 50..width * 50 + 40].iter_mut() {
            *px = 180; // one prompt line, 40 of 100 columns
        }
        let pct = black_pixel_fraction(&pixels, width, height, width);
        assert!(pct > 0.97, "expected >0.97 black, got {pct}");
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        assert_eq!(black_pixel_fraction(&[], 0, 0, 0), 0.0);
        // linesize < width is malformed input, not a reason to panic on the video thread.
        assert_eq!(black_pixel_fraction(&[16, 16], 4, 1, 2), 0.0);
        // A plane shorter than the geometry claims truncates instead of indexing out of range.
        assert_eq!(black_pixel_fraction(&[16, 16, 16, 16], 4, 8, 4), 1.0);
    }
}
