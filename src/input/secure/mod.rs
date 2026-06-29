//! Secure-input gating: withhold key events from capture while a *secure context*
//! (currently a focused password field) is active.
//!
//! Policy: default to capturing. Only a positive secure signal flips the gate on; when
//! classification is unknown we capture and rely on server-side post-processing. This
//! raises the floor on what leaves the machine — it is best-effort, not a guarantee.
//!
//! Linux uses AT-SPI focus events to detect password fields (see [`atspi_gate`]). On
//! macOS/Windows the OS handles this (e.g. macOS Secure Event Input), so the gate here
//! is inert and the rdev backend never consults it.
//!
//! P2 (scaffolded via [`Sources::terminal_secure`]): detect terminal password prompts by
//! observing a cleared `ECHO` termios flag on the focused pty. The ECHO-off signal is
//! validated; the focused-window -> pid -> pts mapping on Wayland is not yet, so the
//! terminal source is defined but not yet driven.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[cfg(target_os = "linux")]
mod atspi_gate;

/// Shared gate consulted by the input backend on every key event. Cheap to read.
#[derive(Debug)]
pub struct SecureInputState {
    suppress: AtomicBool,
    sources: Mutex<Sources>,
}

#[derive(Debug, Default)]
#[allow(dead_code)] // some fields are only written on Linux
struct Sources {
    /// A focused password field was detected via AT-SPI.
    atspi_secure: bool,
    /// A focused terminal has a password prompt (echo disabled). P2; not yet driven.
    terminal_secure: bool,
    /// Human-readable reason for the active suppression.
    reason: Option<String>,
}

/// Edge reported by an update, so callers can act on transitions (e.g. emit a marker).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Left/Unchanged only matched on Linux
pub enum Transition {
    Entered,
    Left,
    Unchanged,
}

#[allow(dead_code)] // several accessors are platform-conditional
impl SecureInputState {
    pub fn new() -> Self {
        Self {
            suppress: AtomicBool::new(false),
            sources: Mutex::new(Sources::default()),
        }
    }

    /// Hot path: called by the input backend per key event.
    #[inline]
    pub fn should_suppress_keys(&self) -> bool {
        self.suppress.load(Ordering::Relaxed)
    }

    /// Human-readable reason for the current suppression, if any.
    pub fn reason(&self) -> Option<String> {
        self.sources.lock().unwrap().reason.clone()
    }

    /// Update the AT-SPI-derived signal (a focused password field).
    #[cfg(target_os = "linux")]
    pub fn set_atspi_secure(&self, secure: bool, reason: Option<String>) -> Transition {
        let mut s = self.sources.lock().unwrap();
        s.atspi_secure = secure;
        self.recompute(&mut s, reason)
    }

    /// Update the terminal-derived signal (a focused pty with ECHO disabled). P2.
    #[cfg(target_os = "linux")]
    pub fn set_terminal_secure(&self, secure: bool, reason: Option<String>) -> Transition {
        let mut s = self.sources.lock().unwrap();
        s.terminal_secure = secure;
        self.recompute(&mut s, reason)
    }

    #[cfg(target_os = "linux")]
    fn recompute(&self, s: &mut Sources, reason: Option<String>) -> Transition {
        let now = s.atspi_secure || s.terminal_secure;
        if now {
            if reason.is_some() {
                s.reason = reason;
            }
        } else {
            s.reason = None;
        }
        match (self.suppress.swap(now, Ordering::Relaxed), now) {
            (false, true) => Transition::Entered,
            (true, false) => Transition::Left,
            _ => Transition::Unchanged,
        }
    }
}

impl Default for SecureInputState {
    fn default() -> Self {
        Self::new()
    }
}

/// Launch secure-input gating. Linux: optionally enable system accessibility, then run
/// the AT-SPI focus listener. Other platforms: no-op.
#[cfg(target_os = "linux")]
pub fn spawn(
    state: std::sync::Arc<SecureInputState>,
    marker_tx: tokio::sync::mpsc::UnboundedSender<crate::data::InputEvent>,
    enable_accessibility: bool,
) {
    tokio::spawn(async move {
        if let Err(e) = atspi_gate::run(state, marker_tx, enable_accessibility).await {
            tracing::warn!("secure-input gate exited: {e:#}");
        }
    });
}

#[cfg(not(target_os = "linux"))]
pub fn spawn(
    _state: std::sync::Arc<SecureInputState>,
    _marker_tx: tokio::sync::mpsc::UnboundedSender<crate::data::InputEvent>,
    _enable_accessibility: bool,
) {
}
