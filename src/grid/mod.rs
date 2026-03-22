pub mod buffer;
pub mod cell;

use buffer::Buffer;
use cell::{Cell, CellFlags, Color};

#[derive(Debug)]
pub struct Grid {
    pub buffer: Buffer,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub saved_cursor_row: usize,
    pub saved_cursor_col: usize,
    pub scroll_top: usize,
    pub scroll_bottom: usize,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
    pub dirty: Vec<bool>,
}

impl Grid {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            buffer: Buffer::new(cols, rows),
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor_row: 0,
            saved_cursor_col: 0,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            dirty: vec![true; rows],
        }
    }

    pub fn cols(&self) -> usize {
        self.buffer.cols()
    }

    pub fn rows(&self) -> usize {
        self.buffer.rows()
    }

    pub fn template_cell(&self) -> Cell {
        Cell {
            c: ' ',
            fg: self.fg,
            bg: self.bg,
            flags: CellFlags::empty(),
        }
    }

    pub fn put_char(&mut self, c: char) {
        if self.cursor_col >= self.cols() {
            self.carriage_return();
            self.newline();
        }
        let row = self.cursor_row;
        let col = self.cursor_col;
        let cell = self.buffer.cell_mut(row, col);
        cell.c = c;
        cell.fg = self.fg;
        cell.bg = self.bg;
        cell.flags = self.flags;
        self.dirty[row] = true;
        self.cursor_col += 1;
    }

    pub fn newline(&mut self) {
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor_row < self.rows() - 1 {
            self.cursor_row += 1;
        }
    }

    pub fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        }
    }

    pub fn tab(&mut self) {
        let next_tab = (self.cursor_col / 8 + 1) * 8;
        self.cursor_col = next_tab.min(self.cols() - 1);
    }

    pub fn scroll_up(&mut self, count: usize) {
        let template = self.template_cell();
        self.buffer
            .scroll_up(self.scroll_top, self.scroll_bottom, count, template);
        for row in self.scroll_top..=self.scroll_bottom {
            self.dirty[row] = true;
        }
    }

    pub fn scroll_down(&mut self, count: usize) {
        let template = self.template_cell();
        self.buffer
            .scroll_down(self.scroll_top, self.scroll_bottom, count, template);
        for row in self.scroll_top..=self.scroll_bottom {
            self.dirty[row] = true;
        }
    }

    pub fn erase_in_line(&mut self, mode: u16) {
        let row = self.cursor_row;
        let cols = self.cols();
        let template = self.template_cell();
        match mode {
            0 => {
                for col in self.cursor_col..cols {
                    *self.buffer.cell_mut(row, col) = template;
                }
            }
            1 => {
                for col in 0..=self.cursor_col.min(cols - 1) {
                    *self.buffer.cell_mut(row, col) = template;
                }
            }
            2 => {
                self.buffer.clear_row(row, template);
            }
            _ => {}
        }
        self.dirty[row] = true;
    }

    pub fn erase_in_display(&mut self, mode: u16) {
        let template = self.template_cell();
        match mode {
            0 => {
                self.erase_in_line(0);
                for row in self.cursor_row + 1..self.rows() {
                    self.buffer.clear_row(row, template);
                    self.dirty[row] = true;
                }
            }
            1 => {
                self.erase_in_line(1);
                for row in 0..self.cursor_row {
                    self.buffer.clear_row(row, template);
                    self.dirty[row] = true;
                }
            }
            2 | 3 => {
                for row in 0..self.rows() {
                    self.buffer.clear_row(row, template);
                    self.dirty[row] = true;
                }
            }
            _ => {}
        }
    }

    pub fn set_cursor_pos(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.rows() - 1);
        self.cursor_col = col.min(self.cols() - 1);
    }

    pub fn move_cursor_up(&mut self, n: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(n);
    }

    pub fn move_cursor_down(&mut self, n: usize) {
        self.cursor_row = (self.cursor_row + n).min(self.rows() - 1);
    }

    pub fn move_cursor_forward(&mut self, n: usize) {
        self.cursor_col = (self.cursor_col + n).min(self.cols() - 1);
    }

    pub fn move_cursor_backward(&mut self, n: usize) {
        self.cursor_col = self.cursor_col.saturating_sub(n);
    }

    pub fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        let bottom = bottom.min(self.rows() - 1);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
            self.cursor_row = 0;
            self.cursor_col = 0;
        }
    }

    pub fn insert_lines(&mut self, count: usize) {
        if self.cursor_row >= self.scroll_top && self.cursor_row <= self.scroll_bottom {
            let old_top = self.scroll_top;
            self.scroll_top = self.cursor_row;
            self.scroll_down(count);
            self.scroll_top = old_top;
        }
    }

    pub fn delete_lines(&mut self, count: usize) {
        if self.cursor_row >= self.scroll_top && self.cursor_row <= self.scroll_bottom {
            let old_top = self.scroll_top;
            self.scroll_top = self.cursor_row;
            self.scroll_up(count);
            self.scroll_top = old_top;
        }
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor_row = self.cursor_row;
        self.saved_cursor_col = self.cursor_col;
    }

    pub fn restore_cursor(&mut self) {
        self.cursor_row = self.saved_cursor_row;
        self.cursor_col = self.saved_cursor_col;
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.buffer.resize(cols, rows);
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.dirty = vec![true; rows];
    }

    pub fn mark_all_dirty(&mut self) {
        for d in &mut self.dirty {
            *d = true;
        }
    }

    pub fn clear_dirty(&mut self) {
        for d in &mut self.dirty {
            *d = false;
        }
    }

    pub fn is_any_dirty(&self) -> bool {
        self.dirty.iter().any(|&d| d)
    }
}
