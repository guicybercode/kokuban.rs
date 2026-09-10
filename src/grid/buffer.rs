use super::cell::Cell;

#[derive(Debug)]
pub struct Buffer {
    cells: Vec<Cell>,
    // Logical rows point into one cell allocation. Scrolling rotates these
    // offsets instead of copying every cell that remains on screen.
    row_starts: Vec<usize>,
    cols: usize,
    rows: usize,
}

impl Buffer {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols * rows],
            row_starts: (0..rows).map(|row| row * cols).collect(),
            cols,
            rows,
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

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn clear_row(&mut self, row: usize, template: Cell) {
        self.row_mut(row).fill(template);
    }

    pub fn scroll_up(&mut self, top: usize, bottom: usize, count: usize, template: Cell) {
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        self.row_starts[top..=bottom].rotate_left(count);
        for row in bottom + 1 - count..=bottom {
            self.clear_row(row, template);
        }
    }

    pub fn scroll_down(&mut self, top: usize, bottom: usize, count: usize, template: Cell) {
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        self.row_starts[top..=bottom].rotate_right(count);
        for row in top..top + count {
            self.clear_row(row, template);
        }
    }

    pub fn extract_row(&self, row: usize) -> Vec<Cell> {
        let start = self.row_starts[row];
        self.cells[start..start + self.cols].to_vec()
    }

    pub fn resize(&mut self, new_cols: usize, new_rows: usize) {
        let mut new_cells = vec![Cell::default(); new_cols * new_rows];
        let copy_rows = self.rows.min(new_rows);
        let copy_cols = self.cols.min(new_cols);
        for row in 0..copy_rows {
            let source = self.row_starts[row];
            let destination = row * new_cols;
            new_cells[destination..destination + copy_cols]
                .copy_from_slice(&self.cells[source..source + copy_cols]);
        }
        self.cells = new_cells;
        self.row_starts = (0..new_rows).map(|row| row * new_cols).collect();
        self.cols = new_cols;
        self.rows = new_rows;
    }
}

#[cfg(test)]
mod tests {
    use super::Buffer;
    use crate::grid::cell::{Cell, CellFlags, Color, UnderlineStyle};

    #[test]
    fn repeated_region_scrolls_preserve_row_edits_and_resize_order() {
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
                    vec![template; cols]
                };
            }
            if up {
                buffer.scroll_up(top, bottom, count, template);
            } else {
                buffer.scroll_down(top, bottom, count, template);
            }

            // Exercise both mutable accessors after the physical row order changed.
            let edited_row = (step * 13) % rows;
            buffer.row_mut(edited_row).fill(template);
            expected[edited_row].fill(template);
            let edited_col = (step * 17) % cols;
            buffer.cell_mut(edited_row, edited_col).c = '日';
            expected[edited_row][edited_col].c = '日';

            if step % 13 == 0 {
                let new_cols = 1 + (step * 17) % 11;
                let new_rows = 1 + (step * 19) % 8;
                expected.resize(new_rows, vec![Cell::default(); new_cols]);
                for row in &mut expected {
                    row.resize(new_cols, Cell::default());
                }
                buffer.resize(new_cols, new_rows);
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
    fn scrolling_preserves_cells_outside_the_region_and_fills_exposed_rows() {
        let template = Cell {
            c: ' ',
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
                                    buffer.scroll_up(top, bottom, count, template);
                                } else {
                                    buffer.scroll_down(top, bottom, count, template);
                                }
                                let shift = count.min(bottom - top + 1) as isize;
                                for row in 0..rows {
                                    for col in 0..cols {
                                        let source = row as isize + if up { shift } else { -shift };
                                        let expected = if row < top || row > bottom {
                                            before[row][col]
                                        } else if source >= top as isize
                                            && source <= bottom as isize
                                        {
                                            before[source as usize][col]
                                        } else {
                                            template
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
