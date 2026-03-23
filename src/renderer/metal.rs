use super::atlas::{GlyphAtlas, GlyphKey};
use super::box_drawing;
use super::braille;
use super::brush::BrushRenderer;
use super::image_store::ImageStore;
use super::kitty_handler::PlacementMode;
use super::shaders::SHADER_SOURCE;
use super::Vertex;
use crate::app::selection::SelectionState;
use crate::grid::cell::{CellFlags, Color, UnderlineStyle};
use crate::grid::{CursorShape, Grid};
use crate::pane::layout::{DividerInfo, PixelRect, DIVIDER_THICKNESS};
use crate::pane::tree::SplitDirection;
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

pub struct PaneRenderData<'a> {
    pub grid: &'a Grid,
    pub rect: PixelRect,
    pub selection: Option<&'a SelectionState>,
    pub is_focused: bool,
    pub pane_index: usize,
    pub cwd: &'a str,
    pub prompt_mark_rows: Vec<usize>,
    pub show_cursor: bool,
}

pub struct ConfirmOverlayInfo {
    pub region: PixelRect,
    pub title: String,
    pub process_text: Option<String>,
    pub opacity: f32,
}

pub struct ChromeColors {
    pub sumi_dark: (u8, u8, u8),
    pub sumi_medium: (u8, u8, u8),
    pub sumi_light: (u8, u8, u8),
    pub sumi_ghost: (u8, u8, u8),
    pub hanko_red: (u8, u8, u8),
    pub hanko_dim: (u8, u8, u8),
}

pub struct MetalRenderer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline_state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    image_pipeline_state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    vertex_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    uniform_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    atlas_texture: Retained<ProtocolObject<dyn MTLTexture>>,
    sampler_state: Retained<ProtocolObject<dyn MTLSamplerState>>,
    pub brush: BrushRenderer,
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

            let attr0 = attributes.objectAtIndexedSubscript(0);
            attr0.setFormat(MTLVertexFormat::Float2);
            attr0.setOffset(0);
            attr0.setBufferIndex(0);

            let attr1 = attributes.objectAtIndexedSubscript(1);
            attr1.setFormat(MTLVertexFormat::Float2);
            attr1.setOffset(8);
            attr1.setBufferIndex(0);

            let attr2 = attributes.objectAtIndexedSubscript(2);
            attr2.setFormat(MTLVertexFormat::UInt);
            attr2.setOffset(16);
            attr2.setBufferIndex(0);

            let attr3 = attributes.objectAtIndexedSubscript(3);
            attr3.setFormat(MTLVertexFormat::UInt);
            attr3.setOffset(20);
            attr3.setBufferIndex(0);

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

            // Image pipeline: same vertex format, different fragment shader
            let image_fragment_fn_name = NSString::from_str("image_fragment");
            let image_fragment_fn = library
                .newFunctionWithName(&image_fragment_fn_name)
                .expect("image_fragment not found");
            let image_pipeline_desc = MTLRenderPipelineDescriptor::new();
            image_pipeline_desc.setVertexFunction(Some(&vertex_fn));
            image_pipeline_desc.setFragmentFunction(Some(&image_fragment_fn));
            image_pipeline_desc.setVertexDescriptor(Some(&vertex_desc));
            let image_color_attachment = image_pipeline_desc
                .colorAttachments()
                .objectAtIndexedSubscript(0);
            image_color_attachment.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            image_color_attachment.setBlendingEnabled(true);
            image_color_attachment.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
            image_color_attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            image_color_attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
            image_color_attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            let image_pipeline_state = device
                .newRenderPipelineStateWithDescriptor_error(&image_pipeline_desc)
                .expect("Failed to create image render pipeline state");

            let max_vertices = 200 * 100 * 12;
            let buffer_size = max_vertices * std::mem::size_of::<Vertex>();
            let vertex_buffer = device
                .newBufferWithLength_options(buffer_size, MTLResourceOptions::StorageModeShared)
                .expect("Failed to create vertex buffer");

            let uniform_buffer = device
                .newBufferWithLength_options(16, MTLResourceOptions::StorageModeShared)
                .expect("Failed to create uniform buffer");

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

            let sampler_desc = MTLSamplerDescriptor::new();
            sampler_desc.setMinFilter(MTLSamplerMinMagFilter::Linear);
            sampler_desc.setMagFilter(MTLSamplerMinMagFilter::Linear);
            let sampler_state = device
                .newSamplerStateWithDescriptor(&sampler_desc)
                .expect("Failed to create sampler state");

            let brush = BrushRenderer::new(&device);

            log::info!("Metal renderer initialized");

            Self {
                device,
                command_queue,
                pipeline_state,
                image_pipeline_state,
                vertex_buffer,
                uniform_buffer,
                atlas_texture,
                sampler_state,
                brush,
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

    fn build_pane_vertices(
        &self,
        pane: &PaneRenderData,
        atlas: &mut GlyphAtlas,
        selection_fg: (u8, u8, u8),
        selection_bg: (u8, u8, u8),
        status_bar_height: f32,
        prompt_indicator_color: Option<(u8, u8, u8)>,
        vertices: &mut Vec<Vertex>,
    ) {
        let grid = pane.grid;
        let rect = pane.rect;
        let cell_w = atlas.cell_width;
        let cell_h = atlas.cell_height;
        let white_u = atlas.white_uv.0;
        let white_v = atlas.white_uv.1;
        let grid_height = rect.height - status_bar_height;
        let line_thickness = 1.5;

        for row in 0..grid.rows() {
            for col in 0..grid.cols() {
                let cell = grid.visible_cell(row, col);

                // Skip continuation cells of wide chars (bg already drawn by wide cell)
                if cell.flags.contains(CellFlags::WIDE_CONT) {
                    continue;
                }

                let bold = cell.flags.contains(CellFlags::BOLD);
                let reverse = cell.flags.contains(CellFlags::REVERSE);
                let is_wide = cell.flags.contains(CellFlags::WIDE);
                let render_width = if is_wide { 2.0 } else { 1.0 };

                let (fg, bg) = if reverse {
                    (self.resolve_color(cell.bg, false, false), self.resolve_color(cell.fg, true, bold))
                } else {
                    (self.resolve_color(cell.fg, true, bold), self.resolve_color(cell.bg, false, false))
                };

                let is_selected = pane.selection
                    .map(|s| s.contains(row, col, grid.scroll_offset, grid.scrollback_len()))
                    .unwrap_or(false);

                let (fg_packed, bg_packed) = if is_selected {
                    (Self::pack_color(selection_fg.0, selection_fg.1, selection_fg.2, 255),
                     Self::pack_color(selection_bg.0, selection_bg.1, selection_bg.2, 255))
                } else {
                    (Self::pack_color(fg.0, fg.1, fg.2, 255),
                     Self::pack_color(bg.0, bg.1, bg.2, 255))
                };

                let x0 = rect.x + col as f32 * cell_w;
                let y0 = rect.y + row as f32 * cell_h;
                if y0 + cell_h > rect.y + grid_height { continue; }
                let x1 = x0 + cell_w * render_width;
                let y1 = y0 + cell_h;

                // Background quad (spans full width for wide chars)
                vertices.push(Vertex::new(x0, y0, white_u, white_v, bg_packed, bg_packed));
                vertices.push(Vertex::new(x1, y0, white_u, white_v, bg_packed, bg_packed));
                vertices.push(Vertex::new(x0, y1, white_u, white_v, bg_packed, bg_packed));
                vertices.push(Vertex::new(x1, y0, white_u, white_v, bg_packed, bg_packed));
                vertices.push(Vertex::new(x1, y1, white_u, white_v, bg_packed, bg_packed));
                vertices.push(Vertex::new(x0, y1, white_u, white_v, bg_packed, bg_packed));

                if cell.c != ' ' && cell.c != '\0' {
                    let cw = cell_w * render_width;
                    // Box drawing: geometric lines
                    if let Some(segs) = box_drawing::box_drawing_lines(cell.c, cw, cell_h) {
                        for (lx0, ly0, lx1, ly1) in segs {
                            let is_horiz = (ly0 - ly1).abs() < 0.01;
                            let half = line_thickness / 2.0;
                            let (rx0, ry0, rx1, ry1) = if is_horiz {
                                (x0 + lx0, y0 + ly0 - half, x0 + lx1, y0 + ly1 + half)
                            } else {
                                (x0 + lx0 - half, y0 + ly0, x0 + lx1 + half, y0 + ly1)
                            };
                            vertices.push(Vertex::new(rx0, ry0, white_u, white_v, fg_packed, fg_packed));
                            vertices.push(Vertex::new(rx1, ry0, white_u, white_v, fg_packed, fg_packed));
                            vertices.push(Vertex::new(rx0, ry1, white_u, white_v, fg_packed, fg_packed));
                            vertices.push(Vertex::new(rx1, ry0, white_u, white_v, fg_packed, fg_packed));
                            vertices.push(Vertex::new(rx1, ry1, white_u, white_v, fg_packed, fg_packed));
                            vertices.push(Vertex::new(rx0, ry1, white_u, white_v, fg_packed, fg_packed));
                        }
                    } else if braille::is_braille(cell.c) {
                        // Braille: render dots
                        let dots = braille::braille_dots(cell.c);
                        let dot_r = cell_w / 6.0;
                        let col_spacing = cw / 2.0;
                        let row_spacing = cell_h / 4.0;
                        for (dc, dr) in dots {
                            let dx = x0 + (dc as f32 + 0.5) * col_spacing - dot_r;
                            let dy = y0 + (dr as f32 + 0.25) * row_spacing - dot_r;
                            vertices.push(Vertex::new(dx, dy, white_u, white_v, fg_packed, fg_packed));
                            vertices.push(Vertex::new(dx + dot_r * 2.0, dy, white_u, white_v, fg_packed, fg_packed));
                            vertices.push(Vertex::new(dx, dy + dot_r * 2.0, white_u, white_v, fg_packed, fg_packed));
                            vertices.push(Vertex::new(dx + dot_r * 2.0, dy, white_u, white_v, fg_packed, fg_packed));
                            vertices.push(Vertex::new(dx + dot_r * 2.0, dy + dot_r * 2.0, white_u, white_v, fg_packed, fg_packed));
                            vertices.push(Vertex::new(dx, dy + dot_r * 2.0, white_u, white_v, fg_packed, fg_packed));
                        }
                    } else {
                        // Normal glyph from atlas
                        let key = GlyphKey { c: cell.c, bold, italic: cell.flags.contains(CellFlags::ITALIC) };
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

                // Styled underlines
                if cell.underline_style != UnderlineStyle::None {
                    let ul_color = match cell.underline_color {
                        Color::Default => fg,
                        other => self.resolve_color(other, true, false),
                    };
                    let ul_packed = Self::pack_color(ul_color.0, ul_color.1, ul_color.2, 255);
                    let ul_y = y0 + atlas.ascent + 2.0;
                    let ul_h = line_thickness;
                    match cell.underline_style {
                        UnderlineStyle::Single => {
                            Self::add_rect(vertices, x0, ul_y, x1, ul_y + ul_h, white_u, white_v, ul_packed);
                        }
                        UnderlineStyle::Double => {
                            Self::add_rect(vertices, x0, ul_y, x1, ul_y + ul_h, white_u, white_v, ul_packed);
                            Self::add_rect(vertices, x0, ul_y + ul_h + 1.0, x1, ul_y + ul_h * 2.0 + 1.0, white_u, white_v, ul_packed);
                        }
                        UnderlineStyle::Curly => {
                            // Approximate curly underline with small segments
                            let uw = cell_w * render_width;
                            let steps = (uw / 3.0).max(2.0) as usize;
                            let step_w = uw / steps as f32;
                            for s in 0..steps {
                                let sx = x0 + s as f32 * step_w;
                                let sy = ul_y + ((s as f32 * std::f32::consts::PI).sin() * 2.0);
                                Self::add_rect(vertices, sx, sy, sx + step_w, sy + ul_h, white_u, white_v, ul_packed);
                            }
                        }
                        UnderlineStyle::Dotted => {
                            let mut dx = x0;
                            while dx < x1 {
                                Self::add_rect(vertices, dx, ul_y, dx + ul_h, ul_y + ul_h, white_u, white_v, ul_packed);
                                dx += ul_h + 2.0;
                            }
                        }
                        UnderlineStyle::Dashed => {
                            let mut dx = x0;
                            while dx < x1 {
                                let end = (dx + 4.0).min(x1);
                                Self::add_rect(vertices, dx, ul_y, end, ul_y + ul_h, white_u, white_v, ul_packed);
                                dx += 6.0;
                            }
                        }
                        UnderlineStyle::None => {}
                    }
                }
            }
        }

        // Cursor rendering
        if pane.show_cursor && grid.scroll_offset == 0 {
            let crow = grid.cursor_row;
            let ccol = grid.cursor_col;
            if crow < grid.rows() && ccol < grid.cols() {
                let cx0 = rect.x + ccol as f32 * cell_w;
                let cy0 = rect.y + crow as f32 * cell_h;
                if cy0 + cell_h <= rect.y + grid_height {
                    let cursor_color = Self::pack_color(192, 192, 192, 180);
                    match grid.cursor_style.shape {
                        CursorShape::Block => {
                            Self::add_rect(vertices, cx0, cy0, cx0 + cell_w, cy0 + cell_h, white_u, white_v, cursor_color);
                        }
                        CursorShape::Bar => {
                            Self::add_rect(vertices, cx0, cy0, cx0 + 2.0, cy0 + cell_h, white_u, white_v, cursor_color);
                        }
                        CursorShape::Underline => {
                            Self::add_rect(vertices, cx0, cy0 + cell_h - 2.0, cx0 + cell_w, cy0 + cell_h, white_u, white_v, cursor_color);
                        }
                    }
                }
            }
        }

        // Prompt mark indicator dots
        if let Some(color) = prompt_indicator_color {
            let dot_color = Self::pack_color(color.0, color.1, color.2, 255);
            for &vis_row in &pane.prompt_mark_rows {
                let dx0 = rect.x + 1.0;
                let dy0 = rect.y + vis_row as f32 * cell_h + cell_h * 0.4;
                Self::add_rect(vertices, dx0, dy0, dx0 + 3.0, dy0 + 3.0, white_u, white_v, dot_color);
            }
        }
    }

    fn add_rect(vertices: &mut Vec<Vertex>, x0: f32, y0: f32, x1: f32, y1: f32, u: f32, v: f32, color: u32) {
        vertices.push(Vertex::new(x0, y0, u, v, color, color));
        vertices.push(Vertex::new(x1, y0, u, v, color, color));
        vertices.push(Vertex::new(x0, y1, u, v, color, color));
        vertices.push(Vertex::new(x1, y0, u, v, color, color));
        vertices.push(Vertex::new(x1, y1, u, v, color, color));
        vertices.push(Vertex::new(x0, y1, u, v, color, color));
    }

    fn build_status_bar_vertices(
        &self,
        pane: &PaneRenderData,
        atlas: &mut GlyphAtlas,
        chrome: &ChromeColors,
        status_bar_height: f32,
        vertices: &mut Vec<Vertex>,
    ) {
        let rect = pane.rect;
        let bar_y = rect.y + rect.height - status_bar_height;
        let white_u = atlas.white_uv.0;
        let white_v = atlas.white_uv.1;

        // Status bar background
        let bg = Self::pack_color(chrome.sumi_dark.0, chrome.sumi_dark.1, chrome.sumi_dark.2, 255);
        let x0 = rect.x;
        let x1 = rect.x + rect.width;
        let y0 = bar_y;
        let y1 = bar_y + status_bar_height;

        vertices.push(Vertex::new(x0, y0, white_u, white_v, bg, bg));
        vertices.push(Vertex::new(x1, y0, white_u, white_v, bg, bg));
        vertices.push(Vertex::new(x0, y1, white_u, white_v, bg, bg));
        vertices.push(Vertex::new(x1, y0, white_u, white_v, bg, bg));
        vertices.push(Vertex::new(x1, y1, white_u, white_v, bg, bg));
        vertices.push(Vertex::new(x0, y1, white_u, white_v, bg, bg));

        // Render status text: "黒板 · zsh · /cwd · ■"
        let cell_w = atlas.cell_width;
        let cell_h = atlas.cell_height;
        let text_y = bar_y + (status_bar_height - cell_h) / 2.0;
        let mut cursor_x = rect.x + cell_w;

        let light = Self::pack_color(chrome.sumi_light.0, chrome.sumi_light.1, chrome.sumi_light.2, 255);
        let medium = Self::pack_color(chrome.sumi_medium.0, chrome.sumi_medium.1, chrome.sumi_medium.2, 255);
        let indicator_color = if pane.is_focused {
            Self::pack_color(chrome.hanko_red.0, chrome.hanko_red.1, chrome.hanko_red.2, 255)
        } else {
            Self::pack_color(chrome.sumi_ghost.0, chrome.sumi_ghost.1, chrome.sumi_ghost.2, 255)
        };

        // "黒板"
        for c in "黒板".chars() {
            cursor_x = self.render_char(c, cursor_x, text_y, light, bg, atlas, vertices);
        }

        // " · zsh"
        for c in " · zsh".chars() {
            cursor_x = self.render_char(c, cursor_x, text_y, medium, bg, atlas, vertices);
        }

        // " · /cwd"
        if !pane.cwd.is_empty() {
            for c in " · ".chars() {
                cursor_x = self.render_char(c, cursor_x, text_y, medium, bg, atlas, vertices);
            }
            // Truncate cwd if too long
            let max_cwd_chars = ((rect.width - cursor_x + rect.x - cell_w * 4.0) / cell_w) as usize;
            let cwd_display = if pane.cwd.len() > max_cwd_chars && max_cwd_chars > 3 {
                &pane.cwd[pane.cwd.len() - max_cwd_chars..]
            } else {
                pane.cwd
            };
            for c in cwd_display.chars() {
                if cursor_x + cell_w > x1 - cell_w * 3.0 {
                    break;
                }
                cursor_x = self.render_char(c, cursor_x, text_y, light, bg, atlas, vertices);
            }
        }

        // " · ■" (focus indicator) at the right
        let indicator_x = x1 - cell_w * 2.0;
        self.render_char('■', indicator_x, text_y, indicator_color, bg, atlas, vertices);
    }

    fn render_char(
        &self,
        c: char,
        x: f32,
        y: f32,
        fg: u32,
        bg: u32,
        atlas: &mut GlyphAtlas,
        vertices: &mut Vec<Vertex>,
    ) -> f32 {
        let key = GlyphKey {
            c,
            bold: false,
            italic: false,
        };
        let glyph = atlas.get_or_insert(key);
        let cell_w = atlas.cell_width;

        if glyph.pixel_w > 0 && glyph.pixel_h > 0 {
            let gx0 = x + glyph.bearing_x;
            let gy0 = y + atlas.ascent + glyph.bearing_y;
            let gx1 = gx0 + glyph.pixel_w as f32;
            let gy1 = gy0 + glyph.pixel_h as f32;

            let u0 = glyph.uv_x;
            let v0 = glyph.uv_y;
            let u1 = glyph.uv_x + glyph.uv_w;
            let v1 = glyph.uv_y + glyph.uv_h;

            vertices.push(Vertex::new(gx0, gy0, u0, v0, fg, bg));
            vertices.push(Vertex::new(gx1, gy0, u1, v0, fg, bg));
            vertices.push(Vertex::new(gx0, gy1, u0, v1, fg, bg));
            vertices.push(Vertex::new(gx1, gy0, u1, v0, fg, bg));
            vertices.push(Vertex::new(gx1, gy1, u1, v1, fg, bg));
            vertices.push(Vertex::new(gx0, gy1, u0, v1, fg, bg));
        }

        x + cell_w
    }

    fn build_divider_vertices(
        &self,
        divider: &DividerInfo,
        bg_packed: u32,
        chrome: &ChromeColors,
    ) -> Vec<Vertex> {
        let mut vertices = Vec::new();
        let half = DIVIDER_THICKNESS / 2.0;

        let fg_color = if divider.touches_focused {
            chrome.hanko_red
        } else {
            chrome.sumi_ghost
        };
        let fg = Self::pack_color(fg_color.0, fg_color.1, fg_color.2, 255);

        let variant = ((divider.x0 as u32).wrapping_mul(7) + (divider.y0 as u32).wrapping_mul(13)) as usize;
        let (u0, v0, u_w, v_h) = self.brush.uv_rect(variant);
        let u1 = u0 + u_w;
        let v1 = v0 + v_h;

        match divider.direction {
            SplitDirection::Vertical => {
                let x0 = divider.x0 - half;
                let x1 = divider.x0 + half;
                let y0 = divider.y0;
                let y1 = divider.y1;

                vertices.push(Vertex::new(x0, y0, u0, v0, fg, bg_packed));
                vertices.push(Vertex::new(x1, y0, u1, v0, fg, bg_packed));
                vertices.push(Vertex::new(x0, y1, u0, v1, fg, bg_packed));
                vertices.push(Vertex::new(x1, y0, u1, v0, fg, bg_packed));
                vertices.push(Vertex::new(x1, y1, u1, v1, fg, bg_packed));
                vertices.push(Vertex::new(x0, y1, u0, v1, fg, bg_packed));
            }
            SplitDirection::Horizontal => {
                let x0 = divider.x0;
                let x1 = divider.x1;
                let y0 = divider.y0 - half;
                let y1 = divider.y0 + half;

                // For horizontal, rotate UV: u maps to y, v maps to x
                vertices.push(Vertex::new(x0, y0, u0, v0, fg, bg_packed));
                vertices.push(Vertex::new(x1, y0, u0, v1, fg, bg_packed));
                vertices.push(Vertex::new(x0, y1, u1, v0, fg, bg_packed));
                vertices.push(Vertex::new(x1, y0, u0, v1, fg, bg_packed));
                vertices.push(Vertex::new(x1, y1, u1, v1, fg, bg_packed));
                vertices.push(Vertex::new(x0, y1, u1, v0, fg, bg_packed));
            }
        }

        vertices
    }

    pub fn draw_frame(
        &mut self,
        panes: &[PaneRenderData],
        dividers: &[DividerInfo],
        atlas: &mut GlyphAtlas,
        drawable: &ProtocolObject<dyn MTLDrawable>,
        texture: &ProtocolObject<dyn MTLTexture>,
        viewport_width: f32,
        viewport_height: f32,
        bg_opacity: f32,
        selection_fg: (u8, u8, u8),
        selection_bg: (u8, u8, u8),
        chrome: &ChromeColors,
        status_bar_height: f32,
        prompt_indicator_color: Option<(u8, u8, u8)>,
        image_store: Option<&ImageStore>,
        confirm_overlay: Option<&ConfirmOverlayInfo>,
    ) {
        // Build pane content + status bar vertices (use atlas texture)
        let mut content_vertices: Vec<Vertex> = Vec::with_capacity(200 * 80 * 12);
        for pane in panes {
            self.build_pane_vertices(pane, atlas, selection_fg, selection_bg, status_bar_height, prompt_indicator_color, &mut content_vertices);
            self.build_status_bar_vertices(pane, atlas, chrome, status_bar_height, &mut content_vertices);
        }

        // Build divider vertices (use brush texture)
        let bg_packed = Self::pack_color(self.default_bg.0, self.default_bg.1, self.default_bg.2, 255);
        let mut divider_vertices: Vec<Vertex> = Vec::new();
        for div in dividers {
            divider_vertices.extend(self.build_divider_vertices(div, bg_packed, chrome));
        }

        // Build image quad vertices for each pane
        struct ImageDrawCall {
            vertex_start: usize,
            vertex_count: usize,
            image_id: super::image_store::ImageId,
        }
        let mut image_vertices: Vec<Vertex> = Vec::new();
        let mut image_draw_calls: Vec<ImageDrawCall> = Vec::new();

        if let Some(store) = image_store {
            let cell_w = atlas.cell_width;
            let cell_h = atlas.cell_height;
            let white = 0xFFFFFFFFu32; // dummy, unused by image shader

            for pane in panes {
                let rect = pane.rect;
                let grid_height = rect.height - status_bar_height;

                for placement in &pane.grid.image_placements {
                    if store.get(placement.image_id).is_none() {
                        continue;
                    }

                    let (x0, y0, w, h) = match &placement.mode {
                        PlacementMode::Inline { row, col, cols, rows } => {
                            let x = rect.x + *col as f32 * cell_w;
                            let y = rect.y + *row as f32 * cell_h;
                            let w = *cols as f32 * cell_w;
                            let h = *rows as f32 * cell_h;
                            (x, y, w, h)
                        }
                        PlacementMode::Overlay { x, y, width, height } => {
                            (rect.x + x, rect.y + y, *width, *height)
                        }
                    };

                    // Clip to pane bounds
                    if y0 + h <= rect.y || y0 >= rect.y + grid_height {
                        continue;
                    }
                    if x0 + w <= rect.x || x0 >= rect.x + rect.width {
                        continue;
                    }

                    let x1 = (x0 + w).min(rect.x + rect.width);
                    let y1 = (y0 + h).min(rect.y + grid_height);

                    // Compute UV coordinates (clipped)
                    let u0 = if x0 < rect.x { (rect.x - x0) / w } else { 0.0 };
                    let v0 = if y0 < rect.y { (rect.y - y0) / h } else { 0.0 };
                    let u1 = (x1 - x0) / w;
                    let v1 = (y1 - y0) / h;
                    let x0 = x0.max(rect.x);
                    let y0 = y0.max(rect.y);

                    let start = image_vertices.len();
                    image_vertices.push(Vertex::new(x0, y0, u0, v0, white, white));
                    image_vertices.push(Vertex::new(x1, y0, u1, v0, white, white));
                    image_vertices.push(Vertex::new(x0, y1, u0, v1, white, white));
                    image_vertices.push(Vertex::new(x1, y0, u1, v0, white, white));
                    image_vertices.push(Vertex::new(x1, y1, u1, v1, white, white));
                    image_vertices.push(Vertex::new(x0, y1, u0, v1, white, white));

                    image_draw_calls.push(ImageDrawCall {
                        vertex_start: start,
                        vertex_count: 6,
                        image_id: placement.image_id,
                    });
                }
            }
        }

        // Build confirm overlay vertices (uses atlas texture, drawn last)
        let mut overlay_vertices: Vec<Vertex> = Vec::new();
        if let Some(overlay) = confirm_overlay {
            self.build_confirm_overlay(overlay, atlas, chrome, &mut overlay_vertices);
        }

        // Combine ALL vertices into single buffer: content, dividers, images, overlay
        let content_count = content_vertices.len();
        let divider_count = divider_vertices.len();
        let image_base_offset = content_count + divider_count;
        let overlay_offset = image_base_offset + image_vertices.len();
        let overlay_count = overlay_vertices.len();

        let mut all_vertices = content_vertices;
        all_vertices.extend(divider_vertices);
        all_vertices.extend(image_vertices);
        all_vertices.extend(overlay_vertices);

        if all_vertices.is_empty() {
            return;
        }

        unsafe {
            let needed = all_vertices.len() * std::mem::size_of::<Vertex>();
            if needed > self.vertex_buffer.length() {
                self.vertex_buffer = self
                    .device
                    .newBufferWithLength_options(needed * 2, MTLResourceOptions::StorageModeShared)
                    .expect("Failed to resize vertex buffer");
            }

            let ptr = self.vertex_buffer.contents().as_ptr() as *mut Vertex;
            std::ptr::copy_nonoverlapping(all_vertices.as_ptr(), ptr, all_vertices.len());

            let ptr = self.uniform_buffer.contents().as_ptr() as *mut [f32; 2];
            *ptr = [viewport_width, viewport_height];

            if atlas.dirty {
                let region = MTLRegion {
                    origin: MTLOrigin { x: 0, y: 0, z: 0 },
                    size: MTLSize {
                        width: atlas.width as usize,
                        height: atlas.height as usize,
                        depth: 1,
                    },
                };
                let bytes_ptr =
                    std::ptr::NonNull::new(atlas.pixels.as_ptr() as *mut std::ffi::c_void).unwrap();
                self.atlas_texture
                    .replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                        region,
                        0,
                        bytes_ptr,
                        atlas.width as usize,
                    );
                atlas.dirty = false;
            }

            let command_buffer = self
                .command_queue
                .commandBuffer()
                .expect("Failed to create command buffer");

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
                alpha: bg_opacity as f64,
            });

            let encoder = command_buffer
                .renderCommandEncoderWithDescriptor(&render_pass_desc)
                .expect("Failed to create render encoder");

            encoder.setRenderPipelineState(&self.pipeline_state);
            encoder.setVertexBuffer_offset_atIndex(Some(&self.vertex_buffer), 0, 0);
            encoder.setVertexBuffer_offset_atIndex(Some(&self.uniform_buffer), 0, 1);
            encoder.setFragmentSamplerState_atIndex(Some(&self.sampler_state), 0);

            // Draw pane content + status bars with atlas texture
            if content_count > 0 {
                encoder.setFragmentTexture_atIndex(Some(&self.atlas_texture), 0);
                encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    content_count,
                );
            }

            // Draw images with image pipeline (after text, so they overlay)
            if !image_draw_calls.is_empty() {
                if let Some(store) = image_store {
                    encoder.setRenderPipelineState(&self.image_pipeline_state);
                    for call in &image_draw_calls {
                        if let Some(img) = store.get(call.image_id) {
                            encoder.setFragmentTexture_atIndex(Some(&img.texture), 0);
                            encoder.drawPrimitives_vertexStart_vertexCount(
                                MTLPrimitiveType::Triangle,
                                image_base_offset + call.vertex_start,
                                call.vertex_count,
                            );
                        }
                    }
                }
            }

            // Draw dividers with brush texture
            if divider_count > 0 {
                encoder.setRenderPipelineState(&self.pipeline_state);
                encoder.setFragmentTexture_atIndex(Some(&self.brush.texture), 0);
                encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::Triangle,
                    content_count,
                    divider_count,
                );
            }

            // Draw confirm overlay ON TOP of everything (uses atlas texture)
            if overlay_count > 0 {
                encoder.setRenderPipelineState(&self.pipeline_state);
                encoder.setFragmentTexture_atIndex(Some(&self.atlas_texture), 0);
                encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::Triangle,
                    overlay_offset,
                    overlay_count,
                );
            }

            encoder.endEncoding();

            command_buffer.presentDrawable(drawable);
            command_buffer.commit();
        }
    }

    fn build_confirm_overlay(
        &self,
        overlay: &ConfirmOverlayInfo,
        atlas: &mut GlyphAtlas,
        chrome: &ChromeColors,
        vertices: &mut Vec<Vertex>,
    ) {
        let region = overlay.region;
        let alpha = overlay.opacity;
        let cell_w = atlas.cell_width;
        let cell_h = atlas.cell_height;
        let white_u = atlas.white_uv.0;
        let white_v = atlas.white_uv.1;

        // 1. Dimming overlay — semi-transparent black covering the region
        let dim_alpha = (0.85 * alpha * 255.0) as u8;
        let dim_color = Self::pack_color(0x1a, 0x1a, 0x1a, dim_alpha);
        Self::add_rect(vertices, region.x, region.y,
            region.x + region.width, region.y + region.height,
            white_u, white_v, dim_color);

        // 2. Calculate dialog box dimensions (account for CJK double-width)
        let title = &overlay.title;
        let process_text = &overlay.process_text;

        let full_title = format!("\u{9589}\u{3058}\u{308B} \u{00B7} {}", title);
        let actions_text = "[Y] \u{9589} confirm      [N] \u{623B} cancel";

        let char_width = |s: &str| -> f32 {
            s.chars().map(|c| {
                if unicode_width::UnicodeWidthChar::width(c).unwrap_or(1) >= 2 { 2.0 } else { 1.0 }
            }).sum::<f32>() * cell_w
        };

        let title_width = char_width(&full_title);
        let actions_width = char_width(actions_text);
        let process_width = process_text.as_ref().map_or(0.0, |t| char_width(t));

        let content_width = title_width.max(actions_width).max(process_width);

        let padding_h = cell_w * 4.0;
        let padding_v = cell_h * 2.0;

        let has_process = process_text.is_some();
        let line_count = if has_process { 5.0 } else { 4.0 };

        let box_width = content_width + padding_h * 2.0;
        let box_height = cell_h * line_count + padding_v * 2.0;

        let box_x = region.x + (region.width - box_width) / 2.0;
        let box_y = region.y + (region.height - box_height) / 2.0;

        // 3. Dialog background
        let bg_alpha = (0.95 * alpha * 255.0) as u8;
        let bg_color = Self::pack_color(chrome.sumi_dark.0, chrome.sumi_dark.1, chrome.sumi_dark.2, bg_alpha);
        Self::add_rect(vertices, box_x, box_y, box_x + box_width, box_y + box_height,
            white_u, white_v, bg_color);

        // 4. Corner brush marks — short L-shaped strokes at each corner
        let corner_len = cell_h * 1.5;
        let corner_thickness = 2.0;
        let mark_alpha = (alpha * 255.0) as u8;
        let mark_color = Self::pack_color(chrome.sumi_medium.0, chrome.sumi_medium.1, chrome.sumi_medium.2, mark_alpha);

        // Top-left: horizontal right + vertical down
        Self::add_rect(vertices, box_x, box_y, box_x + corner_len, box_y + corner_thickness, white_u, white_v, mark_color);
        Self::add_rect(vertices, box_x, box_y, box_x + corner_thickness, box_y + corner_len, white_u, white_v, mark_color);
        // Top-right: horizontal left + vertical down
        Self::add_rect(vertices, box_x + box_width - corner_len, box_y, box_x + box_width, box_y + corner_thickness, white_u, white_v, mark_color);
        Self::add_rect(vertices, box_x + box_width - corner_thickness, box_y, box_x + box_width, box_y + corner_len, white_u, white_v, mark_color);
        // Bottom-left: horizontal right + vertical up
        Self::add_rect(vertices, box_x, box_y + box_height - corner_thickness, box_x + corner_len, box_y + box_height, white_u, white_v, mark_color);
        Self::add_rect(vertices, box_x, box_y + box_height - corner_len, box_x + corner_thickness, box_y + box_height, white_u, white_v, mark_color);
        // Bottom-right: horizontal left + vertical up
        Self::add_rect(vertices, box_x + box_width - corner_len, box_y + box_height - corner_thickness, box_x + box_width, box_y + box_height, white_u, white_v, mark_color);
        Self::add_rect(vertices, box_x + box_width - corner_thickness, box_y + box_height - corner_len, box_x + box_width, box_y + box_height, white_u, white_v, mark_color);

        // 5. Title text: "閉じる · <title>"
        let text_alpha = (alpha * 255.0) as u8;
        let title_color = Self::pack_color(chrome.sumi_light.0, chrome.sumi_light.1, chrome.sumi_light.2, text_alpha);

        let title_x = box_x + (box_width - title_width) / 2.0;
        let title_y = box_y + padding_v;

        let mut cursor_x = title_x;
        for c in full_title.chars() {
            cursor_x = self.render_char_with_alpha(c, cursor_x, title_y, title_color, bg_color, atlas, vertices);
        }

        // 5b. Process text if present
        let actions_y_offset = if has_process {
            let proc_text = process_text.as_ref().unwrap();
            let proc_x = box_x + (box_width - process_width) / 2.0;
            let proc_y = title_y + cell_h * 1.5;
            let proc_color = Self::pack_color(chrome.sumi_medium.0, chrome.sumi_medium.1, chrome.sumi_medium.2, text_alpha);
            let mut cx = proc_x;
            for c in proc_text.chars() {
                cx = self.render_char_with_alpha(c, cx, proc_y, proc_color, bg_color, atlas, vertices);
            }
            cell_h * 1.5
        } else {
            0.0
        };

        // 6. Action keys line
        let actions_y = title_y + cell_h * 2.0 + actions_y_offset;
        let hanko_color = Self::pack_color(chrome.hanko_red.0, chrome.hanko_red.1, chrome.hanko_red.2, text_alpha);
        let medium_color = Self::pack_color(chrome.sumi_medium.0, chrome.sumi_medium.1, chrome.sumi_medium.2, text_alpha);
        let light_color = title_color;

        // Center the actions line
        let mut ax = box_x + (box_width - actions_width) / 2.0;

        // Render "[Y]" in hanko_red, " 閉 confirm" in light, "      " spacing, "[N]" in medium, " 戻 cancel" in light
        for c in "[Y]".chars() {
            ax = self.render_char_with_alpha(c, ax, actions_y, hanko_color, bg_color, atlas, vertices);
        }
        for c in " \u{9589} confirm".chars() {
            ax = self.render_char_with_alpha(c, ax, actions_y, light_color, bg_color, atlas, vertices);
        }
        for c in "      ".chars() {
            ax = self.render_char_with_alpha(c, ax, actions_y, bg_color, bg_color, atlas, vertices);
        }
        for c in "[N]".chars() {
            ax = self.render_char_with_alpha(c, ax, actions_y, medium_color, bg_color, atlas, vertices);
        }
        for c in " \u{623B} cancel".chars() {
            ax = self.render_char_with_alpha(c, ax, actions_y, light_color, bg_color, atlas, vertices);
        }
    }

    fn render_char_with_alpha(
        &self,
        c: char,
        x: f32,
        y: f32,
        fg: u32,
        bg: u32,
        atlas: &mut GlyphAtlas,
        vertices: &mut Vec<Vertex>,
    ) -> f32 {
        let key = GlyphKey { c, bold: false, italic: false };
        let glyph = atlas.get_or_insert(key);
        let char_width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1);
        let advance = atlas.cell_width * char_width as f32;

        if glyph.pixel_w > 0 && glyph.pixel_h > 0 {
            let gx0 = x + glyph.bearing_x;
            let gy0 = y + atlas.ascent + glyph.bearing_y;
            let gx1 = gx0 + glyph.pixel_w as f32;
            let gy1 = gy0 + glyph.pixel_h as f32;
            let u0 = glyph.uv_x;
            let v0 = glyph.uv_y;
            let u1 = glyph.uv_x + glyph.uv_w;
            let v1 = glyph.uv_y + glyph.uv_h;
            vertices.push(Vertex::new(gx0, gy0, u0, v0, fg, bg));
            vertices.push(Vertex::new(gx1, gy0, u1, v0, fg, bg));
            vertices.push(Vertex::new(gx0, gy1, u0, v1, fg, bg));
            vertices.push(Vertex::new(gx1, gy0, u1, v0, fg, bg));
            vertices.push(Vertex::new(gx1, gy1, u1, v1, fg, bg));
            vertices.push(Vertex::new(gx0, gy1, u0, v1, fg, bg));
        }

        x + advance
    }
}
