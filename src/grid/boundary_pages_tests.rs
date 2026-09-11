use super::*;
use unicode_segmentation::UnicodeSegmentation;

include!("boundary_pages_reference.rs");
include!("boundary_pages_corpus.rs");

fn reference_category(c: char) -> OracleCategory {
    use std::cmp::Ordering;
    match REFERENCE_RANGES.binary_search_by(|&(lo, hi, _)| {
        if hi < c {
            Ordering::Less
        } else if lo > c {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }) {
        Ok(index) => REFERENCE_RANGES[index].2,
        Err(_) => GC_Any,
    }
}

#[test]
fn every_classified_scalar_and_predicate_matches_the_original_table() {
    assert_eq!(
        unicode_segmentation::UNICODE_VERSION,
        GENERATED_UNICODE_VERSION
    );
    let mut checked = 0;
    for cp in 0..=0x10ffff {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let category = reference_category(c);
        match successor_class(c) {
            SuccessorClass::Other => assert_eq!(category, GC_Any, "U+{cp:04X}"),
            SuccessorClass::Attach => assert!(
                matches!(category, GC_Extend | GC_ZWJ | GC_SpacingMark),
                "U+{cp:04X}: {category:?}"
            ),
            SuccessorClass::Unknown => (),
        }
        assert_eq!(is_prepend(c), category == GC_Prepend, "U+{cp:04X}");
        assert_eq!(
            is_control_cr_lf(c),
            matches!(category, GC_Control | GC_CR | GC_LF),
            "U+{cp:04X}"
        );
        checked += 1;
    }
    assert_eq!(checked, 1_112_064);
}

fn check_stream(s: &str, expected: &[&str]) {
    let mut clusters: Vec<String> = Vec::new();
    for c in s.chars() {
        if let Some(previous) = clusters.last_mut() {
            if super::super::scalar_extends_grapheme(previous, c) {
                previous.push(c);
                continue;
            }
        }
        clusters.push(c.to_string());
    }
    let observed: Vec<&str> = clusters.iter().map(String::as_str).collect();
    assert_eq!(observed, expected, "input={s:?}");
}

#[test]
fn official_unicode_17_grapheme_break_corpus_matches_streaming() {
    for &(s, expected) in TEST_SAME {
        check_stream(s, expected);
    }
    for &(s, extended, _) in TEST_DIFF {
        check_stream(s, extended);
    }
}

#[test]
fn fast_decisions_match_concatenation_at_property_endpoints_with_context() {
    // Contexts deliberately distinguish GB9b from control precedence, GB9c,
    // GB11, RI parity, and a Prepend whose final scalar is an Extend.
    let contexts = [
        "a",
        "é",
        "日",
        "\r",
        "\n",
        "\0",
        "\u{ad}",
        "\u{2028}",
        "\u{600}",
        "\u{600}\u{600}",
        "\u{600}\u{301}",
        "\u{113d1}",
        "e\u{301}\u{308}",
        "\u{301}",
        "\u{903}",
        "🇧",
        "🇧🇷",
        "\u{600}🇧🇷",
        "👩",
        "👩🏽\u{200d}",
        "a\u{200d}",
        "👩\u{200d}\u{301}",
        "क\u{94d}",
        "क\u{94d}\u{200d}",
        "क\u{93c}",
        "क\u{93c}\u{94d}\u{200d}ष\u{94d}",
        "\u{1100}",
        "\u{1100}\u{1161}",
        "\u{1100}\u{1161}\u{11a8}",
        "🏴\u{e0067}\u{e0062}\u{e007f}",
        "❤\u{fe0f}",
    ];
    let mut next_scalars = vec![' ', 'a', 'é', 'λ', '日', '𝔸', '𠀀', '\u{301}', '\u{308}'];
    for &(lo, hi, _) in REFERENCE_RANGES {
        next_scalars.extend([lo, hi]);
        for cp in [lo as u32 - (lo as u32 > 0) as u32, hi as u32 + 1] {
            if let Some(c) = char::from_u32(cp) {
                next_scalars.push(c);
            }
        }
    }
    next_scalars.sort_unstable();
    next_scalars.dedup();
    for previous in contexts {
        assert_eq!(
            previous.graphemes(true).count(),
            1,
            "bad test context: {previous:?}"
        );
        let last = previous.chars().next_back().unwrap();
        for &next in &next_scalars {
            let combined = format!("{previous}{next}");
            let expected = combined.graphemes(true).count() == 1;
            if let Some(fast) = scalar_extends(last, next) {
                assert_eq!(fast, expected, "previous={previous:?}, next={next:?}");
            }
            assert_eq!(
                super::super::scalar_extends_grapheme(previous, next),
                expected,
                "previous={previous:?}, next={next:?}"
            );
        }
    }
}

#[test]
fn mixed_pages_and_contextual_successors_keep_the_fallback() {
    for next in [
        '\r', '\n', '\u{600}', 'क', 'ष', '👩', '🇧', '\u{1100}', '\u{1161}', '\u{11a8}', '\u{200d}',
    ] {
        assert_eq!(scalar_extends('a', next), None, "next={next:?}");
    }
    assert_eq!(scalar_extends('\u{600}', '日'), Some(true));
    assert_eq!(scalar_extends('\u{301}', '日'), Some(false));
    assert_eq!(scalar_extends('\n', '\u{301}'), Some(false));
    assert_eq!(scalar_extends('日', '\u{301}'), Some(true));
}

#[test]
fn long_cluster_contexts_match_the_full_segmenter() {
    let accent_run = format!("e{}", "\u{301}".repeat(512));
    let prepend_run = "\u{600}".repeat(128);
    let emoji_run = format!("👩{}\u{200d}", "\u{301}".repeat(128));
    let indic_run = format!(
        "क{}\u{94d}{}",
        "\u{93c}".repeat(128),
        "\u{200d}".repeat(128)
    );
    for previous in [&accent_run, &prepend_run, &emoji_run, &indic_run] {
        assert_eq!(previous.graphemes(true).count(), 1, "bad long context");
        for next in ['a', 'é', '日', '\u{301}', '\u{200d}', '💻', 'ष', '🇧', '\n'] {
            let combined = format!("{previous}{next}");
            assert_eq!(
                super::super::scalar_extends_grapheme(previous, next),
                combined.graphemes(true).count() == 1,
                "prefix length={}, next={next:?}",
                previous.len()
            );
        }
    }
}
