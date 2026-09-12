use super::cell::Cell;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RowMetadata {
    pub len: usize,
    pub wrapped: bool,
}

#[derive(Debug)]
pub struct Buffer {
    cells: Vec<Cell>,
    // Logical rows point into one cell allocation. Scrolling rotates these
    // offsets instead of copying every cell that remains on screen.
    row_starts: Vec<usize>,
    // All three row vectors share this circular origin. Whole-screen scrolls
    // change it without moving the row mappings or their associated state.
    row_origin: usize,
    // Cells from this column onward are identical to the row's last cell.
    // A frontier at `cols` means the suffix is unknown. Mutable access only
    // moves the frontier right; clearing establishes a uniform whole row.
    uniform_suffix_start: Vec<usize>,
    cols: usize,
    rows: usize,
    metadata: Vec<RowMetadata>,
}

impl Buffer {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols * rows],
            row_starts: (0..rows).map(|row| row * cols).collect(),
            row_origin: 0,
            uniform_suffix_start: vec![0; rows],
            cols,
            rows,
            metadata: vec![RowMetadata::default(); rows],
        }
    }

    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        let row = self.row_index(row);
        &self.cells[self.row_starts[row] + col]
    }

    pub fn cell_mut(&mut self, row: usize, col: usize) -> &mut Cell {
        assert!(col < self.cols);
        let row = self.row_index(row);
        self.uniform_suffix_start[row] = self.uniform_suffix_start[row].max(col + 1);
        &mut self.cells[self.row_starts[row] + col]
    }

    pub fn row_mut(&mut self, row: usize) -> &mut [Cell] {
        self.row_range_mut(row, 0..self.cols)
    }

    pub(crate) fn row_range_mut(&mut self, row: usize, range: std::ops::Range<usize>) -> &mut [Cell] {
        assert!(range.start <= range.end && range.end <= self.cols);
        let row = self.row_index(row);
        if !range.is_empty() {
            self.uniform_suffix_start[row] = self.uniform_suffix_start[row].max(range.end);
        }
        let start = self.row_starts[row];
        &mut self.cells[start + range.start..start + range.end]
    }

    pub(crate) fn row_metadata(&self, row: usize) -> RowMetadata {
        let row = self.row_index(row);
        let mut metadata = self.metadata[row];
        // Public cell access is also used by screen writers and test fixtures.
        // Keep directly assigned nonblank cells in the retained content range.
        let start = self.row_starts[row];
        let suffix = self.uniform_suffix_start[row];
        if suffix < self.cols && self.cells[start + self.cols - 1].c != ' ' {
            metadata.len = metadata.len.max(self.cols);
            return metadata;
        }
        let scan_start = metadata.len.min(suffix);
        if let Some(col) = self.cells[start + scan_start..start + suffix]
            .iter().rposition(|cell| cell.c != ' ')
        {
            metadata.len = metadata.len.max(scan_start + col + 1);
        }
        metadata
    }

    pub(crate) fn set_row_metadata(&mut self, row: usize, metadata: RowMetadata) {
        let row = self.row_index(row);
        self.metadata[row] = metadata;
    }

    pub(crate) fn mark_written(&mut self, row: usize, end: usize) {
        let row = self.row_index(row);
        self.metadata[row].len = self.metadata[row].len.max(end.min(self.cols));
    }

    pub(crate) fn set_wrapped(&mut self, row: usize, wrapped: bool) {
        let row = self.row_index(row);
        self.metadata[row].wrapped = wrapped;
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn clear_row(&mut self, row: usize, template: Cell) {
        let row = self.row_index(row);
        let start = self.row_starts[row];
        let suffix = self.uniform_suffix_start[row];
        let end = if suffix < self.cols && self.cells[start + self.cols - 1] == template {
            suffix
        } else {
            self.cols
        };
        self.cells[start..start + end].fill(template);
        self.metadata[row] = RowMetadata::default();
        self.uniform_suffix_start[row] = 0;
    }

    pub fn scroll_up(&mut self, top: usize, bottom: usize, count: usize, template: Cell) {
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        assert!(top <= bottom && bottom < self.rows);
        if top == 0 && bottom == self.rows - 1 {
            let remaining = self.rows - self.row_origin;
            self.row_origin = if count < remaining {
                self.row_origin + count
            } else {
                count - remaining
            };
        } else {
            self.normalize_rows();
            self.row_starts[top..=bottom].rotate_left(count);
            self.uniform_suffix_start[top..=bottom].rotate_left(count);
            self.metadata[top..=bottom].rotate_left(count);
        }
        for row in bottom + 1 - count..=bottom {
            self.clear_row(row, template.clone());
        }
    }

    pub fn scroll_down(&mut self, top: usize, bottom: usize, count: usize, template: Cell) {
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        assert!(top <= bottom && bottom < self.rows);
        if top == 0 && bottom == self.rows - 1 {
            self.row_origin = if count <= self.row_origin {
                self.row_origin - count
            } else {
                self.rows - (count - self.row_origin)
            };
        } else {
            self.normalize_rows();
            self.row_starts[top..=bottom].rotate_right(count);
            self.uniform_suffix_start[top..=bottom].rotate_right(count);
            self.metadata[top..=bottom].rotate_right(count);
        }
        for row in top..top + count {
            self.clear_row(row, template.clone());
        }
    }

    pub fn extract_row(&self, row: usize) -> Vec<Cell> {
        let row = self.row_index(row);
        let start = self.row_starts[row];
        let cells = &self.cells[start..start + self.cols];
        let suffix = self.uniform_suffix_start[row];
        if suffix == self.cols || cells[self.cols - 1].grapheme.is_some() {
            return cells.to_vec();
        }
        // Preserve the complete row, including styled trailing cells, while
        // reading the known scalar suffix from a single repeated template.
        let mut extracted = Vec::with_capacity(self.cols);
        extracted.extend_from_slice(&cells[..suffix]);
        let template = &cells[self.cols - 1];
        extracted.resize_with(self.cols, || Cell {
            c: template.c,
            grapheme: None,
            fg: template.fg,
            bg: template.bg,
            flags: template.flags,
            underline_style: template.underline_style,
            underline_color: template.underline_color,
        });
        extracted
    }

    pub(crate) fn from_retained_rows(cols: usize, rows: &[super::reflow::RetainedRow]) -> Self {
        let mut buffer = Self::new(cols, rows.len());
        for (index, row) in rows.iter().enumerate() {
            buffer.row_mut(index).clone_from_slice(&row.cells);
            buffer.set_row_metadata(index, row.metadata);
        }
        buffer
    }

    #[inline]
    fn row_index(&self, row: usize) -> usize {
        assert!(row < self.rows);
        let remaining = self.rows - self.row_origin;
        if row < remaining { self.row_origin + row } else { row - remaining }
    }

    fn normalize_rows(&mut self) {
        if self.row_origin == 0 { return; }
        self.row_starts.rotate_left(self.row_origin);
        self.uniform_suffix_start.rotate_left(self.row_origin);
        self.metadata.rotate_left(self.row_origin);
        self.row_origin = 0;
    }

}

#[cfg(test)]
mod tests {
    use super::{Buffer, RowMetadata};
    use crate::grid::cell::{Cell, CellFlags, Color, UnderlineStyle};

    #[test]
    fn whole_screen_scrolls_wrap_without_rotating_row_vectors() {
        for rows in [1, 3, 7] {
            let cols = 9;
            let mut buffer = Buffer::new(cols, rows);
            let mut expected = vec![vec![Cell::default(); cols]; rows];
            let mut metadata = vec![RowMetadata::default(); rows];
            let original_starts = buffer.row_starts.clone();
            for step in 0..96 {
                let row = step % rows;
                let cell = Cell { c: 'e', grapheme: Some(format!("e{}", char::from_u32(0x300 + (step % 16) as u32).unwrap()).into()),
                    fg: Color::Indexed(step as u8), ..Cell::default() };
                *buffer.cell_mut(row, 0) = cell.clone();
                expected[row][0] = cell;
                buffer.mark_written(row, 1 + step % cols);
                metadata[row].len = metadata[row].len.max(1 + step % cols);
                buffer.set_wrapped(row, step % 3 == 0);
                metadata[row].wrapped = step % 3 == 0;
                let count = match step % 7 { 0 => 0, 1 => usize::MAX, 2 => rows, _ => 1 + step % rows };
                let shift = count.min(rows);
                let template = Cell { bg: Color::Indexed((step % 4) as u8), ..Cell::default() };
                if step % 2 == 0 {
                    buffer.scroll_up(0, rows - 1, count, template.clone());
                    expected.rotate_left(shift);
                    metadata.rotate_left(shift);
                    for row in rows - shift..rows {
                        expected[row].fill(template.clone());
                        metadata[row] = RowMetadata::default();
                    }
                } else {
                    buffer.scroll_down(0, rows - 1, count, template.clone());
                    expected.rotate_right(shift);
                    metadata.rotate_right(shift);
                    for row in 0..shift {
                        expected[row].fill(template.clone());
                        metadata[row] = RowMetadata::default();
                    }
                }
                assert_eq!(buffer.row_starts, original_starts);
                assert!(buffer.row_origin < rows);
                for row in 0..rows {
                    assert_eq!(buffer.extract_row(row), expected[row], "rows={rows}, step={step}, row={row}");
                    let actual = buffer.row_metadata(row);
                    assert_eq!((actual.len, actual.wrapped), (metadata[row].len, metadata[row].wrapped));
                }
            }
        }
    }

    #[test]
    fn partial_scrolls_normalize_the_ring_before_editing_the_region() {
        let mut buffer = Buffer::new(8, 7);
        for row in 0..7 {
            buffer.cell_mut(row, 0).c = char::from(b'a' + row as u8);
            buffer.mark_written(row, row + 1);
            buffer.set_wrapped(row, row % 2 == 0);
        }
        buffer.scroll_up(0, 6, 5, Cell::default());
        buffer.scroll_down(0, 6, 2, Cell::default());
        assert_eq!(buffer.row_origin, 3);
        buffer.cell_mut(5, 3).c = 'x';
        buffer.row_range_mut(2, 1..3).fill(Cell { c: 'y', ..Cell::default() });
        buffer.set_row_metadata(2, RowMetadata { len: 6, wrapped: true });
        let before: Vec<_> = (0..7).map(|row| (buffer.extract_row(row), buffer.row_metadata(row))).collect();
        let template = Cell { bg: Color::Rgb(1, 2, 3), flags: CellFlags::REVERSE, ..Cell::default() };
        buffer.scroll_up(1, 5, 2, template.clone());
        assert_eq!(buffer.row_origin, 0);
        for (row, source) in [(0, 0), (1, 3), (2, 4), (3, 5), (6, 6)] {
            assert_eq!(buffer.extract_row(row), before[source].0);
            let actual = buffer.row_metadata(row);
            assert_eq!((actual.len, actual.wrapped), (before[source].1.len, before[source].1.wrapped));
        }
        for row in [4, 5] { assert_eq!(buffer.extract_row(row), vec![template.clone(); 8]); }

        buffer.scroll_down(0, 6, 1, Cell::default());
        assert_eq!(buffer.row_origin, 6);
        let before: Vec<_> = (0..7).map(|row| (buffer.extract_row(row), buffer.row_metadata(row))).collect();
        buffer.scroll_down(2, 5, usize::MAX, template.clone());
        assert_eq!(buffer.row_origin, 0);
        for row in [0, 1, 6] {
            assert_eq!(buffer.extract_row(row), before[row].0);
            let actual = buffer.row_metadata(row);
            assert_eq!((actual.len, actual.wrapped), (before[row].1.len, before[row].1.wrapped));
        }
        for row in 2..=5 { assert_eq!(buffer.extract_row(row), vec![template.clone(); 8]); }
    }

    #[test]
    fn ring_access_rejects_out_of_range_rows_and_keeps_empty_buffers_valid() {
        let mut buffer = Buffer::new(3, 4);
        buffer.scroll_up(0, 3, 3, Cell::default());
        for row in [4, usize::MAX] {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                buffer.cell_mut(row, 0).c = 'x';
            }));
            assert!(result.is_err());
            assert_eq!(buffer.row_origin, 3);
        }
        let mut no_rows = Buffer::new(3, 0);
        no_rows.scroll_up(0, 0, 0, Cell::default());
        no_rows.scroll_down(0, 0, 0, Cell::default());
        assert_eq!(no_rows.row_origin, 0);
        assert!(std::panic::catch_unwind(|| no_rows.extract_row(0)).is_err());

        let mut no_columns = Buffer::new(0, 3);
        no_columns.scroll_up(0, 2, 2, Cell::default());
        no_columns.scroll_down(1, 2, 1, Cell::default());
        for row in 0..3 { assert!(no_columns.extract_row(row).is_empty()); }
    }

    #[test]
    fn ring_scrolls_release_only_removed_graphemes() {
        let mut buffer = Buffer::new(4, 3);
        let mut owners = Vec::new();
        for row in 0..3 {
            let text: std::sync::Arc<str> = format!("e{}", char::from_u32(0x300 + row as u32).unwrap()).into();
            owners.push(std::sync::Arc::downgrade(&text));
            *buffer.cell_mut(row, 0) = Cell { c: 'e', grapheme: Some(text), ..Cell::default() };
        }
        buffer.scroll_up(0, 2, 1, Cell::default());
        assert!(owners[0].upgrade().is_none());
        let snapshot = buffer.extract_row(0);
        buffer.scroll_down(0, 2, 2, Cell::default());
        assert!(owners[2].upgrade().is_none());
        assert!(owners[1].upgrade().is_some());
        buffer.scroll_up(1, 2, usize::MAX, Cell::default());
        assert_eq!(owners[1].strong_count(), 1);
        drop(snapshot);
        assert!(owners[1].upgrade().is_none());
    }

    #[test]
    fn rejected_mutable_access_cannot_invalidate_another_rows_suffix() {
        let mut buffer = Buffer::new(4, 2);
        for range_access in [false, true] {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if range_access {
                    buffer.row_range_mut(0, 0..5).fill(Cell { c: 'x', ..Cell::default() });
                } else {
                    buffer.cell_mut(0, 4).c = 'x';
                }
            }));
            assert!(result.is_err());
            assert_eq!(buffer.uniform_suffix_start, vec![0, 0]);
            buffer.clear_row(1, Cell::default());
            assert_eq!(buffer.extract_row(0), vec![Cell::default(); 4]);
            assert_eq!(buffer.extract_row(1), vec![Cell::default(); 4]);
        }
    }

    #[test]
    fn partial_writes_keep_blank_suffixes_and_release_erased_graphemes() {
        let mut buffer = Buffer::new(12, 2);
        let text: std::sync::Arc<str> = "e\u{301}".into();
        let erased = std::sync::Arc::downgrade(&text);
        *buffer.cell_mut(0, 0) = Cell { c: 'e', grapheme: Some(text), ..Cell::default() };
        buffer.row_range_mut(0, 1..3).fill(Cell { c: 'x', ..Cell::default() });
        buffer.mark_written(0, 5); // Printed spaces remain retained content.
        assert_eq!(buffer.uniform_suffix_start[buffer.row_index(0)], 3);
        assert_eq!(buffer.row_metadata(0).len, 5);
        buffer.clear_row(0, Cell::default());
        assert!(erased.upgrade().is_none());
        assert_eq!(buffer.uniform_suffix_start[buffer.row_index(0)], 0);
        assert_eq!(buffer.extract_row(0), vec![Cell::default(); 12]);

        let styled = Cell { bg: Color::Indexed(5), flags: CellFlags::REVERSE, ..Cell::default() };
        buffer.clear_row(0, styled.clone());
        buffer.cell_mut(0, 2).c = 'x';
        buffer.clear_row(0, Cell::default());
        assert_eq!(buffer.extract_row(0), vec![Cell::default(); 12]);

        buffer.cell_mut(0, 11).c = 'z';
        assert_eq!(buffer.uniform_suffix_start[buffer.row_index(0)], 12);
        assert_eq!(buffer.row_metadata(0).len, 12);
        buffer.clear_row(0, styled.clone());
        assert_eq!(buffer.extract_row(0), vec![styled; 12]);
    }

    #[test]
    fn clearing_uniform_rows_compares_graphemes_and_wide_flags() {
        let mut buffer = Buffer::new(5, 1);
        let first = Cell { c: 'e', grapheme: Some("e\u{301}".into()), ..Cell::default() };
        let second = Cell { grapheme: Some("e\u{308}".into()), ..first.clone() };
        let wide = Cell { flags: CellFlags::WIDE, ..second.clone() };
        let continuation = Cell { flags: CellFlags::WIDE_CONT, ..wide.clone() };
        for template in [first, second, wide, continuation] {
            buffer.clear_row(0, template.clone());
            assert_eq!(buffer.extract_row(0), vec![template; 5]);
            assert_eq!(buffer.row_metadata(0).len, 5);
        }
    }

    #[test]
    fn extracted_rows_preserve_styles_and_own_their_compound_prefix() {
        let mut buffer = Buffer::new(8, 1);
        let template = Cell {
            fg: Color::Rgb(1, 2, 3), bg: Color::Indexed(4), flags: CellFlags::REVERSE,
            underline_style: UnderlineStyle::Curly, underline_color: Color::Indexed(5),
            ..Cell::default()
        };
        buffer.clear_row(0, template.clone());
        let text: std::sync::Arc<str> = "e\u{301}".into();
        let retained = std::sync::Arc::downgrade(&text);
        *buffer.cell_mut(0, 0) = Cell { c: 'e', grapheme: Some(text), ..template.clone() };
        let extracted = buffer.extract_row(0);
        buffer.clear_row(0, Cell::default());
        assert_eq!(extracted.len(), 8);
        assert_eq!(extracted[0].text(), "e\u{301}");
        assert!(extracted[1..].iter().all(|cell| cell == &template));
        assert_eq!(retained.strong_count(), 1);
        drop(extracted);
        assert!(retained.upgrade().is_none());
    }

    #[test]
    fn extracting_compound_suffixes_preserves_each_arc_owner() {
        let mut buffer = Buffer::new(3, 1);
        let first: std::sync::Arc<str> = "e\u{301}".into();
        let second: std::sync::Arc<str> = "e\u{301}".into();
        assert!(!std::sync::Arc::ptr_eq(&first, &second));
        buffer.clear_row(0, Cell { c: 'e', grapheme: Some(first.clone()), ..Cell::default() });
        let replacement = Cell { c: 'e', grapheme: Some(second.clone()), ..Cell::default() };
        *buffer.cell_mut(0, 0) = replacement.clone();
        // Clearing with an equal template keeps the old suffix allocations.
        buffer.clear_row(0, replacement);
        assert_eq!(buffer.uniform_suffix_start[buffer.row_index(0)], 0);
        let extracted = buffer.extract_row(0);
        assert!(std::sync::Arc::ptr_eq(extracted[0].grapheme.as_ref().unwrap(), &second));
        for cell in &extracted[1..] {
            assert!(std::sync::Arc::ptr_eq(cell.grapheme.as_ref().unwrap(), &first));
        }
    }

    #[test]
    fn extraction_preserves_empty_and_single_column_rows() {
        for cols in [0, 1] {
            let mut buffer = Buffer::new(cols, 1);
            assert_eq!(buffer.extract_row(0), vec![Cell::default(); cols]);
            let template = Cell { c: 'x', bg: Color::Indexed(3), ..Cell::default() };
            buffer.clear_row(0, template.clone());
            assert_eq!(buffer.extract_row(0), vec![template; cols]);
        }
    }

    #[test]
    fn suffix_tracking_matches_full_row_reference_after_mixed_edits() {
        let (cols, rows) = (13, 7);
        let mut buffer = Buffer::new(cols, rows);
        let mut expected = vec![vec![Cell::default(); cols]; rows];
        let mut metadata = vec![RowMetadata::default(); rows];
        let mut state = 0x5a17_2026_u64;
        let mut random = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 32) as usize
        };
        for step in 0..600 {
            let row = random() % rows;
            let col = random() % cols;
            let template = match step % 5 {
                0 | 1 => Cell::default(),
                2 => Cell { bg: Color::Indexed(4), flags: CellFlags::ITALIC, ..Cell::default() },
                3 => Cell { c: 'e', grapheme: Some("e\u{301}".into()), ..Cell::default() },
                _ => Cell { fg: Color::Rgb(1, 2, 3), underline_style: UnderlineStyle::Curly,
                    underline_color: Color::Indexed(7), ..Cell::default() },
            };
            match random() % 8 {
                0 => {
                    *buffer.cell_mut(row, col) = template.clone();
                    expected[row][col] = template;
                }
                1 => {
                    let end = col + random() % (cols - col + 1);
                    buffer.row_range_mut(row, col..end).fill(template.clone());
                    expected[row][col..end].fill(template);
                }
                2 => {
                    buffer.row_mut(row).rotate_right(col);
                    expected[row].rotate_right(col);
                }
                3 => {
                    buffer.clear_row(row, template.clone());
                    expected[row].fill(template);
                    metadata[row] = RowMetadata::default();
                }
                4 => {
                    let bottom = row + random() % (rows - row);
                    let count = random() % (rows + 2);
                    let count = count.min(bottom - row + 1);
                    if random() % 2 == 0 {
                        buffer.scroll_up(row, bottom, count, template.clone());
                        expected[row..=bottom].rotate_left(count);
                        metadata[row..=bottom].rotate_left(count);
                        for erased in bottom + 1 - count..=bottom {
                            expected[erased].fill(template.clone());
                            metadata[erased] = RowMetadata::default();
                        }
                    } else {
                        buffer.scroll_down(row, bottom, count, template.clone());
                        expected[row..=bottom].rotate_right(count);
                        metadata[row..=bottom].rotate_right(count);
                        for erased in row..row + count {
                            expected[erased].fill(template.clone());
                            metadata[erased] = RowMetadata::default();
                        }
                    }
                }
                5 => {
                    metadata[row] = RowMetadata { len: col, wrapped: step % 2 == 0 };
                    buffer.set_row_metadata(row, metadata[row]);
                }
                6 => {
                    let end = random() % (cols + 5);
                    buffer.mark_written(row, end);
                    metadata[row].len = metadata[row].len.max(end.min(cols));
                    buffer.set_wrapped(row, step % 2 == 0);
                    metadata[row].wrapped = step % 2 == 0;
                }
                _ => {
                    let retained: Vec<_> = expected.iter().zip(&metadata)
                        .map(|(cells, metadata)| crate::grid::reflow::RetainedRow {
                            cells: cells.clone(), metadata: *metadata,
                        }).collect();
                    buffer = Buffer::from_retained_rows(cols, &retained);
                }
            }
            for row in 0..rows {
                assert_eq!(buffer.extract_row(row), expected[row], "step={step}, row={row}");
                let last_nonblank = expected[row].iter().rposition(|cell| cell.c != ' ')
                    .map_or(0, |col| col + 1);
                let actual = buffer.row_metadata(row);
                assert_eq!((actual.len, actual.wrapped),
                    (metadata[row].len.max(last_nonblank), metadata[row].wrapped),
                    "step={step}, row={row}");
                let suffix = buffer.uniform_suffix_start[buffer.row_index(row)];
                assert!(suffix <= cols);
                assert!(expected[row][suffix..].iter().all(|cell| cell == &expected[row][cols - 1]),
                    "invalid suffix after step={step}, row={row}");
            }
        }
    }

    #[test]
    fn repeated_region_scrolls_preserve_row_edits_and_reconstruction_order() {
        let mut buffer = Buffer::new(9, 7);
        let mut expected = vec![vec![Cell::default(); 9]; 7];
        for step in 0..200 {
            let rows = buffer.rows();
            let cols = buffer.cols();
            let top = if step % 4 == 0 { 0 } else { (step * 7) % rows };
            let bottom = if step % 4 == 0 {
                rows - 1
            } else {
                top + (step * 11) % (rows - top)
            };
            let count = match step % 9 {
                0 => 0,
                1 => usize::MAX,
                _ => step % (bottom - top + 1) + 1,
            };
            let template = Cell {
                c: char::from(b'!' + (step % 90) as u8),
                grapheme: None,
                fg: Color::Indexed((step % 256) as u8),
                bg: Color::Rgb(12, 34, 56),
                flags: CellFlags::BOLD | CellFlags::ITALIC,
                underline_style: UnderlineStyle::Curly,
                underline_color: Color::Indexed(7),
            };
            let up = step % 3 != 0;
            let before = expected.clone();
            let shift = count.min(bottom - top + 1) as isize;
            for row in top..=bottom {
                let source = row as isize + if up { shift } else { -shift };
                expected[row] = if source >= top as isize && source <= bottom as isize {
                    before[source as usize].clone()
                } else {
                    vec![template.clone(); cols]
                };
            }
            if up {
                buffer.scroll_up(top, bottom, count, template.clone());
            } else {
                buffer.scroll_down(top, bottom, count, template.clone());
            }

            // Exercise both mutable accessors after the physical row order changed.
            let edited_row = (step * 13) % rows;
            buffer.row_mut(edited_row).fill(template.clone());
            expected[edited_row].fill(template);
            let edited_col = (step * 17) % cols;
            buffer.cell_mut(edited_row, edited_col).c = '日';
            expected[edited_row][edited_col].c = '日';

            if step % 13 == 0 {
                let retained: Vec<_> = (0..rows).map(|row| crate::grid::reflow::RetainedRow {
                    cells: buffer.extract_row(row), metadata: buffer.row_metadata(row),
                }).collect();
                buffer = Buffer::from_retained_rows(cols, &retained);
            }
            assert_eq!(buffer.rows(), expected.len());
            assert_eq!(buffer.cols(), expected[0].len());
            for (row, expected_row) in expected.iter().enumerate() {
                assert_eq!(
                    format!("{:?}", buffer.extract_row(row)),
                    format!("{expected_row:?}"),
                    "step={step}, row={row}"
                );
                for (col, expected_cell) in expected_row.iter().enumerate() {
                    assert_eq!(
                        format!("{:?}", buffer.cell(row, col)),
                        format!("{expected_cell:?}"),
                        "step={step}, row={row}, col={col}"
                    );
                }
            }
        }
    }

    #[test]
    fn row_offset_rotation_keeps_graphemes_lengths_and_wrap_flags_together() {
        let mut buffer = Buffer::new(8, 6);
        for row in 0..6 {
            *buffer.cell_mut(row, 0) = Cell {
                c: 'e', grapheme: Some(format!("e{}", char::from_u32(0x300 + row as u32).unwrap()).into()), ..Cell::default()
            };
            buffer.mark_written(row, row + 1);
            buffer.set_wrapped(row, row % 2 == 0);
        }
        let before: Vec<_> = (0..6).map(|row| (buffer.extract_row(row), buffer.row_metadata(row))).collect();
        buffer.scroll_up(1, 4, 1, Cell::default());
        for (row, source) in [(0, 0), (1, 2), (2, 3), (3, 4), (5, 5)] {
            assert_eq!(buffer.extract_row(row), before[source].0);
            assert_eq!(buffer.row_metadata(row).len, before[source].1.len);
            assert_eq!(buffer.row_metadata(row).wrapped, before[source].1.wrapped);
        }
        assert_eq!(buffer.row_metadata(4).len, 0);
        assert!(!buffer.row_metadata(4).wrapped);
        buffer.scroll_down(1, 4, 1, Cell::default());
        for row in 2..=4 {
            assert_eq!(buffer.extract_row(row), before[row].0);
            assert_eq!(buffer.row_metadata(row).len, before[row].1.len);
            assert_eq!(buffer.row_metadata(row).wrapped, before[row].1.wrapped);
        }
        assert_eq!(buffer.row_metadata(1).len, 0);
        assert!(!buffer.row_metadata(1).wrapped);
    }

    #[test]
    fn scrolling_preserves_cells_outside_the_region_and_fills_exposed_rows() {
        let template = Cell {
            c: ' ',
            grapheme: None,
            fg: Color::Rgb(12, 34, 56),
            bg: Color::Indexed(7),
            flags: CellFlags::ITALIC,
            underline_style: UnderlineStyle::Curly,
            underline_color: Color::Rgb(65, 43, 21),
        };
        for cols in [1, 3, 7] {
            for rows in [1, 2, 6] {
                for top in 0..rows {
                    for bottom in top..rows {
                        for count in (0..=rows + 1).chain([usize::MAX]) {
                            for up in [true, false] {
                                let mut buffer = Buffer::new(cols, rows);
                                for row in 0..rows {
                                    for col in 0..cols {
                                        *buffer.cell_mut(row, col) = Cell {
                                            c: char::from(b'A' + (row * cols + col) as u8),
                                            grapheme: None,
                                            fg: Color::Indexed(row as u8),
                                            bg: Color::Indexed(col as u8),
                                            flags: CellFlags::BOLD,
                                            underline_style: UnderlineStyle::Double,
                                            underline_color: Color::Indexed((row + col) as u8),
                                        };
                                    }
                                }
                                let before: Vec<_> =
                                    (0..rows).map(|row| buffer.extract_row(row)).collect();
                                if up {
                                    buffer.scroll_up(top, bottom, count, template.clone());
                                } else {
                                    buffer.scroll_down(top, bottom, count, template.clone());
                                }
                                let shift = count.min(bottom - top + 1) as isize;
                                for row in 0..rows {
                                    for col in 0..cols {
                                        let source = row as isize + if up { shift } else { -shift };
                                        let expected = if row < top || row > bottom {
                                            before[row][col].clone()
                                        } else if source >= top as isize
                                            && source <= bottom as isize
                                        {
                                            before[source as usize][col].clone()
                                        } else {
                                            template.clone()
                                        };
                                        let actual = buffer.cell(row, col);
                                        assert_eq!(
                                            (actual.c, actual.fg, actual.bg, actual.flags, actual.underline_style, actual.underline_color),
                                            (expected.c, expected.fg, expected.bg, expected.flags, expected.underline_style, expected.underline_color),
                                            "{cols}x{rows}, region={top}..={bottom}, count={count}, up={up}, cell=({row},{col})",
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
