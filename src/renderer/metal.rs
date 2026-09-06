use super::box_drawing;
use super::braille;
use super::brush::BrushRenderer;
use super::image_store::ImageStore;
use super::pane_scene::{
    append_translated, content_clip, image_intersects_content, PaneScene, PaneSceneCache, SceneImage,
};
use super::shaders::SHADER_SOURCE;
use super::Vertex;
use crate::glyph_atlas::{GlyphAtlas, GlyphEntry, GlyphKey};
use crate::grid::cell::{CellFlags, Color, UnderlineStyle};
use crate::grid::CursorShape;
use crate::layout::{DividerInfo, SplitDirection, DIVIDER_THICKNESS};
use crate::render_scene::{ChromeColors, ConfirmOverlayInfo, PaneRenderData};
use crate::terminal_colors::TerminalColors;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::*;

type MetalTexture = Retained<ProtocolObject<dyn MTLTexture>>;

struct SceneDrawCall {
    vertex_start: usize,
    vertex_count: usize,
    texture: MetalTexture,
    clip: [usize; 4],
}

struct FrameBuffers {
    vertices: Retained<ProtocolObject<dyn MTLBuffer>>,
    uniforms: Retained<ProtocolObject<dyn MTLBuffer>>,
    submission: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
}

impl FrameBuffers {
    fn is_available(&self) -> bool {
        self.submission.as_ref().is_none_or(|submission| {
            matches!(submission.status(), MTLCommandBufferStatus::Completed | MTLCommandBufferStatus::Error)
        })
    }
}

fn white_pixel_uv(atlas_width: u32, atlas_height: u32) -> (f32, f32) {
    (0.5 / atlas_width as f32, 0.5 / atlas_height as f32)
}

fn status_cwd_suffix(cwd: &str, max_chars: usize) -> &str {
    if max_chars == 0 {
        return "";
    }
    let start = cwd.char_indices().rev().nth(max_chars - 1)
        .map_or(0, |(index, _)| index);
    &cwd[start..]
}

fn cell_content_is_visible(flags: CellFlags) -> bool {
    !flags.contains(CellFlags::HIDDEN)
}

fn glyph_uv_bounds(
    atlas_width: u32,
    atlas_height: u32,
    glyph: &GlyphEntry,
) -> (f32, f32, f32, f32) {
    let width = atlas_width as f32;
    let height = atlas_height as f32;
    (
        glyph.atlas_x as f32 / width,
        glyph.atlas_y as f32 / height,
        (glyph.atlas_x + glyph.pixel_w) as f32 / width,
        (glyph.atlas_y + glyph.pixel_h) as f32 / height,
    )
}

pub struct MetalRenderer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline_state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    image_pipeline_state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    frames: [FrameBuffers; 3],
    atlas_texture: Retained<ProtocolObject<dyn MTLTexture>>,
    sampler_state: Retained<ProtocolObject<dyn MTLSamplerState>>,
    pub brush: BrushRenderer,
    colors: TerminalColors,
    pane_scenes: PaneSceneCache<MetalTexture>,
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

            let frames = std::array::from_fn(|_| FrameBuffers {
                vertices: device.newBufferWithLength_options(
                    std::mem::size_of::<Vertex>(), MTLResourceOptions::StorageModeShared,
                ).expect("Failed to create frame vertex buffer"),
                uniforms: device.newBufferWithLength_options(16, MTLResourceOptions::StorageModeShared)
                    .expect("Failed to create frame uniform buffer"),
                submission: None,
            });

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
                frames,
                atlas_texture,
                sampler_state,
                brush,
                colors: TerminalColors::new(default_fg, default_bg),
                pane_scenes: PaneSceneCache::default(),
            }
        }
    }

    fn pack_color(r: u8, g: u8, b: u8, a: u8) -> u32 {
        (r as u32) << 24 | (g as u32) << 16 | (b as u32) << 8 | a as u32
    }

    fn pack_cell_colors(
        foreground: (u8, u8, u8),
        background: (u8, u8, u8),
        selection_foreground: (u8, u8, u8),
        selection_background: (u8, u8, u8),
        selected: bool,
    ) -> (u32, u32) {
        let (foreground, background) = if selected {
            (selection_foreground, selection_background)
        } else {
            (foreground, background)
        };
        (
            Self::pack_color(foreground.0, foreground.1, foreground.2, 255),
            Self::pack_color(background.0, background.1, background.2, 255),
        )
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
        let (white_u, white_v) = white_pixel_uv(atlas.width, atlas.height);
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
                let is_wide = cell.flags.contains(CellFlags::WIDE);
                let render_width = if is_wide { 2.0 } else { 1.0 };
                let content_is_visible = cell_content_is_visible(cell.flags);

                let resolved =
                    self.colors
                        .resolve_cell_colors(cell.fg, cell.bg, cell.flags);
                let fg = resolved.foreground;
                let bg = resolved.background;

                let selected_by_range = pane.selection
                    .map(|s| s.contains(row, col, grid.scroll_offset, grid.scrollback_len()))
                    .unwrap_or(false);
                let (fg_packed, bg_packed) = Self::pack_cell_colors(
                    fg,
                    bg,
                    selection_fg,
                    selection_bg,
                    selected_by_range,
                );

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

                if !content_is_visible {
                    continue;
                }

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
                            let gx0 = x0 + glyph.bearing_x as f32;
                            let gy0 = y0 + atlas.ascent + glyph.bearing_y as f32;
                            let gx1 = gx0 + glyph.pixel_w as f32;
                            let gy1 = gy0 + glyph.pixel_h as f32;
                            let (u0, v0, u1, v1) =
                                glyph_uv_bounds(atlas.width, atlas.height, &glyph);
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
                        other => self.colors.resolve_foreground(other, false),
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
            if let Some(ccol) = grid.screen_cursor_col().filter(|_| crow < grid.rows()) {
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
        let (white_u, white_v) = white_pixel_uv(atlas.width, atlas.height);

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
            let cwd_display = if max_cwd_chars > 3 {
                status_cwd_suffix(pane.cwd, max_cwd_chars)
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
            let gx0 = x + glyph.bearing_x as f32;
            let gy0 = y + atlas.ascent + glyph.bearing_y as f32;
            let gx1 = gx0 + glyph.pixel_w as f32;
            let gy1 = gy0 + glyph.pixel_h as f32;

            let (u0, v0, u1, v1) =
                glyph_uv_bounds(atlas.width, atlas.height, &glyph);

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

    fn build_pane_images(
        pane: &PaneRenderData,
        atlas: &GlyphAtlas,
        status_bar_height: f32,
        image_store: Option<&ImageStore>,
    ) -> Vec<SceneImage<MetalTexture>> {
        let mut images = Vec::new();
        let Some(store) = image_store else { return images };
        let rect = pane.rect;
        let grid_height = (rect.height - status_bar_height).max(0.0);
        for placement in &pane.grid.image_placements {
            let Some(image) = store.get(placement.image_id) else { continue };
            if !image_intersects_content(&placement.mode,
                [atlas.cell_width, atlas.cell_height], [rect.width, grid_height])
            {
                continue;
            }
            let (placement_x, placement_y, w, h) =
                placement.mode.pixel_rect(atlas.cell_width, atlas.cell_height);
            let x0 = rect.x + placement_x;
            let y0 = rect.y + placement_y;
            let x1 = (x0 + w).min(rect.x + rect.width);
            let y1 = (y0 + h).min(rect.y + grid_height);
            let u0 = if x0 < rect.x { (rect.x - x0) / w } else { 0.0 };
            let v0 = if y0 < rect.y { (rect.y - y0) / h } else { 0.0 };
            let u1 = (x1 - x0) / w;
            let v1 = (y1 - y0) / h;
            let x0 = x0.max(rect.x);
            let y0 = y0.max(rect.y);
            let white = u32::MAX;
            images.push(SceneImage {
                vertices: [
                    Vertex::new(x0, y0, u0, v0, white, white),
                    Vertex::new(x1, y0, u1, v0, white, white),
                    Vertex::new(x0, y1, u0, v1, white, white),
                    Vertex::new(x1, y0, u1, v0, white, white),
                    Vertex::new(x1, y1, u1, v1, white, white),
                    Vertex::new(x0, y1, u0, v1, white, white),
                ],
                texture: image.texture.clone(),
            });
        }
        images
    }

    fn upload_atlas(&mut self, atlas: &mut GlyphAtlas) {
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::R8Unorm, atlas.width as usize, atlas.height as usize, false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        let texture = self.device.newTextureWithDescriptor(&descriptor)
            .expect("Failed to create updated glyph atlas texture");
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize { width: atlas.width as usize, height: atlas.height as usize, depth: 1 },
        };
        let bytes = std::ptr::NonNull::new(atlas.pixels.as_ptr() as *mut std::ffi::c_void).unwrap();
        unsafe {
            texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region, 0, bytes, atlas.width as usize,
            );
        }
        self.atlas_texture = texture;
        atlas.dirty = false;
    }

    pub fn draw_frame(
        &mut self,
        panes: &[PaneRenderData],
        dividers: &[DividerInfo],
        atlas: &mut GlyphAtlas,
        drawable: Option<&ProtocolObject<dyn MTLDrawable>>,
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
    ) -> bool {
        // Reuse a slot only after Metal has stopped reading both its vertex and
        // viewport buffers. A busy GPU retries on the next UI timer tick.
        let Some(frame_index) = self.frames.iter().position(FrameBuffers::is_available) else {
            return false;
        };
        // Cache only complete visible scenes. Frozen panes never build from the
        // live grid, even when AppKit requests a forced draw or another pane moves.
        let mut scenes = std::mem::take(&mut self.pane_scenes);
        scenes.retain(|id| panes.iter().any(|pane| pane.id == id));
        let frozen: Vec<bool> = panes.iter()
            .map(|pane| pane.grid.synchronized_output_active())
            .collect();
        let mut chrome_vertices = Vec::new();
        for (pane, &synchronized) in panes.iter().zip(&frozen) {
            scenes.update_with(pane.id, synchronized, || {
                let mut content = Vec::new();
                self.build_pane_vertices(pane, atlas, selection_fg, selection_bg,
                    status_bar_height, prompt_indicator_color, &mut content);
                PaneScene {
                    origin: [pane.rect.x, pane.rect.y],
                    content,
                    atlas_texture: self.atlas_texture.clone(),
                    images: Self::build_pane_images(pane, atlas, status_bar_height, image_store),
                }
            });
            self.build_status_bar_vertices(pane, atlas, chrome, status_bar_height, &mut chrome_vertices);
        }

        let default_bg = self.colors.default_background();
        let bg_packed = Self::pack_color(default_bg.0, default_bg.1, default_bg.2, 255);
        let mut divider_vertices = Vec::new();
        for div in dividers {
            divider_vertices.extend(self.build_divider_vertices(div, bg_packed, chrome));
        }
        let mut overlay_vertices = Vec::new();
        if let Some(overlay) = confirm_overlay {
            self.build_confirm_overlay(overlay, atlas, chrome, &mut overlay_vertices);
        }

        // Atlas handles in cached scenes and submitted GPU work must remain
        // immutable, including across font zoom and backing-scale changes.
        if atlas.dirty {
            self.upload_atlas(atlas);
        }
        for (pane, &synchronized) in panes.iter().zip(&frozen) {
            if !synchronized {
                if let Some(scene) = scenes.get_mut(pane.id) {
                    scene.atlas_texture = self.atlas_texture.clone();
                }
            }
        }

        let mut all_vertices = Vec::new();
        let mut content_draw_calls = Vec::new();
        let mut image_draw_calls = Vec::new();
        let viewport = [viewport_width, viewport_height];
        for pane in panes {
            let Some(scene) = scenes.get(pane.id) else { continue };
            let Some(clip) = content_clip(pane.rect, status_bar_height, viewport) else { continue };
            let start = all_vertices.len();
            append_translated(&mut all_vertices, &scene.content, scene.origin, pane.rect);
            content_draw_calls.push(SceneDrawCall {
                vertex_start: start,
                vertex_count: scene.content.len(),
                texture: scene.atlas_texture.clone(),
                clip,
            });
            for image in &scene.images {
                let start = all_vertices.len();
                append_translated(&mut all_vertices, &image.vertices, scene.origin, pane.rect);
                image_draw_calls.push(SceneDrawCall {
                    vertex_start: start,
                    vertex_count: image.vertices.len(),
                    texture: image.texture.clone(),
                    clip,
                });
            }
        }
        self.pane_scenes = scenes;
        let chrome_offset = all_vertices.len();
        let chrome_count = chrome_vertices.len();
        all_vertices.extend(chrome_vertices);
        let divider_offset = all_vertices.len();
        let divider_count = divider_vertices.len();
        all_vertices.extend(divider_vertices);
        let overlay_offset = all_vertices.len();
        let overlay_count = overlay_vertices.len();
        all_vertices.extend(overlay_vertices);

        unsafe {
            let frame = &mut self.frames[frame_index];
            let needed = all_vertices.len() * std::mem::size_of::<Vertex>();
            if needed > frame.vertices.length() {
                frame.vertices = self
                    .device
                    .newBufferWithLength_options(needed * 2, MTLResourceOptions::StorageModeShared)
                    .expect("Failed to resize vertex buffer");
            }

            let ptr = frame.vertices.contents().as_ptr() as *mut Vertex;
            std::ptr::copy_nonoverlapping(all_vertices.as_ptr(), ptr, all_vertices.len());

            let ptr = frame.uniforms.contents().as_ptr() as *mut [f32; 2];
            *ptr = [viewport_width, viewport_height];

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

            let (br, bg_r, bb) = self.colors.default_background();
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
            encoder.setVertexBuffer_offset_atIndex(Some(&frame.vertices), 0, 0);
            encoder.setVertexBuffer_offset_atIndex(Some(&frame.uniforms), 0, 1);
            encoder.setFragmentSamplerState_atIndex(Some(&self.sampler_state), 0);

            for call in &content_draw_calls {
                if call.vertex_count == 0 { continue; }
                encoder.setScissorRect(MTLScissorRect {
                    x: call.clip[0], y: call.clip[1], width: call.clip[2], height: call.clip[3],
                });
                encoder.setFragmentTexture_atIndex(Some(&call.texture), 0);
                encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::Triangle, call.vertex_start, call.vertex_count,
                );
            }

            // Image handles come from the presented scene, not the live store:
            // deletion or animation uploads during BSU cannot leak into this frame.
            encoder.setRenderPipelineState(&self.image_pipeline_state);
            for call in &image_draw_calls {
                encoder.setScissorRect(MTLScissorRect {
                    x: call.clip[0], y: call.clip[1], width: call.clip[2], height: call.clip[3],
                });
                encoder.setFragmentTexture_atIndex(Some(&call.texture), 0);
                encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::Triangle, call.vertex_start, call.vertex_count,
                );
            }

            encoder.setScissorRect(MTLScissorRect {
                x: 0, y: 0, width: viewport_width as usize, height: viewport_height as usize,
            });
            encoder.setRenderPipelineState(&self.pipeline_state);
            if chrome_count > 0 {
                encoder.setFragmentTexture_atIndex(Some(&self.atlas_texture), 0);
                encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::Triangle, chrome_offset, chrome_count,
                );
            }

            // Draw dividers with brush texture
            if divider_count > 0 {
                encoder.setRenderPipelineState(&self.pipeline_state);
                encoder.setFragmentTexture_atIndex(Some(&self.brush.texture), 0);
                encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::Triangle,
                    divider_offset,
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

            if let Some(drawable) = drawable {
                command_buffer.presentDrawable(drawable);
            }
            frame.submission = Some(command_buffer.clone());
            command_buffer.commit();
        }
        true
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
        let (white_u, white_v) = white_pixel_uv(atlas.width, atlas.height);

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
            let gx0 = x + glyph.bearing_x as f32;
            let gy0 = y + atlas.ascent + glyph.bearing_y as f32;
            let gx1 = gx0 + glyph.pixel_w as f32;
            let gy1 = gy0 + glyph.pixel_h as f32;
            let (u0, v0, u1, v1) =
                glyph_uv_bounds(atlas.width, atlas.height, &glyph);
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

#[cfg(test)]
mod tests {
    use super::{
        cell_content_is_visible, glyph_uv_bounds, status_cwd_suffix, white_pixel_uv, MetalRenderer,
    };
    use crate::glyph_atlas::GlyphEntry;
    use crate::grid::cell::{CellFlags, Color};
    use crate::grid::Grid;
    use crate::glyph_atlas::GlyphAtlas;
    use crate::layout::PixelRect;
    use crate::render_scene::{ChromeColors, PaneRenderData};
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_metal::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() <= f32::EPSILON);
    }

    #[test]
    fn status_cwd_truncation_keeps_utf8_boundaries_during_resize() {
        for cwd in ["/tmp/café", "/日本語/端末", "/tmp/🦊/café", "short", ""] {
            let characters: Vec<char> = cwd.chars().collect();
            for limit in 0..=characters.len() + 2 {
                let expected: String = characters.iter()
                    .skip(characters.len().saturating_sub(limit)).collect();
                assert_eq!(status_cwd_suffix(cwd, limit), expected);
            }
        }
    }

    fn test_target(renderer: &MetalRenderer, width: usize, height: usize) -> super::MetalTexture {
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm, width, height, false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        descriptor.setStorageMode(MTLStorageMode::Shared);
        renderer.device.newTextureWithDescriptor(&descriptor).unwrap()
    }

    fn test_draw(
        renderer: &mut MetalRenderer,
        atlas: &mut GlyphAtlas,
        grid: &Grid,
        texture: &ProtocolObject<dyn MTLTexture>,
    ) -> bool {
        let pane = PaneRenderData {
            id: 1,
            grid,
            rect: PixelRect { x: 0.0, y: 0.0, width: texture.width() as f32, height: texture.height() as f32 },
            selection: None,
            is_focused: false,
            pane_index: 0,
            cwd: "",
            prompt_mark_rows: Vec::new(),
            show_cursor: false,
        };
        let chrome = ChromeColors {
            sumi_dark: (0, 0, 0), sumi_medium: (0, 0, 0), sumi_light: (0, 0, 0),
            sumi_ghost: (0, 0, 0), hanko_red: (0, 0, 0), hanko_dim: (0, 0, 0),
        };
        renderer.draw_frame(&[pane], &[], atlas, None, texture,
            texture.width() as f32, texture.height() as f32, 1.0,
            (255, 255, 255), (0, 0, 0), &chrome, 0.0, None, None, None)
    }

    fn paint_test_grid(grid: &mut Grid, background: (u8, u8, u8)) {
        for row in 0..grid.rows() {
            for column in 0..grid.cols() {
                grid.buffer.cell_mut(row, column).bg = Color::Rgb(background.0, background.1, background.2);
            }
        }
    }

    fn test_pixel(texture: &ProtocolObject<dyn MTLTexture>, x: usize, y: usize) -> [u8; 4] {
        let mut bytes = [0u8; 4];
        unsafe {
            texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(bytes.as_mut_ptr().cast()).unwrap(), 4,
                MTLRegion { origin: MTLOrigin { x, y, z: 0 }, size: MTLSize { width: 1, height: 1, depth: 1 } },
                0,
            );
        }
        bytes
    }

    fn test_region(texture: &ProtocolObject<dyn MTLTexture>, width: usize, height: usize) -> Vec<u8> {
        let mut bytes = vec![0; width * height * 4];
        unsafe {
            texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                std::ptr::NonNull::new(bytes.as_mut_ptr().cast()).unwrap(), width * 4,
                MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width, height, depth: 1 } },
                0,
            );
        }
        bytes
    }

    fn finish_test_frames(renderer: &MetalRenderer) {
        for frame in &renderer.frames {
            if let Some(submission) = &frame.submission {
                submission.waitUntilCompleted();
                assert_eq!(submission.status(), MTLCommandBufferStatus::Completed);
            }
        }
    }

    #[test]
    fn metal_in_flight_frames_keep_distinct_vertices_and_viewports_until_gpu_completion() {
        let Some(device) = MTLCreateSystemDefaultDevice() else { return };
        let mut renderer = MetalRenderer::new(device, (255, 255, 255), (0, 0, 0));
        let mut atlas = GlyphAtlas::new("Menlo", 12.0, 1.0).unwrap();
        let mut grid = Grid::new(8, 4, 0);
        let sample_x = (atlas.cell_width * grid.cols() as f32 - 2.0) as usize;

        // Hold the real GPU queue so all three submitted frames remain in flight.
        // Release on unwind too, so a failed assertion cannot leave the GPU waiting.
        struct ReleaseEvent(Retained<ProtocolObject<dyn MTLSharedEvent>>);
        impl Drop for ReleaseEvent {
            fn drop(&mut self) { self.0.setSignaledValue(1); }
        }
        let event = ReleaseEvent(renderer.device.newSharedEvent().unwrap());
        let gate = renderer.command_queue.commandBuffer().unwrap();
        gate.encodeWaitForEvent_value(ProtocolObject::from_ref(&*event.0), 1);
        gate.commit();

        let colors = [(200, 10, 20), (10, 200, 20), (10, 20, 200)];
        let mut targets = Vec::new();
        for (index, color) in colors.into_iter().enumerate() {
            paint_test_grid(&mut grid, color);
            let texture = test_target(&renderer, 96 + index * 32, 96);
            assert!(test_draw(&mut renderer, &mut atlas, &grid, &texture));
            targets.push(texture);
        }
        assert!(renderer.frames.iter().all(|frame| !frame.is_available()));
        let previous = renderer.pane_scenes.get(1).unwrap().content[0].bg_color;
        paint_test_grid(&mut grid, (255, 255, 255));
        let rejected = test_target(&renderer, 256, 128);
        assert!(!test_draw(&mut renderer, &mut atlas, &grid, &rejected));
        assert_eq!(renderer.pane_scenes.get(1).unwrap().content[0].bg_color, previous);

        drop(event);
        finish_test_frames(&renderer);
        for (texture, (r, g, b)) in targets.iter().zip(colors) {
            assert_eq!(test_pixel(texture, sample_x, 2), [b, g, r, 255]);
        }
        assert!(test_draw(&mut renderer, &mut atlas, &grid, &rejected));
        finish_test_frames(&renderer);
        assert_eq!(test_pixel(&rejected, sample_x, 2), [255; 4]);
    }

    #[test]
    fn metal_synchronized_scene_keeps_previous_pixels_through_resize_until_release() {
        let Some(device) = MTLCreateSystemDefaultDevice() else { return };
        let mut renderer = MetalRenderer::new(device, (255, 255, 255), (0, 0, 0));
        let mut atlas = GlyphAtlas::new("Menlo", 12.0, 1.0).unwrap();
        let mut grid = Grid::new(8, 4, 0);
        paint_test_grid(&mut grid, (200, 10, 20));
        grid.buffer.cell_mut(0, 0).set_char('A');
        let glyph_region = [atlas.cell_width.ceil() as usize, atlas.cell_height.ceil() as usize];
        let first = test_target(&renderer, 128, 96);
        assert!(test_draw(&mut renderer, &mut atlas, &grid, &first));
        finish_test_frames(&renderer);
        let glyph_pixels = test_region(&first, glyph_region[0], glyph_region[1]);
        assert!(glyph_pixels.chunks_exact(4).any(|pixel| pixel != [20, 10, 200, 255]));
        assert_eq!(test_pixel(&first, 40, 2), [20, 10, 200, 255]);

        grid.set_synchronized_output_at(true, std::time::Instant::now() + std::time::Duration::from_secs(60));
        paint_test_grid(&mut grid, (10, 200, 20));
        atlas.clear_and_resize(18.0).unwrap();
        let resized = test_target(&renderer, 96, 96);
        assert!(test_draw(&mut renderer, &mut atlas, &grid, &resized));
        finish_test_frames(&renderer);
        assert_eq!(test_region(&resized, glyph_region[0], glyph_region[1]), glyph_pixels);
        assert_eq!(test_pixel(&resized, 40, 2), [20, 10, 200, 255]);

        grid.set_synchronized_output(false);
        assert!(test_draw(&mut renderer, &mut atlas, &grid, &resized));
        finish_test_frames(&renderer);
        assert_eq!(test_pixel(&resized, 40, 2), [20, 200, 10, 255]);
    }

    #[test]
    fn converts_integer_atlas_coordinates_to_metal_uvs() {
        let glyph = GlyphEntry {
            atlas_x: 20,
            atlas_y: 20,
            pixel_w: 50,
            pixel_h: 20,
            bearing_x: -2,
            bearing_y: 3,
        };

        let (white_u, white_v) = white_pixel_uv(200, 80);
        assert_close(white_u, 0.0025);
        assert_close(white_v, 0.00625);

        let (u0, v0, u1, v1) = glyph_uv_bounds(200, 80, &glyph);
        assert_close(u0, 0.1);
        assert_close(v0, 0.25);
        assert_close(u1, 0.35);
        assert_close(v1, 0.5);
    }

    #[test]
    fn packs_resolved_terminal_colors_with_opaque_alpha() {
        let packed = MetalRenderer::pack_color(0x12, 0x34, 0x56, u8::MAX);

        assert_eq!(packed, 0x1234_56ff);
        assert_eq!(packed & 0xff, u32::from(u8::MAX));
    }

    #[test]
    fn concealed_metal_cells_never_render_content_or_decorations() {
        for visible_flags in [
            CellFlags::empty(),
            CellFlags::BOLD | CellFlags::FAINT | CellFlags::REVERSE,
            CellFlags::ITALIC | CellFlags::UNDERLINE | CellFlags::WIDE,
        ] {
            assert!(cell_content_is_visible(visible_flags));
            assert!(!cell_content_is_visible(visible_flags | CellFlags::HIDDEN));
        }

        assert!(!cell_content_is_visible(
            CellFlags::HIDDEN | CellFlags::UNDERLINE | CellFlags::REVERSE
        ));
        let selection_foreground = (0x11, 0x22, 0x33);
        let selection_background = (0x44, 0x55, 0x66);
        let (foreground, background) = MetalRenderer::pack_cell_colors(
            (0xaa, 0xbb, 0xcc),
            (0xdd, 0xee, 0xff),
            selection_foreground,
            selection_background,
            true,
        );
        assert_eq!(
            foreground,
            MetalRenderer::pack_color(0x11, 0x22, 0x33, u8::MAX)
        );
        assert_eq!(
            background,
            MetalRenderer::pack_color(0x44, 0x55, 0x66, u8::MAX)
        );
        assert!(!cell_content_is_visible(CellFlags::HIDDEN));
    }
}
