//! Action parsing and execution for computer-use predictions.
//!
//! Parses natural-language action strings like `CLICK(480, 420)` into
//! structured Action enums, and executes them via rdev::simulate.

use anyhow::Result;
use std::thread;
use std::time::Duration;
use tracing::{info, warn};

/// A parsed computer-use action.
#[derive(Debug, Clone)]
pub enum Action {
    Click { x: f64, y: f64 },
    DoubleClick { x: f64, y: f64 },
    RightClick { x: f64, y: f64 },
    DragTo { x: f64, y: f64 },
    Typing { text: String },
    Press { key: String },
    Hotkey { keys: Vec<String> },
    Scroll { dx: i32, dy: i32 },
}

/// Parse an action string like `CLICK(480, 420)` into an Action.
pub fn parse_action(s: &str) -> Option<Action> {
    let s = s.trim();

    // CLICK(x, y)
    if let Some(args) = strip_fn(s, "CLICK") {
        let (x, y) = parse_two_floats(args)?;
        return Some(Action::Click { x, y });
    }
    // DOUBLE_CLICK(x, y)
    if let Some(args) = strip_fn(s, "DOUBLE_CLICK") {
        let (x, y) = parse_two_floats(args)?;
        return Some(Action::DoubleClick { x, y });
    }
    // RIGHT_CLICK(x, y)
    if let Some(args) = strip_fn(s, "RIGHT_CLICK") {
        let (x, y) = parse_two_floats(args)?;
        return Some(Action::RightClick { x, y });
    }
    // DRAG_TO(x, y)
    if let Some(args) = strip_fn(s, "DRAG_TO") {
        let (x, y) = parse_two_floats(args)?;
        return Some(Action::DragTo { x, y });
    }
    // TYPING("text")
    if let Some(args) = strip_fn(s, "TYPING") {
        let text = parse_quoted_string(args)?;
        return Some(Action::Typing { text });
    }
    // PRESS(key)
    if let Some(args) = strip_fn(s, "PRESS") {
        let key = args.trim().trim_matches('"').trim_matches('\'').to_string();
        return Some(Action::Press { key });
    }
    // HOTKEY(key1, key2, ...)
    if let Some(args) = strip_fn(s, "HOTKEY") {
        let keys: Vec<String> = args
            .split(',')
            .map(|k| k.trim().trim_matches('"').trim_matches('\'').to_lowercase())
            .filter(|k| !k.is_empty())
            .collect();
        if !keys.is_empty() {
            return Some(Action::Hotkey { keys });
        }
    }
    // SCROLL(dx, dy)
    if let Some(args) = strip_fn(s, "SCROLL") {
        let (dx, dy) = parse_two_ints(args)?;
        return Some(Action::Scroll { dx, dy });
    }

    // --- pyautogui format (Kimi K2.5 style) ---
    // pyautogui.click(x, y)
    if let Some(args) = strip_fn(s, "pyautogui.click") {
        let (x, y) = parse_two_floats(args)?;
        return Some(Action::Click { x, y });
    }
    // pyautogui.doubleClick(x, y)
    if let Some(args) = strip_fn(s, "pyautogui.doubleClick") {
        let (x, y) = parse_two_floats(args)?;
        return Some(Action::DoubleClick { x, y });
    }
    // pyautogui.rightClick(x, y)
    if let Some(args) = strip_fn(s, "pyautogui.rightClick") {
        let (x, y) = parse_two_floats(args)?;
        return Some(Action::RightClick { x, y });
    }
    // pyautogui.moveTo(x, y)
    if let Some(args) = strip_fn(s, "pyautogui.moveTo") {
        let (x, y) = parse_two_floats(args)?;
        return Some(Action::Click { x, y }); // treat moveTo as click for now
    }
    // pyautogui.write("text") or pyautogui.typewrite("text")
    if let Some(args) = strip_fn(s, "pyautogui.write").or_else(|| strip_fn(s, "pyautogui.typewrite")) {
        let text = parse_quoted_string(args)?;
        return Some(Action::Typing { text });
    }
    // pyautogui.press("key")
    if let Some(args) = strip_fn(s, "pyautogui.press") {
        let key = args.trim().trim_matches('"').trim_matches('\'').to_string();
        return Some(Action::Press { key });
    }
    // pyautogui.hotkey("key1", "key2", ...)
    if let Some(args) = strip_fn(s, "pyautogui.hotkey") {
        let keys: Vec<String> = args
            .split(',')
            .map(|k| k.trim().trim_matches('"').trim_matches('\'').to_lowercase())
            .filter(|k| !k.is_empty())
            .collect();
        if !keys.is_empty() {
            return Some(Action::Hotkey { keys });
        }
    }
    // pyautogui.scroll(amount) — single int, negative = down
    if let Some(args) = strip_fn(s, "pyautogui.scroll") {
        if let Ok(dy) = args.trim().parse::<i32>() {
            return Some(Action::Scroll { dx: 0, dy });
        }
    }

    None
}

/// Execute a sequence of actions via rdev::simulate.
/// Coordinates are mapped based on coordinate_system:
///   "normalized_1000" — model outputs 0-1000, scaled to screen
///   "absolute_pixels" — model outputs in frame_width x frame_height, scaled to screen
pub fn execute_actions(actions: &[Action], frame_width: u32, frame_height: u32, coordinate_system: &str) -> Result<()> {
    let (screen_w, screen_h) = rdev::display_size().map_err(|e| anyhow::anyhow!("display_size: {:?}", e))?;
    let (scale_x, scale_y) = match coordinate_system {
        "absolute_pixels" => (screen_w as f64 / frame_width as f64, screen_h as f64 / frame_height as f64),
        _ => (screen_w as f64 / 1000.0, screen_h as f64 / 1000.0), // normalized_1000
    };

    info!("Executing {} actions (screen: {}x{}, coord_sys: {}, scale: {:.2}x{:.2})",
        actions.len(), screen_w, screen_h, coordinate_system, scale_x, scale_y);

    for (i, action) in actions.iter().enumerate() {
        if i > 0 {
            thread::sleep(Duration::from_millis(100));
        }
        execute_one(action, scale_x, scale_y)?;
    }

    Ok(())
}

fn execute_one(action: &Action, scale_x: f64, scale_y: f64) -> Result<()> {
    match action {
        Action::Click { x, y } => {
            let sx = x * scale_x;
            let sy = y * scale_y;
            info!("Execute: CLICK({}, {}) -> screen({:.0}, {:.0})", x, y, sx, sy);
            sim(rdev::EventType::MouseMove { x: sx, y: sy, delta_x: 0.0, delta_y: 0.0 })?;
            thread::sleep(Duration::from_millis(20));
            sim(rdev::EventType::ButtonPress(rdev::Button::Left))?;
            thread::sleep(Duration::from_millis(20));
            sim(rdev::EventType::ButtonRelease(rdev::Button::Left))?;
        }
        Action::DoubleClick { x, y } => {
            let sx = x * scale_x;
            let sy = y * scale_y;
            info!("Execute: DOUBLE_CLICK({}, {}) -> screen({:.0}, {:.0})", x, y, sx, sy);
            sim(rdev::EventType::MouseMove { x: sx, y: sy, delta_x: 0.0, delta_y: 0.0 })?;
            for _ in 0..2 {
                thread::sleep(Duration::from_millis(20));
                sim(rdev::EventType::ButtonPress(rdev::Button::Left))?;
                thread::sleep(Duration::from_millis(20));
                sim(rdev::EventType::ButtonRelease(rdev::Button::Left))?;
            }
        }
        Action::RightClick { x, y } => {
            let sx = x * scale_x;
            let sy = y * scale_y;
            info!("Execute: RIGHT_CLICK({}, {}) -> screen({:.0}, {:.0})", x, y, sx, sy);
            sim(rdev::EventType::MouseMove { x: sx, y: sy, delta_x: 0.0, delta_y: 0.0 })?;
            thread::sleep(Duration::from_millis(20));
            sim(rdev::EventType::ButtonPress(rdev::Button::Right))?;
            thread::sleep(Duration::from_millis(20));
            sim(rdev::EventType::ButtonRelease(rdev::Button::Right))?;
        }
        Action::DragTo { x, y } => {
            let sx = x * scale_x;
            let sy = y * scale_y;
            info!("Execute: DRAG_TO({}, {}) -> screen({:.0}, {:.0})", x, y, sx, sy);
            sim(rdev::EventType::ButtonPress(rdev::Button::Left))?;
            thread::sleep(Duration::from_millis(50));
            sim(rdev::EventType::MouseMove { x: sx, y: sy, delta_x: 0.0, delta_y: 0.0 })?;
            thread::sleep(Duration::from_millis(50));
            sim(rdev::EventType::ButtonRelease(rdev::Button::Left))?;
        }
        Action::Typing { text } => {
            info!("Execute: TYPING(\"{}\")", text);
            for ch in text.chars() {
                if let Some(key) = char_to_key(ch) {
                    let needs_shift = ch.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch);
                    if needs_shift {
                        sim(rdev::EventType::KeyPress(rdev::Key::ShiftLeft))?;
                    }
                    sim(rdev::EventType::KeyPress(key))?;
                    thread::sleep(Duration::from_millis(10));
                    sim(rdev::EventType::KeyRelease(key))?;
                    if needs_shift {
                        sim(rdev::EventType::KeyRelease(rdev::Key::ShiftLeft))?;
                    }
                } else {
                    warn!("Cannot type character: {:?}", ch);
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        Action::Press { key } => {
            info!("Execute: PRESS({})", key);
            if let Some(k) = key_name_to_rdev(key) {
                sim(rdev::EventType::KeyPress(k))?;
                thread::sleep(Duration::from_millis(20));
                sim(rdev::EventType::KeyRelease(k))?;
            } else {
                warn!("Unknown key: {}", key);
            }
        }
        Action::Hotkey { keys } => {
            info!("Execute: HOTKEY({:?})", keys);
            let rkeys: Vec<rdev::Key> = keys.iter().filter_map(|k| key_name_to_rdev(k)).collect();
            // Press all keys
            for k in &rkeys {
                sim(rdev::EventType::KeyPress(*k))?;
                thread::sleep(Duration::from_millis(10));
            }
            // Release in reverse
            for k in rkeys.iter().rev() {
                sim(rdev::EventType::KeyRelease(*k))?;
                thread::sleep(Duration::from_millis(10));
            }
        }
        Action::Scroll { dx, dy } => {
            info!("Execute: SCROLL({}, {})", dx, dy);
            sim(rdev::EventType::Wheel {
                delta_x: *dx as i64,
                delta_y: *dy as i64,
            })?;
        }
    }
    Ok(())
}

fn sim(event: rdev::EventType) -> Result<()> {
    rdev::simulate(&event).map_err(|e| anyhow::anyhow!("simulate failed: {:?}", e))
}

// ---- Parsing helpers ----

fn strip_fn<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let s = s.trim();
    if s.len() > name.len() + 2
        && s[..name.len()].eq_ignore_ascii_case(name)
        && s.as_bytes()[name.len()] == b'('
        && s.ends_with(')')
    {
        Some(&s[name.len() + 1..s.len() - 1])
    } else {
        None
    }
}

fn parse_two_floats(s: &str) -> Option<(f64, f64)> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() >= 2 {
        let a = parts[0].trim().parse().ok()?;
        let b = parts[1].trim().parse().ok()?;
        Some((a, b))
    } else {
        None
    }
}

fn parse_two_ints(s: &str) -> Option<(i32, i32)> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() >= 2 {
        let a = parts[0].trim().parse().ok()?;
        let b = parts[1].trim().parse().ok()?;
        Some((a, b))
    } else {
        None
    }
}

fn parse_quoted_string(s: &str) -> Option<String> {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        Some(s[1..s.len() - 1].to_string())
    } else {
        // Accept unquoted text as well
        Some(s.to_string())
    }
}

// ---- Key mapping ----

fn key_name_to_rdev(name: &str) -> Option<rdev::Key> {
    match name.to_lowercase().trim() {
        "enter" | "return" => Some(rdev::Key::Return),
        "backspace" => Some(rdev::Key::Backspace),
        "tab" => Some(rdev::Key::Tab),
        "escape" | "esc" => Some(rdev::Key::Escape),
        "space" => Some(rdev::Key::Space),
        "delete" | "del" => Some(rdev::Key::Delete),
        "up" => Some(rdev::Key::UpArrow),
        "down" => Some(rdev::Key::DownArrow),
        "left" => Some(rdev::Key::LeftArrow),
        "right" => Some(rdev::Key::RightArrow),
        "home" => Some(rdev::Key::Home),
        "end" => Some(rdev::Key::End),
        "pageup" | "pgup" => Some(rdev::Key::PageUp),
        "pagedown" | "pgdn" => Some(rdev::Key::PageDown),
        "shift" => Some(rdev::Key::ShiftLeft),
        "ctrl" | "control" => Some(rdev::Key::ControlLeft),
        "alt" | "opt" | "option" => Some(rdev::Key::Alt),
        "cmd" | "command" | "meta" => Some(rdev::Key::MetaLeft),
        "capslock" => Some(rdev::Key::CapsLock),
        "f1" => Some(rdev::Key::F1),
        "f2" => Some(rdev::Key::F2),
        "f3" => Some(rdev::Key::F3),
        "f4" => Some(rdev::Key::F4),
        "f5" => Some(rdev::Key::F5),
        "f6" => Some(rdev::Key::F6),
        "f7" => Some(rdev::Key::F7),
        "f8" => Some(rdev::Key::F8),
        "f9" => Some(rdev::Key::F9),
        "f10" => Some(rdev::Key::F10),
        "f11" => Some(rdev::Key::F11),
        "f12" => Some(rdev::Key::F12),
        // Single character keys
        s if s.len() == 1 => char_to_key(s.chars().next().unwrap()),
        _ => None,
    }
}

fn char_to_key(c: char) -> Option<rdev::Key> {
    match c.to_ascii_lowercase() {
        'a' => Some(rdev::Key::KeyA),
        'b' => Some(rdev::Key::KeyB),
        'c' => Some(rdev::Key::KeyC),
        'd' => Some(rdev::Key::KeyD),
        'e' => Some(rdev::Key::KeyE),
        'f' => Some(rdev::Key::KeyF),
        'g' => Some(rdev::Key::KeyG),
        'h' => Some(rdev::Key::KeyH),
        'i' => Some(rdev::Key::KeyI),
        'j' => Some(rdev::Key::KeyJ),
        'k' => Some(rdev::Key::KeyK),
        'l' => Some(rdev::Key::KeyL),
        'm' => Some(rdev::Key::KeyM),
        'n' => Some(rdev::Key::KeyN),
        'o' => Some(rdev::Key::KeyO),
        'p' => Some(rdev::Key::KeyP),
        'q' => Some(rdev::Key::KeyQ),
        'r' => Some(rdev::Key::KeyR),
        's' => Some(rdev::Key::KeyS),
        't' => Some(rdev::Key::KeyT),
        'u' => Some(rdev::Key::KeyU),
        'v' => Some(rdev::Key::KeyV),
        'w' => Some(rdev::Key::KeyW),
        'x' => Some(rdev::Key::KeyX),
        'y' => Some(rdev::Key::KeyY),
        'z' => Some(rdev::Key::KeyZ),
        '0' | ')' => Some(rdev::Key::Num0),
        '1' | '!' => Some(rdev::Key::Num1),
        '2' | '@' => Some(rdev::Key::Num2),
        '3' | '#' => Some(rdev::Key::Num3),
        '4' | '$' => Some(rdev::Key::Num4),
        '5' | '%' => Some(rdev::Key::Num5),
        '6' | '^' => Some(rdev::Key::Num6),
        '7' | '&' => Some(rdev::Key::Num7),
        '8' | '*' => Some(rdev::Key::Num8),
        '9' | '(' => Some(rdev::Key::Num9),
        ' ' => Some(rdev::Key::Space),
        '-' | '_' => Some(rdev::Key::Minus),
        '=' | '+' => Some(rdev::Key::Equal),
        '[' | '{' => Some(rdev::Key::LeftBracket),
        ']' | '}' => Some(rdev::Key::RightBracket),
        '\\' | '|' => Some(rdev::Key::BackSlash),
        ';' | ':' => Some(rdev::Key::SemiColon),
        '\'' | '"' => Some(rdev::Key::Quote),
        ',' | '<' => Some(rdev::Key::Comma),
        '.' | '>' => Some(rdev::Key::Dot),
        '/' | '?' => Some(rdev::Key::Slash),
        '`' | '~' => Some(rdev::Key::BackQuote),
        '\n' => Some(rdev::Key::Return),
        '\t' => Some(rdev::Key::Tab),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Qwen tool_call JSON parsing
// ---------------------------------------------------------------------------

/// Result of parsing a Qwen tool_call JSON.
pub struct ToolCallResult {
    /// Native action string (e.g. "CLICK(500, 300)")
    pub action: String,
    /// Optional hint coordinate for non-spatial actions (type, key, scroll)
    pub hint_coordinate: Option<(f64, f64)>,
}

/// Parse a Qwen-style tool_call JSON into a native action string + optional hint coordinate.
/// Input: `{"name": "computer_use", "arguments": {"action": "left_click", "coordinate": [500, 300]}}`
/// Output: ToolCallResult with action="CLICK(500, 300)" (or None if unparseable)
pub fn parse_tool_call_json(json_str: &str) -> Option<ToolCallResult> {
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let args = &v["arguments"];
    let action = args["action"].as_str()?;

    // Extract optional coordinate (used by all spatial actions, and as hint for non-spatial)
    let coord = args.get("coordinate").and_then(|c| {
        let x = c[0].as_f64()?;
        let y = c[1].as_f64()?;
        Some((x, y))
    });

    match action {
        "left_click" => {
            let (x, y) = coord?;
            Some(ToolCallResult { action: format!("CLICK({}, {})", x, y), hint_coordinate: None })
        }
        "double_click" => {
            let (x, y) = coord?;
            Some(ToolCallResult { action: format!("DOUBLE_CLICK({}, {})", x, y), hint_coordinate: None })
        }
        "right_click" => {
            let (x, y) = coord?;
            Some(ToolCallResult { action: format!("RIGHT_CLICK({}, {})", x, y), hint_coordinate: None })
        }
        "middle_click" => {
            let (x, y) = coord?;
            Some(ToolCallResult { action: format!("CLICK({}, {})", x, y), hint_coordinate: None })
        }
        "left_click_drag" => {
            let (x, y) = coord?;
            Some(ToolCallResult { action: format!("DRAG_TO({}, {})", x, y), hint_coordinate: None })
        }
        "mouse_move" => {
            let (x, y) = coord?;
            Some(ToolCallResult { action: format!("CLICK({}, {})", x, y), hint_coordinate: None })
        }
        "type" => {
            let text = args["text"].as_str()?;
            Some(ToolCallResult { action: format!("TYPING(\"{}\")", text), hint_coordinate: coord })
        }
        "key" => {
            let keys = &args["keys"];
            let action_str = if let Some(arr) = keys.as_array() {
                let key_strs: Vec<&str> = arr.iter().filter_map(|k| k.as_str()).collect();
                if key_strs.len() == 1 {
                    Some(format!("PRESS({})", key_strs[0]))
                } else if key_strs.len() > 1 {
                    Some(format!("HOTKEY({})", key_strs.join(", ")))
                } else {
                    None
                }
            } else if let Some(key) = keys.as_str() {
                if key.contains('+') {
                    let parts: Vec<&str> = key.split('+').collect();
                    Some(format!("HOTKEY({})", parts.join(", ")))
                } else {
                    Some(format!("PRESS({})", key))
                }
            } else {
                None
            };
            action_str.map(|a| ToolCallResult { action: a, hint_coordinate: coord })
        }
        "scroll" => {
            let pixels = args["pixels"].as_i64().unwrap_or(-3) as i32;
            Some(ToolCallResult { action: format!("SCROLL(0, {})", pixels), hint_coordinate: coord })
        }
        "wait" | "terminate" => None,
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// ActionDisplay — structured action info for the spatial overlay
// ---------------------------------------------------------------------------

/// Action type tag for the overlay.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ActionDisplayKind {
    None = 0,
    Click = 1,
    DoubleClick = 2,
    RightClick = 3,
    DragTo = 4,
    Typing = 5,
    Press = 6,
    Hotkey = 7,
    Scroll = 8,
}

/// Single action's display metadata for the spatial overlay.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct ActionDisplayItem {
    pub kind: ActionDisplayKind,
    /// Screen-space X (meaningful for spatial actions)
    pub screen_x: f64,
    /// Screen-space Y
    pub screen_y: f64,
    /// Null-terminated UTF-8 label, e.g. "Tab: click"
    pub label: [u8; 128],
    pub label_len: u32,
}

impl Default for ActionDisplayItem {
    fn default() -> Self {
        Self {
            kind: ActionDisplayKind::None,
            screen_x: 0.0,
            screen_y: 0.0,
            label: [0u8; 128],
            label_len: 0,
        }
    }
}

impl ActionDisplayItem {
    fn with_label(mut self, text: &str) -> Self {
        let bytes = text.as_bytes();
        let len = bytes.len().min(127);
        self.label[..len].copy_from_slice(&bytes[..len]);
        self.label[len] = 0;
        self.label_len = len as u32;
        self
    }
}

/// Container for action display items (max 8).
#[derive(Debug, Clone)]
pub struct ActionDisplay {
    pub items: [ActionDisplayItem; 8],
    pub count: u32,
}

impl Default for ActionDisplay {
    fn default() -> Self {
        Self {
            items: std::array::from_fn(|_| ActionDisplayItem::default()),
            count: 0,
        }
    }
}

/// Build display metadata for a set of action strings.
/// Converts model coordinates to screen coordinates using the same logic as execute_actions().
/// `hint_coordinates` provides optional model-space coordinates for non-spatial actions (type, key, scroll).
pub fn build_action_display(
    action_strings: &[String],
    frame_width: u32,
    frame_height: u32,
    coordinate_system: &str,
    hint_coordinates: &[Option<(f64, f64)>],
) -> ActionDisplay {
    let mut display = ActionDisplay::default();

    let (screen_w, screen_h) = match rdev::display_size() {
        Ok(size) => size,
        Err(_) => return display,
    };
    let (scale_x, scale_y) = match coordinate_system {
        "absolute_pixels" => (screen_w as f64 / frame_width as f64, screen_h as f64 / frame_height as f64),
        _ => (screen_w as f64 / 1000.0, screen_h as f64 / 1000.0),
    };

    for (i, s) in action_strings.iter().enumerate().take(8) {
        let Some(action) = parse_action(s) else { continue };
        // Hint coordinate for non-spatial actions (from tool_call JSON)
        let hint = hint_coordinates.get(i).copied().flatten()
            .map(|(x, y)| (x * scale_x, y * scale_y));

        let item = match &action {
            Action::Click { x, y } => ActionDisplayItem {
                kind: ActionDisplayKind::Click,
                screen_x: x * scale_x,
                screen_y: y * scale_y,
                ..Default::default()
            }.with_label("Tab: click"),
            Action::DoubleClick { x, y } => ActionDisplayItem {
                kind: ActionDisplayKind::DoubleClick,
                screen_x: x * scale_x,
                screen_y: y * scale_y,
                ..Default::default()
            }.with_label("Tab: double-click"),
            Action::RightClick { x, y } => ActionDisplayItem {
                kind: ActionDisplayKind::RightClick,
                screen_x: x * scale_x,
                screen_y: y * scale_y,
                ..Default::default()
            }.with_label("Tab: right-click"),
            Action::DragTo { x, y } => ActionDisplayItem {
                kind: ActionDisplayKind::DragTo,
                screen_x: x * scale_x,
                screen_y: y * scale_y,
                ..Default::default()
            }.with_label("Tab: drag here"),
            Action::Typing { text } => {
                let (sx, sy) = hint.unwrap_or((0.0, 0.0));
                ActionDisplayItem {
                    kind: ActionDisplayKind::Typing,
                    screen_x: sx,
                    screen_y: sy,
                    ..Default::default()
                }.with_label(&format!("Tab: type '{}'", text))
            }
            Action::Press { key } => {
                let (sx, sy) = hint.unwrap_or((0.0, 0.0));
                ActionDisplayItem {
                    kind: ActionDisplayKind::Press,
                    screen_x: sx,
                    screen_y: sy,
                    ..Default::default()
                }.with_label(&format!("Tab: {}", key))
            }
            Action::Hotkey { keys } => {
                let (sx, sy) = hint.unwrap_or((0.0, 0.0));
                ActionDisplayItem {
                    kind: ActionDisplayKind::Hotkey,
                    screen_x: sx,
                    screen_y: sy,
                    ..Default::default()
                }.with_label(&format!("Tab: {}", keys.join("+")))
            }
            Action::Scroll { dx, dy } => {
                let dir = if *dy < 0 { "down" } else if *dy > 0 { "up" } else if *dx < 0 { "left" } else { "right" };
                let (sx, sy) = hint.unwrap_or((0.0, 0.0));
                ActionDisplayItem {
                    kind: ActionDisplayKind::Scroll,
                    screen_x: sx,
                    screen_y: sy,
                    ..Default::default()
                }.with_label(&format!("Tab: scroll {}", dir))
            }
        };
        display.items[display.count as usize] = item;
        display.count += 1;
    }

    display
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_click() {
        let a = parse_action("CLICK(480, 420)").unwrap();
        match a {
            Action::Click { x, y } => {
                assert_eq!(x, 480.0);
                assert_eq!(y, 420.0);
            }
            _ => panic!("expected Click"),
        }
    }

    #[test]
    fn test_parse_typing() {
        let a = parse_action("TYPING(\"cargo build\")").unwrap();
        match a {
            Action::Typing { text } => assert_eq!(text, "cargo build"),
            _ => panic!("expected Typing"),
        }
    }

    #[test]
    fn test_parse_hotkey() {
        let a = parse_action("HOTKEY(cmd, s)").unwrap();
        match a {
            Action::Hotkey { keys } => assert_eq!(keys, vec!["cmd", "s"]),
            _ => panic!("expected Hotkey"),
        }
    }

    #[test]
    fn test_parse_scroll() {
        let a = parse_action("SCROLL(0, -5)").unwrap();
        match a {
            Action::Scroll { dx, dy } => {
                assert_eq!(dx, 0);
                assert_eq!(dy, -5);
            }
            _ => panic!("expected Scroll"),
        }
    }

    #[test]
    fn test_parse_press() {
        let a = parse_action("PRESS(enter)").unwrap();
        match a {
            Action::Press { key } => assert_eq!(key, "enter"),
            _ => panic!("expected Press"),
        }
    }
}
