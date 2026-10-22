//! Windows: forward libobs log lines into our tracing log (#175).
//!
//! libobs-wrapper's default `ConsoleLogger` only `println!`s, and the GUI-subsystem release
//! binary has no stdout, so encoder errors (for example QSV's `MFX_ERR_UNSUPPORTED`) never
//! reached the agent log. This logger keeps printing exactly what `ConsoleLogger` printed and
//! additionally forwards:
//! - warning and error lines, always. Outside an encoder start attempt they pass a flood
//!   guard (at most `WARN_ERROR_PER_MINUTE` per minute, then one line saying how many were
//!   suppressed), so a warning OBS repeats every frame cannot fill the log;
//! - info lines only while an encoder start attempt is in progress (see
//!   `encoder_start_window`). Inside the attempt every level shares one budget of
//!   `LINES_PER_WINDOW` lines, so the encoder's settings dump and its errors are captured
//!   even right after a noisy startup, without OBS's normal chatter.
//!
//! Debug lines are never forwarded. `log` runs inside libobs' log callback, under the
//! wrapper's global logger mutex, on whatever thread logged; it must stay cheap and must
//! never panic (a panic would poison that mutex and abort in the next callback).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use libobs_wrapper::enums::ObsLogLevel;
use libobs_wrapper::logger::{ConsoleLogger, ObsLogger};
use tracing::{error, info, warn};

const WARN_ERROR_PER_MINUTE: u32 = 20;
const LINES_PER_WINDOW: u32 = 150;
const FLOOD_PERIOD: Duration = Duration::from_secs(60);

/// Number of encoder start attempts in progress (normally 0 or 1).
static WINDOW_DEPTH: AtomicUsize = AtomicUsize::new(0);
/// Bumped each time a window opens, so the logger resets its per-attempt info budget.
static WINDOW_GENERATION: AtomicU64 = AtomicU64::new(0);

/// While alive, libobs info, warning and error lines are forwarded (shared budget).
pub(super) struct EncoderStartWindow(());

impl Drop for EncoderStartWindow {
    fn drop(&mut self) {
        WINDOW_DEPTH.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Open an info-forwarding window around one encoder build + start attempt.
pub(super) fn encoder_start_window() -> EncoderStartWindow {
    WINDOW_GENERATION.fetch_add(1, Ordering::SeqCst);
    WINDOW_DEPTH.fetch_add(1, Ordering::SeqCst);
    EncoderStartWindow(())
}

/// Fixed-window counter: admits up to `limit` events per `period`, and reports how many were
/// dropped in the previous window when a new one starts.
#[derive(Debug)]
struct FloodGuard {
    limit: u32,
    period: Duration,
    window_start: Option<Instant>,
    count: u32,
    suppressed: u32,
}

impl FloodGuard {
    fn new(limit: u32, period: Duration) -> Self {
        Self {
            limit,
            period,
            window_start: None,
            count: 0,
            suppressed: 0,
        }
    }

    /// Returns (admit this event, lines suppressed in the window that just ended).
    fn admit(&mut self, now: Instant) -> (bool, u32) {
        let mut ended_suppressed = 0;
        let expired = match self.window_start {
            None => true,
            Some(start) => now.saturating_duration_since(start) >= self.period,
        };
        if expired {
            ended_suppressed = self.suppressed;
            self.window_start = Some(now);
            self.count = 0;
            self.suppressed = 0;
        }
        if self.count < self.limit {
            self.count += 1;
            (true, ended_suppressed)
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
            (false, ended_suppressed)
        }
    }
}

#[derive(Debug)]
pub(super) struct TracingObsLogger {
    console: ConsoleLogger,
    warn_error_guard: FloodGuard,
    window_generation: u64,
    window_left: u32,
}

impl TracingObsLogger {
    pub(super) fn new() -> Self {
        Self {
            console: ConsoleLogger::new(),
            warn_error_guard: FloodGuard::new(WARN_ERROR_PER_MINUTE, FLOOD_PERIOD),
            window_generation: u64::MAX,
            window_left: 0,
        }
    }
}

impl ObsLogger for TracingObsLogger {
    fn log(&mut self, level: ObsLogLevel, msg: String) {
        let in_window = WINDOW_DEPTH.load(Ordering::SeqCst) > 0;
        let forward = match level {
            ObsLogLevel::Debug => false,
            _ if in_window => {
                let generation = WINDOW_GENERATION.load(Ordering::SeqCst);
                if generation != self.window_generation {
                    self.window_generation = generation;
                    self.window_left = LINES_PER_WINDOW;
                }
                if self.window_left > 0 {
                    self.window_left -= 1;
                    true
                } else {
                    false
                }
            }
            ObsLogLevel::Info => false,
            ObsLogLevel::Error | ObsLogLevel::Warning => {
                let (admit, suppressed) = self.warn_error_guard.admit(Instant::now());
                if suppressed > 0 {
                    warn!(
                        "[libobs] suppressed {} warning/error lines in the last minute",
                        suppressed
                    );
                }
                admit
            }
        };
        if forward {
            match level {
                ObsLogLevel::Error => error!("[libobs] {}", msg),
                ObsLogLevel::Warning => warn!("[libobs] {}", msg),
                _ => info!("[libobs] {}", msg),
            }
        }

        // Unchanged from the wrapper's default logger (stdout, discarded by the release binary).
        self.console.log(level, msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flood_guard_caps_per_window_and_reports_suppressed() {
        let mut g = FloodGuard::new(2, Duration::from_secs(60));
        let t0 = Instant::now();
        assert_eq!(g.admit(t0), (true, 0));
        assert_eq!(g.admit(t0), (true, 0));
        assert_eq!(g.admit(t0), (false, 0));
        assert_eq!(g.admit(t0 + Duration::from_secs(1)), (false, 0));
        // New window: admitted again, and the 2 dropped lines are reported once.
        assert_eq!(g.admit(t0 + Duration::from_secs(61)), (true, 2));
        assert_eq!(g.admit(t0 + Duration::from_secs(62)), (true, 0));
    }

    #[test]
    fn info_window_depth_tracks_guard_lifetime() {
        let before = WINDOW_DEPTH.load(Ordering::SeqCst);
        let gen_before = WINDOW_GENERATION.load(Ordering::SeqCst);
        {
            let _w = encoder_start_window();
            assert!(WINDOW_DEPTH.load(Ordering::SeqCst) > before);
            assert!(WINDOW_GENERATION.load(Ordering::SeqCst) > gen_before);
        }
        assert_eq!(WINDOW_DEPTH.load(Ordering::SeqCst), before);
    }
}
