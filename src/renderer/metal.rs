use super::atlas::{GlyphAtlas, GlyphKey};
use super::shaders::SHADER_SOURCE;
use super::Vertex;
use crate::grid::cell::{CellFlags, Color};
use crate::grid::Grid;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::*;

// Standard 256-color palette (first 16 colors)
const ANSI_COLORS: [(u8, u8, u8); 16] = [
    (0, 0, 0),       // 0 Black
    (205, 49, 49),    // 1 Red
    (13, 188, 121),   // 2 Green
    (229, 229, 16),   // 3 Yellow
    (36, 114, 200),   // 4 Blue
    (188, 63, 188),   // 5 Magenta
    (17, 168, 205),   // 6 Cyan
    (229, 229, 229),  // 7 White
    (102, 102, 102),  // 8 Bright Black
    (241, 76, 76),    // 9 Bright Red
    (35, 209, 139),   // 10 Bright Green
    (245, 245, 67),   // 11 Bright Yellow
    (59, 142, 234),   // 12 Bright Blue
    (214, 112, 214),  // 13 Bright Magenta
    (41, 184, 219),   // 14 Bright Cyan
    (229, 229, 229),  // 15 Bright White
];

pub struct MetalRenderer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline_state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    vertex_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    uniform_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    atlas_texture: Retained<ProtocolObject<dyn MTLTexture>>,
    sampler_state: Retained<ProtocolObject<dyn MTLSamplerState>>,
    default_fg: (u8, u8, u8),
    default_bg: (u8, u8, u8),
}

impl MetalRenderer {
    pub fn new(
        device: Retained<ProtocolObject<dyn MTLDevice>>,
        default_fg: (u8, u8, u8),
        default_bg: (u8, u8, u8),
    ) -> Self {
        unsafe {
            let command_queue = device.newCommandQueue().expect("Failed to create command queue");

            // Compile shaders
            let source = NSString::from_str(SHADER_SOURCE);
            let library = device
                .newLibraryWithSource_options_error(&source, None)
                .expect("Failed to compile shader library");

            let vertex_fn_name = NSString::from_str("vertex_main");
            let fragment_fn_name = NSString::from_str("fragment_main");
            let vertex_fn = library
                .newFunctionWithName(&vertex_fn_name)
                .expect("vertex_main not found");
            let fragment_fn = library
                .newFunctionWithName(&fragment_fn_name)
                .expect("fragment_main not found");

            // Vertex descriptor
            let vertex_desc = MTLVertexDescriptor::new();
            let attributes = vertex_desc.attributes();
            let layouts = vertex_desc.layouts();

            // position: float2 at offset 0
            let attr0 = attributes.objectAtIndexedSubscript(0);
            attr0.setFormat(MTLVertexFormat::Float2);
            attr0.setOffset(0);
            attr0.setBufferIndex(0);

            // uv: float2 at offset 8
            let attr1 = attributes.objectAtIndexedSubscript(1);
            attr1.setFormat(MTLVertexFormat::Float2);
            attr1.setOffset(8);
            attr1.setBufferIndex(0);

            // fg_color: uint at offset 16
            let attr2 = attributes.objectAtIndexedSubscript(2);
            attr2.setFormat(MTLVertexFormat::UInt);
            attr2.setOffset(16);
            attr2.setBufferIndex(0);

            // bg_color: uint at offset 20
            let attr3 = attributes.objectAtIndexedSubscript(3);
            attr3.setFormat(MTLVertexFormat::UInt);
            attr3.setOffset(20);
            attr3.setBufferIndex(0);

            // Layout
            let layout0 = layouts.objectAtIndexedSubscript(0);
            layout0.setStride(std::mem::size_of::<Vertex>());
            layout0.setStepFunction(MTLVertexStepFunction::PerVertex);

            // Pipeline descriptor
            let pipeline_desc = MTLRenderPipelineDescriptor::new();
            pipeline_desc.setVertexFunction(Some(&vertex_fn));
            pipeline_desc.setFragmentFunction(Some(&fragment_fn));
            pipeline_desc.setVertexDescriptor(Some(&vertex_desc));

            let color_attachment = pipeline_desc
                .colorAttachments()
                .objectAtIndexedSubscript(0);
            color_attachment.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            color_attachment.setBlendingEnabled(true);
            color_attachment.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
            color_attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            color_attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
            color_attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);

            let pipeline_state = device
                .newRenderPipelineStateWithDescriptor_error(&pipeline_desc)
                .expect("Failed to create render pipeline state");

            // Vertex buffer (pre-allocate for 80x24 grid, 12 verts per cell)
            let max_vertices = 80 * 50 * 12;
            let buffer_size = max_vertices * std::mem::size_of::<Vertex>();
            let vertex_buffer = device
                .newBufferWithLength_options(buffer_size, MTLResourceOptions::StorageModeShared)
                .expect("Failed to create vertex buffer");

            // Uniform buffer
            let uniform_buffer = device
                .newBufferWithLength_options(16, MTLResourceOptions::StorageModeShared)
                .expect("Failed to create uniform buffer");

            // Atlas texture
            let tex_desc = MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::R8Unorm,
                1024,
                1024,
                false,
            );
            tex_desc.setUsage(MTLTextureUsage::ShaderRead);
            let atlas_texture = device
                .newTextureWithDescriptor(&tex_desc)
                .expect("Failed to create atlas texture");

            // Sampler
            let sampler_desc = MTLSamplerDescriptor::new();
            sampler_desc.setMinFilter(MTLSamplerMinMagFilter::Linear);
            sampler_desc.setMagFilter(MTLSamplerMinMagFilter::Linear);
            let sampler_state = device
                .newSamplerStateWithDescriptor(&sampler_desc)
                .expect("Failed to create sampler state");

            log::info!("Metal renderer initialized");

            Self {
                device,
                command_queue,
                pipeline_state,
                vertex_buffer,
                uniform_buffer,
                atlas_texture,
                sampler_state,
                default_fg,
                default_bg,
            }
        }
    }

    fn resolve_color(&self, color: Color, is_fg: bool, bold: bool) -> (u8, u8, u8) {
        match color {
            Color::Default => {
                if is_fg {
                    self.default_fg
                } else {
                    self.default_bg
                }
            }
            Color::Indexed(idx) => {
                if idx < 16 {
                    let actual_idx = if is_fg && bold && idx < 8 {
                        idx + 8
                    } else {
                        idx
                    };
                    ANSI_COLORS[actual_idx as usize]
                } else if idx < 232 {
                    let idx = idx - 16;
                    let r = (idx / 36) * 51;
                    let g = ((idx % 36) / 6) * 51;
                    let b = (idx % 6) * 51;
                    (r, g, b)
                } else {
                    let level = 8 + (idx - 232) * 10;
                    (level, level, level)
                }
            }
            Color::Rgb(r, g, b) => (r, g, b),
        }
    }

    fn pack_color(r: u8, g: u8, b: u8, a: u8) -> u32 {
        (r as u32) << 24 | (g as u32) << 16 | (b as u32) << 8 | a as u32
    }

    pub fn draw(
        &mut self,
        grid: &Grid,
        atlas: &mut GlyphAtlas,
        drawable: &ProtocolObject<dyn MTLDrawable>,
        texture: &ProtocolObject<dyn MTLTexture>,
        viewport_width: f32,
        viewport_height: f32,
    ) {
        let mut vertices: Vec<Vertex> = Vec::with_capacity(grid.rows() * grid.cols() * 12);

        let cell_w = atlas.cell_width;
        let cell_h = atlas.cell_height;
        let white_u = atlas.white_uv.0;
        let white_v = atlas.white_uv.1;

        for row in 0..grid.rows() {
            for col in 0..grid.cols() {
                let cell = grid.buffer.cell(row, col);
                let bold = cell.flags.contains(CellFlags::BOLD);
                let reverse = cell.flags.contains(CellFlags::REVERSE);

                let (fg, bg) = if reverse {
                    (
                        self.resolve_color(cell.bg, false, false),
                        self.resolve_color(cell.fg, true, bold),
                    )
                } else {
                    (
                        self.resolve_color(cell.fg, true, bold),
                        self.resolve_color(cell.bg, false, false),
                    )
                };

                let fg_packed = Self::pack_color(fg.0, fg.1, fg.2, 255);
                let bg_packed = Self::pack_color(bg.0, bg.1, bg.2, 255);

                let x0 = col as f32 * cell_w;
                let y0 = row as f32 * cell_h;
                let x1 = x0 + cell_w;
                let y1 = y0 + cell_h;

                // Background quad (always)
                vertices.push(Vertex::new(x0, y0, white_u, white_v, bg_packed, bg_packed));
                vertices.push(Vertex::new(x1, y0, white_u, white_v, bg_packed, bg_packed));
                vertices.push(Vertex::new(x0, y1, white_u, white_v, bg_packed, bg_packed));
                vertices.push(Vertex::new(x1, y0, white_u, white_v, bg_packed, bg_packed));
                vertices.push(Vertex::new(x1, y1, white_u, white_v, bg_packed, bg_packed));
                vertices.push(Vertex::new(x0, y1, white_u, white_v, bg_packed, bg_packed));

                // Glyph quad (skip spaces)
                if cell.c != ' ' && cell.c != '\0' {
                    let key = GlyphKey {
                        c: cell.c,
                        bold,
                        italic: cell.flags.contains(CellFlags::ITALIC),
                    };
                    let glyph = atlas.get_or_insert(key);

                    if glyph.pixel_w > 0 && glyph.pixel_h > 0 {
                        let gx0 = x0 + glyph.bearing_x;
                        let gy0 = y0 + atlas.ascent + glyph.bearing_y;
                        let gx1 = gx0 + glyph.pixel_w as f32;
                        let gy1 = gy0 + glyph.pixel_h as f32;

                        let u0 = glyph.uv_x;
                        let v0 = glyph.uv_y;
                        let u1 = glyph.uv_x + glyph.uv_w;
                        let v1 = glyph.uv_y + glyph.uv_h;

                        vertices.push(Vertex::new(gx0, gy0, u0, v0, fg_packed, bg_packed));
                        vertices.push(Vertex::new(gx1, gy0, u1, v0, fg_packed, bg_packed));
                        vertices.push(Vertex::new(gx0, gy1, u0, v1, fg_packed, bg_packed));
                        vertices.push(Vertex::new(gx1, gy0, u1, v0, fg_packed, bg_packed));
                        vertices.push(Vertex::new(gx1, gy1, u1, v1, fg_packed, bg_packed));
                        vertices.push(Vertex::new(gx0, gy1, u0, v1, fg_packed, bg_packed));
                    }
                }
            }
        }

        if vertices.is_empty() {
            return;
        }

        unsafe {
            // Check vertex buffer size
            let needed = vertices.len() * std::mem::size_of::<Vertex>();
            if needed > self.vertex_buffer.length() {
                self.vertex_buffer = self
                    .device
                    .newBufferWithLength_options(needed * 2, MTLResourceOptions::StorageModeShared)
                    .expect("Failed to resize vertex buffer");
            }

            // Copy vertices
            let ptr = self.vertex_buffer.contents().as_ptr() as *mut Vertex;
            std::ptr::copy_nonoverlapping(vertices.as_ptr(), ptr, vertices.len());

            // Update uniforms
            let ptr = self.uniform_buffer.contents().as_ptr() as *mut [f32; 2];
            *ptr = [viewport_width, viewport_height];

            // Upload atlas if dirty
            if atlas.dirty {
                let region = MTLRegion {
                    origin: MTLOrigin { x: 0, y: 0, z: 0 },
                    size: MTLSize {
                        width: atlas.width as usize,
                        height: atlas.height as usize,
                        depth: 1,
                    },
                };
                let bytes_ptr = std::ptr::NonNull::new(atlas.pixels.as_ptr() as *mut std::ffi::c_void).unwrap();
                self.atlas_texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                    region,
                    0,
                    bytes_ptr,
                    atlas.width as usize,
                );
                atlas.dirty = false;
            }

            // Render
            let command_buffer = self.command_queue.commandBuffer().expect("Failed to create command buffer");

            let render_pass_desc = MTLRenderPassDescriptor::new();
            let color_attachment = render_pass_desc
                .colorAttachments()
                .objectAtIndexedSubscript(0);
            color_attachment.setTexture(Some(texture));
            color_attachment.setLoadAction(MTLLoadAction::Clear);
            color_attachment.setStoreAction(MTLStoreAction::Store);

            let (br, bg_r, bb) = self.default_bg;
            color_attachment.setClearColor(MTLClearColor {
                red: br as f64 / 255.0,
                green: bg_r as f64 / 255.0,
                blue: bb as f64 / 255.0,
                alpha: 1.0,
            });

            let encoder = command_buffer
                .renderCommandEncoderWithDescriptor(&render_pass_desc)
                .expect("Failed to create render encoder");

            encoder.setRenderPipelineState(&self.pipeline_state);
            encoder.setVertexBuffer_offset_atIndex(Some(&self.vertex_buffer), 0, 0);
            encoder.setVertexBuffer_offset_atIndex(Some(&self.uniform_buffer), 0, 1);
            encoder.setFragmentTexture_atIndex(Some(&self.atlas_texture), 0);
            encoder.setFragmentSamplerState_atIndex(Some(&self.sampler_state), 0);

            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, vertices.len());
            encoder.endEncoding();

            command_buffer.presentDrawable(drawable);
            command_buffer.commit();
        }
    }
}
