use super::State;
use crate::grid::cell::{CellFlags, Color, UnderlineStyle};
use crate::grid::marks::PromptMarkKind;
use crate::grid::{CharSet, CursorShape, CursorStyle, Grid, MouseEncoding, MouseTracking};
use crate::parser::kitty_graphics;
use crate::parser::sixel;

const MAX_CSI_PARAMS: usize = 32;
const MAX_INTERMEDIATES: usize = 2;
const MAX_OSC_BYTES: usize = 64 * 1024;
// Kitty requires direct payloads to be chunked at 4 KiB. Keep room for
// control metadata and implementations that use a somewhat larger packet.
const MAX_APC_BYTES: usize = 16 * 1024;
const MAX_DCS_BYTES: usize = 16 * 1024 * 1024;

fn terminal_pixel_extent(cells: usize, cell_pixels: u16) -> u64 {
    u64::try_from(cells)
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::from(cell_pixels))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlStringKind {
    Osc,
    Apc,
    Dcs,
}

#[derive(Debug, Clone, Copy)]
struct ControlStringLimits {
    osc: usize,
    apc: usize,
    dcs: usize,
}

impl Default for ControlStringLimits {
    fn default() -> Self {
        Self {
            osc: MAX_OSC_BYTES,
            apc: MAX_APC_BYTES,
            dcs: MAX_DCS_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GraphicsSupport {
    pub(crate) kitty: bool,
    pub(crate) sixel: bool,
}

impl Default for GraphicsSupport {
    fn default() -> Self {
        Self {
            kitty: true,
            sixel: true,
        }
    }
}

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
    active_control_string: Option<ControlStringKind>,
    control_string_overflowed: bool,
    control_string_limits: ControlStringLimits,
    graphics_support: GraphicsSupport,
}

impl Parser {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_options(ControlStringLimits::default(), GraphicsSupport::default())
    }

    #[cfg(test)]
    fn with_control_string_limits(control_string_limits: ControlStringLimits) -> Self {
        Self::with_options(control_string_limits, GraphicsSupport::default())
    }

    fn with_graphics_support(graphics_support: GraphicsSupport) -> Self {
        Self::with_options(ControlStringLimits::default(), graphics_support)
    }

    fn with_options(
        control_string_limits: ControlStringLimits,
        graphics_support: GraphicsSupport,
    ) -> Self {
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
            active_control_string: None,
            control_string_overflowed: false,
            control_string_limits,
            graphics_support,
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
            State::EscapeIgnore => self.escape_ignore(byte),
            State::CsiEntry => self.csi_entry(byte, grid),
            State::CsiParam => self.csi_param(byte, grid),
            State::CsiIntermediate => self.csi_intermediate(byte, grid),
            State::CsiIgnore => self.csi_ignore(byte),
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
        if byte != b'\\' && self.active_control_string.is_some() {
            self.cancel_active_control_string();
        }

        match byte {
            b'\\' => {
                self.finish_active_control_string(grid);
                self.state = State::Ground;
            }
            b'_' => {
                // APC — Application Program Command (used by Kitty graphics)
                self.begin_control_string(ControlStringKind::Apc);
                self.state = State::ApcString;
            }
            b'P' => {
                // DCS — Device Control String (used by Sixel)
                self.begin_control_string(ControlStringKind::Dcs);
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
                self.begin_control_string(ControlStringKind::Osc);
                self.state = State::OscString;
            }
            0x1b => { self.state = State::Escape; }
            b'7' => { grid.save_cursor(); self.state = State::Ground; }
            b'8' => { grid.restore_cursor(); self.state = State::Ground; }
            b'D' => { grid.newline(); self.state = State::Ground; }
            b'M' => {
                grid.cancel_pending_wrap();
                if grid.cursor_row == grid.scroll_top {
                    grid.scroll_down(1);
                } else if grid.cursor_row > 0 {
                    grid.cursor_row -= 1;
                }
                self.state = State::Ground;
            }
            b'c' => {
                grid.reset_terminal_state();
                self.state = State::Ground;
            }
            0x20..=0x2f => {
                self.intermediates.clear();
                self.push_intermediate(byte);
                self.state = State::EscapeIntermediate;
            }
            _ => { self.state = State::Ground; }
        }
    }

    fn escape_intermediate(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x20..=0x2f => {
                if !self.push_intermediate(byte) {
                    self.state = State::EscapeIgnore;
                }
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
            0x1b => { self.state = State::Escape; }
            _ => { self.state = State::Ground; }
        }
    }

    fn escape_ignore(&mut self, byte: u8) {
        match byte {
            0x1b => { self.state = State::Escape; }
            0x18 | 0x1a | 0x30..=0x7e => { self.state = State::Ground; }
            _ => {}
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
                if self.push_param(0, false) {
                    self.state = State::CsiParam;
                } else {
                    self.state = State::CsiIgnore;
                }
            }
            b'?' | b'>' | b'!' => {
                if self.push_intermediate(byte) {
                    self.state = State::CsiParam;
                } else {
                    self.state = State::CsiIgnore;
                }
            }
            0x20..=0x2f => {
                if self.push_intermediate(byte) {
                    self.state = State::CsiIntermediate;
                } else {
                    self.state = State::CsiIgnore;
                }
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
                self.current_param = Some(
                    self.current_param
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(digit),
                );
            }
            b';' => {
                let accepted = self.push_param(
                    self.current_param.unwrap_or(0),
                    self.current_is_sub,
                );
                self.current_param = None;
                self.current_is_sub = false;
                if !accepted {
                    self.state = State::CsiIgnore;
                }
            }
            b':' => {
                let accepted = self.push_param(
                    self.current_param.unwrap_or(0),
                    self.current_is_sub,
                );
                self.current_param = None;
                self.current_is_sub = true; // next param is a sub-param
                if !accepted {
                    self.state = State::CsiIgnore;
                }
            }
            0x20..=0x2f => {
                if let Some(param) = self.current_param.take() {
                    if !self.push_param(param, self.current_is_sub) {
                        self.state = State::CsiIgnore;
                        return;
                    }
                }
                if self.push_intermediate(byte) {
                    self.state = State::CsiIntermediate;
                } else {
                    self.state = State::CsiIgnore;
                }
            }
            0x40..=0x7e => {
                let accepted = match self.current_param.take() {
                    Some(param) => self.push_param(param, self.current_is_sub),
                    None => true,
                };
                if accepted {
                    self.dispatch_csi(byte, grid);
                }
                self.state = State::Ground;
            }
            0x1b => { self.state = State::Escape; }
            _ => { self.state = State::Ground; }
        }
    }

    fn csi_intermediate(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x20..=0x2f => {
                if !self.push_intermediate(byte) {
                    self.state = State::CsiIgnore;
                }
            }
            0x40..=0x7e => {
                let accepted = match self.current_param.take() {
                    Some(param) => self.push_param(param, self.current_is_sub),
                    None => true,
                };
                if accepted {
                    self.dispatch_csi(byte, grid);
                }
                self.state = State::Ground;
            }
            _ => { self.state = State::Ground; }
        }
    }

    fn csi_ignore(&mut self, byte: u8) {
        match byte {
            0x1b => { self.state = State::Escape; }
            0x18 | 0x1a | 0x40..=0x7e => { self.state = State::Ground; }
            _ => {}
        }
    }

    fn push_param(&mut self, value: u16, is_sub: bool) -> bool {
        if self.params.len() >= MAX_CSI_PARAMS {
            return false;
        }
        self.params.push(value);
        self.param_is_sub.push(is_sub);
        true
    }

    fn push_intermediate(&mut self, byte: u8) -> bool {
        if self.intermediates.len() >= MAX_INTERMEDIATES {
            return false;
        }
        self.intermediates.push(byte);
        true
    }

    fn begin_control_string(&mut self, kind: ControlStringKind) {
        self.cancel_active_control_string();
        self.active_control_string = Some(kind);
        self.control_string_overflowed = false;
        *self.control_string_data_mut(kind) = Vec::new();
    }

    fn push_control_string_byte(&mut self, kind: ControlStringKind, byte: u8) {
        if self.active_control_string != Some(kind) || self.control_string_overflowed {
            return;
        }

        let limit = match kind {
            ControlStringKind::Osc => self.control_string_limits.osc,
            ControlStringKind::Apc => self.control_string_limits.apc,
            ControlStringKind::Dcs => self.control_string_limits.dcs,
        };
        let data = self.control_string_data_mut(kind);
        if data.len() >= limit {
            // Release retained memory immediately and consume the rest of the
            // string without ever dispatching a truncated prefix.
            *data = Vec::new();
            self.control_string_overflowed = true;
        } else {
            data.push(byte);
        }
    }

    fn finish_active_control_string(&mut self, grid: &mut Grid) {
        let Some(kind) = self.active_control_string.take() else {
            return;
        };
        let overflowed = std::mem::replace(&mut self.control_string_overflowed, false);
        if overflowed {
            *self.control_string_data_mut(kind) = Vec::new();
            return;
        }

        match kind {
            ControlStringKind::Osc => self.dispatch_osc(grid),
            ControlStringKind::Apc => self.dispatch_apc(grid),
            ControlStringKind::Dcs => self.dispatch_dcs(grid),
        }
    }

    fn cancel_active_control_string(&mut self) {
        if let Some(kind) = self.active_control_string.take() {
            *self.control_string_data_mut(kind) = Vec::new();
        }
        self.control_string_overflowed = false;
    }

    fn control_string_data_mut(&mut self, kind: ControlStringKind) -> &mut Vec<u8> {
        match kind {
            ControlStringKind::Osc => &mut self.osc_data,
            ControlStringKind::Apc => &mut self.apc_data,
            ControlStringKind::Dcs => &mut self.dcs_data,
        }
    }

    fn osc_string(&mut self, byte: u8, grid: &mut Grid) {
        match byte {
            0x07 => {
                self.finish_active_control_string(grid);
                self.state = State::Ground;
            }
            0x1b => { self.state = State::Escape; }
            0x18 | 0x1a => {
                self.cancel_active_control_string();
                self.state = State::Ground;
            }
            _ => { self.push_control_string_byte(ControlStringKind::Osc, byte); }
        }
    }

    fn apc_string(&mut self, byte: u8, _grid: &mut Grid) {
        match byte {
            0x1b => { self.state = State::Escape; } // Will see '\' next for ST
            0x18 | 0x1a => {
                self.cancel_active_control_string();
                self.state = State::Ground;
            }
            _ if self.graphics_support.kitty => {
                self.push_control_string_byte(ControlStringKind::Apc, byte);
            }
            _ => {}
        }
    }

    fn dcs_passthrough(&mut self, byte: u8, _grid: &mut Grid) {
        match byte {
            0x1b => { self.state = State::Escape; } // Will see '\' next for ST
            0x18 | 0x1a => {
                self.cancel_active_control_string();
                self.state = State::Ground;
            }
            _ if self.graphics_support.sixel => {
                self.push_control_string_byte(ControlStringKind::Dcs, byte);
            }
            _ => {}
        }
    }

    fn dispatch_apc(&mut self, grid: &mut Grid) {
        let data = std::mem::take(&mut self.apc_data);
        if data.is_empty() || !self.graphics_support.kitty {
            return;
        }

        // Check if this is a Kitty graphics command (starts with 'G')
        if data.first() == Some(&b'G') {
            if let Some(cmd) = kitty_graphics::parse_kitty_command(&data[1..]) {
                grid.queue_kitty_command(cmd);
            }
        } else {
            log::trace!("Unknown APC sequence, first byte: {:?}", data.first());
        }
    }

    fn dispatch_dcs(&mut self, grid: &mut Grid) {
        let data = std::mem::take(&mut self.dcs_data);
        if data.is_empty() || !self.graphics_support.sixel {
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
            if !grid.has_sixel_queue_slot() {
                log::warn!("Discarding Sixel image: pending image count limit reached");
                return;
            }
            let remaining_bytes = grid.remaining_sixel_bytes();
            if remaining_bytes == 0 {
                log::warn!("Discarding Sixel image: pending image budget exhausted");
                return;
            }
            match sixel::decode_sixel_with_byte_limit(sixel_data, remaining_bytes) {
                Ok(image) => {
                    if !grid.queue_sixel_image(image) {
                        log::warn!("Discarding Sixel image: pending image budget exceeded");
                    }
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
                "0" | "2" => grid.set_title(pt),
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
                        grid.queue_response(resp.into_bytes());
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
                        grid.queue_response(resp.into_bytes());
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
                grid.queue_response(b"\x1bP>|Kokuban 0.1.0\x1b\\".to_vec());
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
                grid.cancel_pending_wrap();
                grid.cursor_col = col.min(grid.cols() - 1);
            }
            b'd' => {
                let row = params.first().copied().unwrap_or(1).max(1) as usize - 1;
                grid.cancel_pending_wrap();
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
                grid.delete_chars(n);
            }
            b'@' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.insert_blank_chars(n);
            }
            b'X' => {
                let n = params.first().copied().unwrap_or(1).max(1) as usize;
                grid.erase_chars(n);
            }
            b'S' => { grid.scroll_up(params.first().copied().unwrap_or(1).max(1) as usize); }
            b'T' => { grid.scroll_down(params.first().copied().unwrap_or(1).max(1) as usize); }

            // Device status report
            b'n' => {
                let ps = params.first().copied().unwrap_or(0);
                if ps == 6 {
                    // Cursor position report
                    let cursor_col = grid.screen_cursor_col().unwrap_or(grid.cols() - 1);
                    let resp = format!("\x1b[{};{}R", grid.cursor_row + 1, cursor_col + 1);
                    grid.queue_response(resp.into_bytes());
                }
            }

            // Device attributes
            b'c' => {
                if has_gt {
                    // DA2
                    grid.queue_response(b"\x1b[>1;0;0c".to_vec());
                } else if params.is_empty() || params.first().copied().unwrap_or(0) == 0 {
                    // DA1 (62=VT220, 4=Sixel, 22=color)
                    let response = if self.graphics_support.sixel {
                        b"\x1b[?62;4;22c".to_vec()
                    } else {
                        b"\x1b[?62;22c".to_vec()
                    };
                    grid.queue_response(response);
                }
            }

            // Window operations
            b't' => {
                let ps = params.first().copied().unwrap_or(0);
                match ps {
                    14 => {
                        // Report terminal size in pixels
                        let h = terminal_pixel_extent(grid.rows(), grid.cell_pixel_height);
                        let w = terminal_pixel_extent(grid.cols(), grid.cell_pixel_width);
                        let resp = format!("\x1b[4;{h};{w}t");
                        grid.queue_response(resp.into_bytes());
                    }
                    16 => {
                        // Report cell size in pixels
                        let ch = grid.cell_pixel_height;
                        let cw = grid.cell_pixel_width;
                        let resp = format!("\x1b[6;{ch};{cw}t");
                        grid.queue_response(resp.into_bytes());
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
                            1 => { grid.application_cursor_keys = set; }
                            7 => { grid.set_auto_wrap(set); }
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
                            1007 => { grid.alternate_scroll = set; }
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
    #[cfg(test)]
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
        }
    }

    pub(crate) fn with_graphics_support(graphics_support: GraphicsSupport) -> Self {
        Self {
            parser: Parser::with_graphics_support(graphics_support),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
        }
    }

    #[cfg(test)]
    fn with_control_string_limits(limits: ControlStringLimits) -> Self {
        Self {
            parser: Parser::with_control_string_limits(limits),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
        }
    }

    #[cfg(test)]
    pub fn feed(&mut self, input: &[u8], grid: &mut Grid) {
        for &byte in input {
            self.feed_byte(byte, grid);
        }
    }

    /// Consume input through the first newly queued terminal event.
    /// The caller must drain any existing events before calling this method.
    pub(crate) fn feed_until_terminal_event(&mut self, input: &[u8], grid: &mut Grid) -> usize {
        debug_assert!(!grid.has_pending_terminal_events());

        for (index, &byte) in input.iter().enumerate() {
            self.feed_byte(byte, grid);
            if grid.has_pending_terminal_events() {
                return index + 1;
            }
        }

        input.len()
    }

    fn feed_byte(&mut self, byte: u8, grid: &mut Grid) {
        // OSC, APC and DCS payloads are byte-oriented. Decoding their UTF-8
        // here would print the decoded character into the terminal grid
        // instead of keeping it inside the control string.
        if self.parser.state != State::Ground {
            self.utf8_len = 0;
            self.utf8_expected = 0;
            self.parser.advance(byte, grid);
            return;
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

#[cfg(test)]
mod tests {
    use super::{
        terminal_pixel_extent, ControlStringLimits, GraphicsSupport, Utf8Parser, MAX_CSI_PARAMS,
        MAX_INTERMEDIATES,
    };
    use crate::grid::cell::{CellFlags, Color, UnderlineStyle};
    use crate::grid::{Grid, MouseEncoding, MouseTracking, TerminalEvent};
    use crate::parser::kitty_graphics::{KittyAction, KittyCommand};
    use crate::parser::sixel::MAX_RGBA_BYTES;
    use crate::parser::State;
    use crate::input::keyboard::{encode_terminal_key, TerminalKey};

    fn grid() -> Grid {
        Grid::new(40, 4, 100)
    }

    fn limited_parser(osc: usize, apc: usize, dcs: usize) -> Utf8Parser {
        Utf8Parser::with_control_string_limits(ControlStringLimits { osc, apc, dcs })
    }

    fn drain_kitty_commands(grid: &mut Grid) -> Vec<KittyCommand> {
        grid.drain_terminal_events()
            .into_iter()
            .filter_map(|event| match event {
                TerminalEvent::KittyGraphics { command, .. } => Some(command),
                TerminalEvent::Response(_) => None,
            })
            .collect()
    }

    #[test]
    fn applies_remaining_queue_budget_before_decoding_sixel() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();
        let raster = b"\x1bPq\"1;1;4;4\x1b\\";

        grid.set_pending_sixel_bytes_for_test(MAX_RGBA_BYTES - 63);
        parser.feed(raster, &mut grid);
        assert!(grid.drain_sixel_images().is_empty());

        grid.set_pending_sixel_bytes_for_test(MAX_RGBA_BYTES - 64);
        parser.feed(raster, &mut grid);
        let images = grid.drain_sixel_images();
        assert_eq!(images.len(), 1);
        assert_eq!((images[0].width, images[0].height), (4, 4));
        assert_eq!(images[0].pixels.len(), 64);
    }

    fn feed_st_string(
        parser: &mut Utf8Parser,
        grid: &mut Grid,
        introducer: &[u8],
        data: &[u8],
    ) {
        parser.feed(introducer, grid);
        parser.feed(data, grid);
        parser.feed(b"\x1b", grid);
        parser.feed(b"\\", grid);
    }

    #[test]
    fn graphics_support_gates_parsing_buffering_and_da1() {
        for (kitty, sixel) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut parser = Utf8Parser::with_graphics_support(GraphicsSupport { kitty, sixel });
            let mut grid = grid();

            parser.feed(b"\x1b_Ga=d,d=a", &mut grid);
            assert_eq!(parser.parser.apc_data.is_empty(), !kitty);
            if !kitty {
                assert_eq!(parser.parser.apc_data.capacity(), 0);
            }
            parser.feed(b"\x1b\\", &mut grid);
            assert_eq!(drain_kitty_commands(&mut grid).len(), usize::from(kitty));

            parser.feed(b"\x1bPq~", &mut grid);
            assert_eq!(parser.parser.dcs_data.is_empty(), !sixel);
            if !sixel {
                assert_eq!(parser.parser.dcs_data.capacity(), 0);
            }
            parser.feed(b"\x1b\\", &mut grid);
            assert_eq!(grid.drain_sixel_images().len(), usize::from(sixel));

            parser.feed(b"\x1b[c", &mut grid);
            let events = grid.drain_terminal_events();
            let expected_da1 = if sixel {
                b"\x1b[?62;4;22c".as_slice()
            } else {
                b"\x1b[?62;22c".as_slice()
            };
            assert!(matches!(
                events.as_slice(),
                [TerminalEvent::Response(response)] if response == expected_da1
            ));
        }
    }

    #[test]
    fn terminal_pixel_extent_saturates_at_the_u64_limit() {
        let exact_extent = u128::try_from(usize::MAX)
            .unwrap_or(u128::MAX)
            .saturating_mul(u128::from(u16::MAX));
        let expected = u64::try_from(exact_extent).unwrap_or(u64::MAX);

        assert_eq!(terminal_pixel_extent(usize::MAX, u16::MAX), expected);
    }

    #[test]
    fn terminal_pixel_report_preserves_extents_larger_than_u16() {
        let mut parser = Utf8Parser::new();
        let mut grid = Grid::new(1024, 256, 0);
        grid.cell_pixel_width = u16::MAX;
        grid.cell_pixel_height = u16::MAX;

        parser.feed(b"\x1b[14t", &mut grid);

        assert_eq!(
            terminal_pixel_extent(grid.cols(), grid.cell_pixel_width),
            67_107_840
        );
        assert_eq!(
            terminal_pixel_extent(grid.rows(), grid.cell_pixel_height),
            16_776_960
        );
        let events = grid.drain_terminal_events();
        assert!(matches!(
            events.as_slice(),
            [TerminalEvent::Response(response)]
                if response == b"\x1b[4;16776960;67107840t"
        ));
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

        assert_eq!(grid.title(), "Kokuban 日本");
        assert_eq!(grid.buffer.cell(0, 0).c, ' ');
        assert_eq!(grid.cursor_col, 0);
    }

    #[test]
    fn keeps_utf8_inside_st_terminated_osc_split_across_reads() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        parser.feed("\x1b]0;黒".as_bytes(), &mut grid);
        parser.feed("板\x1b\\".as_bytes(), &mut grid);

        assert_eq!(grid.title(), "黒板");
        assert_eq!(grid.buffer.cell(0, 0).c, ' ');
        assert_eq!(grid.cursor_col, 0);
    }

    #[test]
    fn advances_window_title_revision_only_when_the_title_changes() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        assert_eq!(grid.title_revision(), 0);

        parser.feed(b"\x1b]2;first\x07", &mut grid);
        assert_eq!(grid.title(), "first");
        assert_eq!(grid.title_revision(), 1);

        parser.feed(b"\x1b]0;first\x1b\\", &mut grid);
        assert_eq!(grid.title_revision(), 1);

        parser.feed("\x1b]2;日本\x07".as_bytes(), &mut grid);
        assert_eq!(grid.title(), "日本");
        assert_eq!(grid.title_revision(), 2);

        parser.feed(b"\x1bc\x1b]2;after reset\x07", &mut grid);
        assert_eq!(grid.title(), "after reset");
        assert_eq!(grid.title_revision(), 4);

        parser.feed(b"\x1bc", &mut grid);
        assert!(grid.title().is_empty());
        assert_eq!(grid.title_revision(), 5);

        parser.feed(b"\x1bc", &mut grid);
        assert_eq!(grid.title_revision(), 5);
    }

    #[test]
    fn toggles_application_cursor_keys_with_decckm() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        assert!(!grid.application_cursor_keys);

        parser.feed(b"\x1b[?25l", &mut grid);
        assert!(!grid.cursor_visible);

        parser.feed(b"\x1b[?1;", &mut grid);
        assert!(!grid.application_cursor_keys);
        parser.feed(b"25h", &mut grid);
        assert!(grid.application_cursor_keys);
        assert!(grid.cursor_visible);

        parser.feed(b"\x1b[?1l", &mut grid);
        assert!(!grid.application_cursor_keys);

        parser.feed(b"\x1b[?1h\x1bc", &mut grid);
        assert!(!grid.application_cursor_keys);
    }

    #[test]
    fn toggles_auto_wrap_with_decawm_and_resets_it_with_ris() {
        let mut parser = Utf8Parser::new();
        let mut grid = Grid::new(3, 2, 10);

        assert!(grid.auto_wrap);
        parser.feed(b"\x1b[?7l", &mut grid);
        assert!(!grid.auto_wrap);

        parser.feed(b"abcde", &mut grid);
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, grid.cols()));
        assert_eq!(grid.buffer.cell(0, 0).c, 'a');
        assert_eq!(grid.buffer.cell(0, 1).c, 'b');
        assert_eq!(grid.buffer.cell(0, 2).c, 'e');
        assert_eq!(grid.buffer.cell(1, 0).c, ' ');
        assert_eq!(grid.scrollback_len(), 0);

        parser.feed(b"\x1b[?7h", &mut grid);
        assert!(grid.auto_wrap);
        parser.feed(b"f", &mut grid);
        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
        assert_eq!(grid.buffer.cell(1, 0).c, 'f');

        parser.feed(b"\x1b[?7l\x1bc", &mut grid);
        assert!(grid.auto_wrap);
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 0));
        assert_eq!(grid.buffer.cell(0, 0).c, ' ');

        parser.feed(b"abcd", &mut grid);
        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
        assert_eq!(grid.buffer.cell(0, 2).c, 'c');
        assert_eq!(grid.buffer.cell(1, 0).c, 'd');
    }

    #[test]
    fn decawm_toggle_without_printable_preserves_pending_wrap() {
        let mut parser = Utf8Parser::new();
        let mut grid = Grid::new(2, 2, 0);
        parser.feed(b"ab", &mut grid);
        assert_eq!(grid.cursor_col, grid.cols());

        parser.feed(b"\x1b[?7l\x1b[?7h", &mut grid);
        parser.feed(b"c", &mut grid);

        assert!(grid.auto_wrap);
        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
        assert_eq!(grid.buffer.cell(0, 1).c, 'b');
        assert_eq!(grid.buffer.cell(1, 0).c, 'c');
    }

    #[test]
    fn decawm_participates_in_multi_parameter_private_modes() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        parser.feed(b"\x1b[?1;7;25l", &mut grid);
        assert!(!grid.application_cursor_keys);
        assert!(!grid.auto_wrap);
        assert!(!grid.cursor_visible);

        parser.feed(b"\x1b[?1;7;25h", &mut grid);
        assert!(grid.application_cursor_keys);
        assert!(grid.auto_wrap);
        assert!(grid.cursor_visible);
    }

    #[test]
    fn cursor_position_report_projects_pending_wrap_to_the_right_margin() {
        let mut parser = Utf8Parser::new();
        let mut grid = Grid::new(2, 2, 0);

        parser.feed(b"ab\x1b[6n", &mut grid);
        assert!(matches!(
            grid.drain_terminal_events().as_slice(),
            [TerminalEvent::Response(response)] if response == b"\x1b[1;2R"
        ));

        parser.feed(b"\x1b[?7lc\x1b[6n", &mut grid);
        assert!(matches!(
            grid.drain_terminal_events().as_slice(),
            [TerminalEvent::Response(response)] if response == b"\x1b[1;2R"
        ));
    }

    #[test]
    fn kitty_events_project_pending_wrap_to_the_right_margin() {
        let mut parser = Utf8Parser::new();
        let mut grid = Grid::new(3, 2, 0);

        parser.feed(b"abc\x1b_Ga=d,d=c\x1b\\", &mut grid);

        assert!(matches!(
            grid.drain_terminal_events().as_slice(),
            [TerminalEvent::KittyGraphics {
                cursor_row: 0,
                cursor_col: 2,
                ..
            }]
        ));
        assert_eq!(grid.cursor_col, grid.cols());
    }

    #[test]
    fn screen_editing_controls_cancel_pending_wrap_before_the_next_printable() {
        let controls: [(&str, &[u8]); 16] = [
            ("LF", b"\n"),
            ("VT", b"\x0b"),
            ("FF", b"\x0c"),
            ("IND", b"\x1bD"),
            ("RI", b"\x1bM"),
            ("CUU", b"\x1b[A"),
            ("CUD", b"\x1b[B"),
            ("HPA", b"\x1b[3G"),
            ("VPA", b"\x1b[2d"),
            ("EL", b"\x1b[K"),
            ("ED", b"\x1b[J"),
            ("DCH", b"\x1b[P"),
            ("ICH", b"\x1b[@"),
            ("ECH", b"\x1b[X"),
            ("IL", b"\x1b[L"),
            ("DL", b"\x1b[M"),
        ];

        for (name, control) in controls {
            let mut parser = Utf8Parser::new();
            let mut grid = Grid::new(3, 3, 0);
            grid.set_cursor_pos(1, 0);
            parser.feed(b"abc", &mut grid);
            assert_eq!(grid.cursor_col, grid.cols(), "setup failed for {name}");

            parser.feed(control, &mut grid);
            let row_after_control = grid.cursor_row;
            assert_eq!(grid.cursor_col, 2, "{name} did not cancel pending wrap");

            parser.feed(b"X", &mut grid);

            assert_eq!(grid.cursor_row, row_after_control, "{name} wrapped twice");
            assert_eq!(grid.buffer.cell(row_after_control, 2).c, 'X', "{name}");
        }
    }

    #[test]
    fn screen_scroll_controls_preserve_pending_wrap() {
        for (name, control) in [("SU", b"\x1b[S".as_slice()), ("SD", b"\x1b[T".as_slice())] {
            let mut parser = Utf8Parser::new();
            let mut grid = Grid::new(3, 4, 0);
            grid.set_cursor_pos(1, 0);
            parser.feed(b"abc", &mut grid);
            assert_eq!(grid.cursor_col, grid.cols(), "setup failed for {name}");

            parser.feed(control, &mut grid);
            assert_eq!(grid.cursor_col, grid.cols(), "{name} consumed pending wrap");

            parser.feed(b"X", &mut grid);
            assert_eq!((grid.cursor_row, grid.cursor_col), (2, 1), "{name}");
            assert_eq!(grid.buffer.cell(2, 0).c, 'X', "{name}");
        }
    }

    #[test]
    fn horizontal_tab_preserves_pending_wrap_at_the_right_margin() {
        let mut parser = Utf8Parser::new();
        let mut grid = Grid::new(3, 2, 0);
        parser.feed(b"abc\t", &mut grid);

        assert_eq!(grid.cursor_col, grid.cols());
        parser.feed(b"X", &mut grid);

        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
        assert_eq!(grid.buffer.cell(0, 2).c, 'c');
        assert_eq!(grid.buffer.cell(1, 0).c, 'X');
    }

    #[test]
    fn horizontal_tab_moves_the_physical_cursor_after_grow_without_clearing_wrap() {
        let mut parser = Utf8Parser::new();
        let mut grid = Grid::new(3, 2, 0);
        parser.feed(b"abc", &mut grid);

        grid.resize(10, 2);
        parser.feed(b"\t\x1b[6n", &mut grid);

        assert_eq!(grid.cursor_col, 8);
        assert!(grid.is_wrap_pending());
        assert!(matches!(
            grid.drain_terminal_events().as_slice(),
            [TerminalEvent::Response(response)] if response == b"\x1b[1;9R"
        ));

        parser.feed(b"X", &mut grid);
        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
        assert_eq!(grid.buffer.cell(1, 0).c, 'X');
    }

    #[test]
    fn non_moving_sequences_preserve_pending_wrap() {
        let mut parser = Utf8Parser::new();
        let mut grid = Grid::new(3, 2, 0);
        parser.feed(b"abc", &mut grid);

        parser.feed(b"\x1b[31m\x1b[?7l\x1b[?7h\x1b[6n", &mut grid);
        assert_eq!(grid.cursor_col, grid.cols());
        parser.feed(b"X", &mut grid);

        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
        assert_eq!(grid.buffer.cell(1, 0).c, 'X');
    }

    #[test]
    fn toggles_focus_reporting_with_dec_private_mode_and_ris() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        assert!(!grid.focus_events);
        parser.feed(b"\x1b[?1004h", &mut grid);
        assert!(grid.focus_events);

        parser.feed(b"\x1b[?1004l", &mut grid);
        assert!(!grid.focus_events);

        parser.feed(b"\x1b[?1004h\x1bc", &mut grid);
        assert!(!grid.focus_events);
    }

    #[test]
    fn toggles_mouse_tracking_and_encoding_with_dec_private_modes_and_ris() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        assert_eq!(grid.mouse_tracking, MouseTracking::None);
        assert_eq!(grid.mouse_encoding, MouseEncoding::Default);

        for (mode, tracking) in [
            (1000, MouseTracking::Normal),
            (1002, MouseTracking::ButtonEvent),
            (1003, MouseTracking::AnyEvent),
        ] {
            parser.feed(format!("\x1b[?{mode}h").as_bytes(), &mut grid);
            assert_eq!(grid.mouse_tracking, tracking);
            parser.feed(format!("\x1b[?{mode}l").as_bytes(), &mut grid);
            assert_eq!(grid.mouse_tracking, MouseTracking::None);
        }

        parser.feed(b"\x1b[?1000;1006h", &mut grid);
        assert_eq!(grid.mouse_tracking, MouseTracking::Normal);
        assert_eq!(grid.mouse_encoding, MouseEncoding::Sgr);

        parser.feed(b"\x1b[?1006l", &mut grid);
        assert_eq!(grid.mouse_tracking, MouseTracking::Normal);
        assert_eq!(grid.mouse_encoding, MouseEncoding::Default);

        parser.feed(b"\x1b[?1003;1006h\x1bc", &mut grid);
        assert_eq!(grid.mouse_tracking, MouseTracking::None);
        assert_eq!(grid.mouse_encoding, MouseEncoding::Default);
    }

    #[test]
    fn toggles_alternate_scroll_independently_and_preserves_it_across_ris() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        assert!(!grid.alternate_scroll);
        assert!(!grid.using_alt_screen);

        parser.feed(b"\x1b[?1007;1049h", &mut grid);
        assert!(grid.alternate_scroll);
        assert!(grid.using_alt_screen);

        parser.feed(b"\x1b[?1049l", &mut grid);
        assert!(grid.alternate_scroll);
        assert!(!grid.using_alt_screen);

        parser.feed(b"\x1b[?1007l", &mut grid);
        assert!(!grid.alternate_scroll);

        parser.feed(b"\x1b[?1007;1047h\x1bc", &mut grid);
        assert!(grid.alternate_scroll);
        assert!(!grid.using_alt_screen);

        parser.feed(b"\x1b[?1007l\x1bc", &mut grid);
        assert!(!grid.alternate_scroll);
    }

    #[test]
    fn decckm_mode_drives_cursor_key_encoding() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        assert_eq!(
            encode_terminal_key(TerminalKey::Up, grid.application_cursor_keys),
            Some(b"\x1b[A".to_vec())
        );
        parser.feed(b"\x1b[?1h", &mut grid);
        assert_eq!(
            encode_terminal_key(TerminalKey::Up, grid.application_cursor_keys),
            Some(b"\x1bOA".to_vec())
        );
        parser.feed(b"\x1b[?1l", &mut grid);
        assert_eq!(
            encode_terminal_key(TerminalKey::Up, grid.application_cursor_keys),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn saturates_oversized_numeric_parameters_across_reads() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        parser.feed(b"\x1b[655", &mut grid);
        parser.feed(b"35A", &mut grid);
        assert_eq!(parser.parser.params, vec![u16::MAX]);

        parser.feed(b"\x1b[?6553", &mut grid);
        parser.feed(b"799999h", &mut grid);
        assert_eq!(parser.parser.params, vec![u16::MAX]);
        assert!(!grid.application_cursor_keys);
    }

    #[test]
    fn accepts_parameter_limit_and_ignores_excess_sequence() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        let mut at_limit = String::from("\x1b[?");
        for index in 0..MAX_CSI_PARAMS {
            if index > 0 {
                at_limit.push(';');
            }
            at_limit.push(if index + 1 == MAX_CSI_PARAMS { '1' } else { '0' });
        }
        at_limit.push('h');
        parser.feed(at_limit.as_bytes(), &mut grid);
        assert!(grid.application_cursor_keys);

        parser.feed(b"\x1b[?1l", &mut grid);
        let mut over_limit = String::from("\x1b[?1");
        for _ in 0..MAX_CSI_PARAMS {
            over_limit.push_str(";0");
        }
        over_limit.push('h');
        parser.feed(over_limit.as_bytes(), &mut grid);

        assert!(!grid.application_cursor_keys);
        assert_eq!(parser.parser.params.len(), MAX_CSI_PARAMS);
        assert_eq!(parser.parser.param_is_sub.len(), MAX_CSI_PARAMS);
        assert_eq!(parser.parser.state, State::Ground);

        parser.feed(b"\x1b[?1h", &mut grid);
        assert!(grid.application_cursor_keys);
    }

    #[test]
    fn subparameters_count_toward_limit_without_dispatching_prefix() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();
        let mut sequence = String::from("\x1b[4");
        for _ in 0..MAX_CSI_PARAMS {
            sequence.push_str(":1");
        }
        sequence.push('m');

        parser.feed(sequence.as_bytes(), &mut grid);

        assert_eq!(grid.underline_style, UnderlineStyle::None);
        assert!(!grid.flags.contains(CellFlags::UNDERLINE));
        assert_eq!(parser.parser.params.len(), MAX_CSI_PARAMS);
        assert_eq!(parser.parser.params.len(), parser.parser.param_is_sub.len());

        parser.feed(b"\x1b[4:3m", &mut grid);
        assert_eq!(grid.underline_style, UnderlineStyle::Curly);
    }

    #[test]
    fn ignores_excess_intermediates_and_recovers() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();

        parser.feed(b"\x1b[31   m", &mut grid);
        assert_eq!(grid.fg, Color::Default);
        assert_eq!(parser.parser.intermediates.len(), MAX_INTERMEDIATES);
        assert_eq!(parser.parser.state, State::Ground);

        parser.feed(b"\x1b[31m", &mut grid);
        assert_eq!(grid.fg, Color::Indexed(1));

        let excess = vec![b'('; 1_000];
        parser.feed(b"\x1b((", &mut grid);
        parser.feed(&excess, &mut grid);
        assert_eq!(parser.parser.intermediates.len(), MAX_INTERMEDIATES);
        assert_eq!(parser.parser.state, State::EscapeIgnore);

        parser.feed(b"Bz", &mut grid);
        assert_eq!(parser.parser.state, State::Ground);
        assert_eq!(grid.buffer.cell(0, 0).c, 'z');
    }

    #[test]
    fn bounds_osc_and_discards_overflow_for_bell_and_st() {
        let mut parser = limited_parser(8, 64, 64);
        let mut grid = grid();
        let exact = b"2;123456";
        assert_eq!(exact.len(), 8);

        parser.feed(b"\x1b]", &mut grid);
        parser.feed(exact, &mut grid);
        parser.feed(b"\x07", &mut grid);
        assert_eq!(grid.title(), "123456");
        assert_eq!(grid.title_revision(), 1);

        parser.feed(b"\x1b]2;1234567\x07", &mut grid);
        assert_eq!(grid.title(), "123456");
        assert_eq!(grid.title_revision(), 1);
        assert_eq!(parser.parser.osc_data.capacity(), 0);
        assert!(parser.parser.active_control_string.is_none());
        assert!(!parser.parser.control_string_overflowed);

        parser.feed(b"\x1b]2;overflow", &mut grid);
        parser.feed(b"\x1b", &mut grid);
        assert!(parser.parser.control_string_overflowed);
        parser.feed(b"\\z", &mut grid);
        assert_eq!(grid.title(), "123456");
        assert_eq!(grid.title_revision(), 1);
        assert_eq!(grid.buffer.cell(0, 0).c, 'z');

        feed_st_string(&mut parser, &mut grid, b"\x1b]", b"2;next");
        assert_eq!(grid.title(), "next");
        assert_eq!(grid.title_revision(), 2);
    }

    #[test]
    fn bounds_kitty_apc_without_dispatching_truncated_command() {
        let kitty = b"Gf=32,s=1,v=1,i=9;AAAAAA==";
        let mut parser = limited_parser(64, kitty.len(), 64);
        let mut grid = grid();

        feed_st_string(&mut parser, &mut grid, b"\x1b_", kitty);
        let commands = drain_kitty_commands(&mut grid);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].image_id, Some(9));
        assert_eq!(commands[0].payload, vec![0; 4]);

        parser.feed(b"\x1b_", &mut grid);
        parser.feed(kitty, &mut grid);
        parser.feed(b"x\x1b\\", &mut grid);
        assert!(drain_kitty_commands(&mut grid).is_empty());
        assert_eq!(parser.parser.apc_data.capacity(), 0);
    }

    #[test]
    fn preserves_terminal_event_order_and_kitty_cursor_snapshots() {
        let mut parser = Utf8Parser::new();
        let mut grid = grid();
        let stream = b"\x1b[2;3H\x1b[6n\x1b_Ga=d,d=c\x1b\\\x1b[3;4H\x1b_Ga=q,f=32,s=1,v=1,i=7;AAAAAA==\x1b\\\x1b[>c\x1b[4;5H";

        parser.feed(stream, &mut grid);

        assert_eq!((grid.cursor_row, grid.cursor_col), (3, 4));
        let events = grid.drain_terminal_events();
        assert_eq!(events.len(), 4);
        assert!(matches!(
            &events[0],
            TerminalEvent::Response(response) if response == b"\x1b[2;3R"
        ));
        match &events[1] {
            TerminalEvent::KittyGraphics {
                command,
                cursor_row,
                cursor_col,
            } => {
                assert_eq!(command.action, KittyAction::Delete);
                assert_eq!((*cursor_row, *cursor_col), (1, 2));
            }
            TerminalEvent::Response(_) => panic!("expected a Kitty delete event"),
        }
        match &events[2] {
            TerminalEvent::KittyGraphics {
                command,
                cursor_row,
                cursor_col,
            } => {
                assert_eq!(command.action, KittyAction::Query);
                assert_eq!((*cursor_row, *cursor_col), (2, 3));
            }
            TerminalEvent::Response(_) => panic!("expected a Kitty query event"),
        }
        assert!(matches!(
            &events[3],
            TerminalEvent::Response(response) if response == b"\x1b[>1;0;0c"
        ));
    }

    #[test]
    fn kitty_event_boundary_precedes_trailing_text() {
        const IMAGE: &[u8] = b"\x1b_Ga=p,i=7,c=2,r=1\x1b\\";
        const CURSOR_QUERY: &[u8] = b"\x1b[6n";
        let mut parser = Utf8Parser::new();
        let mut grid = grid();
        let stream = [IMAGE, CURSOR_QUERY, b"X"].concat();

        let consumed = parser.feed_until_terminal_event(&stream, &mut grid);

        assert_eq!(consumed, IMAGE.len());
        assert_eq!((grid.cursor_row, grid.cursor_col), (0, 0));
        assert_eq!(grid.buffer.cell(0, 0).c, ' ');
        let events = grid.drain_terminal_events();
        assert!(matches!(
            events.as_slice(),
            [TerminalEvent::KittyGraphics {
                command,
                cursor_row: 0,
                cursor_col: 0,
            }] if command.columns == Some(2) && command.rows == Some(1)
        ));

        grid.advance_image_cursor(2, 1);
        let response_consumed = parser.feed_until_terminal_event(&stream[consumed..], &mut grid);
        assert_eq!(response_consumed, CURSOR_QUERY.len());
        assert!(matches!(
            grid.drain_terminal_events().as_slice(),
            [TerminalEvent::Response(response)] if response == b"\x1b[2;3R"
        ));

        let text_start = consumed + response_consumed;
        let text_consumed =
            parser.feed_until_terminal_event(&stream[text_start..], &mut grid);

        assert_eq!(text_consumed, 1);
        assert_eq!(grid.buffer.cell(1, 2).c, 'X');
        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 3));
    }

    #[test]
    fn consecutive_inline_placements_observe_previous_advance() {
        const FIRST: &[u8] = b"\x1b_Ga=p,i=7,c=2,r=1\x1b\\";
        const SECOND: &[u8] = b"\x1b_Ga=p,i=8,c=3,r=1\x1b\\";
        let mut parser = Utf8Parser::new();
        let mut grid = grid();
        let stream = [FIRST, SECOND].concat();

        let first_consumed = parser.feed_until_terminal_event(&stream, &mut grid);
        assert_eq!(first_consumed, FIRST.len());
        let first_events = grid.drain_terminal_events();
        assert!(matches!(
            first_events.as_slice(),
            [TerminalEvent::KittyGraphics {
                command,
                cursor_row: 0,
                cursor_col: 0,
            }] if command.image_id == Some(7)
        ));
        grid.advance_image_cursor(2, 1);

        let second_consumed = parser.feed_until_terminal_event(
            &stream[first_consumed..],
            &mut grid,
        );
        assert_eq!(second_consumed, SECOND.len());
        let second_events = grid.drain_terminal_events();
        assert!(matches!(
            second_events.as_slice(),
            [TerminalEvent::KittyGraphics {
                command,
                cursor_row: 1,
                cursor_col: 2,
            }] if command.image_id == Some(8)
        ));
    }

    #[test]
    fn bounds_sixel_dcs_without_decoding_truncated_payload() {
        let sixel = b"q~";
        let mut parser = limited_parser(64, 64, sixel.len());
        let mut grid = grid();

        feed_st_string(&mut parser, &mut grid, b"\x1bP", sixel);
        let images = grid.drain_sixel_images();
        assert_eq!(images.len(), 1);
        assert_eq!((images[0].width, images[0].height), (1, 6));

        parser.feed(b"\x1bP", &mut grid);
        parser.feed(sixel, &mut grid);
        parser.feed(b"~\x1b\\", &mut grid);
        assert!(grid.drain_sixel_images().is_empty());
        assert_eq!(parser.parser.dcs_data.capacity(), 0);
    }

    #[test]
    fn aborted_osc_cannot_steal_later_apc_terminator() {
        let kitty = b"Gf=32,s=1,v=1,i=7;AAAAAA==";
        let mut parser = limited_parser(64, 64, 64);
        let mut grid = grid();

        parser.feed(b"\x1b]2;stale\x1b_", &mut grid);
        parser.feed(kitty, &mut grid);
        parser.feed(b"\x1b\\", &mut grid);

        assert!(grid.title().is_empty());
        assert_eq!(grid.title_revision(), 0);
        let commands = drain_kitty_commands(&mut grid);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].image_id, Some(7));
        assert!(parser.parser.active_control_string.is_none());
    }
}
