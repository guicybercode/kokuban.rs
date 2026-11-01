pub mod box_drawing;
#[cfg(target_os = "macos")]
pub mod braille;
#[cfg(target_os = "macos")]
pub mod brush;
pub(crate) mod image_decode;
// Kitty animation state machine driven by `software_image_store` (Linux);
// the Metal store only borrows `AnimationError`.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub(crate) mod image_animation;
#[cfg(target_os = "macos")]
pub mod image_store;
#[cfg(any(target_os = "linux", test))]
pub(crate) mod software_image_store;
#[cfg(target_os = "linux")]
pub(crate) use software_image_store as image_store;
pub mod kitty_handler;
#[cfg(target_os = "macos")]
pub mod metal;
#[cfg(target_os = "macos")]
pub mod shaders;

// Metal vertex layout; the Linux path rasterizes in software.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub fg_color: u32,
    pub bg_color: u32,
}

#[cfg(target_os = "macos")]
impl Vertex {
    pub fn new(x: f32, y: f32, u: f32, v: f32, fg: u32, bg: u32) -> Self {
        Self {
            position: [x, y],
            uv: [u, v],
            fg_color: fg,
            bg_color: bg,
        }
    }
}
