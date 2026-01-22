//! rdev-based input capture backend
//! Works on Windows, macOS, and Linux (X11)

use crate::data::{EventType, InputEvent, KeyEvent, MouseButton, MouseButtonEvent, MouseMoveEvent, MouseScrollEvent};
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
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl RdevBackend {
    /// Create a new rdev backend
    pub fn new() -> Self {
        Self {
            capturing: Arc::new(AtomicBool::new(false)),
            thread_handle: None,
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

        let handle = thread::spawn(move || {
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
                    rdev::EventType::MouseMove { x, y } => {
                        Some(EventType::MouseMove(MouseMoveEvent { x, y }))
                    }
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

        self.thread_handle = Some(handle);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.capturing.store(false, Ordering::SeqCst);

        // Note: rdev::listen() doesn't have a clean way to stop from another thread
        // The thread will exit when the process exits or when we forcefully terminate it
        // For now, we just set the flag and let events be dropped

        if let Some(handle) = self.thread_handle.take() {
            // We can't cleanly join because rdev::listen blocks indefinitely
            // Just drop the handle
            drop(handle);
        }

        info!("rdev backend stop requested");
        Ok(())
    }

    fn is_capturing(&self) -> bool {
        self.capturing.load(Ordering::SeqCst)
    }
}
