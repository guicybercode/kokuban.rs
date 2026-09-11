pub mod buffer;
pub mod cell;
pub mod marks;
mod reflow;

use buffer::{Buffer, RowMetadata};
use cell::{Cell, CellFlags, Color, UnderlineStyle};
use marks::MarkIndex;
use reflow::{Cursor as ReflowCursor, RetainedRow};
use std::collections::VecDeque;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::parser::kitty_graphics::KittyCommand;
use crate::parser::sixel::{SixelImage, MAX_RGBA_BYTES as MAX_PENDING_SIXEL_BYTES};
use crate::graphics::{ImageId, ImagePlacement, PlacementMode};

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
    grapheme: None,
    fg: Color::Default,
    bg: Color::Default,
    flags: CellFlags::empty(),
    underline_style: UnderlineStyle::None,
    underline_color: Color::Default,
};

/// Test the boundary after an existing single grapheme without copying it.
fn scalar_extends_grapheme(previous: &str, c: char) -> bool {
    let last = previous.chars().next_back().expect("cell text is nonempty");
    let chunk_start = previous.len() - last.len_utf8();
    let mut bytes = [0; 8];
    last.encode_utf8(&mut bytes);
    c.encode_utf8(&mut bytes[last.len_utf8()..]);
    let chunk = std::str::from_utf8(&bytes[..last.len_utf8() + c.len_utf8()])
        .expect("both scalars were encoded as UTF-8");
    let mut cursor = GraphemeCursor::new(previous.len(), previous.len() + c.len_utf8(), true);
    loop {
        match cursor.is_boundary(chunk, chunk_start) {
            Ok(boundary) => return !boundary,
            Err(GraphemeIncomplete::PreContext(end)) => {
                // RI pairs, emoji ZWJ sequences and Indic conjuncts can need
                // more than the adjacent scalars. The existing cell owns all
                // preceding context, so lending its prefix needs no allocation.
                cursor.provide_context(&previous[..end], 0);
            }
            Err(_) => unreachable!("the chunk contains both sides of the boundary"),
        }
    }
}

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
struct SavedPrimaryHistory {
    cells: VecDeque<Vec<Cell>>,
    metadata: VecDeque<RowMetadata>,
    tail: Vec<RetainedRow>,
    total_lines: usize,
    cell_budget: usize,
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
    saved_cursor_retained_row: Option<usize>,
    pub scroll_top: usize,
    pub scroll_bottom: usize,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
    pub dirty: Vec<bool>,
    // Scrollback
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_metadata: VecDeque<RowMetadata>,
    resize_tail: Vec<RetainedRow>,
    saved_primary_history: Option<SavedPrimaryHistory>,
    scrollback_hard_lines: usize,
    scrollback_cells: usize,
    scrollback_cell_budget: usize,
    scrollback_max: usize,
    pub scroll_offset: usize,
    // Alternate screen
    alt_buffer: Option<Buffer>,
    alt_cursor: (usize, usize),
    alt_wrap_pending: bool,
    saved_primary_saved_cursor: Option<ReflowCursor>,
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
    // Coordinate changes that cannot be tracked by primary-screen scrollback.
    selection_revision: u64,
    screen_revision: u64,
    // Ordered protocol events to process before parsing subsequent PTY bytes.
    pending_terminal_events: Vec<TerminalEvent>,
    // Colors for query responses
    pub default_fg_hex: String,
    pub default_bg_hex: String,
    pending_sixel_count: usize,
    pending_sixel_bytes: usize,
    // Active image placements for this grid
    pub image_placements: Vec<ImagePlacement>,
    // Primary-screen placements hidden while the alternate screen is active.
    saved_primary_image_placements: Option<Vec<ImagePlacement>>,
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
            saved_cursor_retained_row: None,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            dirty: vec![true; rows],
            scrollback: VecDeque::new(),
            scrollback_metadata: VecDeque::new(),
            resize_tail: Vec::new(),
            saved_primary_history: None,
            scrollback_hard_lines: 0,
            scrollback_cells: 0,
            scrollback_cell_budget: scrollback_max.saturating_mul(cols),
            scrollback_max,
            scroll_offset: 0,
            alt_buffer: None,
            alt_cursor: (0, 0),
            alt_wrap_pending: false,
            saved_primary_saved_cursor: None,
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
            selection_revision: 0,
            screen_revision: 0,
            pending_terminal_events: Vec::new(),
            default_fg_hex: String::new(),
            default_bg_hex: String::new(),
            pending_sixel_count: 0,
            pending_sixel_bytes: 0,
            image_placements: Vec::new(),
            saved_primary_image_placements: None,
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
    pub(crate) fn retained_row_wrapped(&self, row: usize) -> bool {
        self.retained_row_metadata(row).wrapped
    }

    pub(crate) fn retained_row_len(&self, row: usize) -> usize {
        self.retained_row_metadata(row).len
    }

    pub(crate) fn retained_rows(&self) -> usize {
        self.scrollback.len() + self.rows() + self.resize_tail.len()
    }

    pub(crate) fn retained_cell_data(&self, row: usize, col: usize) -> &Cell {
        if row < self.scrollback.len() {
            self.scrollback_cell_data(row, col)
        } else if row - self.scrollback.len() < self.rows() {
            self.buffer.cell(row - self.scrollback.len(), col)
        } else {
            self.resize_tail.get(row - self.scrollback.len() - self.rows())
                .and_then(|row| row.cells.get(col)).unwrap_or(&DEFAULT_CELL)
        }
    }

    fn retained_row_metadata(&self, row: usize) -> RowMetadata {
        if row < self.scrollback.len() {
            self.scrollback_metadata.get(row).copied().unwrap_or_default()
        } else if row - self.scrollback.len() < self.rows() {
            self.buffer.row_metadata(row - self.scrollback.len())
        } else {
            self.resize_tail.get(row - self.scrollback.len() - self.rows())
                .map(|row| row.metadata).unwrap_or_default()
        }
    }

    pub fn scrollback_max(&self) -> usize { self.scrollback_max }
    pub(crate) fn selection_revision(&self) -> u64 { self.selection_revision }
    pub(crate) fn screen_revision(&self) -> u64 { self.screen_revision }

    /// Iterate every placement reference for cache retention, including a hidden primary screen.
    pub(crate) fn all_image_placements(&self) -> impl Iterator<Item = &ImagePlacement> {
        self.image_placements.iter().chain(
            self.saved_primary_image_placements
                .iter()
                .flat_map(|placements| placements.iter()),
        )
    }

    /// Remove stale Kitty references from a primary screen hidden by the alternate screen.
    pub(crate) fn remove_hidden_primary_kitty_placements(&mut self, image_id: ImageId) {
        if let Some(placements) = self.saved_primary_image_placements.as_mut() {
            placements.retain(|placement| {
                placement.image_id != image_id || placement.placement_id == 0
            });
        }
    }

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
        reset.selection_revision = self.selection_revision.wrapping_add(1);
        reset.screen_revision = self.screen_revision.wrapping_add(1);
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
            || self.cols() == 1
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
    grapheme: None,
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

    /// Write a run of printable ASCII, updating cursor and damage once per row.
    pub(crate) fn put_ascii(&mut self, mut text: &[u8]) {
        debug_assert!(text.iter().all(|byte| matches!(byte, b' '..=b'~')));
        if self.charset != CharSet::Ascii || self.insert_mode {
            for &byte in text {
                self.put_char(char::from(byte));
            }
            return;
        }

        if let Some((&first, rest)) = text.split_first() {
            if self.extend_grapheme(char::from(first)) { text = rest; }
        }
        let cols = self.cols();
        let template = Cell {
            c: ' ',
    grapheme: None,
            fg: self.fg,
            bg: self.bg,
            flags: self.flags & !(CellFlags::WIDE | CellFlags::WIDE_CONT),
            underline_style: self.underline_style,
            underline_color: self.underline_color,
        };
        while !text.is_empty() {
            // Let the regular writer consume delayed wrap, including a wrap
            // preserved across resize and overwrites with DECAWM disabled.
            if self.is_wrap_pending() || self.cursor_col >= cols {
                self.put_char(char::from(text[0]));
                text = &text[1..];
                continue;
            }

            let row = self.cursor_row;
            let col = self.cursor_col;
            let count = text.len().min(cols - col);
            // Interior cells are all replaced. Only the run's boundaries can
            // leave half of an existing wide character outside the write.
            self.clear_wide_overlap(row, col, 1);
            if count > 1 {
                self.clear_wide_overlap(row, col + count - 1, 1);
            }
            let cells = self.buffer.row_range_mut(row, col..col + count);
            for (cell, &byte) in cells.iter_mut().zip(&text[..count]) {
                *cell = Cell { c: char::from(byte), ..template.clone() };
            }
            self.buffer.mark_written(row, col + count);
            self.dirty[row] = true;
            self.cursor_col += count;
            self.wrap_pending = self.cursor_col >= cols;
            text = &text[count..];
        }
    }

    /// Place a character at the cursor, handling wide chars and DEC charset.
    pub fn put_char(&mut self, c: char) {
        let c = if self.charset == CharSet::DecSpecial {
            dec_special_map(c)
        } else {
            c
        };

        if self.extend_grapheme(c) {
            return;
        }
        let scalar_width = c.width();
        let char_width = scalar_width.unwrap_or(1).max(1).min(self.cols());
        let cols = self.cols();

        // Under stable dimensions `cursor_col == cols` is the delayed-wrap
        // sentinel. `wrap_pending` keeps the LCF independent from the physical
        // column when a resize moves the right margin before the next print.
        if self.is_wrap_pending() || self.cursor_col >= cols {
            self.wrap_pending = false;
            if self.auto_wrap {
                self.soft_wrap();
            } else {
                self.cursor_col = self.cursor_col.min(cols - 1);
            }
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
            self.soft_wrap();
        }

        let row = self.cursor_row;
        let col = self.cursor_col;

        // Insert mode: shift content right
        if self.insert_mode {
            let cols = self.cols();
            let shift = char_width;
            for c_idx in (col + shift..cols).rev() {
                let src = self.buffer.cell(row, c_idx - shift).clone();
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
        cell.grapheme = None;
        cell.fg = self.fg;
        cell.bg = self.bg;
        cell.flags = self.flags;
        cell.underline_style = self.underline_style;
        cell.underline_color = self.underline_color;

        if scalar_width == Some(2) {
            cell.flags.insert(CellFlags::WIDE);
            cell.flags.remove(CellFlags::WIDE_CONT);
            // Set continuation cell
            if col + 1 < self.cols() {
                let cont = self.buffer.cell_mut(row, col + 1);
                cont.c = '\0';
                cont.grapheme = None;
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
        self.buffer.mark_written(row, col + char_width.max(1));
        self.dirty[row] = true;
        self.cursor_col += char_width;
        self.wrap_pending = self.cursor_col >= cols;
    }

    /// Append a scalar only when UAX #29 keeps it in the preceding cluster.
    /// This runs before delayed wrap, so an accent received in another PTY read
    /// still belongs to the glyph at the right margin.
    fn extend_grapheme(&mut self, c: char) -> bool {
        if self.cursor_col == 0 { return false; }
        let row = self.cursor_row;
        let mut col = if self.is_wrap_pending() {
            self.cursor_col.min(self.cols() - 1)
        } else { self.cursor_col - 1 };
        if self.buffer.cell(row, col).flags.contains(CellFlags::WIDE_CONT) && col > 0 {
            col -= 1;
        }
        let previous = self.buffer.cell(row, col);
        if previous.grapheme.is_none() && previous.c.is_ascii() && c.is_ascii() { return false; }
        let previous_len = self.buffer.row_metadata(row).len;
        if col >= previous_len { return false; }
        let mut scalar_bytes = [0; 4];
        let previous_text = previous.grapheme.as_deref()
            .unwrap_or_else(|| previous.c.encode_utf8(&mut scalar_bytes));
        if !scalar_extends_grapheme(previous_text, c) { return false; }
        let mut text = String::with_capacity(previous_text.len() + c.len_utf8());
        text.push_str(previous_text);
        text.push(c);
        let old_width = if previous.flags.contains(CellFlags::WIDE)
            && col + 1 < self.cols() { 2 } else { 1 };
        let natural_width = text.width().clamp(1, 2);
        let new_width = natural_width.min(self.cols());
        let mut cell = previous.clone();
        cell.grapheme = Some(text.into());
        cell.flags.remove(CellFlags::WIDE | CellFlags::WIDE_CONT);
        if natural_width == 2 { cell.flags.insert(CellFlags::WIDE); }
        if new_width > old_width && col + new_width > self.cols() && self.auto_wrap {
            *self.buffer.cell_mut(row, col) = self.template_cell();
            let mut metadata = self.buffer.row_metadata(row);
            metadata.len = metadata.len.min(col);
            self.buffer.set_row_metadata(row, metadata);
            self.dirty[row] = true;
            self.soft_wrap();
            self.write_cluster_cell(cell, new_width);
            return true;
        }
        let new_width = new_width.min(self.cols() - col);
        if natural_width == 2 && new_width == 1 && self.cols() > 1 {
            cell.flags.remove(CellFlags::WIDE);
        }
        self.clear_wide_overlap(row, col, new_width);
        *self.buffer.cell_mut(row, col) = cell;
        if new_width == 2 && col + 1 < self.cols() {
            let mut continuation = self.template_cell();
            continuation.c = '\0';
            continuation.flags = CellFlags::WIDE_CONT;
            *self.buffer.cell_mut(row, col + 1) = continuation;
        }
        if new_width < old_width && previous_len == col + old_width {
            let mut metadata = self.buffer.row_metadata(row);
            metadata.len = col + new_width;
            self.buffer.set_row_metadata(row, metadata);
        }
        self.buffer.mark_written(row, col + new_width);
        self.cursor_col = (col + new_width).min(self.cols());
        self.wrap_pending = self.cursor_col == self.cols();
        self.dirty[row] = true;
        true
    }

    fn write_cluster_cell(&mut self, cell: Cell, width: usize) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        self.clear_wide_overlap(row, col, width);
        *self.buffer.cell_mut(row, col) = cell;
        if width == 2 {
            let mut continuation = self.template_cell();
            continuation.c = '\0';
            continuation.flags = CellFlags::WIDE_CONT;
            *self.buffer.cell_mut(row, col + 1) = continuation;
        }
        self.buffer.mark_written(row, col + width);
        self.cursor_col += width;
        self.wrap_pending = self.cursor_col >= self.cols();
        self.dirty[row] = true;
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
                prev.grapheme = None;
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
                cont.grapheme = None;
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
                && cells.len() > 1
                && (col + 1 == cells.len()
                    || !cells[col + 1].flags.contains(CellFlags::WIDE_CONT));
            let orphaned_continuation = flags.contains(CellFlags::WIDE_CONT)
                && (col == 0 || !cells[col - 1].flags.contains(CellFlags::WIDE));
            if orphaned_leader || orphaned_continuation {
                cells[col] = template.clone();
            }
        }
    }

    fn repair_wide_row(&mut self, row: usize) {
        let template = self.template_cell();
        Self::repair_wide_buffer_row(&mut self.buffer, row, template.clone());
    }

    fn soft_wrap(&mut self) {
        self.buffer.set_wrapped(self.cursor_row, true);
        self.carriage_return();
        self.advance_line();
    }

    pub fn newline(&mut self) {
        self.buffer.set_wrapped(self.cursor_row, false);
        self.advance_line();
    }

    fn advance_line(&mut self) {
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
        let count = count.min(self.scroll_bottom - self.scroll_top + 1);
        let save_scrollback = !self.using_alt_screen
            && self.scroll_top == 0
            && self.scroll_bottom == self.rows() - 1;
        if save_scrollback {
            for i in 0..count {
                let row_data = self.buffer.extract_row(i);
                let metadata = self.buffer.row_metadata(i);
                self.scrollback_cells += row_data.len();
                self.scrollback_hard_lines += usize::from(!metadata.wrapped);
                self.scrollback.push_back(row_data);
                self.scrollback_metadata.push_back(metadata);
                // Logical lines prevent reflow from consuming the line budget;
                // the cell budget also bounds an endless soft-wrapped stream.
                while self.scrollback_hard_lines > self.scrollback_max
                    || self.scrollback_cells > self.scrollback_cell_budget
                {
                    if let Some(evicted) = self.scrollback.pop_front() {
                        self.saved_cursor_retained_row = self.saved_cursor_retained_row.and_then(|row| row.checked_sub(1));
                        self.scrollback_cells -= evicted.len();
                        if self.scrollback_metadata.pop_front().is_some_and(|row| !row.wrapped) {
                            self.scrollback_hard_lines -= 1;
                        }
                    } else { break; }
                }
            }
            self.total_lines_pushed += count;
        } else if count != 0 {
            self.selection_revision = self.selection_revision.wrapping_add(1);
        }
        self.scroll_image_placements(count, true, save_scrollback);
        let template = self.template_cell();
        self.buffer.scroll_up(self.scroll_top, self.scroll_bottom, count, template);
        if self.scroll_top == 0 && self.scroll_bottom == self.rows() - 1 && !self.resize_tail.is_empty() {
            let reveal = count.min(self.resize_tail.len());
            let first_row = self.rows() - count;
            for (offset, retained) in self.resize_tail.drain(..reveal).enumerate() {
                self.buffer.row_mut(first_row + offset).clone_from_slice(&retained.cells);
                self.buffer.set_row_metadata(first_row + offset, retained.metadata);
            }
        }
        for row in self.scroll_top..=self.scroll_bottom {
            self.dirty[row] = true;
        }
    }

    pub fn scroll_down(&mut self, count: usize) {
        let count = count.min(self.scroll_bottom - self.scroll_top + 1);
        if count != 0 {
            self.selection_revision = self.selection_revision.wrapping_add(1);
        }
        self.scroll_image_placements(count, false, false);
        let template = self.template_cell();
        self.buffer.scroll_down(self.scroll_top, self.scroll_bottom, count, template);
        for row in self.scroll_top..=self.scroll_bottom {
            self.dirty[row] = true;
        }
    }

    fn scroll_image_placements(&mut self, count: usize, up: bool, save_scrollback: bool) {
        if count == 0 {
            return;
        }
        let top = self.scroll_top as i64;
        let end = self.scroll_bottom as i64 + 1;
        let full_screen = self.scroll_top == 0 && self.scroll_bottom == self.rows() - 1;
        let oldest_row = if save_scrollback { -(self.scrollback.len() as i64) } else { top };
        let delta = if up { -(count as i64) } else { count as i64 };
        let cell_width = f32::from(self.cell_pixel_width);
        let cell_height = f32::from(self.cell_pixel_height);
        self.image_placements.retain_mut(|placement| {
            let (row, _, _, rows) = placement.mode.signed_cell_rect(cell_width, cell_height);
            let bottom = row.saturating_add(i64::from(rows));
            if !save_scrollback {
                // Scrolling a region must leave unrelated history and fixed rows alone.
                if bottom <= top || row >= end {
                    return true;
                }
                // A single image cannot split around fixed rows. Discard intersecting
                // placements at margins instead of drawing over the fixed content.
                // Reverse scrolling also must not resurrect pixels lost above row zero.
                if (!full_screen && (row < top || bottom > end)) || (!up && row < top) {
                    return false;
                }
            }
            let moved_row = row.saturating_add(delta);
            let moved_bottom = bottom.saturating_add(delta);
            let retained = if save_scrollback || (full_screen && up) {
                moved_bottom > oldest_row && moved_row < end
            } else {
                moved_row >= top && moved_bottom <= end
            };
            if retained {
                let PlacementMode::Inline { row, .. } = &mut placement.mode;
                *row = moved_row;
            }
            retained
        });
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
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.screen_revision = self.screen_revision.wrapping_add(1);
        debug_assert!(self.saved_primary_image_placements.is_none());
        self.using_alt_screen = true;
        self.scroll_offset = 0;
        self.alt_cursor = (
            self.cursor_row,
            self.screen_cursor_col().unwrap_or(self.cols() - 1),
        );
        self.alt_wrap_pending = self.is_wrap_pending();
        self.saved_primary_saved_cursor = Some(ReflowCursor {
            row: self.saved_cursor_row, col: self.saved_cursor_col, pending: self.saved_wrap_pending, retained_row: self.saved_cursor_retained_row,
        });
        self.saved_cursor_row = 0;
        self.saved_cursor_col = 0;
        self.saved_cursor_retained_row = None;
        self.saved_wrap_pending = false;
        let cols = self.cols();
        let rows = self.rows();
        let primary = std::mem::replace(&mut self.buffer, Buffer::new(cols, rows));
        self.alt_buffer = Some(primary);
        self.saved_primary_history = Some(SavedPrimaryHistory {
            cells: std::mem::take(&mut self.scrollback),
            metadata: std::mem::take(&mut self.scrollback_metadata),
            tail: std::mem::take(&mut self.resize_tail),
            total_lines: self.total_lines_pushed,
            cell_budget: self.scrollback_cell_budget,
        });
        self.scrollback_hard_lines = 0;
        self.scrollback_cells = 0;
        self.scrollback_cell_budget = self.scrollback_max.saturating_mul(cols);
        self.total_lines_pushed = 0;
        self.saved_primary_image_placements = Some(std::mem::take(&mut self.image_placements));
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.wrap_pending = false;
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
        self.mark_all_dirty();
    }

    pub fn leave_alt_screen(&mut self) {
        if !self.using_alt_screen { return; }
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.screen_revision = self.screen_revision.wrapping_add(1);
        if let Some(primary) = self.alt_buffer.take() {
            self.buffer = primary;
        }
        if let Some(history) = self.saved_primary_history.take() {
            self.scrollback = history.cells;
            self.scrollback_metadata = history.metadata;
            self.resize_tail = history.tail;
            self.total_lines_pushed = history.total_lines;
            self.scrollback_cell_budget = history.cell_budget;
            self.recount_history();
        }
        if let Some(cursor) = self.saved_primary_saved_cursor.take() {
            self.saved_cursor_row = cursor.row;
            self.saved_cursor_col = cursor.col;
            self.saved_wrap_pending = cursor.pending;
            self.saved_cursor_retained_row = cursor.retained_row;
        }
        self.using_alt_screen = false;
        self.cursor_row = self.alt_cursor.0.min(self.rows().saturating_sub(1));
        self.cursor_col = self.alt_cursor.1.min(self.cols() - 1);
        self.wrap_pending = self.alt_wrap_pending;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows().saturating_sub(1);
        self.image_placements = self
            .saved_primary_image_placements
            .take()
            .unwrap_or_default();
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
        let metadata = self.buffer.row_metadata(row);
        self.buffer.set_row_metadata(row, RowMetadata { len: metadata.len.saturating_sub(count).max(col.min(metadata.len)), ..metadata });
        self.clear_wide_overlap(row, col, count);
        for destination in col..cols {
            let source = destination.saturating_add(count);
            let cell = if source < cols {
                self.buffer.cell(row, source).clone()
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
        let metadata = self.buffer.row_metadata(row);
        self.buffer.set_row_metadata(row, RowMetadata { len: (metadata.len.max(col) + count).min(cols), ..metadata });
        self.clear_wide_overlap(row, col, 0);
        for destination in (col..cols).rev() {
            let cell = if destination >= col + count {
                self.buffer.cell(row, destination - count).clone()
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
        let metadata = self.buffer.row_metadata(row);
        if end >= metadata.len {
            self.buffer.set_row_metadata(row, RowMetadata { len: metadata.len.min(start), wrapped: false });
        }
        self.clear_wide_overlap(row, start, end - start);
        let template = self.template_cell();
        for col in start..end {
            *self.buffer.cell_mut(row, col) = template.clone();
        }
        self.repair_wide_row(row);
        self.dirty[row] = true;
    }

    pub fn erase_in_display(&mut self, mode: u16) {
        let template = self.template_cell();
        match mode {
            0 => {
                self.resize_tail.clear();
                self.cancel_pending_wrap();
                self.erase_in_line(0);
                for row in self.cursor_row + 1..self.rows() {
                    self.buffer.clear_row(row, template.clone());
                    self.dirty[row] = true;
                }
            }
            1 => {
                self.cancel_pending_wrap();
                self.erase_in_line(1);
                for row in 0..self.cursor_row {
                    self.buffer.clear_row(row, template.clone());
                    self.dirty[row] = true;
                }
            }
            2 => {
                self.resize_tail.clear();
                self.cancel_pending_wrap();
                self.selection_revision = self.selection_revision.wrapping_add(1);
                for row in 0..self.rows() {
                    self.buffer.clear_row(row, template.clone());
                    self.dirty[row] = true;
                }
                let cell_width = f32::from(self.cell_pixel_width);
                let cell_height = f32::from(self.cell_pixel_height);
                self.image_placements.retain(|placement| {
                    let (row, _, _, rows) = placement.mode.signed_cell_rect(cell_width, cell_height);
                    row.saturating_add(i64::from(rows)) <= 0
                });
            }
            3 => {
                if self.using_alt_screen {
                    return;
                }
                let viewport_changed = self.scroll_offset != 0;
                self.selection_revision = self.selection_revision.wrapping_add(1);
                self.saved_cursor_retained_row = self.saved_cursor_retained_row.and_then(|row| row.checked_sub(self.scrollback.len()));
                self.scrollback.clear();
                self.scrollback_metadata.clear();
                self.scrollback_hard_lines = 0;
                self.scrollback_cells = 0;
                let cell_width = f32::from(self.cell_pixel_width);
                let cell_height = f32::from(self.cell_pixel_height);
                self.image_placements.retain(|placement| {
                    let (row, _, _, rows) = placement.mode.signed_cell_rect(cell_width, cell_height);
                    row.saturating_add(i64::from(rows)) > 0
                });
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
        self.saved_cursor_retained_row = None;
        self.saved_cursor_row = self.cursor_row;
        self.saved_cursor_col = self.screen_cursor_col().unwrap_or(self.cols() - 1);
        self.saved_wrap_pending = self.is_wrap_pending();
    }

    pub fn restore_cursor(&mut self) {
        if let Some(row) = self.saved_cursor_retained_row.take() {
            self.reveal_saved_cursor(row);
        }
        self.cursor_row = self.saved_cursor_row.min(self.rows().saturating_sub(1));
        self.cursor_col = self.saved_cursor_col.min(self.cols() - 1);
        self.wrap_pending = self.saved_wrap_pending;
    }

    /// Bring an offscreen saved text position back into the editable screen.
    /// The displaced viewport remains retained, just as it does during resize.
    fn reveal_saved_cursor(&mut self, retained_row: usize) {
        let old_history = self.scrollback.len();
        let rows = self.rows();
        let cols = self.cols();
        let mut retained: Vec<_> = self.scrollback.drain(..).zip(self.scrollback_metadata.drain(..))
            .map(|(cells, metadata)| RetainedRow { cells, metadata }).collect();
        retained.extend((0..rows).map(|row| RetainedRow {
            cells: self.buffer.extract_row(row), metadata: self.buffer.row_metadata(row),
        }));
        retained.append(&mut self.resize_tail);
        let retained_row = retained_row.min(retained.len() - 1);
        let start = retained_row.min(retained.len().saturating_sub(rows));
        for row in retained.drain(..start) {
            self.scrollback.push_back(row.cells);
            self.scrollback_metadata.push_back(row.metadata);
        }
        if retained.len() > rows { self.resize_tail = retained.split_off(rows); }
        retained.resize_with(rows, || RetainedRow::blank(cols));
        self.buffer = Buffer::from_retained_rows(cols, &retained);
        self.saved_cursor_row = retained_row - start;
        self.total_lines_pushed = self.total_lines_pushed.saturating_sub(old_history) + start;
        self.recount_history();
        self.scrollback_cell_budget = self.scrollback_cell_budget.max(self.scrollback_cells);
        self.scroll_offset = 0;
        self.selection_revision = self.selection_revision.wrapping_add(1);
        for placement in &mut self.image_placements {
            let PlacementMode::Inline { row, .. } = &mut placement.mode;
            *row += old_history as i64 - start as i64;
        }
        self.mark_all_dirty();
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        assert!(cols > 0 && rows > 0, "terminal grid dimensions must be non-zero");
        if (cols, rows) == (self.cols(), self.rows()) { return; }
        self.selection_revision = self.selection_revision.wrapping_add(1);
        let old_cols = self.cols();
        let mut cursors = [
            ReflowCursor { row: self.cursor_row, col: self.cursor_col.min(old_cols - 1), pending: self.is_wrap_pending(), retained_row: None },
            ReflowCursor { row: self.saved_cursor_row.min(self.rows() - 1), col: self.saved_cursor_col.min(old_cols - 1), pending: self.saved_wrap_pending, retained_row: self.saved_cursor_retained_row },
        ];
        let old_history = self.scrollback.len();
        Self::resize_screen(&mut self.buffer, &mut self.scrollback,
            &mut self.scrollback_metadata, &mut self.resize_tail, &mut cursors, cols, rows);
        self.total_lines_pushed = self.total_lines_pushed.saturating_sub(old_history) + self.scrollback.len();
        self.recount_history();
        // Resizing may move preexisting screen content into history, even when
        // history is disabled. Keep that snapshot; new output remains bounded.
        self.scrollback_cell_budget = self.scrollback_cell_budget.max(self.scrollback_cells);
        self.cursor_row = cursors[0].row;
        self.cursor_col = cursors[0].col;
        self.wrap_pending = cursors[0].pending;
        self.saved_cursor_row = cursors[1].row;
        self.saved_cursor_col = cursors[1].col;
        self.saved_wrap_pending = cursors[1].pending;
        self.saved_cursor_retained_row = cursors[1].retained_row;
        if let (Some(primary), Some(history)) =
            (self.alt_buffer.as_mut(), self.saved_primary_history.as_mut())
        {
            let mut cursor = [
                ReflowCursor { row: self.alt_cursor.0, col: self.alt_cursor.1, pending: self.alt_wrap_pending, retained_row: None },
                self.saved_primary_saved_cursor.unwrap_or(ReflowCursor { row: 0, col: 0, pending: false, retained_row: None }),
            ];
            let old_history = history.cells.len();
            Self::resize_screen(primary, &mut history.cells, &mut history.metadata, &mut history.tail, &mut cursor, cols, rows);
            history.total_lines = history.total_lines.saturating_sub(old_history) + history.cells.len();
            history.cell_budget = history.cell_budget.max(history.cells.iter().map(Vec::len).sum());
            self.alt_cursor = (cursor[0].row, cursor[0].col);
            self.alt_wrap_pending = cursor[0].pending;
            self.saved_primary_saved_cursor = Some(cursor[1]);
        }
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.dirty = vec![true; rows];
    }

    fn recount_history(&mut self) {
        self.scrollback_hard_lines = self.scrollback_metadata.iter().filter(|row| !row.wrapped).count();
        self.scrollback_cells = self.scrollback.iter().map(Vec::len).sum();
    }

    fn resize_screen(
        buffer: &mut Buffer,
        history: &mut VecDeque<Vec<Cell>>,
        history_metadata: &mut VecDeque<RowMetadata>,
        tail: &mut Vec<RetainedRow>,
        cursors: &mut [ReflowCursor],
        cols: usize,
        rows: usize,
    ) {
        let old_history = history.len();
        let mut source: Vec<_> = history.drain(..).zip(history_metadata.drain(..))
            .map(|(cells, metadata)| RetainedRow { cells, metadata }).collect();
        source.extend((0..buffer.rows()).map(|row| RetainedRow {
            cells: buffer.extract_row(row), metadata: buffer.row_metadata(row),
        }));
        source.append(tail);
        for cursor in cursors.iter_mut() { cursor.row = cursor.retained_row.take().unwrap_or(cursor.row + old_history); }
        let last_cursor_row = cursors.iter().map(|cursor| cursor.row).max().unwrap_or(0);
        while source.len() > last_cursor_row + 1 && source.last().is_some_and(|row| row.metadata.len == 0) {
            source.pop();
        }
        let result = reflow::reflow(&source, cols, cursors);
        let mut reflowed = result.rows;
        // If content exists below a cursor near the top, retain that content
        // after the viewport rather than moving the cursor into history.
        let screen_start = reflowed.len().saturating_sub(rows).min(result.cursors[0].row);
        for (cursor, mapped) in cursors.iter_mut().zip(result.cursors) {
            *cursor = ReflowCursor {
                row: mapped.row.saturating_sub(screen_start).min(rows - 1),
                retained_row: (!(screen_start..screen_start + rows).contains(&mapped.row)).then_some(mapped.row),
                ..mapped
            };
        }
        for row in reflowed.drain(..screen_start) {
            history.push_back(row.cells);
            history_metadata.push_back(row.metadata);
        }
        if reflowed.len() > rows { *tail = reflowed.split_off(rows); }
        reflowed.resize_with(rows, || RetainedRow::blank(cols));
        *buffer = Buffer::from_retained_rows(cols, &reflowed);
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
        scalar_extends_grapheme, Grid, TerminalEvent,
    };
    use crate::graphics::{ImagePlacement, InlineRenderSize, PlacementMode};
    use unicode_segmentation::UnicodeSegmentation;

    #[test]
    fn endless_soft_wrapping_respects_the_history_cell_budget() {
        for max in [0, 1, 3] {
            let mut grid = Grid::new(4, 2, max);
            grid.put_ascii(&vec![b'x'; 4096]);
            assert!(grid.scrollback_cells <= max * 4);
            assert!(grid.scrollback_len() <= max);
            assert_eq!(grid.scrollback_hard_lines, 0);
        }
    }

    #[test]
    fn soft_wrap_metadata_follows_scrollback_and_explicit_newlines() {
        let mut grid = Grid::new(4, 2, 10);
        grid.put_ascii(b"abc defghi");
        assert_eq!(grid.scrollback_len(), 1);
        assert!(grid.retained_row_wrapped(0));
        assert!(grid.retained_row_wrapped(1));
        assert_eq!(grid.retained_row_len(0), 4);
        assert_eq!(grid.retained_row_len(2), 2);
        grid.newline();
        assert!(!grid.retained_row_wrapped(2));
        grid.erase_in_display(2);
        assert_eq!(grid.retained_row_len(grid.scrollback_len()), 0);
    }

    #[test]
    fn row_lengths_distinguish_printed_spaces_from_wide_wrap_padding() {
        let mut grid = Grid::new(4, 2, 10);
        grid.put_ascii(b"ab ");
        grid.put_char('日');
        assert!(grid.retained_row_wrapped(0));
        assert_eq!(grid.retained_row_len(0), 3);
        assert_eq!(grid.retained_row_len(1), 2);
        grid.carriage_return();
        grid.erase_in_line(0);
        assert_eq!(grid.retained_row_len(1), 0);
    }

    use crate::parser::{ansi::Utf8Parser, sixel::SixelImage};

    #[test]
    fn ascii_batches_preserve_delayed_wrap_across_resize_and_wide_overwrites() {
        for auto_wrap in [false, true] {
            for new_cols in [2, 4, 8] {
                for col in 0..=4 {
                    let setup = || {
                        let mut grid = Grid::new(4, 3, 8);
                        for c in "日本日本日本".chars() {
                            grid.put_char(c);
                        }
                        grid.cursor_col = col;
                        grid.set_auto_wrap(auto_wrap);
                        grid.resize(new_cols, 3);
                        grid.clear_dirty();
                        grid
                    };
                    let mut batched = setup();
                    let mut scalar = setup();
                    let text = b"ABCDEFGHIJKLMN";
                    batched.put_ascii(text);
                    for &byte in text {
                        scalar.put_char(char::from(byte));
                    }
                    assert_eq!(format!("{batched:?}"), format!("{scalar:?}"),
                        "auto_wrap={auto_wrap}, new_cols={new_cols}, col={col}");
                }
            }
        }
    }

    #[test]
    fn ascii_batches_repair_wide_pairs_at_both_ends_of_each_run() {
        for col in 0..8 {
            for count in 0..=16 {
                let setup = || {
                    let mut grid = Grid::new(8, 3, 8);
                    for c in "日本語日本語日本語日本語".chars() {
                        grid.put_char(c);
                    }
                    grid.set_cursor_pos(0, col);
                    grid.clear_dirty();
                    grid
                };
                let mut batched = setup();
                let mut scalar = setup();
                let text = &b"abcdefghijklmnop"[..count];
                batched.put_ascii(text);
                for &byte in text {
                    scalar.put_char(char::from(byte));
                }
                assert_eq!(format!("{batched:?}"), format!("{scalar:?}"),
                    "col={col}, count={count}");
            }
        }
    }

    #[test]
    fn selection_and_paste_revisions_distinguish_repaint_from_screen_changes() {
        let mut grid = Grid::new(8, 4, 2);
        let selection = grid.selection_revision();
        let screen = grid.screen_revision();
        grid.erase_in_display(2);
        assert_ne!(grid.selection_revision(), selection);
        assert_eq!(grid.screen_revision(), screen);
        grid.resize(9, 4);
        assert_eq!(grid.screen_revision(), screen);
        let selection = grid.selection_revision();
        grid.scroll_up(3);
        assert_eq!(grid.selection_revision(), selection, "primary scrollback uses row rebasing");
        grid.enter_alt_screen();
        grid.leave_alt_screen();
        assert_ne!(grid.screen_revision(), screen, "a complete screen round-trip invalidates pending paste");
        let screen = grid.screen_revision();
        grid.reset_terminal_state();
        assert_ne!(grid.screen_revision(), screen);
    }

    #[test]
    fn selection_revision_survives_history_reset_and_partial_scroll() {
        let mut grid = Grid::new(8, 4, 2);
        grid.scroll_up(3);
        let before = grid.selection_revision();
        grid.erase_in_display(3);
        assert_ne!(grid.selection_revision(), before);
        assert_eq!(grid.total_lines_pushed, 0);
        let before = grid.selection_revision();
        grid.scroll_top = 1;
        grid.scroll_up(1);
        assert_ne!(grid.selection_revision(), before);
        let before = grid.selection_revision();
        grid.scroll_down(0);
        assert_eq!(grid.selection_revision(), before);
        grid.scroll_down(1);
        assert_ne!(grid.selection_revision(), before);
    }

    fn sixel_image(byte_len: usize) -> SixelImage {
        SixelImage {
            width: 1,
            height: (byte_len / 4) as u32,
            pixels: vec![0; byte_len],
        }
    }

    fn image_placement(image_id: u64) -> ImagePlacement {
        ImagePlacement {
            image_id,
            placement_id: image_id as u32,
            client_placement_id: Some(image_id as u32),
            mode: PlacementMode::Inline {
                row: 0,
                col: 0,
                cols: 1,
                rows: 1,
                x_offset: 0,
                y_offset: 0,
                render_size: InlineRenderSize::CellAnchored,
            },
            z_index: image_id as i32,
        }
    }

    fn image_ids<'a>(placements: impl IntoIterator<Item = &'a ImagePlacement>) -> Vec<u64> {
        placements
            .into_iter()
            .map(|placement| placement.image_id)
            .collect()
    }

    fn inline_at(image_id: u64, row: i64, rows: u32) -> ImagePlacement {
        let mut placement = image_placement(image_id);
        let PlacementMode::Inline { row: origin, rows: height, .. } = &mut placement.mode;
        *origin = row;
        *height = rows;
        placement
    }

    fn image_row(grid: &Grid, image_id: u64) -> Option<i64> {
        grid.image_placements.iter().find_map(|placement| {
            let PlacementMode::Inline { row, .. } = placement.mode;
            (placement.image_id == image_id).then_some(row)
        })
    }

    #[test]
    fn images_follow_newlines_into_bounded_primary_history() {
        let mut grid = Grid::new(6, 3, 2);
        grid.image_placements.push(inline_at(1, 1, 2));
        grid.set_cursor_pos(2, 0);

        for expected in [0, -1, -2, -3] {
            grid.newline();
            assert_eq!(image_row(&grid, 1), Some(expected));
            assert_eq!(grid.cursor_row, 2);
        }
        assert_eq!(grid.scrollback_len(), 2);
        grid.scroll_viewport_up(2);
        // The final retained image row appears at viewport row zero.
        assert_eq!(grid.image_placements[0].mode.pixel_rect(8.0, 16.0).1
            + grid.scroll_offset as f32 * 16.0, -16.0);

        grid.newline();
        assert!(grid.image_placements.is_empty());
    }

    #[test]
    fn partial_region_scroll_preserves_fixed_images_and_discards_crossing_images() {
        let mut grid = Grid::new(6, 6, 10);
        grid.image_placements = vec![
            inline_at(1, 0, 1),
            inline_at(2, 3, 1),
            inline_at(3, 1, 1),
            inline_at(4, 4, 2),
            inline_at(5, 5, 1),
        ];
        grid.set_scroll_region(1, 4);
        grid.scroll_up(1);

        assert_eq!(image_ids(&grid.image_placements), [1, 2, 5]);
        assert_eq!(image_row(&grid, 1), Some(0));
        assert_eq!(image_row(&grid, 2), Some(2));
        assert_eq!(image_row(&grid, 5), Some(5));
        assert_eq!(grid.scrollback_len(), 0);

        grid.set_scroll_region(0, 3);
        grid.scroll_up(usize::MAX);
        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(grid.total_lines_pushed, 0);
        assert_eq!(image_ids(&grid.image_placements), [5]);
    }

    #[test]
    fn alternate_images_scroll_out_without_moving_hidden_primary_images() {
        let mut grid = Grid::new(6, 4, 10);
        grid.image_placements.push(inline_at(1, 2, 1));
        grid.enter_alt_screen();
        grid.image_placements.push(inline_at(2, 0, 2));

        grid.scroll_up(1);
        assert_eq!(image_row(&grid, 2), Some(-1));
        grid.scroll_up(1);
        assert!(grid.image_placements.is_empty());
        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(image_ids(grid.all_image_placements()), [1]);

        grid.leave_alt_screen();
        assert_eq!(image_row(&grid, 1), Some(2));
    }

    #[test]
    fn scroll_retention_uses_current_native_pixel_height() {
        let mut grid = Grid::new(6, 6, 0);
        grid.cell_pixel_height = 10;
        let mut image = inline_at(1, 0, 1);
        let PlacementMode::Inline { render_size, .. } = &mut image.mode;
        *render_size = InlineRenderSize::NativePixels { width: 8, height: 40 };
        grid.image_placements.push(image);

        grid.scroll_up(3);
        assert_eq!(image_row(&grid, 1), Some(-3));
        assert_eq!(grid.image_placements[0].mode.effective_cell_rect(8.0, 10.0), (0, 0, 1, 1));
        grid.scroll_up(1);
        assert!(grid.image_placements.is_empty());
    }

    #[test]
    fn insert_delete_lines_move_images_and_leave_saved_history_in_place() {
        let mut grid = Grid::new(6, 5, 10);
        grid.image_placements = vec![inline_at(1, 0, 1), inline_at(2, 3, 1)];
        grid.scroll_up(1);
        assert_eq!(image_row(&grid, 1), Some(-1));
        grid.set_cursor_pos(1, 0);

        grid.insert_lines(1);
        assert_eq!(image_row(&grid, 1), Some(-1));
        assert_eq!(image_row(&grid, 2), Some(3));
        grid.delete_lines(1);
        assert_eq!(image_row(&grid, 2), Some(2));
        grid.scroll_down(3);
        assert_eq!(image_ids(&grid.image_placements), [1]);
        assert_eq!(image_row(&grid, 1), Some(-1));
    }

    #[test]
    fn erase_screen_and_saved_lines_keep_the_other_image_set() {
        let mut grid = Grid::new(6, 5, 10);
        grid.image_placements = vec![inline_at(1, 0, 1), inline_at(2, 3, 1)];
        grid.scroll_up(1);
        grid.erase_in_display(2);
        assert_eq!(image_ids(&grid.image_placements), [1]);
        grid.image_placements.push(inline_at(3, 1, 1));
        grid.erase_in_display(3);
        assert_eq!(image_ids(&grid.image_placements), [3]);
        assert_eq!(grid.scrollback_len(), 0);
    }

    #[test]
    fn zero_and_oversized_scroll_counts_match_image_and_text_movement() {
        let mut grid = Grid::new(6, 3, 10);
        grid.image_placements.push(inline_at(1, 2, 1));
        grid.scroll_up(0);
        grid.scroll_down(0);
        assert_eq!(image_row(&grid, 1), Some(2));
        grid.scroll_up(usize::MAX);
        assert_eq!(image_row(&grid, 1), Some(-1));
        assert_eq!(grid.scrollback_len(), 3);
        assert_eq!(grid.total_lines_pushed, 3);
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
    fn resize_preserves_cursor_rows_and_clamps_unused_columns() {
        let mut grid = Grid::new(12, 8, 100);
        grid.set_cursor_pos(7, 11);
        grid.save_cursor();

        grid.resize(4, 3);
        assert_eq!((grid.cursor_row, grid.cursor_col), (2, 3));

        grid.resize(20, 10);
        grid.restore_cursor();
        assert_eq!((grid.cursor_row, grid.cursor_col), (7, 3));
        grid.put_char('x');
        assert_eq!(grid.buffer.cell(7, 3).c, 'x');
    }

    #[test]
    fn resize_preserves_primary_logical_cursor_under_alt_screen() {
        let mut grid = Grid::new(12, 8, 100);
        grid.set_cursor_pos(7, 11);
        grid.enter_alt_screen();

        grid.resize(4, 3);
        grid.resize(20, 10);
        grid.leave_alt_screen();

        assert_eq!((grid.cursor_row, grid.cursor_col), (7, 3));
        grid.put_char('x');
        assert_eq!(grid.buffer.cell(7, 3).c, 'x');
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
    fn alt_screen_hides_and_restores_primary_image_placements() {
        let mut grid = Grid::new(2, 2, 0);
        grid.image_placements = vec![image_placement(11), image_placement(12)];

        grid.enter_alt_screen();

        assert!(grid.image_placements.is_empty());

        grid.leave_alt_screen();

        assert_eq!(image_ids(&grid.image_placements), [11, 12]);
    }

    #[test]
    fn retransmission_cleanup_removes_hidden_kitty_but_preserves_sixel() {
        let mut grid = Grid::new(2, 2, 0);
        let mut sixel = image_placement(11);
        sixel.placement_id = 0;
        sixel.client_placement_id = None;
        grid.image_placements = vec![image_placement(11), sixel];
        grid.enter_alt_screen();
        let mut active_replacement = image_placement(11);
        active_replacement.placement_id = 22;
        grid.image_placements.push(active_replacement);

        grid.remove_hidden_primary_kitty_placements(11);

        assert_eq!(grid.image_placements[0].placement_id, 22);
        assert_eq!(
            grid.all_image_placements()
                .map(|placement| placement.placement_id)
                .collect::<Vec<_>>(),
            [22, 0]
        );

        grid.leave_alt_screen();
        assert_eq!(grid.image_placements.len(), 1);
        assert_eq!(grid.image_placements[0].image_id, 11);
        assert_eq!(grid.image_placements[0].placement_id, 0);
    }

    #[test]
    fn csi_1049_round_trip_restores_primary_image_placements() {
        let mut parser = Utf8Parser::new();
        let mut grid = Grid::new(2, 2, 0);
        grid.image_placements.push(image_placement(11));

        parser.feed(b"\x1b[?1049h", &mut grid);
        assert!(grid.using_alt_screen);
        assert!(grid.image_placements.is_empty());
        grid.image_placements.push(image_placement(21));

        parser.feed(b"\x1b[?1049l", &mut grid);

        assert!(!grid.using_alt_screen);
        assert_eq!(image_ids(&grid.image_placements), [11]);
    }

    #[test]
    fn leaving_alt_screen_discards_alternate_image_placements() {
        let mut grid = Grid::new(2, 2, 0);
        grid.image_placements.push(image_placement(11));
        grid.enter_alt_screen();
        grid.image_placements.push(image_placement(21));

        grid.leave_alt_screen();

        assert_eq!(image_ids(&grid.image_placements), [11]);
        grid.enter_alt_screen();
        assert!(grid.image_placements.is_empty());
    }

    #[test]
    fn erase_display_all_clears_primary_image_placements() {
        let mut grid = Grid::new(2, 2, 0);
        grid.image_placements.push(image_placement(11));

        grid.erase_in_display(2);

        assert!(grid.image_placements.is_empty());
        assert!(grid.all_image_placements().next().is_none());
    }

    #[test]
    fn erase_display_all_on_alt_screen_preserves_primary_image_placements() {
        let mut grid = Grid::new(2, 2, 0);
        grid.image_placements.push(image_placement(11));
        grid.enter_alt_screen();
        grid.image_placements.push(image_placement(21));

        grid.erase_in_display(2);

        assert!(grid.image_placements.is_empty());
        assert_eq!(image_ids(grid.all_image_placements()), [11]);

        grid.leave_alt_screen();
        assert_eq!(image_ids(&grid.image_placements), [11]);
    }

    #[test]
    fn partial_and_scrollback_erases_preserve_active_image_placements() {
        for mode in [0, 1, 3] {
            let mut grid = Grid::new(2, 2, 0);
            grid.image_placements.push(image_placement(11));

            grid.erase_in_display(mode);

            assert_eq!(image_ids(&grid.image_placements), [11], "ED mode {mode}");
        }
    }

    #[test]
    fn alt_screen_image_lifecycle_ignores_repeated_transitions() {
        let mut grid = Grid::new(2, 2, 0);
        grid.image_placements.push(image_placement(11));
        grid.enter_alt_screen();
        grid.image_placements.push(image_placement(21));

        grid.enter_alt_screen();

        assert_eq!(image_ids(&grid.image_placements), [21]);
        let mut all_ids = image_ids(grid.all_image_placements());
        all_ids.sort_unstable();
        assert_eq!(all_ids, [11, 21]);

        grid.leave_alt_screen();
        assert_eq!(image_ids(&grid.image_placements), [11]);

        grid.leave_alt_screen();
        assert_eq!(image_ids(&grid.image_placements), [11]);
    }

    #[test]
    fn resize_preserves_screen_specific_image_placements() {
        let mut grid = Grid::new(4, 3, 0);
        grid.image_placements.push(image_placement(11));
        grid.enter_alt_screen();
        grid.image_placements.push(image_placement(21));

        grid.resize(2, 2);

        assert_eq!(image_ids(&grid.image_placements), [21]);
        let mut all_ids = image_ids(grid.all_image_placements());
        all_ids.sort_unstable();
        assert_eq!(all_ids, [11, 21]);

        grid.leave_alt_screen();
        assert_eq!(image_ids(&grid.image_placements), [11]);
    }

    #[test]
    fn reset_discards_active_and_hidden_image_placements() {
        let mut grid = Grid::new(2, 2, 0);
        grid.image_placements.push(image_placement(11));
        grid.enter_alt_screen();
        grid.image_placements.push(image_placement(21));

        grid.reset_terminal_state();

        assert!(!grid.using_alt_screen);
        assert!(grid.image_placements.is_empty());
        assert!(grid.all_image_placements().next().is_none());
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
    fn combining_marks_extend_wide_glyphs_without_overwriting_them() {
        let mut grid = Grid::new(4, 1, 0);
        grid.put_char('日');
        grid.put_char('\u{301}');
        assert_eq!(grid.buffer.cell(0, 0).text(), "日\u{301}");
        assert_eq!(grid.cursor_col, 2);
        assert_wide_row_valid(&grid, 0);
    }

    #[test]
    fn one_column_grid_retains_wide_characters() {
        for auto_wrap in [true, false] {
            let mut grid = Grid::new(1, 2, 0);
            grid.set_auto_wrap(auto_wrap);
            grid.put_char('日');
            assert_eq!(grid.buffer.cell(0, 0).text(), "日");
            assert_eq!(grid.buffer.cell(0, 0).display_width(), 2);
            assert_eq!((grid.cursor_row, grid.cursor_col), (0, 1));
        }
    }

    #[test]
    fn one_column_emoji_presentation_keeps_natural_width() {
        for auto_wrap in [true, false] {
            let mut grid = Grid::new(1, 2, 4);
            grid.set_auto_wrap(auto_wrap);
            grid.put_char('❤');
            grid.put_char('\u{fe0f}');
            let cell = grid.buffer.cell(0, 0);
            assert_eq!(cell.text(), "❤\u{fe0f}");
            assert_eq!(cell.display_width(), 2);
            assert!(cell.flags.contains(CellFlags::WIDE));
            assert!(!cell.flags.contains(CellFlags::WIDE_CONT));
            assert_eq!((grid.cursor_row, grid.cursor_col), (0, 1));
            assert!(grid.is_wrap_pending());
            assert_eq!(grid.retained_row_len(0), 1);
            assert_eq!(grid.scrollback_len(), 0);
        }
    }

    #[test]
    fn presentation_width_recovers_after_reflow_from_one_column() {
        for (text, natural_width) in [("❤\u{fe0f}", 2), ("♈\u{fe0e}", 1), ("👩🏽‍💻", 2)] {
            let mut grid = Grid::new(1, 3, 4);
            for c in text.chars() { grid.put_char(c); }
            for cols in [4, 1, 3] {
                grid.resize(cols, 3);
                let occupied = natural_width.min(cols);
                let cell = grid.buffer.cell(0, 0);
                assert_eq!(cell.text(), text, "cols={cols}");
                assert_eq!(cell.display_width(), natural_width, "cols={cols}");
                assert_eq!(cell.flags.contains(CellFlags::WIDE), natural_width == 2);
                if occupied == 2 {
                    assert!(grid.buffer.cell(0, 1).flags.contains(CellFlags::WIDE_CONT));
                } else if cols > 1 {
                    assert_eq!(grid.buffer.cell(0, 1).text(), " ");
                }
                assert_eq!((grid.cursor_row, grid.cursor_col), (0, occupied.min(cols - 1)));
                assert_eq!(grid.is_wrap_pending(), occupied == cols);
                assert_eq!(grid.retained_row_len(0), occupied);
                if cols > 1 { assert_wide_row_valid(&grid, 0); }
            }
        }
    }

    #[test]
    fn text_presentation_shrinks_wide_cells_without_erasing_following_text() {
        for (cols, following_text) in [(2, false), (4, true)] {
            let mut grid = Grid::new(cols, 2, 4);
            if following_text {
                grid.set_cursor_pos(0, 3);
                grid.put_char('Z');
                grid.set_cursor_pos(0, 0);
            }
            grid.put_char('♈');
            assert_eq!(grid.is_wrap_pending(), cols == 2);
            grid.put_char('\u{fe0e}');
            let cell = grid.buffer.cell(0, 0);
            assert_eq!(cell.text(), "♈\u{fe0e}");
            assert_eq!(cell.display_width(), 1);
            assert!(!cell.flags.intersects(CellFlags::WIDE | CellFlags::WIDE_CONT));
            assert_eq!(grid.buffer.cell(0, 1).text(), " ");
            assert!(!grid.buffer.cell(0, 1).flags.contains(CellFlags::WIDE_CONT));
            assert_eq!((grid.cursor_row, grid.cursor_col), (0, 1));
            assert!(!grid.is_wrap_pending());
            assert_eq!(grid.retained_row_len(0), if following_text { 4 } else { 1 });
            if following_text { assert_eq!(grid.buffer.cell(0, 3).c, 'Z'); }
            assert_wide_row_valid(&grid, 0);
            grid.put_char('X');
            assert_eq!(grid.buffer.cell(0, 0).text(), "♈\u{fe0e}");
            assert_eq!(grid.buffer.cell(0, 1).c, 'X');
        }
    }

    #[test]
    fn emoji_presentation_at_nowrap_margin_clips_only_occupied_width() {
        let mut grid = Grid::new(2, 2, 4);
        grid.set_auto_wrap(false);
        grid.put_char('a');
        grid.put_char('❤');
        grid.put_char('\u{fe0f}');
        let cell = grid.buffer.cell(0, 1);
        assert_eq!(cell.text(), "❤\u{fe0f}");
        assert_eq!(cell.display_width(), 2);
        assert!(!cell.flags.intersects(CellFlags::WIDE | CellFlags::WIDE_CONT));
        assert_eq!(grid.buffer.cell(0, 0).c, 'a');
        assert_eq!(grid.buffer.cell(1, 0).text(), " ");
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 2));
        assert!(grid.is_wrap_pending());
        assert_eq!(grid.retained_row_len(0), 2);
        assert_eq!(grid.scrollback_len(), 0);

        grid.resize(4, 2);
        assert_eq!(grid.buffer.cell(0, 1).text(), "❤\u{fe0f}");
        assert!(grid.buffer.cell(0, 1).flags.contains(CellFlags::WIDE));
        assert!(grid.buffer.cell(0, 2).flags.contains(CellFlags::WIDE_CONT));
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 3));
        assert!(!grid.is_wrap_pending());
        assert_eq!(grid.retained_row_len(0), 3);
        assert_wide_row_valid(&grid, 0);
    }

    #[test]
    fn one_column_wide_char_consumes_pending_margin() {
        let mut wrapping = Grid::new(1, 2, 0);
        wrapping.put_char('x');
        wrapping.put_char('日');
        assert_eq!((wrapping.cursor_row, wrapping.cursor_col), (1, 1));
        assert_eq!(wrapping.buffer.cell(0, 0).c, 'x');
        assert_eq!(wrapping.buffer.cell(1, 0).text(), "日");

        let mut overwriting = Grid::new(1, 2, 0);
        overwriting.set_auto_wrap(false);
        overwriting.put_char('x');
        overwriting.put_char('日');
        assert_eq!((overwriting.cursor_row, overwriting.cursor_col), (0, 1));
        assert_eq!(overwriting.buffer.cell(0, 0).text(), "日");
    }

    #[test]
    fn extended_graphemes_are_single_cells_across_scalar_writes() {
        for (text, width) in [("e\u{301}", 1), ("👩🏽‍💻", 2), ("🇧🇷", 2), ("1️⃣", 2)] {
            let mut grid = Grid::new(8, 2, 10);
            for scalar in text.chars() { grid.put_char(scalar); }
            assert_eq!(grid.buffer.cell(0, 0).text(), text);
            assert_eq!(grid.cursor_col, width);
            grid.put_char('X');
            assert_eq!(grid.buffer.cell(0, width).c, 'X');
        }
    }

    #[test]
    fn grapheme_boundary_uses_complete_unicode_context() {
        for (previous, next, extends) in [
            ("e", '\u{301}', true),
            ("e\u{301}", 'a', false),
            ("❤", '\u{fe0f}', true),
            ("1\u{fe0f}", '\u{20e3}', true),
            ("👩🏽\u{200d}", '💻', true),
            ("a\u{200d}", '💻', false),
            ("👩\u{200d}\u{301}", '💻', false),
            ("🇧", '🇷', true),
            ("🇧🇷", '🇺', false),
            ("क\u{94d}", 'ष', true),
            ("क\u{94d}\u{200d}", 'ष', true),
            ("क\u{93c}", 'ष', false),
            ("\u{600}", 'a', true),
            ("\u{600}", '\n', false),
            ("\r", '\n', true),
            ("\n", '\u{301}', false),
            ("\u{1100}", '\u{1161}', true),
            ("\u{1100}\u{1161}", '\u{11a8}', true),
        ] {
            assert_eq!(scalar_extends_grapheme(previous, next), extends,
                "previous={previous:?}, next={next:?}");
        }
    }

    #[test]
    fn split_grapheme_boundaries_match_concatenation() {
        let scalars = [
            'a', ' ', '\r', '\n', '\0', '\u{301}', '\u{308}', '\u{600}',
            '\u{903}', 'क', 'ष', '\u{93c}', '\u{94d}', '\u{200c}', '\u{200d}',
            '\u{1100}', '\u{1161}', '\u{11a8}', '가', '각', '❤', '\u{fe0e}',
            '\u{fe0f}', '\u{20e3}', '👩', '💻', '🏽', '🇧', '🇷', '\u{e0067}',
        ];
        let check = |previous: &str| {
            assert_eq!(previous.graphemes(true).count(), 1);
            for next in scalars {
                let combined = format!("{previous}{next}");
                assert_eq!(scalar_extends_grapheme(previous, next),
                    combined.graphemes(true).count() == 1,
                    "previous={previous:?}, next={next:?}");
            }
        };
        for first in scalars {
            check(&first.to_string());
            for second in scalars {
                let previous = format!("{first}{second}");
                if previous.graphemes(true).count() == 1 {
                    check(&previous);
                }
            }
        }
        for cluster in [
            "e\u{301}\u{308}", "👩🏽\u{200d}💻", "👩\u{200d}👩\u{200d}👧\u{200d}👦",
            "क\u{93c}\u{94d}\u{200d}ष\u{94d}क", "\u{600}\u{600}a\u{301}",
            "\u{1100}\u{1161}\u{11a8}", "🏴\u{e0067}\u{e0062}\u{e007f}",
        ] {
            for (offset, scalar) in cluster.char_indices() {
                check(&cluster[..offset + scalar.len_utf8()]);
            }
        }
    }

    #[test]
    fn combining_at_delayed_wrap_does_not_start_a_new_row() {
        let mut grid = Grid::new(2, 2, 10);
        grid.put_ascii(b"ae");
        grid.put_char('\u{301}');
        assert_eq!(grid.buffer.cell(0, 1).text(), "e\u{301}");
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 2));
        grid.put_char('X');
        assert_eq!(grid.buffer.cell(1, 0).c, 'X');
        assert!(grid.retained_row_wrapped(0));
    }

    #[test]
    fn emoji_presentation_growth_moves_the_complete_cluster_at_margin() {
        let mut grid = Grid::new(2, 2, 10);
        grid.put_char('a');
        grid.put_char('❤');
        grid.put_char('\u{fe0f}');
        assert_eq!(grid.buffer.cell(1, 0).text(), "❤️");
        assert_eq!(grid.retained_row_len(0), 1);
        assert!(grid.retained_row_wrapped(0));
        assert_wide_row_valid(&grid, 1);
    }

    #[test]
    fn emoji_presentation_growth_at_scroll_bottom_preserves_history() {
        let mut grid = Grid::new(2, 2, 4);
        grid.put_ascii(b"zz");
        grid.newline();
        grid.carriage_return();
        grid.put_char('a');
        grid.put_char('❤');
        grid.put_char('\u{fe0f}');
        assert_eq!(grid.scrollback_len(), 1);
        assert_eq!(grid.scrollback_cell(0, 0), 'z');
        assert_eq!(grid.scrollback_cell(0, 1), 'z');
        assert_eq!(grid.retained_row_len(0), 2);
        assert_eq!(grid.buffer.cell(0, 0).c, 'a');
        assert_eq!(grid.retained_row_len(1), 1);
        assert!(grid.retained_row_wrapped(1));
        assert_eq!(grid.buffer.cell(1, 0).text(), "❤\u{fe0f}");
        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 2));
        assert!(grid.is_wrap_pending());
        assert_eq!(grid.retained_row_len(2), 2);
        assert_wide_row_valid(&grid, 1);
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

        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 2));
        assert_eq!(row_text(&grid, 0), "abc");
        assert_eq!(row_text(&grid, 1), "dx ");
    }

    #[test]
    fn resize_maps_pending_wrap_to_the_end_of_reflowed_content() {
        for (columns, before, pending, after) in [
            (5, (0, 3), false, (0, 4)),
            (3, (0, 3), true, (1, 1)),
            (2, (1, 1), false, (1, 2)),
        ] {
            let mut grid = Grid::new(3, 2, 0);
            grid.put_ascii(b"abc");
            grid.resize(columns, 2);
            assert_eq!((grid.cursor_row, grid.cursor_col), before);
            assert_eq!(grid.is_wrap_pending(), pending);
            grid.put_char('X');
            assert_eq!((grid.cursor_row, grid.cursor_col), after);
            assert_eq!(grid.buffer.cell(after.0, after.1 - 1).c, 'X');
        }
    }

    #[test]
    fn disabled_auto_wrap_after_grow_appends_to_preserved_content() {
        let mut grid = Grid::new(3, 2, 0);
        for c in ['a', 'b', 'c'] {
            grid.put_char(c);
        }

        grid.resize(5, 2);
        grid.set_auto_wrap(false);
        grid.put_char('X');

        let first_row: String = (0..5).map(|col| grid.buffer.cell(0, col).c).collect();
        assert_eq!(first_row, "abcX ");
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 4));
        assert!(!grid.is_wrap_pending());
    }

    #[test]
    fn saved_and_alternate_cursors_follow_reflowed_content() {
        let mut saved = Grid::new(3, 2, 0);
        for c in ['a', 'b', 'c'] {
            saved.put_char(c);
        }
        saved.save_cursor();
        saved.resize(5, 2);
        saved.set_cursor_pos(0, 0);

        saved.restore_cursor();
        assert_eq!(saved.cursor_col, 3);
        assert!(!saved.is_wrap_pending());
        saved.put_char('X');
        assert_eq!((saved.cursor_row, saved.cursor_col), (0, 4));
        assert_eq!(saved.buffer.cell(0, 3).c, 'X');

        let mut alternate = Grid::new(3, 2, 0);
        for c in ['a', 'b', 'c'] {
            alternate.put_char(c);
        }
        alternate.enter_alt_screen();
        alternate.resize(5, 2);

        alternate.leave_alt_screen();
        assert_eq!(alternate.cursor_col, 3);
        assert!(!alternate.is_wrap_pending());
        alternate.put_char('X');
        assert_eq!((alternate.cursor_row, alternate.cursor_col), (0, 4));
        assert_eq!(alternate.buffer.cell(0, 3).c, 'X');
    }

    #[test]
    fn resize_reflows_wide_chars_in_active_and_saved_buffers() {
        let mut active = Grid::new(4, 2, 0);
        active.set_cursor_pos(0, 2);
        active.put_char('日');

        active.resize(3, 2);

        assert_wide_row_valid(&active, 0);
        assert_eq!(active.buffer.cell(0, 2).c, ' ');
        assert_eq!(active.buffer.cell(1, 0).text(), "日");
        assert_wide_row_valid(&active, 1);

        let mut saved_primary = Grid::new(4, 2, 0);
        saved_primary.set_cursor_pos(0, 2);
        saved_primary.put_char('日');
        saved_primary.enter_alt_screen();

        saved_primary.resize(3, 2);
        saved_primary.leave_alt_screen();

        assert_wide_row_valid(&saved_primary, 0);
        assert_eq!(saved_primary.buffer.cell(0, 2).c, ' ');
        assert_eq!(saved_primary.buffer.cell(1, 0).text(), "日");
        assert_wide_row_valid(&saved_primary, 1);
    }

    #[test]
    fn resize_reflows_every_scrollback_glyph_without_hidden_clipping() {
        let mut grid = Grid::new(4, 2, 10);
        grid.put_ascii(b"abcd");
        grid.set_cursor_pos(1, 2);
        grid.put_char('日');
        grid.scroll_up(2);
        for cols in [3, 1, 4, 8] {
            grid.resize(cols, 2);
            let mut text = String::new();
            for row in 0..grid.retained_rows() {
                for col in 0..grid.retained_row_len(row) {
                    let cell = grid.retained_cell_data(row, col);
                    if !cell.flags.contains(CellFlags::WIDE_CONT) { text.push_str(&cell.text()); }
                }
            }
            assert_eq!(text, "abcd  日", "cols={cols}");
        }
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
