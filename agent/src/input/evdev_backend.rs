//! evdev-based input capture backend for Linux Wayland
//! Requires user to be in the 'input' group

#[cfg(target_os = "linux")]
use crate::data::{EventType, InputEvent, KeyEvent, MouseButton, MouseButtonEvent, MouseMoveEvent, MouseScrollEvent};
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
    /// The instant when the backend was started, used for timestamp calculation
    start_time: Option<Instant>,
}

#[cfg(target_os = "linux")]
impl EvdevBackend {
    /// Create a new evdev backend
    /// This will enumerate input devices and filter for keyboards and mice
    pub fn new() -> Result<Self> {
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
            start_time: None,
        })
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
            let start_time = start_time;
            
            let handle = thread::spawn(move || {
                let device_name = device.name().unwrap_or("Unknown").to_string();
                info!("Started evdev capture for: {}", device_name);
                
                loop {
                    if !capturing.load(Ordering::SeqCst) {
                        break;
                    }
                    
                    // Fetch events with timeout
                    match device.fetch_events() {
                        Ok(events) => {
                            for ev in events {
                                let timestamp_us = start_time.elapsed().as_micros() as u64;
                                
                                let event_type = match ev.kind() {
                                    InputEventKind::Key(key) => {
                                        let key_event = KeyEvent {
                                            code: key.0 as u32,
                                            name: format!("{:?}", key),
                                        };
                                        
                                        if ev.value() == 1 {
                                            Some(EventType::KeyPress(key_event))
                                        } else if ev.value() == 0 {
                                            Some(EventType::KeyRelease(key_event))
                                        } else {
                                            None // Key repeat, ignore
                                        }
                                    }
                                    InputEventKind::RelAxis(axis) => {
                                        use evdev::RelativeAxisType;
                                        match axis {
                                            // Emit raw delta values directly (true relative motion)
                                            RelativeAxisType::REL_X => {
                                                Some(EventType::MouseMove(MouseMoveEvent {
                                                    delta_x: ev.value() as f64,
                                                    delta_y: 0.0,
                                                }))
                                            }
                                            RelativeAxisType::REL_Y => {
                                                Some(EventType::MouseMove(MouseMoveEvent {
                                                    delta_x: 0.0,
                                                    delta_y: ev.value() as f64,
                                                }))
                                            }
                                            RelativeAxisType::REL_WHEEL => {
                                                Some(EventType::MouseScroll(MouseScrollEvent {
                                                    delta_x: 0,
                                                    delta_y: ev.value() as i64,
                                                    x: 0.0,
                                                    y: 0.0,
                                                }))
                                            }
                                            RelativeAxisType::REL_HWHEEL => {
                                                Some(EventType::MouseScroll(MouseScrollEvent {
                                                    delta_x: ev.value() as i64,
                                                    delta_y: 0,
                                                    x: 0.0,
                                                    y: 0.0,
                                                }))
                                            }
                                            _ => None,
                                        }
                                    }
                                    _ => None,
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
    
    fn current_timestamp(&self) -> Option<u64> {
        self.start_time.map(|t| t.elapsed().as_micros() as u64)
    }
}
