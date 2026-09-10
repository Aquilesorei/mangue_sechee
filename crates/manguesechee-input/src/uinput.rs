//! Virtual mouse and keyboard via Linux uinput.
//! Requires write access to `/dev/uinput` (uinput group or root).

use anyhow::Context;
use evdev::{
    uinput::VirtualDeviceBuilder, AttributeSet, EventType,
    InputEvent as EvdevEvent, Key, RelativeAxisType,
};
use manguesechee_core::events::{InputEvent, MouseButton};

const SYN_REPORT: u16 = 0;

fn syn() -> EvdevEvent {
    EvdevEvent::new(EventType::SYNCHRONIZATION, SYN_REPORT, 0)
}

// ── MouseInjector ─────────────────────────────────────────────────────────────

pub struct MouseInjector {
    device: evdev::uinput::VirtualDevice,
}

impl MouseInjector {
    pub fn new() -> anyhow::Result<Self> {
        let mut keys: AttributeSet<Key> = AttributeSet::new();
        for k in [Key::BTN_LEFT, Key::BTN_RIGHT, Key::BTN_MIDDLE,
                  Key::BTN_SIDE, Key::BTN_EXTRA] {
            keys.insert(k);
        }

        let mut axes: AttributeSet<RelativeAxisType> = AttributeSet::new();
        for a in [RelativeAxisType::REL_X, RelativeAxisType::REL_Y,
                  RelativeAxisType::REL_WHEEL, RelativeAxisType::REL_HWHEEL] {
            axes.insert(a);
        }

        let device = VirtualDeviceBuilder::new()
            .context("VirtualDeviceBuilder")?
            .name("Manguesechee Virtual Mouse")
            .with_keys(&keys).context("set mouse keys")?
            .with_relative_axes(&axes).context("set rel axes")?
            .build().context("build mouse device")?;

        Ok(Self { device })
    }

    pub fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()> {
        match event {
            InputEvent::MouseMove { dx, dy } => {
                let mut evs: Vec<EvdevEvent> = Vec::with_capacity(3);
                if *dx != 0 { evs.push(EvdevEvent::new(EventType::RELATIVE, RelativeAxisType::REL_X.0, *dx)); }
                if *dy != 0 { evs.push(EvdevEvent::new(EventType::RELATIVE, RelativeAxisType::REL_Y.0, *dy)); }
                evs.push(syn());
                self.device.emit(&evs).context("emit MouseMove")?;
            }
            InputEvent::MouseButton { button, pressed } => {
                let code = match button {
                    MouseButton::Left     => Key::BTN_LEFT.code(),
                    MouseButton::Right    => Key::BTN_RIGHT.code(),
                    MouseButton::Middle   => Key::BTN_MIDDLE.code(),
                    MouseButton::Other(4) => Key::BTN_SIDE.code(),
                    MouseButton::Other(5) => Key::BTN_EXTRA.code(),
                    MouseButton::Other(_) => return Ok(()),
                };
                self.device.emit(&[
                    EvdevEvent::new(EventType::KEY, code, if *pressed { 1 } else { 0 }),
                    syn(),
                ]).context("emit MouseButton")?;
            }
            InputEvent::MouseScroll { dx, dy } => {
                let mut evs: Vec<EvdevEvent> = Vec::with_capacity(3);
                if *dy != 0.0 { evs.push(EvdevEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL.0, *dy as i32)); }
                if *dx != 0.0 { evs.push(EvdevEvent::new(EventType::RELATIVE, RelativeAxisType::REL_HWHEEL.0, *dx as i32)); }
                evs.push(syn());
                self.device.emit(&evs).context("emit MouseScroll")?;
            }
            _ => {}
        }
        Ok(())
    }
}

// ── KeyboardInjector ──────────────────────────────────────────────────────────

pub struct KeyboardInjector {
    device: evdev::uinput::VirtualDevice,
}

impl KeyboardInjector {
    pub fn new() -> anyhow::Result<Self> {
        // Register all keys 0x00..=0x2ff (covers standard + media keys).
        // The kernel ignores unknown codes so over-declaring is safe.
        let mut keys: AttributeSet<Key> = AttributeSet::new();
        for code in 0x00u16..=0x2ffu16 {
            keys.insert(Key::new(code));
        }

        let device = VirtualDeviceBuilder::new()
            .context("VirtualDeviceBuilder keyboard")?
            .name("Manguesechee Virtual Keyboard")
            .with_keys(&keys).context("set keyboard keys")?
            .build().context("build keyboard device")?;

        Ok(Self { device })
    }

    pub fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()> {
        match event {
            InputEvent::Key { key, pressed } => {
                self.device.emit(&[
                    EvdevEvent::new(EventType::KEY, key.0, if *pressed { 1 } else { 0 }),
                    syn(),
                ]).context("emit Key")?;
            }
            InputEvent::KeySync { pressed_keys } => {
                // Release all keys, then press only those in pressed_keys.
                // Simple approach: emit a release for each code in 0..=0x2ff,
                // then press the ones that should be held.
                // In practice this is only called once on connect so the cost is fine.
                let mut evs: Vec<EvdevEvent> = Vec::new();
                for code in 0x00u16..=0x2ffu16 {
                    evs.push(EvdevEvent::new(EventType::KEY, code, 0));
                }
                for kc in pressed_keys {
                    evs.push(EvdevEvent::new(EventType::KEY, kc.0, 1));
                }
                evs.push(syn());
                self.device.emit(&evs).context("emit KeySync")?;
            }
            _ => {}
        }
        Ok(())
    }
}
