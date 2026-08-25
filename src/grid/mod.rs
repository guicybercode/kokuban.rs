pub mod buffer;
pub mod cell;
pub mod marks;

use buffer::Buffer;
use cell::{Cell, CellFlags, Color, UnderlineStyle};
use marks::MarkIndex;
use std::collections::VecDeque;
use unicode_width::UnicodeWidthChar;

use crate::parser::kitty_graphics::KittyCommand;
use crate::parser::sixel::{SixelImage, MAX_RGBA_BYTES as MAX_PENDING_SIXEL_BYTES};
use crate::graphics::ImagePlacement;

const MAX_PENDING_SIXEL_IMAGES: usize = 256;

#[derive(Debug)]
pub(crate) enum TerminalEvent {
    Response(Vec<u8>),
    KittyGraphics {
        command: KittyCommand,
        cursor_row: usize,
        cursor_col: usize,
    },
}

const DEFAULT_CELL: Cell = Cell {
    c: ' ',
    fg: Color::Default,
    bg: Color::Default,
    flags: CellFlags::empty(),
    underline_style: UnderlineStyle::None,
    underline_color: Color::Default,
};

// Mouse tracking modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseTracking {
    None,
    Normal,      // 1000
    ButtonEvent, // 1002
    AnyEvent,    // 1003
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEncoding {
    Default,
    Sgr, // 1006
}

// Cursor shape
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Copy)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blinking: bool,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self { shape: CursorShape::Block, blinking: true }
    }
}

// DEC character set
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharSet {
    Ascii,
    DecSpecial,
}

pub fn dec_special_map(c: char) -> char {
    match c {
        'j' => '┘', 'k' => '┐', 'l' => '┌', 'm' => '└',
        'n' => '┼', 'q' => '─', 't' => '├', 'u' => '┤',
        'v' => '┴', 'w' => '┬', 'x' => '│', 'a' => '▒',
        'f' => '°', 'g' => '±', 'h' => '▒', 'i' => '␋',
        'y' => '≤', 'z' => '≥', '{' => 'π', '|' => '≠',
        '}' => '£', '~' => '·',
        _ => c,
    }
}

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
    // Scrollback
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_max: usize,
    pub scroll_offset: usize,
    // Alternate screen
    alt_buffer: Option<Buffer>,
    alt_cursor: (usize, usize),
    pub using_alt_screen: bool,
    // Mode flags
    pub cursor_visible: bool,
    pub application_cursor_keys: bool,
    pub bracketed_paste: bool,
    pub mouse_tracking: MouseTracking,
    pub mouse_encoding: MouseEncoding,
    pub focus_events: bool,
    pub cursor_style: CursorStyle,
    pub insert_mode: bool,
    pub charset: CharSet,
    // Underline state (current SGR)
    pub underline_style: UnderlineStyle,
    pub underline_color: Color,
    // Terminal state from OSC sequences
    pub title: String,
    pub cwd: String,
    // Prompt marks
    pub marks: MarkIndex,
    pub total_lines_pushed: usize,
    // Ordered protocol events to process after parsing this PTY read.
    pending_terminal_events: Vec<TerminalEvent>,
    // Colors for query responses
    pub default_fg_hex: String,
    pub default_bg_hex: String,
    pending_sixel_images: Vec<SixelImage>,
    pending_sixel_bytes: usize,
    // Active image placements for this grid
    pub image_placements: Vec<ImagePlacement>,
    // Cell pixel dimensions (set by renderer for accurate CSI t responses)
    pub cell_pixel_width: u16,
    pub cell_pixel_height: u16,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, scrollback_max: usize) -> Self {
        assert!(
            cols > 0 && rows > 0,
            "terminal grid dimensions must be non-zero"
        );
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
            scrollback: VecDeque::new(),
            scrollback_max,
            scroll_offset: 0,
            alt_buffer: None,
            alt_cursor: (0, 0),
            using_alt_screen: false,
            cursor_visible: true,
            application_cursor_keys: false,
            bracketed_paste: false,
            mouse_tracking: MouseTracking::None,
            mouse_encoding: MouseEncoding::Default,
            focus_events: false,
            cursor_style: CursorStyle::default(),
            insert_mode: false,
            charset: CharSet::Ascii,
            underline_style: UnderlineStyle::None,
            underline_color: Color::Default,
            title: String::new(),
            cwd: String::new(),
            marks: MarkIndex::default(),
            total_lines_pushed: 0,
            pending_terminal_events: Vec::new(),
            default_fg_hex: String::new(),
            default_bg_hex: String::new(),
            pending_sixel_images: Vec::new(),
            pending_sixel_bytes: 0,
            image_placements: Vec::new(),
            cell_pixel_width: 8,
            cell_pixel_height: 16,
        }
    }

    pub fn cols(&self) -> usize { self.buffer.cols() }
    pub fn rows(&self) -> usize { self.buffer.rows() }
    pub fn scrollback_len(&self) -> usize { self.scrollback.len() }
    pub fn scrollback_max(&self) -> usize { self.scrollback_max }

    pub fn scrollback_cell(&self, row: usize, col: usize) -> char {
        if let Some(row_data) = self.scrollback.get(row) {
            if col < row_data.len() {
                return row_data[col].c;
            }
        }
        ' '
    }

    pub fn template_cell(&self) -> Cell {
        Cell {
            c: ' ',
            fg: self.fg,
            bg: self.bg,
            flags: CellFlags::empty(),
            underline_style: UnderlineStyle::None,
            underline_color: Color::Default,
        }
    }

    pub(crate) fn queue_response(&mut self, response: Vec<u8>) {
        self.pending_terminal_events.push(TerminalEvent::Response(response));
    }

    pub(crate) fn queue_kitty_command(&mut self, command: KittyCommand) {
        self.pending_terminal_events.push(TerminalEvent::KittyGraphics {
            command,
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
        });
    }

    pub(crate) fn drain_terminal_events(&mut self) -> Vec<TerminalEvent> {
        std::mem::take(&mut self.pending_terminal_events)
    }

    pub fn drain_sixel_images(&mut self) -> Vec<SixelImage> {
        self.pending_sixel_bytes = 0;
        std::mem::take(&mut self.pending_sixel_images)
    }

    pub(crate) fn queue_sixel_image(&mut self, image: SixelImage) -> bool {
        self.queue_sixel_image_with_limit(image, MAX_PENDING_SIXEL_BYTES)
    }

    pub(crate) fn remaining_sixel_bytes(&self) -> usize {
        MAX_PENDING_SIXEL_BYTES.saturating_sub(self.pending_sixel_bytes)
    }

    pub(crate) fn has_sixel_queue_slot(&self) -> bool {
        self.pending_sixel_images.len() < MAX_PENDING_SIXEL_IMAGES
    }

    #[cfg(test)]
    pub(crate) fn set_pending_sixel_bytes_for_test(&mut self, bytes: usize) {
        assert!(bytes <= MAX_PENDING_SIXEL_BYTES);
        self.pending_sixel_bytes = bytes;
    }

    fn queue_sixel_image_with_limit(&mut self, image: SixelImage, limit: usize) -> bool {
        if !self.has_sixel_queue_slot() {
            return false;
        }

        let Some(pending_bytes) = self
            .pending_sixel_bytes
            .checked_add(image.pixels.capacity())
        else {
            return false;
        };
        if pending_bytes > limit {
            return false;
        }

        self.pending_sixel_images.push(image);
        self.pending_sixel_bytes = pending_bytes;
        true
    }

    /// Place a character at the cursor, handling wide chars and DEC charset.
    pub fn put_char(&mut self, c: char) {
        let c = if self.charset == CharSet::DecSpecial {
            dec_special_map(c)
        } else {
            c
        };

        let char_width = c.width().unwrap_or(1);

        // Wide char at last column: wrap first
        if char_width == 2 && self.cursor_col >= self.cols().saturating_sub(1) {
            // Pad the current cell with a space, then wrap
            if self.cursor_col < self.cols() {
                let row = self.cursor_row;
                let col = self.cursor_col;
                *self.buffer.cell_mut(row, col) = self.template_cell();
            }
            self.cursor_col = self.cols(); // trigger wrap on next line
        }

        if self.cursor_col >= self.cols() {
            self.carriage_return();
            self.newline();
        }

        let row = self.cursor_row;
        let col = self.cursor_col;

        // Insert mode: shift content right
        if self.insert_mode {
            let cols = self.cols();
            let shift = char_width;
            for c_idx in (col + shift..cols).rev() {
                let src = *self.buffer.cell(row, c_idx - shift);
                *self.buffer.cell_mut(row, c_idx) = src;
            }
        }

        // Clear any wide char that we're overwriting
        self.clear_wide_overlap(row, col, char_width);

        let cell = self.buffer.cell_mut(row, col);
        cell.c = c;
        cell.fg = self.fg;
        cell.bg = self.bg;
        cell.flags = self.flags;
        cell.underline_style = self.underline_style;
        cell.underline_color = self.underline_color;

        if char_width == 2 {
            cell.flags.insert(CellFlags::WIDE);
            cell.flags.remove(CellFlags::WIDE_CONT);
            // Set continuation cell
            if col + 1 < self.cols() {
                let cont = self.buffer.cell_mut(row, col + 1);
                cont.c = '\0';
                cont.fg = self.fg;
                cont.bg = self.bg;
                cont.flags = CellFlags::WIDE_CONT;
                cont.underline_style = UnderlineStyle::None;
                cont.underline_color = Color::Default;
            }
        } else {
            cell.flags.remove(CellFlags::WIDE);
            cell.flags.remove(CellFlags::WIDE_CONT);
        }

        self.dirty[row] = true;
        self.cursor_col += char_width;
    }

    /// Clear wide char overlap when overwriting cells.
    fn clear_wide_overlap(&mut self, row: usize, col: usize, width: usize) {
        // If we're overwriting the continuation half of a wide char, clear its first half
        if col < self.cols() {
            let cell = self.buffer.cell(row, col);
            if cell.flags.contains(CellFlags::WIDE_CONT) && col > 0 {
                let prev = self.buffer.cell_mut(row, col - 1);
                prev.c = ' ';
                prev.flags.remove(CellFlags::WIDE);
            }
        }
        // If we're overwriting the first half of a wide char, clear continuation
        for c in col..col + width {
            if c < self.cols() {
                let cell = self.buffer.cell(row, c);
                if cell.flags.contains(CellFlags::WIDE) && c + 1 < self.cols() {
                    let cont = self.buffer.cell_mut(row, c + 1);
                    cont.c = ' ';
                    cont.flags.remove(CellFlags::WIDE_CONT);
                }
            }
        }
    }

    pub fn newline(&mut self) {
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor_row < self.rows() - 1 {
            self.cursor_row += 1;
        }
    }

    pub fn carriage_return(&mut self) { self.cursor_col = 0; }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        }
    }

    pub fn tab(&mut self) {
        let next_tab = (self.cursor_col / 8 + 1) * 8;
        self.cursor_col = next_tab.min(self.cols() - 1);
    }

    pub fn current_absolute_row(&self) -> usize {
        self.total_lines_pushed + self.cursor_row
    }

    pub fn scroll_up(&mut self, count: usize) {
        if !self.using_alt_screen && self.scroll_top == 0 {
            let pushed = count.min(self.rows());
            for i in 0..pushed {
                let row_data = self.buffer.extract_row(i);
                self.scrollback.push_back(row_data);
                if self.scrollback.len() > self.scrollback_max {
                    self.scrollback.pop_front();
                }
            }
            self.total_lines_pushed += pushed;
        }
        let template = self.template_cell();
        self.buffer.scroll_up(self.scroll_top, self.scroll_bottom, count, template);
        for row in self.scroll_top..=self.scroll_bottom {
            self.dirty[row] = true;
        }
    }

    pub fn scroll_down(&mut self, count: usize) {
        let template = self.template_cell();
        self.buffer.scroll_down(self.scroll_top, self.scroll_bottom, count, template);
        for row in self.scroll_top..=self.scroll_bottom {
            self.dirty[row] = true;
        }
    }

    pub fn visible_cell(&self, vis_row: usize, col: usize) -> &Cell {
        if self.scroll_offset == 0 {
            return self.buffer.cell(vis_row, col);
        }
        let sb_len = self.scrollback.len();
        let start = sb_len.saturating_sub(self.scroll_offset);
        let abs_row = start + vis_row;
        if abs_row < sb_len {
            let row_data = &self.scrollback[abs_row];
            if col < row_data.len() { &row_data[col] } else { &DEFAULT_CELL }
        } else {
            let buffer_row = abs_row - sb_len;
            if buffer_row < self.buffer.rows() { self.buffer.cell(buffer_row, col) } else { &DEFAULT_CELL }
        }
    }

    pub fn scroll_viewport_up(&mut self, lines: usize) {
        let max = self.scrollback.len();
        self.scroll_offset = (self.scroll_offset + lines).min(max);
        self.mark_all_dirty();
    }

    pub fn scroll_viewport_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        self.mark_all_dirty();
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.scroll_offset != 0 {
            self.scroll_offset = 0;
            self.mark_all_dirty();
        }
    }

    pub fn enter_alt_screen(&mut self) {
        if self.using_alt_screen { return; }
        self.using_alt_screen = true;
        self.scroll_offset = 0;
        self.alt_cursor = (self.cursor_row, self.cursor_col);
        let cols = self.cols();
        let rows = self.rows();
        let primary = std::mem::replace(&mut self.buffer, Buffer::new(cols, rows));
        self.alt_buffer = Some(primary);
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
        self.mark_all_dirty();
    }

    pub fn leave_alt_screen(&mut self) {
        if !self.using_alt_screen { return; }
        if let Some(primary) = self.alt_buffer.take() {
            self.buffer = primary;
        }
        self.using_alt_screen = false;
        self.cursor_row = self.alt_cursor.0.min(self.rows().saturating_sub(1));
        // `cols` is a valid transient position: it means the next printable
        // character must wrap before being written.
        self.cursor_col = self.alt_cursor.1.min(self.cols());
        self.scroll_top = 0;
        self.scroll_bottom = self.rows().saturating_sub(1);
        // Clear all image placements when leaving alt screen
        self.image_placements.clear();
        self.mark_all_dirty();
    }

    pub fn erase_in_line(&mut self, mode: u16) {
        let row = self.cursor_row;
        let cols = self.cols();
        let template = self.template_cell();
        match mode {
            0 => { for col in self.cursor_col..cols { *self.buffer.cell_mut(row, col) = template; } }
            1 => { for col in 0..=self.cursor_col.min(cols - 1) { *self.buffer.cell_mut(row, col) = template; } }
            2 => { self.buffer.clear_row(row, template); }
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

    pub fn move_cursor_up(&mut self, n: usize) { self.cursor_row = self.cursor_row.saturating_sub(n); }
    pub fn move_cursor_down(&mut self, n: usize) { self.cursor_row = (self.cursor_row + n).min(self.rows() - 1); }
    pub fn move_cursor_forward(&mut self, n: usize) { self.cursor_col = (self.cursor_col + n).min(self.cols() - 1); }
    pub fn move_cursor_backward(&mut self, n: usize) { self.cursor_col = self.cursor_col.saturating_sub(n); }

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
        self.cursor_row = self.saved_cursor_row.min(self.rows().saturating_sub(1));
        // Preserve the pending-wrap sentinel at `cursor_col == cols`.
        self.cursor_col = self.saved_cursor_col.min(self.cols());
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        assert!(
            cols > 0 && rows > 0,
            "terminal grid dimensions must be non-zero"
        );
        self.buffer.resize(cols, rows);
        if let Some(ref mut alt) = self.alt_buffer {
            alt.resize(cols, rows);
        }
        let max_row = rows - 1;
        let max_col = cols - 1;
        self.scroll_top = 0;
        self.scroll_bottom = max_row;
        self.cursor_row = self.cursor_row.min(max_row);
        self.cursor_col = self.cursor_col.min(max_col);
        self.saved_cursor_row = self.saved_cursor_row.min(max_row);
        self.saved_cursor_col = self.saved_cursor_col.min(max_col);
        self.alt_cursor.0 = self.alt_cursor.0.min(max_row);
        self.alt_cursor.1 = self.alt_cursor.1.min(max_col);
        self.dirty = vec![true; rows];
    }

    pub fn mark_all_dirty(&mut self) { for d in &mut self.dirty { *d = true; } }
    pub fn clear_dirty(&mut self) { for d in &mut self.dirty { *d = false; } }
    pub fn is_any_dirty(&self) -> bool { self.dirty.iter().any(|&d| d) }
}

#[cfg(test)]
mod tests {
    use super::Grid;
    use crate::parser::sixel::SixelImage;

    fn sixel_image(byte_len: usize) -> SixelImage {
        SixelImage {
            width: 1,
            height: (byte_len / 4) as u32,
            pixels: vec![0; byte_len],
        }
    }

    #[test]
    fn resize_clamps_active_and_saved_cursors() {
        let mut grid = Grid::new(12, 8, 100);
        grid.set_cursor_pos(7, 11);
        grid.save_cursor();

        grid.resize(4, 3);
        assert_eq!((grid.cursor_row, grid.cursor_col), (2, 3));

        grid.resize(20, 10);
        grid.restore_cursor();
        assert_eq!((grid.cursor_row, grid.cursor_col), (2, 3));
        grid.put_char('x');
        assert_eq!(grid.buffer.cell(2, 3).c, 'x');
    }

    #[test]
    fn resize_clamps_primary_cursor_saved_by_alt_screen() {
        let mut grid = Grid::new(12, 8, 100);
        grid.set_cursor_pos(7, 11);
        grid.enter_alt_screen();

        grid.resize(4, 3);
        grid.resize(20, 10);
        grid.leave_alt_screen();

        assert_eq!((grid.cursor_row, grid.cursor_col), (2, 3));
        grid.put_char('x');
        assert_eq!(grid.buffer.cell(2, 3).c, 'x');
    }

    #[test]
    fn save_restore_preserves_pending_wrap() {
        let mut grid = Grid::new(2, 2, 0);
        grid.put_char('a');
        grid.put_char('b');
        assert_eq!(grid.cursor_col, grid.cols());
        grid.save_cursor();
        grid.set_cursor_pos(0, 0);

        grid.restore_cursor();
        grid.put_char('c');

        assert_eq!(grid.buffer.cell(0, 1).c, 'b');
        assert_eq!(grid.buffer.cell(1, 0).c, 'c');
    }

    #[test]
    fn alt_screen_round_trip_preserves_pending_wrap() {
        let mut grid = Grid::new(2, 2, 0);
        grid.put_char('a');
        grid.put_char('b');
        assert_eq!(grid.cursor_col, grid.cols());

        grid.enter_alt_screen();
        grid.leave_alt_screen();
        grid.put_char('c');

        assert_eq!(grid.buffer.cell(0, 1).c, 'b');
        assert_eq!(grid.buffer.cell(1, 0).c, 'c');
    }

    #[test]
    fn pending_sixel_queue_enforces_and_resets_its_byte_budget() {
        let mut grid = Grid::new(2, 2, 0);

        assert!(grid.queue_sixel_image_with_limit(sixel_image(8), 12));
        assert!(grid.queue_sixel_image_with_limit(sixel_image(4), 12));
        assert_eq!(grid.pending_sixel_bytes, 12);
        assert_eq!(
            grid.remaining_sixel_bytes(),
            super::MAX_PENDING_SIXEL_BYTES - 12
        );
        assert!(!grid.queue_sixel_image_with_limit(sixel_image(4), 12));
        assert_eq!(grid.pending_sixel_images.len(), 2);

        let drained = grid.drain_sixel_images();
        assert_eq!(drained.len(), 2);
        assert_eq!(grid.pending_sixel_bytes, 0);
        assert!(grid.queue_sixel_image_with_limit(sixel_image(8), 12));

        grid.pending_sixel_bytes = usize::MAX;
        assert!(!grid.queue_sixel_image_with_limit(sixel_image(4), usize::MAX));
    }

    #[test]
    fn pending_sixel_queue_bounds_per_image_metadata() {
        let mut grid = Grid::new(2, 2, 0);

        for _ in 0..super::MAX_PENDING_SIXEL_IMAGES {
            assert!(grid.queue_sixel_image(sixel_image(4)));
        }
        assert!(!grid.has_sixel_queue_slot());
        assert!(!grid.queue_sixel_image(sixel_image(4)));

        assert_eq!(
            grid.drain_sixel_images().len(),
            super::MAX_PENDING_SIXEL_IMAGES
        );
        assert!(grid.has_sixel_queue_slot());
    }

    #[test]
    #[should_panic(expected = "terminal grid dimensions must be non-zero")]
    fn construction_rejects_zero_dimensions() {
        let _ = Grid::new(1, 0, 0);
    }

    #[test]
    #[should_panic(expected = "terminal grid dimensions must be non-zero")]
    fn resize_rejects_zero_dimensions() {
        let mut grid = Grid::new(1, 1, 0);
        grid.resize(0, 1);
    }
}
