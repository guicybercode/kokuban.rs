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

const MAX_ATLAS_HEIGHT: u32 = 4096;

pub struct GlyphAtlas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub glyphs: HashMap<GlyphKey, GlyphEntry>,
    pub cell_width: f32,
    pub cell_height: f32,
    pub ascent: f32,
    pub descent: f32,
    fonts: Vec<Font>,
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
        Self::new_with_fallback(font_family, font_size, scale_factor, &[])
    }

    pub fn new_with_fallback(
        font_family: &str,
        font_size: f32,
        scale_factor: f32,
        fallback_families: &[String],
    ) -> Self {
        let source = SystemSource::new();
        let mut props = font_kit::properties::Properties::new();
        props.style = font_kit::properties::Style::Normal;
        props.weight = font_kit::properties::Weight::NORMAL;
        let primary_font = source
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

        // Build fallback chain
        let mut fonts = vec![primary_font];
        let primary_name = fonts[0].full_name();
        for family in fallback_families {
            match source.select_best_match(
                &[font_kit::family_name::FamilyName::Title(family.clone())],
                &props,
            ) {
                Ok(handle) => match handle.load() {
                    Ok(f) => {
                        let name = f.full_name();
                        if name != primary_name {
                            log::info!("Fallback font loaded: {name}");
                            fonts.push(f);
                        }
                    }
                    Err(e) => log::debug!("Skipping fallback '{family}': {e}"),
                },
                Err(e) => log::debug!("Skipping fallback '{family}': {e}"),
            }
        }

        let metrics = fonts[0].metrics();
        let units_per_em = metrics.units_per_em as f32;
        let scaled_size = font_size * scale_factor;

        let ascent = metrics.ascent / units_per_em * scaled_size;
        let descent = metrics.descent / units_per_em * scaled_size; // negative
        let leading = metrics.line_gap / units_per_em * scaled_size;

        let cell_height = (ascent - descent + leading).ceil();
        // Use advance of 'M' for cell width
        let glyph_id = fonts[0].glyph_for_char('M').unwrap_or(0);
        let advance = fonts[0].advance(glyph_id).unwrap_or_default();
        let cell_width = (advance.x() / units_per_em * scaled_size).ceil();

        log::info!(
            "Font: {} size={font_size} scale={scale_factor} cell={}x{} ascent={:.1} descent={:.1} fallbacks={}",
            fonts[0].full_name(),
            cell_width,
            cell_height,
            ascent,
            descent,
            fonts.len() - 1,
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
            fonts,
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

    /// Clear all cached glyphs and reset the atlas. Glyphs will be re-rasterized lazily.
    pub fn clear_and_resize(&mut self, new_font_size: f32) {
        self.font_size = new_font_size;
        let scaled_size = self.font_size * self.scale_factor;

        // Derive metrics from primary font only
        let metrics = self.fonts[0].metrics();
        let units_per_em = metrics.units_per_em as f32;
        let ascent = metrics.ascent / units_per_em * scaled_size;
        let descent = metrics.descent / units_per_em * scaled_size;
        let leading = metrics.line_gap / units_per_em * scaled_size;

        self.cell_height = (ascent - descent + leading).ceil();
        let glyph_id = self.fonts[0].glyph_for_char('M').unwrap_or(0);
        let advance = self.fonts[0].advance(glyph_id).unwrap_or_default();
        self.cell_width = (advance.x() / units_per_em * scaled_size).ceil();
        self.ascent = ascent;
        self.descent = descent;

        log::info!(
            "Font resized: size={new_font_size} cell={}x{} ascent={:.1}",
            self.cell_width,
            self.cell_height,
            self.ascent,
        );

        // Reset to initial size
        self.height = 1024;
        self.pixels = vec![0u8; (self.width * self.height) as usize];
        self.pixels[0] = 255; // white pixel at (0,0)
        self.white_uv = (0.5 / self.width as f32, 0.5 / self.height as f32);
        self.glyphs.clear();
        self.cursor_x = 2;
        self.cursor_y = 0;
        self.row_height = 0;
        self.dirty = true;

        // Pre-rasterize ASCII
        for c in ' '..='~' {
            self.get_or_insert(GlyphKey {
                c,
                bold: false,
                italic: false,
            });
        }
    }

    /// Find the first font in the chain that has a glyph for the given character.
    fn find_font_for_char(&self, c: char) -> Option<(usize, u32)> {
        for (i, font) in self.fonts.iter().enumerate() {
            if let Some(glyph_id) = font.glyph_for_char(c) {
                return Some((i, glyph_id));
            }
        }
        None
    }

    /// Grow the atlas height by doubling, up to MAX_ATLAS_HEIGHT.
    fn grow_atlas(&mut self) -> bool {
        let new_height = (self.height * 2).min(MAX_ATLAS_HEIGHT);
        if new_height == self.height {
            return false; // Already at max
        }
        log::info!("Growing atlas from {}x{} to {}x{}", self.width, self.height, self.width, new_height);
        let mut new_pixels = vec![0u8; (self.width * new_height) as usize];
        // Copy existing data
        new_pixels[..(self.width * self.height) as usize]
            .copy_from_slice(&self.pixels);
        self.pixels = new_pixels;
        self.height = new_height;
        // Recompute white_uv (width unchanged so x stays, y changes)
        self.white_uv = (0.5 / self.width as f32, 0.5 / self.height as f32);
        // Recompute all existing glyph UVs since height changed
        for entry in self.glyphs.values_mut() {
            // UV coordinates were based on old height — we need to recompute from pixel positions
            // pixel_y = entry.uv_y * old_height, new uv_y = pixel_y / new_height
            // But we don't store pixel positions. Since width is unchanged and we doubled height,
            // uv_y and uv_h simply halve.
            entry.uv_y *= (self.height / 2) as f32 / self.height as f32;
            entry.uv_h *= (self.height / 2) as f32 / self.height as f32;
        }
        self.dirty = true;
        true
    }

    pub fn get_or_insert(&mut self, key: GlyphKey) -> GlyphEntry {
        if let Some(&entry) = self.glyphs.get(&key) {
            return entry;
        }

        let (font_idx, glyph_id) = match self.find_font_for_char(key.c) {
            Some(result) => result,
            None => {
                // No font has this glyph — return empty
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

        let font = &self.fonts[font_idx];
        let scaled_size = self.font_size * self.scale_factor;
        let raster_rect = font
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

        // Check if atlas is full — try to grow
        if self.cursor_y + glyph_h > self.height {
            if !self.grow_atlas() {
                log::warn!("Glyph atlas full at max size, cannot rasterize '{}'", key.c);
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
        }

        // Rasterize
        let mut canvas = Canvas::new(
            Vector2I::new(glyph_w as i32, glyph_h as i32),
            Format::A8,
        );

        let origin = Vector2F::new(-raster_rect.origin_x() as f32, -raster_rect.origin_y() as f32);
        font.rasterize_glyph(
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
