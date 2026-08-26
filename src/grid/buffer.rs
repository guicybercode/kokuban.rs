use super::cell::Cell;

#[derive(Debug)]
pub struct Buffer {
    cells: Vec<Cell>,
    cols: usize,
    rows: usize,
}

impl Buffer {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols * rows],
            cols,
            rows,
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

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn clear_row(&mut self, row: usize, template: Cell) {
        let start = row * self.cols;
        for i in start..start + self.cols {
            self.cells[i] = template;
        }
    }

    pub fn scroll_up(&mut self, top: usize, bottom: usize, count: usize, template: Cell) {
        let count = count.min(bottom - top + 1);
        for row in top..=bottom {
            if row + count <= bottom {
                let src_start = (row + count) * self.cols;
                let dst_start = row * self.cols;
                for col in 0..self.cols {
                    self.cells[dst_start + col] = self.cells[src_start + col];
                }
            } else {
                self.clear_row(row, template);
            }
        }
    }

    pub fn scroll_down(&mut self, top: usize, bottom: usize, count: usize, template: Cell) {
        let count = count.min(bottom - top + 1);
        for row in (top..=bottom).rev() {
            if row >= top + count {
                let src_start = (row - count) * self.cols;
                let dst_start = row * self.cols;
                for col in 0..self.cols {
                    self.cells[dst_start + col] = self.cells[src_start + col];
                }
            } else {
                self.clear_row(row, template);
            }
        }
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
                new_cells[row * new_cols + col] = self.cells[row * self.cols + col];
            }
        }
        self.cells = new_cells;
        self.cols = new_cols;
        self.rows = new_rows;
    }
}
