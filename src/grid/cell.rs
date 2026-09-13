use bitflags::bitflags;
use std::{borrow::Cow, fmt, ops::Deref, sync::Arc};
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

/// Immutable UTF-8 text for a compound cell. Short clusters need no allocation.
/// The private representation keeps length-based storage and zeroed inline tails
/// canonical, so derived equality is also textual equality.
#[derive(Clone, PartialEq, Eq)]
pub struct Grapheme(GraphemeRepr);

#[derive(Clone, PartialEq, Eq)]
enum GraphemeRepr {
    Inline {
        len: u8,
        bytes: [u8; Grapheme::INLINE_CAPACITY],
    },
    Heap(Arc<String>),
}

impl Grapheme {
    pub(super) const INLINE_CAPACITY: usize = 14;

    /// Append one encoded scalar without rebuilding short text through a string.
    #[inline]
    pub(super) fn from_appended(previous: &str, c: char) -> Self {
        let len = previous.len() + c.len_utf8();
        if len <= Self::INLINE_CAPACITY {
            let mut bytes = [0; Self::INLINE_CAPACITY];
            bytes[..previous.len()].copy_from_slice(previous.as_bytes());
            c.encode_utf8(&mut bytes[previous.len()..len]);
            Self(GraphemeRepr::Inline {
                len: len as u8,
                bytes,
            })
        } else {
            let mut text = String::with_capacity(len);
            text.push_str(previous);
            text.push(c);
            Self(GraphemeRepr::Heap(Arc::new(text)))
        }
    }

    #[cfg(test)]
    pub(crate) fn heap_weak(&self) -> Option<std::sync::Weak<String>> {
        match &self.0 {
            GraphemeRepr::Inline { .. } => None,
            GraphemeRepr::Heap(text) => Some(Arc::downgrade(text)),
        }
    }
}

impl From<&str> for Grapheme {
    fn from(text: &str) -> Self {
        if text.len() <= Self::INLINE_CAPACITY {
            let mut bytes = [0; Self::INLINE_CAPACITY];
            bytes[..text.len()].copy_from_slice(text.as_bytes());
            Self(GraphemeRepr::Inline {
                len: text.len() as u8,
                bytes,
            })
        } else {
            Self(GraphemeRepr::Heap(Arc::new(text.to_owned())))
        }
    }
}

impl From<String> for Grapheme {
    fn from(text: String) -> Self {
        if text.len() <= Self::INLINE_CAPACITY {
            Self::from(text.as_str())
        } else {
            Self(GraphemeRepr::Heap(Arc::new(text)))
        }
    }
}

impl Deref for Grapheme {
    type Target = str;

    fn deref(&self) -> &str {
        match &self.0 {
            GraphemeRepr::Inline { len, bytes } => std::str::from_utf8(&bytes[..usize::from(*len)])
                .expect("inline graphemes are constructed from valid UTF-8"),
            GraphemeRepr::Heap(text) => text,
        }
    }
}

impl AsRef<str> for Grapheme {
    fn as_ref(&self) -> &str {
        self
    }
}

impl fmt::Debug for Grapheme {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_ref(), formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub c: char,
    pub grapheme: Option<Grapheme>,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
    pub underline_style: UnderlineStyle,
    pub underline_color: Color,
}

impl Cell {
    /// Replace compound text with one scalar and copy only the requested style.
    #[inline]
    pub(super) fn write_scalar(&mut self, c: char, style: &Self, flags: CellFlags) {
        self.grapheme = None;
        self.c = c;
        self.fg = style.fg;
        self.bg = style.bg;
        self.flags = flags;
        self.underline_style = style.underline_style;
        self.underline_color = style.underline_color;
    }

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
        }
        .clamp(1, 2)
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
    use super::{Cell, CellFlags, Color, Grapheme, GraphemeRepr, UnderlineStyle};

    #[test]
    fn grapheme_constructors_preserve_utf8_and_canonical_storage() {
        let mut samples = vec![
            String::new(),
            "e\u{301}".into(),
            "🇧🇷".into(),
            "1\u{fe0f}\u{20e3}".into(),
            "👩🏽‍💻".into(),
            "a\0\u{301}".into(),
        ];
        for len in [13, 14, 15, 63, 64, 65, 8195] {
            let mut text = String::from(if len % 2 == 0 { "é" } else { "e" });
            text.push_str(&"\u{301}".repeat((len - text.len()) / 2));
            assert_eq!(text.len(), len);
            samples.push(text);
        }
        for text in samples {
            let borrowed = Grapheme::from(text.as_str());
            let owned = Grapheme::from(text.clone());
            assert_eq!(borrowed, owned);
            assert_eq!(borrowed.as_ref(), text);
            assert_eq!(borrowed.as_bytes(), text.as_bytes());
            assert_eq!(format!("{borrowed:?}"), format!("{text:?}"));
            assert_eq!(borrowed.heap_weak().is_none(), text.len() <= 14);
            if let GraphemeRepr::Inline { len, bytes } = &borrowed.0 {
                assert_eq!(usize::from(*len), text.len());
                assert!(bytes[text.len()..].iter().all(|&byte| byte == 0));
            }
        }
        assert_ne!(Grapheme::from("e\u{301}"), Grapheme::from("e\u{308}"));
        assert_ne!(Grapheme::from("e\u{301}"), Grapheme::from("e\u{301}\0"));
    }

    #[test]
    fn appended_scalars_match_concatenation_and_canonical_storage() {
        let mut prefixes = vec![
            String::new(),
            "e\0".into(),
            "é".into(),
            "日".into(),
            "🇧🇷".into(),
            "👩🏽‍".into(),
        ];
        prefixes.extend((0..=17).map(|len| "a".repeat(len)));
        prefixes.extend(["é".repeat(32), format!("e{}", "\u{301}".repeat(32))]);
        for previous in prefixes {
            for next in [
                '\0',
                '\u{7f}',
                '\u{80}',
                '\u{7ff}',
                '\u{800}',
                '\u{ffff}',
                '\u{10000}',
                '\u{10ffff}',
            ] {
                let mut expected = previous.clone();
                expected.push(next);
                let actual = Grapheme::from_appended(&previous, next);
                assert_eq!(actual.as_bytes(), expected.as_bytes());
                assert_eq!(actual, Grapheme::from(expected.as_str()));
                match &actual.0 {
                    GraphemeRepr::Inline { len, bytes } => {
                        assert!(expected.len() <= Grapheme::INLINE_CAPACITY);
                        assert_eq!(usize::from(*len), expected.len());
                        assert!(bytes[expected.len()..].iter().all(|&byte| byte == 0));
                    }
                    GraphemeRepr::Heap(text) => {
                        assert!(expected.len() > Grapheme::INLINE_CAPACITY);
                        assert_eq!(text.as_str(), expected);
                        let owner = actual.heap_weak().unwrap();
                        let snapshot = actual.clone();
                        drop(actual);
                        assert_eq!(snapshot.as_ref(), expected);
                        drop(snapshot);
                        assert!(owner.upgrade().is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn grapheme_clones_keep_text_alive_and_take_ownership_of_long_strings() {
        let text = "e\u{301}".to_owned();
        let inline = Grapheme::from(text);
        let cloned = inline.clone();
        drop(inline);
        assert_eq!(cloned.as_ref(), "e\u{301}");
        assert!(cloned.heap_weak().is_none());

        let long = format!("e{}", "\u{301}".repeat(7));
        let string_bytes = long.as_ptr();
        let heap = Grapheme::from(long);
        assert_eq!(heap.as_ptr(), string_bytes);
        let owner = heap.heap_weak().unwrap();
        let cloned = heap.clone();
        assert!(owner.ptr_eq(&cloned.heap_weak().unwrap()));
        drop(heap);
        assert_eq!(owner.strong_count(), 1);
        assert_eq!(cloned.as_ref(), format!("e{}", "\u{301}".repeat(7)));
        drop(cloned);
        assert!(owner.upgrade().is_none());
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn inline_graphemes_keep_the_existing_cell_layout() {
        use std::mem::size_of;
        assert_eq!(size_of::<Grapheme>(), 16);
        assert_eq!(size_of::<Option<Grapheme>>(), 16);
        assert_eq!(size_of::<Cell>(), 40);
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Grapheme>();
    }

    #[test]
    fn scalar_writes_copy_style_and_release_compound_text() {
        let style = Cell {
            c: 'S',
            grapheme: Some(Grapheme::from("e\u{301}")),
            fg: Color::Indexed(7),
            bg: Color::Rgb(3, 4, 5),
            flags: CellFlags::BOLD | CellFlags::WIDE_CONT,
            underline_style: UnderlineStyle::Curly,
            underline_color: Color::Indexed(2),
        };
        for text in ["e\u{301}".to_owned(), format!("e{}", "\u{301}".repeat(7))] {
            let grapheme = Grapheme::from(text);
            let heap_owner = grapheme.heap_weak();
            let mut cell = Cell { grapheme: Some(grapheme), ..Cell::default() };
            let flags = CellFlags::ITALIC | CellFlags::WIDE;
            cell.write_scalar('日', &style, flags);
            assert_eq!(cell, Cell {
                c: '日', grapheme: None, fg: style.fg, bg: style.bg, flags,
                underline_style: style.underline_style, underline_color: style.underline_color,
            });
            if let Some(owner) = heap_owner {
                assert!(owner.upgrade().is_none(), "scalar overwrite must release the heap owner");
            }
            assert_eq!(style.grapheme.as_deref(), Some("e\u{301}"));
        }
    }

    #[test]
    fn hidden_uses_the_last_unassigned_cell_flag_bit() {
        assert_eq!(CellFlags::HIDDEN.bits(), 0b1000_0000);
        assert_eq!(CellFlags::all().bits(), u8::MAX);
    }
}
