//! Input event data structures

use serde::{Deserialize, Serialize};

/// Serialized app_id used when recording is active but the frontmost app is filtered out.
pub const UNCAPTURED_APP_ID: &str = "UNCAPTURED";
/// Serialized app_id used when the frontmost app cannot be determined.
pub const UNKNOWN_APP_ID: &str = "UNKNOWN";

/// A single input event (keyboard or mouse)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputEvent {
    /// Timestamp in microseconds since session start
    pub timestamp_us: u64,

    /// The type of event
    pub event: EventType,
}

/// Type of input event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EventType {
    /// Frontmost application context changed
    ContextChanged(ContextEvent),

    /// Key press event
    KeyPress(KeyEvent),

    /// Key release event
    KeyRelease(KeyEvent),

    /// Mouse button press
    MousePress(MouseButtonEvent),

    /// Mouse button release
    MouseRelease(MouseButtonEvent),

    /// Mouse movement
    MouseMove(MouseMoveEvent),

    /// Mouse scroll
    MouseScroll(MouseScrollEvent),

    /// Segment metadata (emitted once at the start of each segment)
    Metadata(MetadataEvent),

    /// A span of input withheld from capture for privacy (e.g. a focused password
    /// field). Carries no key content; marks where suppression began so post-processing
    /// sees a labeled gap rather than a silent hole.
    Redacted(RedactedEvent),
}

/// Frontmost application context at a point in time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEvent {
    /// Bundle identifier / process name, or one of the sentinel app_id constants above
    pub app_id: String,
}

/// Keyboard event data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEvent {
    /// Key code (platform-specific)
    pub code: u32,

    /// Key name (e.g., "KeyA", "Enter", "ShiftLeft")
    pub name: String,
}

/// Mouse button event data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseButtonEvent {
    /// Button identifier
    pub button: MouseButton,

    /// X coordinate at time of click
    pub x: f64,

    /// Y coordinate at time of click
    pub y: f64,
}

/// Mouse button identifier
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u8),
}

/// Mouse movement event data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseMoveEvent {
    /// Relative X movement (device units, true delta on supported platforms)
    pub delta_x: f64,

    /// Relative Y movement (device units, true delta on supported platforms)
    pub delta_y: f64,
}

/// Mouse scroll event data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseScrollEvent {
    /// Horizontal scroll delta
    pub delta_x: i64,

    /// Vertical scroll delta
    pub delta_y: i64,

    /// X coordinate at time of scroll
    pub x: f64,

    /// Y coordinate at time of scroll
    pub y: f64,
}

/// Segment metadata (display resolution, timing, video output dimensions)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataEvent {
    /// Native display width in logical points
    pub display_width: u32,

    /// Native display height in logical points
    pub display_height: u32,

    /// Video output width in pixels (after downscale)
    pub output_width: u32,

    /// Video output height in pixels (after downscale)
    pub output_height: u32,

    /// UTC timestamp when this segment started (ISO 8601)
    pub timestamp_utc: String,
}

/// Marker emitted when secure-input gating begins withholding key events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedEvent {
    /// Why capture was suppressed (e.g. "secure-field").
    pub reason: String,
}

#[cfg(not(target_os = "linux"))]
impl From<rdev::Key> for KeyEvent {
    fn from(key: rdev::Key) -> Self {
        let (code, name) = match key {
            rdev::Key::Alt => (0, "Alt".to_string()),
            rdev::Key::AltGr => (1, "AltGr".to_string()),
            rdev::Key::Backspace => (2, "Backspace".to_string()),
            rdev::Key::CapsLock => (3, "CapsLock".to_string()),
            rdev::Key::ControlLeft => (4, "ControlLeft".to_string()),
            rdev::Key::ControlRight => (5, "ControlRight".to_string()),
            rdev::Key::Delete => (6, "Delete".to_string()),
            rdev::Key::DownArrow => (7, "DownArrow".to_string()),
            rdev::Key::End => (8, "End".to_string()),
            rdev::Key::Escape => (9, "Escape".to_string()),
            rdev::Key::F1 => (10, "F1".to_string()),
            rdev::Key::F2 => (11, "F2".to_string()),
            rdev::Key::F3 => (12, "F3".to_string()),
            rdev::Key::F4 => (13, "F4".to_string()),
            rdev::Key::F5 => (14, "F5".to_string()),
            rdev::Key::F6 => (15, "F6".to_string()),
            rdev::Key::F7 => (16, "F7".to_string()),
            rdev::Key::F8 => (17, "F8".to_string()),
            rdev::Key::F9 => (18, "F9".to_string()),
            rdev::Key::F10 => (19, "F10".to_string()),
            rdev::Key::F11 => (20, "F11".to_string()),
            rdev::Key::F12 => (21, "F12".to_string()),
            rdev::Key::F13 => (200, "F13".to_string()),
            rdev::Key::F14 => (201, "F14".to_string()),
            rdev::Key::F15 => (202, "F15".to_string()),
            rdev::Key::F16 => (203, "F16".to_string()),
            rdev::Key::F17 => (204, "F17".to_string()),
            rdev::Key::F18 => (205, "F18".to_string()),
            rdev::Key::F19 => (206, "F19".to_string()),
            rdev::Key::F20 => (207, "F20".to_string()),
            rdev::Key::F21 => (208, "F21".to_string()),
            rdev::Key::F22 => (209, "F22".to_string()),
            rdev::Key::F23 => (210, "F23".to_string()),
            rdev::Key::F24 => (211, "F24".to_string()),
            rdev::Key::Home => (22, "Home".to_string()),
            rdev::Key::LeftArrow => (23, "LeftArrow".to_string()),
            rdev::Key::MetaLeft => (24, "MetaLeft".to_string()),
            rdev::Key::MetaRight => (25, "MetaRight".to_string()),
            rdev::Key::PageDown => (26, "PageDown".to_string()),
            rdev::Key::PageUp => (27, "PageUp".to_string()),
            rdev::Key::Return => (28, "Return".to_string()),
            rdev::Key::RightArrow => (29, "RightArrow".to_string()),
            rdev::Key::ShiftLeft => (30, "ShiftLeft".to_string()),
            rdev::Key::ShiftRight => (31, "ShiftRight".to_string()),
            rdev::Key::Space => (32, "Space".to_string()),
            rdev::Key::Tab => (33, "Tab".to_string()),
            rdev::Key::UpArrow => (34, "UpArrow".to_string()),
            rdev::Key::PrintScreen => (35, "PrintScreen".to_string()),
            rdev::Key::ScrollLock => (36, "ScrollLock".to_string()),
            rdev::Key::Pause => (37, "Pause".to_string()),
            rdev::Key::NumLock => (38, "NumLock".to_string()),
            rdev::Key::BackQuote => (39, "BackQuote".to_string()),
            rdev::Key::Num1 => (40, "Num1".to_string()),
            rdev::Key::Num2 => (41, "Num2".to_string()),
            rdev::Key::Num3 => (42, "Num3".to_string()),
            rdev::Key::Num4 => (43, "Num4".to_string()),
            rdev::Key::Num5 => (44, "Num5".to_string()),
            rdev::Key::Num6 => (45, "Num6".to_string()),
            rdev::Key::Num7 => (46, "Num7".to_string()),
            rdev::Key::Num8 => (47, "Num8".to_string()),
            rdev::Key::Num9 => (48, "Num9".to_string()),
            rdev::Key::Num0 => (49, "Num0".to_string()),
            rdev::Key::Minus => (50, "Minus".to_string()),
            rdev::Key::Equal => (51, "Equal".to_string()),
            rdev::Key::KeyQ => (52, "KeyQ".to_string()),
            rdev::Key::KeyW => (53, "KeyW".to_string()),
            rdev::Key::KeyE => (54, "KeyE".to_string()),
            rdev::Key::KeyR => (55, "KeyR".to_string()),
            rdev::Key::KeyT => (56, "KeyT".to_string()),
            rdev::Key::KeyY => (57, "KeyY".to_string()),
            rdev::Key::KeyU => (58, "KeyU".to_string()),
            rdev::Key::KeyI => (59, "KeyI".to_string()),
            rdev::Key::KeyO => (60, "KeyO".to_string()),
            rdev::Key::KeyP => (61, "KeyP".to_string()),
            rdev::Key::LeftBracket => (62, "LeftBracket".to_string()),
            rdev::Key::RightBracket => (63, "RightBracket".to_string()),
            rdev::Key::KeyA => (64, "KeyA".to_string()),
            rdev::Key::KeyS => (65, "KeyS".to_string()),
            rdev::Key::KeyD => (66, "KeyD".to_string()),
            rdev::Key::KeyF => (67, "KeyF".to_string()),
            rdev::Key::KeyG => (68, "KeyG".to_string()),
            rdev::Key::KeyH => (69, "KeyH".to_string()),
            rdev::Key::KeyJ => (70, "KeyJ".to_string()),
            rdev::Key::KeyK => (71, "KeyK".to_string()),
            rdev::Key::KeyL => (72, "KeyL".to_string()),
            rdev::Key::SemiColon => (73, "SemiColon".to_string()),
            rdev::Key::Quote => (74, "Quote".to_string()),
            rdev::Key::BackSlash => (75, "BackSlash".to_string()),
            rdev::Key::IntlBackslash => (76, "IntlBackslash".to_string()),
            rdev::Key::KeyZ => (77, "KeyZ".to_string()),
            rdev::Key::KeyX => (78, "KeyX".to_string()),
            rdev::Key::KeyC => (79, "KeyC".to_string()),
            rdev::Key::KeyV => (80, "KeyV".to_string()),
            rdev::Key::KeyB => (81, "KeyB".to_string()),
            rdev::Key::KeyN => (82, "KeyN".to_string()),
            rdev::Key::KeyM => (83, "KeyM".to_string()),
            rdev::Key::Comma => (84, "Comma".to_string()),
            rdev::Key::Dot => (85, "Dot".to_string()),
            rdev::Key::Slash => (86, "Slash".to_string()),
            rdev::Key::Insert => (87, "Insert".to_string()),
            rdev::Key::KpReturn => (88, "KpReturn".to_string()),
            rdev::Key::KpMinus => (89, "KpMinus".to_string()),
            rdev::Key::KpPlus => (90, "KpPlus".to_string()),
            rdev::Key::KpMultiply => (91, "KpMultiply".to_string()),
            rdev::Key::KpDivide => (92, "KpDivide".to_string()),
            rdev::Key::Kp0 => (93, "Kp0".to_string()),
            rdev::Key::Kp1 => (94, "Kp1".to_string()),
            rdev::Key::Kp2 => (95, "Kp2".to_string()),
            rdev::Key::Kp3 => (96, "Kp3".to_string()),
            rdev::Key::Kp4 => (97, "Kp4".to_string()),
            rdev::Key::Kp5 => (98, "Kp5".to_string()),
            rdev::Key::Kp6 => (99, "Kp6".to_string()),
            rdev::Key::Kp7 => (100, "Kp7".to_string()),
            rdev::Key::Kp8 => (101, "Kp8".to_string()),
            rdev::Key::Kp9 => (102, "Kp9".to_string()),
            rdev::Key::KpDelete => (103, "KpDelete".to_string()),
            rdev::Key::Function => (104, "Function".to_string()),
            rdev::Key::VolumeUp => (220, "VolumeUp".to_string()),
            rdev::Key::VolumeDown => (221, "VolumeDown".to_string()),
            rdev::Key::VolumeMute => (222, "VolumeMute".to_string()),
            rdev::Key::BrightnessUp => (223, "BrightnessUp".to_string()),
            rdev::Key::BrightnessDown => (224, "BrightnessDown".to_string()),
            rdev::Key::PreviousTrack => (225, "PreviousTrack".to_string()),
            rdev::Key::PlayPause => (226, "PlayPause".to_string()),
            rdev::Key::PlayCd => (227, "PlayCd".to_string()),
            rdev::Key::NextTrack => (228, "NextTrack".to_string()),
            rdev::Key::Unknown(code) => (code as u32 + 1000, format!("Unknown({})", code)),
        };

        Self { code, name }
    }
}

#[cfg(not(target_os = "linux"))]
impl From<rdev::Button> for MouseButton {
    fn from(button: rdev::Button) -> Self {
        match button {
            rdev::Button::Left => MouseButton::Left,
            rdev::Button::Right => MouseButton::Right,
            rdev::Button::Middle => MouseButton::Middle,
            rdev::Button::Unknown(n) => MouseButton::Other(n),
        }
    }
}

#[cfg(target_os = "linux")]
impl From<evdev::Key> for KeyEvent {
    /// Map a Linux evdev key into the *same* curated `(code, name)` namespace the macOS
    /// rdev backend emits (see the `From<rdev::Key>` impl above), so recordings from both
    /// platforms share one keyboard vocabulary. evdev keycodes are position-based (a stable
    /// kernel ABI), independent of layout/DE/display server.
    ///
    /// Keys with no curated equivalent fall back to the raw kernel code:
    /// `(code + 1000, "Unknown(code)")` -- mirroring the macOS `Unknown` path -- so
    /// post-processing can still reconstruct the physical key. The +1000 offset keeps the
    /// fallback clear of the curated range (0..=228); evdev key codes top out at 0x2ff.
    fn from(key: evdev::Key) -> Self {
        use evdev::Key;
        let (code, name) = match key {
            Key::KEY_LEFTALT => (0, "Alt"),
            Key::KEY_RIGHTALT => (1, "AltGr"),
            Key::KEY_BACKSPACE => (2, "Backspace"),
            Key::KEY_CAPSLOCK => (3, "CapsLock"),
            Key::KEY_LEFTCTRL => (4, "ControlLeft"),
            Key::KEY_RIGHTCTRL => (5, "ControlRight"),
            Key::KEY_DELETE => (6, "Delete"),
            Key::KEY_DOWN => (7, "DownArrow"),
            Key::KEY_END => (8, "End"),
            Key::KEY_ESC => (9, "Escape"),
            Key::KEY_F1 => (10, "F1"),
            Key::KEY_F2 => (11, "F2"),
            Key::KEY_F3 => (12, "F3"),
            Key::KEY_F4 => (13, "F4"),
            Key::KEY_F5 => (14, "F5"),
            Key::KEY_F6 => (15, "F6"),
            Key::KEY_F7 => (16, "F7"),
            Key::KEY_F8 => (17, "F8"),
            Key::KEY_F9 => (18, "F9"),
            Key::KEY_F10 => (19, "F10"),
            Key::KEY_F11 => (20, "F11"),
            Key::KEY_F12 => (21, "F12"),
            Key::KEY_F13 => (200, "F13"),
            Key::KEY_F14 => (201, "F14"),
            Key::KEY_F15 => (202, "F15"),
            Key::KEY_F16 => (203, "F16"),
            Key::KEY_F17 => (204, "F17"),
            Key::KEY_F18 => (205, "F18"),
            Key::KEY_F19 => (206, "F19"),
            Key::KEY_F20 => (207, "F20"),
            Key::KEY_F21 => (208, "F21"),
            Key::KEY_F22 => (209, "F22"),
            Key::KEY_F23 => (210, "F23"),
            Key::KEY_F24 => (211, "F24"),
            Key::KEY_HOME => (22, "Home"),
            Key::KEY_LEFT => (23, "LeftArrow"),
            Key::KEY_LEFTMETA => (24, "MetaLeft"),
            Key::KEY_RIGHTMETA => (25, "MetaRight"),
            Key::KEY_PAGEDOWN => (26, "PageDown"),
            Key::KEY_PAGEUP => (27, "PageUp"),
            Key::KEY_ENTER => (28, "Return"),
            Key::KEY_RIGHT => (29, "RightArrow"),
            Key::KEY_LEFTSHIFT => (30, "ShiftLeft"),
            Key::KEY_RIGHTSHIFT => (31, "ShiftRight"),
            Key::KEY_SPACE => (32, "Space"),
            Key::KEY_TAB => (33, "Tab"),
            Key::KEY_UP => (34, "UpArrow"),
            Key::KEY_SYSRQ => (35, "PrintScreen"),
            Key::KEY_SCROLLLOCK => (36, "ScrollLock"),
            Key::KEY_PAUSE => (37, "Pause"),
            Key::KEY_NUMLOCK => (38, "NumLock"),
            Key::KEY_GRAVE => (39, "BackQuote"),
            Key::KEY_1 => (40, "Num1"),
            Key::KEY_2 => (41, "Num2"),
            Key::KEY_3 => (42, "Num3"),
            Key::KEY_4 => (43, "Num4"),
            Key::KEY_5 => (44, "Num5"),
            Key::KEY_6 => (45, "Num6"),
            Key::KEY_7 => (46, "Num7"),
            Key::KEY_8 => (47, "Num8"),
            Key::KEY_9 => (48, "Num9"),
            Key::KEY_0 => (49, "Num0"),
            Key::KEY_MINUS => (50, "Minus"),
            Key::KEY_EQUAL => (51, "Equal"),
            Key::KEY_Q => (52, "KeyQ"),
            Key::KEY_W => (53, "KeyW"),
            Key::KEY_E => (54, "KeyE"),
            Key::KEY_R => (55, "KeyR"),
            Key::KEY_T => (56, "KeyT"),
            Key::KEY_Y => (57, "KeyY"),
            Key::KEY_U => (58, "KeyU"),
            Key::KEY_I => (59, "KeyI"),
            Key::KEY_O => (60, "KeyO"),
            Key::KEY_P => (61, "KeyP"),
            Key::KEY_LEFTBRACE => (62, "LeftBracket"),
            Key::KEY_RIGHTBRACE => (63, "RightBracket"),
            Key::KEY_A => (64, "KeyA"),
            Key::KEY_S => (65, "KeyS"),
            Key::KEY_D => (66, "KeyD"),
            Key::KEY_F => (67, "KeyF"),
            Key::KEY_G => (68, "KeyG"),
            Key::KEY_H => (69, "KeyH"),
            Key::KEY_J => (70, "KeyJ"),
            Key::KEY_K => (71, "KeyK"),
            Key::KEY_L => (72, "KeyL"),
            Key::KEY_SEMICOLON => (73, "SemiColon"),
            Key::KEY_APOSTROPHE => (74, "Quote"),
            Key::KEY_BACKSLASH => (75, "BackSlash"),
            Key::KEY_102ND => (76, "IntlBackslash"),
            Key::KEY_Z => (77, "KeyZ"),
            Key::KEY_X => (78, "KeyX"),
            Key::KEY_C => (79, "KeyC"),
            Key::KEY_V => (80, "KeyV"),
            Key::KEY_B => (81, "KeyB"),
            Key::KEY_N => (82, "KeyN"),
            Key::KEY_M => (83, "KeyM"),
            Key::KEY_COMMA => (84, "Comma"),
            Key::KEY_DOT => (85, "Dot"),
            Key::KEY_SLASH => (86, "Slash"),
            Key::KEY_INSERT => (87, "Insert"),
            Key::KEY_KPENTER => (88, "KpReturn"),
            Key::KEY_KPMINUS => (89, "KpMinus"),
            Key::KEY_KPPLUS => (90, "KpPlus"),
            Key::KEY_KPASTERISK => (91, "KpMultiply"),
            Key::KEY_KPSLASH => (92, "KpDivide"),
            Key::KEY_KP0 => (93, "Kp0"),
            Key::KEY_KP1 => (94, "Kp1"),
            Key::KEY_KP2 => (95, "Kp2"),
            Key::KEY_KP3 => (96, "Kp3"),
            Key::KEY_KP4 => (97, "Kp4"),
            Key::KEY_KP5 => (98, "Kp5"),
            Key::KEY_KP6 => (99, "Kp6"),
            Key::KEY_KP7 => (100, "Kp7"),
            Key::KEY_KP8 => (101, "Kp8"),
            Key::KEY_KP9 => (102, "Kp9"),
            Key::KEY_KPDOT => (103, "KpDelete"),
            Key::KEY_FN => (104, "Function"),
            Key::KEY_VOLUMEUP => (220, "VolumeUp"),
            Key::KEY_VOLUMEDOWN => (221, "VolumeDown"),
            Key::KEY_MUTE => (222, "VolumeMute"),
            Key::KEY_BRIGHTNESSUP => (223, "BrightnessUp"),
            Key::KEY_BRIGHTNESSDOWN => (224, "BrightnessDown"),
            Key::KEY_PREVIOUSSONG => (225, "PreviousTrack"),
            Key::KEY_PLAYPAUSE => (226, "PlayPause"),
            Key::KEY_PLAYCD => (227, "PlayCd"),
            Key::KEY_NEXTSONG => (228, "NextTrack"),
            // Raw fallback: preserve the kernel keycode so post-processing can map it
            // back to a physical key. Offset by 1000 to stay clear of curated codes.
            _ => {
                return Self {
                    code: key.0 as u32 + 1000,
                    name: format!("Unknown({})", key.0),
                };
            }
        };
        Self {
            code,
            name: name.to_string(),
        }
    }
}

#[cfg(target_os = "linux")]
impl MouseButton {
    /// Map an evdev key to a mouse button if it falls in the pointer-button range
    /// (`BTN_LEFT..=BTN_TASK`). In evdev, pointer buttons arrive as `Key` events
    /// alongside keystrokes, so the backend uses this to separate the two.
    ///
    /// Returns `None` for keyboard keys and non-pointer buttons (gamepad, stylus, ...),
    /// which the backend treats as key events. `Other` carries the offset from `BTN_LEFT`,
    /// which also happens to line up with macOS/CGEvent button numbering (left=0, right=1,
    /// middle=2, extras=3+), so the exact button stays recoverable.
    pub fn from_evdev_key(key: evdev::Key) -> Option<Self> {
        use evdev::Key;
        match key {
            Key::BTN_LEFT => Some(MouseButton::Left),
            Key::BTN_RIGHT => Some(MouseButton::Right),
            Key::BTN_MIDDLE => Some(MouseButton::Middle),
            Key::BTN_SIDE | Key::BTN_EXTRA | Key::BTN_FORWARD | Key::BTN_BACK | Key::BTN_TASK => {
                Some(MouseButton::Other((key.0 - Key::BTN_LEFT.0) as u8))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_changed_msgpack_roundtrip() {
        let event = InputEvent {
            timestamp_us: 42,
            event: EventType::ContextChanged(ContextEvent {
                app_id: UNCAPTURED_APP_ID.to_string(),
            }),
        };

        let bytes = rmp_serde::to_vec(&event).unwrap();
        let decoded: InputEvent = rmp_serde::from_slice(&bytes).unwrap();

        match decoded.event {
            EventType::ContextChanged(ctx) => assert_eq!(ctx.app_id, UNCAPTURED_APP_ID),
            other => panic!("unexpected event after roundtrip: {:?}", other),
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod evdev_mapping_tests {
    use super::*;
    use evdev::Key;

    // Each evdev key must produce exactly the (code, name) the macOS rdev table
    // produces for the same physical key -- this is the cross-platform contract.
    #[test]
    fn evdev_keys_match_macos_namespace() {
        let cases: &[(Key, u32, &str)] = &[
            (Key::KEY_A, 64, "KeyA"),
            (Key::KEY_Z, 77, "KeyZ"),
            (Key::KEY_1, 40, "Num1"),
            (Key::KEY_0, 49, "Num0"),
            (Key::KEY_ENTER, 28, "Return"),
            (Key::KEY_ESC, 9, "Escape"),
            (Key::KEY_SPACE, 32, "Space"),
            (Key::KEY_TAB, 33, "Tab"),
            (Key::KEY_LEFTSHIFT, 30, "ShiftLeft"),
            (Key::KEY_RIGHTSHIFT, 31, "ShiftRight"),
            (Key::KEY_LEFTCTRL, 4, "ControlLeft"),
            (Key::KEY_LEFTALT, 0, "Alt"),
            (Key::KEY_RIGHTALT, 1, "AltGr"),
            (Key::KEY_LEFTMETA, 24, "MetaLeft"),
            (Key::KEY_F1, 10, "F1"),
            (Key::KEY_F12, 21, "F12"),
            (Key::KEY_F13, 200, "F13"),
            (Key::KEY_F24, 211, "F24"),
            (Key::KEY_UP, 34, "UpArrow"),
            (Key::KEY_LEFT, 23, "LeftArrow"),
            (Key::KEY_GRAVE, 39, "BackQuote"),
            (Key::KEY_102ND, 76, "IntlBackslash"),
            (Key::KEY_KP0, 93, "Kp0"),
            (Key::KEY_KPENTER, 88, "KpReturn"),
            (Key::KEY_KPDOT, 103, "KpDelete"),
            (Key::KEY_FN, 104, "Function"),
            (Key::KEY_MUTE, 222, "VolumeMute"),
            (Key::KEY_NEXTSONG, 228, "NextTrack"),
        ];
        for &(key, code, name) in cases {
            let ev = KeyEvent::from(key);
            assert_eq!(ev.code, code, "code mismatch for {:?}", key);
            assert_eq!(ev.name, name, "name mismatch for {:?}", key);
        }
    }

    #[test]
    fn unrecognized_key_falls_back_to_raw_code() {
        // A code with no curated equivalent must round-trip through the raw fallback
        // so post-processing can reconstruct it.
        let raw = Key::new(0x2ff);
        let ev = KeyEvent::from(raw);
        assert_eq!(ev.code, raw.0 as u32 + 1000);
        assert_eq!(ev.name, format!("Unknown({})", raw.0));
        assert!(
            ev.code >= 1000,
            "fallback codes stay clear of the curated range"
        );
    }

    #[test]
    fn pointer_buttons_map_to_mouse_buttons() {
        assert!(matches!(
            MouseButton::from_evdev_key(Key::BTN_LEFT),
            Some(MouseButton::Left)
        ));
        assert!(matches!(
            MouseButton::from_evdev_key(Key::BTN_RIGHT),
            Some(MouseButton::Right)
        ));
        assert!(matches!(
            MouseButton::from_evdev_key(Key::BTN_MIDDLE),
            Some(MouseButton::Middle)
        ));
        assert!(matches!(
            MouseButton::from_evdev_key(Key::BTN_SIDE),
            Some(MouseButton::Other(3))
        ));
        // Keyboard keys are not pointer buttons.
        assert!(MouseButton::from_evdev_key(Key::KEY_A).is_none());
    }
}
