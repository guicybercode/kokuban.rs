// Keybinds are dispatched by the macOS window today; the Linux window will
// consume the same table once its pane port lands.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub mod keybind;
pub mod keyboard;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod mouse;
// `app::window` has its own paste encoder; only `linux_window` uses this one.
#[cfg(any(target_os = "linux", test))]
pub mod paste;
