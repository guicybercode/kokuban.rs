//! Android's A8 glyph atlas, using the Rust fontdue parser/rasterizer.
//!
//! Fonts are read from Android's system directories; no desktop font discovery,
//! FreeType, fontconfig, or bundled proprietary fonts are required. Fallback
//! bytes are loaded on first use and at most two fallback fonts are retained.
//! Fallback outlines are rasterized per glyph instead of expanding whole CJK fonts.

#[path = "android_font_fallback.rs"]
mod font_fallback;

use font_fallback::FontFallback;
use fontdue::{Font, FontSettings, Metrics};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use thiserror::Error;

const ATLAS_SIZE: u32 = 1024;
const MAX_CACHED_GLYPHS: usize = 8192;
const MAX_FALLBACK_FONTS: usize = 2;
const MAX_FONT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SCALED_SIZE: f32 = 256.0;
const SYSTEM_FONT_DIRECTORIES: &[&str] = &["/system/fonts", "/product/fonts"];

#[derive(Debug, Error)]
pub enum GlyphAtlasError {
    #[error("font size and scale factor must be finite and positive, with physical size <= {MAX_SCALED_SIZE} (font size: {font_size}, scale: {scale_factor})")]
    InvalidSizing { font_size: f32, scale_factor: f32 },
    #[error(
        "could not load Android font '{requested_family}' or system monospace fallback: {reason}"
    )]
    FontUnavailable {
        requested_family: String,
        reason: String,
    },
    #[error("Android font has invalid metrics at size {font_size} and scale {scale_factor}")]
    InvalidMetrics { font_size: f32, scale_factor: f32 },
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct GlyphKey {
    pub c: char,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphEntry {
    pub atlas_x: u32,
    pub atlas_y: u32,
    pub pixel_w: u32,
    pub pixel_h: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
}

const EMPTY_GLYPH: GlyphEntry = GlyphEntry {
    atlas_x: 0,
    atlas_y: 0,
    pixel_w: 0,
    pixel_h: 0,
    bearing_x: 0,
    bearing_y: 0,
};

struct FallbackFont {
    path: PathBuf,
    font: FontFallback,
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
    pub dirty: bool,
    font: Font,
    fallbacks: Vec<FallbackFont>,
    font_size: f32,
    scale_factor: f32,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    replacement: GlyphEntry,
}

impl GlyphAtlas {
    pub fn new(
        font_family: &str,
        font_size: f32,
        scale_factor: f32,
    ) -> Result<Self, GlyphAtlasError> {
        validate_sizing(font_size, scale_factor)?;
        let mut paths = Vec::new();
        if Path::new(font_family).is_absolute() {
            paths.push(PathBuf::from(font_family));
        } else if !font_family.is_empty() && !font_family.contains(['/', '\\']) {
            let family: String = font_family.chars().filter(|c| !c.is_whitespace()).collect();
            for directory in SYSTEM_FONT_DIRECTORIES {
                paths.push(Path::new(directory).join(format!("{family}-Regular.ttf")));
                paths.push(Path::new(directory).join(format!("{family}.ttf")));
            }
        }
        for name in [
            "RobotoMono-Regular.ttf",
            "DroidSansMono.ttf",
            "NotoSansMono-Regular.ttf",
            "Roboto-Regular.ttf",
            "RobotoStatic-Regular.ttf",
            "NotoSans-Regular.ttf",
        ] {
            paths.extend(system_paths(name));
        }
        let mut errors = Vec::new();
        for path in paths {
            match load_font(&path, font_size * scale_factor) {
                Ok(font) => match Self::with_font(font, font_size, scale_factor) {
                    Ok(atlas) => {
                        log::info!(
                            "Android font: {} size={font_size} scale={scale_factor} cell={}x{}",
                            path.display(),
                            atlas.cell_width,
                            atlas.cell_height,
                        );
                        return Ok(atlas);
                    }
                    Err(error) => errors.push(format!("{}: {error}", path.display())),
                },
                Err(error) => errors.push(format!("{}: {error}", path.display())),
            }
        }
        Err(GlyphAtlasError::FontUnavailable {
            requested_family: font_family.to_owned(),
            reason: errors.join("; "),
        })
    }

    fn with_font(font: Font, font_size: f32, scale_factor: f32) -> Result<Self, GlyphAtlasError> {
        let (cell_width, cell_height, ascent, descent) =
            scaled_metrics(&font, font_size, scale_factor)?;
        let mut atlas = Self {
            width: ATLAS_SIZE,
            height: ATLAS_SIZE,
            pixels: vec![0; (ATLAS_SIZE * ATLAS_SIZE) as usize],
            glyphs: HashMap::new(),
            cell_width,
            cell_height,
            ascent,
            descent,
            dirty: true,
            font,
            fallbacks: Vec::new(),
            font_size,
            scale_factor,
            cursor_x: 2,
            cursor_y: 0,
            row_height: 0,
            replacement: EMPTY_GLYPH,
        };
        atlas.reset_cache();
        Ok(atlas)
    }

    pub fn clear_and_resize(&mut self, new_font_size: f32) -> Result<(), GlyphAtlasError> {
        // Validate before changing any fields, so failed zoom preserves the frame.
        let (width, height, ascent, descent) =
            scaled_metrics(&self.font, new_font_size, self.scale_factor)?;
        self.font_size = new_font_size;
        self.cell_width = width;
        self.cell_height = height;
        self.ascent = ascent;
        self.descent = descent;
        self.reset_cache();
        Ok(())
    }

    fn reset_cache(&mut self) {
        self.pixels.fill(0);
        self.pixels[0] = 255;
        self.glyphs.clear();
        self.cursor_x = 2;
        self.cursor_y = 0;
        self.row_height = 0;
        self.dirty = true;
        self.replacement = EMPTY_GLYPH;
        let character = if self.font.has_glyph('\u{fffd}') {
            '\u{fffd}'
        } else {
            '?'
        };
        self.replacement = self.insert_font_glyph(
            GlyphKey {
                c: character,
                bold: false,
                italic: false,
            },
            None,
        );
        for c in ' '..='~' {
            self.get_or_insert(GlyphKey {
                c,
                bold: false,
                italic: false,
            });
        }
    }

    pub fn get_or_insert(&mut self, key: GlyphKey) -> GlyphEntry {
        if let Some(&entry) = self.glyphs.get(&key) {
            return entry;
        }
        // Terminal output can contain arbitrarily many distinct unsupported
        // codepoints. Bound metadata as well as the 1 MiB bitmap.
        if self.glyphs.len() >= MAX_CACHED_GLYPHS {
            return self.replacement;
        }
        let entry = if self.font.has_glyph(key.c) {
            self.insert_font_glyph(key, None)
        } else if let Some(index) = self.fallback_for(key.c) {
            self.insert_font_glyph(key, Some(index))
        } else {
            self.replacement
        };
        self.glyphs.insert(key, entry);
        entry
    }

    fn fallback_for(&mut self, character: char) -> Option<usize> {
        if let Some(index) = self
            .fallbacks
            .iter()
            .position(|font| font.font.has_glyph(character))
        {
            // Most recently used font is last; the primary font is never evicted.
            let font = self.fallbacks.remove(index);
            self.fallbacks.push(font);
            return Some(self.fallbacks.len() - 1);
        }
        for name in fallback_names(character) {
            for path in system_paths(name) {
                if self.fallbacks.iter().any(|font| font.path == path) || !path.is_file() {
                    continue;
                }
                // Release bytes before reading another large CJK collection.
                // Already-rasterized atlas entries remain valid after eviction.
                if self.fallbacks.len() == MAX_FALLBACK_FONTS {
                    self.fallbacks.remove(0);
                }
                let Ok(font) = read_font_bytes(&path).and_then(FontFallback::from_bytes) else {
                    continue;
                };
                let has_glyph = font.has_glyph(character);
                log::info!("Android fallback font: {}", path.display());
                self.fallbacks.push(FallbackFont { path, font });
                if has_glyph {
                    return Some(self.fallbacks.len() - 1);
                }
            }
        }
        None
    }

    fn insert_font_glyph(&mut self, key: GlyphKey, fallback: Option<usize>) -> GlyphEntry {
        let scaled_size = self.font_size * self.scale_factor;
        let metrics = match fallback {
            Some(index) => match self.fallbacks[index].font.metrics(key.c, scaled_size) {
                Some(metrics) => metrics,
                None => return self.replacement,
            },
            None => self.font.metrics(key.c, scaled_size),
        };
        if metrics.width == 0 || metrics.height == 0 {
            return EMPTY_GLYPH;
        }
        // Inspect dimensions before asking fontdue to allocate the raster.
        let (width, height) = styled_dimensions(metrics, key);
        if width >= self.width as usize || height > self.height as usize {
            return self.replacement;
        }
        if self.cursor_x + width as u32 + 1 > self.width {
            self.cursor_y += self.row_height + 1;
            self.cursor_x = 0;
            self.row_height = 0;
        }
        if self.cursor_y + height as u32 > self.height {
            return self.replacement;
        }
        let bitmap = match fallback {
            Some(index) => match self.fallbacks[index].font.rasterize(
                key.c,
                scaled_size,
                metrics,
                ATLAS_SIZE as usize,
            ) {
                Some(bitmap) => bitmap,
                None => return self.replacement,
            },
            None => self.font.rasterize(key.c, scaled_size).1,
        };
        let entry = GlyphEntry {
            atlas_x: self.cursor_x,
            atlas_y: self.cursor_y,
            pixel_w: width as u32,
            pixel_h: height as u32,
            bearing_x: metrics.xmin,
            // fontdue's ymin is bottom-up; software_raster expects top-down
            // offset relative to the baseline, including descenders.
            bearing_y: metrics
                .ymin
                .saturating_neg()
                .saturating_sub(metrics.height as i32),
        };
        for y in 0..metrics.height {
            let slant = if key.italic {
                (metrics.height - 1 - y) / 5
            } else {
                0
            };
            for x in 0..metrics.width {
                let coverage = bitmap[y * metrics.width + x];
                let destination = (self.cursor_y as usize + y) * self.width as usize
                    + self.cursor_x as usize
                    + x
                    + slant;
                self.pixels[destination] = self.pixels[destination].max(coverage);
                if key.bold {
                    self.pixels[destination + 1] = self.pixels[destination + 1].max(coverage);
                }
            }
        }
        self.cursor_x += width as u32 + 1;
        self.row_height = self.row_height.max(height as u32);
        self.dirty = true;
        entry
    }
}

fn validate_sizing(font_size: f32, scale_factor: f32) -> Result<(), GlyphAtlasError> {
    let scaled = font_size * scale_factor;
    if !font_size.is_finite()
        || font_size <= 0.0
        || !scale_factor.is_finite()
        || scale_factor <= 0.0
        || !scaled.is_finite()
        || scaled <= 0.0
        || scaled > MAX_SCALED_SIZE
    {
        return Err(GlyphAtlasError::InvalidSizing {
            font_size,
            scale_factor,
        });
    }
    Ok(())
}

fn scaled_metrics(
    font: &Font,
    size: f32,
    scale: f32,
) -> Result<(f32, f32, f32, f32), GlyphAtlasError> {
    validate_sizing(size, scale)?;
    let invalid = || GlyphAtlasError::InvalidMetrics {
        font_size: size,
        scale_factor: scale,
    };
    let line = font
        .horizontal_line_metrics(size * scale)
        .ok_or_else(invalid)?;
    let width = font.metrics('M', size * scale).advance_width.ceil();
    let height = line.new_line_size.ceil();
    if !width.is_finite()
        || width <= 0.0
        || width >= ATLAS_SIZE as f32
        || !height.is_finite()
        || height <= 0.0
        || height >= ATLAS_SIZE as f32
        || !line.ascent.is_finite()
        || !line.descent.is_finite()
    {
        return Err(invalid());
    }
    Ok((width, height, line.ascent, line.descent))
}

fn styled_dimensions(metrics: Metrics, key: GlyphKey) -> (usize, usize) {
    let slant = if key.italic {
        metrics.height.saturating_sub(1) / 5
    } else {
        0
    };
    (
        metrics
            .width
            .saturating_add(slant)
            .saturating_add(usize::from(key.bold)),
        metrics.height,
    )
}

fn read_font_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let size = file.metadata().map_err(|error| error.to_string())?.len();
    if size > MAX_FONT_BYTES {
        return Err("font file exceeds 32 MiB limit".to_owned());
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(MAX_FONT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_FONT_BYTES {
        return Err("font file exceeds 32 MiB limit".to_owned());
    }
    Ok(bytes)
}

fn load_font(path: &Path, scale: f32) -> Result<Font, String> {
    Font::from_bytes(
        read_font_bytes(path)?,
        FontSettings {
            scale,
            load_substitutions: false,
            ..FontSettings::default()
        },
    )
    .map_err(str::to_owned)
}

fn system_paths(name: &str) -> impl Iterator<Item = PathBuf> + '_ {
    SYSTEM_FONT_DIRECTORIES
        .iter()
        .map(move |directory| Path::new(directory).join(name))
}

fn fallback_names(character: char) -> &'static [&'static str] {
    // Select a script before parsing a font: loading all CJK outlines for an
    // unsupported emoji would cost substantial time and memory unnecessarily.
    match character as u32 {
        0x2e80..=0xa4cf
        | 0xac00..=0xd7af
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f
        | 0x20000..=0x323af => &[
            "NotoSansCJK-Regular.ttc",
            "NotoSansCJK-VF.ttc",
            "NotoSansCJKjp-Regular.otf",
        ],
        0x0600..=0x08ff | 0xfb50..=0xfdff | 0xfe70..=0xfeff => {
            &["NotoNaskhArabic-Regular.ttf", "NotoSansArabic-Regular.ttf"]
        }
        0x0590..=0x05ff => &["NotoSansHebrew-Regular.ttf"],
        0x0900..=0x097f => &["NotoSansDevanagari-Regular.ttf"],
        0x0980..=0x09ff => &["NotoSansBengali-Regular.ttf"],
        0x0e00..=0x0e7f => &["NotoSansThai-Regular.ttf"],
        0x0e80..=0x0eff => &["NotoSansLao-Regular.ttf"],
        0x1000..=0x109f => &["NotoSansMyanmar-Regular.ttf"],
        0x1780..=0x17ff => &["NotoSansKhmer-Regular.ttf"],
        0x2200..=0x2bff | 0x1f000..=0x1faff => &[
            "NotoSansSymbols-Regular-Subsetted.ttf",
            "NotoSansSymbols2-Regular.ttf",
        ],
        _ => &[
            "NotoSans-Regular.ttf",
            "Roboto-Regular.ttf",
            "RobotoStatic-Regular.ttf",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_font() -> Font {
        Font::from_bytes(
            &include_bytes!("../fonts/android-test.ttf")[..],
            FontSettings::default(),
        )
        .expect("original geometric TrueType fixture must parse")
    }

    fn atlas() -> GlyphAtlas {
        GlyphAtlas::with_font(fixture_font(), 20.0, 1.0).unwrap()
    }

    fn key(c: char) -> GlyphKey {
        GlyphKey {
            c,
            bold: false,
            italic: false,
        }
    }

    fn coverage(atlas: &GlyphAtlas, glyph: GlyphEntry) -> u32 {
        (0..glyph.pixel_h)
            .flat_map(|y| {
                (0..glyph.pixel_w).map(move |x| {
                    u32::from(
                        atlas.pixels
                            [((glyph.atlas_y + y) * atlas.width + glyph.atlas_x + x) as usize],
                    )
                })
            })
            .sum()
    }

    #[test]
    fn invalid_sizing_fails_before_font_discovery() {
        for (size, scale) in [
            (0.0, 1.0),
            (-1.0, 1.0),
            (f32::NAN, 1.0),
            (f32::INFINITY, 1.0),
            (14.0, 0.0),
            (14.0, -1.0),
            (14.0, f32::NAN),
            (14.0, f32::INFINITY),
            (f32::MAX, f32::MAX),
            (257.0, 1.0),
        ] {
            assert!(matches!(
                GlyphAtlas::new("missing", size, scale),
                Err(GlyphAtlasError::InvalidSizing { .. })
            ));
        }
    }

    #[test]
    fn rasterizes_a8_with_baseline_and_descender_bearings() {
        let mut atlas = atlas();
        let capital = atlas.get_or_insert(key('A'));
        let descender = atlas.get_or_insert(key('g'));
        let accent = atlas.get_or_insert(key('é'));
        assert_eq!(atlas.pixels.len(), 1024 * 1024);
        assert_eq!(atlas.pixels[0], 255);
        assert_eq!(atlas.cell_width, 12.0);
        assert_eq!(atlas.cell_height, 24.0);
        assert!(coverage(&atlas, capital) > 0);
        assert!(coverage(&atlas, descender) > 0);
        assert!(coverage(&atlas, accent) > 0);
        assert_eq!(capital.bearing_y + capital.pixel_h as i32, 0);
        assert!(descender.bearing_y + descender.pixel_h as i32 > 0);
        assert!(accent.bearing_y < capital.bearing_y);
        for entry in atlas.glyphs.values() {
            assert!(entry.atlas_x + entry.pixel_w <= atlas.width);
            assert!(entry.atlas_y + entry.pixel_h <= atlas.height);
        }
        assert_eq!(atlas.get_or_insert(key(' ')), EMPTY_GLYPH);
    }

    #[test]
    fn cff2_collection_fallback_produces_visible_hangul_outlines() {
        // Android can install a variable CFF2 collection under the filename
        // NotoSansCJK-Regular.ttc. Without ttf-parser's variable-fonts feature,
        // fontdue still finds its cmap entries but returns empty glyph outlines.
        let font =
            FontFallback::from_bytes(include_bytes!("../fonts/android-cff2-test.ttc").to_vec())
                .unwrap();
        assert!(font.has_glyph('ㄱ'));
        assert!(font.has_glyph('가'));
        let mut atlas = atlas();
        atlas.fallbacks.push(FallbackFont {
            path: PathBuf::from("android-cff2-test.ttc"),
            font,
        });
        for character in ['ㄱ', '가'] {
            let glyph = atlas.get_or_insert(key(character));
            assert!(glyph.pixel_w > 0 && glyph.pixel_h > 0);
            assert!(coverage(&atlas, glyph) > 0);
            assert_ne!(glyph, atlas.replacement);
        }
        assert_eq!(atlas.get_or_insert(key('\u{3000}')), EMPTY_GLYPH);
        assert_eq!((atlas.cell_width, atlas.cell_height), (12.0, 24.0));
    }

    #[test]
    fn repeats_are_cached_and_unknown_characters_remain_visible() {
        let mut atlas = atlas();
        let expected = atlas.get_or_insert(key('A'));
        let count = atlas.glyphs.len();
        atlas.dirty = false;
        assert_eq!(atlas.get_or_insert(key('A')), expected);
        assert_eq!(atlas.glyphs.len(), count);
        assert!(!atlas.dirty);
        let unknown = atlas.get_or_insert(key('\u{10ffff}'));
        assert_eq!(unknown, atlas.replacement);
        assert!(coverage(&atlas, unknown) > 0);
    }

    #[test]
    fn bold_and_italic_change_raster_without_changing_cell_advance() {
        let mut atlas = atlas();
        let normal = atlas.get_or_insert(key('A'));
        let bold = atlas.get_or_insert(GlyphKey {
            bold: true,
            ..key('A')
        });
        let italic = atlas.get_or_insert(GlyphKey {
            italic: true,
            ..key('A')
        });
        assert_eq!(bold.pixel_w, normal.pixel_w + 1);
        assert!(italic.pixel_w > normal.pixel_w);
        assert!(coverage(&atlas, bold) > coverage(&atlas, normal));
        assert_eq!(coverage(&atlas, italic), coverage(&atlas, normal));
        assert_eq!(atlas.cell_width, 12.0);
    }

    #[test]
    fn invalid_zoom_preserves_cache_and_valid_zoom_rebuilds_it() {
        let mut atlas = atlas();
        let expected = atlas.get_or_insert(key('A'));
        let count = atlas.glyphs.len();
        atlas.dirty = false;
        assert!(atlas.clear_and_resize(f32::NAN).is_err());
        assert_eq!(atlas.get_or_insert(key('A')), expected);
        assert_eq!(atlas.glyphs.len(), count);
        assert!(!atlas.dirty);
        atlas.clear_and_resize(40.0).unwrap();
        let bigger = atlas.get_or_insert(key('A'));
        assert!(bigger.pixel_w > expected.pixel_w);
        assert!(atlas.dirty);
        assert_eq!(atlas.cell_width, 24.0);
        assert_eq!(atlas.pixels[0], 255);
    }

    #[test]
    fn exhaustion_keeps_old_entries_valid_and_bounds_metadata() {
        let mut atlas = atlas();
        let expected = atlas.get_or_insert(key('A'));
        atlas.cursor_y = atlas.height;
        let overflow = atlas.get_or_insert(GlyphKey {
            bold: true,
            ..key('A')
        });
        assert_eq!(overflow, atlas.replacement);
        assert_eq!(atlas.get_or_insert(key('A')), expected);
        // Use known keys without rasterization to exercise the public cache cap.
        while atlas.glyphs.len() < MAX_CACHED_GLYPHS {
            let c = char::from_u32(0x10000 + atlas.glyphs.len() as u32).unwrap();
            atlas.glyphs.insert(key(c), EMPTY_GLYPH);
        }
        assert_eq!(atlas.get_or_insert(key('\u{10ffff}')), atlas.replacement);
        assert_eq!(atlas.glyphs.len(), MAX_CACHED_GLYPHS);
        assert_eq!(atlas.pixels.len(), 1024 * 1024);
    }

    #[test]
    fn cjk_and_symbols_choose_their_own_system_fallbacks() {
        assert!(fallback_names('中').contains(&"NotoSansCJK-Regular.ttc"));
        assert!(fallback_names('あ').contains(&"NotoSansCJK-Regular.ttc"));
        assert!(fallback_names('한').contains(&"NotoSansCJK-Regular.ttc"));
        assert!(!fallback_names('😀').contains(&"NotoSansCJK-Regular.ttc"));
    }
}
