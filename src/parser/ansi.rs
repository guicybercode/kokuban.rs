use super::State;
use crate::grid::cell::{CellFlags, Color};
use crate::grid::Grid;

pub struct Parser {
    state: State,
    params: Vec<u16>,
    current_param: Option<u16>,
    intermediates: Vec<u8>,
    osc_data: Vec<u8>,
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            params: Vec::new(),
            current_param: None,
            intermediates: Vec::new(),
            osc_data: Vec::new(),
        }
    }

    pub fn feed(&mut self, input: &[u8], grid: &mut Grid) {
        for &byte in input {
            self.advance(byte, grid);
        }
    }

    fn advance(&mut self, byte: u8, grid: &mut Grid) {
        match self.state {
            State::Ground => self.ground(byte, grid),
            State::Escape => self.escape(byte, grid),
            State::EscapeIntermediate => self.escape_intermediate(byte, grid),
            State::CsiEntry => self.csi_entry(byte, grid),
            State::CsiParam => self.csi_param(byte, grid),
            State::CsiIntermediate => self.csi_intermediate(byte, grid),
            State::OscString => self.osc_string(byte, grid),
        }
    }

    fn ground(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x1b => {
                self.state = State::Escape;
            }
            0x00..=0x1a | 0x1c..=0x1f => {
                self.execute(byte, grid);
            }
            0x20..=0x7e => {
                grid.put_char(byte as char);
            }
            0x80..=0xff => {
                // UTF-8 handling: decode multi-byte sequences
                // For simplicity, treat as Latin-1 for high bytes in ground state
                // Real UTF-8 would need a separate decoder
                if byte >= 0xc0 {
                    // Start of multi-byte: just print replacement for now
                    // A proper implementation would buffer UTF-8 bytes
                }
                // We handle UTF-8 via a separate path
            }
            _ => {}
        }
    }

    fn escape(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            b'[' => {
                self.params.clear();
                self.current_param = None;
                self.intermediates.clear();
                self.state = State::CsiEntry;
            }
            b']' => {
                self.osc_data.clear();
                self.state = State::OscString;
            }
            b'7' => {
                grid.save_cursor();
                self.state = State::Ground;
            }
            b'8' => {
                grid.restore_cursor();
                self.state = State::Ground;
            }
            b'D' => {
                grid.newline();
                self.state = State::Ground;
            }
            b'M' => {
                if grid.cursor_row == grid.scroll_top {
                    grid.scroll_down(1);
                } else if grid.cursor_row > 0 {
                    grid.cursor_row -= 1;
                }
                self.state = State::Ground;
            }
            b'c' => {
                // RIS - full reset
                let cols = grid.cols();
                let rows = grid.rows();
                let scrollback_max = grid.scrollback_max();
                *grid = Grid::new(cols, rows, scrollback_max);
                self.state = State::Ground;
            }
            0x20..=0x2f => {
                self.intermediates.clear();
                self.intermediates.push(byte);
                self.state = State::EscapeIntermediate;
            }
            _ => {
                // Unrecognized escape sequence, return to ground
                self.state = State::Ground;
            }
        }
    }

    fn escape_intermediate(&mut self, byte: u8, _grid: &mut Grid) {
        match byte {
            0x20..=0x2f => {
                self.intermediates.push(byte);
            }
            0x30..=0x7e => {
                // Final byte of escape sequence — discard for now
                self.state = State::Ground;
            }
            _ => {
                self.state = State::Ground;
            }
        }
    }

    fn csi_entry(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            b'0'..=b'9' => {
                self.current_param = Some((byte - b'0') as u16);
                self.state = State::CsiParam;
            }
            b';' => {
                self.params.push(0);
                self.state = State::CsiParam;
            }
            b'?' | b'>' | b'!' => {
                self.intermediates.push(byte);
                self.state = State::CsiParam;
            }
            0x20..=0x2f => {
                self.intermediates.push(byte);
                self.state = State::CsiIntermediate;
            }
            0x40..=0x7e => {
                self.dispatch_csi(byte, grid);
                self.state = State::Ground;
            }
            0x1b => {
                self.state = State::Escape;
            }
            _ => {
                self.state = State::Ground;
            }
        }
    }

    fn csi_param(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            b'0'..=b'9' => {
                let digit = (byte - b'0') as u16;
                self.current_param = Some(self.current_param.unwrap_or(0) * 10 + digit);
            }
            b';' => {
                self.params.push(self.current_param.unwrap_or(0));
                self.current_param = None;
            }
            b':' => {
                // Sub-parameter separator — treat like ';' for basic compat
                self.params.push(self.current_param.unwrap_or(0));
                self.current_param = None;
            }
            0x20..=0x2f => {
                if let Some(p) = self.current_param.take() {
                    self.params.push(p);
                }
                self.intermediates.push(byte);
                self.state = State::CsiIntermediate;
            }
            0x40..=0x7e => {
                if let Some(p) = self.current_param.take() {
                    self.params.push(p);
                }
                self.dispatch_csi(byte, grid);
                self.state = State::Ground;
            }
            0x1b => {
                self.state = State::Escape;
            }
            _ => {
                self.state = State::Ground;
            }
        }
    }

    fn csi_intermediate(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x20..=0x2f => {
                self.intermediates.push(byte);
            }
            0x40..=0x7e => {
                if let Some(p) = self.current_param.take() {
                    self.params.push(p);
                }
                self.dispatch_csi(byte, grid);
                self.state = State::Ground;
            }
            _ => {
                self.state = State::Ground;
            }
        }
    }

    fn osc_string(&mut self, byte: u8, _grid: &mut Grid) {
        match byte {
            0x07 => {
                // BEL terminates OSC
                self.state = State::Ground;
            }
            0x1b => {
                // ESC might start ST (ESC \)
                self.state = State::Escape;
            }
            _ => {
                self.osc_data.push(byte);
            }
        }
    }

    fn execute(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x0a | 0x0b | 0x0c => grid.newline(),       // LF, VT, FF
            0x0d => grid.carriage_return(),               // CR
            0x08 => grid.backspace(),                     // BS
            0x09 => grid.tab(),                           // HT
            0x07 => { /* BEL - ignore */ }
            _ => {}
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8, grid: &mut Grid) {
        let params = &self.params;
        let has_question = self.intermediates.contains(&b'?');

        match final_byte {
            b'm' => self.handle_sgr(grid),
            b'H' | b'f' => {
                let row = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                let col = params.get(1).copied().unwrap_or(1).max(1) as usize - 1;
                grid.set_cursor_pos(row, col);
            }
            b'A' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.move_cursor_up(n);
            }
            b'B' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.move_cursor_down(n);
            }
            b'C' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.move_cursor_forward(n);
            }
            b'D' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.move_cursor_backward(n);
            }
            b'J' => {
                let mode = params.first().copied().unwrap_or(0);
                grid.erase_in_display(mode);
            }
            b'K' => {
                let mode = params.first().copied().unwrap_or(0);
                grid.erase_in_line(mode);
            }
            b'r' => {
                let top = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                let bottom = params
                    .get(1)
                    .copied()
                    .unwrap_or(grid.rows() as u16)
                    .max(1) as usize
                    - 1;
                grid.set_scroll_region(top, bottom);
            }
            b'L' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.insert_lines(n);
            }
            b'M' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.delete_lines(n);
            }
            b'G' => {
                let col = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                grid.cursor_col = col.min(grid.cols() - 1);
            }
            b'd' => {
                let row = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                grid.cursor_row = row.min(grid.rows() - 1);
            }
            b'E' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.move_cursor_down(n);
                grid.cursor_col = 0;
            }
            b'F' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.move_cursor_up(n);
                grid.cursor_col = 0;
            }
            b'P' => {
                // Delete characters
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let row = grid.cursor_row;
                let col = grid.cursor_col;
                let cols = grid.cols();
                for c in col..cols {
                    let src_char = if c + n < cols {
                        let src = grid.buffer.cell(row, c + n);
                        *src
                    } else {
                        grid.template_cell()
                    };
                    *grid.buffer.cell_mut(row, c) = src_char;
                }
                grid.dirty[row] = true;
            }
            b'@' => {
                // Insert characters
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let row = grid.cursor_row;
                let col = grid.cursor_col;
                let cols = grid.cols();
                for c in (col..cols).rev() {
                    if c >= col + n {
                        let src = *grid.buffer.cell(row, c - n);
                        *grid.buffer.cell_mut(row, c) = src;
                    } else {
                        *grid.buffer.cell_mut(row, c) = grid.template_cell();
                    }
                }
                grid.dirty[row] = true;
            }
            b'X' => {
                // Erase characters
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let row = grid.cursor_row;
                let col = grid.cursor_col;
                let cols = grid.cols();
                let template = grid.template_cell();
                for c in col..(col + n).min(cols) {
                    *grid.buffer.cell_mut(row, c) = template;
                }
                grid.dirty[row] = true;
            }
            b'S' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.scroll_up(n);
            }
            b'T' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.scroll_down(n);
            }
            b'h' | b'l' => {
                let set = final_byte == b'h';
                if has_question {
                    for &param in params.iter() {
                        match param {
                            1049 => {
                                if set {
                                    grid.save_cursor();
                                    grid.enter_alt_screen();
                                } else {
                                    grid.leave_alt_screen();
                                    grid.restore_cursor();
                                }
                            }
                            47 | 1047 => {
                                if set {
                                    grid.enter_alt_screen();
                                } else {
                                    grid.leave_alt_screen();
                                }
                            }
                            25 => {
                                grid.cursor_visible = set;
                            }
                            2004 => {
                                grid.bracketed_paste = set;
                            }
                            _ => {
                                log::trace!("Ignoring DEC private mode {param}");
                            }
                        }
                    }
                }
            }
            b'n' => {
                // Device status report — ignore
            }
            b'c' => {
                // Device attributes — ignore
            }
            _ => {
                log::trace!(
                    "Unhandled CSI: params={:?} intermediates={:?} final={}",
                    params,
                    self.intermediates,
                    final_byte as char
                );
            }
        }
    }

    fn handle_sgr(&self, grid: &mut Grid) {
        let params = &self.params;
        if params.is_empty() {
            grid.fg = Color::Default;
            grid.bg = Color::Default;
            grid.flags = CellFlags::empty();
            return;
        }

        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    grid.fg = Color::Default;
                    grid.bg = Color::Default;
                    grid.flags = CellFlags::empty();
                }
                1 => grid.flags.insert(CellFlags::BOLD),
                3 => grid.flags.insert(CellFlags::ITALIC),
                4 => grid.flags.insert(CellFlags::UNDERLINE),
                7 => grid.flags.insert(CellFlags::REVERSE),
                22 => grid.flags.remove(CellFlags::BOLD),
                23 => grid.flags.remove(CellFlags::ITALIC),
                24 => grid.flags.remove(CellFlags::UNDERLINE),
                27 => grid.flags.remove(CellFlags::REVERSE),
                30..=37 => grid.fg = Color::Indexed((params[i] - 30) as u8),
                38 => {
                    if let Some(color) = self.parse_extended_color(params, &mut i) {
                        grid.fg = color;
                    }
                }
                39 => grid.fg = Color::Default,
                40..=47 => grid.bg = Color::Indexed((params[i] - 40) as u8),
                48 => {
                    if let Some(color) = self.parse_extended_color(params, &mut i) {
                        grid.bg = color;
                    }
                }
                49 => grid.bg = Color::Default,
                90..=97 => grid.fg = Color::Indexed((params[i] - 90 + 8) as u8),
                100..=107 => grid.bg = Color::Indexed((params[i] - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
    }

    fn parse_extended_color(&self, params: &[u16], i: &mut usize) -> Option<Color> {
        if *i + 1 >= params.len() {
            return None;
        }
        match params[*i + 1] {
            5 => {
                // 256-color: 38;5;N
                if *i + 2 < params.len() {
                    *i += 2;
                    Some(Color::Indexed(params[*i] as u8))
                } else {
                    None
                }
            }
            2 => {
                // True color: 38;2;R;G;B
                if *i + 4 < params.len() {
                    let r = params[*i + 2] as u8;
                    let g = params[*i + 3] as u8;
                    let b = params[*i + 4] as u8;
                    *i += 4;
                    Some(Color::Rgb(r, g, b))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// UTF-8 aware parser wrapper
pub struct Utf8Parser {
    pub parser: Parser,
    utf8_buf: [u8; 4],
    utf8_len: usize,
    utf8_expected: usize,
}

impl Utf8Parser {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
        }
    }

    pub fn feed(&mut self, input: &[u8], grid: &mut Grid) {
        for &byte in input {
            if self.utf8_expected > 0 {
                if byte & 0xc0 == 0x80 {
                    self.utf8_buf[self.utf8_len] = byte;
                    self.utf8_len += 1;
                    self.utf8_expected -= 1;
                    if self.utf8_expected == 0 {
                        if let Ok(s) = std::str::from_utf8(&self.utf8_buf[..self.utf8_len]) {
                            for c in s.chars() {
                                grid.put_char(c);
                            }
                        }
                    }
                } else {
                    // Invalid continuation, reset
                    self.utf8_expected = 0;
                    self.utf8_len = 0;
                    self.parser.advance(byte, grid);
                }
            } else if byte & 0x80 == 0 {
                // ASCII
                self.parser.advance(byte, grid);
            } else if byte & 0xe0 == 0xc0 {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_expected = 1;
            } else if byte & 0xf0 == 0xe0 {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_expected = 2;
            } else if byte & 0xf8 == 0xf0 {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_expected = 3;
            } else {
                // Invalid byte, pass through
                self.parser.advance(byte, grid);
            }
        }
    }
}
