//! Input event data structures

use serde::{Deserialize, Serialize};

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
    /// X coordinate
    pub x: f64,
    
    /// Y coordinate
    pub y: f64,
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
            rdev::Key::Unknown(code) => (code as u32 + 1000, format!("Unknown({})", code)),
        };
        
        Self { code, name }
    }
}

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
