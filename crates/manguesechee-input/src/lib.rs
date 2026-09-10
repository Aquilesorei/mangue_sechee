pub mod backend;
pub mod capture;
pub mod injection;
pub mod uinput;
pub mod wayland;

pub use capture::{find_all_keyboards, find_all_mice, find_keyboard, find_mouse, KeyboardCapture, MouseCapture};
pub use uinput::{KeyboardInjector, MouseInjector};
pub use wayland::{detect_screen_size, try_detect_screen_size};
