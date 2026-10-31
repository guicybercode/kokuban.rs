//! OSC 8 hyperlink spans, kept in a side table keyed by absolute row so the
//! `Cell` layout stays untouched.

use std::collections::VecDeque;
use std::sync::Arc;

/// Upper bound on retained spans; the oldest span is dropped past this.
pub const MAX_HYPERLINK_SPANS: usize = 4096;
/// Longest URI accepted by OSC 8; longer targets are ignored.
pub const MAX_HYPERLINK_URI_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperlinkSpan {
    pub abs_row: usize,
    pub start_col: usize,
    /// Exclusive end column.
    pub end_col: usize,
    pub url: Arc<str>,
}

#[derive(Debug, Default)]
pub struct HyperlinkIndex {
    spans: VecDeque<HyperlinkSpan>,
    /// Highest `abs_row` of any retained span; rows above it need no overlap scan.
    max_abs_row: Option<usize>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl HyperlinkIndex {
    pub fn len(&self) -> usize {
        self.spans.len()
    }
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
    pub fn spans(&self) -> impl Iterator<Item = &HyperlinkSpan> {
        self.spans.iter()
    }

    /// Record `width` cells written at (`abs_row`, `col`) under `url`,
    /// extending the previous span when the write is contiguous.
    pub fn record(&mut self, abs_row: usize, col: usize, width: usize, url: &Arc<str>) {
        let width = width.max(1);
        if let Some(last) = self.spans.back_mut() {
            if last.abs_row == abs_row && last.end_col == col && Arc::ptr_eq(&last.url, url) {
                last.end_col = col + width;
                return;
            }
        }
        self.clear_cells(abs_row, col, col + width);
        if self.spans.len() >= MAX_HYPERLINK_SPANS {
            self.spans.pop_front();
        }
        self.spans.push_back(HyperlinkSpan {
            abs_row,
            start_col: col,
            end_col: col + width,
            url: Arc::clone(url),
        });
        self.max_abs_row = Some(self.max_abs_row.map_or(abs_row, |max| max.max(abs_row)));
    }

    /// Forget the link under cells `[start, end)` of `abs_row` because other
    /// text overwrote them.
    pub fn clear_cells(&mut self, abs_row: usize, start: usize, end: usize) {
        if self.max_abs_row.is_none_or(|max| abs_row > max) || start >= end {
            return;
        }
        let mut split = Vec::new();
        self.spans.retain_mut(|span| {
            if span.abs_row != abs_row || span.end_col <= start || span.start_col >= end {
                return true;
            }
            if span.start_col < start && span.end_col > end {
                split.push(HyperlinkSpan {
                    start_col: end,
                    ..span.clone()
                });
                span.end_col = start;
                return true;
            }
            if span.start_col < start {
                span.end_col = start;
                return true;
            }
            if span.end_col > end {
                span.start_col = end;
                return true;
            }
            false
        });
        self.spans.extend(split);
    }

    pub fn hyperlink_at(&self, abs_row: usize, col: usize) -> Option<&str> {
        self.spans
            .iter()
            .rev()
            .find(|span| span.abs_row == abs_row && col >= span.start_col && col < span.end_col)
            .map(|span| &*span.url)
    }

    /// Drop spans on rows that left the scrollback.
    pub fn evict_before(&mut self, first_retained_abs_row: usize) {
        while self
            .spans
            .front()
            .is_some_and(|span| span.abs_row < first_retained_abs_row)
        {
            self.spans.pop_front();
        }
        self.spans
            .retain(|span| span.abs_row >= first_retained_abs_row);
        if self.spans.is_empty() {
            self.max_abs_row = None;
        }
    }

    /// ED 3: history is gone and the screen now starts at absolute row 0.
    pub fn erase_saved_lines(&mut self, screen_start_absolute_row: usize) {
        self.evict_before(screen_start_absolute_row);
        for span in &mut self.spans {
            span.abs_row -= screen_start_absolute_row;
        }
        self.max_abs_row = self
            .max_abs_row
            .map(|max| max.saturating_sub(screen_start_absolute_row));
    }

    pub fn clear(&mut self) {
        self.spans.clear();
        self.max_abs_row = None;
    }
}

/// Parse the `pt` of `OSC 8 ; params ; URI`. Returns `Some(None)` to close the
/// active link, `Some(Some(url))` to open one, `None` when malformed.
pub fn parse_osc8(pt: &str) -> Option<Option<Arc<str>>> {
    let (_params, uri) = pt.split_once(';')?;
    if uri.is_empty() {
        return Some(None);
    }
    if uri.len() > MAX_HYPERLINK_URI_BYTES || uri.chars().any(char::is_control) {
        log::info!("Ignoring OSC 8 hyperlink: URI rejected");
        return None;
    }
    Some(Some(Arc::from(uri)))
}

/// Schemes a click may hand to the system opener.
pub fn is_openable_hyperlink(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    ["http", "https", "file", "mailto"]
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_osc8_distinguishes_open_close_and_rejects_long_uris() {
        assert_eq!(
            parse_osc8(";https://a.example").unwrap().as_deref(),
            Some("https://a.example")
        );
        assert_eq!(
            parse_osc8("id=x;mailto:a@b").unwrap().as_deref(),
            Some("mailto:a@b")
        );
        assert_eq!(parse_osc8(";"), Some(None));
        assert_eq!(parse_osc8("id=x;"), Some(None));
        assert_eq!(parse_osc8("no-separator"), None);
        let long = format!(";https://{}", "a".repeat(MAX_HYPERLINK_URI_BYTES));
        assert_eq!(parse_osc8(&long), None);
    }

    #[test]
    fn scheme_allowlist_is_case_insensitive() {
        assert!(is_openable_hyperlink("HTTPS://x"));
        assert!(is_openable_hyperlink("file:///tmp"));
        assert!(!is_openable_hyperlink("javascript:alert(1)"));
        assert!(!is_openable_hyperlink("no-scheme"));
    }

    #[test]
    fn clear_cells_splits_and_trims_spans() {
        let mut index = HyperlinkIndex::default();
        let url: Arc<str> = Arc::from("https://x");
        for col in 0..10 {
            index.record(3, col, 1, &url);
        }
        assert_eq!(index.len(), 1);
        index.clear_cells(3, 4, 6);
        assert_eq!(index.hyperlink_at(3, 3), Some("https://x"));
        assert_eq!(index.hyperlink_at(3, 4), None);
        assert_eq!(index.hyperlink_at(3, 5), None);
        assert_eq!(index.hyperlink_at(3, 6), Some("https://x"));
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn span_cap_drops_oldest() {
        let mut index = HyperlinkIndex::default();
        let url: Arc<str> = Arc::from("https://x");
        for row in 0..(MAX_HYPERLINK_SPANS + 5) {
            index.record(row, 0, 1, &url);
        }
        assert_eq!(index.len(), MAX_HYPERLINK_SPANS);
        assert_eq!(index.hyperlink_at(0, 0), None);
        assert_eq!(
            index.hyperlink_at(MAX_HYPERLINK_SPANS + 4, 0),
            Some("https://x")
        );
    }
}
