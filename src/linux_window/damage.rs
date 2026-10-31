//! Damage tracking between presented frames: remembers the scene behind each
//! swapchain buffer and computes the vertical band that must be repainted.

use super::{cell_content_is_visible, rounded_f64_i32, GridSnapshot};
use crate::glyph_atlas::GlyphAtlas;
use crate::grid::cell::CellFlags;

// Softbuffer may rotate multiple buffers; snapshots describe their last pixels.
// Older/unknown buffers safely use a full repaint instead of growing the cache.
pub(super) const MAX_PRESENTED_SCENES: usize = 4;

pub(super) struct PresentedScene {
    pub(super) snapshot: GridSnapshot,
    pub(super) frame_size: (u32, u32),
    pub(super) cell_dimensions: (u16, u16),
    pub(super) has_overlays: bool,
}

pub(super) fn scene_for_buffer_age(
    history: &std::collections::VecDeque<PresentedScene>,
    age: u8,
) -> Option<&PresentedScene> {
    history.get(usize::from(age.checked_sub(1)?))
}

pub(super) fn frame_damage(
    previous: Option<&PresentedScene>,
    snapshot: &GridSnapshot,
    frame_size: (u32, u32),
    cell_dimensions: (u16, u16),
    has_overlays: bool,
    atlas: &mut GlyphAtlas,
) -> std::ops::Range<u32> {
    let Some(previous) = previous.filter(|previous| {
        !has_overlays
            && !previous.has_overlays
            && previous.frame_size == frame_size
            && previous.cell_dimensions == cell_dimensions
            && previous.snapshot.columns == snapshot.columns
            && previous.snapshot.rows == snapshot.rows
    }) else {
        return 0..frame_size.1;
    };
    let old = &previous.snapshot;
    let mut top = i64::from(frame_size.1);
    let mut bottom = 0i64;
    let cell_height = i64::from(cell_dimensions.1);
    if old.cursor != snapshot.cursor {
        for cursor in [old.cursor, snapshot.cursor].into_iter().flatten() {
            let y = cursor.row as i64 * cell_height;
            top = top.min(y);
            bottom = bottom.max(y + cell_height);
        }
    }
    let mut changed_rows = (0..snapshot.rows).filter(|&row| {
        let start = row * snapshot.columns;
        let end = start + snapshot.columns;
        old.cells[start..end] != snapshot.cells[start..end]
    });
    if let Some(first) = changed_rows.next() {
        let last = changed_rows.next_back().unwrap_or(first);
        top = top.min(first as i64 * cell_height);
        bottom = bottom.max((last + 1) as i64 * cell_height);
        // Scrolling usually changes the full viewport. Avoid looking up every
        // old/new glyph before a frame that already needs a complete repaint.
        if top == 0 && bottom >= (snapshot.rows as i64 * cell_height).min(i64::from(frame_size.1)) {
            return 0..frame_size.1;
        }
        // Include old and new changed ink, which may extend beyond a cell.
        // Unchanged ink outside this band is already correct in the buffer.
        for row in first..=last {
            let start = row * snapshot.columns;
            let end = start + snapshot.columns;
            for (old_cell, new_cell) in old.cells[start..end]
                .iter()
                .zip(&snapshot.cells[start..end])
            {
                if old_cell == new_cell {
                    continue;
                }
                for cell in [old_cell, new_cell] {
                    if !cell_content_is_visible(cell.flags)
                        || cell.flags.contains(CellFlags::WIDE_CONT)
                        || (cell.grapheme.is_none() && (cell.c == ' ' || cell.c == '\0'))
                    {
                        continue;
                    }
                    let glyph = atlas.get_or_insert_cell(cell);
                    let Some(baseline) = rounded_f64_i32(
                        row as f64 * f64::from(cell_dimensions.1) + f64::from(atlas.ascent),
                    ) else {
                        continue;
                    };
                    let y = i64::from(baseline) + i64::from(glyph.bearing_y);
                    top = top.min(y);
                    bottom = bottom.max(y + i64::from(glyph.pixel_h));
                }
            }
        }
    }
    if top >= bottom {
        return 0..0;
    }
    let height = i64::from(frame_size.1);
    top.clamp(0, height) as u32..bottom.clamp(0, height) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyph_atlas::GlyphKey;
    use crate::grid::cell::{Color, UnderlineStyle};
    use crate::grid::{CursorShape, Grid};
    use crate::linux_window::{
        atlas_cell_dimensions, draw_grid_snapshot_in_band, draw_grid_snapshot_with_images,
        rgb_to_xrgb, rounded_i32, snapshot_locked_grid,
    };
    use crate::terminal_colors::TerminalColors;
    use std::collections::VecDeque;
    use std::time::Instant;

    fn atlas() -> GlyphAtlas {
        GlyphAtlas::new("kokuban-test-font-that-does-not-exist", 14.0, 1.0).unwrap()
    }

    fn colors() -> TerminalColors {
        TerminalColors::new((224, 192, 160), (16, 32, 48))
    }

    fn paint(
        frame: &mut [u32],
        scene: &PresentedScene,
        atlas: &mut GlyphAtlas,
        damage: std::ops::Range<u32>,
    ) {
        let width = scene.frame_size.0 as usize;
        frame[damage.start as usize * width..damage.end as usize * width].fill(0x0010_2030);
        draw_grid_snapshot_in_band(
            frame,
            scene.frame_size,
            atlas,
            &colors(),
            scene.cell_dimensions,
            &scene.snapshot,
            true,
            &[],
            damage,
        );
    }

    fn assert_matches_full(frame: &[u32], scene: &PresentedScene, atlas: &mut GlyphAtlas) {
        let mut expected = vec![0x0010_2030; frame.len()];
        draw_grid_snapshot_with_images(
            &mut expected,
            scene.frame_size,
            atlas,
            colors(),
            scene.cell_dimensions,
            &scene.snapshot,
            true,
            &[],
        );
        let mismatch = frame
            .iter()
            .zip(expected)
            .position(|(actual, expected)| *actual != expected);
        assert_eq!(
            mismatch, None,
            "incremental pixels must match a full repaint"
        );
    }

    #[test]
    fn rotated_buffers_match_full_frames_through_text_scroll_styles_and_cursor_changes() {
        for buffer_count in 1..=3 {
            let mut atlas = atlas();
            let cell_dimensions = atlas_cell_dimensions(&atlas).unwrap();
            let frame_size = (
                u32::from(cell_dimensions.0) * 20 + 3,
                u32::from(cell_dimensions.1) * 8 + 5,
            );
            let mut grid = Grid::new(20, 8, 32);
            let mut buffers =
                vec![vec![0xdead_beef; (frame_size.0 * frame_size.1) as usize]; buffer_count];
            let mut presented_at = vec![None; buffer_count];
            let mut history = VecDeque::new();
            for step in 0..60 {
                grid.set_cursor_pos(step % 8, step % 16);
                match step % 10 {
                    0 | 1 => grid.put_char(char::from(b'A' + step as u8 % 26)),
                    2 => grid.put_char('界'),
                    3 => grid.erase_in_line(2),
                    4 => grid.scroll_up(1),
                    5 => {
                        let cell = grid.buffer.cell_mut(step % 8, step % 16);
                        cell.bg = Color::Rgb(96, 48, 24);
                        cell.fg = Color::Rgb(32, 64, 255);
                        cell.flags = CellFlags::REVERSE | CellFlags::FAINT;
                        cell.underline_style = UnderlineStyle::Curly;
                    }
                    6 => grid.cursor_visible = !grid.cursor_visible,
                    7 => grid.cursor_style.shape = CursorShape::Bar,
                    8 => grid.scroll_viewport_up(2),
                    _ => grid.scroll_to_bottom(),
                }
                let scene = PresentedScene {
                    snapshot: snapshot_locked_grid(&grid),
                    frame_size,
                    cell_dimensions,
                    has_overlays: false,
                };
                let slot = step % buffer_count;
                let age = presented_at[slot].map_or(0, |last: usize| (step - last) as u8);
                let damage = frame_damage(
                    scene_for_buffer_age(&history, age),
                    &scene.snapshot,
                    frame_size,
                    cell_dimensions,
                    false,
                    &mut atlas,
                );
                if age == 0 {
                    assert_eq!(damage, 0..frame_size.1);
                }
                paint(&mut buffers[slot], &scene, &mut atlas, damage);
                assert_matches_full(&buffers[slot], &scene, &mut atlas);
                history.push_front(scene);
                history.truncate(MAX_PRESENTED_SCENES);
                presented_at[slot] = Some(step);
            }
        }
    }

    #[test]
    fn small_edits_limit_damage_and_unchanged_scenes_need_no_rasterization() {
        let mut atlas = atlas();
        let cell_dimensions = atlas_cell_dimensions(&atlas).unwrap();
        let frame_size = (
            u32::from(cell_dimensions.0) * 20,
            u32::from(cell_dimensions.1) * 8,
        );
        let mut grid = Grid::new(20, 8, 0);
        grid.cursor_visible = false;
        let previous = PresentedScene {
            snapshot: snapshot_locked_grid(&grid),
            frame_size,
            cell_dimensions,
            has_overlays: false,
        };
        assert_eq!(
            frame_damage(
                Some(&previous),
                &previous.snapshot,
                frame_size,
                cell_dimensions,
                false,
                &mut atlas
            ),
            0..0
        );
        grid.set_cursor_pos(3, 5);
        grid.put_char('x');
        let current = snapshot_locked_grid(&grid);
        let damage = frame_damage(
            Some(&previous),
            &current,
            frame_size,
            cell_dimensions,
            false,
            &mut atlas,
        );
        assert!(damage.end - damage.start < frame_size.1 / 2);
        assert!(damage.start <= 3 * u32::from(cell_dimensions.1));
        assert!(damage.end >= 4 * u32::from(cell_dimensions.1));
    }

    #[test]
    fn changing_only_a_grapheme_repaints_old_and_new_ink_like_a_full_frame() {
        for (before, after) in [
            ("e\u{301}", "e\u{308}"),
            (" \u{301}", " \u{308}"),
            ("👩🏽\u{200d}💻", "👩🏻\u{200d}💻"),
            ("🇧🇷", "🇧🇪"),
        ] {
            let mut atlas = atlas();
            let cell_dimensions = atlas_cell_dimensions(&atlas).unwrap();
            let cell_height = u32::from(cell_dimensions.1);
            let frame_size = (u32::from(cell_dimensions.0) * 10, cell_height * 8);
            // Move the baseline near the row's top so real accent/emoji ink
            // overhangs its cell, exercising both damage bounds and band offsets.
            atlas.ascent = 0.5;
            let mut grid = Grid::new(10, 8, 0);
            grid.cursor_visible = false;
            grid.set_cursor_pos(4, 3);
            for c in before.chars() {
                grid.put_char(c);
            }
            let previous = PresentedScene {
                snapshot: snapshot_locked_grid(&grid),
                frame_size,
                cell_dimensions,
                has_overlays: false,
            };
            let mut frame = vec![0; (frame_size.0 * frame_size.1) as usize];
            paint(&mut frame, &previous, &mut atlas, 0..frame_size.1);
            let old_cell = previous.snapshot.cell(4, 3).unwrap();
            let old_glyph = atlas.get_or_insert_cell(old_cell);

            // c, flags, colors, cursor, and grid dimensions are unchanged.
            grid.buffer.cell_mut(4, 3).grapheme = Some(std::sync::Arc::from(after));
            let current = PresentedScene {
                snapshot: snapshot_locked_grid(&grid),
                frame_size,
                cell_dimensions,
                has_overlays: false,
            };
            let new_cell = current.snapshot.cell(4, 3).unwrap();
            assert_eq!(old_cell.c, new_cell.c);
            assert_eq!(old_cell.flags, new_cell.flags);
            let new_glyph = atlas.get_or_insert_cell(new_cell);
            let damage = frame_damage(
                Some(&previous),
                &current.snapshot,
                frame_size,
                cell_dimensions,
                false,
                &mut atlas,
            );
            assert!(
                !damage.is_empty(),
                "same-base grapheme change was ignored: {before} -> {after}"
            );
            assert!(
                damage.start > 0 && damage.end < frame_size.1,
                "expected a partial repaint"
            );
            let baseline = (f64::from(4 * cell_height) + f64::from(atlas.ascent)).round() as i64;
            for glyph in [old_glyph, new_glyph] {
                let ink_top = baseline + i64::from(glyph.bearing_y);
                let ink_bottom = ink_top + i64::from(glyph.pixel_h);
                assert!(
                    i64::from(damage.start) <= ink_top,
                    "clipped accent/emoji overhang: {before} -> {after}"
                );
                assert!(i64::from(damage.end) >= ink_bottom);
            }
            paint(&mut frame, &current, &mut atlas, damage);
            assert_matches_full(&frame, &current, &mut atlas);
        }
    }

    #[test]
    fn unknown_age_dimensions_and_overlay_transitions_require_full_repaint() {
        let mut atlas = atlas();
        let cell_dimensions = atlas_cell_dimensions(&atlas).unwrap();
        let frame_size = (200, 160);
        let grid = Grid::new(10, 8, 0);
        let current = snapshot_locked_grid(&grid);
        let mut history = VecDeque::from([PresentedScene {
            snapshot: snapshot_locked_grid(&grid),
            frame_size,
            cell_dimensions,
            has_overlays: false,
        }]);
        for age in [0, 2, 255] {
            assert_eq!(
                frame_damage(
                    scene_for_buffer_age(&history, age),
                    &current,
                    frame_size,
                    cell_dimensions,
                    false,
                    &mut atlas
                ),
                0..frame_size.1
            );
        }
        for (size, cells, overlay) in [
            ((201, 160), cell_dimensions, false),
            (
                frame_size,
                (cell_dimensions.0 + 1, cell_dimensions.1),
                false,
            ),
            (frame_size, cell_dimensions, true),
        ] {
            assert_eq!(
                frame_damage(history.front(), &current, size, cells, overlay, &mut atlas),
                0..size.1
            );
        }
        history.front_mut().unwrap().has_overlays = true;
        assert_eq!(
            frame_damage(
                history.front(),
                &current,
                frame_size,
                cell_dimensions,
                false,
                &mut atlas
            ),
            0..frame_size.1
        );
        history.front_mut().unwrap().has_overlays = false;
        let resized = snapshot_locked_grid(&Grid::new(11, 8, 0));
        assert_eq!(
            frame_damage(
                history.front(),
                &resized,
                frame_size,
                cell_dimensions,
                false,
                &mut atlas
            ),
            0..frame_size.1
        );
    }

    #[test]
    fn glyph_overhang_is_removed_and_repainted_with_fractional_font_metrics() {
        let mut atlas = atlas();
        let cell_dimensions = atlas_cell_dimensions(&atlas).unwrap();
        let cell_height = u32::from(cell_dimensions.1);
        // Synthetic font ink spans several cells, with a half-pixel baseline.
        atlas.ascent = cell_height as f32 + 0.5;
        atlas.glyphs.insert(
            GlyphKey {
                c: '☃',
                bold: false,
                italic: false,
            },
            crate::glyph_atlas::GlyphEntry {
                atlas_x: 0,
                atlas_y: 0,
                pixel_w: 3,
                pixel_h: cell_height * 3,
                bearing_x: -2,
                bearing_y: -(cell_height as i32) * 2,
                color: false,
            },
        );
        for y in 0..cell_height * 3 {
            atlas.pixels[y as usize * atlas.width as usize..y as usize * atlas.width as usize + 3]
                .fill(192);
        }
        let frame_size = (u32::from(cell_dimensions.0) * 10, cell_height * 8);
        let mut grid = Grid::new(10, 8, 0);
        grid.cursor_visible = false;
        grid.set_cursor_pos(4, 3);
        grid.put_char('☃');
        // Its unchanged neighbor also overlaps the band's edge and must be
        // composited after clearing the changed glyph's old ink.
        grid.set_cursor_pos(3, 6);
        grid.put_char('☃');
        let mut previous = PresentedScene {
            snapshot: snapshot_locked_grid(&grid),
            frame_size,
            cell_dimensions,
            has_overlays: false,
        };
        let mut frame = vec![0; (frame_size.0 * frame_size.1) as usize];
        paint(&mut frame, &previous, &mut atlas, 0..frame_size.1);
        let ink_x = usize::from(cell_dimensions.0) * 3 - 2;
        let ink_y = (f64::from(cell_height * 4) + f64::from(atlas.ascent)).round() as usize
            - cell_height as usize * 2;
        assert_ne!(frame[ink_y * frame_size.0 as usize + ink_x], 0x0010_2030);
        assert_eq!(
            frame[(ink_y - 1) * frame_size.0 as usize + ink_x],
            0x0010_2030
        );
        for character in [' ', '☃', 'x'] {
            grid.set_cursor_pos(4, 3);
            grid.put_char(character);
            let current = PresentedScene {
                snapshot: snapshot_locked_grid(&grid),
                frame_size,
                cell_dimensions,
                has_overlays: false,
            };
            let damage = frame_damage(
                Some(&previous),
                &current.snapshot,
                frame_size,
                cell_dimensions,
                false,
                &mut atlas,
            );
            assert!(damage.start < 4 * cell_height);
            assert!(damage.end > 5 * cell_height);
            paint(&mut frame, &current, &mut atlas, damage);
            assert_matches_full(&frame, &current, &mut atlas);
            previous = current;
        }
    }

    #[test]
    fn one_pixel_bands_preserve_overhang_backgrounds_and_independent_underlines() {
        for ascent in [-0.5, 0.5, 17.5] {
            let mut atlas = atlas();
            // Fixed cell geometry makes every cell/ink boundary independent of
            // the installed font. Both signs of half-pixel baselines matter.
            let cell_dimensions = (8, 12);
            atlas.ascent = ascent;
            let short = crate::glyph_atlas::GlyphEntry {
                atlas_x: 0,
                atlas_y: 0,
                pixel_w: 5,
                pixel_h: 1,
                bearing_x: 1,
                bearing_y: -rounded_i32(ascent).unwrap(),
                color: false,
            };
            let tall = crate::glyph_atlas::GlyphEntry {
                atlas_x: 8,
                pixel_w: 16,
                pixel_h: 36,
                bearing_x: -1,
                bearing_y: -18,
                ..short
            };
            for (character, glyph) in [('A', short), ('日', tall)] {
                assert!(!atlas.is_color(glyph));
                for y in 0..glyph.pixel_h {
                    let start = (glyph.atlas_y + y) as usize * atlas.width as usize
                        + glyph.atlas_x as usize;
                    atlas.pixels[start..start + glyph.pixel_w as usize].fill(192);
                }
                atlas.glyphs.insert(
                    GlyphKey {
                        c: character,
                        bold: false,
                        italic: false,
                    },
                    glyph,
                );
            }

            let mut grid = Grid::new(10, 6, 0);
            grid.cursor_visible = false;
            for row in 0..6 {
                for column in 0..10 {
                    grid.buffer.cell_mut(row, column).bg =
                        Color::Rgb(30 + row as u8 * 15, 40 + column as u8 * 10, 60);
                }
            }
            // Unchanged wide neighbors extend into bands outside their rows.
            for row in [2, 4, 5] {
                grid.set_cursor_pos(row, 7);
                grid.put_char('日');
            }
            for (column, style) in [
                UnderlineStyle::Single,
                UnderlineStyle::Double,
                UnderlineStyle::Curly,
                UnderlineStyle::Dotted,
                UnderlineStyle::Dashed,
            ]
            .into_iter()
            .enumerate()
            {
                grid.set_cursor_pos(3, column);
                grid.put_char('A');
                let cell = grid.buffer.cell_mut(3, column);
                cell.underline_style = style;
                cell.underline_color = Color::Rgb(240, 10, 20);
            }
            let reversed = grid.buffer.cell_mut(1, 1);
            reversed.flags.insert(CellFlags::REVERSE);
            reversed.bg = Color::Default;
            let hidden = grid.buffer.cell_mut(3, 5);
            hidden.c = 'A';
            hidden.flags.insert(CellFlags::HIDDEN);
            hidden.underline_style = UnderlineStyle::Single;
            let blank = grid.buffer.cell_mut(3, 6);
            blank.underline_style = UnderlineStyle::Double;
            blank.underline_color = Color::Rgb(240, 10, 20);

            let scene = PresentedScene {
                snapshot: snapshot_locked_grid(&grid),
                frame_size: (80, 80), // Also exercise padding below the grid.
                cell_dimensions,
                has_overlays: false,
            };
            let mut expected = vec![0; 80 * 80];
            paint(&mut expected, &scene, &mut atlas, 0..80);
            assert!(
                expected.contains(&rgb_to_xrgb(240, 10, 20)),
                "underline fixture has no ink"
            );
            for y in 0..80 {
                // Starting with a sentinel also verifies no writes escape the
                // requested band. Narrow bands hit exact glyph top/bottom edges.
                let mut actual = vec![0x00ab_cdef; expected.len()];
                paint(&mut actual, &scene, &mut atlas, y..y + 1);
                let start = y as usize * 80;
                let end = start + 80;
                assert_eq!(
                    &actual[start..end],
                    &expected[start..end],
                    "ascent={ascent} y={y}"
                );
                assert!(actual[..start]
                    .iter()
                    .chain(&actual[end..])
                    .all(|pixel| *pixel == 0x00ab_cdef));
            }
        }
    }

    #[test]
    #[ignore = "CPU raster microbenchmark; run release on an idle Linux host with --nocapture"]
    fn benchmark_frame_repaint() {
        use std::hint::black_box;
        use unicode_width::UnicodeWidthStr;

        fn positive_setting(name: &str, default: usize) -> usize {
            let value = std::env::var(name).map_or(default, |value| {
                value
                    .parse()
                    .unwrap_or_else(|_| panic!("{name} must be a positive integer"))
            });
            assert!(value > 0, "{name} must be a positive integer");
            value
        }

        fn checksum(frame: &[u32]) -> u64 {
            frame
                .iter()
                .flat_map(|pixel| pixel.to_le_bytes())
                .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
                })
        }

        fn assert_frame(frame: &[u32], expected: &[u32]) {
            assert_eq!(frame.len(), expected.len());
            let mismatch = frame.iter().zip(expected).position(|(a, b)| a != b);
            assert_eq!(
                mismatch, None,
                "incremental pixels must match a full repaint"
            );
        }

        let content = std::env::var("KOKUBAN_FRAME_CONTENT").unwrap_or_else(|_| "all".into());
        let change = std::env::var("KOKUBAN_FRAME_CHANGE").unwrap_or_else(|_| "all".into());
        assert!(["all", "ascii", "ascii-after-emoji", "unicode"].contains(&content.as_str()));
        assert!(["all", "single-row", "full-screen"].contains(&change.as_str()));
        let samples = positive_setting("KOKUBAN_FRAME_SAMPLES", 5);
        let steps = positive_setting("KOKUBAN_FRAME_STEPS", 300);
        let warmup = positive_setting("KOKUBAN_FRAME_WARMUP", 30);
        let output_dir = std::env::var_os("KOKUBAN_FRAME_OUTPUT_DIR").map(std::path::PathBuf::from);
        if let Some(directory) = &output_dir {
            std::fs::create_dir_all(directory).unwrap();
        }

        for workload in ["ascii", "ascii-after-emoji", "unicode"] {
            if content != "all" && content != workload {
                continue;
            }
            // A fresh atlas keeps plain ASCII independent of earlier emoji cases.
            let mut atlas = GlyphAtlas::new("DejaVu Sans Mono", 14.0, 1.0).unwrap();
            assert!(atlas.glyphs.values().all(|glyph| !atlas.is_color(*glyph)));
            if workload != "ascii" {
                let emoji = atlas.get_or_insert_text("👩🏽\u{200d}💻", false, false);
                assert!(
                    atlas.is_color(emoji),
                    "install Noto Color Emoji for {workload}"
                );
                assert!(emoji.pixel_w > 0 && emoji.pixel_h > 0);
            }
            let cell_dimensions = atlas_cell_dimensions(&atlas).unwrap();
            let frame_size = (
                u32::from(cell_dimensions.0) * 120,
                u32::from(cell_dimensions.1) * 40,
            );
            let patterns = [
                "Abéλ界e\u{301}👩🏽\u{200d}💻🇧🇷x",
                "CdñΩ語o\u{308}👨🏻\u{200d}💻🇯🇵y",
            ];
            for pattern in patterns {
                assert_eq!(pattern.width(), 12, "fixture must fill exactly 120 columns");
            }
            let write_row = |grid: &mut Grid, row, variant: usize| {
                grid.set_cursor_pos(row, 0);
                if workload == "unicode" {
                    for character in patterns[variant].repeat(10).chars() {
                        grid.put_char(character);
                    }
                } else {
                    for column in 0..120 {
                        grid.put_char(char::from(b'!' + variant as u8 + (column % 90) as u8));
                    }
                }
            };

            for mutation in ["single-row", "full-screen"] {
                if change != "all" && change != mutation {
                    continue;
                }
                let mut grid = Grid::new(120, 40, 0);
                grid.cursor_visible = false;
                for row in 0..40 {
                    write_row(&mut grid, row, 0);
                }
                let scene = |grid: &Grid| PresentedScene {
                    snapshot: snapshot_locked_grid(grid),
                    frame_size,
                    cell_dimensions,
                    has_overlays: false,
                };
                let first = scene(&grid);
                for row in 0..40 {
                    if mutation == "full-screen" || row == 20 {
                        write_row(&mut grid, row, 1);
                    }
                }
                let scenes = [first, scene(&grid)];
                let mut frame = vec![0; (frame_size.0 * frame_size.1) as usize];
                // Build full reference frames and warm every glyph before any timing.
                let expected = scenes.each_ref().map(|scene| {
                    let mut pixels = vec![0; frame.len()];
                    paint(&mut pixels, scene, &mut atlas, 0..frame_size.1);
                    pixels
                });
                assert!(
                    expected[0] != expected[1],
                    "fixture must change visible pixels"
                );
                let mut color_cells = 0;
                let mut mono_cells = 0;
                for cell in &scenes[0].snapshot.cells {
                    if cell.flags.contains(CellFlags::WIDE_CONT) {
                        continue;
                    }
                    let glyph = atlas.get_or_insert_cell(cell);
                    assert!(glyph.pixel_w > 0 && glyph.pixel_h > 0);
                    if atlas.is_color(glyph) {
                        color_cells += 1;
                    } else {
                        mono_cells += 1;
                    }
                }
                assert!(mono_cells > 0);
                assert_eq!(color_cells > 0, workload == "unicode");
                for (index, pixels) in expected.iter().enumerate() {
                    if let Some(directory) = &output_dir {
                        let bytes: Vec<u8> = pixels
                            .iter()
                            .flat_map(|pixel| pixel.to_le_bytes())
                            .collect();
                        let path =
                            directory.join(format!("{workload}-{mutation}-{index}.xrgb8888le"));
                        std::fs::write(path, bytes).unwrap();
                    }
                }
                frame.copy_from_slice(&expected[1]);
                let mut bands = Vec::new();
                for step in 0..2 {
                    let damage = frame_damage(
                        Some(&scenes[1 - step]),
                        &scenes[step].snapshot,
                        frame_size,
                        cell_dimensions,
                        false,
                        &mut atlas,
                    );
                    assert!(!damage.is_empty(), "visible edit must produce damage");
                    if mutation == "single-row" {
                        assert!(damage.end - damage.start < frame_size.1);
                    } else {
                        assert_eq!(damage, 0..frame_size.1);
                    }
                    bands.push((damage.start, damage.end));
                    paint(&mut frame, &scenes[step], &mut atlas, damage);
                    assert_frame(&frame, &expected[step]);
                }
                println!(
                    "frame-repaint fixture content={workload} change={mutation} cols=120 rows=40 \
                     width={} height={} cell_width={} cell_height={} warmup={warmup} \
                     samples={samples} steps={steps} mono_cells={mono_cells} color_cells={color_cells} \
                     bands={bands:?} frame0={:016x} frame1={:016x}",
                    frame_size.0, frame_size.1, cell_dimensions.0, cell_dimensions.1,
                    checksum(&expected[0]), checksum(&expected[1]),
                );
                for sample in 0..samples {
                    for incremental in if sample % 2 == 0 {
                        [false, true]
                    } else {
                        [true, false]
                    } {
                        // Snapshot creation, font rasterization, pixel checks and output
                        // are excluded. This includes damage calculation and CPU paint;
                        // it does not measure PTY, window presentation or input latency.
                        for (measured, iterations) in [(false, warmup), (true, steps)] {
                            paint(&mut frame, &scenes[1], &mut atlas, 0..frame_size.1);
                            atlas.dirty = false;
                            let start = Instant::now();
                            for step in 0..iterations {
                                let current = &scenes[step % 2];
                                let previous = &scenes[(step + 1) % 2];
                                let damage = if incremental {
                                    frame_damage(
                                        Some(previous),
                                        &current.snapshot,
                                        frame_size,
                                        cell_dimensions,
                                        false,
                                        &mut atlas,
                                    )
                                } else {
                                    0..frame_size.1
                                };
                                paint(black_box(&mut frame), current, &mut atlas, damage);
                                black_box(&frame);
                            }
                            let elapsed = start.elapsed();
                            assert!(!atlas.dirty, "glyph rasterization escaped prewarm");
                            assert_frame(&frame, &expected[(iterations - 1) % 2]);
                            if measured {
                                println!(
                                    "frame-repaint sample content={workload} change={mutation} \
                                     sample={sample} incremental={incremental} frames={iterations} \
                                     elapsed_ns={} ns_per_frame={:.3}",
                                    elapsed.as_nanos(),
                                    elapsed.as_nanos() as f64 / iterations as f64,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
