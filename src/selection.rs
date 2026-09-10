use crate::grid::{cell::CellFlags, Grid};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridPoint {
    pub row: i64,
    pub col: usize,
}

/// Map a pointer cell to retained-history coordinates. Dragging outside the
/// viewport clamps to its edge, and either half of a wide glyph selects its
/// leader. The coordinates remain compatible with the macOS selection path.
pub fn point_from_viewport(grid: &Grid, vis_row: usize, col: usize) -> GridPoint {
    let vis_row = vis_row.min(grid.rows() - 1);
    let col = wide_leader(grid, viewport_row(grid, vis_row), col.min(grid.cols() - 1));
    GridPoint {
        row: row_as_i64(viewport_row(grid, vis_row)),
        col,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionTextError {
    TooLarge,
}

#[derive(Debug, Default)]
pub struct SelectionState {
    anchor: Option<GridPoint>,
    end: Option<GridPoint>,
}

impl SelectionState {
    pub fn start(&mut self, point: GridPoint) {
        self.anchor = Some(point);
        self.end = Some(point);
    }

    pub fn update(&mut self, point: GridPoint) {
        self.end = Some(point);
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.end = None;
    }

    /// Rebase retained-history coordinates after rows are evicted. A selection
    /// partially in lost history keeps its surviving portion; one entirely in
    /// lost history is cleared. The caller must clear selection separately on
    /// grid resets, screen switches, or other changes of coordinate epoch.
    pub fn rebase_after_eviction(&mut self, dropped_rows: usize) -> bool {
        if dropped_rows == 0 || !self.is_active() {
            return false;
        }
        let Ok(dropped_rows) = i64::try_from(dropped_rows) else {
            self.clear();
            return true;
        };
        for point in [&mut self.anchor, &mut self.end].into_iter().flatten() {
            point.row = point.row.saturating_sub(dropped_rows);
        }
        if self.normalized().is_some_and(|(_, end)| end.row < 0) {
            self.clear();
        }
        true
    }

    pub fn is_active(&self) -> bool {
        self.anchor.is_some() && self.end.is_some()
    }

    /// Returns (start, end) normalized so start <= end in reading order.
    pub fn normalized(&self) -> Option<(GridPoint, GridPoint)> {
        let a = self.anchor?;
        let b = self.end?;
        if a.row < b.row || (a.row == b.row && a.col <= b.col) {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    /// Check if a visible (row, col) falls within the selection.
    /// Converts viewport coords to absolute coords using scroll_offset and scrollback_len.
    pub fn contains(
        &self,
        vis_row: usize,
        col: usize,
        scroll_offset: usize,
        scrollback_len: usize,
    ) -> bool {
        let (start, end) = match self.normalized() {
            Some(pair) => pair,
            None => return false,
        };
        let abs_row = row_as_i64(
            scrollback_len
                .saturating_sub(scroll_offset)
                .saturating_add(vis_row),
        );
        if abs_row < start.row || abs_row > end.row {
            return false;
        }
        if abs_row == start.row && abs_row == end.row {
            return col >= start.col && col <= end.col;
        }
        if abs_row == start.row {
            return col >= start.col;
        }
        if abs_row == end.row {
            return col <= end.col;
        }
        true
    }

    /// Highlight both cells of a selected wide glyph. The legacy `contains`
    /// method remains available for renderers that already project their cells.
    pub fn contains_cell(&self, grid: &Grid, vis_row: usize, col: usize) -> bool {
        if vis_row >= grid.rows() || col >= grid.cols() {
            return false;
        }
        let Some((start, end)) = self.normalized() else {
            return false;
        };
        let row = viewport_row(grid, vis_row);
        let abs_row = row_as_i64(row);
        if abs_row < start.row || abs_row > end.row {
            return false;
        }
        let col = wide_leader(grid, row, col);
        let start_col = if abs_row == start.row && start.col < grid.cols() {
            wide_leader(grid, row, start.col)
        } else {
            start.col
        };
        let end_col = if abs_row == end.row && end.col < grid.cols() {
            wide_leader(grid, row, end.col)
        } else {
            end.col
        };
        (abs_row != start.row || col >= start_col) && (abs_row != end.row || col <= end_col)
    }

    /// Extract selected text from the grid.
    pub fn get_text(&self, grid: &Grid) -> String {
        self.get_text_with_limit(grid, usize::MAX)
            .unwrap_or_default()
    }

    /// Copy logical text, joining automatically wrapped rows while preserving
    /// explicit line breaks and printed spaces. Unwritten padding never enters
    /// the result, and the UTF-8 byte limit is checked before each append.
    pub fn get_text_with_limit(
        &self,
        grid: &Grid,
        max_bytes: usize,
    ) -> Result<String, SelectionTextError> {
        let (start, end) = match self.normalized() {
            Some(pair) => pair,
            None => return Ok(String::new()),
        };
        let retained_rows = grid.retained_rows();
        let last_row = row_as_i64(retained_rows - 1);
        if end.row < 0 || start.row > last_row {
            return Ok(String::new());
        }
        let first = start.row.max(0) as usize;
        let last = end.row.min(last_row) as usize;
        let mut text = String::new();
        for row in first..=last {
            let abs_row = row_as_i64(row);
            let mut col_start = if abs_row == start.row {
                start.col.min(grid.cols())
            } else {
                0
            };
            let col_end = if abs_row == end.row {
                end.col.saturating_add(1).min(grid.cols())
            } else {
                grid.cols()
            }
            .min(grid.retained_row_len(row));
            if col_start < grid.cols() {
                col_start = wide_leader(grid, row, col_start);
            }
            for col in col_start..col_end {
                let cell = retained_cell(grid, row, col);
                if cell.flags.contains(CellFlags::WIDE_CONT) || cell.c == '\0' {
                    continue;
                }
                if let Some(grapheme) = cell.grapheme.as_deref() {
                    check_text_budget(&text, grapheme.len(), max_bytes)?;
                    text.push_str(grapheme);
                } else {
                    check_text_budget(&text, cell.c.len_utf8(), max_bytes)?;
                    text.push(cell.c);
                }
            }
            if row != last && !grid.retained_row_wrapped(row) {
                check_text_budget(&text, 1, max_bytes)?;
                text.push('\n');
            }
        }
        Ok(text)
    }
}

fn row_as_i64(row: usize) -> i64 {
    i64::try_from(row).unwrap_or(i64::MAX)
}

fn viewport_row(grid: &Grid, vis_row: usize) -> usize {
    grid.scrollback_len()
        .saturating_sub(grid.scroll_offset)
        .saturating_add(vis_row)
}

fn retained_cell(grid: &Grid, row: usize, col: usize) -> &crate::grid::cell::Cell {
    grid.retained_cell_data(row, col)
}

fn wide_leader(grid: &Grid, row: usize, col: usize) -> usize {
    if col > 0
        && retained_cell(grid, row, col)
            .flags
            .contains(CellFlags::WIDE_CONT)
        && retained_cell(grid, row, col - 1)
            .flags
            .contains(CellFlags::WIDE)
    {
        col - 1
    } else {
        col
    }
}

fn check_text_budget(
    text: &str,
    extra_bytes: usize,
    max_bytes: usize,
) -> Result<(), SelectionTextError> {
    if extra_bytes > max_bytes.saturating_sub(text.len()) {
        Err(SelectionTextError::TooLarge)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{point_from_viewport, GridPoint, SelectionState, SelectionTextError};
    use crate::grid::{cell::CellFlags, Grid};

    fn selected(start: GridPoint, end: GridPoint) -> SelectionState {
        let mut selection = SelectionState::default();
        selection.start(start);
        selection.update(end);
        selection
    }

    fn write_row(grid: &mut Grid, row: usize, text: &str) {
        grid.set_cursor_pos(row, 0);
        for character in text.chars() {
            grid.put_char(character);
        }
    }

    #[test]
    fn normalizes_reverse_drag_order() {
        let mut selection = SelectionState::default();
        selection.start(GridPoint { row: 4, col: 8 });
        selection.update(GridPoint { row: 2, col: 3 });

        let (start, end) = selection.normalized().unwrap();
        assert_eq!((start.row, start.col), (2, 3));
        assert_eq!((end.row, end.col), (4, 8));
    }

    #[test]
    fn copy_joins_wrapped_urls_across_screen_and_history() {
        let url = "https://example.com/very/long/path?query=terminal";
        let mut grid = Grid::new(8, 3, 100);
        grid.put_ascii(url.as_bytes());
        let last = grid.scrollback_len() + grid.cursor_row;
        let selection = selected(
            GridPoint { row: 0, col: 0 },
            GridPoint { row: last as i64, col: 7 },
        );
        assert_eq!(selection.get_text(&grid), url);
        assert_eq!(selection.get_text_with_limit(&grid, url.len()), Ok(url.to_owned()));
        assert_eq!(selection.get_text_with_limit(&grid, url.len() - 1), Err(SelectionTextError::TooLarge));
        let reverse = selected(selection.end.unwrap(), selection.anchor.unwrap());
        assert_eq!(reverse.get_text(&grid), url);
    }

    #[test]
    fn copy_preserves_real_newlines_and_printed_spaces_at_wrap_boundaries() {
        let mut grid = Grid::new(5, 6, 100);
        grid.put_ascii(b"echo  hello  ");
        grid.carriage_return();
        grid.newline();
        grid.put_ascii(b"next ");
        let selection = selected(
            GridPoint { row: 0, col: 0 },
            GridPoint { row: 3, col: 4 },
        );
        assert_eq!(selection.get_text(&grid), "echo  hello  \nnext ");
    }

    #[test]
    fn copy_omits_wide_glyph_wrap_padding_and_keeps_partial_selection() {
        let mut grid = Grid::new(4, 4, 100);
        grid.put_ascii(b"abc");
        grid.put_char('日');
        grid.put_ascii(b"end");
        let selection = selected(
            GridPoint { row: 0, col: 1 },
            GridPoint { row: 2, col: 0 },
        );
        assert_eq!(selection.get_text(&grid), "bc日end");
    }

    #[test]
    fn copy_preserves_wide_history_at_every_resized_width() {
        let mut grid = Grid::new(4, 1, 10);
        grid.put_ascii(b"ab");
        grid.put_char('日');
        grid.put_ascii(b"c");
        for cols in [4, 3, 1, 4] {
            grid.resize(cols, 1);
            let selection = selected(
                GridPoint { row: 0, col: 0 },
                GridPoint { row: (grid.retained_rows() - 1) as i64, col: cols - 1 },
            );
            assert_eq!(selection.get_text(&grid), "ab日c", "width={cols}");
        }
    }

    #[test]
    fn copy_clips_negative_and_extreme_rows_to_the_actual_grid() {
        let mut grid = Grid::new(4, 2, 10);
        write_row(&mut grid, 0, "abcd");
        write_row(&mut grid, 1, "ef");
        let selection = selected(
            GridPoint {
                row: i64::MIN,
                col: usize::MAX,
            },
            GridPoint {
                row: i64::MAX,
                col: usize::MAX,
            },
        );
        assert_eq!(selection.get_text(&grid), "abcd\nef");
        assert_eq!(
            selection.get_text_with_limit(&grid, 7),
            Ok("abcd\nef".to_string())
        );
        for (start, end) in [(i64::MIN, -1), (2, i64::MAX)] {
            let selection = selected(
                GridPoint { row: start, col: 0 },
                GridPoint {
                    row: end,
                    col: usize::MAX,
                },
            );
            assert_eq!(selection.get_text_with_limit(&grid, 0), Ok(String::new()));
        }
    }

    #[test]
    fn copy_clips_columns_without_reading_the_next_physical_row() {
        let mut grid = Grid::new(4, 2, 10);
        write_row(&mut grid, 0, "abcd");
        write_row(&mut grid, 1, "efgh");
        let selection = selected(
            GridPoint { row: 0, col: 2 },
            GridPoint {
                row: 0,
                col: usize::MAX,
            },
        );
        assert_eq!(selection.get_text(&grid), "cd");
        let selection = selected(
            GridPoint {
                row: 0,
                col: usize::MAX,
            },
            GridPoint {
                row: 0,
                col: usize::MAX,
            },
        );
        assert_eq!(selection.get_text(&grid), "");
        let selection = selected(
            GridPoint {
                row: 0,
                col: usize::MAX,
            },
            GridPoint {
                row: 1,
                col: usize::MAX,
            },
        );
        assert_eq!(selection.get_text(&grid), "\nefgh");
    }

    #[test]
    fn viewport_points_clamp_and_snap_wide_continuations() {
        let mut grid = Grid::new(4, 2, 10);
        write_row(&mut grid, 0, "日a");
        assert_eq!(
            point_from_viewport(&grid, 0, 1),
            GridPoint { row: 0, col: 0 }
        );
        assert_eq!(
            point_from_viewport(&grid, usize::MAX, usize::MAX),
            GridPoint { row: 1, col: 3 }
        );
        grid.scroll_up(1);
        grid.scroll_viewport_up(1);
        assert_eq!(
            point_from_viewport(&grid, 0, 1),
            GridPoint { row: 0, col: 0 }
        );
        assert_eq!(
            point_from_viewport(&grid, 1, 0),
            GridPoint { row: 1, col: 0 }
        );
        grid.scroll_to_bottom();
        assert_eq!(
            point_from_viewport(&grid, 0, 0),
            GridPoint { row: 1, col: 0 }
        );
    }

    #[test]
    fn either_half_of_a_wide_character_is_copied_and_highlighted_once() {
        let mut grid = Grid::new(5, 2, 10);
        write_row(&mut grid, 0, "a日b");
        for col in [1, 2] {
            let selection = selected(GridPoint { row: 0, col }, GridPoint { row: 0, col });
            assert_eq!(selection.get_text(&grid), "日");
            assert!(selection.contains_cell(&grid, 0, 1));
            assert!(selection.contains_cell(&grid, 0, 2));
            assert!(!selection.contains_cell(&grid, 0, 0));
            assert!(!selection.contains_cell(&grid, 0, 3));
        }
        let selection = selected(GridPoint { row: 0, col: 3 }, GridPoint { row: 0, col: 2 });
        assert_eq!(selection.get_text(&grid), "日b");
        assert!(!selection.contains_cell(&grid, usize::MAX, usize::MAX));
    }

    #[test]
    fn utf8_byte_budget_rejects_before_copying_past_the_limit() {
        let mut grid = Grid::new(5, 1, 10);
        write_row(&mut grid, 0, "日a");
        let selection = selected(GridPoint { row: 0, col: 0 }, GridPoint { row: 0, col: 4 });
        assert_eq!(
            selection.get_text_with_limit(&grid, 4),
            Ok("日a".to_string())
        );
        assert_eq!(
            selection.get_text_with_limit(&grid, 3),
            Err(SelectionTextError::TooLarge)
        );
        assert_eq!(
            selection.get_text_with_limit(&grid, 0),
            Err(SelectionTextError::TooLarge)
        );
        let blank = Grid::new(1000, 1, 10);
        let selection = selected(GridPoint { row: 0, col: 0 }, GridPoint { row: 0, col: 999 });
        assert_eq!(selection.get_text_with_limit(&blank, 0), Ok(String::new()));
    }

    #[test]
    fn byte_budget_counts_interior_spaces_and_newlines_but_not_trailing_padding() {
        let mut grid = Grid::new(8, 2, 10);
        write_row(&mut grid, 0, "a  b");
        write_row(&mut grid, 1, " c");
        let selection = selected(GridPoint { row: 0, col: 0 }, GridPoint { row: 1, col: 7 });
        assert_eq!(
            selection.get_text_with_limit(&grid, 7),
            Ok("a  b\n c".to_string())
        );
        assert_eq!(
            selection.get_text_with_limit(&grid, 6),
            Err(SelectionTextError::TooLarge)
        );
        let blank = Grid::new(8, 2, 10);
        assert_eq!(
            selection.get_text_with_limit(&blank, 1),
            Ok("\n".to_string())
        );
        assert_eq!(
            selection.get_text_with_limit(&blank, 0),
            Err(SelectionTextError::TooLarge)
        );
    }

    #[test]
    fn copy_preserves_combining_graphemes_and_nonbreaking_spaces() {
        let mut grid = Grid::new(5, 1, 10);
        for character in "e\u{301}\u{a0}".chars() {
            grid.put_char(character);
        }
        let selection = selected(GridPoint { row: 0, col: 0 }, GridPoint { row: 0, col: 4 });
        assert_eq!(
            selection.get_text_with_limit(&grid, 5),
            Ok("e\u{301}\u{a0}".to_string())
        );
        assert_eq!(
            selection.get_text_with_limit(&grid, 4),
            Err(SelectionTextError::TooLarge)
        );
    }

    #[test]
    fn copy_never_splits_a_compound_emoji_or_combining_cluster() {
        for grapheme in ["e\u{301}", "👩🏽‍💻", "🇧🇷", "1\u{fe0f}\u{20e3}", " \u{301}"] {
            let mut grid = Grid::new(8, 2, 10);
            for character in grapheme.chars() {
                grid.put_char(character);
            }
            for col in 0..grid.cursor_col {
                let selection = selected(GridPoint { row: 0, col }, GridPoint { row: 0, col });
                assert_eq!(selection.get_text(&grid), grapheme);
                assert_eq!(selection.get_text_with_limit(&grid, grapheme.len()), Ok(grapheme.to_owned()));
                assert_eq!(selection.get_text_with_limit(&grid, grapheme.len() - 1), Err(SelectionTextError::TooLarge));
            }
        }
    }

    #[test]
    fn orphaned_continuations_and_nuls_never_enter_clipboard_text() {
        let mut grid = Grid::new(3, 1, 10);
        grid.buffer.cell_mut(0, 0).flags = CellFlags::WIDE_CONT;
        grid.buffer.cell_mut(0, 0).c = '\0';
        grid.buffer.cell_mut(0, 1).c = '\0';
        grid.buffer.cell_mut(0, 2).c = 'a';
        let selection = selected(GridPoint { row: 0, col: 0 }, GridPoint { row: 0, col: 2 });
        assert_eq!(selection.get_text(&grid), "a");
    }

    #[test]
    fn eviction_rebasing_preserves_the_same_text_until_its_rows_are_lost() {
        let mut grid = Grid::new(4, 2, 1);
        write_row(&mut grid, 0, "old");
        write_row(&mut grid, 1, "keep");
        grid.scroll_up(1);
        let mut selection = selected(GridPoint { row: 1, col: 0 }, GridPoint { row: 1, col: 3 });
        assert_eq!(selection.get_text(&grid), "keep");
        let dropped_before = grid
            .total_lines_pushed
            .saturating_sub(grid.scrollback_len());
        grid.scroll_up(1);
        let dropped_after = grid
            .total_lines_pushed
            .saturating_sub(grid.scrollback_len());
        assert!(selection.rebase_after_eviction(dropped_after - dropped_before));
        assert_eq!(selection.get_text(&grid), "keep");
        grid.scroll_up(1);
        assert!(selection.rebase_after_eviction(1));
        assert!(!selection.is_active());
        assert!(!selection.rebase_after_eviction(1));
    }

    #[test]
    fn reverse_selection_clips_only_the_part_lost_to_eviction() {
        let mut grid = Grid::new(4, 2, 1);
        write_row(&mut grid, 0, "old");
        write_row(&mut grid, 1, "keep");
        let mut selection = selected(GridPoint { row: 1, col: 3 }, GridPoint { row: 0, col: 2 });
        grid.scroll_up(1);
        grid.scroll_up(1);
        assert!(selection.rebase_after_eviction(1));
        assert_eq!(selection.get_text(&grid), "keep");
        assert!(!selection.rebase_after_eviction(0));
        assert!(selection.rebase_after_eviction(usize::MAX));
        assert!(!selection.is_active());
    }

    #[test]
    fn legacy_contains_uses_saturating_viewport_arithmetic() {
        let selection = selected(GridPoint { row: 0, col: 0 }, GridPoint { row: 0, col: 1 });
        assert!(selection.contains(0, 0, usize::MAX, 0));
        assert!(!selection.contains(usize::MAX, 0, 0, usize::MAX));
    }
}
