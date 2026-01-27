//! Input capture backend trait

use crate::data::InputEvent;
use anyhow::Result;
use tokio::sync::mpsc;

/// Trait for input capture backends
pub trait InputBackend: Send + Sync {
    /// Start capturing input events
    /// Events are sent to the provided channel
    fn start(&mut self, tx: mpsc::UnboundedSender<InputEvent>) -> Result<()>;
    
    /// Get the current timestamp in microseconds since the backend started.
    /// Returns None if the backend hasn't been started yet.
    /// This is used to synchronize input events with video recording start time.
    fn current_timestamp(&self) -> Option<u64>;
}

/// Create the appropriate input backend for the current platform
pub fn create_input_backend() -> Box<dyn InputBackend> {
    #[cfg(target_os = "linux")]
    {
        // Check if we're on Wayland
        if std::env::var("XDG_SESSION_TYPE").map(|s| s == "wayland").unwrap_or(false) {
            // Try evdev backend for Wayland
            if let Ok(backend) = super::evdev_backend::EvdevBackend::new() {
                tracing::info!("Using evdev backend for Wayland");
                return Box::new(backend);
            }
            tracing::warn!("evdev backend failed, falling back to rdev (may not work on Wayland)");
        }
    }
    
    // Default to rdev backend
    tracing::info!("Using rdev backend for input capture");
    Box::new(super::rdev_backend::RdevBackend::new())
}
