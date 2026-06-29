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
use anyhow::Result;
#[cfg(target_os = "linux")]
use evdev::{Device, InputEventKind};
#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};
#[cfg(target_os = "linux")]
use tokio::sync::mpsc;
#[cfg(target_os = "linux")]
use tracing::{debug, info, warn};

/// Directory holding the per-device event nodes we capture from.
#[cfg(target_os = "linux")]
const DEVICE_DIR: &str = "/dev/input";

/// How often the hotplug watcher rescans `DEVICE_DIR` for devices that appeared after
/// startup (USB/Bluetooth plug-in, or re-enumeration after suspend/resume). Human hotplug
/// doesn't need sub-second latency, and a single `read_dir` per tick is negligible.
#[cfg(target_os = "linux")]
const HOTPLUG_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// `true` if a device node disappeared underneath us. evdev reads return `ENODEV` once the
/// kernel removes the node — on unplug, or when suspend/resume re-enumerates and invalidates
/// the open fd. The owning capture thread treats this as terminal and exits so the hotplug
/// watcher can re-adopt the device when its node reappears.
#[cfg(target_os = "linux")]
fn is_device_disconnected(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(libc::ENODEV)
}

#[cfg(target_os = "linux")]
fn is_event_node(path: &Path) -> bool {
    path.to_string_lossy().contains("event")
}

/// Open a `/dev/input` node and keep it only if it looks like a keyboard or mouse. Returns
/// the device name alongside the handle. Shared by startup enumeration and the hotplug
/// watcher so both apply identical filtering — there is one codepath for "adopt a device".
#[cfg(target_os = "linux")]
fn open_input_device(path: &Path) -> Option<(String, Device)> {
    match Device::open(path) {
        Ok(device) => {
            let has_keys = device.supported_keys().is_some();
            let has_rel = device.supported_relative_axes().is_some();
            if has_keys || has_rel {
                let name = device.name().unwrap_or("Unknown").to_string();
                Some((name, device))
            } else {
                None
            }
        }
        Err(e) => {
            debug!("Could not open {:?}: {}", path, e);
            None
        }
    }
}

/// Set of device paths currently owned by a live capture thread. The hotplug watcher adds a
/// path before spawning its thread and skips anything already present; the capture thread
/// removes its path on exit, so a device that disconnects and returns (same node) is
/// re-adopted on the next watcher tick.
#[cfg(target_os = "linux")]
type ActiveDevices = Arc<Mutex<HashSet<PathBuf>>>;

#[cfg(target_os = "linux")]
pub struct EvdevBackend {
    devices: Vec<(PathBuf, Device)>,
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

        // Enumerate all input devices present at startup. Devices that appear later are
        // picked up by the hotplug watcher spawned in `start()`.
        for entry in std::fs::read_dir(DEVICE_DIR)? {
            let entry = entry?;
            let path = entry.path();

            if !is_event_node(&path) {
                continue;
            }

            if let Some((name, device)) = open_input_device(&path) {
                info!("Found input device: {} ({:?})", name, path);
                devices.push((path, device));
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

/// Run the read loop for a single device on its own thread, forwarding coalesced events to
/// `tx` until capture stops or the device disconnects. On disconnect the path is removed from
/// `active` so the hotplug watcher can re-adopt the device if it returns.
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn spawn_capture_thread(
    path: PathBuf,
    mut device: Device,
    tx: mpsc::UnboundedSender<InputEvent>,
    capturing: Arc<AtomicBool>,
    secure: Arc<SecureInputState>,
    start_time: Instant,
    active: ActiveDevices,
) {
    thread::spawn(move || {
        let device_name = device.name().unwrap_or("Unknown").to_string();
        info!("Started evdev capture for: {} ({:?})", device_name, path);

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
                            if let Err(e) = tx.send(InputEvent {
                                timestamp_us,
                                event,
                            }) {
                                debug!("Failed to send input event: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    // A removed node (unplug, or suspend/resume re-enumeration) is terminal for
                    // this fd: exit so the watcher re-adopts the device when it returns. Other
                    // errors are treated as transient and retried.
                    if is_device_disconnected(&e) {
                        info!("Input device disconnected: {} ({:?})", device_name, path);
                        break;
                    }
                    warn!("evdev fetch error for {}: {}", device_name, e);
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }

        active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&path);
        info!("Stopped evdev capture for: {}", device_name);
    });
}

/// Poll `DEVICE_DIR` for keyboards/mice that appear after startup and bring them under
/// capture. This is what makes capture survive plug-in of a new device and the device
/// re-enumeration that follows a suspend/resume cycle (the old fds die with `ENODEV`, their
/// capture threads exit, and the freshly created nodes are adopted here).
#[cfg(target_os = "linux")]
fn spawn_hotplug_watcher(
    tx: mpsc::UnboundedSender<InputEvent>,
    capturing: Arc<AtomicBool>,
    secure: Arc<SecureInputState>,
    start_time: Instant,
    active: ActiveDevices,
) {
    thread::spawn(move || {
        info!("Started evdev hotplug watcher");
        while capturing.load(Ordering::SeqCst) {
            thread::sleep(HOTPLUG_POLL_INTERVAL);
            if !capturing.load(Ordering::SeqCst) {
                break;
            }

            let entries = match std::fs::read_dir(DEVICE_DIR) {
                Ok(entries) => entries,
                Err(e) => {
                    warn!("hotplug watcher: cannot read {}: {}", DEVICE_DIR, e);
                    continue;
                }
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if !is_event_node(&path) {
                    continue;
                }
                // Already captured (this is the common case every tick) — skip before the
                // relatively expensive open. The watcher is the only adder, so a plain
                // contains-check then insert cannot race into a double-spawn.
                if active
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .contains(&path)
                {
                    continue;
                }

                if let Some((name, device)) = open_input_device(&path) {
                    info!("Hotplugged input device: {} ({:?})", name, path);
                    active
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(path.clone());
                    spawn_capture_thread(
                        path,
                        device,
                        tx.clone(),
                        capturing.clone(),
                        secure.clone(),
                        start_time,
                        active.clone(),
                    );
                }
            }
        }
        info!("Stopped evdev hotplug watcher");
    });
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
                    let be = MouseButtonEvent {
                        button,
                        x: 0.0,
                        y: 0.0,
                    };
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

        // Tracks which device paths currently have a live capture thread; shared with the
        // hotplug watcher so it never double-adopts and so disconnected devices can return.
        let active: ActiveDevices = Arc::new(Mutex::new(HashSet::new()));

        // Spawn a capture thread per device enumerated at startup. Register each path before
        // spawning the watcher so its first tick treats them as already-owned.
        let devices = std::mem::take(&mut self.devices);
        for (path, device) in devices {
            active
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(path.clone());
            spawn_capture_thread(
                path,
                device,
                tx.clone(),
                self.capturing.clone(),
                self.secure.clone(),
                start_time,
                active.clone(),
            );
        }

        // Adopt devices that appear later (plug-in, suspend/resume re-enumeration).
        spawn_hotplug_watcher(
            tx,
            self.capturing.clone(),
            self.secure.clone(),
            start_time,
            active,
        );

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

    // ENODEV (the node vanished: unplug, or suspend/resume re-enumeration) must be classified
    // as terminal so the capture thread exits and the watcher can re-adopt the device.
    #[test]
    fn enodev_is_treated_as_disconnect() {
        let err = std::io::Error::from_raw_os_error(libc::ENODEV);
        assert!(is_device_disconnected(&err));
    }

    // Transient/spurious errors must NOT be mistaken for a disconnect, or a still-present
    // device would be dropped and never recaptured (the watcher only adds *new* nodes).
    #[test]
    fn transient_and_non_os_errors_are_not_disconnect() {
        assert!(!is_device_disconnected(&std::io::Error::from_raw_os_error(
            libc::EAGAIN
        )));
        assert!(!is_device_disconnected(&std::io::Error::from_raw_os_error(
            libc::EINTR
        )));
        assert!(!is_device_disconnected(&std::io::Error::new(
            std::io::ErrorKind::Other,
            "no errno",
        )));
    }

    // A motion packet (REL_X, REL_Y, SYN_REPORT) must coalesce into ONE MouseMove carrying
    // both axes -- matching macOS, not two split-axis events.
    #[test]
    fn diagonal_motion_coalesces_to_one_event() {
        let mut c = EventCoalescer::default();
        let mut out = Vec::new();
        c.feed(
            InputEventKind::RelAxis(RelativeAxisType::REL_X),
            7,
            false,
            &mut out,
        );
        c.feed(
            InputEventKind::RelAxis(RelativeAxisType::REL_Y),
            4,
            false,
            &mut out,
        );
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
        c.feed(
            InputEventKind::RelAxis(RelativeAxisType::REL_WHEEL),
            -1,
            false,
            &mut out,
        );
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

// Live hotplug test driving a real uinput virtual device against the real EvdevBackend.
// Ignored by default: it needs rw on /dev/uinput and the user in the 'input' group.
// Run with:  cargo test --bin crowd-cast-agent -- --ignored hotplug
#[cfg(all(test, target_os = "linux"))]
mod hotplug_live_tests {
    use super::*;
    use crate::input::secure::SecureInputState;
    use crate::input::InputBackend;
    use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
    use evdev::{AttributeSet, EventType as EvType, InputEvent as EvInputEvent, Key};

    fn make_virtual_keyboard(name: &str) -> VirtualDevice {
        let mut keys = AttributeSet::<Key>::new();
        keys.insert(Key::KEY_A);
        VirtualDeviceBuilder::new()
            .expect("open /dev/uinput (needs rw access)")
            .name(name)
            .with_keys(&keys)
            .expect("declare KEY_A")
            .build()
            .expect("build virtual keyboard")
    }

    /// Emit KEY_A press+release from the virtual device repeatedly until a KeyA KeyPress is seen
    /// on the capture channel, or `timeout` elapses. A `true` return means the watcher adopted
    /// the device and its events flow through the unified pipeline.
    fn captures_within(
        rx: &mut mpsc::UnboundedReceiver<InputEvent>,
        dev: &mut VirtualDevice,
        timeout: Duration,
    ) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            dev.emit(&[EvInputEvent::new(EvType::KEY, Key::KEY_A.code(), 1)])
                .expect("emit press");
            dev.emit(&[EvInputEvent::new(EvType::KEY, Key::KEY_A.code(), 0)])
                .expect("emit release");
            while let Ok(ev) = rx.try_recv() {
                if let EventType::KeyPress(k) = ev.event {
                    if k.name == "KeyA" {
                        return true;
                    }
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    #[ignore = "needs /dev/uinput rw + 'input' group"]
    fn hotplugged_device_is_captured_and_readopted_after_disconnect() {
        let secure = Arc::new(SecureInputState::new());
        let mut backend = EvdevBackend::new(secure).expect("enumerate input devices");
        let (tx, mut rx) = mpsc::unbounded_channel();
        backend.start(tx).expect("start backend");

        // 1) Plug in a brand-new device after start(): the watcher must adopt it and pipe its keys.
        let mut vkbd = make_virtual_keyboard("crowd-cast-hotplug-test-1");
        assert!(
            captures_within(&mut rx, &mut vkbd, Duration::from_secs(5)),
            "watcher never captured the hotplugged virtual keyboard"
        );

        // 2) Unplug: dropping closes uinput, the node vanishes, the capture thread sees ENODEV,
        //    exits, and releases its path.
        drop(vkbd);
        thread::sleep(Duration::from_secs(2));

        // 3) Re-plug (typically the same event node): only succeeds if the path was released on
        //    disconnect, so this proves both adoption AND disconnect cleanup.
        let mut vkbd2 = make_virtual_keyboard("crowd-cast-hotplug-test-2");
        assert!(
            captures_within(&mut rx, &mut vkbd2, Duration::from_secs(5)),
            "watcher did not re-adopt a device after a prior disconnect"
        );

        backend.stop();
        drop(vkbd2);
    }
}
