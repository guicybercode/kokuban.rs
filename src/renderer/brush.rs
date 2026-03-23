use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::*;

pub const BRUSH_TEX_WIDTH: u32 = 16;
pub const BRUSH_TEX_HEIGHT: u32 = 256;
pub const BRUSH_VARIANTS: usize = 4;

pub struct BrushRenderer {
    pub texture: Retained<ProtocolObject<dyn MTLTexture>>,
    pub variant_v_offsets: [f32; BRUSH_VARIANTS],
    pub variant_v_height: f32,
}

impl BrushRenderer {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Self {
        let total_height = BRUSH_TEX_HEIGHT * BRUSH_VARIANTS as u32;
        let mut pixels = vec![0u8; (BRUSH_TEX_WIDTH * total_height) as usize];

        for variant in 0..BRUSH_VARIANTS {
            let offset = variant as u32 * BRUSH_TEX_HEIGHT;
            generate_brush_variant(
                &mut pixels,
                BRUSH_TEX_WIDTH,
                offset,
                BRUSH_TEX_HEIGHT,
                BRUSH_TEX_WIDTH,
                variant as u32 * 7919 + 42,
            );
        }

        let tex_desc =
            unsafe {
                MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                    MTLPixelFormat::R8Unorm,
                    BRUSH_TEX_WIDTH as usize,
                    total_height as usize,
                    false,
                )
            };
        tex_desc.setUsage(MTLTextureUsage::ShaderRead);

        let texture = device
            .newTextureWithDescriptor(&tex_desc)
            .expect("Failed to create brush texture");

        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: BRUSH_TEX_WIDTH as usize,
                height: total_height as usize,
                depth: 1,
            },
        };

        unsafe {
            let bytes_ptr =
                std::ptr::NonNull::new(pixels.as_ptr() as *mut std::ffi::c_void).unwrap();
            texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                bytes_ptr,
                BRUSH_TEX_WIDTH as usize,
            );
        }

        let fh = total_height as f32;
        let variant_v_height = BRUSH_TEX_HEIGHT as f32 / fh;
        let variant_v_offsets = [
            0.0 * variant_v_height,
            1.0 * variant_v_height,
            2.0 * variant_v_height,
            3.0 * variant_v_height,
        ];

        Self {
            texture,
            variant_v_offsets,
            variant_v_height,
        }
    }

    /// UV rect for a given brush variant.
    pub fn uv_rect(&self, variant: usize) -> (f32, f32, f32, f32) {
        let v = variant % BRUSH_VARIANTS;
        (0.0, self.variant_v_offsets[v], 1.0, self.variant_v_height)
    }
}

fn generate_brush_variant(
    pixels: &mut [u8],
    tex_width: u32,
    y_offset: u32,
    height: u32,
    width: u32,
    seed: u32,
) {
    for y in 0..height {
        let t = y as f32 / height as f32;

        // Stroke width envelope: thick in middle, thin at ends
        let envelope = (std::f32::consts::PI * t).sin();
        let base_width = envelope * 0.85 + 0.15;

        // Deterministic edge noise
        let noise = pseudo_noise(y, seed) * 0.12;
        let stroke_width = (base_width + noise).clamp(0.05, 1.0);

        for x in 0..width {
            let nx = ((x as f32 / width as f32) - 0.5).abs() * 2.0;
            let d = nx / stroke_width;

            let alpha = if d < 0.6 {
                1.0
            } else if d < 1.0 {
                let t = (d - 0.6) / 0.4;
                (1.0 - t * t).max(0.0)
            } else {
                0.0
            };

            let idx = ((y_offset + y) * tex_width + x) as usize;
            pixels[idx] = (alpha * 255.0) as u8;
        }
    }
}

fn pseudo_noise(y: u32, seed: u32) -> f32 {
    let n = y
        .wrapping_mul(1103515245)
        .wrapping_add(seed.wrapping_mul(12345));
    let n = n ^ (n >> 16);
    (n & 0xFFFF) as f32 / 65535.0 * 2.0 - 1.0
}
