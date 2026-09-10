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
    cols: usize,
    rows: usize,
    metadata: Vec<RowMetadata>,
}

impl Buffer {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols * rows],
            row_starts: (0..rows).map(|row| row * cols).collect(),
            cols,
            rows,
            metadata: vec![RowMetadata::default(); rows],
        }
    }

    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        &self.cells[self.row_starts[row] + col]
    }

    pub fn cell_mut(&mut self, row: usize, col: usize) -> &mut Cell {
        &mut self.cells[self.row_starts[row] + col]
    }

    pub fn row_mut(&mut self, row: usize) -> &mut [Cell] {
        let start = self.row_starts[row];
        &mut self.cells[start..start + self.cols]
    }

    pub(crate) fn row_metadata(&self, row: usize) -> RowMetadata {
        let mut metadata = self.metadata[row];
        // Public cell access is also used by screen writers and test fixtures.
        // Keep directly assigned nonblank cells in the retained content range.
        let start = self.row_starts[row];
        if let Some(col) = self.cells[start..start + self.cols]
            .iter().rposition(|cell| cell.c != ' ')
        {
            metadata.len = metadata.len.max(col + 1);
        }
        metadata
    }

    pub(crate) fn set_row_metadata(&mut self, row: usize, metadata: RowMetadata) {
        self.metadata[row] = metadata;
    }

    pub(crate) fn mark_written(&mut self, row: usize, end: usize) {
        self.metadata[row].len = self.metadata[row].len.max(end.min(self.cols));
    }

    pub(crate) fn set_wrapped(&mut self, row: usize, wrapped: bool) {
        self.metadata[row].wrapped = wrapped;
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn clear_row(&mut self, row: usize, template: Cell) {
        self.metadata[row] = RowMetadata::default();
        self.row_mut(row).fill(template);
    }

    pub fn scroll_up(&mut self, top: usize, bottom: usize, count: usize, template: Cell) {
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        self.row_starts[top..=bottom].rotate_left(count);
        self.metadata[top..=bottom].rotate_left(count);
        for row in bottom + 1 - count..=bottom {
            self.clear_row(row, template.clone());
        }
    }

    pub fn scroll_down(&mut self, top: usize, bottom: usize, count: usize, template: Cell) {
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        self.row_starts[top..=bottom].rotate_right(count);
        self.metadata[top..=bottom].rotate_right(count);
        for row in top..top + count {
            self.clear_row(row, template.clone());
        }
    }

    pub fn extract_row(&self, row: usize) -> Vec<Cell> {
        let start = self.row_starts[row];
        self.cells[start..start + self.cols].to_vec()
    }

    pub(crate) fn from_retained_rows(cols: usize, rows: &[super::reflow::RetainedRow]) -> Self {
        let mut buffer = Self::new(cols, rows.len());
        for (index, row) in rows.iter().enumerate() {
            buffer.row_mut(index).clone_from_slice(&row.cells);
            buffer.metadata[index] = row.metadata;
        }
        buffer
    }

}

#[cfg(test)]
mod tests {
    use super::Buffer;
    use crate::grid::cell::{Cell, CellFlags, Color, UnderlineStyle};

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
