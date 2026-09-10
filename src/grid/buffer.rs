use super::cell::Cell;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RowMetadata {
    pub len: usize,
    pub wrapped: bool,
}

#[derive(Debug)]
pub struct Buffer {
    cells: Vec<Cell>,
    cols: usize,
    rows: usize,
    metadata: Vec<RowMetadata>,
}

impl Buffer {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols * rows],
            cols,
            rows,
            metadata: vec![RowMetadata::default(); rows],
        }
    }

    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        &self.cells[row * self.cols + col]
    }

    pub fn cell_mut(&mut self, row: usize, col: usize) -> &mut Cell {
        &mut self.cells[row * self.cols + col]
    }

    pub fn row_mut(&mut self, row: usize) -> &mut [Cell] {
        let start = row * self.cols;
        &mut self.cells[start..start + self.cols]
    }

    pub(crate) fn row_metadata(&self, row: usize) -> RowMetadata {
        let mut metadata = self.metadata[row];
        // Public cell access is also used by screen writers and test fixtures.
        // Keep directly assigned nonblank cells in the retained content range.
        let start = row * self.cols;
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
        let start = row * self.cols;
        for i in start..start + self.cols {
            self.cells[i] = template.clone();
        }
    }

    pub fn scroll_up(&mut self, top: usize, bottom: usize, count: usize, template: Cell) {
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        let start = top * self.cols;
        let end = (bottom + 1) * self.cols;
        let shift = count * self.cols;
        self.metadata.copy_within(top + count..bottom + 1, top);
        self.metadata[bottom + 1 - count..bottom + 1].fill(RowMetadata::default());
        self.cells[start..end].rotate_left(shift);
        self.cells[end - shift..end].fill(template);
    }

    pub fn scroll_down(&mut self, top: usize, bottom: usize, count: usize, template: Cell) {
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        let start = top * self.cols;
        let end = (bottom + 1) * self.cols;
        let shift = count * self.cols;
        self.metadata.copy_within(top..bottom + 1 - count, top + count);
        self.metadata[top..top + count].fill(RowMetadata::default());
        self.cells[start..end].rotate_right(shift);
        self.cells[start..start + shift].fill(template);
    }

    pub fn extract_row(&self, row: usize) -> Vec<Cell> {
        let start = row * self.cols;
        self.cells[start..start + self.cols].to_vec()
    }

    pub fn resize(&mut self, new_cols: usize, new_rows: usize) {
        let mut new_cells = vec![Cell::default(); new_cols * new_rows];
        let copy_rows = self.rows.min(new_rows);
        let copy_cols = self.cols.min(new_cols);
        for row in 0..copy_rows {
            for col in 0..copy_cols {
                new_cells[row * new_cols + col] = self.cells[row * self.cols + col].clone();
            }
        }
        self.metadata.resize(new_rows, RowMetadata::default());
        for metadata in &mut self.metadata { metadata.len = metadata.len.min(new_cols); }
        self.cells = new_cells;
        self.cols = new_cols;
        self.rows = new_rows;
    }
}

#[cfg(test)]
mod tests {
    use super::Buffer;
    use crate::grid::cell::{Cell, CellFlags, Color, UnderlineStyle};

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
