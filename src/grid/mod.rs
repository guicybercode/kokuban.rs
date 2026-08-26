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
    SixelGraphics {
        image: SixelImage,
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
    wrap_pending: bool,
    saved_wrap_pending: bool,
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
    alt_wrap_pending: bool,
    pub using_alt_screen: bool,
    // Mode flags
    pub cursor_visible: bool,
    pub application_cursor_keys: bool,
    pub auto_wrap: bool,
    pub bracketed_paste: bool,
    pub mouse_tracking: MouseTracking,
    pub mouse_encoding: MouseEncoding,
    pub alternate_scroll: bool,
    pub focus_events: bool,
    pub cursor_style: CursorStyle,
    pub insert_mode: bool,
    pub charset: CharSet,
    // Underline state (current SGR)
    pub underline_style: UnderlineStyle,
    pub underline_color: Color,
    // Terminal state from OSC sequences
    title: String,
    title_revision: u64,
    pub cwd: String,
    // Prompt marks
    pub marks: MarkIndex,
    pub total_lines_pushed: usize,
    // Ordered protocol events to process before parsing subsequent PTY bytes.
    pending_terminal_events: Vec<TerminalEvent>,
    // Colors for query responses
    pub default_fg_hex: String,
    pub default_bg_hex: String,
    pending_sixel_count: usize,
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
            wrap_pending: false,
            saved_wrap_pending: false,
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
            alt_wrap_pending: false,
            using_alt_screen: false,
            cursor_visible: true,
            application_cursor_keys: false,
            auto_wrap: true,
            bracketed_paste: false,
            mouse_tracking: MouseTracking::None,
            mouse_encoding: MouseEncoding::Default,
            alternate_scroll: false,
            focus_events: false,
            cursor_style: CursorStyle::default(),
            insert_mode: false,
            charset: CharSet::Ascii,
            underline_style: UnderlineStyle::None,
            underline_color: Color::Default,
            title: String::new(),
            title_revision: 0,
            cwd: String::new(),
            marks: MarkIndex::default(),
            total_lines_pushed: 0,
            pending_terminal_events: Vec::new(),
            default_fg_hex: String::new(),
            default_bg_hex: String::new(),
            pending_sixel_count: 0,
            pending_sixel_bytes: 0,
            image_placements: Vec::new(),
            cell_pixel_width: 8,
            cell_pixel_height: 16,
        }
    }

    pub fn cols(&self) -> usize { self.buffer.cols() }
    pub fn rows(&self) -> usize { self.buffer.rows() }
    pub(crate) fn screen_cursor_col(&self) -> Option<usize> {
        (self.cursor_col <= self.cols()).then(|| self.cursor_col.min(self.cols() - 1))
    }
    pub(crate) fn is_wrap_pending(&self) -> bool {
        self.wrap_pending || self.cursor_col == self.cols()
    }
    pub(crate) fn cancel_pending_wrap(&mut self) {
        self.wrap_pending = false;
        self.cursor_col = self.cursor_col.min(self.cols() - 1);
    }
    pub fn scrollback_len(&self) -> usize { self.scrollback.len() }
    pub fn scrollback_max(&self) -> usize { self.scrollback_max }

    pub(crate) fn title(&self) -> &str { &self.title }
    pub(crate) fn title_revision(&self) -> u64 { self.title_revision }

    pub(crate) fn set_title(&mut self, title: &str) {
        if self.title == title {
            return;
        }

        self.title.clear();
        self.title.push_str(title);
        self.title_revision = self.title_revision.wrapping_add(1);
    }

    pub(crate) fn reset_terminal_state(&mut self) {
        // Xterm keeps its resource-backed alternate-scroll mode across RIS.
        let alternate_scroll = self.alternate_scroll;
        let mut reset = Self::new(self.cols(), self.rows(), self.scrollback_max());
        reset.alternate_scroll = alternate_scroll;
        // RIS clears the title, but consumers still need a monotonic change signal.
        reset.title_revision = self
            .title_revision
            .wrapping_add(u64::from(!self.title.is_empty()));
        *self = reset;
    }

    pub fn scrollback_cell(&self, row: usize, col: usize) -> char {
        self.scrollback_cell_data(row, col).c
    }

    pub(crate) fn scrollback_cell_data(&self, row: usize, col: usize) -> &Cell {
        self.scrollback
            .get(row)
            .map(|row_data| self.project_scrollback_cell(row_data, col))
            .unwrap_or(&DEFAULT_CELL)
    }

    fn project_scrollback_cell<'a>(&self, row_data: &'a [Cell], col: usize) -> &'a Cell {
        if col >= self.cols() {
            return &DEFAULT_CELL;
        }
        let Some(cell) = row_data.get(col) else {
            return &DEFAULT_CELL;
        };
        let valid_wide_leader = !cell.flags.contains(CellFlags::WIDE)
            || (col + 1 < self.cols()
                && row_data
                    .get(col + 1)
                    .is_some_and(|next| next.flags.contains(CellFlags::WIDE_CONT)));
        let valid_wide_continuation = !cell.flags.contains(CellFlags::WIDE_CONT)
            || (col > 0
                && row_data
                    .get(col - 1)
                    .is_some_and(|previous| previous.flags.contains(CellFlags::WIDE)));
        if valid_wide_leader && valid_wide_continuation {
            cell
        } else {
            &DEFAULT_CELL
        }
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
            cursor_col: self.screen_cursor_col().unwrap_or(self.cols() - 1),
        });
    }

    pub(crate) fn drain_terminal_events(&mut self) -> Vec<TerminalEvent> {
        let events = std::mem::take(&mut self.pending_terminal_events);
        self.pending_sixel_count = 0;
        self.pending_sixel_bytes = 0;
        events
    }

    pub(crate) fn has_pending_terminal_events(&self) -> bool {
        !self.pending_terminal_events.is_empty()
    }

    pub(crate) fn queue_sixel_image(&mut self, image: SixelImage) -> bool {
        self.queue_sixel_image_with_limit(image, MAX_PENDING_SIXEL_BYTES)
    }

    pub(crate) fn remaining_sixel_bytes(&self) -> usize {
        MAX_PENDING_SIXEL_BYTES.saturating_sub(self.pending_sixel_bytes)
    }

    pub(crate) fn has_sixel_queue_slot(&self) -> bool {
        self.pending_sixel_count < MAX_PENDING_SIXEL_IMAGES
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

        self.pending_terminal_events.push(TerminalEvent::SixelGraphics {
            image,
            cursor_row: self.cursor_row,
            cursor_col: self.screen_cursor_col().unwrap_or(self.cols() - 1),
        });
        self.pending_sixel_count += 1;
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
        let cols = self.cols();

        // Under stable dimensions `cursor_col == cols` is the delayed-wrap
        // sentinel. `wrap_pending` keeps the LCF independent from the physical
        // column when a resize moves the right margin before the next print.
        if self.is_wrap_pending() || self.cursor_col >= cols {
            self.wrap_pending = false;
            if self.auto_wrap {
                self.carriage_return();
                self.newline();
            } else {
                self.cursor_col = self.cursor_col.min(cols - 1);
            }
        }

        // A double-width character cannot be represented without a
        // continuation cell. Real terminal windows are normally wider than
        // one column, but Grid permits a one-column size for robustness. Do
        // this after consuming delayed wrap because the character is still a
        // printable input even when its glyph cannot be represented.
        if char_width == 2 && cols < 2 {
            return;
        }

        // Wide char at last column: wrap first
        if char_width == 2 && self.cursor_col == cols - 1 {
            if !self.auto_wrap {
                // There is no valid continuation cell. xterm ignores a wide
                // character in this position when DECAWM is reset.
                return;
            }

            // The glyph is written on the next line; existing content at the
            // right margin remains intact.
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
        } else {
            // Clear any wide char that we're overwriting. In insert mode the
            // shifted copy must stay intact; writing the insertion cells and
            // repairing the row below removes only split or truncated pairs.
            self.clear_wide_overlap(row, col, char_width.max(1));
        }

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

        if self.insert_mode {
            self.repair_wide_row(row);
        }
        self.dirty[row] = true;
        self.cursor_col += char_width;
        self.wrap_pending = self.cursor_col >= cols;
    }

    pub fn set_auto_wrap(&mut self, enabled: bool) {
        self.auto_wrap = enabled;
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
        let end = col.saturating_add(width).min(self.cols());
        for c in col..end {
            let cell = self.buffer.cell(row, c);
            if cell.flags.contains(CellFlags::WIDE) && c + 1 < self.cols() {
                let cont = self.buffer.cell_mut(row, c + 1);
                cont.c = ' ';
                cont.flags.remove(CellFlags::WIDE_CONT);
            }
        }
    }

    fn repair_wide_buffer_row(buffer: &mut Buffer, row: usize, template: Cell) {
        Self::repair_wide_cells(buffer.row_mut(row), template);
    }

    fn repair_wide_cells(cells: &mut [Cell], template: Cell) {
        for col in 0..cells.len() {
            let flags = cells[col].flags;
            let orphaned_leader = flags.contains(CellFlags::WIDE)
                && (col + 1 == cells.len()
                    || !cells[col + 1].flags.contains(CellFlags::WIDE_CONT));
            let orphaned_continuation = flags.contains(CellFlags::WIDE_CONT)
                && (col == 0 || !cells[col - 1].flags.contains(CellFlags::WIDE));
            if orphaned_leader || orphaned_continuation {
                cells[col] = template;
            }
        }
    }

    fn repair_wide_buffer(buffer: &mut Buffer, template: Cell) {
        for row in 0..buffer.rows() {
            Self::repair_wide_buffer_row(buffer, row, template);
        }
    }

    fn repair_wide_row(&mut self, row: usize) {
        let template = self.template_cell();
        Self::repair_wide_buffer_row(&mut self.buffer, row, template);
    }

    pub fn newline(&mut self) {
        self.cancel_pending_wrap();
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor_row < self.rows() - 1 {
            self.cursor_row += 1;
        }
    }

    pub fn carriage_return(&mut self) {
        self.wrap_pending = false;
        self.cursor_col = 0;
    }

    pub fn backspace(&mut self) {
        self.cancel_pending_wrap();
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        }
    }

    pub fn tab(&mut self) {
        // HT at the delayed-wrap sentinel is a no-op. After a resize, however,
        // the last-column flag may remain set while the physical cursor sits
        // inside the new margins; xterm moves that cursor to the next tab stop
        // without consuming the flag.
        if self.cursor_col >= self.cols() {
            return;
        }
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
            self.project_scrollback_cell(&self.scrollback[abs_row], col)
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
        self.alt_cursor = (
            self.cursor_row,
            self.screen_cursor_col().unwrap_or(self.cols() - 1),
        );
        self.alt_wrap_pending = self.is_wrap_pending();
        let cols = self.cols();
        let rows = self.rows();
        let primary = std::mem::replace(&mut self.buffer, Buffer::new(cols, rows));
        self.alt_buffer = Some(primary);
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.wrap_pending = false;
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
        self.cursor_col = self.alt_cursor.1.min(self.cols() - 1);
        self.wrap_pending = self.alt_wrap_pending;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows().saturating_sub(1);
        // Clear all image placements when leaving alt screen
        self.image_placements.clear();
        self.mark_all_dirty();
    }

    pub fn erase_in_line(&mut self, mode: u16) {
        self.cancel_pending_wrap();
        let row = self.cursor_row;
        let cols = self.cols();
        match mode {
            0 => self.erase_cell_range(row, self.cursor_col.min(cols), cols),
            1 => self.erase_cell_range(row, 0, self.cursor_col.min(cols - 1) + 1),
            2 => self.erase_cell_range(row, 0, cols),
            _ => {}
        }
    }

    pub(crate) fn erase_chars(&mut self, count: usize) {
        self.cancel_pending_wrap();
        let start = self.cursor_col.min(self.cols());
        let end = start.saturating_add(count).min(self.cols());
        self.erase_cell_range(self.cursor_row, start, end);
    }

    pub(crate) fn delete_chars(&mut self, count: usize) {
        self.cancel_pending_wrap();
        let row = self.cursor_row;
        let cols = self.cols();
        let col = self.cursor_col.min(cols);
        if col == cols {
            return;
        }
        let count = count.min(cols - col);
        self.clear_wide_overlap(row, col, count);
        for destination in col..cols {
            let source = destination.saturating_add(count);
            let cell = if source < cols {
                *self.buffer.cell(row, source)
            } else {
                self.template_cell()
            };
            *self.buffer.cell_mut(row, destination) = cell;
        }
        self.repair_wide_row(row);
        self.dirty[row] = true;
    }

    pub(crate) fn insert_blank_chars(&mut self, count: usize) {
        self.cancel_pending_wrap();
        let row = self.cursor_row;
        let cols = self.cols();
        let col = self.cursor_col.min(cols);
        if col == cols {
            return;
        }
        let count = count.min(cols - col);
        self.clear_wide_overlap(row, col, 0);
        for destination in (col..cols).rev() {
            let cell = if destination >= col + count {
                *self.buffer.cell(row, destination - count)
            } else {
                self.template_cell()
            };
            *self.buffer.cell_mut(row, destination) = cell;
        }
        self.repair_wide_row(row);
        self.dirty[row] = true;
    }

    fn erase_cell_range(&mut self, row: usize, start: usize, end: usize) {
        let start = start.min(self.cols());
        let end = end.min(self.cols());
        if start >= end {
            return;
        }
        self.clear_wide_overlap(row, start, end - start);
        let template = self.template_cell();
        for col in start..end {
            *self.buffer.cell_mut(row, col) = template;
        }
        self.repair_wide_row(row);
        self.dirty[row] = true;
    }

    pub fn erase_in_display(&mut self, mode: u16) {
        let template = self.template_cell();
        match mode {
            0 => {
                self.cancel_pending_wrap();
                self.erase_in_line(0);
                for row in self.cursor_row + 1..self.rows() {
                    self.buffer.clear_row(row, template);
                    self.dirty[row] = true;
                }
            }
            1 => {
                self.cancel_pending_wrap();
                self.erase_in_line(1);
                for row in 0..self.cursor_row {
                    self.buffer.clear_row(row, template);
                    self.dirty[row] = true;
                }
            }
            2 => {
                self.cancel_pending_wrap();
                for row in 0..self.rows() {
                    self.buffer.clear_row(row, template);
                    self.dirty[row] = true;
                }
            }
            3 => {
                if self.using_alt_screen {
                    return;
                }
                let viewport_changed = self.scroll_offset != 0;
                self.scrollback.clear();
                self.scroll_offset = 0;
                self.marks.erase_saved_lines(self.total_lines_pushed);
                self.total_lines_pushed = 0;
                if viewport_changed {
                    self.mark_all_dirty();
                }
            }
            _ => {}
        }
    }

    pub fn set_cursor_pos(&mut self, row: usize, col: usize) {
        self.wrap_pending = false;
        self.cursor_row = row.min(self.rows() - 1);
        self.cursor_col = col.min(self.cols() - 1);
    }

    pub fn move_cursor_up(&mut self, n: usize) {
        self.cancel_pending_wrap();
        self.cursor_row = self.cursor_row.saturating_sub(n);
    }

    pub fn move_cursor_down(&mut self, n: usize) {
        self.cancel_pending_wrap();
        self.cursor_row = (self.cursor_row + n).min(self.rows() - 1);
    }

    pub fn move_cursor_forward(&mut self, n: usize) {
        self.cancel_pending_wrap();
        self.cursor_col = (self.cursor_col + n).min(self.cols() - 1);
    }

    pub fn move_cursor_backward(&mut self, n: usize) {
        self.cancel_pending_wrap();
        self.cursor_col = self.cursor_col.saturating_sub(n);
    }

    pub(crate) fn advance_image_cursor(&mut self, cols: usize, rows: usize) {
        self.move_cursor_forward(cols);
        for _ in 0..rows {
            self.newline();
        }
    }

    pub fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        self.cancel_pending_wrap();
        let bottom = bottom.min(self.rows() - 1);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
            self.cursor_row = 0;
            self.cursor_col = 0;
        }
    }

    pub fn insert_lines(&mut self, count: usize) {
        self.cancel_pending_wrap();
        if self.cursor_row >= self.scroll_top && self.cursor_row <= self.scroll_bottom {
            let old_top = self.scroll_top;
            self.scroll_top = self.cursor_row;
            self.scroll_down(count);
            self.scroll_top = old_top;
        }
    }

    pub fn delete_lines(&mut self, count: usize) {
        self.cancel_pending_wrap();
        if self.cursor_row >= self.scroll_top && self.cursor_row <= self.scroll_bottom {
            let old_top = self.scroll_top;
            self.scroll_top = self.cursor_row;
            self.scroll_up(count);
            self.scroll_top = old_top;
        }
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor_row = self.cursor_row;
        self.saved_cursor_col = self.screen_cursor_col().unwrap_or(self.cols() - 1);
        self.saved_wrap_pending = self.is_wrap_pending();
    }

    pub fn restore_cursor(&mut self) {
        self.cursor_row = self.saved_cursor_row.min(self.rows().saturating_sub(1));
        self.cursor_col = self.saved_cursor_col.min(self.cols() - 1);
        self.wrap_pending = self.saved_wrap_pending;
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        assert!(
            cols > 0 && rows > 0,
            "terminal grid dimensions must be non-zero"
        );
        let old_max_col = self.cols() - 1;
        let cursor_col = self
            .screen_cursor_col()
            .unwrap_or(old_max_col)
            .min(cols - 1);
        let wrap_pending = self.is_wrap_pending();
        let saved_cursor_col = self.saved_cursor_col.min(old_max_col).min(cols - 1);
        let alt_cursor_col = self.alt_cursor.1.min(old_max_col).min(cols - 1);
        self.buffer.resize(cols, rows);
        let template = self.template_cell();
        Self::repair_wide_buffer(&mut self.buffer, template);
        if let Some(ref mut alt) = self.alt_buffer {
            alt.resize(cols, rows);
            Self::repair_wide_buffer(alt, template);
        }
        let max_row = rows - 1;
        self.scroll_top = 0;
        self.scroll_bottom = max_row;
        self.cursor_row = self.cursor_row.min(max_row);
        self.cursor_col = cursor_col;
        self.wrap_pending = wrap_pending;
        self.saved_cursor_row = self.saved_cursor_row.min(max_row);
        self.saved_cursor_col = saved_cursor_col;
        self.alt_cursor.0 = self.alt_cursor.0.min(max_row);
        self.alt_cursor.1 = alt_cursor_col;
        self.dirty = vec![true; rows];
    }

    pub fn mark_all_dirty(&mut self) { for d in &mut self.dirty { *d = true; } }
    pub fn clear_dirty(&mut self) { for d in &mut self.dirty { *d = false; } }
    pub fn is_any_dirty(&self) -> bool { self.dirty.iter().any(|&d| d) }
}

#[cfg(test)]
mod tests {
    use super::{
        cell::{CellFlags, Color, UnderlineStyle},
        marks::PromptMarkKind,
        Grid, TerminalEvent,
    };
    use crate::parser::sixel::SixelImage;

    fn sixel_image(byte_len: usize) -> SixelImage {
        SixelImage {
            width: 1,
            height: (byte_len / 4) as u32,
            pixels: vec![0; byte_len],
        }
    }

    fn assert_wide_row_valid(grid: &Grid, row: usize) {
        for col in 0..grid.cols() {
            let flags = grid.buffer.cell(row, col).flags;
            if flags.contains(CellFlags::WIDE) {
                assert!(col + 1 < grid.cols());
                assert!(grid
                    .buffer
                    .cell(row, col + 1)
                    .flags
                    .contains(CellFlags::WIDE_CONT));
            }
            if flags.contains(CellFlags::WIDE_CONT) {
                assert!(col > 0);
                assert!(grid
                    .buffer
                    .cell(row, col - 1)
                    .flags
                    .contains(CellFlags::WIDE));
            }
        }
    }

    fn row_text(grid: &Grid, row: usize) -> String {
        (0..grid.cols())
            .map(|column| grid.buffer.cell(row, column).c)
            .collect()
    }

    #[test]
    fn erase_saved_lines_preserves_screen_and_rebases_current_marks() {
        let mut grid = Grid::new(4, 3, 10);
        grid.marks
            .push(PromptMarkKind::PromptStart, grid.current_absolute_row());
        for c in ['o', 'l', 'd'] {
            grid.put_char(c);
        }
        grid.carriage_return();
        grid.newline();

        grid.marks
            .push(PromptMarkKind::PromptStart, grid.current_absolute_row());
        for c in ['m', 'i', 'd'] {
            grid.put_char(c);
        }
        grid.carriage_return();
        grid.newline();

        grid.marks
            .push(PromptMarkKind::PromptStart, grid.current_absolute_row());
        for c in ['b', 'o', 't'] {
            grid.put_char(c);
        }
        grid.carriage_return();
        grid.newline();

        grid.marks
            .push(PromptMarkKind::CommandStart, grid.current_absolute_row());
        for c in ['n', 'e', 'w'] {
            grid.put_char(c);
        }
        grid.fg = Color::Indexed(7);
        grid.bg = Color::Rgb(1, 2, 3);
        grid.flags = CellFlags::BOLD | CellFlags::ITALIC;
        grid.underline_style = UnderlineStyle::Curly;
        grid.underline_color = Color::Indexed(4);

        assert_eq!(grid.scrollback_len(), 1);
        assert_eq!(grid.total_lines_pushed, 1);
        grid.scroll_viewport_up(1);
        assert_eq!(grid.visible_cell(0, 0).c, 'o');
        grid.clear_dirty();
        let cursor = (grid.cursor_row, grid.cursor_col);
        let attributes = (
            grid.fg,
            grid.bg,
            grid.flags,
            grid.underline_style,
            grid.underline_color,
        );

        grid.erase_in_display(3);

        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(grid.scroll_offset, 0);
        assert_eq!(grid.total_lines_pushed, 0);
        assert_eq!(grid.current_absolute_row(), grid.cursor_row);
        assert_eq!((grid.cursor_row, grid.cursor_col), cursor);
        assert_eq!(row_text(&grid, 0), "mid ");
        assert_eq!(row_text(&grid, 1), "bot ");
        assert_eq!(row_text(&grid, 2), "new ");
        assert_eq!(
            (
                grid.fg,
                grid.bg,
                grid.flags,
                grid.underline_style,
                grid.underline_color,
            ),
            attributes
        );
        assert!(grid.dirty.iter().all(|dirty| *dirty));
        assert_eq!(grid.marks.visible_prompt_rows(0, 0, grid.rows()), [0, 1]);
        assert_eq!(grid.marks.prev_prompt(2), Some(1));
        assert_eq!(grid.marks.next_prompt(0), Some(1));
    }

    #[test]
    fn erase_saved_lines_at_bottom_does_not_dirty_unchanged_screen() {
        let mut grid = Grid::new(3, 2, 10);
        grid.put_char('a');
        grid.scroll_up(1);
        grid.put_char('b');
        grid.clear_dirty();
        let screen = [row_text(&grid, 0), row_text(&grid, 1)];

        grid.erase_in_display(3);

        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!([row_text(&grid, 0), row_text(&grid, 1)], screen);
        assert!(!grid.is_any_dirty());
    }

    #[test]
    fn erase_display_all_preserves_saved_lines() {
        let mut grid = Grid::new(3, 2, 10);
        for c in ['o', 'l', 'd'] {
            grid.put_char(c);
        }
        grid.carriage_return();
        grid.newline();
        for c in ['n', 'e', 'w'] {
            grid.put_char(c);
        }
        grid.scroll_up(1);
        grid.scroll_viewport_up(1);
        let total_lines_pushed = grid.total_lines_pushed;

        grid.erase_in_display(2);

        assert_eq!(grid.scrollback_len(), 1);
        assert_eq!(grid.scrollback_cell(0, 0), 'o');
        assert_eq!(grid.scroll_offset, 1);
        assert_eq!(grid.total_lines_pushed, total_lines_pushed);
        assert_eq!(row_text(&grid, 0), "   ");
        assert_eq!(row_text(&grid, 1), "   ");
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
    fn auto_wrap_defaults_to_delayed_wrap() {
        let mut grid = Grid::new(2, 2, 0);

        assert!(grid.auto_wrap);
        grid.put_char('a');
        grid.put_char('b');
        assert_eq!(grid.cursor_col, grid.cols());

        grid.put_char('c');

        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
        assert_eq!(grid.buffer.cell(0, 1).c, 'b');
        assert_eq!(grid.buffer.cell(1, 0).c, 'c');
    }

    #[test]
    fn delayed_wrap_scrolls_exactly_once_after_the_next_printable() {
        let mut grid = Grid::new(3, 2, 10);
        grid.set_cursor_pos(1, 0);

        for c in ['a', 'b', 'c'] {
            grid.put_char(c);
        }

        assert_eq!((grid.cursor_row, grid.cursor_col), (1, grid.cols()));
        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(grid.total_lines_pushed, 0);

        grid.put_char('d');

        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
        assert_eq!(grid.scrollback_len(), 1);
        assert_eq!(grid.total_lines_pushed, 1);
        assert_eq!(grid.buffer.cell(0, 2).c, 'c');
        assert_eq!(grid.buffer.cell(1, 0).c, 'd');
    }

    #[test]
    fn disabled_auto_wrap_overwrites_right_margin_without_scrolling() {
        let mut grid = Grid::new(3, 2, 10);
        grid.set_auto_wrap(false);
        grid.set_cursor_pos(1, 0);

        for c in ['a', 'b', 'c', 'd', 'e'] {
            grid.put_char(c);
        }

        assert!(!grid.auto_wrap);
        assert_eq!((grid.cursor_row, grid.cursor_col), (1, grid.cols()));
        assert_eq!(grid.buffer.cell(1, 0).c, 'a');
        assert_eq!(grid.buffer.cell(1, 1).c, 'b');
        assert_eq!(grid.buffer.cell(1, 2).c, 'e');
        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(grid.total_lines_pushed, 0);
    }

    #[test]
    fn auto_wrap_toggle_preserves_pending_margin_until_next_printable() {
        let mut grid = Grid::new(2, 2, 0);
        grid.put_char('a');
        grid.put_char('b');
        assert_eq!(grid.cursor_col, grid.cols());

        grid.set_auto_wrap(false);
        grid.set_auto_wrap(true);
        grid.put_char('c');

        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
        assert_eq!(grid.buffer.cell(0, 1).c, 'b');
        assert_eq!(grid.buffer.cell(1, 0).c, 'c');
    }

    #[test]
    fn disabled_auto_wrap_consumes_pending_margin_without_wrapping() {
        let mut grid = Grid::new(2, 2, 0);
        grid.put_char('a');
        grid.put_char('b');
        grid.set_auto_wrap(false);

        grid.put_char('c');

        assert_eq!((grid.cursor_row, grid.cursor_col), (0, grid.cols()));
        assert_eq!(grid.buffer.cell(0, 1).c, 'c');
        assert_eq!(grid.buffer.cell(1, 0).c, ' ');
    }

    #[test]
    fn disabled_auto_wrap_ignores_wide_char_at_right_margin() {
        let mut grid = Grid::new(3, 2, 0);
        grid.set_auto_wrap(false);
        for c in ['a', 'b', 'c'] {
            grid.put_char(c);
        }

        grid.put_char('日');

        let right = grid.buffer.cell(0, 2);
        assert_eq!(right.c, 'c');
        assert!(!right
            .flags
            .intersects(CellFlags::WIDE | CellFlags::WIDE_CONT));
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 2));
    }

    #[test]
    fn disabled_auto_wrap_keeps_wide_margin_cells_valid() {
        let mut grid = Grid::new(4, 2, 0);
        grid.set_auto_wrap(false);
        grid.set_cursor_pos(0, 2);

        grid.put_char('日');

        assert!(grid.buffer.cell(0, 2).flags.contains(CellFlags::WIDE));
        assert!(grid.buffer.cell(0, 3).flags.contains(CellFlags::WIDE_CONT));
        assert_eq!(grid.cursor_col, grid.cols());

        grid.put_char('x');

        assert_eq!(grid.buffer.cell(0, 2).c, ' ');
        assert!(!grid.buffer.cell(0, 2).flags.contains(CellFlags::WIDE));
        assert_eq!(grid.buffer.cell(0, 3).c, 'x');
        assert!(!grid.buffer.cell(0, 3).flags.contains(CellFlags::WIDE_CONT));
        assert_eq!(grid.cursor_col, grid.cols());
    }

    #[test]
    fn wide_wrap_preserves_existing_right_margin_content() {
        let mut grid = Grid::new(3, 2, 0);
        for c in ['a', 'b', 'c'] {
            grid.put_char(c);
        }
        grid.set_cursor_pos(0, 2);

        grid.put_char('日');

        assert_eq!(grid.buffer.cell(0, 2).c, 'c');
        assert_eq!(grid.buffer.cell(1, 0).c, '日');
        assert!(grid.buffer.cell(1, 0).flags.contains(CellFlags::WIDE));
        assert!(grid.buffer.cell(1, 1).flags.contains(CellFlags::WIDE_CONT));
        assert_wide_row_valid(&grid, 0);
        assert_wide_row_valid(&grid, 1);
    }

    #[test]
    fn wide_wrap_at_scroll_bottom_preserves_the_margin_before_scrolling() {
        let mut grid = Grid::new(3, 2, 10);
        grid.set_cursor_pos(1, 0);
        for c in ['a', 'b', 'c'] {
            grid.put_char(c);
        }
        grid.set_cursor_pos(1, 2);

        grid.put_char('日');

        assert_eq!(grid.scrollback_len(), 1);
        assert_eq!(grid.buffer.cell(0, 2).c, 'c');
        assert_eq!(grid.buffer.cell(1, 0).c, '日');
        assert_wide_row_valid(&grid, 0);
        assert_wide_row_valid(&grid, 1);
    }

    #[test]
    fn wide_wrap_from_a_continuation_keeps_both_rows_valid() {
        let mut grid = Grid::new(4, 2, 0);
        grid.set_auto_wrap(false);
        grid.set_cursor_pos(0, 2);
        grid.put_char('日');
        grid.put_char('日');
        assert_eq!(grid.cursor_col, 3);

        grid.set_auto_wrap(true);
        grid.put_char('本');

        assert_eq!(grid.buffer.cell(0, 2).c, '日');
        assert!(grid.buffer.cell(0, 2).flags.contains(CellFlags::WIDE));
        assert!(grid.buffer.cell(0, 3).flags.contains(CellFlags::WIDE_CONT));
        assert_eq!(grid.buffer.cell(1, 0).c, '本');
        assert!(grid.buffer.cell(1, 0).flags.contains(CellFlags::WIDE));
        assert!(grid.buffer.cell(1, 1).flags.contains(CellFlags::WIDE_CONT));
        assert_wide_row_valid(&grid, 0);
        assert_wide_row_valid(&grid, 1);
    }

    #[test]
    fn wide_aware_editing_never_leaves_an_orphaned_half() {
        type NamedGridEdit = (&'static str, fn(&mut Grid));
        let edits: [NamedGridEdit; 4] = [
            ("erase line", |grid| grid.erase_in_line(0)),
            ("erase chars", |grid| grid.erase_chars(1)),
            ("delete chars", |grid| grid.delete_chars(1)),
            ("insert chars", |grid| grid.insert_blank_chars(1)),
        ];

        for (name, edit) in edits {
            let mut grid = Grid::new(4, 2, 0);
            grid.set_auto_wrap(false);
            grid.set_cursor_pos(0, 2);
            grid.put_char('日');
            grid.put_char('日');
            assert_eq!(grid.cursor_col, 3, "setup failed for {name}");

            edit(&mut grid);

            assert_wide_row_valid(&grid, 0);
            assert_eq!(grid.buffer.cell(0, 2).c, ' ', "{name}");
            assert_eq!(grid.buffer.cell(0, 3).c, ' ', "{name}");
        }
    }

    #[test]
    fn zero_width_writes_clear_both_halves_of_an_overlapped_wide_character() {
        for column in [0, 1] {
            let mut grid = Grid::new(4, 1, 0);
            grid.put_char('日');
            grid.set_cursor_pos(0, column);

            grid.put_char('\u{301}');

            assert_eq!(grid.buffer.cell(0, column).c, '\u{301}');
            assert_wide_row_valid(&grid, 0);
            assert!(!grid.buffer.cell(0, 0).flags.contains(CellFlags::WIDE));
            assert!(!grid
                .buffer
                .cell(0, 1)
                .flags
                .contains(CellFlags::WIDE_CONT));
        }
    }

    #[test]
    fn one_column_grid_ignores_unrepresentable_wide_char() {
        for auto_wrap in [true, false] {
            let mut grid = Grid::new(1, 2, 0);
            grid.set_auto_wrap(auto_wrap);

            grid.put_char('日');

            assert_eq!(grid.buffer.cell(0, 0).c, ' ');
            assert!(!grid
                .buffer
                .cell(0, 0)
                .flags
                .intersects(CellFlags::WIDE | CellFlags::WIDE_CONT));
            assert_eq!((grid.cursor_row, grid.cursor_col), (0, 0));
        }
    }

    #[test]
    fn one_column_wide_char_consumes_pending_margin() {
        let mut wrapping = Grid::new(1, 2, 0);
        wrapping.put_char('x');
        assert_eq!(wrapping.cursor_col, wrapping.cols());

        wrapping.put_char('日');

        assert_eq!((wrapping.cursor_row, wrapping.cursor_col), (1, 0));
        assert_eq!(wrapping.buffer.cell(0, 0).c, 'x');
        assert_eq!(wrapping.buffer.cell(1, 0).c, ' ');

        let mut overwriting = Grid::new(1, 2, 0);
        overwriting.set_auto_wrap(false);
        overwriting.put_char('x');
        assert_eq!(overwriting.cursor_col, overwriting.cols());

        overwriting.put_char('日');

        assert_eq!((overwriting.cursor_row, overwriting.cursor_col), (0, 0));
        assert_eq!(overwriting.buffer.cell(0, 0).c, 'x');
        assert_eq!(overwriting.buffer.cell(1, 0).c, ' ');
    }

    #[test]
    fn screen_cursor_projects_pending_wrap_and_rejects_invalid_columns() {
        for auto_wrap in [true, false] {
            let mut grid = Grid::new(4, 1, 0);
            grid.set_auto_wrap(auto_wrap);
            grid.set_cursor_pos(0, 2);
            grid.put_char('日');

            assert_eq!(grid.cursor_col, grid.cols());
            assert_eq!(grid.screen_cursor_col(), Some(3));
            assert!(grid.buffer.cell(0, 3).flags.contains(CellFlags::WIDE_CONT));

            grid.cursor_col = grid.cols() + 1;
            assert_eq!(grid.screen_cursor_col(), None);
        }
    }

    #[test]
    fn restored_pending_margin_obeys_disabled_auto_wrap() {
        let mut grid = Grid::new(2, 2, 0);
        grid.put_char('a');
        grid.put_char('b');
        grid.save_cursor();
        grid.set_cursor_pos(0, 0);
        grid.set_auto_wrap(false);

        grid.restore_cursor();
        grid.put_char('c');

        assert_eq!((grid.cursor_row, grid.cursor_col), (0, grid.cols()));
        assert_eq!(grid.buffer.cell(0, 1).c, 'c');
        assert_eq!(grid.buffer.cell(1, 0).c, ' ');
    }

    #[test]
    fn primary_pending_margin_obeys_auto_wrap_changed_on_alt_screen() {
        let mut grid = Grid::new(2, 2, 0);
        grid.put_char('a');
        grid.put_char('b');
        grid.enter_alt_screen();
        grid.set_auto_wrap(false);

        grid.leave_alt_screen();
        grid.put_char('c');

        assert_eq!((grid.cursor_row, grid.cursor_col), (0, grid.cols()));
        assert_eq!(grid.buffer.cell(0, 1).c, 'c');
        assert_eq!(grid.buffer.cell(1, 0).c, ' ');
    }

    #[test]
    fn disabled_auto_wrap_remains_bounded_after_resize_and_insert() {
        let mut grid = Grid::new(4, 2, 0);
        grid.set_auto_wrap(false);
        for c in ['a', 'b', 'c', 'd'] {
            grid.put_char(c);
        }
        grid.resize(3, 2);
        grid.insert_mode = true;

        grid.put_char('x');

        assert_eq!((grid.cursor_row, grid.cursor_col), (0, grid.cols()));
        assert_eq!(grid.buffer.cell(0, 2).c, 'x');
        assert_eq!(grid.buffer.cell(1, 0).c, ' ');
    }

    #[test]
    fn resize_preserves_pending_wrap_at_the_projected_physical_column() {
        for (columns, expected_physical_column) in [(5, 2), (3, 2), (2, 1)] {
            let mut grid = Grid::new(3, 2, 0);
            for c in ['a', 'b', 'c'] {
                grid.put_char(c);
            }
            assert_eq!(grid.cursor_col, grid.cols());

            grid.resize(columns, 2);

            assert_eq!(grid.cursor_col, expected_physical_column);
            assert!(grid.is_wrap_pending());
            grid.put_char('X');

            assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
            assert_eq!(grid.buffer.cell(1, 0).c, 'X');
            assert_eq!(grid.scrollback_len(), 0);
        }
    }

    #[test]
    fn disabled_auto_wrap_after_grow_writes_at_the_old_physical_column() {
        let mut grid = Grid::new(3, 2, 0);
        for c in ['a', 'b', 'c'] {
            grid.put_char(c);
        }

        grid.resize(5, 2);
        grid.set_auto_wrap(false);
        grid.put_char('X');

        let first_row: String = (0..5).map(|col| grid.buffer.cell(0, col).c).collect();
        assert_eq!(first_row, "abX  ");
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 3));
        assert!(!grid.is_wrap_pending());
    }

    #[test]
    fn saved_and_alternate_cursors_keep_pending_wrap_across_resize() {
        let mut saved = Grid::new(3, 2, 0);
        for c in ['a', 'b', 'c'] {
            saved.put_char(c);
        }
        saved.save_cursor();
        saved.resize(5, 2);
        saved.set_cursor_pos(0, 0);

        saved.restore_cursor();
        assert_eq!(saved.cursor_col, 2);
        assert!(saved.is_wrap_pending());
        saved.put_char('X');
        assert_eq!((saved.cursor_row, saved.cursor_col), (1, 1));
        assert_eq!(saved.buffer.cell(1, 0).c, 'X');

        let mut alternate = Grid::new(3, 2, 0);
        for c in ['a', 'b', 'c'] {
            alternate.put_char(c);
        }
        alternate.enter_alt_screen();
        alternate.resize(5, 2);

        alternate.leave_alt_screen();
        assert_eq!(alternate.cursor_col, 2);
        assert!(alternate.is_wrap_pending());
        alternate.put_char('X');
        assert_eq!((alternate.cursor_row, alternate.cursor_col), (1, 1));
        assert_eq!(alternate.buffer.cell(1, 0).c, 'X');
    }

    #[test]
    fn resize_repairs_truncated_wide_chars_in_active_and_saved_buffers() {
        let mut active = Grid::new(4, 2, 0);
        active.set_cursor_pos(0, 2);
        active.put_char('日');

        active.resize(3, 2);

        assert_wide_row_valid(&active, 0);
        assert_eq!(active.buffer.cell(0, 2).c, ' ');

        let mut saved_primary = Grid::new(4, 2, 0);
        saved_primary.set_cursor_pos(0, 2);
        saved_primary.put_char('日');
        saved_primary.enter_alt_screen();

        saved_primary.resize(3, 2);
        saved_primary.leave_alt_screen();

        assert_wide_row_valid(&saved_primary, 0);
        assert_eq!(saved_primary.buffer.cell(0, 2).c, ' ');
    }

    #[test]
    fn resize_projects_scrollback_wide_chars_without_losing_hidden_history() {
        let mut grid = Grid::new(4, 2, 10);
        for c in ['a', 'b', 'c', 'd'] {
            grid.put_char(c);
        }
        grid.set_cursor_pos(1, 2);
        grid.put_char('日');
        grid.scroll_up(2);

        grid.resize(3, 2);
        grid.scroll_viewport_up(2);

        assert_eq!(grid.visible_cell(0, 2).c, 'c');
        let history_margin = grid.visible_cell(1, 2);
        assert_eq!(history_margin.c, ' ');
        assert!(!history_margin
            .flags
            .intersects(CellFlags::WIDE | CellFlags::WIDE_CONT));

        grid.resize(4, 2);

        assert_eq!(grid.visible_cell(0, 3).c, 'd');
        assert_eq!(grid.visible_cell(1, 2).c, '日');
        assert!(grid.visible_cell(1, 2).flags.contains(CellFlags::WIDE));
        assert!(grid
            .visible_cell(1, 3)
            .flags
            .contains(CellFlags::WIDE_CONT));
    }

    #[test]
    fn explicit_line_and_cursor_controls_cancel_pending_wrap() {
        let mut grid = Grid::new(3, 3, 0);
        for c in ['a', 'b', 'c'] {
            grid.put_char(c);
        }
        assert_eq!(grid.cursor_col, grid.cols());

        grid.newline();
        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 2));
        grid.put_char('x');

        assert_eq!((grid.cursor_row, grid.cursor_col), (1, grid.cols()));
        assert_eq!(grid.buffer.cell(1, 2).c, 'x');

        grid.backspace();
        assert_eq!(grid.cursor_col, 1);
        grid.move_cursor_down(1);
        assert_eq!((grid.cursor_row, grid.cursor_col), (2, 1));
    }

    #[test]
    fn insert_mode_is_applied_after_delayed_wrap() {
        let mut grid = Grid::new(3, 2, 0);
        for (column, c) in ['q', 'r', 's'].into_iter().enumerate() {
            grid.buffer.cell_mut(1, column).c = c;
        }
        for c in ['a', 'b', 'c'] {
            grid.put_char(c);
        }
        grid.insert_mode = true;

        grid.put_char('x');

        let second_row: String = (0..3).map(|col| grid.buffer.cell(1, col).c).collect();
        assert_eq!(second_row, "xqr");
        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
    }

    #[test]
    fn insert_mode_preserves_a_shifted_wide_character() {
        let mut grid = Grid::new(4, 1, 0);
        grid.put_char('日');
        grid.set_cursor_pos(0, 0);
        grid.insert_mode = true;

        grid.put_char('x');

        assert_eq!(grid.buffer.cell(0, 0).c, 'x');
        assert_eq!(grid.buffer.cell(0, 1).c, '日');
        assert!(grid.buffer.cell(0, 1).flags.contains(CellFlags::WIDE));
        assert!(grid
            .buffer
            .cell(0, 2)
            .flags
            .contains(CellFlags::WIDE_CONT));
        assert_wide_row_valid(&grid, 0);
    }

    #[test]
    fn insert_mode_can_shift_a_wide_character_by_two_cells() {
        let mut grid = Grid::new(5, 1, 0);
        grid.put_char('日');
        grid.set_cursor_pos(0, 0);
        grid.insert_mode = true;

        grid.put_char('本');

        assert_eq!(grid.buffer.cell(0, 0).c, '本');
        assert_eq!(grid.buffer.cell(0, 2).c, '日');
        assert!(grid.buffer.cell(0, 0).flags.contains(CellFlags::WIDE));
        assert!(grid
            .buffer
            .cell(0, 1)
            .flags
            .contains(CellFlags::WIDE_CONT));
        assert!(grid.buffer.cell(0, 2).flags.contains(CellFlags::WIDE));
        assert!(grid
            .buffer
            .cell(0, 3)
            .flags
            .contains(CellFlags::WIDE_CONT));
        assert_wide_row_valid(&grid, 0);
    }

    #[test]
    fn insert_mode_repairs_wide_pairs_split_at_the_cursor_or_margin() {
        let mut on_continuation = Grid::new(5, 1, 0);
        on_continuation.put_char('日');
        on_continuation.buffer.cell_mut(0, 2).c = 'a';
        on_continuation.buffer.cell_mut(0, 3).c = 'b';
        on_continuation.set_cursor_pos(0, 1);
        on_continuation.insert_mode = true;

        on_continuation.put_char('x');

        let continuation_row: String = (0..5)
            .map(|col| on_continuation.buffer.cell(0, col).c)
            .collect();
        assert_eq!(continuation_row, " x ab");
        assert_wide_row_valid(&on_continuation, 0);

        let mut at_margin = Grid::new(4, 1, 0);
        at_margin.set_cursor_pos(0, 2);
        at_margin.put_char('日');
        at_margin.set_cursor_pos(0, 1);
        at_margin.insert_mode = true;

        at_margin.put_char('x');

        let margin_row: String = (0..4)
            .map(|col| at_margin.buffer.cell(0, col).c)
            .collect();
        assert_eq!(margin_row, " x  ");
        assert_wide_row_valid(&at_margin, 0);
    }

    #[test]
    fn image_cursor_advance_moves_right_and_down() {
        let mut grid = Grid::new(10, 6, 0);
        grid.set_cursor_pos(1, 2);

        grid.advance_image_cursor(3, 2);

        assert_eq!((grid.cursor_row, grid.cursor_col), (3, 5));
    }

    #[test]
    fn pending_sixel_queue_enforces_and_resets_its_byte_budget() {
        let mut grid = Grid::new(2, 2, 0);

        grid.queue_response(b"ready".to_vec());
        assert!(grid.queue_sixel_image_with_limit(sixel_image(8), 12));
        assert!(grid.queue_sixel_image_with_limit(sixel_image(4), 12));
        assert_eq!(grid.pending_sixel_bytes, 12);
        assert_eq!(
            grid.remaining_sixel_bytes(),
            super::MAX_PENDING_SIXEL_BYTES - 12
        );
        assert!(!grid.queue_sixel_image_with_limit(sixel_image(4), 12));
        assert_eq!(grid.pending_sixel_count, 2);

        let drained = grid.drain_terminal_events();
        assert_eq!(drained.len(), 3);
        assert!(matches!(
            drained.first(),
            Some(TerminalEvent::Response(response)) if response == b"ready"
        ));
        assert!(drained[1..]
            .iter()
            .all(|event| matches!(event, TerminalEvent::SixelGraphics { .. })));
        assert_eq!(grid.pending_sixel_count, 0);
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
        assert_eq!(grid.pending_sixel_count, super::MAX_PENDING_SIXEL_IMAGES);

        let drained = grid.drain_terminal_events();
        assert_eq!(drained.len(), super::MAX_PENDING_SIXEL_IMAGES);
        assert!(drained
            .iter()
            .all(|event| matches!(event, TerminalEvent::SixelGraphics { .. })));
        assert_eq!(grid.pending_sixel_count, 0);
        assert_eq!(grid.pending_sixel_bytes, 0);
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
