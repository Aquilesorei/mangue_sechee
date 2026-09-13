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

use std::collections::HashSet;


pub struct MouseInjector {
    device:       evdev::uinput::VirtualDevice,
    held_buttons: HashSet<u16>,
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

        let builder = VirtualDeviceBuilder::new()
            .map_err(|e| anyhow::anyhow!("VirtualDeviceBuilder: {e} (permission denied on /dev/uinput. Ensure permissions are set via sudo ./dist/update.sh or sudo chmod 0666 /dev/uinput)"))?;

        let device = builder
            .name("Manguesechee Virtual Mouse")
            .with_keys(&keys).context("set mouse keys")?
            .with_relative_axes(&axes).context("set rel axes")?
            .build().context("build mouse device")?;

        Ok(Self {
            device,
            held_buttons: HashSet::new(),
        })
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
                if *pressed {
                    self.held_buttons.insert(code);
                } else {
                    self.held_buttons.remove(&code);
                }
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

    pub fn release_all(&mut self) -> anyhow::Result<()> {
        let mut evs: Vec<EvdevEvent> = Vec::new();
        for &code in &self.held_buttons {
            evs.push(EvdevEvent::new(EventType::KEY, code, 0));
        }
        for k in [Key::BTN_LEFT, Key::BTN_RIGHT, Key::BTN_MIDDLE, Key::BTN_SIDE, Key::BTN_EXTRA] {
            if !self.held_buttons.contains(&k.code()) {
                evs.push(EvdevEvent::new(EventType::KEY, k.code(), 0));
            }
        }
        evs.push(syn());
        let _ = self.device.emit(&evs);
        self.held_buttons.clear();
        Ok(())
    }
}

impl Drop for MouseInjector {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}


pub struct KeyboardInjector {
    device:    evdev::uinput::VirtualDevice,
    held_keys: HashSet<u16>,
}

impl KeyboardInjector {
    pub fn new() -> anyhow::Result<Self> {
        // Register all keys 0x00..=0x2ff (covers standard + media keys).
        // The kernel ignores unknown codes so over-declaring is safe.
        let mut keys: AttributeSet<Key> = AttributeSet::new();
        for code in 0x00u16..=0x2ffu16 {
            keys.insert(Key::new(code));
        }

        let builder = VirtualDeviceBuilder::new()
            .map_err(|e| anyhow::anyhow!("VirtualDeviceBuilder: {e} (permission denied on /dev/uinput. Ensure permissions are set via sudo ./dist/update.sh or sudo chmod 0666 /dev/uinput)"))?;

        let device = builder
            .name("Manguesechee Virtual Keyboard")
            .with_keys(&keys).context("set keyboard keys")?
            .build().context("build keyboard device")?;

        Ok(Self {
            device,
            held_keys: HashSet::new(),
        })
    }

    pub fn inject(&mut self, event: &InputEvent) -> anyhow::Result<()> {
        match event {
            InputEvent::Key { key, pressed } => {
                if *pressed {
                    self.held_keys.insert(key.0);
                } else {
                    self.held_keys.remove(&key.0);
                }
                self.device.emit(&[
                    EvdevEvent::new(EventType::KEY, key.0, if *pressed { 1 } else { 0 }),
                    syn(),
                ]).context("emit Key")?;
            }
            InputEvent::KeySync { pressed_keys } => {
                let target_set: HashSet<u16> = pressed_keys.iter().map(|k| k.0).collect();
                let mut evs: Vec<EvdevEvent> = Vec::new();
                for &code in &self.held_keys {
                    if !target_set.contains(&code) {
                        evs.push(EvdevEvent::new(EventType::KEY, code, 0));
                    }
                }
                for &code in &target_set {
                    if !self.held_keys.contains(&code) {
                        evs.push(EvdevEvent::new(EventType::KEY, code, 1));
                    }
                }
                if !evs.is_empty() {
                    evs.push(syn());
                    self.device.emit(&evs).context("emit KeySync")?;
                }
                self.held_keys = target_set;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn release_all(&mut self) -> anyhow::Result<()> {
        if !self.held_keys.is_empty() {
            let mut evs: Vec<EvdevEvent> = Vec::with_capacity(self.held_keys.len() + 1);
            for &code in &self.held_keys {
                evs.push(EvdevEvent::new(EventType::KEY, code, 0));
            }
            evs.push(syn());
            let _ = self.device.emit(&evs);
            self.held_keys.clear();
        }
        Ok(())
    }
}

impl Drop for KeyboardInjector {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}

