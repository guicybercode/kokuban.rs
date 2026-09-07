use bitflags::bitflags;
use std::sync::Arc;
use unicode_normalization::UnicodeNormalization;

// Bound both retained memory and normalization work for zero-width input,
// which does not advance the cursor or consume the scrollback budget.
pub(crate) const MAX_COMBINING_BYTES: usize = 64;

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
    /// The retained scalar sequence, without normalization.
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
        if self.tail.as_deref().map_or(0, String::len) + c.len_utf8() > MAX_COMBINING_BYTES {
            return;
        }
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
    use super::{Cell, CellFlags, MAX_COMBINING_BYTES};
    use std::sync::Arc;

    #[test]
    fn excessive_combining_input_does_not_copy_a_full_snapshot() {
        let mut cell = Cell::default();
        for _ in 0..MAX_COMBINING_BYTES / '\u{301}'.len_utf8() {
            cell.push_combining('\u{301}');
        }
        let snapshot = cell.clone();
        for _ in 0..10_000 {
            cell.push_combining('\u{301}');
        }
        assert_eq!(cell.text_len(), 1 + MAX_COMBINING_BYTES);
        assert!(Arc::ptr_eq(
            cell.tail.as_ref().unwrap(),
            snapshot.tail.as_ref().unwrap()
        ));
    }

    #[test]
    fn combining_limit_keeps_complete_scalars_and_resets_on_replacement() {
        let mut cell = Cell::default();
        for _ in 0..100 {
            cell.push_combining('\u{20dd}');
        }
        assert_eq!(cell.text_len(), 64); // Base plus 21 complete three-byte marks.
        assert_eq!(cell.chars().count(), 22);
        cell.set_char('e');
        cell.push_combining('\u{301}');
        assert_eq!(cell.chars().collect::<String>(), "e\u{301}");
        assert_eq!(cell.normalized_chars().collect::<String>(), "é");
    }

    #[test]
    fn hidden_uses_the_last_unassigned_cell_flag_bit() {
        assert_eq!(CellFlags::HIDDEN.bits(), 0b1000_0000);
        assert_eq!(CellFlags::all().bits(), u8::MAX);
    }
}
