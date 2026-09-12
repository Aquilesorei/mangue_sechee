//! Hotkey parsing and evaluation engine for evdev input events.

use manguesechee_core::config::HotkeyConfig;
use manguesechee_core::events::KeyCode;
use manguesechee_core::protocol::Edge;

/// Action triggered by a recognized hotkey combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    /// Toggle cursor locking on/off on the current screen.
    ToggleCursorLock,
    /// Instantly switch active screen focus between controller and peer.
    SwitchScreen,
    /// Directional jump across spatial grid topology (e.g. Ctrl+Alt+Arrow).
    DirectionalJump(Edge),
    /// Emergency abort: force-ungrab hardware and drop remote holds.
    EmergencyEscape,
}

/// Parsed keyboard shortcut combination (modifiers + trigger key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyCombo {
    pub ctrl:  bool,
    pub alt:   bool,
    pub shift: bool,
    pub meta:  bool,
    pub key:   KeyCode,
}

impl HotkeyCombo {
    /// Parse a string shortcut like "Ctrl+Alt+Right", "ScrollLock", "Pause", or "Ctrl+Alt+Escape".
    /// Returns `None` if disabled, empty, or unparseable.
    pub fn parse(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty()
            || trimmed.eq_ignore_ascii_case("none")
            || trimmed.eq_ignore_ascii_case("disabled")
        {
            return None;
        }

        let parts: Vec<&str> = trimmed.split('+').map(|p| p.trim()).collect();
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut meta = false;
        let mut trigger_key: Option<KeyCode> = None;

        for part in parts {
            match part.to_lowercase().as_str() {
                "ctrl" | "control" | "lctrl" | "rctrl" => ctrl = true,
                "alt" | "lalt" | "ralt" | "altgr" => alt = true,
                "shift" | "lshift" | "rshift" => shift = true,
                "super" | "meta" | "win" | "cmd" => meta = true,
                other => {
                    if let Some(kc) = KeyCode::from_name(other) {
                        trigger_key = Some(kc);
                    }
                }
            }
        }

        trigger_key.map(|key| Self { ctrl, alt, shift, meta, key })
    }

    /// Check if this combination matches the given key event and active modifiers.
    pub fn matches(&self, key: KeyCode, ctrl: bool, alt: bool, shift: bool, meta: bool) -> bool {
        self.key == key
            && self.ctrl == ctrl
            && self.alt == alt
            && self.shift == shift
            && self.meta == meta
    }
}

/// Real-time hotkey matcher tracking modifier states and firing actions.
#[derive(Debug, Clone)]
pub struct HotkeyMatcher {
    pub enabled:          bool,
    pub toggle_lock:      Option<HotkeyCombo>,
    pub switch_screen:    Option<HotkeyCombo>,
    pub emergency_escape: Option<HotkeyCombo>,

    lctrl:  bool,
    rctrl:  bool,
    lalt:   bool,
    ralt:   bool,
    lshift: bool,
    rshift: bool,
    lmeta:  bool,
    rmeta:  bool,
}

impl HotkeyMatcher {
    /// Create a matcher configured with user settings.
    pub fn from_config(config: &HotkeyConfig) -> Self {
        Self {
            enabled:          config.enabled,
            toggle_lock:      HotkeyCombo::parse(&config.toggle_cursor_lock),
            switch_screen:    HotkeyCombo::parse(&config.switch_screen),
            emergency_escape: HotkeyCombo::parse(&config.emergency_escape),
            lctrl:  false,
            rctrl:  false,
            lalt:   false,
            ralt:   false,
            lshift: false,
            rshift: false,
            lmeta:  false,
            rmeta:  false,
        }
    }

    #[inline]
    pub fn ctrl_held(&self) -> bool {
        self.lctrl || self.rctrl
    }

    #[inline]
    pub fn alt_held(&self) -> bool {
        self.lalt || self.ralt
    }

    #[inline]
    pub fn shift_held(&self) -> bool {
        self.lshift || self.rshift
    }

    #[inline]
    pub fn meta_held(&self) -> bool {
        self.lmeta || self.rmeta
    }

    /// Feed a key event into the matcher.
    /// Updates internal modifier key tracking.
    /// Returns `Some(HotkeyAction)` if this event triggered a configured hotkey.
    pub fn process_key(&mut self, key: KeyCode, pressed: bool) -> Option<HotkeyAction> {
        // 1. Maintain modifier states
        match key {
            KeyCode::KEY_LEFTCTRL   => self.lctrl = pressed,
            KeyCode::KEY_RIGHTCTRL  => self.rctrl = pressed,
            KeyCode::KEY_LEFTALT    => self.lalt = pressed,
            KeyCode::KEY_RIGHTALT   => self.ralt = pressed,
            KeyCode::KEY_LEFTSHIFT  => self.lshift = pressed,
            KeyCode::KEY_RIGHTSHIFT => self.rshift = pressed,
            KeyCode::KEY_LEFTMETA   => self.lmeta = pressed,
            KeyCode::KEY_RIGHTMETA  => self.rmeta = pressed,
            _ => {}
        }

        if !self.enabled {
            return None;
        }

        // Shortcuts only trigger on key-down
        if !pressed {
            return None;
        }

        let ctrl = self.ctrl_held();
        let alt = self.alt_held();
        let shift = self.shift_held();
        let meta = self.meta_held();

        // 2. Emergency escape (highest priority)
        if let Some(ref combo) = self.emergency_escape {
            if combo.matches(key, ctrl, alt, shift, meta) {
                return Some(HotkeyAction::EmergencyEscape);
            }
        }

        // Built-in hardwired emergency escape: Ctrl + Alt + Escape is ALWAYS active for safety
        if ctrl && alt && key == KeyCode::KEY_ESC {
            return Some(HotkeyAction::EmergencyEscape);
        }

        // 3. Directional spatial grid navigation: Ctrl + Alt + Arrow keys
        if ctrl && alt {
            match key {
                KeyCode::KEY_UP    => return Some(HotkeyAction::DirectionalJump(Edge::Top)),
                KeyCode::KEY_DOWN  => return Some(HotkeyAction::DirectionalJump(Edge::Bottom)),
                KeyCode::KEY_LEFT  => return Some(HotkeyAction::DirectionalJump(Edge::Left)),
                KeyCode::KEY_RIGHT => return Some(HotkeyAction::DirectionalJump(Edge::Right)),
                _ => {}
            }
        }

        // 4. Switch screen (teleport jump)
        if let Some(ref combo) = self.switch_screen {
            if combo.matches(key, ctrl, alt, shift, meta) {
                return Some(HotkeyAction::SwitchScreen);
            }
        }

        // 5. Toggle cursor lock
        if let Some(ref combo) = self.toggle_lock {
            if combo.matches(key, ctrl, alt, shift, meta) {
                return Some(HotkeyAction::ToggleCursorLock);
            }
        }

        None
    }

    /// Reset all held modifier tracking states (e.g. after mode switch or breakout).
    pub fn reset_modifiers(&mut self) {
        self.lctrl = false;
        self.rctrl = false;
        self.lalt = false;
        self.ralt = false;
        self.lshift = false;
        self.rshift = false;
        self.lmeta = false;
        self.rmeta = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hotkey_combo() {
        let scroll = HotkeyCombo::parse("ScrollLock").unwrap();
        assert_eq!(scroll.key, KeyCode::KEY_SCROLLLOCK);
        assert!(!scroll.ctrl && !scroll.alt && !scroll.shift && !scroll.meta);

        let pause = HotkeyCombo::parse("Pause / Break").unwrap();
        assert_eq!(pause.key, KeyCode::KEY_PAUSE);

        let ctrl_alt_right = HotkeyCombo::parse("Ctrl + Alt + Right").unwrap();
        assert_eq!(ctrl_alt_right.key, KeyCode::KEY_RIGHT);
        assert!(ctrl_alt_right.ctrl);
        assert!(ctrl_alt_right.alt);
        assert!(!ctrl_alt_right.shift);

        let disabled = HotkeyCombo::parse("Disabled");
        assert!(disabled.is_none());

        let none = HotkeyCombo::parse("None");
        assert!(none.is_none());
    }

    #[test]
    fn test_hotkey_matcher_triggers() {
        let cfg = HotkeyConfig {
            enabled:            true,
            toggle_cursor_lock: "ScrollLock".into(),
            switch_screen:      "Ctrl+Alt+Tab".into(),
            emergency_escape:   "Ctrl+Alt+Escape".into(),
        };

        let mut matcher = HotkeyMatcher::from_config(&cfg);

        // Standalone ScrollLock triggers cursor lock toggle
        assert_eq!(
            matcher.process_key(KeyCode::KEY_SCROLLLOCK, true),
            Some(HotkeyAction::ToggleCursorLock)
        );
        assert_eq!(matcher.process_key(KeyCode::KEY_SCROLLLOCK, false), None);

        // Hold LeftCtrl, then LeftAlt, then Right Arrow -> DirectionalJump
        assert_eq!(matcher.process_key(KeyCode::KEY_LEFTCTRL, true), None);
        assert!(matcher.ctrl_held());
        assert_eq!(matcher.process_key(KeyCode::KEY_LEFTALT, true), None);
        assert!(matcher.alt_held());

        assert_eq!(
            matcher.process_key(KeyCode::KEY_RIGHT, true),
            Some(HotkeyAction::DirectionalJump(Edge::Right))
        );

        // Switch screen via Tab while Ctrl+Alt are held
        assert_eq!(
            matcher.process_key(KeyCode::KEY_TAB, true),
            Some(HotkeyAction::SwitchScreen)
        );

        // Emergency escape via Escape while Ctrl and Alt are still held
        assert_eq!(
            matcher.process_key(KeyCode::KEY_ESC, true),
            Some(HotkeyAction::EmergencyEscape)
        );

        // Release Ctrl and Alt
        matcher.process_key(KeyCode::KEY_LEFTCTRL, false);
        matcher.process_key(KeyCode::KEY_LEFTALT, false);
        assert!(!matcher.ctrl_held());
        assert!(!matcher.alt_held());

        // Pressing Right Arrow alone should not trigger switch
        assert_eq!(matcher.process_key(KeyCode::KEY_RIGHT, true), None);
    }
}
