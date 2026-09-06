/// Original application artwork, embedded so launches do not depend on asset paths.
pub const PNG: &[u8] = include_bytes!("../assets/kokuban-icon.png");

#[cfg(target_os = "linux")]
pub const APP_ID: &str = "io.github.guicybercode.kokuban";
