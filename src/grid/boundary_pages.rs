//! Conservative, stateless decisions for successors whose category needs no context.
//!
//! In the pinned segmentation rules, GC_Any joins only after Prepend (GB9b).
//! Extend/ZWJ/SpacingMark join except after Control/CR/LF (GB4 before GB9/9a).
//! Mixed pages and all other categories defer to the existing GraphemeCursor.

include!("boundary_pages_data.rs");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SuccessorClass {
    Unknown,
    Other,
    Attach,
}

#[inline]
fn successor_class(c: char) -> SuccessorClass {
    if matches!(c, ' '..='~') {
        return SuccessorClass::Other;
    }
    let page = (c as usize) >> 6;
    let value = (SUCCESSOR_PAGES[page >> 5] >> ((page & 31) * 2)) & 3;
    match value {
        1 => SuccessorClass::Other,
        2 => SuccessorClass::Attach,
        _ => SuccessorClass::Unknown,
    }
}

/// True only for successors that join no predecessor other than Prepend.
#[inline]
pub(super) fn is_other(c: char) -> bool {
    unicode_segmentation::UNICODE_VERSION == GENERATED_UNICODE_VERSION
        && successor_class(c) == SuccessorClass::Other
}

#[inline]
pub(super) fn scalar_extends(last: char, next: char) -> Option<bool> {
    // The exact dependency pin protects against category changes within the
    // same Unicode release; this guard also rejects a different Unicode version.
    if unicode_segmentation::UNICODE_VERSION != GENERATED_UNICODE_VERSION {
        return None;
    }
    match successor_class(next) {
        SuccessorClass::Other => Some(is_prepend(last)),
        SuccessorClass::Attach => Some(!is_control_cr_lf(last)),
        SuccessorClass::Unknown => None,
    }
}

#[cfg(test)]
#[path = "boundary_pages_tests.rs"]
mod tests;
