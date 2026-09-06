pub mod keybind;
pub mod keyboard;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod mouse;
pub mod paste;
