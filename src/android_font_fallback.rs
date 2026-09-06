//! Retain font bytes, then outline and rasterize only the requested fallback glyph.
use ab_glyph_rasterizer::{point, Point, Rasterizer};
use fontdue::Metrics;
use ttf_parser::{Face, OutlineBuilder};

pub(super) struct FontFallback {
    bytes: Box<[u8]>,
}

impl FontFallback {
    pub(super) fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        Face::parse(&bytes, 0).map_err(|error| error.to_string())?;
        Ok(Self {
            bytes: bytes.into_boxed_slice(),
        })
    }

    pub(super) fn has_glyph(&self, character: char) -> bool {
        Face::parse(&self.bytes, 0)
            .ok()
            .and_then(|face| face.glyph_index(character))
            .is_some_and(|id| id.0 != 0)
    }

    pub(super) fn metrics(&self, character: char, size: f32) -> Option<Metrics> {
        let face = Face::parse(&self.bytes, 0).ok()?;
        let glyph = face.glyph_index(character)?;
        let scale = size / f32::from(face.units_per_em());
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let advance_width = f32::from(face.glyph_hor_advance(glyph).unwrap_or(0)) * scale;
        // A mapped space has advance but deliberately no outline.
        let Some(bounds) = face.glyph_bounding_box(glyph) else {
            return Some(Metrics {
                advance_width,
                ..Metrics::default()
            });
        };
        let left = (f32::from(bounds.x_min) * scale).floor();
        let bottom = (f32::from(bounds.y_min) * scale).floor();
        let right = (f32::from(bounds.x_max) * scale).ceil();
        let top = (f32::from(bounds.y_max) * scale).ceil();
        if ![left, bottom, right, top].iter().all(|v| v.is_finite()) {
            return None;
        }
        Some(Metrics {
            xmin: left as i32,
            ymin: bottom as i32,
            width: (right - left).max(0.0) as usize,
            height: (top - bottom).max(0.0) as usize,
            advance_width,
            ..Metrics::default()
        })
    }

    pub(super) fn rasterize(
        &self,
        character: char,
        size: f32,
        metrics: Metrics,
        max_dimension: usize,
    ) -> Option<Vec<u8>> {
        // Recheck at the allocator boundary, even though the atlas has already
        // checked styled dimensions and available shelf space.
        if metrics.width == 0
            || metrics.height == 0
            || metrics.width > max_dimension
            || metrics.height > max_dimension
        {
            return None;
        }
        let len = metrics.width.checked_mul(metrics.height)?;
        len.checked_add(4)?;
        let face = Face::parse(&self.bytes, 0).ok()?;
        let glyph = face.glyph_index(character)?;
        let scale = size / f32::from(face.units_per_em());
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let mut outline = Outline {
            raster: Rasterizer::new(metrics.width, metrics.height),
            scale,
            left: metrics.xmin as f32,
            top: metrics.ymin as f32 + metrics.height as f32,
            start: None,
            current: point(0.0, 0.0),
            invalid: false,
        };
        face.outline_glyph(glyph, &mut outline)?;
        outline.close();
        if outline.invalid {
            return None;
        }
        let mut pixels = vec![0; len];
        outline.raster.for_each_pixel(|index, coverage| {
            pixels[index] = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
        });
        Some(pixels)
    }
}

struct Outline {
    raster: Rasterizer,
    scale: f32,
    left: f32,
    top: f32,
    start: Option<Point>,
    current: Point,
    invalid: bool,
}

impl Outline {
    fn position(&mut self, x: f32, y: f32) -> Point {
        let position = point(x * self.scale - self.left, self.top - y * self.scale);
        if position.x.is_finite() && position.y.is_finite() {
            position
        } else {
            self.invalid = true;
            point(0.0, 0.0)
        }
    }
}

impl OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.current = self.position(x, y);
        self.start = Some(self.current);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let next = self.position(x, y);
        self.raster.draw_line(self.current, next);
        self.current = next;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = self.position(x1, y1);
        let next = self.position(x, y);
        self.raster.draw_quad(self.current, control, next);
        self.current = next;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let first = self.position(x1, y1);
        let second = self.position(x2, y2);
        let next = self.position(x, y);
        self.raster.draw_cubic(self.current, first, second, next);
        self.current = next;
    }

    fn close(&mut self) {
        if let Some(start) = self.start.take() {
            self.raster.draw_line(self.current, start);
            self.current = start;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn true_type_rectangles_preserve_baseline_and_quadratic_curves_have_coverage() {
        let font =
            FontFallback::from_bytes(include_bytes!("../fonts/android-test.ttf").to_vec()).unwrap();
        let capital = font.metrics('A', 20.0).unwrap();
        let pixels = font.rasterize('A', 20.0, capital, 1024).unwrap();
        assert_eq!(
            (capital.xmin, capital.ymin, capital.width, capital.height),
            (1, 0, 9, 14)
        );
        assert!(pixels.iter().all(|&pixel| pixel == 255));
        let descender = font.metrics('g', 20.0).unwrap();
        assert_eq!(descender.ymin, -4);
        let curve = font.metrics('Ω', 20.0).unwrap();
        let pixels = font.rasterize('Ω', 20.0, curve, 1024).unwrap();
        assert!(pixels.iter().any(|&pixel| pixel > 0));
        assert!(pixels.iter().any(|&pixel| pixel == 0));
    }

    #[test]
    fn cff2_cubic_outlines_are_visible_but_ideographic_space_stays_empty() {
        let font =
            FontFallback::from_bytes(include_bytes!("../fonts/android-cff2-test.ttc").to_vec())
                .unwrap();
        let metrics = font.metrics('가', 20.0).unwrap();
        assert!(metrics.ymin < 0);
        let pixels = font.rasterize('가', 20.0, metrics, 1024).unwrap();
        assert!(pixels.iter().any(|&pixel| pixel > 0));
        assert!(pixels.iter().any(|&pixel| pixel == 0));
        let space = font.metrics('\u{3000}', 20.0).unwrap();
        assert_eq!(
            (space.width, space.height, space.advance_width),
            (0, 0, 20.0)
        );
    }

    #[test]
    fn invalid_scale_and_oversized_rasters_are_rejected_before_allocation() {
        let font =
            FontFallback::from_bytes(include_bytes!("../fonts/android-test.ttf").to_vec()).unwrap();
        for size in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(font.metrics('A', size).is_none());
        }
        let metrics = font.metrics('A', 20.0).unwrap();
        assert!(font.rasterize('A', f32::NAN, metrics, 1024).is_none());
        assert!(font
            .rasterize(
                'A',
                20.0,
                Metrics {
                    width: 1025,
                    ..metrics
                },
                1024
            )
            .is_none());
        assert!(font
            .rasterize(
                'A',
                20.0,
                Metrics {
                    height: usize::MAX,
                    ..metrics
                },
                1024
            )
            .is_none());
    }
}
