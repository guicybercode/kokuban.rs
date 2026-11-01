use super::{
    ime_cursor_area, ime_payload_from_event, ime_preedit_payload_with_limits, layout_ime_preedit,
    layout_ime_preedit_for_snapshot, layout_ime_preedit_with_mode, replace_ime_preedit,
    reveal_ime_input_viewport, sync_ime_cursor_area_with, ImeCursorArea, ImePreedit,
    ImePreeditCursor, ImePreeditGlyph, ImePreeditLayout, ImePreeditPayload,
    IME_PREEDIT_REPLACEMENT, MAX_IME_PREEDIT_BYTES, MAX_IME_PREEDIT_RENDER_CELLS,
};
use crate::glyph_atlas::GlyphAtlas;
use crate::grid::cell::{Cell, CellFlags, Color};
use crate::grid::{CursorShape, Grid};
use crate::input::linux::ImeCommitPayload;
use crate::linux_window::dispatch::dispatch_encoded_terminal_input_with;
use crate::linux_window::draw::{
    contrasting_cursor_color, draw_grid_snapshot, draw_ime_preedit, ime_preedit_background_at,
    ime_preedit_glyph_colors, resolve_cell_colors, rgb_to_xrgb, snapshot_grid, CURSOR_THICKNESS,
};
use crate::linux_window::sizing::atlas_cell_dimensions;
use crate::linux_window::test_support::{
    grid_with_scrollback, test_atlas, test_colors, DEFAULT_BACKGROUND, DEFAULT_FOREGROUND,
};
use std::cell::RefCell;
use std::sync::Mutex;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::Ime;

fn accepted_preedit(text: &str, cursor: Option<(usize, usize)>) -> ImePreedit {
    match ime_preedit_payload_with_limits(text.to_string(), cursor, usize::MAX, usize::MAX) {
        ImePreeditPayload::Text(preedit) => preedit,
        payload => panic!("expected accepted IME preedit, got {payload:?}"),
    }
}

fn render_preedit(
    grid: Grid,
    preedit: &ImePreedit,
    atlas: &mut GlyphAtlas,
) -> (Vec<u32>, (u16, u16), ImePreeditLayout) {
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
        .expect("test preedit frame should fit memory");
    let background = rgb_to_xrgb(
        DEFAULT_BACKGROUND.0,
        DEFAULT_BACKGROUND.1,
        DEFAULT_BACKGROUND.2,
    );
    let mut frame = vec![background; frame_len];
    let grid = Mutex::new(grid);
    let snapshot = snapshot_grid(&grid).expect("test grid should not be poisoned");
    let layout = layout_ime_preedit_for_snapshot(preedit, &snapshot);

    draw_grid_snapshot(
        &mut frame,
        frame_size,
        atlas,
        test_colors(),
        cell_dimensions,
        &snapshot,
        false,
    );
    draw_ime_preedit(
        &mut frame,
        frame_size,
        atlas,
        &test_colors(),
        cell_dimensions,
        &snapshot,
        &layout,
    );

    (frame, cell_dimensions, layout)
}

#[test]
fn only_final_ime_commits_become_terminal_payloads() {
    let grid = Mutex::new(Grid::new(8, 2, 0));
    let written = RefCell::new(Vec::new());
    {
        let forward_if_committed = |event| {
            let Some(ImeCommitPayload::Bytes(bytes)) = ime_payload_from_event(event) else {
                return;
            };
            dispatch_encoded_terminal_input_with(&grid, Some(bytes), |bytes| {
                written.borrow_mut().extend_from_slice(&bytes);
                Ok(())
            })
            .expect("fresh Grid should accept committed IME text");
        };

        forward_if_committed(Ime::Enabled);
        forward_if_committed(Ime::Preedit("に".to_string(), Some((3, 3))));
        forward_if_committed(Ime::Preedit(String::new(), None));
        forward_if_committed(Ime::Disabled);
        assert!(
            written.borrow().is_empty(),
            "provisional IME events must never reach the terminal writer"
        );

        forward_if_committed(Ime::Commit("日".to_string()));
    }
    assert_eq!(written.into_inner(), "日".as_bytes());
}

#[test]
fn ime_preedit_preserves_utf8_ranges_and_hides_malformed_cursors() {
    let text = "aé日🙂";
    for boundary in [0, 1, 3, 6, 10] {
        let preedit = accepted_preedit(text, Some((boundary, boundary)));
        assert_eq!(
            preedit.cursor,
            Some(ImePreeditCursor {
                selection_start: boundary,
                selection_end: boundary,
                caret: boundary,
            })
        );
    }

    let reverse = accepted_preedit(text, Some((10, 1)));
    assert_eq!(
        reverse.cursor,
        Some(ImePreeditCursor {
            selection_start: 1,
            selection_end: 10,
            caret: 1,
        }),
        "the second directional endpoint remains the active caret"
    );

    for malformed in [Some((2, 3)), Some((1, 7)), Some((0, 11))] {
        let preedit = accepted_preedit(text, malformed);
        assert_eq!(preedit.text, text);
        assert_eq!(preedit.cursor, None);
    }
    assert_eq!(accepted_preedit(text, None).cursor, None);
}

#[test]
fn ime_preedit_limits_are_atomic_and_never_truncate_utf8() {
    assert_eq!(
        ime_preedit_payload_with_limits(String::new(), Some((0, 0)), 0, 0),
        ImePreeditPayload::Clear
    );

    let exact_bytes = "é".repeat(MAX_IME_PREEDIT_BYTES / "é".len());
    assert_eq!(exact_bytes.len(), MAX_IME_PREEDIT_BYTES);
    assert!(matches!(
        ime_preedit_payload_with_limits(
            exact_bytes.clone(),
            None,
            MAX_IME_PREEDIT_BYTES,
            usize::MAX,
        ),
        ImePreeditPayload::Text(ImePreedit { text, .. }) if text == exact_bytes
    ));
    assert_eq!(
        ime_preedit_payload_with_limits(
            format!("{exact_bytes}x"),
            None,
            MAX_IME_PREEDIT_BYTES,
            usize::MAX,
        ),
        ImePreeditPayload::TooManyBytes {
            byte_count: MAX_IME_PREEDIT_BYTES + 1,
            limit: MAX_IME_PREEDIT_BYTES,
        }
    );

    let exact_cells = "x".repeat(MAX_IME_PREEDIT_RENDER_CELLS);
    assert!(matches!(
        ime_preedit_payload_with_limits(
            exact_cells,
            None,
            usize::MAX,
            MAX_IME_PREEDIT_RENDER_CELLS,
        ),
        ImePreeditPayload::Text(_)
    ));
    assert_eq!(
        ime_preedit_payload_with_limits(
            "x".repeat(MAX_IME_PREEDIT_RENDER_CELLS + 1),
            None,
            usize::MAX,
            MAX_IME_PREEDIT_RENDER_CELLS,
        ),
        ImePreeditPayload::TooManyRenderCells {
            cell_count: MAX_IME_PREEDIT_RENDER_CELLS + 1,
            limit: MAX_IME_PREEDIT_RENDER_CELLS,
        }
    );

    let mut visible = Some(accepted_preedit("old", None));
    assert!(replace_ime_preedit(&mut visible, None));
    assert_eq!(
        visible, None,
        "a rejected update must clear the old overlay"
    );
    assert!(!replace_ime_preedit(&mut visible, None));
}

#[test]
fn ime_preedit_layout_handles_wide_combining_selection_and_controls() {
    let preedit = accepted_preedit("A日🙂\u{301}B", Some((10, 4)));
    let layout = layout_ime_preedit(&preedit, (0, 0), (6, 2));

    assert_eq!(
        layout.glyphs,
        vec![
            ImePreeditGlyph {
                character: 'A',
                byte_start: 0,
                byte_end: 1,
                row: 0,
                column: 0,
                width: 1,
                selected: false,
            },
            ImePreeditGlyph {
                character: '日',
                byte_start: 1,
                byte_end: 4,
                row: 0,
                column: 1,
                width: 2,
                selected: false,
            },
            ImePreeditGlyph {
                character: '🙂',
                byte_start: 4,
                byte_end: 8,
                row: 0,
                column: 3,
                width: 2,
                selected: true,
            },
            ImePreeditGlyph {
                character: '\u{301}',
                byte_start: 8,
                byte_end: 10,
                row: 0,
                column: 3,
                width: 0,
                selected: true,
            },
            ImePreeditGlyph {
                character: 'B',
                byte_start: 10,
                byte_end: 11,
                row: 0,
                column: 5,
                width: 1,
                selected: false,
            },
        ]
    );
    assert_eq!(layout.caret_cell, Some((0, 3)));

    let controls = accepted_preedit("\0\t\n\r\u{1b}\u{7f}", None);
    let control_layout = layout_ime_preedit(&controls, (0, 0), (8, 1));
    assert_eq!(control_layout.glyphs.len(), 6);
    for (column, glyph) in control_layout.glyphs.iter().enumerate() {
        assert_eq!(glyph.character, IME_PREEDIT_REPLACEMENT);
        assert_eq!(glyph.column, column);
        assert_eq!(glyph.width, 1);
        assert_eq!((glyph.byte_start, glyph.byte_end), (column, column + 1));
    }
}

#[test]
fn ime_preedit_cluster_selection_keeps_base_and_combining_colors_coherent() {
    for cursor in [Some((0, 1)), Some((1, 3))] {
        let layout = layout_ime_preedit(&accepted_preedit("e\u{301}", cursor), (0, 0), (4, 1));
        assert_eq!(layout.glyphs.len(), 2);
        assert!(layout.glyphs.iter().all(|glyph| glyph.selected));
        assert!(layout
            .glyphs
            .iter()
            .all(|glyph| (glyph.row, glyph.column) == (0, 0)));
    }

    let unselected =
        layout_ime_preedit(&accepted_preedit("e\u{301}", Some((3, 3))), (0, 0), (4, 1));
    assert!(unselected.glyphs.iter().all(|glyph| !glyph.selected));

    let wide = layout_ime_preedit(&accepted_preedit("日\u{301}", Some((3, 5))), (0, 0), (4, 1));
    assert_eq!(wide.glyphs[0].width, 2);
    assert_eq!(wide.glyphs[1].width, 0);
    assert!(wide.glyphs.iter().all(|glyph| glyph.selected));
    assert!(wide
        .glyphs
        .iter()
        .all(|glyph| (glyph.row, glyph.column) == (0, 0)));
}

#[test]
fn ime_preedit_layout_wraps_without_half_wide_glyphs_and_clips_at_bottom() {
    let ascii = layout_ime_preedit(&accepted_preedit("abc", Some((3, 3))), (0, 3), (4, 3));
    assert_eq!(
        ascii
            .glyphs
            .iter()
            .map(|glyph| (glyph.character, glyph.row, glyph.column))
            .collect::<Vec<_>>(),
        [('a', 0, 3), ('b', 1, 0), ('c', 1, 1)]
    );
    assert_eq!(ascii.caret_cell, Some((1, 2)));

    let wide = layout_ime_preedit(&accepted_preedit("日", Some((3, 3))), (0, 3), (4, 3));
    assert_eq!((wide.glyphs[0].row, wide.glyphs[0].column), (1, 0));
    assert_eq!(wide.glyphs[0].width, 2);
    assert_eq!(wide.caret_cell, Some((1, 2)));

    let pending = layout_ime_preedit(&accepted_preedit("a", Some((1, 1))), (0, 4), (4, 3));
    assert_eq!((pending.glyphs[0].row, pending.glyphs[0].column), (1, 0));

    let pending_start = layout_ime_preedit(&accepted_preedit("a", Some((0, 0))), (0, 4), (4, 3));
    assert_eq!(pending_start.caret_cell, Some((1, 0)));
    assert_eq!(
        (pending_start.glyphs[0].row, pending_start.glyphs[0].column),
        (1, 0),
        "a caret before a wrapped glyph must follow it onto the next line"
    );

    let wide_start = layout_ime_preedit(&accepted_preedit("日", Some((0, 0))), (0, 3), (4, 3));
    assert_eq!(wide_start.caret_cell, Some((1, 0)));
    assert_eq!(
        (wide_start.glyphs[0].row, wide_start.glyphs[0].column),
        (1, 0)
    );

    let clipped = layout_ime_preedit(&accepted_preedit("ab", Some((2, 2))), (1, 3), (4, 2));
    assert_eq!(clipped.glyphs.len(), 1);
    assert_eq!((clipped.glyphs[0].row, clipped.glyphs[0].column), (1, 3));
    assert_eq!(clipped.caret_cell, Some((1, 3)));

    let clipped_wide = layout_ime_preedit(&accepted_preedit("a日", Some((4, 4))), (1, 2), (4, 2));
    assert_eq!(clipped_wide.glyphs.len(), 1);
    assert_eq!(
        (clipped_wide.glyphs[0].row, clipped_wide.glyphs[0].column),
        (1, 2)
    );
    assert_eq!(
        clipped_wide.caret_cell,
        Some((1, 2)),
        "an offscreen wide glyph must anchor the caret to the last visible preedit cell"
    );

    let clipped_wide_at_edge =
        layout_ime_preedit(&accepted_preedit("a日", Some((4, 4))), (1, 3), (4, 2));
    assert_eq!(clipped_wide_at_edge.glyphs.len(), 1);
    assert_eq!(
        (
            clipped_wide_at_edge.glyphs[0].row,
            clipped_wide_at_edge.glyphs[0].column
        ),
        (1, 3)
    );
    assert_eq!(clipped_wide_at_edge.caret_cell, Some((1, 3)));

    let one_column = layout_ime_preedit(&accepted_preedit("日", None), (0, 0), (1, 1));
    assert_eq!(one_column.glyphs[0].character, IME_PREEDIT_REPLACEMENT);
    assert_eq!(one_column.glyphs[0].width, 1);

    let leading_combining = layout_ime_preedit(&accepted_preedit("\u{301}", None), (0, 2), (4, 1));
    assert_eq!(
        (
            leading_combining.glyphs[0].row,
            leading_combining.glyphs[0].column
        ),
        (0, 2)
    );
    assert_eq!(leading_combining.glyphs[0].width, 0);
}

#[test]
fn ime_preedit_obeys_disabled_auto_wrap_at_the_right_margin() {
    let no_wrap = |text: &str, input_cursor| {
        layout_ime_preedit_with_mode(
            &accepted_preedit(text, Some((text.len(), text.len()))),
            input_cursor,
            (4, 2),
            false,
            None,
        )
    };

    let pending = no_wrap("ab", (0, 4));
    assert_eq!(
        pending
            .glyphs
            .iter()
            .map(|glyph| (glyph.character, glyph.row, glyph.column))
            .collect::<Vec<_>>(),
        [('b', 0, 3)]
    );
    assert_eq!(pending.caret_cell, Some((0, 3)));

    let narrow = no_wrap("abc", (0, 2));
    assert_eq!(
        narrow
            .glyphs
            .iter()
            .map(|glyph| (glyph.character, glyph.row, glyph.column))
            .collect::<Vec<_>>(),
        [('a', 0, 2), ('c', 0, 3)]
    );
    assert_eq!(narrow.caret_cell, Some((0, 3)));

    let ignored_wide = no_wrap("a日", (0, 2));
    assert_eq!(ignored_wide.glyphs.len(), 1);
    assert_eq!(ignored_wide.glyphs[0].character, 'a');
    assert_eq!(ignored_wide.caret_cell, Some((0, 3)));

    let overwritten_wide = no_wrap("日x", (0, 2));
    assert_eq!(overwritten_wide.glyphs.len(), 1);
    assert_eq!(overwritten_wide.glyphs[0].character, 'x');
    assert_eq!(
        (
            overwritten_wide.glyphs[0].row,
            overwritten_wide.glyphs[0].column
        ),
        (0, 3)
    );
    assert_eq!(overwritten_wide.cleared_cells, [(0, 2)]);
    assert_eq!(overwritten_wide.caret_cell, Some((0, 3)));

    let ignored_wide_before_combining = no_wrap("日本\u{301}", (0, 2));
    assert_eq!(ignored_wide_before_combining.glyphs.len(), 1);
    assert_eq!(ignored_wide_before_combining.glyphs[0].character, '\u{301}');
    assert_eq!(
        (
            ignored_wide_before_combining.glyphs[0].row,
            ignored_wide_before_combining.glyphs[0].column
        ),
        (0, 3)
    );
    assert_eq!(ignored_wide_before_combining.cleared_cells, [(0, 2)]);

    let ignored_wide_breaks_the_previous_cluster = no_wrap("ab日\u{301}", (0, 2));
    assert_eq!(
        ignored_wide_breaks_the_previous_cluster
            .glyphs
            .iter()
            .map(|glyph| (glyph.character, glyph.row, glyph.column))
            .collect::<Vec<_>>(),
        [('a', 0, 2), ('\u{301}', 0, 3)]
    );
}

#[test]
fn ime_preedit_uses_the_reflowed_cursor_after_growing() {
    let resized_snapshot = |auto_wrap| {
        let mut grid = Grid::new(3, 2, 0);
        for character in ['a', 'b', 'c'] {
            grid.put_char(character);
        }
        grid.resize(5, 2);
        grid.set_auto_wrap(auto_wrap);
        snapshot_grid(&Mutex::new(grid)).expect("resized Grid snapshot should succeed")
    };

    let wrapping = resized_snapshot(true);
    assert_eq!(wrapping.input_cursor, (0, 3));
    assert!(!wrapping.wrap_pending);
    let wrapping_layout = layout_ime_preedit_for_snapshot(&accepted_preedit("X", None), &wrapping);
    assert_eq!(
        wrapping_layout
            .glyphs
            .iter()
            .map(|glyph| (glyph.character, glyph.row, glyph.column))
            .collect::<Vec<_>>(),
        [('X', 0, 3)]
    );

    let overwriting = resized_snapshot(false);
    assert_eq!(overwriting.input_cursor, (0, 3));
    assert!(!overwriting.wrap_pending);
    let overwriting_layout =
        layout_ime_preedit_for_snapshot(&accepted_preedit("X", None), &overwriting);
    assert_eq!(
        overwriting_layout
            .glyphs
            .iter()
            .map(|glyph| (glyph.character, glyph.row, glyph.column))
            .collect::<Vec<_>>(),
        [('X', 0, 3)]
    );
}

#[test]
fn ime_preedit_clears_an_underlying_wide_leader_without_auto_wrap() {
    let mut grid = Grid::new(4, 1, 0);
    grid.set_auto_wrap(false);
    grid.set_cursor_pos(0, 2);
    grid.put_char('日');
    let snapshot = snapshot_grid(&Mutex::new(grid)).expect("snapshot should succeed");

    let layout = layout_ime_preedit_for_snapshot(&accepted_preedit("x", Some((1, 1))), &snapshot);

    assert!(!snapshot.auto_wrap);
    assert_eq!(layout.glyphs.len(), 1);
    assert_eq!(layout.glyphs[0].character, 'x');
    assert_eq!((layout.glyphs[0].row, layout.glyphs[0].column), (0, 3));
    assert_eq!(layout.cleared_cells, [(0, 2)]);
    assert_eq!(layout.caret_cell, Some((0, 3)));

    let combining =
        layout_ime_preedit_for_snapshot(&accepted_preedit("\u{301}", Some((2, 2))), &snapshot);
    assert_eq!(combining.glyphs.len(), 1);
    assert_eq!(combining.glyphs[0].character, '\u{301}');
    assert_eq!(
        (combining.glyphs[0].row, combining.glyphs[0].column),
        (0, 3)
    );
    assert_eq!(combining.cleared_cells, [(0, 2)]);
}

#[test]
fn ime_preedit_colors_project_a_wide_continuation_to_its_leader() {
    let mut grid = Grid::new(4, 1, 0);
    grid.cursor_visible = false;
    grid.set_auto_wrap(false);
    grid.set_cursor_pos(0, 2);
    grid.fg = Color::Rgb(17, 34, 51);
    grid.bg = Color::Rgb(68, 85, 102);
    grid.flags = CellFlags::REVERSE;
    grid.put_char('日');
    let snapshot = snapshot_grid(&Mutex::new(grid)).expect("snapshot should succeed");
    let layout = layout_ime_preedit_for_snapshot(&accepted_preedit("x", None), &snapshot);
    let glyph = layout
        .glyphs
        .first()
        .expect("preedit glyph should be visible");
    assert_eq!((glyph.row, glyph.column), (0, 3));

    let colors = test_colors();
    let leader_colors = resolve_cell_colors(
        &colors,
        snapshot
            .cell(0, 2)
            .expect("wide leader should be available in the snapshot"),
    );
    assert_eq!(
        ime_preedit_glyph_colors(&colors, &snapshot, glyph),
        (leader_colors.foreground, leader_colors.background)
    );
    assert_eq!(
        ime_preedit_background_at(&colors, &snapshot, &ImePreeditLayout::default(), 0, 3,),
        leader_colors.background
    );
}

#[test]
fn ime_preedit_reveals_live_input_and_replaces_provisional_state() {
    let mut history = grid_with_scrollback(3, 5);
    history.scroll_viewport_up(history.scrollback_len());
    let grid = Mutex::new(history);
    let mut state = None;

    assert!(reveal_ime_input_viewport(&grid).expect("fresh Grid should reveal input"));
    assert_eq!(
        grid.lock()
            .expect("preedit reveal should release the Grid lock")
            .scroll_offset,
        0
    );
    assert!(!reveal_ime_input_viewport(&grid).expect("visible input should be a no-op"));

    let first = accepted_preedit("に", Some((3, 3)));
    let second = accepted_preedit("日本", Some((6, 6)));
    assert!(replace_ime_preedit(&mut state, Some(first)));
    let unchanged = state.clone();
    assert!(!replace_ime_preedit(&mut state, unchanged));
    assert!(replace_ime_preedit(&mut state, Some(second.clone())));
    assert_eq!(state, Some(second));
    assert!(replace_ime_preedit(&mut state, None));
    assert_eq!(state, None);
}

#[test]
fn ime_preedit_render_uses_reverse_selection_underline_and_topmost_caret() {
    let grid = Mutex::new(Grid::new(3, 1, 0));
    let snapshot = snapshot_grid(&grid).expect("fresh Grid snapshot should succeed");
    let preedit = accepted_preedit("  ", Some((0, 1)));
    let layout = layout_ime_preedit(&preedit, snapshot.input_cursor, (3, 1));
    assert_eq!(layout.caret_cell, Some((0, 1)));

    let mut atlas = test_atlas();
    let cell_dimensions =
        atlas_cell_dimensions(&atlas).expect("test atlas should have valid metrics");
    let cell_width = u32::from(cell_dimensions.0);
    let cell_height = u32::from(cell_dimensions.1);
    assert!(cell_width > CURSOR_THICKNESS);
    assert!(cell_height > CURSOR_THICKNESS);
    let frame_size = (cell_width * 3, cell_height);
    let mut frame = vec![
        rgb_to_xrgb(
            DEFAULT_BACKGROUND.0,
            DEFAULT_BACKGROUND.1,
            DEFAULT_BACKGROUND.2,
        );
        usize::try_from(frame_size.0 * frame_size.1)
            .expect("small preedit frame should fit usize")
    ];
    draw_grid_snapshot(
        &mut frame,
        frame_size,
        &mut atlas,
        test_colors(),
        cell_dimensions,
        &snapshot,
        false,
    );
    draw_ime_preedit(
        &mut frame,
        frame_size,
        &mut atlas,
        &test_colors(),
        cell_dimensions,
        &snapshot,
        &layout,
    );

    let pixel = |x: u32, y: u32| {
        let index =
            usize::try_from(y * frame_size.0 + x).expect("small preedit pixel should fit usize");
        frame[index]
    };
    let foreground = rgb_to_xrgb(
        DEFAULT_FOREGROUND.0,
        DEFAULT_FOREGROUND.1,
        DEFAULT_FOREGROUND.2,
    );
    let background = rgb_to_xrgb(
        DEFAULT_BACKGROUND.0,
        DEFAULT_BACKGROUND.1,
        DEFAULT_BACKGROUND.2,
    );
    let middle = cell_width / 2;

    assert_eq!(
        pixel(middle, 0),
        foreground,
        "selection uses reverse background"
    );
    assert_eq!(
        pixel(cell_width + middle, 0),
        background,
        "unselected preedit preserves the terminal background"
    );
    assert_eq!(
        pixel(middle, cell_height - 1),
        background,
        "selected preedit remains underlined with its reversed foreground"
    );
    assert_eq!(
        pixel(cell_width + middle, cell_height - 1),
        foreground,
        "unselected preedit has a visible underline"
    );
    assert_eq!(
        pixel(cell_width, 0),
        contrasting_cursor_color(background),
        "the caret bar is drawn after the overlay"
    );
    assert_eq!(
        ime_cursor_area(
            layout
                .caret_cell
                .expect("valid preedit should expose a caret"),
            (snapshot.columns, snapshot.rows),
            cell_dimensions,
            frame_size,
        ),
        Some(ImeCursorArea {
            position: PhysicalPosition::new(
                i32::try_from(cell_width).expect("cell width should fit i32"),
                0,
            ),
            size: PhysicalSize::new(cell_width, cell_height),
        })
    );
}

#[test]
fn ime_preedit_render_preserves_contrast_on_reversed_terminal_cells() {
    let mut grid = Grid::new(1, 1, 0);
    grid.cursor_visible = false;
    grid.buffer.cell_mut(0, 0).flags = CellFlags::REVERSE;
    let mut atlas = test_atlas();

    let (frame, cell_dimensions, layout) =
        render_preedit(grid, &accepted_preedit(" ", None), &mut atlas);

    assert_eq!(layout.caret_cell, None);
    let cell_width = usize::from(cell_dimensions.0);
    let cell_height = usize::from(cell_dimensions.1);
    let pixel = |x: usize, y: usize| frame[y * cell_width + x];
    let foreground = rgb_to_xrgb(
        DEFAULT_FOREGROUND.0,
        DEFAULT_FOREGROUND.1,
        DEFAULT_FOREGROUND.2,
    );
    let background = rgb_to_xrgb(
        DEFAULT_BACKGROUND.0,
        DEFAULT_BACKGROUND.1,
        DEFAULT_BACKGROUND.2,
    );

    assert_eq!(pixel(cell_width / 2, 0), foreground);
    assert_eq!(pixel(cell_width / 2, cell_height - 1), background);
    assert_ne!(
        pixel(cell_width / 2, 0),
        pixel(cell_width / 2, cell_height - 1),
        "the underline must contrast with a reversed cell background"
    );
}

#[test]
fn ime_preedit_render_decorates_zero_width_glyphs_without_advancing_them() {
    let combining_grid = || {
        let mut grid = Grid::new(1, 1, 0);
        grid.cursor_visible = false;
        *grid.buffer.cell_mut(0, 0) = Cell {
            fg: Color::Rgb(17, 34, 51),
            bg: Color::Rgb(68, 85, 102),
            ..Cell::default()
        };
        grid
    };
    let mut atlas = test_atlas();

    let (selected_frame, _, selected_layout) = render_preedit(
        combining_grid(),
        &accepted_preedit("\u{301}", Some((0, 2))),
        &mut atlas,
    );
    assert_eq!(selected_layout.glyphs[0].width, 0);
    assert_eq!(selected_layout.glyphs[0].column, 0);
    let selected_background = rgb_to_xrgb(17, 34, 51);
    assert!(
        selected_frame.contains(&selected_background),
        "a selected combining mark needs a one-cell reverse background"
    );

    let (underlined_frame, cell_dimensions, underlined_layout) = render_preedit(
        combining_grid(),
        &accepted_preedit("\u{301}", None),
        &mut atlas,
    );
    assert_eq!(underlined_layout.glyphs[0].width, 0);
    assert_eq!(underlined_layout.glyphs[0].column, 0);
    let cell_width = usize::from(cell_dimensions.0);
    let cell_height = usize::from(cell_dimensions.1);
    let underline_start = (cell_height - 1) * cell_width;
    assert!(
        underlined_frame[underline_start..]
            .iter()
            .all(|&pixel| pixel == rgb_to_xrgb(17, 34, 51)),
        "a zero-width glyph needs a visible one-cell underline"
    );
}

#[test]
fn ime_preedit_render_hides_the_terminal_cursor_before_a_wrapped_wide_glyph() {
    let mut grid = Grid::new(4, 2, 0);
    grid.cursor_row = 0;
    grid.cursor_col = 3;
    grid.cursor_style.shape = CursorShape::Block;
    let mut atlas = test_atlas();

    let (frame, cell_dimensions, layout) =
        render_preedit(grid, &accepted_preedit("日", Some((0, 0))), &mut atlas);

    assert_eq!(layout.caret_cell, Some((1, 0)));
    assert_eq!((layout.glyphs[0].row, layout.glyphs[0].column), (1, 0));
    let cell_width = usize::from(cell_dimensions.0);
    let cell_height = usize::from(cell_dimensions.1);
    let frame_width = cell_width * 4;
    let pixel = |x: usize, y: usize| frame[y * frame_width + x];
    let background = rgb_to_xrgb(
        DEFAULT_BACKGROUND.0,
        DEFAULT_BACKGROUND.1,
        DEFAULT_BACKGROUND.2,
    );

    assert_eq!(
        pixel(3 * cell_width + cell_width / 2, cell_height / 2),
        background,
        "composition must suppress the stale terminal cursor on the previous line"
    );
    assert_ne!(
        pixel(0, cell_height),
        background,
        "the preedit caret should appear beside the wrapped glyph"
    );
}

#[test]
fn ime_cursor_area_tracks_the_physical_terminal_cell_and_dpi_metrics() {
    assert_eq!(
        ime_cursor_area((0, 0), (80, 24), (10, 20), (800, 480)),
        Some(ImeCursorArea {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(10, 20),
        })
    );
    assert_eq!(
        ime_cursor_area((2, 3), (80, 24), (10, 20), (800, 480)),
        Some(ImeCursorArea {
            position: PhysicalPosition::new(30, 40),
            size: PhysicalSize::new(10, 20),
        })
    );
    assert_eq!(
        ime_cursor_area((2, 3), (80, 24), (20, 40), (1600, 960)),
        Some(ImeCursorArea {
            position: PhysicalPosition::new(60, 80),
            size: PhysicalSize::new(20, 40),
        })
    );
}

#[test]
fn ime_cursor_area_handles_pending_wrap_clipping_and_invalid_geometry() {
    assert_eq!(
        ime_cursor_area((2, 80), (80, 24), (10, 20), (800, 480)),
        Some(ImeCursorArea {
            position: PhysicalPosition::new(790, 40),
            size: PhysicalSize::new(10, 20),
        }),
        "pending wrap should anchor IME at the last terminal cell"
    );
    assert_eq!(
        ime_cursor_area((0, 0), (1, 1), (10, 20), (5, 7)),
        Some(ImeCursorArea {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(5, 7),
        }),
        "an undersized surface should still receive an in-bounds area"
    );
    assert_eq!(
        ime_cursor_area((2, 3), (80, 24), (10, 20), (25, 25)),
        Some(ImeCursorArea {
            position: PhysicalPosition::new(15, 5),
            size: PhysicalSize::new(10, 20),
        }),
        "a cursor beyond a stale surface must clamp the full area in bounds"
    );

    for invalid in [
        ime_cursor_area((0, 0), (0, 24), (10, 20), (800, 480)),
        ime_cursor_area((0, 0), (80, 0), (10, 20), (800, 480)),
        ime_cursor_area((24, 0), (80, 24), (10, 20), (800, 480)),
        ime_cursor_area((0, 81), (80, 24), (10, 20), (800, 480)),
        ime_cursor_area((0, 0), (80, 24), (0, 20), (800, 480)),
        ime_cursor_area((0, 0), (80, 24), (10, 0), (800, 480)),
        ime_cursor_area((0, 0), (80, 24), (10, 20), (0, 480)),
        ime_cursor_area((0, 0), (80, 24), (10, 20), (800, 0)),
    ] {
        assert_eq!(invalid, None);
    }

    let last_fitting_column =
        usize::try_from(i32::MAX).expect("positive i32 should fit usize") / 10;
    assert!(ime_cursor_area(
        (0, last_fitting_column),
        (last_fitting_column + 2, 1),
        (10, 20),
        (u32::MAX, 20),
    )
    .is_some());
    assert_eq!(
        ime_cursor_area(
            (0, last_fitting_column + 1),
            (last_fitting_column + 2, 1),
            (10, 20),
            (u32::MAX, 20),
        ),
        None,
        "IME coordinates beyond i32 must not wrap"
    );
}

#[test]
fn ime_cursor_uses_live_input_position_while_visual_cursor_is_suppressed() {
    let mut history = grid_with_scrollback(3, 5);
    history.cursor_row = 1;
    history.cursor_col = 2;
    history.cursor_visible = false;
    history.scroll_viewport_up(history.scrollback_len());
    let grid = Mutex::new(history);
    let snapshot = snapshot_grid(&grid).expect("history snapshot should succeed");

    assert!(snapshot.cursor.is_none());
    assert_eq!(snapshot.input_cursor, (1, 2));
    let area = ime_cursor_area(
        snapshot.input_cursor,
        (snapshot.columns, snapshot.rows),
        (10, 20),
        (80, 60),
    );
    assert_eq!(
        area,
        Some(ImeCursorArea {
            position: PhysicalPosition::new(20, 20),
            size: PhysicalSize::new(10, 20),
        })
    );

    let mut last_area = None;
    let observed = RefCell::new(None);
    sync_ime_cursor_area_with(true, &mut last_area, area, |area| {
        assert!(
            grid.try_lock().is_ok(),
            "IME window requests must run without the Grid lock"
        );
        observed.replace(Some(area));
    });
    assert_eq!(observed.into_inner(), area);
}

#[test]
fn ime_cursor_area_sync_requires_an_active_session_and_deduplicates_updates() {
    let first = ImeCursorArea {
        position: PhysicalPosition::new(30, 40),
        size: PhysicalSize::new(10, 20),
    };
    let second = ImeCursorArea {
        position: PhysicalPosition::new(40, 40),
        size: PhysicalSize::new(10, 20),
    };
    let calls = RefCell::new(Vec::new());
    let mut last_area = None;

    sync_ime_cursor_area_with(false, &mut last_area, Some(first), |area| {
        calls.borrow_mut().push(area);
    });
    sync_ime_cursor_area_with(true, &mut last_area, Some(first), |area| {
        calls.borrow_mut().push(area);
    });
    sync_ime_cursor_area_with(true, &mut last_area, Some(first), |area| {
        calls.borrow_mut().push(area);
    });
    assert_eq!(calls.borrow().as_slice(), [first]);
    sync_ime_cursor_area_with(true, &mut last_area, Some(second), |area| {
        calls.borrow_mut().push(area);
    });
    sync_ime_cursor_area_with(true, &mut last_area, Some(second), |area| {
        calls.borrow_mut().push(area);
    });
    assert_eq!(calls.borrow().as_slice(), [first, second]);
    assert_eq!(last_area, Some(second));

    // Ime::Enabled invalidates the cache so a recreated input context is synchronized.
    last_area = None;
    sync_ime_cursor_area_with(true, &mut last_area, Some(first), |area| {
        calls.borrow_mut().push(area);
    });
    sync_ime_cursor_area_with(false, &mut last_area, Some(second), |area| {
        calls.borrow_mut().push(area);
    });

    assert_eq!(calls.into_inner(), [first, second, first]);
    assert_eq!(last_area, Some(first));
}
