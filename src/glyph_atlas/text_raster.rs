//! Shape complete terminal graphemes before rasterizing any glyphs.
//!
//! font-kit's character lookup is not a shaper, and its fallback methods are
//! stubs on macOS/Linux. Keep the original collection index for rustybuzz and
//! explicitly search installed fallback fonts when the primary face cannot
//! represent the complete cluster.

use font_kit::canvas::{Canvas, Format, RasterizationOptions};
use font_kit::error::FontLoadingError;
use font_kit::family_name::FamilyName;
use font_kit::font::Font;
use font_kit::handle::Handle;
use font_kit::hinting::HintingOptions;
use font_kit::properties::Properties;
use font_kit::source::SystemSource;
use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::{Vector2F, Vector2I};
use rustybuzz::ttf_parser::{GlyphId, RasterGlyphImage, RasterImageFormat};
use std::collections::HashSet;
use std::io::Cursor;
use std::sync::Arc;

const MAX_BITMAP_SIDE: u32 = 1024;

pub(super) struct ShapingFont {
    pub font: Font,
    data: Arc<Vec<u8>>,
    index: u32,
}

impl ShapingFont {
    pub fn load(handle: &Handle) -> Result<Self, FontLoadingError> {
        let (data, index) = match handle {
            Handle::Path { path, font_index } => (
                Arc::new(std::fs::read(path).map_err(FontLoadingError::Io)?),
                *font_index,
            ),
            Handle::Memory { bytes, font_index } => (bytes.clone(), *font_index),
        };
        let font = Font::from_bytes(data.clone(), index)?;
        Ok(Self { font, data, index })
    }

    fn shape(&self, text: &str) -> Option<(rustybuzz::Face<'_>, rustybuzz::GlyphBuffer)> {
        let face = rustybuzz::Face::from_slice(&self.data, self.index)?;
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.set_flags(rustybuzz::BufferFlags::REMOVE_DEFAULT_IGNORABLES);
        buffer.guess_segment_properties();
        let shaped = rustybuzz::shape(&face, &[], buffer);
        if shaped.glyph_infos().iter().any(|info| info.glyph_id == 0) {
            return None;
        }
        Some((face, shaped))
    }

    fn rasterize(&self, text: &str, size: f32, max_width: f32) -> Option<Bitmap> {
        let (face, shaped) = self.shape(text)?;
        let advance: i32 = shaped.glyph_positions().iter().map(|p| p.x_advance).sum();
        let mut scale = size / face.units_per_em() as f32;
        if advance > 0 && advance as f32 * scale > max_width {
            scale = max_width / advance as f32;
        }
        let size = scale * face.units_per_em() as f32;
        let mut pen = Vector2F::zero();
        let mut glyphs = Vec::new();
        for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            let offset = pen
                + Vector2F::new(
                    position.x_offset as f32 * scale,
                    -position.y_offset as f32 * scale,
                );
            let glyph = if let Some(image) = face.glyph_raster_image(
                GlyphId(u16::try_from(info.glyph_id).ok()?),
                size.ceil().clamp(1.0, u16::MAX as f32) as u16,
            ) {
                rasterize_bitmap(image, size, offset)?
            } else {
                rasterize_outline(&self.font, info.glyph_id, size, offset)?
            };
            glyphs.push(glyph);
            pen += Vector2F::new(
                position.x_advance as f32 * scale,
                -position.y_advance as f32 * scale,
            );
        }
        Bitmap::compose(glyphs)
    }
}

pub(super) struct TextRasterizer {
    primary: ShapingFont,
    fallbacks: Vec<ShapingFont>,
    preferred_loaded: bool,
    remaining: Option<Vec<Handle>>,
    loaded_names: HashSet<String>,
}

impl TextRasterizer {
    pub fn new(primary: ShapingFont) -> Self {
        let loaded_names = HashSet::from([primary.font.full_name()]);
        Self {
            primary,
            fallbacks: Vec::new(),
            preferred_loaded: false,
            remaining: None,
            loaded_names,
        }
    }

    pub fn rasterize(&mut self, text: &str, size: f32, max_width: f32) -> Option<Bitmap> {
        let emoji = prefers_emoji(text);
        if !emoji {
            if let Some(bitmap) = self.primary.rasterize(text, size, max_width) {
                return Some(bitmap);
            }
        }

        self.load_preferred_fallbacks();
        for font in &self.fallbacks {
            if let Some(bitmap) = font.rasterize(text, size, max_width) {
                return Some(bitmap);
            }
        }
        if emoji {
            if let Some(bitmap) = self.primary.rasterize(text, size, max_width) {
                return Some(bitmap);
            }
        }

        // Load additional installed fonts only until one covers this grapheme.
        // Retain only fonts that actually render text. A missing code point must
        // not keep every installed font file in memory. The atlas caches misses
        // too, so this discovery work is never repeated during frame rendering.
        let remaining = self
            .remaining
            .get_or_insert_with(|| SystemSource::new().all_fonts().unwrap_or_default());
        for handle in remaining.iter() {
            let Ok(font) = ShapingFont::load(handle) else {
                continue;
            };
            let name = font.font.full_name();
            if self.loaded_names.contains(&name) {
                continue;
            }
            let bitmap = font.rasterize(text, size, max_width);
            if bitmap.is_some() {
                self.loaded_names.insert(name);
                self.fallbacks.push(font);
                return bitmap;
            }
        }

        // An unavailable font must leave a visible replacement, not erase text.
        self.primary
            .rasterize("\u{fffd}", size, max_width)
            .or_else(|| self.primary.rasterize("?", size, max_width))
    }

    fn load_preferred_fallbacks(&mut self) {
        if self.preferred_loaded {
            return;
        }
        self.preferred_loaded = true;
        let source = SystemSource::new();
        for family in [
            "Apple Color Emoji",
            "Noto Color Emoji",
            "Noto Emoji",
            "Segoe UI Emoji",
            "Noto Sans CJK SC",
            "Noto Sans Symbols 2",
            "Arial Unicode MS",
        ] {
            let Ok(handle) = source
                .select_best_match(&[FamilyName::Title(family.to_owned())], &Properties::new())
            else {
                continue;
            };
            let Ok(font) = ShapingFont::load(&handle) else {
                continue;
            };
            if self.loaded_names.insert(font.font.full_name()) {
                self.fallbacks.push(font);
            }
        }
    }
}

fn prefers_emoji(text: &str) -> bool {
    !text.contains('\u{fe0e}')
        && text
            .chars()
            .any(|c| matches!(c, '\u{fe0f}' | '\u{20e3}' | '\u{1f000}'..='\u{1faff}'))
}

pub(super) struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub rgba: Vec<u8>,
    pub color: bool,
}

impl Bitmap {
    fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            x: 0,
            y: 0,
            rgba: Vec::new(),
            color: false,
        }
    }

    fn compose(glyphs: Vec<Self>) -> Option<Self> {
        let visible: Vec<_> = glyphs
            .into_iter()
            .filter(|g| g.width > 0 && g.height > 0)
            .collect();
        if visible.is_empty() {
            return Some(Self::empty());
        }
        let left = visible.iter().map(|g| g.x).min()?;
        let top = visible.iter().map(|g| g.y).min()?;
        let right = visible.iter().map(|g| g.x + g.width as i32).max()?;
        let bottom = visible.iter().map(|g| g.y + g.height as i32).max()?;
        let width = u32::try_from(right - left).ok()?;
        let height = u32::try_from(bottom - top).ok()?;
        if width > MAX_BITMAP_SIDE || height > MAX_BITMAP_SIDE {
            return None;
        }
        let mut output = Self {
            width,
            height,
            x: left,
            y: top,
            rgba: vec![0; (width * height * 4) as usize],
            color: visible.iter().any(|g| g.color),
        };
        for glyph in visible {
            for y in 0..glyph.height {
                for x in 0..glyph.width {
                    let src = ((y * glyph.width + x) * 4) as usize;
                    let dst_x = (glyph.x - left) as u32 + x;
                    let dst_y = (glyph.y - top) as u32 + y;
                    let dst = ((dst_y * width + dst_x) * 4) as usize;
                    over(&mut output.rgba[dst..dst + 4], &glyph.rgba[src..src + 4]);
                }
            }
        }
        Some(output)
    }
}

fn over(dst: &mut [u8], src: &[u8]) {
    let source_alpha = u32::from(src[3]);
    let behind = u32::from(dst[3]) * (255 - source_alpha);
    let alpha = source_alpha * 255 + behind;
    if alpha == 0 {
        return;
    }
    for channel in 0..3 {
        dst[channel] = ((u32::from(src[channel]) * source_alpha * 255
            + u32::from(dst[channel]) * behind
            + alpha / 2)
            / alpha) as u8;
    }
    dst[3] = ((alpha + 127) / 255) as u8;
}

fn rasterize_outline(font: &Font, glyph_id: u32, size: f32, offset: Vector2F) -> Option<Bitmap> {
    let transform = Transform2F::from_translation(offset);
    let bounds = font
        .raster_bounds(
            glyph_id,
            size,
            transform,
            HintingOptions::None,
            RasterizationOptions::GrayscaleAa,
        )
        .ok()?;
    let width = u32::try_from(bounds.width()).ok()?;
    let height = u32::try_from(bounds.height()).ok()?;
    if width == 0 || height == 0 {
        return Some(Bitmap::empty());
    }
    if width > MAX_BITMAP_SIDE || height > MAX_BITMAP_SIDE {
        return None;
    }
    let mut canvas = Canvas::new(Vector2I::new(width as i32, height as i32), Format::A8);
    let origin = offset - bounds.origin().to_f32();
    font.rasterize_glyph(
        &mut canvas,
        glyph_id,
        size,
        Transform2F::from_translation(origin),
        HintingOptions::None,
        RasterizationOptions::GrayscaleAa,
    )
    .ok()?;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in canvas.pixels.chunks(canvas.stride).take(height as usize) {
        for &alpha in &row[..width as usize] {
            rgba.extend_from_slice(&[255, 255, 255, alpha]);
        }
    }
    Some(Bitmap {
        width,
        height,
        x: bounds.origin_x(),
        y: bounds.origin_y(),
        rgba,
        color: false,
    })
}

fn rasterize_bitmap(image: RasterGlyphImage<'_>, size: f32, offset: Vector2F) -> Option<Bitmap> {
    if image.format != RasterImageFormat::PNG || image.pixels_per_em == 0 {
        return None;
    }
    let mut decoder = png::Decoder::new(Cursor::new(image.data));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.set_limits(png::Limits {
        bytes: 16 * 1024 * 1024,
    });
    let mut reader = decoder.read_info().ok()?;
    if reader.info().width > MAX_BITMAP_SIDE || reader.info().height > MAX_BITMAP_SIDE {
        return None;
    }
    let mut decoded = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut decoded).ok()?;
    if info.width == 0 || info.height == 0 {
        return None;
    }
    let channels = info.color_type.samples();
    let mut source = Vec::with_capacity((info.width * info.height * 4) as usize);
    for pixel in decoded[..info.buffer_size()].chunks_exact(channels) {
        let rgba = match info.color_type {
            png::ColorType::Rgba => [pixel[0], pixel[1], pixel[2], pixel[3]],
            png::ColorType::Rgb => [pixel[0], pixel[1], pixel[2], 255],
            png::ColorType::GrayscaleAlpha => [pixel[0], pixel[0], pixel[0], pixel[1]],
            png::ColorType::Grayscale => [pixel[0], pixel[0], pixel[0], 255],
            png::ColorType::Indexed => return None,
        };
        source.extend_from_slice(&rgba);
    }
    let scale = size / f32::from(image.pixels_per_em);
    let width = (info.width as f32 * scale).ceil().max(1.0) as u32;
    let height = (info.height as f32 * scale).ceil().max(1.0) as u32;
    if width > MAX_BITMAP_SIDE || height > MAX_BITMAP_SIDE {
        return None;
    }
    let mut rgba = vec![0; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let sx = (x * info.width / width).min(info.width - 1);
            let sy = (y * info.height / height).min(info.height - 1);
            let src = ((sy * info.width + sx) * 4) as usize;
            let dst = ((y * width + x) * 4) as usize;
            rgba[dst..dst + 4].copy_from_slice(&source[src..src + 4]);
        }
    }
    Some(Bitmap {
        width,
        height,
        x: (offset.x() + f32::from(image.x) * scale).floor() as i32,
        y: (offset.y() - (f32::from(image.y) + info.height as f32) * scale).floor() as i32,
        rgba,
        color: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combining_cluster_is_shaped_as_a_whole_instead_of_scalar_overlays() {
        let handle = SystemSource::new()
            .select_best_match(&[FamilyName::Monospace], &Properties::new())
            .unwrap();
        let font = ShapingFont::load(&handle).unwrap();
        let (_, plain) = font.shape("e").unwrap();
        let (_, decomposed) = font.shape("e\u{301}").unwrap();
        let (_, composed) = font.shape("é").unwrap();
        let ids = |buffer: &rustybuzz::GlyphBuffer| {
            buffer
                .glyph_infos()
                .iter()
                .map(|g| g.glyph_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&decomposed), ids(&composed));
        assert_ne!(ids(&plain), ids(&decomposed));
        assert!(decomposed.glyph_infos().iter().all(|g| g.cluster == 0));
    }

    #[test]
    fn system_emoji_font_shapes_zwj_flags_modifiers_and_keycaps_into_single_glyphs() {
        #[cfg(target_os = "macos")]
        let family = "Apple Color Emoji";
        #[cfg(target_os = "linux")]
        let family = "Noto Color Emoji";
        let handle = match SystemSource::new()
            .select_best_match(&[FamilyName::Title(family.to_owned())], &Properties::new())
        {
            Ok(handle) => handle,
            Err(_) => {
                eprintln!("{family} is unavailable; install it to exercise bitmap emoji shaping");
                return;
            }
        };
        let font = ShapingFont::load(&handle).unwrap();
        for text in [
            "👩🏽\u{200d}💻",
            "🇧🇷",
            "1\u{fe0f}\u{20e3}",
            "👨\u{200d}👩\u{200d}👧\u{200d}👦",
        ] {
            let (_, shaped) = font.shape(text).unwrap();
            assert_eq!(shaped.len(), 1, "emoji cluster was split: {text}");
            let image = font.rasterize(text, 18.0, 32.0).unwrap();
            assert!(image.color);
            assert!(image.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0));
        }
    }
}
