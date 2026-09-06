use bitflags::bitflags;
use std::sync::Arc;
use unicode_normalization::UnicodeNormalization;

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

#[derive(Debug, Clone)]
pub struct Cell {
    pub c: char,
    // Only cells with combining scalars allocate. Cloned render snapshots share
    // the immutable tail until another scalar is appended to the live cell.
    pub(crate) tail: Option<Arc<String>>,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
    pub underline_style: UnderlineStyle,
    pub underline_color: Color,
}

impl Cell {
    /// The exact scalar sequence received from the application.
    pub(crate) fn chars(&self) -> impl Iterator<Item = char> + '_ {
        std::iter::once(self.c).chain(self.tail.as_deref().map_or("", String::as_str).chars())
    }

    /// Compose glyphs for the scalar-based renderers without changing copied text.
    /// Remaining combining scalars are drawn at the same cell origin.
    pub(crate) fn normalized_chars(&self) -> impl Iterator<Item = char> + '_ {
        self.chars().nfc()
    }

    pub(crate) fn text_len(&self) -> usize {
        self.c.len_utf8() + self.tail.as_deref().map_or(0, String::len)
    }

    pub(crate) fn set_char(&mut self, c: char) {
        self.c = c;
        self.tail = None;
    }

    pub(crate) fn push_combining(&mut self, c: char) {
        Arc::make_mut(self.tail.get_or_insert_with(|| Arc::new(String::new()))).push(c);
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            c: ' ',
            tail: None,
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
