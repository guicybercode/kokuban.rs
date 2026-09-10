//! Cross-module content contracts through the production PTY byte decoder.
//!
//! These fixtures end with a visible character so the selection helper can
//! exclude unused viewport padding without trimming any expected content.

use crate::grid::{
    cell::{CellFlags, Color, UnderlineStyle},
    Grid,
};
use crate::parser::ansi::GraphicsSupport;
use crate::selection::{GridPoint, SelectionState, SelectionTextError};
use crate::terminal_decoder::TerminalDecoder;

fn decoder() -> TerminalDecoder {
    TerminalDecoder::new(GraphicsSupport {
        kitty: false,
        sixel: false,
    })
}

fn feed(decoder: &mut TerminalDecoder, grid: &mut Grid, mut input: &[u8]) {
    while !input.is_empty() {
        let step = decoder.feed_until_event(input, grid);
        assert!(step.consumed > 0 && step.consumed <= input.len());
        assert!(
            step.events.is_empty(),
            "fixtures must not emit protocol events"
        );
        input = &input[step.consumed..];
    }
}

fn select(start: GridPoint, end: GridPoint) -> SelectionState {
    let mut selection = SelectionState::default();
    selection.start(start);
    selection.update(end);
    selection
}

fn content_selection(grid: &Grid) -> SelectionState {
    for row in (0..grid.retained_rows()).rev() {
        for col in (0..grid.cols()).rev() {
            let cell = grid.retained_cell_data(row, col);
            if cell.c != ' ' && cell.c != '\0' && !cell.flags.contains(CellFlags::WIDE_CONT) {
                return select(
                    GridPoint { row: 0, col: 0 },
                    GridPoint {
                        row: row as i64,
                        col,
                    },
                );
            }
        }
    }
    panic!("fixture unexpectedly lost all visible content");
}

fn copied_content(grid: &Grid) -> String {
    content_selection(grid).get_text(grid)
}

#[test]
fn wrapped_urls_and_commands_copy_exactly_across_every_read_split() {
    let input =
        b"https://example.test/a/long/path?first=one&second=two\r\nprintf '%s' 'a long argument'";
    let expected =
        "https://example.test/a/long/path?first=one&second=two\nprintf '%s' 'a long argument'";

    for cols in [5, 11, 37] {
        for split in 0..=input.len() {
            let mut grid = Grid::new(cols, 3, 128);
            let mut decoder = decoder();
            feed(&mut decoder, &mut grid, &input[..split]);
            feed(&mut decoder, &mut grid, &input[split..]);

            assert_eq!(
                copied_content(&grid),
                expected,
                "cols={cols}, split={split}"
            );
            let forward = content_selection(&grid);
            let (start, end) = forward.normalized().unwrap();
            assert_eq!(select(end, start).get_text(&grid), expected);
        }
    }
}

#[test]
fn unicode_graphemes_are_one_selectable_unit_across_every_utf8_split() {
    for (cluster, columns) in [
        ("e\u{301}", 1),
        ("a\u{308}\u{301}", 1),
        ("👩🏽\u{200d}💻", 2),
        ("👨\u{200d}👩\u{200d}👧\u{200d}👦", 2),
        ("🇧🇷", 2),
        ("1\u{fe0f}\u{20e3}", 2),
        ("❤\u{fe0f}", 2),
        ("\u{1100}\u{1161}", 2),
        // The Arabic number sign is a visible Prepend scalar: together with
        // the following ASCII base it is one grapheme occupying two columns.
        ("\u{600}a", 2),
    ] {
        let input = format!("{cluster}!");
        for split in 0..=input.len() {
            let mut grid = Grid::new(8, 2, 32);
            let mut decoder = decoder();
            feed(&mut decoder, &mut grid, &input.as_bytes()[..split]);
            feed(&mut decoder, &mut grid, &input.as_bytes()[split..]);

            assert_eq!(
                copied_content(&grid),
                input,
                "cluster={cluster:?}, split={split}"
            );
            let leader = GridPoint { row: 0, col: 0 };
            assert_eq!(select(leader, leader).get_text(&grid), cluster);
            assert_eq!(grid.cursor_col, columns + 1, "cluster={cluster:?}");
            if columns == 2 {
                let continuation = GridPoint { row: 0, col: 1 };
                assert_eq!(select(continuation, continuation).get_text(&grid), cluster);
            }
        }
    }
}

#[test]
fn bytewise_graphemes_survive_wraps_and_copy_byte_limits() {
    let input = "e\u{301}👩🏽\u{200d}💻🇧🇷1\u{fe0f}\u{20e3}Z";
    for cols in [1, 2, 3, 7] {
        let mut grid = Grid::new(cols, 2, 64);
        let mut decoder = decoder();
        for byte in input.as_bytes() {
            feed(&mut decoder, &mut grid, std::slice::from_ref(byte));
        }
        let selection = content_selection(&grid);
        assert_eq!(
            selection.get_text_with_limit(&grid, input.len()),
            Ok(input.to_string()),
            "cols={cols}",
        );
        assert_eq!(
            selection.get_text_with_limit(&grid, input.len() - 1),
            Err(SelectionTextError::TooLarge),
        );
    }
}

#[test]
fn a_combining_mark_extends_the_right_margin_before_the_next_wrap() {
    let mut grid = Grid::new(3, 2, 32);
    let mut decoder = decoder();
    feed(&mut decoder, &mut grid, b"abe");
    feed(&mut decoder, &mut grid, "\u{301}".as_bytes());

    let margin = GridPoint { row: 0, col: 2 };
    assert_eq!(select(margin, margin).get_text(&grid), "e\u{301}");
    feed(&mut decoder, &mut grid, b"Z");
    assert_eq!(copied_content(&grid), "abe\u{301}Z");
    assert_eq!((grid.cursor_row, grid.cursor_col), (1, 1));
}

#[test]
fn soft_wraps_preserve_spaces_and_omit_unused_wide_character_padding() {
    for input in ["ab  cd   ef\r\nx\u{a0}y", "abc日def語ghi", "ab \u{301}cd"] {
        let expected = input.replace("\r\n", "\n");
        for cols in [2, 4, 5] {
            let mut grid = Grid::new(cols, 2, 64);
            let mut decoder = decoder();
            feed(&mut decoder, &mut grid, input.as_bytes());
            assert_eq!(
                copied_content(&grid),
                expected,
                "input={input:?}, cols={cols}"
            );
        }
    }
}

#[test]
fn repeated_width_and_height_changes_preserve_history_and_visible_text() {
    let input = "history https://example.test/long/path\r\nsecond  line e\u{301} 👩🏽\u{200d}💻\r\ncurrent 🇧🇷 1\u{fe0f}\u{20e3} 日本語 END";
    let expected = input.replace("\r\n", "\n");
    let mut grid = Grid::new(19, 3, 1024);
    let mut decoder = decoder();
    for chunk in input.as_bytes().chunks(5) {
        feed(&mut decoder, &mut grid, chunk);
    }
    assert!(grid.scrollback_len() > 0);
    assert_eq!(copied_content(&grid), expected);

    for (cols, rows) in [(9, 3), (4, 2), (1, 1), (7, 5), (25, 10), (3, 4), (40, 12)] {
        grid.resize(cols, rows);
        assert_eq!(
            copied_content(&grid),
            expected,
            "after resize to {cols}x{rows}"
        );
    }
}

#[test]
fn shrinking_only_height_retains_rows_that_leave_the_viewport() {
    let input = b"first\r\nsecond\r\nthird\r\nfourth\r\nfifth";
    let expected = "first\nsecond\nthird\nfourth\nfifth";
    let mut grid = Grid::new(16, 6, 32);
    let mut decoder = decoder();
    feed(&mut decoder, &mut grid, input);

    grid.resize(16, 2);
    assert!(grid.scrollback_len() >= 3);
    assert_eq!(copied_content(&grid), expected);
    grid.resize(16, 6);
    assert_eq!(copied_content(&grid), expected);
}

#[test]
fn text_received_after_resize_appends_at_the_reflowed_cursor() {
    for initial_columns in [5, 12] {
        for (cols, rows) in [(3, 2), (1, 1), (19, 5)] {
            let mut grid = Grid::new(initial_columns, 3, 128);
            let mut decoder = decoder();
            feed(&mut decoder, &mut grid, b"hello");
            grid.resize(cols, rows);
            feed(
                &mut decoder,
                &mut grid,
                "-e\u{301}👩\u{200d}💻-tail".as_bytes(),
            );
            assert_eq!(
                copied_content(&grid),
                "hello-e\u{301}👩\u{200d}💻-tail",
                "initial={initial_columns}, resized={cols}x{rows}",
            );
        }
    }
}

#[test]
fn saved_cursor_restores_its_logical_text_position_after_resize() {
    let mut grid = Grid::new(12, 4, 64);
    let mut decoder = decoder();
    feed(&mut decoder, &mut grid, b"abcdefgh\x1b7\x1b[3;1Hbottom");
    grid.resize(4, 6);
    feed(&mut decoder, &mut grid, b"\x1b8Z");

    assert_eq!(copied_content(&grid), "abcdefghZ\n\nbottom");
}

#[test]
fn resizing_an_alternate_screen_preserves_hidden_primary_content_and_cursor() {
    let input = "primary https://example.test/long/path e\u{301} 👩\u{200d}💻";
    let mut grid = Grid::new(10, 4, 128);
    let mut decoder = decoder();
    feed(&mut decoder, &mut grid, input.as_bytes());
    feed(&mut decoder, &mut grid, b"\x1b[?1049hALT");
    grid.resize(3, 2);
    grid.resize(15, 5);
    feed(&mut decoder, &mut grid, b"\x1b[?1049l-tail");

    assert_eq!(copied_content(&grid), format!("{input}-tail"));
}

#[test]
fn nonmoving_sgr_between_scalars_does_not_break_a_grapheme() {
    let mut grid = Grid::new(3, 2, 16);
    let mut decoder = decoder();
    feed(
        &mut decoder,
        &mut grid,
        "abe\x1b[31m\u{301}\x1b[0mZ".as_bytes(),
    );

    assert_eq!(copied_content(&grid), "abe\u{301}Z");
    let margin = GridPoint { row: 0, col: 2 };
    assert_eq!(select(margin, margin).get_text(&grid), "e\u{301}");
}

#[test]
fn cursor_movement_and_erasure_do_not_reconnect_a_previous_grapheme() {
    for (input, expected) in [
        ("e\r\n\u{301}Z", "e\n\u{301}Z"),
        ("e\x1b[2K\r\u{301}Z", "\u{301}Z"),
        ("e\x1b[2;1H\u{301}Z", "e\n\u{301}Z"),
    ] {
        let mut grid = Grid::new(8, 3, 16);
        let mut decoder = decoder();
        for byte in input.as_bytes() {
            feed(&mut decoder, &mut grid, std::slice::from_ref(byte));
        }
        assert_eq!(copied_content(&grid), expected, "input={input:?}");
    }
}

#[test]
fn a_late_emoji_presentation_selector_reflows_the_right_margin() {
    for cols in [2, 3, 5] {
        let prefix = "a".repeat(cols - 1);
        let mut grid = Grid::new(cols, 2, 32);
        let mut decoder = decoder();
        feed(&mut decoder, &mut grid, format!("{prefix}❤").as_bytes());
        feed(&mut decoder, &mut grid, "\u{fe0f}Z".as_bytes());

        assert_eq!(copied_content(&grid), format!("{prefix}❤\u{fe0f}Z"));
        let row = grid.scrollback_len() + grid.cursor_row;
        // The emoji needs a leader and continuation together on a new row.
        let emoji_row = if cols == 2 { row - 1 } else { row };
        let emoji = GridPoint {
            row: emoji_row as i64,
            col: 0,
        };
        assert_eq!(select(emoji, emoji).get_text(&grid), "❤\u{fe0f}");
    }
}

#[test]
fn resizing_between_grapheme_bytes_keeps_the_cluster_intact() {
    for cluster in [
        "e\u{301}",
        "🇧🇷",
        "👩🏽\u{200d}💻",
        "1\u{fe0f}\u{20e3}",
        "\u{600}a",
    ] {
        let input = cluster.as_bytes();
        for split in 0..=input.len() {
            let mut grid = Grid::new(7, 3, 128);
            let mut decoder = decoder();
            feed(&mut decoder, &mut grid, b"abc");
            feed(&mut decoder, &mut grid, &input[..split]);
            grid.resize(2, 2);
            feed(&mut decoder, &mut grid, &input[split..]);
            feed(&mut decoder, &mut grid, b"Z");
            grid.resize(11, 4);

            assert_eq!(
                copied_content(&grid),
                format!("abc{cluster}Z"),
                "cluster={cluster:?}, split={split}"
            );
            let leader = GridPoint { row: 0, col: 3 };
            assert_eq!(
                select(leader, leader).get_text(&grid),
                cluster,
                "cluster={cluster:?}, split={split}"
            );
        }
    }
}

#[test]
fn resize_does_not_evict_existing_text_when_scrollback_is_disabled_or_tiny() {
    let input = "first long line e\u{301}\r\nsecond 👩🏽\u{200d}💻\r\nlast line";
    let expected = input.replace("\r\n", "\n");
    for scrollback_max in [0, 1] {
        let mut grid = Grid::new(24, 4, scrollback_max);
        let mut decoder = decoder();
        feed(&mut decoder, &mut grid, input.as_bytes());
        assert_eq!(copied_content(&grid), expected);

        for (cols, rows) in [(2, 2), (1, 1), (30, 8), (5, 2), (24, 4)] {
            grid.resize(cols, rows);
            assert_eq!(
                copied_content(&grid),
                expected,
                "scrollback_max={scrollback_max}, after resize to {cols}x{rows}",
            );
        }
    }
}

#[test]
fn shrinking_with_cursor_at_top_preserves_populated_rows_below_the_viewport() {
    let mut grid = Grid::new(16, 5, 0);
    let mut decoder = decoder();
    feed(
        &mut decoder,
        &mut grid,
        b"top\r\nmiddle-row\r\nbottom-content\x1b[H",
    );
    for (cols, rows) in [(4, 2), (2, 1), (16, 5)] {
        grid.resize(cols, rows);
        assert_eq!(copied_content(&grid), "top\nmiddle-row\nbottom-content");
    }
    feed(&mut decoder, &mut grid, b"X");
    assert_eq!(copied_content(&grid), "Xop\nmiddle-row\nbottom-content");
}

#[test]
fn character_insertion_deletion_and_erasure_keep_graphemes_and_styles_together() {
    let mut grid = Grid::new(16, 2, 16);
    let mut decoder = decoder();
    let accent = "e\u{301}";
    let emoji = "👩🏽\u{200d}💻";
    feed(
        &mut decoder,
        &mut grid,
        format!("\x1b[1;4;31;44m{accent}{emoji}\x1b[0mZ").as_bytes(),
    );

    for (control, accent_column, expected) in [
        (
            b"\x1b[1;1H\x1b[2@".as_slice(),
            2,
            format!("  {accent}{emoji}Z"),
        ),
        (
            b"\x1b[1;1H\x1b[P".as_slice(),
            1,
            format!(" {accent}{emoji}Z"),
        ),
    ] {
        feed(&mut decoder, &mut grid, control);
        assert_eq!(copied_content(&grid), expected);
        for (col, text) in [(accent_column, accent), (accent_column + 1, emoji)] {
            let cell = grid.buffer.cell(0, col);
            assert_eq!(cell.text(), text);
            assert_eq!(cell.fg, Color::Indexed(1));
            assert_eq!(cell.bg, Color::Indexed(4));
            assert!(cell.flags.contains(CellFlags::BOLD));
            assert_eq!(cell.underline_style, UnderlineStyle::Single);
        }
    }

    // Deleting just the wide continuation must remove its complete grapheme.
    feed(&mut decoder, &mut grid, b"\x1b[1;4H\x1b[P");
    assert_eq!(copied_content(&grid), format!(" {accent} Z"));
    // Erasing the accent cell must remove every combining scalar with its base.
    feed(&mut decoder, &mut grid, b"\x1b[1;2H\x1b[1K");
    assert_eq!(copied_content(&grid), "   Z");
}

#[test]
fn text_presentation_shrinks_a_wide_grapheme_without_copying_a_phantom_space() {
    for (input, expected) in [
        ("⌚\u{fe0e}\r\nZ", "⌚\u{fe0e}\nZ"),
        ("a⌚\u{fe0e}Z", "a⌚\u{fe0e}Z"),
    ] {
        let mut grid = Grid::new(3, 3, 32);
        let mut decoder = decoder();
        for byte in input.as_bytes() {
            feed(&mut decoder, &mut grid, std::slice::from_ref(byte));
        }
        assert_eq!(copied_content(&grid), expected);
        for (cols, rows) in [(1, 2), (9, 4), (2, 2)] {
            grid.resize(cols, rows);
            assert_eq!(copied_content(&grid), expected);
        }
    }
}

#[test]
fn hard_blank_lines_and_deliberate_trailing_spaces_survive_reflow() {
    let input = b"first  \r\n\r\nsecond \r\nEND";
    let expected = "first  \n\nsecond \nEND";
    let mut grid = Grid::new(8, 5, 32);
    let mut decoder = decoder();
    feed(&mut decoder, &mut grid, input);
    for (cols, rows) in [(3, 2), (1, 1), (20, 8), (8, 5)] {
        grid.resize(cols, rows);
        assert_eq!(copied_content(&grid), expected);
    }
}

#[test]
fn clearing_the_display_does_not_resurrect_screen_overflow_after_growing() {
    for erase in [b"\x1b[J".as_slice(), b"\x1b[2J".as_slice()] {
        let mut grid = Grid::new(16, 4, 16);
        let mut decoder = decoder();
        feed(&mut decoder, &mut grid, b"top\r\nmiddle\r\nbottom\x1b[H");
        grid.resize(16, 1);
        feed(&mut decoder, &mut grid, erase);
        feed(&mut decoder, &mut grid, b"fresh");
        grid.resize(16, 4);
        assert_eq!(copied_content(&grid), "fresh", "erase={erase:?}");
    }
}

#[test]
fn erasing_scrollback_preserves_screen_rows_hidden_by_a_height_reduction() {
    let mut grid = Grid::new(16, 4, 16);
    let mut decoder = decoder();
    feed(&mut decoder, &mut grid, b"top\r\nmiddle\r\nbottom\x1b[H");
    grid.resize(16, 1);
    feed(&mut decoder, &mut grid, b"\x1b[3J");
    grid.resize(16, 4);
    assert_eq!(copied_content(&grid), "top\nmiddle\nbottom");
}

#[test]
fn newline_reveals_retained_screen_overflow_before_creating_a_blank_row() {
    let mut grid = Grid::new(16, 4, 16);
    let mut decoder = decoder();
    feed(&mut decoder, &mut grid, b"top\r\nmiddle\r\nbottom\x1b[H");
    grid.resize(16, 1);
    feed(&mut decoder, &mut grid, b"\r\nX");
    assert_eq!(copied_content(&grid), "top\nXiddle\nbottom");
    grid.resize(16, 4);
    assert_eq!(copied_content(&grid), "top\nXiddle\nbottom");
}
