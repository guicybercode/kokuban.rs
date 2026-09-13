use super::{
    buffer::RowMetadata,
    cell::{Cell, CellFlags, Color, UnderlineStyle},
    history::HistoryRow,
    Grid,
};
use std::sync::Arc;

#[test]
fn short_history_allocates_only_the_known_prefix_but_keeps_the_logical_width() {
    let mut grid = Grid::new(80, 2, 10);
    grid.put_ascii(b"ok ");
    grid.scroll_up(1);
    let row = &grid.scrollback[0];
    assert_eq!(row.materialized_len(), 3);
    assert_eq!(row.len(), 80);
    assert_eq!(grid.retained_row_len(0), 3);
    assert_eq!(grid.scrollback_cells, 80);
    for col in 3..80 {
        assert_eq!(row.get(col), Some(&Cell::default()));
    }
    assert_eq!(row.get(80), None);
    assert_eq!(row.get(usize::MAX), None);
    grid.scroll_viewport_up(1);
    assert_eq!(grid.visible_cell(0, 0).c, 'o');
    assert_eq!(grid.visible_cell(0, 79), &Cell::default());
}

#[test]
fn single_cell_history_uses_inline_storage_for_every_cell_attribute() {
    use super::buffer::Buffer;
    let samples = [
        Cell::default(),
        Cell {
            c: 'x',
            ..Cell::default()
        },
        Cell {
            c: 'e',
            grapheme: Some(Arc::from("e\u{301}")),
            fg: Color::Rgb(1, 2, 3),
            bg: Color::Indexed(4),
            flags: CellFlags::BOLD | CellFlags::ITALIC | CellFlags::HIDDEN,
            underline_style: UnderlineStyle::Curly,
            underline_color: Color::Indexed(5),
        },
    ];
    for cols in [1, 80] {
        for expected in &samples {
            let mut buffer = Buffer::new(cols, 1);
            *buffer.cell_mut(0, 0) = expected.clone();
            let row = buffer.extract_history_row(0);
            assert_eq!(row.materialized_len(), 1);
            assert_eq!(row.allocated_capacity(), 0);
            assert_eq!(row.len(), cols);
            assert_eq!(row.get(0), Some(expected));
            assert_eq!(row.get(cols), None);
            assert_eq!(row.get(usize::MAX), None);
            for col in 1..cols {
                assert_eq!(row.get(col), Some(&Cell::default()));
            }
            assert_eq!(row.clone().into_cells(), buffer.extract_row(0));
            // Reflow trims default cells, while an explicit prefix retains its length.
            let reflowed = HistoryRow::from_cells(row.into_cells());
            assert_eq!(
                reflowed.materialized_len(),
                usize::from(expected != &Cell::default())
            );
            assert_eq!(reflowed.allocated_capacity(), 0);
            assert_eq!(reflowed.clone().into_cells(), buffer.extract_row(0));
        }
    }
    let mut unknown = Buffer::new(80, 1);
    unknown.row_mut(0)[0].c = 'x';
    let row = unknown.extract_history_row(0);
    assert_eq!(
        row.materialized_len(),
        80,
        "unknown suffix must keep the full-row fallback"
    );
    assert!(row.allocated_capacity() >= 80);
}

#[test]
fn single_cell_history_clones_keep_shared_text_alive_until_materialized_cells_drop() {
    use super::buffer::Buffer;
    let text: Arc<str> = Arc::from("e\u{301}");
    let owner = Arc::downgrade(&text);
    let mut buffer = Buffer::new(80, 1);
    *buffer.cell_mut(0, 0) = Cell {
        c: 'e',
        grapheme: Some(text),
        bg: Color::Indexed(3),
        ..Cell::default()
    };
    let row = buffer.extract_history_row(0);
    let snapshot = row.clone();
    buffer.clear_row(0, Cell::default());
    drop(row);
    assert_eq!(owner.strong_count(), 1);
    assert_eq!(snapshot.get(0).unwrap().text(), "e\u{301}");
    let cells = snapshot.into_cells();
    assert_eq!(cells.len(), 80);
    assert_eq!(cells[0].bg, Color::Indexed(3));
    assert!(cells[1..].iter().all(|cell| cell == &Cell::default()));
    assert_eq!(owner.strong_count(), 1);
    let row = HistoryRow::from_cells(cells);
    assert_eq!(row.materialized_len(), 1);
    assert_eq!(row.allocated_capacity(), 0);
    assert_eq!(owner.strong_count(), 1);
    drop(row);
    assert!(owner.upgrade().is_none());
}

#[test]
fn blank_history_still_consumes_the_logical_cell_budget_for_soft_wraps() {
    for maximum in [0, 1, 3] {
        let mut grid = Grid::new(80, 2, maximum);
        for _ in 0..20 {
            grid.buffer.set_row_metadata(
                0,
                RowMetadata {
                    len: 0,
                    wrapped: true,
                },
            );
            grid.scroll_up(1);
        }
        assert_eq!(grid.scrollback.len(), maximum);
        assert_eq!(grid.scrollback_cells, maximum * 80);
        assert_eq!(grid.scrollback_hard_lines, 0);
        assert!(grid
            .scrollback
            .iter()
            .all(|row| row.materialized_len() == 0 && row.allocated_capacity() == 0));
    }
}

#[test]
fn styled_blank_suffixes_keep_every_cell_attribute() {
    let templates = [
        Cell {
            fg: Color::Indexed(7),
            ..Cell::default()
        },
        Cell {
            bg: Color::Rgb(1, 2, 3),
            ..Cell::default()
        },
        Cell {
            flags: CellFlags::HIDDEN,
            ..Cell::default()
        },
        Cell {
            underline_style: UnderlineStyle::Curly,
            ..Cell::default()
        },
        Cell {
            underline_color: Color::Indexed(4),
            ..Cell::default()
        },
        Cell {
            grapheme: Some(Arc::from(" \u{301}")),
            ..Cell::default()
        },
    ];
    for template in templates {
        let mut grid = Grid::new(80, 2, 10);
        grid.buffer.clear_row(0, template.clone());
        grid.put_ascii(b"ok");
        let expected = grid.buffer.extract_row(0);
        grid.scroll_up(1);
        assert_eq!(grid.scrollback[0].materialized_len(), 80);
        assert_eq!(grid.scrollback[0].clone().into_cells(), expected);
        assert_eq!(grid.scrollback_cell_data(0, 79), &template);
    }
}

#[test]
fn immutable_history_keeps_wide_pairs_and_graphemes_after_buffer_reuse_and_eviction() {
    let mut grid = Grid::new(80, 2, 1);
    grid.put_char('日');
    grid.put_char('e');
    grid.put_char('\u{301}');
    grid.scroll_up(1);
    assert_eq!(grid.scrollback[0].materialized_len(), 3);
    assert!(grid
        .scrollback_cell_data(0, 0)
        .flags
        .contains(CellFlags::WIDE));
    assert!(grid
        .scrollback_cell_data(0, 1)
        .flags
        .contains(CellFlags::WIDE_CONT));
    let snapshot = grid.scrollback[0].clone();
    let grapheme = snapshot.get(2).unwrap().grapheme.clone().unwrap();
    grid.put_ascii(b"replacement");
    grid.scroll_up(2);
    assert_eq!(snapshot.get(0).unwrap().c, '日');
    assert_eq!(grapheme.as_ref(), "e\u{301}");
    assert_eq!(snapshot.get(2).unwrap().text(), "e\u{301}");
    assert_eq!(grid.scrollback_cells, 80);
}

#[test]
fn compact_extraction_matches_full_rows_after_circular_and_partial_scrolls() {
    use super::buffer::Buffer;
    let mut buffer = Buffer::new(17, 5);
    for step in 0..128 {
        let row = step % 5;
        let col = (step * 7) % 17;
        let template = Cell {
            bg: if step % 3 == 0 {
                Color::Indexed(4)
            } else {
                Color::Default
            },
            ..Cell::default()
        };
        buffer.clear_row(row, template.clone());
        *buffer.cell_mut(row, col) = Cell {
            c: 'e',
            grapheme: Some(Arc::from("e\u{301}")),
            ..Cell::default()
        };
        if step % 2 == 0 {
            buffer.scroll_up(0, 4, step % 5, template);
        } else {
            buffer.scroll_down(1, 3, step % 3, template);
        }
        for row in 0..5 {
            assert_eq!(
                buffer.extract_history_row(row).into_cells(),
                buffer.extract_row(row),
                "step={step}, row={row}"
            );
        }
    }
    let empty = Buffer::new(0, 1).extract_history_row(0);
    assert_eq!(empty.len(), 0);
    assert_eq!(empty.get(0), None);
}

#[test]
fn history_compaction_preserves_spaces_recorded_beyond_the_stored_prefix() {
    for (text, prefix) in [(b"x   ".as_slice(), 1), (b"ok   ".as_slice(), 2)] {
        let mut grid = Grid::new(80, 1, 10);
        grid.put_ascii(text);
        grid.scroll_up(1);
        grid.resize(40, 1);
        assert_eq!(grid.scrollback[0].materialized_len(), prefix);
        assert_eq!(grid.retained_row_len(0), text.len());
        assert_eq!(grid.scrollback_cells, 40);
        grid.enter_alt_screen();
        grid.resize(3, 1);
        grid.resize(80, 1);
        grid.leave_alt_screen();
        assert_eq!(grid.scrollback[0].materialized_len(), prefix);
        assert_eq!(grid.retained_row_len(0), text.len());
        assert_eq!(grid.scrollback_cells, 80);
        if prefix == 1 {
            assert_eq!(grid.scrollback[0].allocated_capacity(), 0);
        }
        for col in prefix..text.len() {
            assert_eq!(grid.retained_cell_data(0, col), &Cell::default());
        }
    }
}

// Compare compact and fully materialized representations through the same
// public operations, including cold paths that move rows back into the buffer.
fn materialize_history(grid: &mut Grid) {
    fn materialize(rows: &mut std::collections::VecDeque<HistoryRow>) {
        for row in rows {
            let cells = row.clone().into_cells();
            *row = HistoryRow::from_prefix(cells, row.len());
        }
    }
    materialize(&mut grid.scrollback);
    if let Some(saved) = &mut grid.saved_primary_history {
        materialize(&mut saved.cells);
    }
}

fn logical_snapshot(grid: &Grid) -> String {
    let rows: Vec<_> = (0..grid.retained_rows())
        .map(|row| {
            let cells: Vec<_> = (0..grid.cols())
                .map(|col| grid.retained_cell_data(row, col))
                .collect();
            (cells, grid.retained_row_metadata(row))
        })
        .collect();
    format!(
        "{rows:?} {:?} {:?} {:?} {:?}",
        (
            grid.cursor_row,
            grid.cursor_col,
            grid.wrap_pending,
            grid.saved_cursor_row,
            grid.saved_cursor_col,
            grid.saved_cursor_retained_row
        ),
        (
            grid.scrollback_cells,
            grid.scrollback_hard_lines,
            grid.scrollback_cell_budget,
            grid.total_lines_pushed
        ),
        (
            grid.scroll_offset,
            grid.selection_revision,
            grid.screen_revision,
            grid.using_alt_screen
        ),
        grid.image_placements,
    )
}

#[test]
fn compact_history_matches_full_history_through_resize_restore_and_screen_switches() {
    let mut compact = Grid::new(80, 4, 9);
    let mut full = Grid::new(80, 4, 9);
    for step in 0..180 {
        for grid in [&mut compact, &mut full] {
            match step % 18 {
                0 => {
                    grid.put_ascii(b"short   ");
                    grid.carriage_return();
                    grid.newline();
                }
                1 => {
                    grid.put_char('日');
                    grid.put_char('e');
                    grid.put_char('\u{301}');
                }
                2 => grid.save_cursor(),
                3 => grid.scroll_up(3),
                4 => grid.resize(5, 2),
                5 => grid.restore_cursor(),
                6 => grid.enter_alt_screen(),
                7 => {
                    grid.put_ascii(b"alt");
                    grid.scroll_up(1);
                }
                8 => grid.resize(1, 3),
                9 => grid.resize(40, 4),
                10 => grid.leave_alt_screen(),
                11 => grid.restore_cursor(),
                12 => {
                    grid.bg = Color::Indexed(3);
                    grid.erase_in_line(2);
                }
                13 => {
                    grid.bg = Color::Default;
                    grid.scroll_up(2);
                }
                14 => grid.resize(80, 2),
                15 => grid.scroll_viewport_up(2),
                16 => grid.put_ascii(b"end "),
                _ => grid.scroll_up(1),
            }
        }
        materialize_history(&mut full);
        assert_eq!(
            logical_snapshot(&compact),
            logical_snapshot(&full),
            "step={step}"
        );
    }
}
