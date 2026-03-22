use font_kit::canvas::{Canvas, Format, RasterizationOptions};
use font_kit::font::Font;
use font_kit::hinting::HintingOptions;
use font_kit::source::SystemSource;
use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::{Vector2F, Vector2I};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct GlyphKey {
    pub c: char,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct GlyphEntry {
    pub uv_x: f32,
    pub uv_y: f32,
    pub uv_w: f32,
    pub uv_h: f32,
    pub pixel_w: u32,
    pub pixel_h: u32,
    pub bearing_x: f32,
    pub bearing_y: f32,
}

pub struct GlyphAtlas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub glyphs: HashMap<GlyphKey, GlyphEntry>,
    pub cell_width: f32,
    pub cell_height: f32,
    pub ascent: f32,
    pub descent: f32,
    font: Font,
    font_size: f32,
    scale_factor: f32,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    pub dirty: bool,
    // White pixel for background quads
    pub white_uv: (f32, f32),
}

impl GlyphAtlas {
    pub fn new(font_family: &str, font_size: f32, scale_factor: f32) -> Self {
        let source = SystemSource::new();
        let mut props = font_kit::properties::Properties::new();
        props.style = font_kit::properties::Style::Normal;
        props.weight = font_kit::properties::Weight::NORMAL;
        let font = source
            .select_best_match(
                &[font_kit::family_name::FamilyName::Title(font_family.to_string())],
                &props,
            )
            .ok()
            .and_then(|handle| handle.load().ok())
            .unwrap_or_else(|| {
                log::warn!("Font '{font_family}' not found, falling back to monospace");
                source
                    .select_best_match(
                        &[font_kit::family_name::FamilyName::Monospace],
                        &font_kit::properties::Properties::new(),
                    )
                    .unwrap()
                    .load()
                    .unwrap()
            });

        let metrics = font.metrics();
        let units_per_em = metrics.units_per_em as f32;
        let scaled_size = font_size * scale_factor;

        let ascent = metrics.ascent / units_per_em * scaled_size;
        let descent = metrics.descent / units_per_em * scaled_size; // negative
        let leading = metrics.line_gap / units_per_em * scaled_size;

        let cell_height = (ascent - descent + leading).ceil();
        // Use advance of 'M' for cell width
        let glyph_id = font.glyph_for_char('M').unwrap_or(0);
        let advance = font.advance(glyph_id).unwrap_or_default();
        let cell_width = (advance.x() / units_per_em * scaled_size).ceil();

        log::info!(
            "Font: {} size={font_size} scale={scale_factor} cell={}x{} ascent={:.1} descent={:.1}",
            font.full_name(),
            cell_width,
            cell_height,
            ascent,
            descent
        );

        let width = 1024;
        let height = 1024;
        let mut pixels = vec![0u8; (width * height) as usize];

        // Reserve 1x1 white pixel at (0,0)
        pixels[0] = 255;
        let white_uv = (0.5 / width as f32, 0.5 / height as f32);

        let mut atlas = Self {
            width,
            height,
            pixels,
            glyphs: HashMap::new(),
            cell_width,
            cell_height,
            ascent,
            descent,
            font,
            font_size,
            scale_factor,
            cursor_x: 2, // Start after white pixel
            cursor_y: 0,
            row_height: 0,
            dirty: true,
            white_uv,
        };

        // Pre-rasterize ASCII printable range
        for c in ' '..='~' {
            atlas.get_or_insert(GlyphKey {
                c,
                bold: false,
                italic: false,
            });
        }

        atlas
    }

    pub fn get_or_insert(&mut self, key: GlyphKey) -> GlyphEntry {
        if let Some(&entry) = self.glyphs.get(&key) {
            return entry;
        }

        let glyph_id = match self.font.glyph_for_char(key.c) {
            Some(id) => id,
            None => {
                // Use space entry or create dummy
                let entry = GlyphEntry {
                    uv_x: self.white_uv.0,
                    uv_y: self.white_uv.1,
                    uv_w: 0.0,
                    uv_h: 0.0,
                    pixel_w: 0,
                    pixel_h: 0,
                    bearing_x: 0.0,
                    bearing_y: 0.0,
                };
                self.glyphs.insert(key, entry);
                return entry;
            }
        };

        let scaled_size = self.font_size * self.scale_factor;
        let raster_rect = self
            .font
            .raster_bounds(
                glyph_id,
                scaled_size,
                Transform2F::default(),
                HintingOptions::None,
                RasterizationOptions::GrayscaleAa,
            )
            .unwrap_or_default();

        let glyph_w = raster_rect.width() as u32;
        let glyph_h = raster_rect.height() as u32;

        if glyph_w == 0 || glyph_h == 0 {
            let entry = GlyphEntry {
                uv_x: self.white_uv.0,
                uv_y: self.white_uv.1,
                uv_w: 0.0,
                uv_h: 0.0,
                pixel_w: 0,
                pixel_h: 0,
                bearing_x: 0.0,
                bearing_y: 0.0,
            };
            self.glyphs.insert(key, entry);
            return entry;
        }

        // Check if we need to advance to next row
        if self.cursor_x + glyph_w + 1 > self.width {
            self.cursor_y += self.row_height + 1;
            self.cursor_x = 0;
            self.row_height = 0;
        }

        // Check if atlas is full
        if self.cursor_y + glyph_h > self.height {
            log::warn!("Glyph atlas full, cannot rasterize '{}'", key.c);
            let entry = GlyphEntry {
                uv_x: self.white_uv.0,
                uv_y: self.white_uv.1,
                uv_w: 0.0,
                uv_h: 0.0,
                pixel_w: 0,
                pixel_h: 0,
                bearing_x: 0.0,
                bearing_y: 0.0,
            };
            self.glyphs.insert(key, entry);
            return entry;
        }

        // Rasterize
        let mut canvas = Canvas::new(
            Vector2I::new(glyph_w as i32, glyph_h as i32),
            Format::A8,
        );

        let origin = Vector2F::new(-raster_rect.origin_x() as f32, -raster_rect.origin_y() as f32);
        self.font
            .rasterize_glyph(
                &mut canvas,
                glyph_id,
                scaled_size,
                Transform2F::from_translation(origin),
                HintingOptions::None,
                RasterizationOptions::GrayscaleAa,
            )
            .ok();

        // Copy to atlas
        for y in 0..glyph_h {
            for x in 0..glyph_w {
                let src_idx = (y * glyph_w + x) as usize;
                let dst_x = self.cursor_x + x;
                let dst_y = self.cursor_y + y;
                let dst_idx = (dst_y * self.width + dst_x) as usize;
                self.pixels[dst_idx] = canvas.pixels[src_idx];
            }
        }

        let entry = GlyphEntry {
            uv_x: self.cursor_x as f32 / self.width as f32,
            uv_y: self.cursor_y as f32 / self.height as f32,
            uv_w: glyph_w as f32 / self.width as f32,
            uv_h: glyph_h as f32 / self.height as f32,
            pixel_w: glyph_w,
            pixel_h: glyph_h,
            bearing_x: raster_rect.origin_x() as f32,
            bearing_y: raster_rect.origin_y() as f32,
        };

        self.cursor_x += glyph_w + 1;
        self.row_height = self.row_height.max(glyph_h);
        self.dirty = true;

        self.glyphs.insert(key, entry);
        entry
    }
}
