use font_kit::canvas::{Canvas, Format, RasterizationOptions};
use font_kit::error::{FontLoadingError, SelectionError};
use font_kit::family_name::FamilyName;
use font_kit::font::Font;
use font_kit::hinting::HintingOptions;
use font_kit::properties::{Properties, Style, Weight};
use font_kit::source::{Source, SystemSource};
use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::{Vector2F, Vector2I};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GlyphAtlasError {
    #[error(
        "font size and scale factor must be finite and greater than zero \
         (font size: {font_size}, scale factor: {scale_factor})"
    )]
    InvalidSizing { font_size: f32, scale_factor: f32 },
    #[error(
        "configured font '{requested_family}' failed ({requested_error}); \
         system monospace fallback selection failed: {source}"
    )]
    FallbackSelection {
        requested_family: String,
        requested_error: String,
        #[source]
        source: SelectionError,
    },
    #[error(
        "configured font '{requested_family}' failed ({requested_error}); \
         system monospace fallback loading failed: {source}"
    )]
    FallbackLoad {
        requested_family: String,
        requested_error: String,
        #[source]
        source: FontLoadingError,
    },
    #[error(
        "font '{font_name}' produced invalid metrics at size {font_size} and scale {scale_factor}: \
         {reason}"
    )]
    InvalidMetrics {
        font_name: String,
        font_size: f32,
        scale_factor: f32,
        reason: &'static str,
    },
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct GlyphKey {
    pub c: char,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct GlyphEntry {
    pub atlas_x: u32,
    pub atlas_y: u32,
    pub pixel_w: u32,
    pub pixel_h: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
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
}

#[derive(Debug, Clone, Copy)]
struct ScaledFontMetrics {
    cell_width: f32,
    cell_height: f32,
    ascent: f32,
    descent: f32,
}

impl GlyphAtlas {
    pub fn new(
        font_family: &str,
        font_size: f32,
        scale_factor: f32,
    ) -> Result<Self, GlyphAtlasError> {
        let source = SystemSource::new();
        Self::new_with_source(&source, font_family, font_size, scale_factor)
    }

    fn new_with_source(
        source: &dyn Source,
        font_family: &str,
        font_size: f32,
        scale_factor: f32,
    ) -> Result<Self, GlyphAtlasError> {
        validate_sizing(font_size, scale_factor)?;

        let mut properties = Properties::new();
        properties.style = Style::Normal;
        properties.weight = Weight::NORMAL;

        let requested_font = source
            .select_best_match(&[FamilyName::Title(font_family.to_string())], &properties)
            .map_err(|error| format!("selection failed: {error}"))
            .and_then(|handle| {
                handle
                    .load()
                    .map_err(|error| format!("loading failed: {error}"))
            });

        let font = match requested_font {
            Ok(font) => font,
            Err(requested_error) => {
                log::warn!(
                    "Font '{font_family}' failed ({requested_error}); falling back to system monospace"
                );

                let fallback_handle = source
                    .select_best_match(&[FamilyName::Monospace], &properties)
                    .map_err(|source| GlyphAtlasError::FallbackSelection {
                        requested_family: font_family.to_string(),
                        requested_error: requested_error.clone(),
                        source,
                    })?;

                fallback_handle
                    .load()
                    .map_err(|source| GlyphAtlasError::FallbackLoad {
                        requested_family: font_family.to_string(),
                        requested_error,
                        source,
                    })?
            }
        };

        let metrics = scaled_font_metrics(&font, font_size, scale_factor)?;
        let cell_width = metrics.cell_width;
        let cell_height = metrics.cell_height;
        let ascent = metrics.ascent;
        let descent = metrics.descent;

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
        };

        // Pre-rasterize ASCII printable range
        for c in ' '..='~' {
            atlas.get_or_insert(GlyphKey {
                c,
                bold: false,
                italic: false,
            });
        }

        Ok(atlas)
    }

    /// Clear all cached glyphs and reset the atlas. Glyphs will be re-rasterized lazily.
    pub fn clear_and_resize(&mut self, new_font_size: f32) -> Result<(), GlyphAtlasError> {
        let metrics = scaled_font_metrics(&self.font, new_font_size, self.scale_factor)?;

        self.font_size = new_font_size;
        self.cell_width = metrics.cell_width;
        self.cell_height = metrics.cell_height;
        self.ascent = metrics.ascent;
        self.descent = metrics.descent;

        log::info!(
            "Font resized: size={new_font_size} cell={}x{} ascent={:.1}",
            self.cell_width,
            self.cell_height,
            self.ascent,
        );

        // Reset pixel buffer
        self.pixels.fill(0);
        self.pixels[0] = 255; // white pixel at (0,0)
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

        Ok(())
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
                    atlas_x: 0,
                    atlas_y: 0,
                    pixel_w: 0,
                    pixel_h: 0,
                    bearing_x: 0,
                    bearing_y: 0,
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
                atlas_x: 0,
                atlas_y: 0,
                pixel_w: 0,
                pixel_h: 0,
                bearing_x: 0,
                bearing_y: 0,
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
                atlas_x: 0,
                atlas_y: 0,
                pixel_w: 0,
                pixel_h: 0,
                bearing_x: 0,
                bearing_y: 0,
            };
            self.glyphs.insert(key, entry);
            return entry;
        }

        // Rasterize
        let mut canvas = Canvas::new(Vector2I::new(glyph_w as i32, glyph_h as i32), Format::A8);

        let origin = Vector2F::new(
            -raster_rect.origin_x() as f32,
            -raster_rect.origin_y() as f32,
        );
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
            atlas_x: self.cursor_x,
            atlas_y: self.cursor_y,
            pixel_w: glyph_w,
            pixel_h: glyph_h,
            bearing_x: raster_rect.origin_x(),
            bearing_y: raster_rect.origin_y(),
        };

        self.cursor_x += glyph_w + 1;
        self.row_height = self.row_height.max(glyph_h);
        self.dirty = true;

        self.glyphs.insert(key, entry);
        entry
    }
}

fn validate_sizing(font_size: f32, scale_factor: f32) -> Result<(), GlyphAtlasError> {
    let scaled_size = font_size * scale_factor;
    if !font_size.is_finite()
        || font_size <= 0.0
        || !scale_factor.is_finite()
        || scale_factor <= 0.0
        || !scaled_size.is_finite()
        || scaled_size <= 0.0
    {
        return Err(GlyphAtlasError::InvalidSizing {
            font_size,
            scale_factor,
        });
    }

    Ok(())
}

fn scaled_font_metrics(
    font: &Font,
    font_size: f32,
    scale_factor: f32,
) -> Result<ScaledFontMetrics, GlyphAtlasError> {
    validate_sizing(font_size, scale_factor)?;

    let metrics = font.metrics();
    if metrics.units_per_em == 0 {
        return Err(invalid_metrics_error(
            font,
            font_size,
            scale_factor,
            "units per em is zero",
        ));
    }

    let units_per_em = metrics.units_per_em as f32;
    let scaled_size = font_size * scale_factor;
    let ascent = metrics.ascent / units_per_em * scaled_size;
    let descent = metrics.descent / units_per_em * scaled_size;
    let leading = metrics.line_gap / units_per_em * scaled_size;
    let cell_height = (ascent - descent + leading).ceil();

    let glyph_id = font.glyph_for_char('M').unwrap_or(0);
    let advance = font.advance(glyph_id).unwrap_or_default();
    let cell_width = (advance.x() / units_per_em * scaled_size).ceil();

    if !cell_width.is_finite()
        || cell_width <= 0.0
        || !cell_height.is_finite()
        || cell_height <= 0.0
        || !ascent.is_finite()
        || !descent.is_finite()
    {
        return Err(invalid_metrics_error(
            font,
            font_size,
            scale_factor,
            "cell dimensions are not finite and positive, or vertical metrics are not finite",
        ));
    }

    Ok(ScaledFontMetrics {
        cell_width,
        cell_height,
        ascent,
        descent,
    })
}

fn invalid_metrics_error(
    font: &Font,
    font_size: f32,
    scale_factor: f32,
    reason: &'static str,
) -> GlyphAtlasError {
    GlyphAtlasError::InvalidMetrics {
        font_name: font.full_name(),
        font_size,
        scale_factor,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use font_kit::family_handle::FamilyHandle;
    use font_kit::handle::Handle;
    use font_kit::sources::mem::MemSource;
    use std::any::Any;
    use std::sync::Arc;

    const MISSING_FONT_FAMILY: &str = "kokuban-test-font-that-does-not-exist";

    struct InvalidFontSource;

    impl Source for InvalidFontSource {
        fn all_fonts(&self) -> Result<Vec<Handle>, SelectionError> {
            Ok(Vec::new())
        }

        fn all_families(&self) -> Result<Vec<String>, SelectionError> {
            Ok(Vec::new())
        }

        fn select_family_by_name(
            &self,
            _family_name: &str,
        ) -> Result<FamilyHandle, SelectionError> {
            Err(SelectionError::NotFound)
        }

        fn select_best_match(
            &self,
            _family_names: &[FamilyName],
            _properties: &Properties,
        ) -> Result<Handle, SelectionError> {
            Ok(Handle::from_memory(Arc::new(Vec::new()), 0))
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_mut_any(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[test]
    fn rejects_invalid_sizing_before_accessing_font_source() {
        let source = MemSource::empty();
        let invalid_sizes = [
            (0.0, 1.0),
            (-1.0, 1.0),
            (f32::NAN, 1.0),
            (f32::INFINITY, 1.0),
            (14.0, 0.0),
            (14.0, -1.0),
            (14.0, f32::NAN),
            (14.0, f32::INFINITY),
            (f32::MAX, f32::MAX),
        ];

        for (font_size, scale_factor) in invalid_sizes {
            let result =
                GlyphAtlas::new_with_source(&source, MISSING_FONT_FAMILY, font_size, scale_factor);

            assert!(matches!(result, Err(GlyphAtlasError::InvalidSizing { .. })));
        }
    }

    #[test]
    fn reports_when_requested_and_fallback_fonts_cannot_be_selected() {
        let source = MemSource::empty();
        let result = GlyphAtlas::new_with_source(&source, MISSING_FONT_FAMILY, 14.0, 1.0);
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("empty font source unexpectedly created an atlas"),
        };
        let message = error.to_string();

        match error {
            GlyphAtlasError::FallbackSelection {
                requested_family,
                requested_error,
                source: SelectionError::NotFound,
            } => {
                assert_eq!(requested_family, MISSING_FONT_FAMILY);
                assert!(requested_error.contains("selection failed"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(message.contains(MISSING_FONT_FAMILY));
        assert!(message.contains("system monospace fallback selection failed"));
    }

    #[test]
    fn reports_when_requested_and_fallback_fonts_cannot_be_loaded() {
        let result =
            GlyphAtlas::new_with_source(&InvalidFontSource, MISSING_FONT_FAMILY, 14.0, 1.0);
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("invalid font source unexpectedly created an atlas"),
        };
        let message = error.to_string();

        match error {
            GlyphAtlasError::FallbackLoad {
                requested_family,
                requested_error,
                ..
            } => {
                assert_eq!(requested_family, MISSING_FONT_FAMILY);
                assert!(requested_error.contains("loading failed"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(message.contains(MISSING_FONT_FAMILY));
        assert!(message.contains("system monospace fallback loading failed"));
    }

    #[test]
    fn rasterizes_visible_glyph_into_a8_atlas() {
        let mut atlas = GlyphAtlas::new(MISSING_FONT_FAMILY, 14.0, 1.0)
            .expect("system monospace fallback should be available");
        let glyph = atlas.get_or_insert(GlyphKey {
            c: 'A',
            bold: false,
            italic: false,
        });

        assert_eq!(atlas.pixels.len(), (atlas.width * atlas.height) as usize);
        assert_eq!(atlas.pixels[0], 255);
        assert!(atlas.cell_width.is_finite() && atlas.cell_width > 0.0);
        assert!(atlas.cell_height.is_finite() && atlas.cell_height > 0.0);
        assert!(atlas.ascent.is_finite() && atlas.ascent > 0.0);
        assert!(atlas.descent.is_finite() && atlas.descent <= 0.0);
        assert!(glyph.pixel_w > 0);
        assert!(glyph.pixel_h > 0);

        for cached in atlas.glyphs.values() {
            assert!(cached.atlas_x.saturating_add(cached.pixel_w) <= atlas.width);
            assert!(cached.atlas_y.saturating_add(cached.pixel_h) <= atlas.height);
        }
        let has_coverage = (0..glyph.pixel_h).any(|y| {
            (0..glyph.pixel_w).any(|x| {
                let index =
                    ((glyph.atlas_y + y) * atlas.width + glyph.atlas_x + x) as usize;
                atlas.pixels[index] != 0
            })
        });
        assert!(has_coverage);
    }

    #[test]
    fn cached_glyph_lookup_does_not_dirty_or_grow_atlas() {
        let mut atlas = GlyphAtlas::new(MISSING_FONT_FAMILY, 14.0, 1.0)
            .expect("system monospace fallback should be available");
        let key = GlyphKey {
            c: 'A',
            bold: false,
            italic: false,
        };
        let expected = atlas.get_or_insert(key);
        let cached_glyph_count = atlas.glyphs.len();
        atlas.dirty = false;

        let cached = atlas.get_or_insert(key);

        assert_eq!(atlas.glyphs.len(), cached_glyph_count);
        assert!(!atlas.dirty);
        assert_eq!(cached.atlas_x, expected.atlas_x);
        assert_eq!(cached.atlas_y, expected.atlas_y);
        assert_eq!(cached.pixel_w, expected.pixel_w);
        assert_eq!(cached.pixel_h, expected.pixel_h);
    }

    #[test]
    fn invalid_resize_preserves_existing_atlas_state() {
        let mut atlas = GlyphAtlas::new(MISSING_FONT_FAMILY, 14.0, 1.0)
            .expect("system monospace fallback should be available");
        let original_font_size = atlas.font_size;
        let original_cell_width = atlas.cell_width;
        let original_cell_height = atlas.cell_height;
        let original_glyph_count = atlas.glyphs.len();
        atlas.dirty = false;

        let result = atlas.clear_and_resize(f32::NAN);

        assert!(matches!(result, Err(GlyphAtlasError::InvalidSizing { .. })));
        assert_eq!(atlas.font_size, original_font_size);
        assert_eq!(atlas.cell_width, original_cell_width);
        assert_eq!(atlas.cell_height, original_cell_height);
        assert_eq!(atlas.glyphs.len(), original_glyph_count);
        assert!(!atlas.dirty);
    }
}
