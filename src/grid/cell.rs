use bitflags::bitflags;
use std::{borrow::Cow, sync::Arc};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CellFlags: u8 {
        const BOLD      = 0b0000_0001;
        const ITALIC    = 0b0000_0010;
        const UNDERLINE = 0b0000_0100;
        const REVERSE   = 0b0000_1000;
        const WIDE      = 0b0001_0000;
        const WIDE_CONT = 0b0010_0000;
        const FAINT     = 0b0100_0000;
        const HIDDEN    = 0b1000_0000;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnderlineStyle {
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub c: char,
    pub grapheme: Option<Arc<str>>,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
    pub underline_style: UnderlineStyle,
    pub underline_color: Color,
}

impl Cell {
    pub fn text(&self) -> Cow<'_, str> {
        match &self.grapheme {
            Some(text) => Cow::Borrowed(text),
            None => Cow::Owned(self.c.to_string()),
        }
    }

    /// Natural terminal width, even when a one-column grid squeezes the glyph.
    pub fn display_width(&self) -> usize {
        match self.grapheme.as_deref() {
            Some(text) => text.width(),
            None => self.c.width().unwrap_or(1),
        }.clamp(1, 2)
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            c: ' ',
            grapheme: None,
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            underline_style: UnderlineStyle::None,
            underline_color: Color::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CellFlags;

    #[test]
    fn hidden_uses_the_last_unassigned_cell_flag_bit() {
        assert_eq!(CellFlags::HIDDEN.bits(), 0b1000_0000);
        assert_eq!(CellFlags::all().bits(), u8::MAX);
    }
}
