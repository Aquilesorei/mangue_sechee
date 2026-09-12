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

impl KeyCode {
    pub const KEY_ESC: KeyCode = KeyCode(1);
    pub const KEY_TAB: KeyCode = KeyCode(15);
    pub const KEY_LEFTCTRL: KeyCode = KeyCode(29);
    pub const KEY_RIGHTCTRL: KeyCode = KeyCode(97);
    pub const KEY_LEFTALT: KeyCode = KeyCode(56);
    pub const KEY_RIGHTALT: KeyCode = KeyCode(100);
    pub const KEY_LEFTSHIFT: KeyCode = KeyCode(42);
    pub const KEY_RIGHTSHIFT: KeyCode = KeyCode(54);
    pub const KEY_LEFTMETA: KeyCode = KeyCode(125);
    pub const KEY_RIGHTMETA: KeyCode = KeyCode(126);
    pub const KEY_SCROLLLOCK: KeyCode = KeyCode(70);
    pub const KEY_PAUSE: KeyCode = KeyCode(119);
    pub const KEY_UP: KeyCode = KeyCode(103);
    pub const KEY_DOWN: KeyCode = KeyCode(108);
    pub const KEY_LEFT: KeyCode = KeyCode(105);
    pub const KEY_RIGHT: KeyCode = KeyCode(106);
    pub const KEY_L: KeyCode = KeyCode(38);
    pub const KEY_F1: KeyCode = KeyCode(59);
    pub const KEY_F2: KeyCode = KeyCode(60);
    pub const KEY_F3: KeyCode = KeyCode(61);
    pub const KEY_F4: KeyCode = KeyCode(62);
    pub const KEY_F5: KeyCode = KeyCode(63);
    pub const KEY_F6: KeyCode = KeyCode(64);
    pub const KEY_F7: KeyCode = KeyCode(65);
    pub const KEY_F8: KeyCode = KeyCode(66);
    pub const KEY_F9: KeyCode = KeyCode(67);
    pub const KEY_F10: KeyCode = KeyCode(68);
    pub const KEY_F11: KeyCode = KeyCode(87);
    pub const KEY_F12: KeyCode = KeyCode(88);

    pub fn is_ctrl(&self) -> bool {
        *self == Self::KEY_LEFTCTRL || *self == Self::KEY_RIGHTCTRL
    }

    pub fn is_alt(&self) -> bool {
        *self == Self::KEY_LEFTALT || *self == Self::KEY_RIGHTALT
    }

    pub fn is_shift(&self) -> bool {
        *self == Self::KEY_LEFTSHIFT || *self == Self::KEY_RIGHTSHIFT
    }

    pub fn is_meta(&self) -> bool {
        *self == Self::KEY_LEFTMETA || *self == Self::KEY_RIGHTMETA
    }

    pub fn from_name(name: &str) -> Option<KeyCode> {
        match name.trim().to_lowercase().as_str() {
            "esc" | "escape" => Some(Self::KEY_ESC),
            "tab" => Some(Self::KEY_TAB),
            "scrolllock" | "scroll_lock" | "scroll lock" => Some(Self::KEY_SCROLLLOCK),
            "pause" | "break" | "pause/break" | "pause / break" => Some(Self::KEY_PAUSE),
            "ctrl" | "leftctrl" | "lctrl" => Some(Self::KEY_LEFTCTRL),
            "rctrl" | "rightctrl" => Some(Self::KEY_RIGHTCTRL),
            "alt" | "leftalt" | "lalt" => Some(Self::KEY_LEFTALT),
            "ralt" | "rightalt" | "altgr" => Some(Self::KEY_RIGHTALT),
            "shift" | "leftshift" | "lshift" => Some(Self::KEY_LEFTSHIFT),
            "rshift" | "rightshift" => Some(Self::KEY_RIGHTSHIFT),
            "super" | "meta" | "win" | "cmd" => Some(Self::KEY_LEFTMETA),
            "up" | "arrowup" => Some(Self::KEY_UP),
            "down" | "arrowdown" => Some(Self::KEY_DOWN),
            "left" | "arrowleft" => Some(Self::KEY_LEFT),
            "right" | "arrowright" | "right / left" | "right/left" => Some(Self::KEY_RIGHT),
            "l" => Some(Self::KEY_L),
            "f1" => Some(Self::KEY_F1),
            "f2" => Some(Self::KEY_F2),
            "f3" => Some(Self::KEY_F3),
            "f4" => Some(Self::KEY_F4),
            "f5" => Some(Self::KEY_F5),
            "f6" => Some(Self::KEY_F6),
            "f7" => Some(Self::KEY_F7),
            "f8" => Some(Self::KEY_F8),
            "f9" => Some(Self::KEY_F9),
            "f10" => Some(Self::KEY_F10),
            "f11" => Some(Self::KEY_F11),
            "f12" => Some(Self::KEY_F12),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match *self {
            Self::KEY_ESC => "Escape",
            Self::KEY_SCROLLLOCK => "ScrollLock",
            Self::KEY_PAUSE => "Pause",
            Self::KEY_LEFTCTRL => "LeftCtrl",
            Self::KEY_RIGHTCTRL => "RightCtrl",
            Self::KEY_LEFTALT => "LeftAlt",
            Self::KEY_RIGHTALT => "RightAlt",
            Self::KEY_LEFTSHIFT => "LeftShift",
            Self::KEY_RIGHTSHIFT => "RightShift",
            Self::KEY_LEFTMETA => "LeftMeta",
            Self::KEY_RIGHTMETA => "RightMeta",
            Self::KEY_UP => "Up",
            Self::KEY_DOWN => "Down",
            Self::KEY_LEFT => "Left",
            Self::KEY_RIGHT => "Right",
            Self::KEY_L => "L",
            Self::KEY_F1 => "F1",
            Self::KEY_F2 => "F2",
            Self::KEY_F3 => "F3",
            Self::KEY_F4 => "F4",
            Self::KEY_F5 => "F5",
            Self::KEY_F6 => "F6",
            Self::KEY_F7 => "F7",
            Self::KEY_F8 => "F8",
            Self::KEY_F9 => "F9",
            Self::KEY_F10 => "F10",
            Self::KEY_F11 => "F11",
            Self::KEY_F12 => "F12",
            _ => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClipboardEvent {
    Text(String),
    Clear,
}
