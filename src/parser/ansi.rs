use super::State;
use crate::grid::cell::{CellFlags, Color, UnderlineStyle};
use crate::grid::marks::PromptMarkKind;
use crate::grid::{CharSet, CursorShape, CursorStyle, Grid, MouseEncoding, MouseTracking};
use crate::parser::kitty_graphics;
use crate::parser::sixel;

pub struct Parser {
    state: State,
    params: Vec<u16>,
    /// true if the separator before this param was ':' (sub-param), false if ';'
    param_is_sub: Vec<bool>,
    current_param: Option<u16>,
    current_is_sub: bool,
    intermediates: Vec<u8>,
    osc_data: Vec<u8>,
    apc_data: Vec<u8>,
    dcs_data: Vec<u8>,
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            params: Vec::new(),
            param_is_sub: Vec::new(),
            current_param: None,
            current_is_sub: false,
            intermediates: Vec::new(),
            osc_data: Vec::new(),
            apc_data: Vec::new(),
            dcs_data: Vec::new(),
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
            State::ApcString => self.apc_string(byte, grid),
            State::DcsPassthrough => self.dcs_passthrough(byte, grid),
        }
    }

    fn ground(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x1b => { self.state = State::Escape; }
            0x00..=0x1a | 0x1c..=0x1f => { self.execute(byte, grid); }
            0x20..=0x7e => { grid.put_char(byte as char); }
            0x80..=0xff => {
                if byte >= 0xc0 {
                    // Start of multi-byte UTF-8 — handled by Utf8Parser wrapper
                }
            }
            _ => {}
        }
    }

    fn escape(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            b'\\' => {
                // ST (String Terminator) — dispatch whatever string we were accumulating
                if !self.osc_data.is_empty() {
                    self.dispatch_osc(grid);
                } else if !self.apc_data.is_empty() {
                    self.dispatch_apc(grid);
                } else if !self.dcs_data.is_empty() {
                    self.dispatch_dcs(grid);
                }
                self.state = State::Ground;
            }
            b'_' => {
                // APC — Application Program Command (used by Kitty graphics)
                self.apc_data.clear();
                self.state = State::ApcString;
            }
            b'P' => {
                // DCS — Device Control String (used by Sixel)
                self.dcs_data.clear();
                self.state = State::DcsPassthrough;
            }
            b'[' => {
                self.params.clear();
                self.param_is_sub.clear();
                self.current_param = None;
                self.current_is_sub = false;
                self.intermediates.clear();
                self.state = State::CsiEntry;
            }
            b']' => {
                self.osc_data.clear();
                self.state = State::OscString;
            }
            b'7' => { grid.save_cursor(); self.state = State::Ground; }
            b'8' => { grid.restore_cursor(); self.state = State::Ground; }
            b'D' => { grid.newline(); self.state = State::Ground; }
            b'M' => {
                if grid.cursor_row == grid.scroll_top {
                    grid.scroll_down(1);
                } else if grid.cursor_row > 0 {
                    grid.cursor_row -= 1;
                }
                self.state = State::Ground;
            }
            b'c' => {
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
            _ => { self.state = State::Ground; }
        }
    }

    fn escape_intermediate(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x20..=0x2f => {
                self.intermediates.push(byte);
            }
            0x30..=0x7e => {
                // Handle DEC charset designation: ESC ( 0 / ESC ( B
                if self.intermediates.len() == 1 && self.intermediates[0] == b'(' {
                    match byte {
                        b'0' => grid.charset = CharSet::DecSpecial,
                        b'B' => grid.charset = CharSet::Ascii,
                        _ => {}
                    }
                }
                self.state = State::Ground;
            }
            _ => { self.state = State::Ground; }
        }
    }

    fn csi_entry(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            b'0'..=b'9' => {
                self.current_param = Some((byte - b'0') as u16);
                self.current_is_sub = false;
                self.state = State::CsiParam;
            }
            b';' => {
                self.params.push(0);
                self.param_is_sub.push(false);
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
            0x1b => { self.state = State::Escape; }
            _ => { self.state = State::Ground; }
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
                self.param_is_sub.push(self.current_is_sub);
                self.current_param = None;
                self.current_is_sub = false;
            }
            b':' => {
                self.params.push(self.current_param.unwrap_or(0));
                self.param_is_sub.push(self.current_is_sub);
                self.current_param = None;
                self.current_is_sub = true; // next param is a sub-param
            }
            0x20..=0x2f => {
                if let Some(p) = self.current_param.take() {
                    self.params.push(p);
                    self.param_is_sub.push(self.current_is_sub);
                }
                self.intermediates.push(byte);
                self.state = State::CsiIntermediate;
            }
            0x40..=0x7e => {
                if let Some(p) = self.current_param.take() {
                    self.params.push(p);
                    self.param_is_sub.push(self.current_is_sub);
                }
                self.dispatch_csi(byte, grid);
                self.state = State::Ground;
            }
            0x1b => { self.state = State::Escape; }
            _ => { self.state = State::Ground; }
        }
    }

    fn csi_intermediate(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x20..=0x2f => { self.intermediates.push(byte); }
            0x40..=0x7e => {
                if let Some(p) = self.current_param.take() {
                    self.params.push(p);
                    self.param_is_sub.push(self.current_is_sub);
                }
                self.dispatch_csi(byte, grid);
                self.state = State::Ground;
            }
            _ => { self.state = State::Ground; }
        }
    }

    fn osc_string(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x07 => { self.dispatch_osc(grid); self.state = State::Ground; }
            0x1b => { self.state = State::Escape; }
            _ => { self.osc_data.push(byte); }
        }
    }

    fn apc_string(&mut self, byte: u8, _grid: &mut Grid) {
        match byte {
            0x1b => { self.state = State::Escape; } // Will see '\' next for ST
            _ => {
                // Cap APC data at 64MB to prevent OOM
                if self.apc_data.len() < 64 * 1024 * 1024 {
                    self.apc_data.push(byte);
                }
            }
        }
    }

    fn dcs_passthrough(&mut self, byte: u8, _grid: &mut Grid) {
        match byte {
            0x1b => { self.state = State::Escape; } // Will see '\' next for ST
            _ => {
                // Cap DCS data at 64MB
                if self.dcs_data.len() < 64 * 1024 * 1024 {
                    self.dcs_data.push(byte);
                }
            }
        }
    }

    fn dispatch_apc(&mut self, grid: &mut Grid) {
        let data = std::mem::take(&mut self.apc_data);
        if data.is_empty() {
            return;
        }

        // Check if this is a Kitty graphics command (starts with 'G')
        if data.first() == Some(&b'G') {
            if let Some(cmd) = kitty_graphics::parse_kitty_command(&data[1..]) {
                grid.pending_kitty_commands.push(cmd);
            }
        } else {
            log::trace!("Unknown APC sequence, first byte: {:?}", data.first());
        }
    }

    fn dispatch_dcs(&mut self, grid: &mut Grid) {
        let data = std::mem::take(&mut self.dcs_data);
        if data.is_empty() {
            return;
        }

        // Check for Sixel: DCS data starts with optional params then 'q'
        // Format: [params]q[sixel_data]
        // Also handle XTVERSION response: DCS > | ...
        if data.first() == Some(&b'>') && data.get(1) == Some(&b'|') {
            // This is a DCS response string (XTVERSION), ignore
            return;
        }

        // Find 'q' to identify Sixel
        let mut i = 0;
        while i < data.len() && (data[i].is_ascii_digit() || data[i] == b';') {
            i += 1;
        }
        if i < data.len() && data[i] == b'q' {
            // This is a Sixel image
            let sixel_data = &data[i + 1..]; // Everything after 'q'
            match sixel::decode_sixel(sixel_data) {
                Ok(image) => {
                    grid.pending_sixel_images.push(image);
                }
                Err(e) => {
                    log::warn!("Failed to decode Sixel image: {e}");
                }
            }
        } else {
            log::trace!("Unknown DCS sequence");
        }
    }

    fn dispatch_osc(&mut self, grid: &mut Grid) {
        let data = std::mem::take(&mut self.osc_data);
        let data_str = match std::str::from_utf8(&data) {
            Ok(s) => s,
            Err(_) => return,
        };
        if let Some(sep_pos) = data_str.find(';') {
            let ps = &data_str[..sep_pos];
            let pt = &data_str[sep_pos + 1..];
            match ps {
                "0" | "2" => { grid.title = pt.to_string(); }
                "7" => {
                    if let Some(path) = pt.strip_prefix("file://") {
                        if let Some(slash_pos) = path.find('/') {
                            grid.cwd = path[slash_pos..].to_string();
                        }
                    } else {
                        grid.cwd = pt.to_string();
                    }
                }
                "10" => {
                    // Query foreground color
                    if pt == "?" && !grid.default_fg_hex.is_empty() {
                        let hex = &grid.default_fg_hex;
                        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(192);
                        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(192);
                        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(192);
                        let resp = format!("\x1b]10;rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}\x1b\\",
                            r, r, g, g, b, b);
                        grid.pending_responses.push(resp.into_bytes());
                    }
                }
                "11" => {
                    // Query background color
                    if pt == "?" && !grid.default_bg_hex.is_empty() {
                        let hex = &grid.default_bg_hex;
                        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(26);
                        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(26);
                        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(46);
                        let resp = format!("\x1b]11;rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}\x1b\\",
                            r, r, g, g, b, b);
                        grid.pending_responses.push(resp.into_bytes());
                    }
                }
                "133" => {
                    let abs_row = grid.current_absolute_row();
                    match pt.chars().next() {
                        Some('A') => { grid.marks.push(PromptMarkKind::PromptStart, abs_row); }
                        Some('B') => { grid.marks.push(PromptMarkKind::CommandStart, abs_row); }
                        Some('C') => { grid.marks.push(PromptMarkKind::CommandExecuted, abs_row); }
                        Some('D') => {
                            let exit_code = pt.get(2..)
                                .and_then(|s| s.parse::<i32>().ok())
                                .unwrap_or(0);
                            grid.marks.push(PromptMarkKind::CommandFinished { exit_code }, abs_row);
                        }
                        _ => {}
                    }
                }
                _ => { log::trace!("Ignoring OSC {ps}"); }
            }
        }
    }

    fn execute(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x0a | 0x0b | 0x0c => grid.newline(),
            0x0d => grid.carriage_return(),
            0x08 => grid.backspace(),
            0x09 => grid.tab(),
            0x07 => {}
            0x0e => { grid.charset = CharSet::DecSpecial; } // SO (Shift Out) → G1
            0x0f => { grid.charset = CharSet::Ascii; }      // SI (Shift In) → G0
            _ => {}
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8, grid: &mut Grid) {
        let params = &self.params;
        let has_question = self.intermediates.contains(&b'?');
        let has_gt = self.intermediates.contains(&b'>');
        let has_space = self.intermediates.contains(&0x20);

        match final_byte {
            // DECSCUSR — cursor shape (CSI Ps SP q)
            b'q' if has_space => {
                let ps = params.first().copied().unwrap_or(0);
                grid.cursor_style = match ps {
                    0 | 1 => CursorStyle { shape: CursorShape::Block, blinking: true },
                    2 => CursorStyle { shape: CursorShape::Block, blinking: false },
                    3 => CursorStyle { shape: CursorShape::Underline, blinking: true },
                    4 => CursorStyle { shape: CursorShape::Underline, blinking: false },
                    5 => CursorStyle { shape: CursorShape::Bar, blinking: true },
                    6 => CursorStyle { shape: CursorShape::Bar, blinking: false },
                    _ => CursorStyle::default(),
                };
            }

            // XTVERSION (CSI > q)
            b'q' if has_gt => {
                grid.pending_responses.push(b"\x1bP>|Kokuban 0.1.0\x1b\\".to_vec());
            }

            b'm' => self.handle_sgr(grid),

            b'H' | b'f' => {
                let row = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                let col = params.get(1).copied().unwrap_or(1).max(1) as usize - 1;
                grid.set_cursor_pos(row, col);
            }
            b'A' => { grid.move_cursor_up(params.first().copied().unwrap_or(1).max(1) as usize); }
            b'B' => { grid.move_cursor_down(params.first().copied().unwrap_or(1).max(1) as usize); }
            b'C' => { grid.move_cursor_forward(params.first().copied().unwrap_or(1).max(1) as usize); }
            b'D' => { grid.move_cursor_backward(params.first().copied().unwrap_or(1).max(1) as usize); }
            b'J' => { grid.erase_in_display(params.first().copied().unwrap_or(0)); }
            b'K' => { grid.erase_in_line(params.first().copied().unwrap_or(0)); }

            b'r' => {
                let top = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                let bottom = params.get(1).copied().unwrap_or(grid.rows() as u16).max(1) as usize - 1;
                grid.set_scroll_region(top, bottom);
            }
            b'L' => { grid.insert_lines(params.first().copied().unwrap_or(1).max(1) as usize); }
            b'M' => { grid.delete_lines(params.first().copied().unwrap_or(1).max(1) as usize); }
            b'G' => {
                let col = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                grid.cursor_col = col.min(grid.cols() - 1);
            }
            b'd' => {
                let row = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                grid.cursor_row = row.min(grid.rows() - 1);
            }
            b'E' => {
                grid.move_cursor_down(params.first().copied().unwrap_or(1).max(1) as usize);
                grid.cursor_col = 0;
            }
            b'F' => {
                grid.move_cursor_up(params.first().copied().unwrap_or(1).max(1) as usize);
                grid.cursor_col = 0;
            }
            b'P' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                let row = grid.cursor_row;
                let col = grid.cursor_col;
                let cols = grid.cols();
                for c in col..cols {
                    let src_char = if c + n < cols {
                        *grid.buffer.cell(row, c + n)
                    } else {
                        grid.template_cell()
                    };
                    *grid.buffer.cell_mut(row, c) = src_char;
                }
                grid.dirty[row] = true;
            }
            b'@' => {
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
            b'S' => { grid.scroll_up(params.first().copied().unwrap_or(1).max(1) as usize); }
            b'T' => { grid.scroll_down(params.first().copied().unwrap_or(1).max(1) as usize); }

            // Device status report
            b'n' => {
                let ps = params.first().copied().unwrap_or(0);
                if ps == 6 {
                    // Cursor position report
                    let resp = format!("\x1b[{};{}R", grid.cursor_row + 1, grid.cursor_col + 1);
                    grid.pending_responses.push(resp.into_bytes());
                }
            }

            // Device attributes
            b'c' => {
                if has_gt {
                    // DA2
                    grid.pending_responses.push(b"\x1b[>1;0;0c".to_vec());
                } else if params.is_empty() || params.first().copied().unwrap_or(0) == 0 {
                    // DA1 (62=VT220, 4=Sixel, 22=color)
                    grid.pending_responses.push(b"\x1b[?62;4;22c".to_vec());
                }
            }

            // Window operations
            b't' => {
                let ps = params.first().copied().unwrap_or(0);
                match ps {
                    14 => {
                        // Report terminal size in pixels
                        let h = grid.rows() as u16 * grid.cell_pixel_height;
                        let w = grid.cols() as u16 * grid.cell_pixel_width;
                        let resp = format!("\x1b[4;{h};{w}t");
                        grid.pending_responses.push(resp.into_bytes());
                    }
                    16 => {
                        // Report cell size in pixels
                        let ch = grid.cell_pixel_height;
                        let cw = grid.cell_pixel_width;
                        let resp = format!("\x1b[6;{ch};{cw}t");
                        grid.pending_responses.push(resp.into_bytes());
                    }
                    _ => {}
                }
            }

            b'h' | b'l' => {
                let set = final_byte == b'h';
                if has_question {
                    // DEC private modes
                    for &param in params.iter() {
                        match param {
                            1049 => {
                                if set { grid.save_cursor(); grid.enter_alt_screen(); }
                                else { grid.leave_alt_screen(); grid.restore_cursor(); }
                            }
                            47 | 1047 => {
                                if set { grid.enter_alt_screen(); }
                                else { grid.leave_alt_screen(); }
                            }
                            25 => { grid.cursor_visible = set; }
                            1000 => { grid.mouse_tracking = if set { MouseTracking::Normal } else { MouseTracking::None }; }
                            1002 => { grid.mouse_tracking = if set { MouseTracking::ButtonEvent } else { MouseTracking::None }; }
                            1003 => { grid.mouse_tracking = if set { MouseTracking::AnyEvent } else { MouseTracking::None }; }
                            1004 => { grid.focus_events = set; }
                            1006 => { grid.mouse_encoding = if set { MouseEncoding::Sgr } else { MouseEncoding::Default }; }
                            2004 => { grid.bracketed_paste = set; }
                            _ => { log::trace!("Ignoring DEC private mode {param}"); }
                        }
                    }
                } else {
                    // ANSI modes (non-private)
                    for &param in params.iter() {
                        match param {
                            4 => { grid.insert_mode = set; }
                            _ => { log::trace!("Ignoring ANSI mode {param}"); }
                        }
                    }
                }
            }
            _ => {
                log::trace!(
                    "Unhandled CSI: params={:?} intermediates={:?} final={}",
                    params, self.intermediates, final_byte as char
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
            grid.underline_style = UnderlineStyle::None;
            grid.underline_color = Color::Default;
            return;
        }

        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    grid.fg = Color::Default;
                    grid.bg = Color::Default;
                    grid.flags = CellFlags::empty();
                    grid.underline_style = UnderlineStyle::None;
                    grid.underline_color = Color::Default;
                }
                1 => grid.flags.insert(CellFlags::BOLD),
                3 => grid.flags.insert(CellFlags::ITALIC),
                4 => {
                    // Check for sub-parameter (colon syntax: 4:N)
                    if i + 1 < params.len() && self.is_sub_param(i + 1) {
                        let sub = params[i + 1];
                        grid.underline_style = match sub {
                            0 => UnderlineStyle::None,
                            1 => UnderlineStyle::Single,
                            2 => UnderlineStyle::Double,
                            3 => UnderlineStyle::Curly,
                            4 => UnderlineStyle::Dotted,
                            5 => UnderlineStyle::Dashed,
                            _ => UnderlineStyle::Single,
                        };
                        if grid.underline_style != UnderlineStyle::None {
                            grid.flags.insert(CellFlags::UNDERLINE);
                        } else {
                            grid.flags.remove(CellFlags::UNDERLINE);
                        }
                        i += 1; // skip sub-param
                    } else {
                        grid.flags.insert(CellFlags::UNDERLINE);
                        grid.underline_style = UnderlineStyle::Single;
                    }
                }
                7 => grid.flags.insert(CellFlags::REVERSE),
                22 => grid.flags.remove(CellFlags::BOLD),
                23 => grid.flags.remove(CellFlags::ITALIC),
                24 => {
                    grid.flags.remove(CellFlags::UNDERLINE);
                    grid.underline_style = UnderlineStyle::None;
                }
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
                58 => {
                    // Underline color (58;2;R;G;B or 58;5;N)
                    if let Some(color) = self.parse_extended_color(params, &mut i) {
                        grid.underline_color = color;
                    }
                }
                59 => { grid.underline_color = Color::Default; }
                90..=97 => grid.fg = Color::Indexed((params[i] - 90 + 8) as u8),
                100..=107 => grid.bg = Color::Indexed((params[i] - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
    }

    fn is_sub_param(&self, idx: usize) -> bool {
        idx < self.param_is_sub.len() && self.param_is_sub[idx]
    }

    fn parse_extended_color(&self, params: &[u16], i: &mut usize) -> Option<Color> {
        if *i + 1 >= params.len() { return None; }
        match params[*i + 1] {
            5 => {
                if *i + 2 < params.len() {
                    *i += 2;
                    Some(Color::Indexed(params[*i] as u8))
                } else { None }
            }
            2 => {
                if *i + 4 < params.len() {
                    let r = params[*i + 2] as u8;
                    let g = params[*i + 3] as u8;
                    let b = params[*i + 4] as u8;
                    *i += 4;
                    Some(Color::Rgb(r, g, b))
                } else { None }
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
            // OSC, APC and DCS payloads are byte-oriented. Decoding their UTF-8
            // here would print the decoded character into the terminal grid
            // instead of keeping it inside the control string.
            if self.parser.state != State::Ground {
                self.utf8_len = 0;
                self.utf8_expected = 0;
                self.parser.advance(byte, grid);
                continue;
            }

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
                    self.utf8_expected = 0;
                    self.utf8_len = 0;
                    self.parser.advance(byte, grid);
                }
            } else if byte & 0x80 == 0 {
                self.parser.advance(byte, grid);
            } else if byte & 0xe0 == 0xc0 {
                self.utf8_buf[0] = byte; self.utf8_len = 1; self.utf8_expected = 1;
            } else if byte & 0xf0 == 0xe0 {
                self.utf8_buf[0] = byte; self.utf8_len = 1; self.utf8_expected = 2;
            } else if byte & 0xf8 == 0xf0 {
                self.utf8_buf[0] = byte; self.utf8_len = 1; self.utf8_expected = 3;
            } else {
                self.parser.advance(byte, grid);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Utf8Parser;
    use crate::grid::Grid;

    fn grid() -> Grid {
        Grid::new(40, 4, 100)
    }

    #[test]
    fn prints_utf8_split_across_reads() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        parser.feed(&[0xe6, 0x97], &mut grid);
        parser.feed(&[0xa5], &mut grid);

        assert_eq!(grid.buffer.cell(0, 0).c, '日');
        assert_eq!(grid.cursor_col, 2);
    }

    #[test]
    fn keeps_utf8_inside_bell_terminated_osc() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        parser.feed("\x1b]2;Kokuban 日本\x07".as_bytes(), &mut grid);

        assert_eq!(grid.title, "Kokuban 日本");
        assert_eq!(grid.buffer.cell(0, 0).c, ' ');
        assert_eq!(grid.cursor_col, 0);
    }

    #[test]
    fn keeps_utf8_inside_st_terminated_osc_split_across_reads() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        parser.feed("\x1b]0;黒".as_bytes(), &mut grid);
        parser.feed("板\x1b\\".as_bytes(), &mut grid);

        assert_eq!(grid.title, "黒板");
        assert_eq!(grid.buffer.cell(0, 0).c, ' ');
        assert_eq!(grid.cursor_col, 0);
    }
}
