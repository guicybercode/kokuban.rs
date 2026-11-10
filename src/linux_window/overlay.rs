//! One-row strips painted over a pane snapshot: the scrollback search prompt
//! and the close confirmation. A strip replaces the cells of one grid row, so
//! the normal glyph path, frame damage and IME placement all apply to it.

use super::draw::GridSnapshot;
use crate::grid::cell::{Cell, CellFlags, Color};
use crate::grid::Grid;
use crate::search::{SearchState, MAX_MATCHES};
use unicode_width::UnicodeWidthChar;

pub(super) type Rgb = (u8, u8, u8);

/// Strip palette: the pane's default colors swapped, so the strip reads as
/// chrome on any theme, plus the hanko accent for keys and "no matches".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StripColors {
    pub(super) text: Rgb,
    pub(super) background: Rgb,
    pub(super) accent: Rgb,
}

/// WCAG AA minimum for normal-size text.
const AA_CONTRAST: f64 = 4.5;

/// WCAG 2 contrast ratio between two sRGB colors, from 1 to 21.
pub(super) fn contrast_ratio(first: Rgb, second: Rgb) -> f64 {
    fn channel(value: u8) -> f64 {
        let value = f64::from(value) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    fn luminance((red, green, blue): Rgb) -> f64 {
        0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
    }
    let (first, second) = (luminance(first), luminance(second));
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

impl StripColors {
    /// The accent marks the keys and "no matches"; a theme whose accent
    /// would fall under AA on the strip falls back to the strip's text color.
    pub(super) fn new(text: Rgb, background: Rgb, accent: Rgb) -> Self {
        let accent = if contrast_ratio(accent, background) >= AA_CONTRAST {
            accent
        } else {
            text
        };
        Self {
            text,
            background,
            accent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StripSegment {
    pub(super) text: String,
    pub(super) foreground: Rgb,
    pub(super) background: Rgb,
}

impl StripSegment {
    pub(super) fn new(text: impl Into<String>, foreground: Rgb, background: Rgb) -> Self {
        Self {
            text: text.into(),
            foreground,
            background,
        }
    }
}

/// The search prompt and the column of its caret, where typed text lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchPrompt {
    pub(super) segments: Vec<StripSegment>,
    pub(super) caret_column: usize,
}

const ELLIPSIS: char = '…';

fn text_width(text: &str) -> usize {
    text.chars().filter_map(UnicodeWidthChar::width).sum()
}

/// The longest suffix of `text` at most `width` columns wide.
fn tail_within(text: &str, width: usize) -> &str {
    let mut used = 0;
    for (index, character) in text.char_indices().rev() {
        used += character.width().unwrap_or(0);
        if used > width {
            let next = index + character.len_utf8();
            return &text[next..];
        }
    }
    text
}

/// Match counter: "3/12", "3/10000+" at the match cap, "no matches", or
/// nothing while the query is empty.
fn match_count(search: &SearchState) -> Option<(String, bool)> {
    if search.query.is_empty() {
        return None;
    }
    let capped = if search.matches.len() >= MAX_MATCHES {
        "+"
    } else {
        ""
    };
    Some(match search.current {
        Some(index) => (
            format!("{}/{}{capped}", index + 1, search.matches.len()),
            false,
        ),
        None => ("no matches".to_string(), true),
    })
}

/// " /query▌  3/12": the query keeps its end visible (an ellipsis replaces
/// the start) and the counter is dropped only when nothing else fits.
pub(super) fn search_prompt(
    search: &SearchState,
    columns: usize,
    colors: StripColors,
) -> SearchPrompt {
    const PREFIX: &str = " /";
    const GAP: usize = 2;
    let count = match_count(search);
    let fixed = text_width(PREFIX) + 1;
    let count_width = count
        .as_ref()
        .map_or(0, |(text, _)| GAP + text_width(text) + 1);
    let count = count.filter(|_| fixed + 1 + count_width <= columns);
    let room = columns
        .saturating_sub(fixed)
        .saturating_sub(count.as_ref().map_or(0, |_| count_width));
    let query_width = text_width(&search.query);
    let query = if query_width <= room {
        search.query.clone()
    } else if room == 0 {
        String::new()
    } else {
        format!("{ELLIPSIS}{}", tail_within(&search.query, room - 1))
    };
    let caret_column = text_width(PREFIX) + text_width(&query);
    let mut segments = vec![
        StripSegment::new(PREFIX, colors.text, colors.background),
        StripSegment::new(query, colors.text, colors.background),
        // A block caret drawn with cell colors, independent of the cursor.
        StripSegment::new(" ", colors.background, colors.text),
    ];
    if let Some((text, missing)) = count {
        let foreground = if missing { colors.accent } else { colors.text };
        segments.push(StripSegment::new(
            " ".repeat(GAP),
            colors.text,
            colors.background,
        ));
        segments.push(StripSegment::new(text, foreground, colors.background));
    }
    SearchPrompt {
        segments,
        caret_column,
    }
}

/// Viewport row of the current match, when it is on screen.
fn current_match_row(search: &SearchState, grid: &Grid) -> Option<usize> {
    let current = search.current_match()?;
    let top = grid.scrollback_len().saturating_sub(grid.scroll_offset);
    usize::try_from(current.row)
        .ok()?
        .checked_sub(top)
        .filter(|row| *row < grid.rows())
}

/// Row the search strip covers. It stays on the last row for the whole
/// search, so navigating never makes it jump; `keep_match_clear_of_strip`
/// scrolls the match off that row instead. Only a match on the last row of
/// the live screen, with nothing below to scroll to, moves the strip to the
/// first row, and it stays there until the search closes.
pub(super) fn search_strip_row(search: &mut SearchState, grid: &Grid) -> usize {
    let last = grid.rows().saturating_sub(1);
    if !search.strip_at_top
        && last > 0
        && grid.scroll_offset == 0
        && current_match_row(search, grid) == Some(last)
    {
        search.strip_at_top = true;
    }
    if search.strip_at_top {
        0
    } else {
        last
    }
}

/// After the search moves to a match: scroll one line when the match sits
/// under the strip, so the strip keeps its row and the match stays visible.
/// Returns true when the viewport moved.
pub(super) fn keep_match_clear_of_strip(search: &SearchState, grid: &mut Grid) -> bool {
    let Some(row) = current_match_row(search, grid) else {
        return false;
    };
    if search.strip_at_top {
        if row == 0 && grid.scroll_offset < grid.scrollback_len() {
            grid.scroll_offset += 1;
            return true;
        }
    } else if row + 1 == grid.rows() && grid.scroll_offset > 0 {
        grid.scroll_offset -= 1;
        return true;
    }
    false
}

/// " Close pane (vim)? [y/N]": only the program names shrink, to an
/// ellipsis or out of the row, so the question and the keys stay visible.
pub(super) fn close_prompt_segments(
    question: &str,
    running: &[String],
    columns: usize,
    colors: StripColors,
) -> Vec<StripSegment> {
    // " " + question + " (" + names + ")" + "? " + "[y/N]".
    let room = columns.saturating_sub(text_width(question) + 11);
    let names = running.join(", ");
    let names = if names.is_empty() || room == 0 {
        String::new()
    } else if text_width(&names) <= room {
        format!(" ({names})")
    } else {
        let mut kept = String::new();
        let mut used = 0;
        for character in names.chars() {
            used += character.width().unwrap_or(0);
            if used + 1 > room {
                break;
            }
            kept.push(character);
        }
        format!(" ({kept}{ELLIPSIS})")
    };
    vec![
        StripSegment::new(
            format!(" {question}{names}? "),
            colors.text,
            colors.background,
        ),
        StripSegment::new("[y/N]", colors.accent, colors.background),
    ]
}

fn strip_cell(character: char, foreground: Rgb, background: Rgb, flags: CellFlags) -> Cell {
    Cell {
        c: character,
        fg: Color::Rgb(foreground.0, foreground.1, foreground.2),
        bg: Color::Rgb(background.0, background.1, background.2),
        flags,
        ..Cell::default()
    }
}

/// Replace `row` of the snapshot with the strip. Text that does not fit ends
/// in an ellipsis; a wide character that would straddle the edge is dropped.
/// The terminal cursor is hidden when it sat on that row, and the row is
/// drawn again after the pane's images so none of them covers the strip.
pub(super) fn paint_strip(
    snapshot: &mut GridSnapshot,
    row: usize,
    segments: &[StripSegment],
    background: Rgb,
) {
    let columns = snapshot.columns;
    if row >= snapshot.rows || columns == 0 {
        return;
    }
    let start = row * columns;
    let line = &mut snapshot.cells[start..start + columns];
    for cell in line.iter_mut() {
        *cell = strip_cell(' ', background, background, CellFlags::empty());
    }
    let total: usize = segments
        .iter()
        .map(|segment| text_width(&segment.text))
        .sum();
    // Leave the last column for the ellipsis when the text is cut.
    let limit = if total > columns {
        columns.saturating_sub(1)
    } else {
        columns
    };
    let mut column: usize = 0;
    let mut truncated = false;
    'segments: for segment in segments {
        for character in segment.text.chars() {
            let Some(width) = character.width() else {
                continue;
            };
            if width == 0 {
                // Combining marks join the previous cell's grapheme.
                if let Some(previous) = column.checked_sub(1).map(|index| &mut line[index]) {
                    let mut text = previous
                        .grapheme
                        .as_deref()
                        .map_or_else(|| previous.c.to_string(), str::to_string);
                    text.push(character);
                    previous.grapheme = Some(text.into());
                }
                continue;
            }
            if column + width > limit {
                truncated = true;
                break 'segments;
            }
            let (foreground, cell_background) = (segment.foreground, segment.background);
            if width == 2 {
                line[column] = strip_cell(character, foreground, cell_background, CellFlags::WIDE);
                line[column + 1] =
                    strip_cell(' ', foreground, cell_background, CellFlags::WIDE_CONT);
            } else {
                line[column] =
                    strip_cell(character, foreground, cell_background, CellFlags::empty());
            }
            column += width;
        }
    }
    if truncated && columns > 0 {
        let foreground = segments
            .first()
            .map_or(background, |segment| segment.foreground);
        line[columns - 1] = strip_cell(ELLIPSIS, foreground, background, CellFlags::empty());
    }
    if snapshot.cursor.is_some_and(|cursor| cursor.row == row) {
        snapshot.cursor = None;
    }
    if !snapshot.strip_rows.contains(&row) {
        snapshot.strip_rows.push(row);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        close_prompt_segments, keep_match_clear_of_strip, paint_strip, search_prompt,
        search_strip_row, StripColors, StripSegment,
    };
    use crate::grid::cell::{CellFlags, Color};
    use crate::grid::{CursorShape, Grid};
    use crate::linux_window::draw::{snapshot_locked_grid, CursorSnapshot, GridSnapshot};
    use crate::search::{SearchKey, SearchMatch, SearchState, MAX_MATCHES};

    const COLORS: StripColors = StripColors {
        text: (31, 58, 42),
        background: (244, 246, 240),
        accent: (181, 49, 44),
    };

    fn row_text(snapshot: &GridSnapshot, row: usize) -> String {
        (0..snapshot.columns)
            .filter_map(|column| snapshot.cell(row, column))
            .filter(|cell| !cell.flags.contains(CellFlags::WIDE_CONT))
            .map(|cell| {
                cell.grapheme
                    .as_deref()
                    .map_or_else(|| cell.c.to_string(), str::to_string)
            })
            .collect()
    }

    fn prompt_text(prompt: &super::SearchPrompt) -> String {
        prompt
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect()
    }

    fn grid_with_lines(columns: usize, rows: usize, lines: &[&str]) -> Grid {
        let mut grid = Grid::new(columns, rows, 100);
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                grid.newline();
                grid.cursor_col = 0;
            }
            for character in line.chars() {
                grid.put_char(character);
            }
        }
        grid
    }

    #[test]
    fn search_prompt_shows_the_query_caret_and_match_count() {
        let grid = grid_with_lines(40, 4, &["error one", "error two", "ok"]);
        let mut search = SearchState::new("error".to_string());
        search.recompute(&grid);
        let prompt = search_prompt(&search, 40, COLORS);
        assert_eq!(prompt_text(&prompt), " /error   2/2");
        assert_eq!(prompt.caret_column, 7);
        let caret = &prompt.segments[2];
        assert_eq!(
            (caret.foreground, caret.background),
            (COLORS.background, COLORS.text)
        );
        assert_eq!(prompt.segments[4].foreground, COLORS.text);

        search.prev();
        assert!(prompt_text(&search_prompt(&search, 40, COLORS)).ends_with("1/2"));
    }

    #[test]
    fn empty_query_shows_only_the_caret_and_misses_use_the_accent() {
        let grid = grid_with_lines(40, 4, &["hello"]);
        let mut search = SearchState::new(String::new());
        search.recompute(&grid);
        let prompt = search_prompt(&search, 40, COLORS);
        assert_eq!(prompt_text(&prompt), " / ");
        assert_eq!(prompt.caret_column, 2);

        let mut search = SearchState::new("absent".to_string());
        search.recompute(&grid);
        let prompt = search_prompt(&search, 40, COLORS);
        assert_eq!(prompt_text(&prompt), " /absent   no matches");
        assert_eq!(prompt.segments.last().unwrap().foreground, COLORS.accent);
    }

    #[test]
    fn the_factory_accent_passes_aa_on_the_strip_and_weak_accents_fall_back() {
        // Factory (blackboard) theme: the strip is chalk #f4f6f0 with board
        // #1f3a2a text; "no matches" and [y/N] use hanko red #b5312c.
        let (board, chalk, hanko) = ((31, 58, 42), (244, 246, 240), (181, 49, 44));
        let ratio = super::contrast_ratio(hanko, chalk);
        assert!((5.60..5.62).contains(&ratio), "{ratio}");
        assert!((11.3..11.4).contains(&super::contrast_ratio(board, chalk)));
        assert_eq!(StripColors::new(board, chalk, hanko).accent, hanko);
        // A light theme (dark text on a light background) gets a dark strip,
        // where hanko red would fall under 4.5:1: use the strip's text color.
        let dark = (34, 34, 34);
        let light = (250, 250, 250);
        assert!(super::contrast_ratio(hanko, dark) < 4.5);
        assert_eq!(StripColors::new(light, dark, hanko).accent, light);
    }

    #[test]
    fn capped_results_are_marked_with_a_plus() {
        let mut search = SearchState::new("x".to_string());
        search.matches = vec![
            SearchMatch {
                row: 0,
                col: 0,
                len: 1
            };
            MAX_MATCHES
        ];
        search.current = Some(MAX_MATCHES - 1);
        assert!(prompt_text(&search_prompt(&search, 40, COLORS)).ends_with("10000/10000+"));
        // Beside a split (29 columns) the counter still fits whole and the
        // query keeps 11 columns; a 14-column pane drops the counter.
        let prompt = search_prompt(&search, 29, COLORS);
        assert_eq!(prompt_text(&prompt), " /x   10000/10000+");
        search.current = Some(0);
        assert_eq!(
            prompt_text(&search_prompt(&search, 29, COLORS)),
            " /x   1/10000+"
        );
        assert_eq!(prompt_text(&search_prompt(&search, 14, COLORS)), " /x ");
    }

    #[test]
    fn long_queries_keep_their_end_visible_then_drop_the_counter() {
        let grid = grid_with_lines(40, 4, &["abcdefghijklmnop"]);
        let mut search = SearchState::new("abcdefghijklmnop".to_string());
        search.recompute(&grid);
        // 20 columns: " /" + query + caret + "  1/1" + trailing space.
        let prompt = search_prompt(&search, 20, COLORS);
        let text = prompt_text(&prompt);
        assert_eq!(text, " /…ghijklmnop   1/1");
        assert_eq!(prompt.caret_column, 13);
        assert!(text.chars().count() < 20);

        // Too narrow for the counter: the query (tail) still shows.
        let prompt = search_prompt(&search, 7, COLORS);
        assert_eq!(prompt_text(&prompt), " /…nop ");
        assert_eq!(prompt.caret_column, 6);
    }

    #[test]
    fn wide_query_characters_are_measured_in_columns() {
        let mut search = SearchState::new("日本語".to_string());
        search.recompute(&Grid::new(20, 2, 10));
        let prompt = search_prompt(&search, 30, COLORS);
        assert_eq!(prompt.caret_column, 8);
        let prompt = search_prompt(&search, 18, COLORS);
        // 18 - " /" - caret - "  no matches " leaves 2 columns: "…" alone fits.
        assert_eq!(prompt.caret_column, 3);
        assert_eq!(prompt.segments[1].text, "…");
    }

    #[test]
    fn strip_replaces_one_row_and_hides_the_cursor_there() {
        let grid = grid_with_lines(12, 3, &["one", "two", "three"]);
        let mut snapshot = snapshot_locked_grid(&grid);
        snapshot.cursor = Some(CursorSnapshot {
            row: 2,
            column: 5,
            shape: CursorShape::Block,
        });
        let segments = vec![
            StripSegment::new(" ab", COLORS.text, COLORS.background),
            StripSegment::new("日", COLORS.accent, COLORS.background),
        ];
        paint_strip(&mut snapshot, 2, &segments, COLORS.background);
        assert_eq!(row_text(&snapshot, 0), "one         ");
        assert_eq!(row_text(&snapshot, 2), " ab日       ");
        let wide = snapshot.cell(2, 3).unwrap();
        assert!(wide.flags.contains(CellFlags::WIDE));
        assert_eq!(wide.fg, Color::Rgb(181, 49, 44));
        assert!(snapshot
            .cell(2, 4)
            .unwrap()
            .flags
            .contains(CellFlags::WIDE_CONT));
        let blank = snapshot.cell(2, 11).unwrap();
        assert_eq!(blank.bg, Color::Rgb(244, 246, 240));
        assert_eq!(snapshot.cursor, None);
        assert_eq!(snapshot.strip_rows, [2]);

        // A strip on another row leaves the cursor alone.
        let mut snapshot = snapshot_locked_grid(&grid);
        let cursor = snapshot.cursor;
        assert!(cursor.is_some_and(|cursor| cursor.row == 2));
        paint_strip(&mut snapshot, 0, &segments, COLORS.background);
        assert_eq!(snapshot.cursor, cursor);
    }

    #[test]
    fn strip_truncates_with_an_ellipsis_and_joins_combining_marks() {
        let grid = Grid::new(8, 2, 10);
        let mut snapshot = snapshot_locked_grid(&grid);
        let segments = vec![StripSegment::new(
            " Close pane? [y/N]",
            COLORS.text,
            COLORS.background,
        )];
        paint_strip(&mut snapshot, 1, &segments, COLORS.background);
        assert_eq!(row_text(&snapshot, 1), " Close …");

        // A wide character that would straddle the edge is dropped.
        let mut snapshot = snapshot_locked_grid(&grid);
        let segments = vec![StripSegment::new(
            "abcdef日x",
            COLORS.text,
            COLORS.background,
        )];
        paint_strip(&mut snapshot, 0, &segments, COLORS.background);
        assert_eq!(row_text(&snapshot, 0), "abcdef …");

        let mut snapshot = snapshot_locked_grid(&grid);
        let segments = vec![StripSegment::new(
            "e\u{301}x",
            COLORS.text,
            COLORS.background,
        )];
        paint_strip(&mut snapshot, 0, &segments, COLORS.background);
        assert_eq!(
            snapshot.cell(0, 0).unwrap().grapheme.as_deref(),
            Some("e\u{301}")
        );
        assert_eq!(snapshot.cell(0, 1).unwrap().c, 'x');
    }

    fn joined(segments: &[StripSegment]) -> String {
        segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect()
    }

    #[test]
    fn close_prompt_names_the_programs_and_highlights_the_keys() {
        let one = ["vim".to_string()];
        let segments = close_prompt_segments("Close pane", &one, 80, COLORS);
        assert_eq!(joined(&segments), " Close pane (vim)? [y/N]");
        assert_eq!(segments[1].foreground, COLORS.accent);

        let two = ["vim".to_string(), "ssh".to_string()];
        let segments = close_prompt_segments("Close window", &two, 80, COLORS);
        assert_eq!(joined(&segments), " Close window (vim, ssh)? [y/N]");
    }

    #[test]
    fn narrow_close_prompts_shrink_only_the_program_names() {
        let names = ["sleep".to_string(), "htop".to_string()];
        // 29 columns, a pane beside a split: 8 columns remain for the names.
        let segments = close_prompt_segments("Close pane", &names, 29, COLORS);
        assert_eq!(joined(&segments), " Close pane (sleep, …)? [y/N]");
        let mut snapshot = snapshot_locked_grid(&Grid::new(29, 2, 10));
        paint_strip(&mut snapshot, 1, &segments, COLORS.background);
        assert_eq!(row_text(&snapshot, 1), " Close pane (sleep, …)? [y/N]");

        let segments = close_prompt_segments("Close pane", &names, 22, COLORS);
        assert_eq!(joined(&segments), " Close pane (…)? [y/N]");
        let segments = close_prompt_segments("Close pane", &names, 21, COLORS);
        assert_eq!(joined(&segments), " Close pane? [y/N]");
    }

    /// `lines` numbered lines in a `rows`-row grid, every one matching
    /// "line", then a shell prompt on the last row when `prompt` is set.
    fn numbered_grid(rows: usize, lines: usize, prompt: bool) -> Grid {
        let mut text: Vec<String> = (0..lines).map(|index| format!("line {index}")).collect();
        if prompt {
            text.push("$ ".to_string());
        }
        let text: Vec<&str> = text.iter().map(String::as_str).collect();
        grid_with_lines(20, rows, &text)
    }

    #[test]
    fn search_strip_keeps_the_last_row_while_navigating() {
        // Ten lines and a prompt in a four-row pane: seven in the scrollback.
        let mut grid = numbered_grid(4, 10, true);
        let mut search = SearchState::new("line".to_string());
        search.recompute(&grid);
        let last = grid.rows() - 1;
        let mut rows = Vec::new();
        // Enter walks down from the oldest line; "line 3" lands on the last
        // row of the viewport and must be scrolled clear of the strip.
        for _ in 0..12 {
            search.handle_key(SearchKey::Enter, &mut grid);
            keep_match_clear_of_strip(&search, &mut grid);
            rows.push(search_strip_row(&mut search, &grid));
            // Every match, including those that land on the last row, is
            // shown above the strip.
            let row = super::current_match_row(&search, &grid);
            assert!(row.is_some_and(|row| row < last), "{search:?} at {row:?}");
        }
        assert!(rows.iter().all(|row| *row == last), "{rows:?}");
        assert!(!search.strip_at_top);
    }

    #[test]
    fn a_match_on_the_live_last_row_moves_the_strip_up_once() {
        // No prompt: "line 9" is the live last row, with nothing below it.
        let mut grid = numbered_grid(4, 10, false);
        let mut search = SearchState::new("line 9".to_string());
        search.recompute(&grid);
        assert_eq!(grid.scroll_offset, 0);
        assert_eq!(search_strip_row(&mut search, &grid), 0);
        // It stays on the first row for every other match, too.
        search.query = "line".to_string();
        search.recompute(&grid);
        // Shift+Enter walks up, so matches reach the first row and must be
        // scrolled clear of the strip there; Enter walks back down.
        for key in [SearchKey::ShiftEnter; 12]
            .into_iter()
            .chain([SearchKey::Enter; 12])
        {
            search.handle_key(key, &mut grid);
            keep_match_clear_of_strip(&search, &mut grid);
            assert_eq!(search_strip_row(&mut search, &grid), 0);
            if search.current != Some(0) {
                let row = super::current_match_row(&search, &grid);
                assert!(row.is_some_and(|row| row > 0), "{search:?} at {row:?}");
            }
        }
        // A new search starts on the last row again.
        let mut search = SearchState::new("line 2".to_string());
        search.recompute(&grid);
        assert_eq!(search_strip_row(&mut search, &grid), grid.rows() - 1);
    }

    #[test]
    fn misses_and_empty_queries_keep_the_strip_on_the_last_row() {
        let grid = grid_with_lines(20, 3, &["alpha", "beta", "gamma"]);
        let mut search = SearchState::new("absent".to_string());
        search.recompute(&grid);
        assert_eq!(search_strip_row(&mut search, &grid), 2);
        let mut search = SearchState::new(String::new());
        search.recompute(&grid);
        assert_eq!(search_strip_row(&mut search, &grid), 2);
    }
}
