//! Input capture backend trait

use crate::data::InputEvent;
use crate::input::secure::SecureInputState;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Trait for input capture backends
pub trait InputBackend: Send + Sync {
    /// Start capturing input events
    /// Events are sent to the provided channel
    fn start(&mut self, tx: mpsc::UnboundedSender<InputEvent>) -> Result<()>;

    /// Stop capturing input events.
    /// Should be called before process exit to allow the event tap to drain cleanly.
    fn stop(&mut self);

    /// Get the current timestamp in microseconds since the backend started.
    /// Returns None if the backend hasn't been started yet.
    /// This is used to synchronize input events with video recording start time.
    fn current_timestamp(&self) -> Option<u64>;
}

/// Create the appropriate input backend for the current platform.
///
/// Linux uses evdev for both X11 and Wayland: raw pre-acceleration deltas, reaches the
/// same input layer raw-input consumers read, and works regardless of display server.
/// rdev is not linked on Linux (see Cargo.toml). macOS/Windows use rdev.
pub fn create_input_backend(secure: Arc<SecureInputState>) -> Box<dyn InputBackend> {
    #[cfg(target_os = "linux")]
    {
        match super::evdev_backend::EvdevBackend::new(secure) {
            Ok(backend) => {
                tracing::info!("Using evdev backend for input capture");
                return Box::new(backend);
            }
            Err(e) => {
                tracing::error!("evdev backend init failed ({e}); input capture disabled -- ensure the user is in the 'input' group. Screen capture is unaffected.");
                return Box::new(NoOpBackend);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Secure-input gating is Linux-only; macOS/Windows rely on OS facilities
        // (e.g. macOS Secure Event Input), so the shared gate is inert here.
        let _ = secure;
        tracing::info!("Using rdev backend for input capture");
        Box::new(super::rdev_backend::RdevBackend::new())
    }
}

/// No-op input backend used on Linux only when evdev initialization fails, so the rest of
/// the app (screen capture) keeps running without input capture.
#[cfg(target_os = "linux")]
struct NoOpBackend;

#[cfg(target_os = "linux")]
impl InputBackend for NoOpBackend {
    fn start(&mut self, _tx: mpsc::UnboundedSender<InputEvent>) -> Result<()> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn current_timestamp(&self) -> Option<u64> {
        None
    }
}
