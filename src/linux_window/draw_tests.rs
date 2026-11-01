use super::*;
use crate::glyph_atlas::{GlyphEntry, GlyphKey};
use crate::linux_window::clipboard::apply_selection_to_snapshot;
use crate::linux_window::sizing::atlas_cell_dimensions;
use crate::linux_window::test_support::{test_atlas, test_colors, DEFAULT_BACKGROUND};

fn render_grid(grid: Grid, atlas: &mut GlyphAtlas) -> (Vec<u32>, (u16, u16)) {
    let columns = grid.cols();
    let rows = grid.rows();
    let cell_dimensions =
        atlas_cell_dimensions(atlas).expect("test atlas should have valid cell metrics");
    let frame_size = (
        u32::try_from(columns).expect("test grid width should fit u32")
            * u32::from(cell_dimensions.0),
        u32::try_from(rows).expect("test grid height should fit u32")
            * u32::from(cell_dimensions.1),
    );
    let frame_len = usize::try_from(u64::from(frame_size.0) * u64::from(frame_size.1))
        .expect("test frame should fit memory");
    let background = rgb_to_xrgb(
        DEFAULT_BACKGROUND.0,
        DEFAULT_BACKGROUND.1,
        DEFAULT_BACKGROUND.2,
    );
    let mut frame = vec![background; frame_len];
    let grid = Mutex::new(grid);
    let snapshot = snapshot_grid(&grid).expect("test grid should not be poisoned");

    draw_grid_snapshot(
        &mut frame,
        frame_size,
        atlas,
        test_colors(),
        cell_dimensions,
        &snapshot,
        true,
    );

    (frame, cell_dimensions)
}

#[test]
fn converts_rgb_to_softbuffer_xrgb() {
    assert_eq!(rgb_to_xrgb(0x1a, 0x2b, 0x3c), 0x001a_2b3c);
    assert_eq!(rgb_to_xrgb(0xff, 0xff, 0xff) >> 24, 0);
}

#[test]
fn rounds_only_finite_coordinates_that_fit_i32() {
    assert_eq!(rounded_i32(12.4), Some(12));
    assert_eq!(rounded_i32(12.5), Some(13));
    assert_eq!(rounded_i32(f32::NAN), None);
    assert_eq!(rounded_i32(f32::INFINITY), None);
    assert_eq!(rounded_i32(f32::MAX), None);
    assert_eq!(rounded_i32(i32::MAX as f32), None);
}

#[test]
fn positions_a8_glyphs_from_cell_origin_ascent_and_bearings() {
    let background = rgb_to_xrgb(0x10, 0x20, 0x30);
    let foreground = rgb_to_xrgb(0xe0, 0x40, 0x20);
    let mut frame = vec![background; 5 * 5];
    let glyph = GlyphEntry {
        atlas_x: 0,
        atlas_y: 0,
        pixel_w: 1,
        pixel_h: 1,
        bearing_x: -1,
        bearing_y: 2,
        color: false,
    };

    draw_cell_glyph(
        &mut frame,
        (5, 5),
        super::GlyphSource {
            pixels: &[255],
            rgba_pixels: None,
            size: (1, 1),
            ascent: 1.4,
        },
        glyph,
        (2, 1),
        foreground,
    );

    assert_eq!(frame[4 * 5 + 1], foreground);
    assert_eq!(
        frame.iter().filter(|&&pixel| pixel != background).count(),
        1
    );
}

#[test]
fn empty_grid_has_no_banner_and_first_cell_renders_at_the_frame_origin() {
    let background = rgb_to_xrgb(
        DEFAULT_BACKGROUND.0,
        DEFAULT_BACKGROUND.1,
        DEFAULT_BACKGROUND.2,
    );
    let mut atlas = test_atlas();
    let mut empty_grid = Grid::new(2, 1, 0);
    empty_grid.cursor_visible = false;

    let (empty_frame, _) = render_grid(empty_grid, &mut atlas);

    assert!(empty_frame.iter().all(|&pixel| pixel == background));

    let character = 'é';
    let key = crate::glyph_atlas::GlyphKey {
        c: character,
        bold: true,
        italic: true,
    };
    assert!(!atlas.glyphs.contains_key(&key));
    let mut grid = Grid::new(2, 1, 0);
    grid.cursor_visible = false;
    *grid.buffer.cell_mut(0, 0) = Cell {
        c: character,
        bg: Color::Rgb(1, 2, 3),
        flags: CellFlags::BOLD | CellFlags::FAINT | CellFlags::ITALIC,
        ..Cell::default()
    };

    let (frame, cell_dimensions) = render_grid(grid, &mut atlas);

    let frame_width = usize::from(cell_dimensions.0) * 2;
    let first_cell_background = rgb_to_xrgb(1, 2, 3);
    assert!(atlas.glyphs.contains_key(&key));
    assert!((0..usize::from(cell_dimensions.1)).any(|row| {
        let start = row * frame_width;
        frame[start..start + usize::from(cell_dimensions.0)].contains(&first_cell_background)
    }));
}

#[test]
fn snapshot_renderer_sends_the_full_grapheme_to_the_atlas() {
    for text in ["e\u{301}", "👩🏽\u{200d}💻", "🇧🇷", "1\u{fe0f}\u{20e3}"] {
        let mut atlas = test_atlas();
        let mut grid = Grid::new(4, 1, 0);
        grid.cursor_visible = false;
        for character in text.chars() {
            grid.put_char(character);
        }
        let (frame, _) = render_grid(grid, &mut atlas);
        atlas.dirty = false;
        let glyph = atlas.get_or_insert_text(text, false, false);
        assert!(!atlas.dirty, "renderer only cached part of {text}");
        assert!(glyph.pixel_w > 0 && glyph.pixel_h > 0);
        let background = rgb_to_xrgb(
            DEFAULT_BACKGROUND.0,
            DEFAULT_BACKGROUND.1,
            DEFAULT_BACKGROUND.2,
        );
        assert!(
            frame.iter().any(|&pixel| pixel != background),
            "blank rendered grapheme: {text}"
        );
    }
}

#[test]
fn resolves_bold_indexed_foreground_before_reverse_and_fills_cell_backgrounds() {
    let colors = test_colors();
    let normal = Cell {
        fg: Color::Indexed(1),
        bg: Color::Rgb(4, 5, 6),
        flags: CellFlags::BOLD,
        ..Cell::default()
    };
    let reversed = Cell {
        flags: CellFlags::BOLD | CellFlags::REVERSE,
        ..normal.clone()
    };

    assert_eq!(
        resolve_cell_colors(&colors, &normal),
        ResolvedCellColors {
            foreground: rgb_to_xrgb(241, 76, 76),
            background: rgb_to_xrgb(4, 5, 6),
        }
    );
    assert_eq!(
        resolve_cell_colors(&colors, &reversed),
        ResolvedCellColors {
            foreground: rgb_to_xrgb(4, 5, 6),
            background: rgb_to_xrgb(241, 76, 76),
        }
    );

    let mut grid = Grid::new(2, 1, 0);
    grid.cursor_visible = false;
    *grid.buffer.cell_mut(0, 0) = Cell {
        bg: Color::Indexed(4),
        ..Cell::default()
    };
    *grid.buffer.cell_mut(0, 1) = reversed;
    let mut atlas = test_atlas();

    let (frame, cell_dimensions) = render_grid(grid, &mut atlas);

    let cell_area = usize::from(cell_dimensions.0) * usize::from(cell_dimensions.1);
    assert_eq!(frame.len(), cell_area * 2);
    for row in 0..usize::from(cell_dimensions.1) {
        let row_start = row * usize::from(cell_dimensions.0) * 2;
        assert!(frame[row_start..row_start + usize::from(cell_dimensions.0)]
            .iter()
            .all(|&pixel| pixel == rgb_to_xrgb(36, 114, 200)));
        assert!(frame[row_start + usize::from(cell_dimensions.0)
            ..row_start + usize::from(cell_dimensions.0) * 2]
            .iter()
            .all(|&pixel| pixel == rgb_to_xrgb(241, 76, 76)));
    }
}

#[test]
fn converts_faint_cell_colors_to_xrgb_without_mutating_style_flags() {
    let cell = Cell {
        fg: Color::Rgb(0, 0, 0),
        bg: Color::Rgb(255, 255, 255),
        flags: CellFlags::BOLD | CellFlags::FAINT,
        ..Cell::default()
    };

    assert_eq!(
        resolve_cell_colors(&test_colors(), &cell),
        ResolvedCellColors {
            foreground: 0x007f_7f7f,
            background: 0x00ff_ffff,
        }
    );
    assert!(cell.flags.contains(CellFlags::BOLD));
    assert!(cell.flags.contains(CellFlags::FAINT));
}

#[test]
fn grid_snapshot_preserves_faint_cell_flags() {
    let mut grid = Grid::new(1, 1, 0);
    grid.flags = CellFlags::FAINT;
    grid.put_char('A');

    let snapshot = snapshot_grid(&Mutex::new(grid)).expect("snapshot should succeed");

    assert!(snapshot
        .cell(0, 0)
        .expect("printed cell should be present")
        .flags
        .contains(CellFlags::FAINT));
}

#[test]
fn linux_underline_rasterizes_every_style_inside_its_cell() {
    let frame_size = (20, 12);
    let background = rgb_to_xrgb(1, 2, 3);
    let underline = rgb_to_xrgb(200, 100, 50);
    let origin = (2, 1);
    let width = 12;
    let height = 10;
    let ascent = 6.0;

    for (style, expected_pixels) in [
        (UnderlineStyle::Single, 12),
        (UnderlineStyle::Double, 24),
        (UnderlineStyle::Curly, 12),
        (UnderlineStyle::Dotted, 4),
        (UnderlineStyle::Dashed, 8),
    ] {
        let mut frame = vec![background; (frame_size.0 * frame_size.1) as usize];

        draw_cell_underline(
            &mut frame, frame_size, origin, width, height, ascent, style, underline,
        );

        let changed = frame
            .iter()
            .enumerate()
            .filter(|&(_, pixel)| *pixel != background)
            .collect::<Vec<_>>();
        assert_eq!(changed.len(), expected_pixels, "{style:?}");
        assert!(changed.iter().all(|&(index, &pixel)| {
            let x = index % frame_size.0 as usize;
            let y = index / frame_size.0 as usize;
            pixel == underline && (2..14).contains(&x) && (1..11).contains(&y)
        }));
    }

    let mut frame = vec![background; (frame_size.0 * frame_size.1) as usize];
    draw_cell_underline(
        &mut frame,
        frame_size,
        origin,
        width,
        height,
        ascent,
        UnderlineStyle::None,
        underline,
    );
    assert!(frame.iter().all(|&pixel| pixel == background));
}

#[test]
fn linux_underlines_blank_and_wide_cells_with_explicit_colors() {
    let mut grid = Grid::new(3, 1, 0);
    grid.cursor_visible = false;
    *grid.buffer.cell_mut(0, 0) = Cell {
        flags: CellFlags::UNDERLINE,
        underline_style: UnderlineStyle::Single,
        underline_color: Color::Rgb(220, 30, 40),
        ..Cell::default()
    };
    *grid.buffer.cell_mut(0, 1) = Cell {
        flags: CellFlags::UNDERLINE | CellFlags::WIDE,
        underline_style: UnderlineStyle::Double,
        underline_color: Color::Rgb(20, 210, 80),
        ..Cell::default()
    };
    *grid.buffer.cell_mut(0, 2) = Cell {
        c: '\0',
        flags: CellFlags::WIDE_CONT,
        ..Cell::default()
    };
    let mut atlas = test_atlas();
    let ascent = atlas.ascent;

    let (frame, cell_dimensions) = render_grid(grid, &mut atlas);

    let cell_width = usize::from(cell_dimensions.0);
    let frame_width = cell_width * 3;
    let single_y = usize::try_from(
        underline_anchor_y(
            0,
            u32::from(cell_dimensions.1),
            ascent,
            UnderlineStyle::Single,
        )
        .expect("single underline should fit its cell"),
    )
    .unwrap();
    let double_y = usize::try_from(
        underline_anchor_y(
            0,
            u32::from(cell_dimensions.1),
            ascent,
            UnderlineStyle::Double,
        )
        .expect("double underline should fit its cell"),
    )
    .unwrap();
    let single = rgb_to_xrgb(220, 30, 40);
    let double = rgb_to_xrgb(20, 210, 80);

    assert!(
        frame[single_y * frame_width..single_y * frame_width + cell_width]
            .iter()
            .all(|&pixel| pixel == single)
    );
    for row in [double_y, double_y + 2] {
        let start = row * frame_width + cell_width;
        assert!(frame[start..start + cell_width * 2]
            .iter()
            .all(|&pixel| pixel == double));
    }
}

#[test]
fn linux_default_underline_tracks_resolved_faint_reverse_foreground() {
    let default_underline = Cell {
        fg: Color::Rgb(0, 0, 0),
        bg: Color::Rgb(255, 255, 255),
        flags: CellFlags::BOLD | CellFlags::FAINT | CellFlags::REVERSE,
        underline_style: UnderlineStyle::Single,
        ..Cell::default()
    };
    let resolved = resolve_cell_colors(&test_colors(), &default_underline);
    assert_eq!(
        resolve_cell_underline_color(&test_colors(), &default_underline, resolved.foreground,),
        resolved.foreground
    );

    let explicit_underline = Cell {
        underline_color: Color::Indexed(1),
        ..default_underline
    };
    assert_eq!(
        resolve_cell_underline_color(&test_colors(), &explicit_underline, resolved.foreground,),
        rgb_to_xrgb(205, 49, 49),
        "explicit underline color is resolved independently from bold/faint/reverse"
    );
}

#[test]
fn linux_underline_anchor_clamps_extreme_metrics_to_the_cell() {
    for style in [
        UnderlineStyle::Single,
        UnderlineStyle::Double,
        UnderlineStyle::Curly,
        UnderlineStyle::Dotted,
        UnderlineStyle::Dashed,
    ] {
        for ascent in [-100.0, 100.0] {
            let y = underline_anchor_y(7, 5, ascent, style)
                .expect("finite metrics should produce an underline anchor");
            assert!((7..=11).contains(&y), "{style:?} at ascent {ascent}");
        }
    }
    assert_eq!(underline_anchor_y(0, 10, 5.0, UnderlineStyle::None), None);
    assert_eq!(underline_anchor_y(0, 0, 5.0, UnderlineStyle::Single), None);
    assert_eq!(
        underline_anchor_y(0, 10, f32::NAN, UnderlineStyle::Single),
        None
    );

    let background = rgb_to_xrgb(1, 2, 3);
    let underline = rgb_to_xrgb(4, 5, 6);
    for style in [UnderlineStyle::Double, UnderlineStyle::Curly] {
        let mut frame = vec![background; 12];
        draw_cell_underline(&mut frame, (4, 3), (0, 1), 4, 1, 100.0, style, underline);
        assert!(frame[..4].iter().all(|&pixel| pixel == background));
        assert!(frame[4..8].iter().all(|&pixel| pixel == underline));
        assert!(frame[8..].iter().all(|&pixel| pixel == background));
    }
}

#[test]
fn concealed_linux_cells_never_render_content_or_decorations() {
    for visible_flags in [
        CellFlags::empty(),
        CellFlags::BOLD | CellFlags::FAINT | CellFlags::REVERSE,
        CellFlags::ITALIC | CellFlags::UNDERLINE | CellFlags::WIDE,
    ] {
        assert!(cell_content_is_visible(visible_flags));
        assert!(!cell_content_is_visible(visible_flags | CellFlags::HIDDEN));
    }
}

#[test]
fn hidden_wide_snapshot_keeps_resolved_background_without_glyph_or_underline() {
    let character = '日';
    let key = crate::glyph_atlas::GlyphKey {
        c: character,
        bold: true,
        italic: true,
    };
    let mut grid = Grid::new(2, 1, 0);
    grid.cursor_visible = false;
    grid.fg = Color::Rgb(180, 120, 60);
    grid.bg = Color::Rgb(10, 20, 30);
    grid.flags = CellFlags::BOLD
        | CellFlags::FAINT
        | CellFlags::ITALIC
        | CellFlags::UNDERLINE
        | CellFlags::REVERSE
        | CellFlags::HIDDEN;
    grid.underline_style = UnderlineStyle::Double;
    grid.put_char(character);

    let grid = Mutex::new(grid);
    let snapshot = snapshot_grid(&grid).expect("hidden grid should snapshot");
    let leader = snapshot.cell(0, 0).expect("wide leader should exist");
    assert!(leader.flags.contains(CellFlags::HIDDEN | CellFlags::WIDE));
    assert_eq!(leader.underline_style, UnderlineStyle::Double);
    assert_eq!(
        snapshot
            .cell(0, 1)
            .expect("wide continuation should exist")
            .flags,
        CellFlags::WIDE_CONT
    );
    assert!(snapshot
        .rendered_cell(0, 1)
        .expect("continuation should resolve through its leader")
        .flags
        .contains(CellFlags::HIDDEN));
    let expected_background = resolve_cell_colors(&test_colors(), leader).background;

    let mut atlas = test_atlas();
    assert!(!atlas.glyphs.contains_key(&key));
    let (frame, _) = render_grid(grid.into_inner().unwrap(), &mut atlas);

    assert!(frame.iter().all(|&pixel| pixel == expected_background));
    assert!(!atlas.glyphs.contains_key(&key));
}

#[test]
fn terminal_cursor_remains_visible_over_hidden_linux_content() {
    let character = 'é';
    let key = crate::glyph_atlas::GlyphKey {
        c: character,
        bold: false,
        italic: false,
    };
    let mut grid = Grid::new(1, 1, 0);
    *grid.buffer.cell_mut(0, 0) = Cell {
        c: character,
        bg: Color::Rgb(10, 20, 30),
        flags: CellFlags::HIDDEN,
        ..Cell::default()
    };
    let background = rgb_to_xrgb(10, 20, 30);
    let mut atlas = test_atlas();
    assert!(!atlas.glyphs.contains_key(&key));

    let (frame, _) = render_grid(grid, &mut atlas);

    assert!(frame.iter().any(|&pixel| pixel != background));
    assert!(!atlas.glyphs.contains_key(&key));
}

#[test]
fn wide_continuations_neither_overwrite_the_leader_background_nor_add_a_glyph() {
    let continuation_character = 'Ω';
    let continuation_key = crate::glyph_atlas::GlyphKey {
        c: continuation_character,
        bold: false,
        italic: false,
    };
    let leader_background = rgb_to_xrgb(9, 8, 7);
    let mut grid = Grid::new(2, 1, 0);
    grid.cursor_visible = false;
    *grid.buffer.cell_mut(0, 0) = Cell {
        bg: Color::Rgb(9, 8, 7),
        flags: CellFlags::WIDE,
        ..Cell::default()
    };
    *grid.buffer.cell_mut(0, 1) = Cell {
        c: continuation_character,
        bg: Color::Rgb(200, 100, 50),
        flags: CellFlags::WIDE_CONT,
        ..Cell::default()
    };
    let mut atlas = test_atlas();
    assert!(!atlas.glyphs.contains_key(&continuation_key));

    let (frame, _) = render_grid(grid, &mut atlas);

    assert!(frame.iter().all(|&pixel| pixel == leader_background));
    assert!(!atlas.glyphs.contains_key(&continuation_key));
}

#[test]
fn pending_cursor_uses_the_wide_leader_background_for_contrast() {
    let mut atlas = test_atlas();

    for shape in [CursorShape::Block, CursorShape::Bar, CursorShape::Underline] {
        let mut grid = Grid::new(2, 1, 0);
        grid.cursor_style.shape = shape;
        *grid.buffer.cell_mut(0, 0) = Cell {
            c: ' ',
            fg: Color::Rgb(255, 255, 255),
            bg: Color::Rgb(0, 0, 0),
            flags: CellFlags::WIDE | CellFlags::REVERSE,
            ..Cell::default()
        };
        *grid.buffer.cell_mut(0, 1) = Cell {
            c: '\0',
            fg: Color::Rgb(255, 255, 255),
            bg: Color::Rgb(0, 0, 0),
            flags: CellFlags::WIDE_CONT,
            ..Cell::default()
        };
        grid.cursor_col = grid.cols();

        let (frame, cell_dimensions) = render_grid(grid, &mut atlas);

        let width = usize::from(cell_dimensions.0);
        let height = usize::from(cell_dimensions.1);
        let frame_width = width * 2;
        let changed_pixels = (0..height)
            .flat_map(|row| {
                let start = row * frame_width + width;
                frame[start..start + width].iter()
            })
            .filter(|&&pixel| pixel != rgb_to_xrgb(255, 255, 255))
            .count();
        let thickness =
            usize::try_from(super::CURSOR_THICKNESS).expect("cursor thickness should fit usize");
        let expected = match shape {
            CursorShape::Block => width * height,
            CursorShape::Bar => thickness.min(width) * height,
            CursorShape::Underline => width * thickness.min(height),
        };
        assert_eq!(changed_pixels, expected, "{shape:?}");
    }
}

#[test]
fn empty_grid_cursor_is_visible_and_follows_each_cursor_shape() {
    let background = rgb_to_xrgb(
        DEFAULT_BACKGROUND.0,
        DEFAULT_BACKGROUND.1,
        DEFAULT_BACKGROUND.2,
    );
    let mut atlas = test_atlas();

    for shape in [CursorShape::Block, CursorShape::Bar, CursorShape::Underline] {
        let mut grid = Grid::new(1, 1, 0);
        grid.cursor_style.shape = shape;
        let (frame, cell_dimensions) = render_grid(grid, &mut atlas);
        let width = usize::from(cell_dimensions.0);
        let height = usize::from(cell_dimensions.1);
        let thickness = usize::try_from(super::CURSOR_THICKNESS)
            .expect("cursor thickness should fit usize")
            .min(if shape == CursorShape::Bar {
                width
            } else {
                height
            });
        let expected_changed = match shape {
            CursorShape::Block => width * height,
            CursorShape::Bar => thickness * height,
            CursorShape::Underline => width * thickness,
        };

        assert_eq!(
            frame.iter().filter(|&&pixel| pixel != background).count(),
            expected_changed
        );
    }

    let mut hidden_grid = Grid::new(1, 1, 0);
    hidden_grid.cursor_visible = false;
    let (hidden_frame, _) = render_grid(hidden_grid, &mut atlas);
    assert!(hidden_frame.iter().all(|&pixel| pixel == background));

    let mut pending_wrap_grid = Grid::new(1, 1, 0);
    pending_wrap_grid.cursor_col = pending_wrap_grid.cols();
    let (pending_wrap_frame, _) = render_grid(pending_wrap_grid, &mut atlas);
    assert!(pending_wrap_frame.iter().any(|&pixel| pixel != background));

    let mut invalid_cursor_grid = Grid::new(1, 1, 0);
    invalid_cursor_grid.cursor_col = invalid_cursor_grid.cols() + 1;
    let (invalid_cursor_frame, _) = render_grid(invalid_cursor_grid, &mut atlas);
    assert!(invalid_cursor_frame
        .iter()
        .all(|&pixel| pixel == background));
}

#[cfg(test)]
mod themed_cursor_tests {
    use super::*;

    const CURSOR: (u8, u8, u8) = (45, 210, 140);

    fn atlas() -> GlyphAtlas {
        GlyphAtlas::new("kokuban-test-font-that-does-not-exist", 14.0, 1.0).unwrap()
    }

    fn colors() -> TerminalColors {
        TerminalColors::new((220, 130, 85), (7, 25, 55)).with_cursor(CURSOR)
    }

    fn render(snapshot: &GridSnapshot, atlas: &mut GlyphAtlas, cursor: bool) -> Vec<u32> {
        let dimensions = atlas_cell_dimensions(atlas).unwrap();
        let size = (
            snapshot.columns as u32 * u32::from(dimensions.0),
            snapshot.rows as u32 * u32::from(dimensions.1),
        );
        let mut frame = vec![rgb_to_xrgb(7, 25, 55); (size.0 * size.1) as usize];
        draw_grid_snapshot(
            &mut frame,
            size,
            atlas,
            colors(),
            dimensions,
            snapshot,
            cursor,
        );
        frame
    }

    #[test]
    fn opaque_block_cursor_preserves_ascii_combining_and_wide_grapheme_ink() {
        let mut atlas = atlas();
        let (width, height) = atlas_cell_dimensions(&atlas).unwrap();
        let (width, height) = (usize::from(width), usize::from(height));
        let cursor_rgb = rgb_to_xrgb(CURSOR.0, CURSOR.1, CURSOR.2);
        for text in ["M", "e\u{301}", "日", "👩🏽\u{200d}💻"] {
            let mut grid = Grid::new(4, 2, 0);
            grid.cursor_row = 1;
            for character in text.chars() {
                grid.put_char(character);
            }
            let mut snapshot = snapshot_locked_grid(&grid);
            let columns = if snapshot.cell(1, 0).unwrap().flags.contains(CellFlags::WIDE) {
                2
            } else {
                1
            };
            let baseline = render(&snapshot, &mut atlas, false);
            let mut grapheme_ink_pixels = 0;
            for column in 0..columns {
                snapshot.cursor.as_mut().unwrap().column = column;
                atlas.dirty = false;
                let actual = render(&snapshot, &mut atlas, true);
                assert!(
                    !atlas.dirty,
                    "cursor should reuse the cached {text} grapheme"
                );

                // Independent oracle: ordinary rendering with reversed cursor
                // colors supplies the expected ink, without the cursor helper.
                let mut reference = snapshot_locked_grid(&grid);
                for cell in &mut reference.cells {
                    cell.fg = Color::Rgb(7, 25, 55);
                    cell.bg = Color::Rgb(CURSOR.0, CURSOR.1, CURSOR.2);
                }
                let ink = render(&reference, &mut atlas, false);
                let mut expected = baseline.clone();
                let mut ink_pixels = 0;
                for y in height..height * 2 {
                    for x in column * width..(column + 1) * width {
                        let index = y * width * 4 + x;
                        expected[index] = ink[index];
                        ink_pixels += usize::from(ink[index] != cursor_rgb);
                    }
                }
                grapheme_ink_pixels += ink_pixels;
                assert_eq!(
                    actual, expected,
                    "cursor erased ink or repainted outside {text} cell {column}"
                );

                // A partial repaint clips against both the cursor and the
                // damaged row band, preserving the untouched buffer pixels.
                let frame_size = ((width * 4) as u32, (height * 2) as u32);
                let band = (height + height / 3) as u32..(height * 2 - 2) as u32;
                let start = band.start as usize * width * 4;
                let end = band.end as usize * width * 4;
                let mut partial = baseline.clone();
                partial[start..end].fill(rgb_to_xrgb(7, 25, 55));
                draw_grid_snapshot_in_band(
                    &mut partial,
                    frame_size,
                    &mut atlas,
                    &colors(),
                    (width as u16, height as u16),
                    &snapshot,
                    true,
                    &[],
                    band,
                );
                assert_eq!(
                    &partial[start..end],
                    &actual[start..end],
                    "damaged cursor band: {text}"
                );
                assert_eq!(&partial[..start], &baseline[..start]);
                assert_eq!(&partial[end..], &baseline[end..]);
            }
            // A missing CJK font may draw a narrow replacement glyph inside a
            // two-cell cluster. Both halves still require exact pixel equality.
            assert!(
                grapheme_ink_pixels > 0,
                "fixture must contain ink for {text}"
            );
        }
    }

    #[test]
    fn opaque_block_cursor_preserves_ink_in_both_halves_of_a_synthetic_wide_glyph() {
        let mut atlas = atlas();
        let (cell_width, cell_height) = atlas_cell_dimensions(&atlas).unwrap();
        let (width, height) = (usize::from(cell_width), usize::from(cell_height));
        let glyph = crate::glyph_atlas::GlyphEntry {
            atlas_x: atlas.width.checked_sub(u32::from(cell_width) * 2).unwrap(),
            atlas_y: atlas.height.checked_sub(u32::from(cell_height)).unwrap(),
            pixel_w: u32::from(cell_width) * 2,
            pixel_h: u32::from(cell_height),
            bearing_x: 0,
            bearing_y: -rounded_f64_i32(f64::from(atlas.ascent)).unwrap(),
            color: false,
        };
        assert!(!atlas.is_color(glyph));
        // Distinct stripes in each half expose a wrong source origin when the
        // cursor is on WIDE_CONT, without requiring an installed CJK font.
        let has_ink = |x: usize, y: usize| {
            let stripe = if x < width { width / 3 } else { width * 2 / 3 };
            x % width == stripe || y == height / 2
        };
        for y in 0..height {
            for x in 0..width * 2 {
                let index = (glyph.atlas_y as usize + y) * atlas.width as usize
                    + glyph.atlas_x as usize
                    + x;
                atlas.pixels[index] = if has_ink(x, y) { 255 } else { 0 };
            }
        }
        atlas.glyphs.insert(
            GlyphKey {
                c: '日',
                bold: false,
                italic: false,
            },
            glyph,
        );
        let mut grid = Grid::new(4, 2, 0);
        grid.cursor_row = 1;
        grid.put_char('日');
        let mut snapshot = snapshot_locked_grid(&grid);
        assert!(snapshot
            .cell(1, 1)
            .unwrap()
            .flags
            .contains(CellFlags::WIDE_CONT));
        let baseline = render(&snapshot, &mut atlas, false);
        for column in 0..2 {
            snapshot.cursor.as_mut().unwrap().column = column;
            let mut expected = baseline.clone();
            let mut ink_pixels = 0;
            for y in 0..height {
                for x in column * width..(column + 1) * width {
                    let ink = has_ink(x, y);
                    ink_pixels += usize::from(ink);
                    expected[(height + y) * width * 4 + x] = if ink {
                        rgb_to_xrgb(7, 25, 55)
                    } else {
                        rgb_to_xrgb(CURSOR.0, CURSOR.1, CURSOR.2)
                    };
                }
            }
            assert!(
                ink_pixels > 0,
                "synthetic glyph must cover wide cell {column}"
            );
            atlas.dirty = false;
            let actual = render(&snapshot, &mut atlas, true);
            assert!(!atlas.dirty, "synthetic glyph must stay cached");
            assert_eq!(
                actual, expected,
                "wide cell {column}: lost ink or modified neighboring pixels"
            );
            let size = ((width * 4) as u32, (height * 2) as u32);
            let band = (height + height / 3) as u32..(height * 2 - 2) as u32;
            let start = band.start as usize * width * 4;
            let end = band.end as usize * width * 4;
            let mut partial = baseline.clone();
            partial[start..end].fill(rgb_to_xrgb(7, 25, 55));
            draw_grid_snapshot_in_band(
                &mut partial,
                size,
                &mut atlas,
                &colors(),
                (cell_width, cell_height),
                &snapshot,
                true,
                &[],
                band,
            );
            assert_eq!(
                &partial[start..end],
                &expected[start..end],
                "wide cell {column}: damaged band"
            );
            assert_eq!(&partial[..start], &baseline[..start]);
            assert_eq!(&partial[end..], &baseline[end..]);
        }
    }

    #[test]
    fn opaque_cursor_uses_readable_text_when_cell_background_matches_cursor() {
        let mut atlas = atlas();
        let mut grid = Grid::new(1, 1, 0);
        grid.bg = Color::Rgb(CURSOR.0, CURSOR.1, CURSOR.2);
        grid.put_char('M');
        grid.cursor_col = 0;
        let snapshot = snapshot_locked_grid(&grid);
        let actual = render(&snapshot, &mut atlas, true);
        let mut reference = snapshot_locked_grid(&grid);
        reference.cells[0].fg = Color::Rgb(7, 25, 55);
        let expected = render(&reference, &mut atlas, false);
        assert_eq!(actual, expected);
        assert!(actual
            .iter()
            .any(|&pixel| pixel != rgb_to_xrgb(CURSOR.0, CURSOR.1, CURSOR.2)));
    }

    #[test]
    fn opaque_cursor_does_not_reveal_concealed_wide_or_combining_content() {
        let mut atlas = atlas();
        let (width, height) = atlas_cell_dimensions(&atlas).unwrap();
        let (width, height) = (usize::from(width), usize::from(height));
        let cursor_rgb = rgb_to_xrgb(CURSOR.0, CURSOR.1, CURSOR.2);
        for text in ["M", "e\u{301}", "日"] {
            let mut grid = Grid::new(3, 1, 0);
            grid.flags = CellFlags::HIDDEN;
            for character in text.chars() {
                grid.put_char(character);
            }
            grid.cursor_col = usize::from(grid.buffer.cell(0, 0).flags.contains(CellFlags::WIDE));
            let snapshot = snapshot_locked_grid(&grid);
            atlas.dirty = false;
            let actual = render(&snapshot, &mut atlas, true);
            assert!(
                !atlas.dirty,
                "concealed {text} must never enter the glyph atlas"
            );
            let column = snapshot.cursor.unwrap().column;
            for y in 0..height {
                let start = y * width * 3 + column * width;
                assert!(actual[start..start + width]
                    .iter()
                    .all(|&pixel| pixel == cursor_rgb));
            }
        }
    }

    #[test]
    fn cropped_cursor_glyph_preserves_neighbor_pixels_and_rgba_source_coordinates() {
        let pixels = [255; 12];
        let mut rgba = [0; 48];
        for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&[(index * 10) as u8, 20, 30, 255]);
        }
        let glyph = crate::glyph_atlas::GlyphEntry {
            atlas_x: 0,
            atlas_y: 0,
            pixel_w: 4,
            pixel_h: 3,
            bearing_x: 0,
            bearing_y: 0,
            color: false,
        };
        for colored in [false, true] {
            let source = GlyphSource {
                pixels: &pixels,
                rgba_pixels: colored.then_some(&rgba),
                size: (4, 3),
                ascent: 0.0,
            };
            let mut frame = [0xdead_beef; 15];
            draw_glyph_in_cursor(
                &mut frame,
                (4, 3),
                source,
                glyph,
                (-1, 0),
                0x112233,
                ((1, 1), (2, 1)),
            );
            let mut expected = [0xdead_beef; 15];
            expected[5] = if colored {
                rgb_to_xrgb(60, 20, 30)
            } else {
                0x112233
            };
            expected[6] = if colored {
                rgb_to_xrgb(70, 20, 30)
            } else {
                0x112233
            };
            assert_eq!(frame, expected, "colored={colored}");
        }
    }
}

#[cfg(test)]
mod graphics_tests {
    use super::{atlas_cell_dimensions, draw_grid_snapshot_with_images, snapshot_grid};
    use crate::config::ImagesConfig;
    use crate::glyph_atlas::GlyphAtlas;
    use crate::grid::cell::Color;
    use crate::grid::Grid;
    use crate::parser::ansi::GraphicsSupport;
    use crate::software_graphics::SoftwareGraphics;
    use crate::terminal_colors::TerminalColors;
    use crate::terminal_decoder::TerminalDecoder;
    use std::sync::Mutex;

    fn render_images(mut grid: Grid, commands: &[u8]) -> (Vec<u32>, (u16, u16)) {
        let mut atlas = GlyphAtlas::new("kokuban-test-font-that-does-not-exist", 14.0, 1.0)
            .expect("system monospace fallback should be available");
        let cell_size = atlas_cell_dimensions(&atlas).unwrap();
        grid.cell_pixel_width = cell_size.0;
        grid.cell_pixel_height = cell_size.1;
        let mut graphics = SoftwareGraphics::new(&ImagesConfig::default());
        let mut decoder = TerminalDecoder::new(GraphicsSupport {
            kitty: true,
            sixel: true,
        });
        let mut remaining = commands;
        while !remaining.is_empty() {
            let step = decoder.feed_until_event(remaining, &mut grid);
            assert!(step.consumed > 0);
            remaining = &remaining[step.consumed..];
            for event in step.events {
                graphics.process(event, &mut grid);
            }
        }
        let images = graphics.snapshot(&grid, cell_size);
        assert!(
            !images.is_empty(),
            "test must exercise actual image compositing"
        );
        let frame_size = (
            grid.cols() as u32 * u32::from(cell_size.0),
            grid.rows() as u32 * u32::from(cell_size.1),
        );
        let snapshot = snapshot_grid(&Mutex::new(grid)).unwrap();
        let mut frame = vec![0; frame_size.0 as usize * frame_size.1 as usize];
        draw_grid_snapshot_with_images(
            &mut frame,
            frame_size,
            &mut atlas,
            TerminalColors::new((255, 255, 255), (0, 0, 0)),
            cell_size,
            &snapshot,
            false,
            &images,
        );
        (frame, cell_size)
    }

    #[test]
    fn lowest_layer_is_visible_through_default_background_but_below_explicit_background() {
        for (z_index, expected_second_cell) in
            [(-1_073_741_825, 0x0000ff), (-1_073_741_824, 0xff0000)]
        {
            let mut grid = Grid::new(2, 1, 0);
            grid.buffer.cell_mut(0, 1).bg = Color::Rgb(0, 0, 255);
            let command =
                format!("\x1b_Ga=T,f=32,s=1,v=1,i=1,c=2,r=1,C=1,z={z_index};/wAA/w==\x1b\\");
            let (frame, cell_size) = render_images(grid, command.as_bytes());

            for row in frame.chunks_exact(usize::from(cell_size.0) * 2) {
                assert!(row[..usize::from(cell_size.0)]
                    .iter()
                    .all(|pixel| *pixel == 0xff0000));
                assert!(row[usize::from(cell_size.0)..]
                    .iter()
                    .all(|pixel| *pixel == expected_second_cell));
            }
        }
    }

    #[test]
    fn negative_layer_is_below_text_and_zero_layer_covers_text() {
        for z_index in [-1, 0] {
            let mut grid = Grid::new(2, 1, 0);
            grid.buffer.cell_mut(0, 0).c = 'M';
            grid.buffer.cell_mut(0, 0).fg = Color::Rgb(255, 255, 255);
            let command =
                format!("\x1b_Ga=T,f=32,s=1,v=1,i=1,c=2,r=1,C=1,z={z_index};/wAA/w==\x1b\\");
            let (frame, cell_size) = render_images(grid, command.as_bytes());

            let glyph_visible = frame.chunks_exact(usize::from(cell_size.0) * 2).any(|row| {
                row[..usize::from(cell_size.0)]
                    .iter()
                    .any(|pixel| *pixel != 0xff0000)
            });
            assert_eq!(glyph_visible, z_index < 0);
            assert!(frame
                .chunks_exact(usize::from(cell_size.0) * 2)
                .all(|row| row[usize::from(cell_size.0)..]
                    .iter()
                    .all(|pixel| *pixel == 0xff0000)));
        }
    }

    #[test]
    fn overlapping_images_are_sorted_by_z_index_before_compositing() {
        let commands = concat!(
            "\x1b_Ga=T,f=32,s=1,v=1,i=1,c=2,r=1,C=1,z=2;/wAA/w==\x1b\\",
            "\x1b_Ga=T,f=32,s=1,v=1,i=2,c=2,r=1,C=1,z=-1;AP8A/w==\x1b\\",
            "\x1b_Ga=T,f=32,s=1,v=1,i=3,c=2,r=1,C=1,z=1;AAD//w==\x1b\\",
        );
        let (frame, _) = render_images(Grid::new(2, 1, 0), commands.as_bytes());

        assert!(frame.iter().all(|pixel| *pixel == 0xff0000));
    }

    #[test]
    fn equal_z_index_uses_client_image_id_independently_of_transmission_order() {
        for (first, second) in [(20, 10), (10, 20)] {
            let pixel = |id| if id == 20 { "/wAA/w==" } else { "AP8A/w==" };
            let commands = format!(
                "\x1b_Ga=T,f=32,s=1,v=1,i={first},c=2,r=1,C=1,z=0;{}\x1b\\\x1b_Ga=T,f=32,s=1,v=1,i={second},c=2,r=1,C=1,z=0;{}\x1b\\",
                pixel(first), pixel(second),
            );
            let (frame, _) = render_images(Grid::new(2, 1, 0), commands.as_bytes());

            assert!(
                frame.iter().all(|pixel| *pixel == 0xff0000),
                "client image 20 must cover image 10 in either transmission order"
            );
        }
    }
}

#[cfg(test)]
mod builtin_shape_tests {
    use super::*;
    use crate::renderer::box_drawing::light_thickness;
    use crate::selection::{point_from_viewport, SelectionState};

    const FOREGROUND: (u8, u8, u8) = (220, 130, 85);
    const BACKGROUND: (u8, u8, u8) = (7, 25, 55);
    const SELECTION_FOREGROUND: (u8, u8, u8) = (250, 240, 10);
    const SELECTION_BACKGROUND: (u8, u8, u8) = (30, 90, 200);
    const CURSOR: (u8, u8, u8) = (45, 210, 140);

    fn xrgb(color: (u8, u8, u8)) -> u32 {
        rgb_to_xrgb(color.0, color.1, color.2)
    }

    fn atlas(scale: f32) -> GlyphAtlas {
        GlyphAtlas::new("kokuban-test-font-that-does-not-exist", 14.0, scale).unwrap()
    }

    fn colors() -> TerminalColors {
        TerminalColors::new(FOREGROUND, BACKGROUND)
            .with_selection(Some(SELECTION_FOREGROUND), Some(SELECTION_BACKGROUND))
    }

    fn grid(lines: &[&str]) -> Grid {
        let columns = lines.iter().map(|line| line.chars().count()).max().unwrap();
        let mut grid = Grid::new(columns, lines.len(), 0);
        for (row, line) in lines.iter().enumerate() {
            grid.carriage_return();
            grid.cursor_row = row;
            for c in line.chars() {
                grid.put_char(c);
            }
        }
        grid.carriage_return();
        grid.cursor_row = 0;
        grid
    }

    struct Frame {
        pixels: Vec<u32>,
        width: u32,
        cell: (u32, u32),
    }

    impl Frame {
        fn at(&self, x: u32, y: u32) -> u32 {
            self.pixels[(y * self.width + x) as usize]
        }

        /// Pixels of one cell, row by row.
        fn cell_pixels(&self, row: u32, column: u32) -> Vec<u32> {
            let (w, h) = self.cell;
            (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| self.at(column * w + x, row * h + y))
                .collect()
        }
    }

    fn render(
        snapshot: &GridSnapshot,
        atlas: &mut GlyphAtlas,
        colors: &TerminalColors,
        cursor: bool,
    ) -> Frame {
        let dimensions = atlas_cell_dimensions(atlas).unwrap();
        let cell = (u32::from(dimensions.0), u32::from(dimensions.1));
        let size = (
            snapshot.columns as u32 * cell.0,
            snapshot.rows as u32 * cell.1,
        );
        let mut pixels = vec![xrgb(BACKGROUND); (size.0 * size.1) as usize];
        draw_grid_snapshot(
            &mut pixels,
            size,
            atlas,
            *colors,
            dimensions,
            snapshot,
            cursor,
        );
        Frame {
            pixels,
            width: size.0,
            cell,
        }
    }

    /// Expected cell pixels for a block filling rows `ink_rows`.
    fn rows_cell(
        cell: (u32, u32),
        ink_rows: std::ops::Range<u32>,
        ink: u32,
        paper: u32,
    ) -> Vec<u32> {
        (0..cell.1)
            .flat_map(|y| {
                let color = if ink_rows.contains(&y) { ink } else { paper };
                std::iter::repeat_n(color, cell.0 as usize)
            })
            .collect()
    }

    #[test]
    fn blocks_fill_whole_cells_and_lines_join_across_cells_at_scale_1_and_2() {
        for scale in [1.0, 2.0] {
            let mut atlas = atlas(scale);
            let snapshot = snapshot_locked_grid(&grid(&["████", "────", "│▀▄│", "│▄▀│"]));
            let frame = render(&snapshot, &mut atlas, &colors(), false);
            let (w, h) = frame.cell;
            let (fg, bg) = (xrgb(FOREGROUND), xrgb(BACKGROUND));

            // Solid blocks leave no seams between cells.
            for y in 0..h {
                for x in 0..frame.width {
                    assert_eq!(
                        frame.at(x, y),
                        fg,
                        "seam in █ row at ({x}, {y}), scale {scale}"
                    );
                }
            }
            // The horizontal line is one unbroken band of light thickness.
            let thickness = light_thickness(scale);
            let top = (h - thickness) / 2;
            for y in 0..h {
                let expected = if (top..top + thickness).contains(&y) {
                    fg
                } else {
                    bg
                };
                for x in 0..frame.width {
                    assert_eq!(
                        frame.at(x, h + y),
                        expected,
                        "─ at ({x}, {y}), scale {scale}"
                    );
                }
            }
            // Vertical lines continue between rows 2 and 3 in both edge columns.
            let left = (w - thickness) / 2;
            for column in [0, 3] {
                for y in 2 * h..4 * h {
                    for x in 0..w {
                        let expected = if (left..left + thickness).contains(&x) {
                            fg
                        } else {
                            bg
                        };
                        assert_eq!(
                            frame.at(column * w + x, y),
                            expected,
                            "│ at column {column} ({x}, {y}), scale {scale}"
                        );
                    }
                }
            }
            // Half blocks: the upper half is floor(h / 2) rows.
            assert_eq!(
                frame.cell_pixels(2, 1),
                rows_cell(frame.cell, 0..h / 2, fg, bg)
            );
            assert_eq!(
                frame.cell_pixels(2, 2),
                rows_cell(frame.cell, h / 2..h, fg, bg)
            );
            // Stacked ▀ over ▄ and ▄ over ▀ meet without a gap.
            assert_eq!(
                frame.cell_pixels(3, 1),
                rows_cell(frame.cell, h / 2..h, fg, bg)
            );
            assert_eq!(
                frame.cell_pixels(3, 2),
                rows_cell(frame.cell, 0..h / 2, fg, bg)
            );
        }
    }

    #[test]
    fn selection_and_reverse_video_recolor_blocks_like_text() {
        let mut atlas = atlas(1.0);
        let colors = colors();
        let grid = grid(&["▀█▀▄"]);
        let mut snapshot = snapshot_locked_grid(&grid);
        let mut selection = SelectionState::default();
        selection.start(point_from_viewport(&grid, 0, 0));
        selection.update(point_from_viewport(&grid, 0, 1));
        apply_selection_to_snapshot(&mut snapshot, &selection, &grid, &colors);
        snapshot.cells[3].flags.insert(CellFlags::REVERSE);
        let frame = render(&snapshot, &mut atlas, &colors, false);
        let h = frame.cell.1;

        let (selected_ink, selected_paper) =
            (xrgb(SELECTION_FOREGROUND), xrgb(SELECTION_BACKGROUND));
        assert_eq!(
            frame.cell_pixels(0, 0),
            rows_cell(frame.cell, 0..h / 2, selected_ink, selected_paper)
        );
        assert_eq!(
            frame.cell_pixels(0, 1),
            rows_cell(frame.cell, 0..h, selected_ink, selected_paper)
        );
        // Unselected cells keep the ordinary colours; reverse video swaps them.
        let (fg, bg) = (xrgb(FOREGROUND), xrgb(BACKGROUND));
        assert_eq!(
            frame.cell_pixels(0, 2),
            rows_cell(frame.cell, 0..h / 2, fg, bg)
        );
        assert_eq!(
            frame.cell_pixels(0, 3),
            rows_cell(frame.cell, h / 2..h, bg, fg)
        );
    }

    #[test]
    fn opaque_block_cursor_inverts_builtin_shapes() {
        let colors = colors().with_cursor(CURSOR);
        for scale in [1.0, 2.0] {
            let mut atlas = atlas(scale);
            for text in ["▀", "░", "╭", "┼", "█", "╳"] {
                let grid = grid(&[&format!("{text}─"), "  "]);
                let snapshot = snapshot_locked_grid(&grid);
                let baseline = render(&snapshot, &mut atlas, &colors, false);
                let actual = render(&snapshot, &mut atlas, &colors, true);

                // Oracle: the cursor cell drawn normally with the cursor
                // colour as background and the cell background as ink.
                let mut reference = snapshot_locked_grid(&grid);
                reference.cells[0].fg = Color::Rgb(BACKGROUND.0, BACKGROUND.1, BACKGROUND.2);
                reference.cells[0].bg = Color::Rgb(CURSOR.0, CURSOR.1, CURSOR.2);
                let reference = render(&reference, &mut atlas, &colors, false);
                assert_eq!(
                    actual.cell_pixels(0, 0),
                    reference.cell_pixels(0, 0),
                    "cursor over {text} at scale {scale}"
                );
                assert_ne!(
                    actual.cell_pixels(0, 0),
                    baseline.cell_pixels(0, 0),
                    "{text}"
                );
                // Nothing outside the cursor cell changes.
                for (row, column) in [(0, 1), (1, 0), (1, 1)] {
                    assert_eq!(
                        actual.cell_pixels(row, column),
                        baseline.cell_pixels(row, column)
                    );
                }
            }
        }
    }

    #[test]
    fn translucent_cursor_is_drawn_over_builtin_shapes() {
        let mut atlas = atlas(1.0);
        let colors = colors();
        for shape in [CursorShape::Block, CursorShape::Bar, CursorShape::Underline] {
            let grid = grid(&["▄"]);
            let mut snapshot = snapshot_locked_grid(&grid);
            snapshot.cursor.as_mut().unwrap().shape = shape;
            let baseline = render(&snapshot, &mut atlas, &colors, false);
            let actual = render(&snapshot, &mut atlas, &colors, true);

            let mut expected = baseline.pixels.clone();
            let (origin, size) = cursor_rectangle(shape, (0, 0), baseline.cell).unwrap();
            fill_rect(
                &mut expected,
                (baseline.width, baseline.cell.1),
                origin,
                size,
                contrasting_cursor_color(xrgb(BACKGROUND)),
                CURSOR_ALPHA,
            );
            assert_eq!(actual.pixels, expected, "{shape:?} cursor over ▄");
        }
    }

    #[test]
    fn partial_band_repaint_matches_a_full_repaint() {
        let mut atlas = atlas(2.0);
        let colors = colors().with_cursor(CURSOR);
        let snapshot = snapshot_locked_grid(&grid(&["▀╭─╮█", "▒│▚│▄", "▄╰┴╯▀"]));
        let full = render(&snapshot, &mut atlas, &colors, true);
        let h = full.cell.1;
        let width = full.width as usize;
        let frame_size = (full.width, 3 * h);
        let band = h / 2..2 * h + h / 3;
        let (start, end) = (band.start as usize * width, band.end as usize * width);
        let mut partial = full.pixels.clone();
        partial[start..end].fill(xrgb(BACKGROUND));
        draw_grid_snapshot_in_band(
            &mut partial,
            frame_size,
            &mut atlas,
            &colors,
            (full.cell.0 as u16, h as u16),
            &snapshot,
            true,
            &[],
            band,
        );
        assert_eq!(partial, full.pixels);
    }
}
