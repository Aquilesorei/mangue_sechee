//! Mouse and keyboard capture via Linux evdev.
//!
//! Supports exclusive grab (EVIOCGRAB) so that while the controller is
//! forwarding, the local compositor does NOT see the events.
//! Call `grab()` when entering forwarding mode, `ungrab()` when returning.

use anyhow::Context;
use evdev::{AbsoluteAxisType, InputEventKind, Key, PropType, RelativeAxisType};
use manguesechee_core::events::{InputEvent, KeyCode, MouseButton};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::info;

// ── MouseCapture ──────────────────────────────────────────────────────────────

pub struct MouseCapture {
    stream:               evdev::EventStream,
    pending_dx:           i32,
    pending_dy:           i32,
    pending_scroll_x:     f32,
    pending_scroll_y:     f32,
    is_grabbed:           bool,

    // Touchpad (pavé tactile) tracking state
    is_touchpad:          bool,
    touch_active:         bool,
    pending_ungrab:       bool,
    pending_ungrab_since: Option<Instant>,
    finger_count:         u8,
    active_slot:          usize,
    cur_x:                Option<i32>,
    cur_y:                Option<i32>,
    last_x:               Option<i32>,
    last_y:               Option<i32>,
}

impl MouseCapture {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        info!("opening pointer/touchpad device: {}", path.display());
        let device = evdev::Device::open(path)
            .with_context(|| format!("open {}", path.display()))?;
        info!("  → {}", device.name().unwrap_or("<unknown>"));
        let is_touchpad = is_touchpad_device(&device);
        let stream = device.into_event_stream()?;
        Ok(Self {
            stream,
            pending_dx: 0,
            pending_dy: 0,
            pending_scroll_x: 0.0,
            pending_scroll_y: 0.0,
            is_grabbed: false,
            is_touchpad,
            touch_active: false,
            pending_ungrab: false,
            pending_ungrab_since: None,
            finger_count: 0,
            active_slot: 0,
            cur_x: None,
            cur_y: None,
            last_x: None,
            last_y: None,
        })
    }

    /// Exclusively grab the device — local compositor stops seeing events.
    pub fn grab(&mut self) -> anyhow::Result<()> {
        self.pending_ungrab = false;
        self.pending_ungrab_since = None;
        if !self.is_grabbed {
            self.stream.device_mut().grab()?;
            self.is_grabbed = true;
            info!("pointer/touchpad grabbed");
        }
        Ok(())
    }

    /// Release the exclusive grab — local compositor sees events again.
    /// For touchpads, if a touch is actively in progress, we defer ungrabbing
    /// until the finger is lifted to prevent sending an orphan touch-up event to KWin/libinput,
    /// which would cause a "double tracking ID -1" bug and disable the device.
    pub fn ungrab(&mut self) -> anyhow::Result<()> {
        if self.is_grabbed {
            if self.is_touchpad && self.touch_active {
                self.pending_ungrab = true;
                self.pending_ungrab_since = Some(Instant::now());
                info!("touchpad ungrab deferred until finger liftoff");
            } else {
                let _ = self.stream.device_mut().ungrab();
                self.is_grabbed = false;
                self.pending_ungrab = false;
                self.pending_ungrab_since = None;
                info!("pointer/touchpad ungrabbed");
            }
        }
        Ok(())
    }

    pub async fn next_event(&mut self) -> anyhow::Result<InputEvent> {
        loop {
            // Check if deferred ungrab timed out even if no new hardware events arrive
            if self.pending_ungrab {
                let timed_out = self.pending_ungrab_since.map_or(false, |t| t.elapsed() > Duration::from_millis(250));
                if !self.touch_active || timed_out {
                    let _ = self.stream.device_mut().ungrab();
                    self.is_grabbed = false;
                    self.pending_ungrab = false;
                    self.pending_ungrab_since = None;
                    info!("deferred touchpad ungrab executed (touch_active={}, timed_out={})", self.touch_active, timed_out);
                }
            }

            // Flush any pending scroll events
            if self.pending_scroll_x.abs() >= 1.0 || self.pending_scroll_y.abs() >= 1.0 {
                let sx = if self.pending_scroll_x.abs() >= 1.0 {
                    let s = self.pending_scroll_x.trunc();
                    self.pending_scroll_x -= s;
                    s
                } else {
                    0.0
                };
                let sy = if self.pending_scroll_y.abs() >= 1.0 {
                    let s = self.pending_scroll_y.trunc();
                    self.pending_scroll_y -= s;
                    s
                } else {
                    0.0
                };
                return Ok(InputEvent::MouseScroll { dx: sx, dy: sy });
            }

            // Flush any pending mouse movement events
            if self.pending_dx != 0 || self.pending_dy != 0 {
                let e = InputEvent::MouseMove {
                    dx: self.pending_dx,
                    dy: self.pending_dy,
                };
                self.pending_dx = 0;
                self.pending_dy = 0;
                return Ok(e);
            }

            let ev = self.stream.next_event().await?;
            match ev.kind() {
                InputEventKind::Synchronization(_) => {
                    // Check if deferred ungrab is pending and finger is lifted
                    if self.pending_ungrab && !self.touch_active {
                        let _ = self.stream.device_mut().ungrab();
                        self.is_grabbed = false;
                        self.pending_ungrab = false;
                        self.pending_ungrab_since = None;
                        info!("deferred touchpad ungrab executed upon finger release");
                    }

                    // Compute touchpad movement / scroll deltas
                    if self.touch_active {
                        if let (Some(cx), Some(cy)) = (self.cur_x, self.cur_y) {
                            if let (Some(lx), Some(ly)) = (self.last_x, self.last_y) {
                                let dx = cx - lx;
                                let dy = cy - ly;

                                if dx != 0 || dy != 0 {
                                    if self.finger_count >= 2 {
                                        // Two-finger scroll: accumulate scroll
                                        // In standard evdev coordinates, y increases downward.
                                        // Moving fingers down (dy > 0) scrolls down (negative delta in wheel ticks).
                                        self.pending_scroll_y -= (dy as f32) / 16.0;
                                        self.pending_scroll_x -= (dx as f32) / 16.0;
                                    } else {
                                        // Single-finger: pointer movement
                                        // Raw touchpad units are ~12 counts/mm (~300 DPI) vs standard mouse (1200-1600 DPI).
                                        // Scale by 2.5x to match screen resolution and allow smooth edge crossings.
                                        let (scaled_dx, scaled_dy) = if self.is_touchpad {
                                            ((dx as f32 * 2.5).round() as i32, (dy as f32 * 2.5).round() as i32)
                                        } else {
                                            (dx, dy)
                                        };
                                        self.pending_dx += scaled_dx;
                                        self.pending_dy += scaled_dy;
                                    }
                                }
                            }
                            self.last_x = Some(cx);
                            self.last_y = Some(cy);
                        }
                    }

                    if self.pending_scroll_x.abs() >= 1.0 || self.pending_scroll_y.abs() >= 1.0 {
                        let sx = if self.pending_scroll_x.abs() >= 1.0 {
                            let s = self.pending_scroll_x.trunc();
                            self.pending_scroll_x -= s;
                            s
                        } else {
                            0.0
                        };
                        let sy = if self.pending_scroll_y.abs() >= 1.0 {
                            let s = self.pending_scroll_y.trunc();
                            self.pending_scroll_y -= s;
                            s
                        } else {
                            0.0
                        };
                        return Ok(InputEvent::MouseScroll { dx: sx, dy: sy });
                    }

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

                InputEventKind::AbsAxis(axis) => match axis {
                    AbsoluteAxisType::ABS_MT_SLOT => {
                        self.active_slot = ev.value() as usize;
                    }
                    AbsoluteAxisType::ABS_X => {
                        self.cur_x = Some(ev.value());
                        self.touch_active = true;
                    }
                    AbsoluteAxisType::ABS_Y => {
                        self.cur_y = Some(ev.value());
                        self.touch_active = true;
                    }
                    AbsoluteAxisType::ABS_MT_POSITION_X => {
                        if self.active_slot == 0 {
                            self.cur_x = Some(ev.value());
                            self.touch_active = true;
                        }
                    }
                    AbsoluteAxisType::ABS_MT_POSITION_Y => {
                        if self.active_slot == 0 {
                            self.cur_y = Some(ev.value());
                            self.touch_active = true;
                        }
                    }
                    AbsoluteAxisType::ABS_MT_TRACKING_ID => {
                        if self.active_slot == 0 {
                            if ev.value() < 0 {
                                // Finger lifted
                                self.cur_x = None;
                                self.cur_y = None;
                                self.last_x = None;
                                self.last_y = None;
                                self.touch_active = false;
                                self.finger_count = 0;
                            } else {
                                self.touch_active = true;
                            }
                        }
                    }
                    _ => {}
                },

                InputEventKind::Key(key) => {
                    match key {
                        Key::BTN_TOUCH => {
                            let pressed = ev.value() != 0;
                            self.touch_active = pressed;
                            if !pressed {
                                self.cur_x = None;
                                self.cur_y = None;
                                self.last_x = None;
                                self.last_y = None;
                                self.finger_count = 0;
                            }
                        }
                        Key::BTN_TOOL_FINGER => {
                            let new_count = if ev.value() != 0 {
                                1
                            } else if self.finger_count == 1 {
                                0
                            } else {
                                self.finger_count
                            };
                            if new_count != self.finger_count {
                                self.finger_count = new_count;
                                self.last_x = None;
                                self.last_y = None;
                            }
                            if ev.value() != 0 {
                                self.touch_active = true;
                            }
                        }
                        Key::BTN_TOOL_DOUBLETAP => {
                            let new_count = if ev.value() != 0 {
                                2
                            } else if self.finger_count == 2 {
                                1
                            } else {
                                self.finger_count
                            };
                            if new_count != self.finger_count {
                                self.finger_count = new_count;
                                self.last_x = None;
                                self.last_y = None;
                            }
                            if ev.value() != 0 {
                                self.touch_active = true;
                            }
                        }
                        Key::BTN_TOOL_TRIPLETAP => {
                            let new_count = if ev.value() != 0 {
                                3
                            } else if self.finger_count == 3 {
                                2
                            } else {
                                self.finger_count
                            };
                            if new_count != self.finger_count {
                                self.finger_count = new_count;
                                self.last_x = None;
                                self.last_y = None;
                            }
                        }
                        Key::BTN_LEFT => {
                            let pressed = ev.value() != 0;
                            let button = if self.finger_count == 2 {
                                MouseButton::Right
                            } else {
                                MouseButton::Left
                            };
                            return Ok(InputEvent::MouseButton { button, pressed });
                        }
                        Key::BTN_RIGHT => {
                            let pressed = ev.value() != 0;
                            return Ok(InputEvent::MouseButton { button: MouseButton::Right, pressed });
                        }
                        Key::BTN_MIDDLE => {
                            let pressed = ev.value() != 0;
                            return Ok(InputEvent::MouseButton { button: MouseButton::Middle, pressed });
                        }
                        Key::BTN_SIDE => {
                            let pressed = ev.value() != 0;
                            return Ok(InputEvent::MouseButton { button: MouseButton::Other(4), pressed });
                        }
                        Key::BTN_EXTRA => {
                            let pressed = ev.value() != 0;
                            return Ok(InputEvent::MouseButton { button: MouseButton::Other(5), pressed });
                        }
                        _ => {}
                    }
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
        || lower.contains("elan2513")
}

pub fn is_touchpad_device(device: &evdev::Device) -> bool {
    let name = device.name().unwrap_or("<unknown>");
    let lower = name.to_lowercase();
    let has_abs = device
        .supported_absolute_axes()
        .map(|a| a.contains(AbsoluteAxisType::ABS_X) || a.contains(AbsoluteAxisType::ABS_MT_POSITION_X))
        .unwrap_or(false);
    let has_touch_keys = device
        .supported_keys()
        .map(|k| k.contains(Key::BTN_TOOL_FINGER) || k.contains(Key::BTN_TOUCH))
        .unwrap_or(false);
    has_abs && has_touch_keys && (lower.contains("touchpad") || lower.contains("trackpad") || lower.contains("glidepoint"))
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

fn is_mouse_or_touchpad(device: &evdev::Device) -> bool {
    let name = device.name().unwrap_or("<unknown>");
    let lower = name.to_lowercase();
    if is_ignored_device(name) || lower.contains("touchscreen") {
        return false;
    }

    // 1. Standard relative mouse (e.g. USB/Bluetooth/Wireless mouse)
    let has_rel = device
        .supported_relative_axes()
        .map(|a| a.contains(RelativeAxisType::REL_X) && a.contains(RelativeAxisType::REL_Y))
        .unwrap_or(false);
    let has_mouse_btn = device
        .supported_keys()
        .map(|k| k.contains(Key::BTN_LEFT) || k.contains(Key::BTN_RIGHT))
        .unwrap_or(false);

    if has_rel && has_mouse_btn {
        return true;
    }

    // 2. Touchpad / trackpad / pavé tactile
    let has_abs = device
        .supported_absolute_axes()
        .map(|a| {
            (a.contains(AbsoluteAxisType::ABS_X) && a.contains(AbsoluteAxisType::ABS_Y))
                || (a.contains(AbsoluteAxisType::ABS_MT_POSITION_X) && a.contains(AbsoluteAxisType::ABS_MT_POSITION_Y))
        })
        .unwrap_or(false);

    let has_touch_keys = device
        .supported_keys()
        .map(|k| {
            k.contains(Key::BTN_TOOL_FINGER)
                || k.contains(Key::BTN_TOUCH)
                || k.contains(Key::BTN_LEFT)
        })
        .unwrap_or(false);

    let props = device.properties();
    let is_direct = props.contains(PropType::DIRECT);
    let is_pointer = props.contains(PropType::POINTER)
        || props.contains(PropType::BUTTONPAD)
        || props.contains(PropType::SEMI_MT);
    let name_indicates_touchpad = lower.contains("touchpad") || lower.contains("trackpad") || lower.contains("glidepoint");

    if has_abs && has_touch_keys && !is_direct && (is_pointer || name_indicates_touchpad) {
        return true;
    }

    false
}

pub fn find_all_mice() -> Vec<PathBuf> {
    let entries = match sorted_event_entries() {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut discovered: Vec<(PathBuf, String)> = Vec::new();
    for entry in entries {
        let path = entry.path();
        if let Ok(device) = evdev::Device::open(&path) {
            let name = device.name().unwrap_or("<unknown>").to_string();
            if is_mouse_or_touchpad(&device) {
                discovered.push((path, name));
            }
        }
    }

    // Deduplicate companion devices: if we have a dedicated Touchpad node (e.g. "SYNA... Touchpad"),
    // discard the legacy/dummy companion Mouse node ("SYNA... Mouse").
    let has_touchpad = discovered.iter().any(|(_, name)| name.to_lowercase().contains("touchpad"));
    let mut mice = Vec::new();
    for (path, name) in discovered {
        let lower = name.to_lowercase();
        if has_touchpad && !lower.contains("touchpad") && (lower.contains("syna") || lower.contains("alps")) {
            info!("skipping duplicate companion pointer: {} ({})", path.display(), name);
            continue;
        }
        info!("found pointer/touchpad: {} ({})", path.display(), name);
        mice.push(path);
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
