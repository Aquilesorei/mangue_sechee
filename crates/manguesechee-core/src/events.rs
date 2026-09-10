use serde::{Deserialize, Serialize};

/// Platform-independent input event.
/// Transmitted between controller and controlled peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InputEvent {
    MouseMove { dx: i32, dy: i32 },
    MouseButton { button: MouseButton, pressed: bool },
    MouseScroll { dx: f32, dy: f32 },
    Key { key: KeyCode, pressed: bool },
    /// Sent once on connection start — full snapshot of currently held keys.
    /// The controlled peer releases any keys not in this list.
    KeySync { pressed_keys: Vec<KeyCode> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u8),
}

/// Linux evdev key code (matches `evdev::Key` values directly).
/// Using the raw u16 avoids a giant translation table and keeps
/// the protocol independent of higher-level keysym layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct KeyCode(pub u16);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClipboardEvent {
    Text(String),
    Clear,
}
