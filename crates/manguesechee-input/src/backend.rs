use manguesechee_core::events::InputEvent;

/// Unified input backend trait — hides Wayland / uinput / X11 details.
pub trait InputBackend: Send + Sync {
    fn capture(&self) -> anyhow::Result<InputEvent>;
    fn inject(&self, event: InputEvent) -> anyhow::Result<()>;
}
