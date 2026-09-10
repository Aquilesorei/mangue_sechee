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
}

impl MouseCapture {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        info!("opening mouse: {}", path.display());
        let device = evdev::Device::open(path)
            .with_context(|| format!("open {}", path.display()))?;
        info!("  → {}", device.name().unwrap_or("<unknown>"));
        let stream = device.into_event_stream()?;
        Ok(Self { stream, pending_dx: 0, pending_dy: 0 })
    }

    /// Exclusively grab the device — local compositor stops seeing events.
    pub fn grab(&mut self) -> anyhow::Result<()> {
        self.stream.device_mut().grab()?;
        info!("mouse grabbed");
        Ok(())
    }

    /// Release the exclusive grab — local compositor sees events again.
    pub fn ungrab(&mut self) -> anyhow::Result<()> {
        self.stream.device_mut().ungrab()?;
        info!("mouse ungrabbed");
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

// ── KeyboardCapture ───────────────────────────────────────────────────────────

pub struct KeyboardCapture {
    stream: evdev::EventStream,
}

impl KeyboardCapture {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        info!("opening keyboard: {}", path.display());
        let device = evdev::Device::open(path)
            .with_context(|| format!("open {}", path.display()))?;
        info!("  → {}", device.name().unwrap_or("<unknown>"));
        let stream = device.into_event_stream()?;
        Ok(Self { stream })
    }

    pub fn grab(&mut self) -> anyhow::Result<()> {
        self.stream.device_mut().grab()?;
        info!("keyboard grabbed");
        Ok(())
    }

    pub fn ungrab(&mut self) -> anyhow::Result<()> {
        self.stream.device_mut().ungrab()?;
        info!("keyboard ungrabbed");
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

fn is_mouse_button(key: Key) -> bool {
    (0x110..=0x11f).contains(&key.code())
}

// ── Device discovery ──────────────────────────────────────────────────────────

pub fn find_mouse() -> anyhow::Result<PathBuf> {
    find_device("mouse", |dev| {
        dev.supported_relative_axes()
            .map(|a| a.contains(RelativeAxisType::REL_X) && a.contains(RelativeAxisType::REL_Y))
            .unwrap_or(false)
    })
}

pub fn find_keyboard() -> anyhow::Result<PathBuf> {
    find_device("keyboard", |dev| {
        dev.supported_keys()
            .map(|k| k.contains(Key::KEY_A))
            .unwrap_or(false)
    })
}

fn find_device(label: &str, predicate: impl Fn(&evdev::Device) -> bool) -> anyhow::Result<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir("/dev/input")
        .context("read /dev/input")?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("event"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if let Ok(device) = evdev::Device::open(&path) {
            if predicate(&device) {
                info!("found {label}: {} ({})", path.display(), device.name().unwrap_or("<unknown>"));
                return Ok(path);
            }
        }
    }
    anyhow::bail!("no {label} found in /dev/input — is your user in the 'input' group?")
}
