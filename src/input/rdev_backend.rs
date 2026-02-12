//! rdev-based input capture backend
//! Works on Windows, macOS, and Linux (X11)

use crate::data::{
    EventType, InputEvent, KeyEvent, MouseButton, MouseButtonEvent, MouseMoveEvent,
    MouseScrollEvent,
};
use crate::input::InputBackend;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// rdev-based input capture backend
pub struct RdevBackend {
    capturing: Arc<AtomicBool>,
    /// The instant when the backend was started, used for timestamp calculation
    start_time: Option<Instant>,
}

impl RdevBackend {
    /// Create a new rdev backend
    pub fn new() -> Self {
        Self {
            capturing: Arc::new(AtomicBool::new(false)),
            start_time: None,
        }
    }
}

impl Default for RdevBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl InputBackend for RdevBackend {
    fn start(&mut self, tx: mpsc::UnboundedSender<InputEvent>) -> Result<()> {
        if self.capturing.load(Ordering::SeqCst) {
            return Ok(()); // Already capturing
        }

        self.capturing.store(true, Ordering::SeqCst);
        let capturing = self.capturing.clone();
        let start_time = Instant::now();
        self.start_time = Some(start_time);

        let handle = thread::spawn(move || {
            // CRITICAL: Tell rdev we're NOT on the main thread so it dispatches
            // TSM (Text Services Manager) API calls to the main thread via GCD.
            // Without this, calling TISGetInputSourceProperty from the event tap
            // thread causes a crash due to dispatch_assert_queue_fail.
            rdev::set_is_main_thread(false);

            info!("rdev input capture started");

            let callback = move |event: rdev::Event| {
                if !capturing.load(Ordering::SeqCst) {
                    return;
                }

                let timestamp_us = start_time.elapsed().as_micros() as u64;

                let event_type = match event.event_type {
                    rdev::EventType::KeyPress(key) => {
                        Some(EventType::KeyPress(KeyEvent::from(key)))
                    }
                    rdev::EventType::KeyRelease(key) => {
                        Some(EventType::KeyRelease(KeyEvent::from(key)))
                    }
                    rdev::EventType::ButtonPress(button) => {
                        // Get current mouse position from the event
                        Some(EventType::MousePress(MouseButtonEvent {
                            button: MouseButton::from(button),
                            x: 0.0, // rdev doesn't provide position with button events
                            y: 0.0,
                        }))
                    }
                    rdev::EventType::ButtonRelease(button) => {
                        Some(EventType::MouseRelease(MouseButtonEvent {
                            button: MouseButton::from(button),
                            x: 0.0,
                            y: 0.0,
                        }))
                    }
                    rdev::EventType::MouseMove {
                        delta_x, delta_y, ..
                    } => Some(EventType::MouseMove(MouseMoveEvent { delta_x, delta_y })),
                    rdev::EventType::Wheel { delta_x, delta_y } => {
                        Some(EventType::MouseScroll(MouseScrollEvent {
                            delta_x,
                            delta_y,
                            x: 0.0,
                            y: 0.0,
                        }))
                    }
                };

                if let Some(event_type) = event_type {
                    let input_event = InputEvent {
                        timestamp_us,
                        event: event_type,
                    };

                    if let Err(e) = tx.send(input_event) {
                        debug!("Failed to send input event: {}", e);
                    }
                }
            };

            // Run the event listener
            if let Err(e) = rdev::listen(callback) {
                error!("rdev listen error: {:?}", e);
            }

            info!("rdev input capture stopped");
        });

        let _ = handle;
        Ok(())
    }

    fn current_timestamp(&self) -> Option<u64> {
        self.start_time.map(|t| t.elapsed().as_micros() as u64)
    }
}
