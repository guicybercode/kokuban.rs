pub mod atlas;
pub mod metal;
pub mod shaders;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub fg_color: u32,
    pub bg_color: u32,
}

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
