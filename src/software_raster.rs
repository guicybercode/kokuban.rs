use crate::glyph_atlas::GlyphEntry;

const RGB_MASK: u32 = 0x00ff_ffff;

/// Blend an A8 glyph into a softbuffer frame (`0x00RRGGBB`).
///
/// `destination` is the top-left pixel of the glyph bitmap. Bearings are kept
/// in [`GlyphEntry`] for the caller to apply while laying out the glyph.
pub(crate) fn draw_glyph_a8(
    frame: &mut [u32],
    frame_size: (u32, u32),
    atlas: &[u8],
    atlas_size: (u32, u32),
    glyph: GlyphEntry,
    destination: (i32, i32),
    rgb: u32,
) {
    if !buffer_contains_surface(frame.len(), frame_size)
        || !buffer_contains_surface(atlas.len(), atlas_size)
        || !glyph_fits_atlas(glyph, atlas_size)
    {
        return;
    }

    let Some(clipped) = clip_rect(destination, (glyph.pixel_w, glyph.pixel_h), frame_size) else {
        return;
    };
    let rgb = rgb & RGB_MASK;

    for row in 0..clipped.height {
        let Some(source_y) = glyph
            .atlas_y
            .checked_add(clipped.source_y)
            .and_then(|y| y.checked_add(row))
        else {
            return;
        };
        let Some(destination_y) = clipped.destination_y.checked_add(row) else {
            return;
        };

        for column in 0..clipped.width {
            let Some(source_x) = glyph
                .atlas_x
                .checked_add(clipped.source_x)
                .and_then(|x| x.checked_add(column))
            else {
                return;
            };
            let Some(destination_x) = clipped.destination_x.checked_add(column) else {
                return;
            };
            let Some(source_index) = pixel_index(atlas_size.0, source_x, source_y) else {
                return;
            };
            let Some(destination_index) = pixel_index(frame_size.0, destination_x, destination_y)
            else {
                return;
            };
            let Some(&coverage) = atlas.get(source_index) else {
                return;
            };

            let Some(destination_pixel) = frame.get_mut(destination_index) else {
                return;
            };
            *destination_pixel = blend_rgb(*destination_pixel, rgb, coverage);
        }
    }
}

/// Fill a clipped rectangle in a softbuffer frame (`0x00RRGGBB`).
pub(crate) fn fill_rect(
    frame: &mut [u32],
    frame_size: (u32, u32),
    origin: (i32, i32),
    size: (u32, u32),
    rgb: u32,
    alpha: u8,
) {
    if alpha == 0 || !buffer_contains_surface(frame.len(), frame_size) {
        return;
    }

    let Some(clipped) = clip_rect(origin, size, frame_size) else {
        return;
    };
    let rgb = rgb & RGB_MASK;

    for row in 0..clipped.height {
        let Some(y) = clipped.destination_y.checked_add(row) else {
            return;
        };
        for column in 0..clipped.width {
            let Some(x) = clipped.destination_x.checked_add(column) else {
                return;
            };
            let Some(index) = pixel_index(frame_size.0, x, y) else {
                return;
            };
            let Some(pixel) = frame.get_mut(index) else {
                return;
            };
            *pixel = blend_rgb(*pixel, rgb, alpha);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClippedRect {
    destination_x: u32,
    destination_y: u32,
    source_x: u32,
    source_y: u32,
    width: u32,
    height: u32,
}

fn buffer_contains_surface(buffer_len: usize, size: (u32, u32)) -> bool {
    let required_len = u64::from(size.0) * u64::from(size.1);
    usize::try_from(required_len).is_ok_and(|required_len| required_len <= buffer_len)
}

fn glyph_fits_atlas(glyph: GlyphEntry, atlas_size: (u32, u32)) -> bool {
    glyph
        .atlas_x
        .checked_add(glyph.pixel_w)
        .is_some_and(|right| right <= atlas_size.0)
        && glyph
            .atlas_y
            .checked_add(glyph.pixel_h)
            .is_some_and(|bottom| bottom <= atlas_size.1)
}

fn clip_rect(
    origin: (i32, i32),
    size: (u32, u32),
    surface_size: (u32, u32),
) -> Option<ClippedRect> {
    let left = i64::from(origin.0);
    let top = i64::from(origin.1);
    let right = left + i64::from(size.0);
    let bottom = top + i64::from(size.1);
    let clipped_left = left.max(0);
    let clipped_top = top.max(0);
    let clipped_right = right.min(i64::from(surface_size.0));
    let clipped_bottom = bottom.min(i64::from(surface_size.1));

    if clipped_left >= clipped_right || clipped_top >= clipped_bottom {
        return None;
    }

    Some(ClippedRect {
        destination_x: u32::try_from(clipped_left).ok()?,
        destination_y: u32::try_from(clipped_top).ok()?,
        source_x: u32::try_from(clipped_left - left).ok()?,
        source_y: u32::try_from(clipped_top - top).ok()?,
        width: u32::try_from(clipped_right - clipped_left).ok()?,
        height: u32::try_from(clipped_bottom - clipped_top).ok()?,
    })
}

fn pixel_index(row_width: u32, x: u32, y: u32) -> Option<usize> {
    let index = u64::from(y)
        .checked_mul(u64::from(row_width))?
        .checked_add(u64::from(x))?;
    usize::try_from(index).ok()
}

fn blend_rgb(destination: u32, foreground: u32, coverage: u8) -> u32 {
    let coverage = u32::from(coverage);
    let inverse = 255 - coverage;

    let blend_channel = |shift: u32| {
        let destination = (destination >> shift) & 0xff_u32;
        let foreground = (foreground >> shift) & 0xff_u32;
        (foreground * coverage + destination * inverse + 127) / 255
    };

    (blend_channel(16) << 16) | (blend_channel(8) << 8) | blend_channel(0)
}

#[cfg(test)]
mod tests {
    use super::{draw_glyph_a8, fill_rect};
    use crate::glyph_atlas::GlyphEntry;

    const BACKGROUND: u32 = 0x0010_2030;
    const FOREGROUND: u32 = 0xffe0_4020;

    fn glyph(atlas_x: u32, atlas_y: u32, pixel_w: u32, pixel_h: u32) -> GlyphEntry {
        GlyphEntry {
            atlas_x,
            atlas_y,
            pixel_w,
            pixel_h,
            bearing_x: 0,
            bearing_y: 0,
        }
    }

    #[test]
    fn blends_zero_half_and_full_a8_coverage() {
        let mut frame = vec![0xaa10_2030, 0xbb10_2030, 0xcc10_2030];
        let atlas = [0, 128, 255];

        draw_glyph_a8(
            &mut frame,
            (3, 1),
            &atlas,
            (3, 1),
            glyph(0, 0, 3, 1),
            (0, 0),
            FOREGROUND,
        );

        assert_eq!(frame, [BACKGROUND, 0x0078_3028, 0x00e0_4020]);
        assert!(frame.iter().all(|pixel| pixel >> 24 == 0));
    }

    #[test]
    fn clips_glyphs_on_all_four_edges_and_preserves_tail_sentinels() {
        let mut frame = vec![BACKGROUND; 11];
        frame[9] = 0xdead_beef;
        frame[10] = 0xcafe_babe;
        let mut atlas = vec![0; 25];
        for y in 1..=3 {
            for x in 1..=3 {
                atlas[y * 5 + x] = 255;
            }
        }

        draw_glyph_a8(
            &mut frame,
            (3, 3),
            &atlas,
            (5, 5),
            glyph(0, 0, 5, 5),
            (-1, -1),
            FOREGROUND,
        );

        assert_eq!(&frame[..9], &[0x00e0_4020; 9]);
        assert_eq!(&frame[9..], &[0xdead_beef, 0xcafe_babe]);
    }

    #[test]
    fn uses_the_glyph_subrectangle_within_the_atlas() {
        let atlas = [0, 0, 0, 0, 255, 128];
        let mut frame = vec![BACKGROUND; 2];

        draw_glyph_a8(
            &mut frame,
            (2, 1),
            &atlas,
            (3, 2),
            glyph(1, 1, 2, 1),
            (0, 0),
            FOREGROUND,
        );

        assert_eq!(frame, [0x00e0_4020, 0x0078_3028]);
    }

    #[test]
    fn rejects_malformed_frames_and_atlases_without_partial_writes() {
        let entry = glyph(0, 0, 2, 2);
        let atlas = [255; 4];

        let mut truncated_frame = vec![BACKGROUND; 3];
        draw_glyph_a8(
            &mut truncated_frame,
            (2, 2),
            &atlas,
            (2, 2),
            entry,
            (0, 0),
            FOREGROUND,
        );
        assert_eq!(truncated_frame, [BACKGROUND; 3]);
        fill_rect(
            &mut truncated_frame,
            (2, 2),
            (0, 0),
            (2, 2),
            FOREGROUND,
            255,
        );
        assert_eq!(truncated_frame, [BACKGROUND; 3]);

        let mut frame = vec![BACKGROUND; 4];
        draw_glyph_a8(
            &mut frame,
            (2, 2),
            &atlas[..3],
            (2, 2),
            entry,
            (0, 0),
            FOREGROUND,
        );
        assert_eq!(frame, [BACKGROUND; 4]);

        draw_glyph_a8(
            &mut frame,
            (2, 2),
            &atlas,
            (2, 2),
            glyph(u32::MAX, 0, 2, 1),
            (0, 0),
            FOREGROUND,
        );
        assert_eq!(frame, [BACKGROUND; 4]);
    }

    #[test]
    fn rejects_impossible_declared_dimensions() {
        let atlas = [255];
        let mut frame = vec![BACKGROUND];

        draw_glyph_a8(
            &mut frame,
            (u32::MAX, u32::MAX),
            &atlas,
            (1, 1),
            glyph(0, 0, 1, 1),
            (0, 0),
            FOREGROUND,
        );
        assert_eq!(frame, [BACKGROUND]);

        draw_glyph_a8(
            &mut frame,
            (0, 1),
            &atlas,
            (1, 1),
            glyph(0, 0, 1, 1),
            (0, 0),
            FOREGROUND,
        );
        fill_rect(&mut frame, (1, 0), (0, 0), (1, 1), FOREGROUND, 255);
        assert_eq!(frame, [BACKGROUND]);

        draw_glyph_a8(
            &mut frame,
            (1, 1),
            &atlas,
            (u32::MAX, u32::MAX),
            glyph(0, 0, 1, 1),
            (0, 0),
            FOREGROUND,
        );
        assert_eq!(frame, [BACKGROUND]);
    }

    #[test]
    fn fills_clipped_rectangles_with_alpha_and_preserves_tail_sentinels() {
        let mut frame = vec![BACKGROUND; 8];
        frame[6] = 0xdead_beef;
        frame[7] = 0xcafe_babe;

        fill_rect(&mut frame, (3, 2), (-1, -1), (5, 4), FOREGROUND, 128);

        assert_eq!(&frame[..6], &[0x0078_3028; 6]);
        assert_eq!(&frame[6..], &[0xdead_beef, 0xcafe_babe]);
        assert!(frame[..6].iter().all(|pixel| pixel >> 24 == 0));
    }

    #[test]
    fn supports_opaque_and_transparent_rectangles() {
        let mut frame = vec![BACKGROUND; 2];

        fill_rect(&mut frame, (2, 1), (0, 0), (1, 1), FOREGROUND, 255);
        fill_rect(&mut frame, (2, 1), (1, 0), (1, 1), FOREGROUND, 0);

        assert_eq!(frame, [0x00e0_4020, BACKGROUND]);
    }

    #[test]
    fn clips_extreme_rectangle_coordinates_without_overflow() {
        let mut frame = vec![BACKGROUND; 4];

        fill_rect(
            &mut frame,
            (2, 2),
            (i32::MIN, i32::MIN),
            (u32::MAX, u32::MAX),
            FOREGROUND,
            255,
        );
        assert_eq!(frame, [0x00e0_4020; 4]);

        fill_rect(
            &mut frame,
            (2, 2),
            (i32::MAX, i32::MAX),
            (u32::MAX, u32::MAX),
            BACKGROUND,
            255,
        );
        assert_eq!(frame, [0x00e0_4020; 4]);
    }
}
