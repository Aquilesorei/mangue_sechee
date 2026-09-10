//! Mouse and keyboard capture via Linux evdev.
//!
//! Supports exclusive grab (EVIOCGRAB) so that while the controller is
//! forwarding, the local compositor does NOT see the events.
//! Call `grab()` when entering forwarding mode, `ungrab()` when returning.

use anyhow::Context;
use evdev::{InputEventKind, Key, RelativeAxisType};
use manguesechee_core::events::{InputEvent, KeyCode, MouseButton};
use std::path::{Path, PathBuf};
use tracing::info;

// ── MouseCapture ──────────────────────────────────────────────────────────────

pub struct MouseCapture {
    stream:     evdev::EventStream,
    pending_dx: i32,
    pending_dy: i32,
    is_grabbed: bool,
}

impl MouseCapture {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        info!("opening mouse: {}", path.display());
        let device = evdev::Device::open(path)
            .with_context(|| format!("open {}", path.display()))?;
        info!("  → {}", device.name().unwrap_or("<unknown>"));
        let stream = device.into_event_stream()?;
        Ok(Self { stream, pending_dx: 0, pending_dy: 0, is_grabbed: false })
    }

    /// Exclusively grab the device — local compositor stops seeing events.
    pub fn grab(&mut self) -> anyhow::Result<()> {
        if !self.is_grabbed {
            self.stream.device_mut().grab()?;
            self.is_grabbed = true;
            info!("mouse grabbed");
        }
        Ok(())
    }

    /// Release the exclusive grab — local compositor sees events again.
    pub fn ungrab(&mut self) -> anyhow::Result<()> {
        if self.is_grabbed {
            let _ = self.stream.device_mut().ungrab();
            self.is_grabbed = false;
            info!("mouse ungrabbed");
        }
        Ok(())
    }

    pub async fn next_event(&mut self) -> anyhow::Result<InputEvent> {
        loop {
            let ev = self.stream.next_event().await?;
            match ev.kind() {
                InputEventKind::Synchronization(_) => {
                    if self.pending_dx != 0 || self.pending_dy != 0 {
                        let e = InputEvent::MouseMove {
                            dx: self.pending_dx,
                            dy: self.pending_dy,
                        };
                        self.pending_dx = 0;
                        self.pending_dy = 0;
                        return Ok(e);
                    }
                }
                InputEventKind::RelAxis(axis) => match axis {
                    RelativeAxisType::REL_X      => self.pending_dx += ev.value(),
                    RelativeAxisType::REL_Y      => self.pending_dy += ev.value(),
                    RelativeAxisType::REL_WHEEL  => return Ok(InputEvent::MouseScroll { dx: 0.0, dy: ev.value() as f32 }),
                    RelativeAxisType::REL_HWHEEL => return Ok(InputEvent::MouseScroll { dx: ev.value() as f32, dy: 0.0 }),
                    _ => {}
                },
                InputEventKind::Key(key) => {
                    let pressed = ev.value() != 0;
                    let button = match key {
                        Key::BTN_LEFT   => MouseButton::Left,
                        Key::BTN_RIGHT  => MouseButton::Right,
                        Key::BTN_MIDDLE => MouseButton::Middle,
                        Key::BTN_SIDE   => MouseButton::Other(4),
                        Key::BTN_EXTRA  => MouseButton::Other(5),
                        _               => continue,
                    };
                    return Ok(InputEvent::MouseButton { button, pressed });
                }
                _ => {}
            }
        }
    }
}

impl Drop for MouseCapture {
    fn drop(&mut self) {
        let _ = self.ungrab();
    }
}

// ── KeyboardCapture ───────────────────────────────────────────────────────────

pub struct KeyboardCapture {
    stream:     evdev::EventStream,
    is_grabbed: bool,
}

impl KeyboardCapture {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        info!("opening keyboard: {}", path.display());
        let device = evdev::Device::open(path)
            .with_context(|| format!("open {}", path.display()))?;
        info!("  → {}", device.name().unwrap_or("<unknown>"));
        let stream = device.into_event_stream()?;
        Ok(Self { stream, is_grabbed: false })
    }

    pub fn grab(&mut self) -> anyhow::Result<()> {
        if !self.is_grabbed {
            self.stream.device_mut().grab()?;
            self.is_grabbed = true;
            info!("keyboard grabbed");
        }
        Ok(())
    }

    pub fn ungrab(&mut self) -> anyhow::Result<()> {
        if self.is_grabbed {
            let _ = self.stream.device_mut().ungrab();
            self.is_grabbed = false;
            info!("keyboard ungrabbed");
        }
        Ok(())
    }

    pub fn snapshot_pressed(path: &Path) -> anyhow::Result<Vec<KeyCode>> {
        let device = evdev::Device::open(path)?;
        let held = device
            .get_key_state()
            .context("read key state")?
            .iter()
            .filter(|k| !is_mouse_button(*k))
            .map(|k| KeyCode(k.code()))
            .collect();
        Ok(held)
    }

    pub async fn next_event(&mut self) -> anyhow::Result<InputEvent> {
        loop {
            let ev = self.stream.next_event().await?;
            if let InputEventKind::Key(key) = ev.kind() {
                if is_mouse_button(key) { continue; }
                match ev.value() {
                    0 => return Ok(InputEvent::Key { key: KeyCode(key.code()), pressed: false }),
                    1 => return Ok(InputEvent::Key { key: KeyCode(key.code()), pressed: true }),
                    _ => continue,
                }
            }
        }
    }
}

impl Drop for KeyboardCapture {
    fn drop(&mut self) {
        let _ = self.ungrab();
    }
}

fn is_mouse_button(key: Key) -> bool {
    (0x110..=0x11f).contains(&key.code())
}

// ── Device discovery ──────────────────────────────────────────────────────────

fn is_ignored_device(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("manguesechee")
        || lower.contains("virtual")
        || lower.contains("uinput")
        || lower.contains("stylus")
        || lower.contains("unknown")
        || lower.contains("power button")
        || lower.contains("sleep button")
        || lower.contains("video bus")
        || lower.contains("hotkey")
        || lower.contains("lid switch")
        || lower.contains("earpods")
        || lower.contains("headphone")
        || lower.contains("mic")
        || lower.contains("speaker")
}

fn sorted_event_entries() -> anyhow::Result<Vec<std::fs::DirEntry>> {
    let mut entries: Vec<_> = std::fs::read_dir("/dev/input")
        .context("read /dev/input")?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
        .collect();

    // Natural numeric sort: event0, event1, ..., event9, event10, ...
    entries.sort_by_key(|e| {
        e.file_name()
            .to_str()
            .and_then(|s| s.strip_prefix("event"))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });
    Ok(entries)
}

pub fn find_all_mice() -> Vec<PathBuf> {
    let entries = match sorted_event_entries() {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut mice = Vec::new();
    for entry in entries {
        let path = entry.path();
        if let Ok(device) = evdev::Device::open(&path) {
            let name = device.name().unwrap_or("<unknown>");
            if is_ignored_device(name) {
                continue;
            }
            let has_rel = device
                .supported_relative_axes()
                .map(|a| a.contains(RelativeAxisType::REL_X) && a.contains(RelativeAxisType::REL_Y))
                .unwrap_or(false);
            let has_btn = device
                .supported_keys()
                .map(|k| k.contains(Key::BTN_LEFT))
                .unwrap_or(false);

            if has_rel && has_btn {
                info!("found mouse: {} ({})", path.display(), name);
                mice.push(path);
            }
        }
    }
    mice
}

pub fn find_all_keyboards() -> Vec<PathBuf> {
    let entries = match sorted_event_entries() {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut keyboards = Vec::new();
    for entry in entries {
        let path = entry.path();
        if let Ok(device) = evdev::Device::open(&path) {
            let name = device.name().unwrap_or("<unknown>");
            if is_ignored_device(name) {
                continue;
            }
            let has_alpha = device
                .supported_keys()
                .map(|k| {
                    k.contains(Key::KEY_A)
                        && k.contains(Key::KEY_Z)
                        && k.contains(Key::KEY_ENTER)
                })
                .unwrap_or(false);

            if has_alpha {
                info!("found keyboard: {} ({})", path.display(), name);
                keyboards.push(path);
            }
        }
    }
    keyboards
}

pub fn find_mouse() -> anyhow::Result<PathBuf> {
    find_all_mice().into_iter().next().context("no mouse found in /dev/input")
}

pub fn find_keyboard() -> anyhow::Result<PathBuf> {
    find_all_keyboards().into_iter().next().context("no keyboard found in /dev/input")
}
