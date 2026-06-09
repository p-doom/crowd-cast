//! evdev-based input capture backend for Linux Wayland
//! Requires user to be in the 'input' group

#[cfg(target_os = "linux")]
use crate::data::{
    EventType, InputEvent, KeyEvent, MouseButton, MouseButtonEvent, MouseMoveEvent,
    MouseScrollEvent,
};
#[cfg(target_os = "linux")]
use crate::input::secure::SecureInputState;
#[cfg(target_os = "linux")]
use crate::input::InputBackend;
#[cfg(target_os = "linux")]
use anyhow::{Context, Result};
#[cfg(target_os = "linux")]
use evdev::{Device, InputEventKind, Key};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::Instant;
#[cfg(target_os = "linux")]
use tokio::sync::mpsc;
#[cfg(target_os = "linux")]
use tracing::{debug, error, info, warn};

#[cfg(target_os = "linux")]
pub struct EvdevBackend {
    devices: Vec<Device>,
    capturing: Arc<AtomicBool>,
    /// Secure-input gate: when set, key events are withheld (e.g. focused password field).
    secure: Arc<SecureInputState>,
    /// The instant when the backend was started, used for timestamp calculation
    start_time: Option<Instant>,
}

#[cfg(target_os = "linux")]
impl EvdevBackend {
    /// Create a new evdev backend
    /// This will enumerate input devices and filter for keyboards and mice
    pub fn new(secure: Arc<SecureInputState>) -> Result<Self> {
        let mut devices = Vec::new();

        // Enumerate all input devices
        for entry in std::fs::read_dir("/dev/input")? {
            let entry = entry?;
            let path = entry.path();

            if !path.to_string_lossy().contains("event") {
                continue;
            }

            match Device::open(&path) {
                Ok(device) => {
                    let name = device.name().unwrap_or("Unknown");
                    let has_keys = device.supported_keys().is_some();
                    let has_rel = device.supported_relative_axes().is_some();

                    // Include keyboards and mice
                    if has_keys || has_rel {
                        info!("Found input device: {} ({:?})", name, path);
                        devices.push(device);
                    }
                }
                Err(e) => {
                    debug!("Could not open {:?}: {}", path, e);
                }
            }
        }

        if devices.is_empty() {
            anyhow::bail!("No input devices found. Make sure you are in the 'input' group.");
        }

        Ok(Self {
            devices,
            capturing: Arc::new(AtomicBool::new(false)),
            secure,
            start_time: None,
        })
    }
}

/// Translates a stream of evdev events into the unified `EventType` schema shared with the
/// macOS backend. Relative motion/scroll are accumulated and flushed as a single combined
/// event per `SYN_REPORT` (matching macOS' one-MouseMove-per-motion); keys and pointer
/// buttons are emitted immediately. `suppress_keys` withholds keystrokes for secure-input
/// gating but never pointer buttons.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct EventCoalescer {
    dx: f64,
    dy: f64,
    scroll_x: i64,
    scroll_y: i64,
}

#[cfg(target_os = "linux")]
impl EventCoalescer {
    fn feed(
        &mut self,
        kind: InputEventKind,
        value: i32,
        suppress_keys: bool,
        out: &mut Vec<EventType>,
    ) {
        use evdev::RelativeAxisType;
        match kind {
            InputEventKind::Key(key) => {
                // Pointer buttons (BTN_*) arrive as Key events; route them to mouse events.
                // Buttons are never gated by secure-input (matches macOS, where clicks aren't
                // withheld for a focused password field).
                if let Some(button) = MouseButton::from_evdev_key(key) {
                    let be = MouseButtonEvent { button, x: 0.0, y: 0.0 };
                    match value {
                        1 => out.push(EventType::MousePress(be)),
                        0 => out.push(EventType::MouseRelease(be)),
                        _ => {}
                    }
                } else if suppress_keys {
                    // Withhold keystrokes while a secure context is active.
                } else {
                    let ke = KeyEvent::from(key);
                    match value {
                        1 => out.push(EventType::KeyPress(ke)),
                        0 => out.push(EventType::KeyRelease(ke)),
                        _ => {} // key repeat (value == 2)
                    }
                }
            }
            InputEventKind::RelAxis(axis) => match axis {
                RelativeAxisType::REL_X => self.dx += value as f64,
                RelativeAxisType::REL_Y => self.dy += value as f64,
                RelativeAxisType::REL_WHEEL => self.scroll_y += value as i64,
                RelativeAxisType::REL_HWHEEL => self.scroll_x += value as i64,
                _ => {}
            },
            // SYN_REPORT delimits one device packet: flush accumulated motion/scroll as
            // single combined events, then reset.
            InputEventKind::Synchronization(_) => {
                if self.dx != 0.0 || self.dy != 0.0 {
                    out.push(EventType::MouseMove(MouseMoveEvent {
                        delta_x: self.dx,
                        delta_y: self.dy,
                    }));
                    self.dx = 0.0;
                    self.dy = 0.0;
                }
                if self.scroll_x != 0 || self.scroll_y != 0 {
                    out.push(EventType::MouseScroll(MouseScrollEvent {
                        delta_x: self.scroll_x,
                        delta_y: self.scroll_y,
                        x: 0.0,
                        y: 0.0,
                    }));
                    self.scroll_x = 0;
                    self.scroll_y = 0;
                }
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl InputBackend for EvdevBackend {
    fn start(&mut self, tx: mpsc::UnboundedSender<InputEvent>) -> Result<()> {
        if self.capturing.load(Ordering::SeqCst) {
            return Ok(());
        }

        self.capturing.store(true, Ordering::SeqCst);
        let start_time = Instant::now();
        self.start_time = Some(start_time);

        // Take ownership of devices for the threads
        let devices = std::mem::take(&mut self.devices);

        for mut device in devices {
            let tx = tx.clone();
            let capturing = self.capturing.clone();
            let secure = self.secure.clone();
            let start_time = start_time;

            let handle = thread::spawn(move || {
                let device_name = device.name().unwrap_or("Unknown").to_string();
                info!("Started evdev capture for: {}", device_name);

                // Translate evdev events into the unified, macOS-matching schema via
                // EventCoalescer (motion/scroll combined per SYN_REPORT; keys/buttons immediate).
                let mut coalescer = EventCoalescer::default();
                let mut out: Vec<EventType> = Vec::with_capacity(4);

                loop {
                    if !capturing.load(Ordering::SeqCst) {
                        break;
                    }

                    match device.fetch_events() {
                        Ok(events) => {
                            for ev in events {
                                let timestamp_us = start_time.elapsed().as_micros() as u64;
                                out.clear();
                                coalescer.feed(
                                    ev.kind(),
                                    ev.value(),
                                    secure.should_suppress_keys(),
                                    &mut out,
                                );
                                for event in out.drain(..) {
                                    if let Err(e) = tx.send(InputEvent { timestamp_us, event }) {
                                        debug!("Failed to send input event: {}", e);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("evdev fetch error for {}: {}", device_name, e);
                            thread::sleep(std::time::Duration::from_millis(100));
                        }
                    }
                }

                info!("Stopped evdev capture for: {}", device_name);
            });

            let _ = handle;
        }

        Ok(())
    }

    fn stop(&mut self) {
        self.capturing.store(false, Ordering::SeqCst);
    }

    fn current_timestamp(&self) -> Option<u64> {
        self.start_time.map(|t| t.elapsed().as_micros() as u64)
    }
}


#[cfg(all(test, target_os = "linux"))]
mod coalescer_tests {
    use super::*;
    use evdev::{InputEventKind, Key, RelativeAxisType, Synchronization};

    fn syn() -> InputEventKind {
        InputEventKind::Synchronization(Synchronization::SYN_REPORT)
    }

    // A motion packet (REL_X, REL_Y, SYN_REPORT) must coalesce into ONE MouseMove carrying
    // both axes -- matching macOS, not two split-axis events.
    #[test]
    fn diagonal_motion_coalesces_to_one_event() {
        let mut c = EventCoalescer::default();
        let mut out = Vec::new();
        c.feed(InputEventKind::RelAxis(RelativeAxisType::REL_X), 7, false, &mut out);
        c.feed(InputEventKind::RelAxis(RelativeAxisType::REL_Y), 4, false, &mut out);
        assert!(out.is_empty(), "nothing emitted before SYN_REPORT");
        c.feed(syn(), 0, false, &mut out);
        assert_eq!(out.len(), 1, "exactly one combined event per packet");
        match &out[0] {
            EventType::MouseMove(m) => {
                assert_eq!(m.delta_x, 7.0);
                assert_eq!(m.delta_y, 4.0);
            }
            other => panic!("expected combined MouseMove, got {:?}", other),
        }
    }

    #[test]
    fn key_emitted_immediately_with_macos_code() {
        let mut c = EventCoalescer::default();
        let mut out = Vec::new();
        c.feed(InputEventKind::Key(Key::KEY_A), 1, false, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            EventType::KeyPress(k) => {
                assert_eq!(k.code, 64);
                assert_eq!(k.name, "KeyA");
            }
            other => panic!("expected KeyPress, got {:?}", other),
        }
    }

    #[test]
    fn secure_gate_withholds_keys_not_buttons() {
        let mut c = EventCoalescer::default();
        let mut out = Vec::new();
        c.feed(InputEventKind::Key(Key::KEY_A), 1, true, &mut out);
        assert!(out.is_empty(), "keystroke withheld under secure gate");
        c.feed(InputEventKind::Key(Key::BTN_LEFT), 1, true, &mut out);
        assert_eq!(out.len(), 1, "pointer buttons are never gated");
        assert!(matches!(out[0], EventType::MousePress(_)));
    }

    #[test]
    fn scroll_coalesces_on_syn() {
        let mut c = EventCoalescer::default();
        let mut out = Vec::new();
        c.feed(InputEventKind::RelAxis(RelativeAxisType::REL_WHEEL), -1, false, &mut out);
        assert!(out.is_empty());
        c.feed(syn(), 0, false, &mut out);
        assert_eq!(out.len(), 1);
        match &out[0] {
            EventType::MouseScroll(s) => {
                assert_eq!(s.delta_y, -1);
                assert_eq!(s.delta_x, 0);
            }
            other => panic!("expected MouseScroll, got {:?}", other),
        }
    }
}
