use super::{boundary_pages, CellFlags, CharSet, Color, Grid, UnderlineStyle};
use std::sync::{Arc, Weak};

fn assert_state(batched: &Grid, scalar: &Grid, context: &str) {
    // Debug includes every Grid field and Buffer's physical cells, row mapping,
    // metadata, and uniform_suffix_start, not only the visible character text.
    assert_eq!(format!("{batched:?}"), format!("{scalar:?}"), "{context}");
}

fn write_pair(batched: &mut Grid, scalar: &mut Grid, text: &str) {
    batched.put_utf8(text);
    for c in text.chars() {
        scalar.put_char(c);
    }
    assert_state(batched, scalar, text);
}

#[test]
fn utf8_spans_match_scalars_for_widths_and_alternating_runs() {
    let samples = [
        "",
        "abcdefghijklmnop",
        "éèêëλμνξοπρσ",
        "日本語漢字天地玄黄宇宙洪荒",
        "é日λ本μ語ν漢ξ字o天地p玄黄q",
        "αβγ日月火abcdefgh水木金δζη土星",
        "\u{20000}\u{20001}𝔸𝔹\u{20002}𝕏𝕐",
        "   日  é  本   ",
    ];
    // Exercise certified width-one and width-two paths as well as mixed pages.
    assert!(boundary_pages::is_other('日'));
    assert!(boundary_pages::is_other('λ'));
    for cols in [1, 2, 3, 8, 17] {
        for text in samples {
            let mut batched = Grid::new(cols, 3, 8);
            let mut scalar = Grid::new(cols, 3, 8);
            batched.clear_dirty();
            scalar.clear_dirty();
            write_pair(&mut batched, &mut scalar, text);
        }
    }
}

#[test]
fn utf8_spans_preserve_prepend_combining_and_contextual_clusters() {
    let samples = [
        "\u{600}日本語",
        "é\u{600}日本\u{600}語漢",
        "日\u{600}\u{600}abé\u{600}λμ",
        "\u{600}\u{301}日é\u{301}\u{308}本",
        "a\u{301}b\u{308}é\u{301}日本\u{301}",
        "🇧🇷🇺🇸🇯🇵🇧日🇷ab",
        "👩🏽\u{200d}💻日👨\u{200d}👩\u{200d}👧本",
        "क\u{94d}\u{200d}ष日क\u{93c}\u{94d}ष本",
        "\u{1100}\u{1161}\u{11a8}日\u{1100}\u{1161}",
        "❤\u{fe0f}日❤\u{fe0e}ab",
        "\u{301}\u{903}日\r\n\0\u{ad}\u{2028}本",
    ];
    for cols in [1, 2, 3, 8] {
        for text in samples {
            // Every scalar boundary can be a separate caller's UTF-8 chunk.
            for split in text
                .char_indices()
                .map(|(byte, _)| byte)
                .chain([text.len()])
            {
                let mut batched = Grid::new(cols, 3, 8);
                let mut scalar = Grid::new(cols, 3, 8);
                write_pair(&mut batched, &mut scalar, &text[..split]);
                write_pair(&mut batched, &mut scalar, &text[split..]);
            }
        }
    }
}

#[test]
fn utf8_spans_preserve_margins_disabled_wrap_and_resized_saved_wrap() {
    for auto_wrap in [false, true] {
        for cols in [1, 2, 3, 8] {
            for new_cols in [1, 2, 3, 8] {
                for cursor_col in 0..=cols {
                    let setup = || {
                        let mut grid = Grid::new(cols, 3, 8);
                        for c in "日本語abcdefgh天地玄黄".chars() {
                            grid.put_char(c);
                        }
                        grid.set_cursor_pos(1, 0);
                        for _ in 0..cols {
                            grid.put_char('x');
                        }
                        grid.save_cursor();
                        grid.set_auto_wrap(auto_wrap);
                        grid.resize(new_cols, 3);
                        grid.restore_cursor();
                        grid.clear_dirty();
                        grid
                    };
                    let mut batched = setup();
                    let mut scalar = setup();
                    write_pair(&mut batched, &mut scalar, "\u{301}日本abλμ天地");
                    // Also enter at ordinary columns and the legacy sentinel.
                    batched.set_cursor_pos(1, 0);
                    scalar.set_cursor_pos(1, 0);
                    batched.cursor_col = cursor_col.min(new_cols);
                    scalar.cursor_col = cursor_col.min(new_cols);
                    write_pair(&mut batched, &mut scalar, "日本λμabc天地");
                }
            }
        }
    }
}

#[test]
fn utf8_spans_fall_back_for_insert_and_dec_special_modes() {
    for charset in [CharSet::Ascii, CharSet::DecSpecial] {
        for insert_mode in [false, true] {
            for cols in [1, 2, 3, 8] {
                for col in 0..cols {
                    let setup = || {
                        let mut grid = Grid::new(cols, 3, 8);
                        for c in "日本e\u{301}語abc天地".chars() {
                            grid.put_char(c);
                        }
                        grid.set_cursor_pos(1, col);
                        grid.charset = charset;
                        grid.insert_mode = insert_mode;
                        grid.clear_dirty();
                        grid
                    };
                    let mut batched = setup();
                    let mut scalar = setup();
                    write_pair(&mut batched, &mut scalar, "lqkx日本\u{301}jmnλμ");
                }
            }
        }
    }
}

#[test]
fn utf8_spans_repair_wide_edges_preserve_styles_and_release_arcs() {
    for col in 0..8 {
        for text in ["λ", "λμνξοπ", "日本", "日本語", "λ日μ本ν"] {
            let setup = || {
                let mut grid = Grid::new(8, 3, 8);
                grid.fg = Color::Indexed(3);
                grid.bg = Color::Rgb(4, 5, 6);
                grid.flags = CellFlags::ITALIC | CellFlags::HIDDEN;
                grid.underline_style = UnderlineStyle::Curly;
                grid.underline_color = Color::Indexed(7);
                for c in "日\u{301}本\u{301}語\u{301}文\u{301}".chars() {
                    grid.put_char(c);
                }
                let arcs: Vec<Weak<str>> = (0..8)
                    .filter_map(|col| grid.buffer.cell(0, col).grapheme.as_ref())
                    .map(Arc::downgrade)
                    .collect();
                grid.set_cursor_pos(0, col);
                grid.fg = Color::Rgb(20, 30, 40);
                grid.bg = Color::Indexed(11);
                grid.flags = CellFlags::BOLD | CellFlags::REVERSE | CellFlags::WIDE_CONT;
                grid.underline_style = UnderlineStyle::Double;
                grid.underline_color = Color::Rgb(70, 80, 90);
                grid.clear_dirty();
                (grid, arcs)
            };
            let (mut batched, batched_arcs) = setup();
            let (mut scalar, scalar_arcs) = setup();
            write_pair(&mut batched, &mut scalar, text);
            for (batched_arc, scalar_arc) in batched_arcs.iter().zip(&scalar_arcs) {
                assert_eq!(batched_arc.strong_count(), scalar_arc.strong_count());
            }
            if col == 1 && text == "λμνξοπ" {
                assert!(batched_arcs.iter().all(|arc| arc.upgrade().is_none()));
                assert_eq!(batched.buffer.cell(0, 0).c, ' ');
                assert_eq!(batched.buffer.cell(0, 7).c, ' ');
            }
        }
    }
}

#[test]
fn utf8_spans_preserve_history_alternate_screen_and_regional_scrolls() {
    for cols in [2, 3, 8] {
        for history in [0, 1, 8] {
            let mut batched = Grid::new(cols, 4, history);
            let mut scalar = Grid::new(cols, 4, history);
            for _ in 0..8 {
                write_pair(&mut batched, &mut scalar, "日本αβγ天地");
                batched.newline();
                scalar.newline();
                batched.carriage_return();
                scalar.carriage_return();
            }
            for grid in [&mut batched, &mut scalar] {
                grid.save_cursor();
                grid.enter_alt_screen();
                grid.set_scroll_region(1, 2);
                grid.set_cursor_pos(1, 0);
                grid.clear_dirty();
            }
            for _ in 0..8 {
                write_pair(&mut batched, &mut scalar, "αβ日本語éé天地");
            }
            for grid in [&mut batched, &mut scalar] {
                grid.scroll_down(1);
                grid.resize(cols + 1, 3);
            }
            write_pair(&mut batched, &mut scalar, "語漢字λμν");
            for grid in [&mut batched, &mut scalar] {
                grid.leave_alt_screen();
                grid.restore_cursor();
            }
            write_pair(&mut batched, &mut scalar, "\u{301}語漢字λμν");
        }
    }
}

#[test]
fn utf8_spans_keep_the_uniform_suffix_frontier_at_the_written_end() {
    for text in ["λμ", "日本", "λ日μ", "  日本  "] {
        let setup = || {
            let mut grid = Grid::new(32, 4, 8);
            grid.fg = Color::Indexed(13);
            grid.bg = Color::Rgb(7, 8, 9);
            grid.underline_style = UnderlineStyle::Dotted;
            grid.erase_in_display(2);
            grid.clear_dirty();
            grid
        };
        let mut batched = setup();
        let mut scalar = setup();
        write_pair(&mut batched, &mut scalar, text);
        // Full-state equality above includes the frontier. Scrolling then uses
        // it to copy the retained row and clear its recycled physical storage.
        for grid in [&mut batched, &mut scalar] {
            grid.set_cursor_pos(3, 0);
            grid.newline();
        }
        assert_state(&batched, &scalar, "short row retained and recycled");
        write_pair(&mut batched, &mut scalar, "天地");
        for grid in [&mut batched, &mut scalar] {
            grid.carriage_return();
            grid.erase_in_line(0);
        }
        assert_state(&batched, &scalar, "short row cleared");
    }
}

#[test]
fn mixed_utf8_spans_preserve_text_across_staging_capacity_and_row_edges() {
    for count in [63, 64, 65, 127, 128, 129] {
        let text: String = ['a', '日', 'λ', '本']
            .into_iter()
            .cycle()
            .take(count)
            .collect();
        for cols in [63, 64, 65, 95, 96, 97, 191, 192, 193] {
            for auto_wrap in [false, true] {
                let mut batched = Grid::new(cols, 3, 8);
                let mut scalar = Grid::new(cols, 3, 8);
                batched.set_auto_wrap(auto_wrap);
                scalar.set_auto_wrap(auto_wrap);
                batched.clear_dirty();
                scalar.clear_dirty();
                write_pair(&mut batched, &mut scalar, &text);
                write_pair(&mut batched, &mut scalar, "\u{301}語abc日");
            }
        }
    }
}

#[test]
fn mixed_utf8_spans_preserve_prepend_near_staging_and_row_boundaries() {
    for prefix_len in [62, 63, 64, 65, 126, 127, 128, 129] {
        let prefix: String = ['a', '日', 'λ']
            .into_iter()
            .cycle()
            .take(prefix_len)
            .collect();
        let text = format!("{prefix}\u{600}日本a\u{600}\u{301}語b👩🏽\u{200d}💻c");
        for cols in [64, 85, 86, 128, 172, 256] {
            let mut batched = Grid::new(cols, 3, 8);
            let mut scalar = Grid::new(cols, 3, 8);
            write_pair(&mut batched, &mut scalar, &text);
            // The next call begins after an independently received Prepend.
            write_pair(&mut batched, &mut scalar, "\u{600}");
            write_pair(&mut batched, &mut scalar, &format!("日{prefix}\u{301}"));
        }
    }
}

#[test]
fn mixed_utf8_spans_repair_wide_pairs_and_release_arcs_at_staging_edges() {
    for col in [0, 1, 63, 64, 65] {
        let setup = || {
            let mut grid = Grid::new(200, 2, 8);
            grid.fg = Color::Indexed(3);
            grid.flags = CellFlags::ITALIC;
            for _ in 0..100 {
                grid.put_char('日');
                grid.put_char('\u{301}');
            }
            let arcs: Vec<Weak<str>> = (0..200)
                .filter_map(|col| grid.buffer.cell(0, col).grapheme.as_ref())
                .map(Arc::downgrade)
                .collect();
            grid.set_cursor_pos(0, col);
            grid.fg = Color::Rgb(4, 5, 6);
            grid.bg = Color::Indexed(7);
            grid.flags = CellFlags::BOLD | CellFlags::UNDERLINE;
            grid.underline_style = UnderlineStyle::Curly;
            grid.underline_color = Color::Rgb(8, 9, 10);
            grid.clear_dirty();
            (grid, arcs)
        };
        let (mut batched, batched_arcs) = setup();
        let (mut scalar, scalar_arcs) = setup();
        let text: String = ['日', 'a', 'b', '本', 'λ']
            .into_iter()
            .cycle()
            .take(129)
            .collect();
        write_pair(&mut batched, &mut scalar, &text);
        for (batched_arc, scalar_arc) in batched_arcs.iter().zip(&scalar_arcs) {
            assert_eq!(batched_arc.strong_count(), scalar_arc.strong_count());
        }
    }
}
