//! Shared warmed glyph lookup fixture for Linux and macOS.
//!
//! Copy into `src/glyph_atlas/lookup_benchmark.rs` and append
//! `#[cfg(test)] mod lookup_benchmark;` to each archived glyph_atlas.rs.
//! Run the same fixture with both revisions in release mode. This measures
//! cached lookup CPU time, not font rasterization, Metal, presentation or input.

use super::{GlyphAtlas, GlyphEntry, GlyphKey};
use crate::grid::cell::{Cell, CellFlags};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

const LOOKUPS_PER_ITERATION: usize = 384;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn setting(name: &str, default: usize, maximum: usize) -> usize {
    let value = std::env::var(name).map_or(default, |value| {
        value
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a positive integer"))
    });
    assert!(
        (1..=maximum).contains(&value),
        "{name} must be between 1 and {maximum}"
    );
    value
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn entry_fingerprint(atlas: &GlyphAtlas, entry: GlyphEntry) -> u64 {
    let mut hash = FNV_OFFSET;
    for value in [entry.atlas_x, entry.atlas_y, entry.pixel_w, entry.pixel_h] {
        hash_bytes(&mut hash, &value.to_le_bytes());
    }
    hash_bytes(&mut hash, &entry.bearing_x.to_le_bytes());
    hash_bytes(&mut hash, &entry.bearing_y.to_le_bytes());
    hash_bytes(&mut hash, &[u8::from(atlas.is_color(entry))]);
    hash
}

fn cells(workload: &str) -> Vec<Cell> {
    const MIXED: [char; 16] = [
        'A', 'z', '3', '!', ' ', '-', 'm', 'Q', 'é', 'λ', 'Ω', '界', '語', 'ñ', 'Ж', '✓',
    ];
    const GRAPHEMES: [&str; 8] = [
        "e\u{301}",
        "o\u{308}",
        "q\u{302}\u{307}",
        "α\u{301}",
        "👩🏽\u{200d}💻",
        "👨🏻\u{200d}💻",
        "🇧🇷",
        "1\u{fe0f}\u{20e3}",
    ];
    let mut cells: Vec<_> = (0..LOOKUPS_PER_ITERATION)
        .map(|index| {
            let (c, grapheme, style) = match workload {
                "ascii-regular" => (char::from(b' ' + (index % 95) as u8), None, 0),
                "ascii-four-styles" => {
                    (char::from(b' ' + (index % 95) as u8), None, index / 95 % 4)
                }
                "mixed-unicode" => (MIXED[index % MIXED.len()], None, index / MIXED.len() % 4),
                "graphemes" => {
                    let text = GRAPHEMES[index % GRAPHEMES.len()];
                    (
                        text.chars().next().unwrap(),
                        Some(Arc::from(text)),
                        index / GRAPHEMES.len() % 4,
                    )
                }
                _ => panic!("unknown workload: {workload}"),
            };
            let mut flags = CellFlags::empty();
            flags.set(CellFlags::BOLD, style & 1 != 0);
            flags.set(CellFlags::ITALIC, style & 2 != 0);
            Cell {
                c,
                grapheme,
                flags,
                ..Cell::default()
            }
        })
        .collect();
    // Fixed u32 state gives both architectures and revisions the same mixed
    // access order without random setup or allocations in the measured loop.
    let mut state = 0x6b6f_6b75u32;
    for last in (1..cells.len()).rev() {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        cells.swap(last, state as usize % (last + 1));
    }
    cells
}

fn fingerprint(atlas: &mut GlyphAtlas, cells: &[Cell]) -> u64 {
    let mut hash = FNV_OFFSET;
    for cell in cells {
        hash_bytes(&mut hash, &(cell.c as u32).to_le_bytes());
        hash_bytes(&mut hash, &[cell.flags.bits()]);
        let text = cell.grapheme.as_deref().unwrap_or("");
        hash_bytes(&mut hash, &(text.len() as u64).to_le_bytes());
        hash_bytes(&mut hash, text.as_bytes());
        let entry = atlas.get_or_insert_cell(cell);
        hash_bytes(&mut hash, &entry_fingerprint(atlas, entry).to_le_bytes());
    }
    hash
}

fn pixel_fingerprint(atlas: &GlyphAtlas) -> u64 {
    let mut hash = FNV_OFFSET;
    hash_bytes(&mut hash, &atlas.pixels);
    hash_bytes(&mut hash, &atlas.rgba_pixels);
    hash
}

fn cache_counts(atlas: &GlyphAtlas) -> (usize, usize) {
    (
        atlas.glyphs.len(),
        atlas.text_glyphs.iter().map(|cache| cache.len()).sum(),
    )
}

fn lookups(atlas: &mut GlyphAtlas, cells: &[Cell], iterations: usize) {
    for _ in 0..iterations {
        for cell in cells {
            black_box(atlas.get_or_insert_cell(black_box(cell)));
        }
    }
}

#[test]
#[ignore = "CPU glyph lookup benchmark; run release on an idle host with --nocapture"]
fn benchmark_warmed_lookups() {
    let samples = setting("KOKUBAN_GLYPH_LOOKUP_SAMPLES", 5, 100);
    let iterations = setting("KOKUBAN_GLYPH_LOOKUP_ITERATIONS", 1000, 100_000);
    let warmup = setting("KOKUBAN_GLYPH_LOOKUP_WARMUP", 100, 100_000);
    for workload in [
        "ascii-regular",
        "ascii-four-styles",
        "mixed-unicode",
        "graphemes",
    ] {
        let cells = cells(workload);
        let mut atlas = GlyphAtlas::new("monospace", 14.0, 1.0).unwrap();
        // Populate every style and fallback before timing. Scalar and cell
        // lookups must return identical entries from the shared cache.
        for cell in &cells {
            let entry = atlas.get_or_insert_cell(cell);
            if cell.grapheme.is_none() {
                let scalar = atlas.get_or_insert(GlyphKey {
                    c: cell.c,
                    bold: cell.flags.contains(CellFlags::BOLD),
                    italic: cell.flags.contains(CellFlags::ITALIC),
                });
                assert_eq!(
                    entry_fingerprint(&atlas, entry),
                    entry_fingerprint(&atlas, scalar)
                );
            }
            if cell.c != ' ' {
                assert!(
                    entry.pixel_w > 0 && entry.pixel_h > 0,
                    "fixture has missing ink: {workload} {cell:?}"
                );
            }
        }
        let expected_fingerprint = fingerprint(&mut atlas, &cells);
        let expected_pixels = pixel_fingerprint(&atlas);
        let expected_counts = cache_counts(&atlas);
        let font_name_hex: String = atlas
            .font
            .full_name()
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        atlas.dirty = false;
        println!(
            "glyph-cache fixture workload={workload} lookups_per_iteration={} iterations={iterations} \
             warmup={warmup} samples={samples} atlas_width={} atlas_height={} scalar_glyphs={} \
             grapheme_glyphs={} fingerprint={expected_fingerprint:016x} pixels_fingerprint={expected_pixels:016x} \
             font_name_hex={font_name_hex} atlas_bytes={} scalar_cache_bytes={}",
            cells.len(), atlas.width, atlas.height, expected_counts.0, expected_counts.1,
            std::mem::size_of_val(&atlas), std::mem::size_of_val(&atlas.glyphs),
        );
        for sample in 0..samples {
            lookups(&mut atlas, &cells, warmup);
            assert!(!atlas.dirty, "warmup rasterized a cached glyph");
            assert_eq!(cache_counts(&atlas), expected_counts);
            let start = Instant::now();
            lookups(&mut atlas, &cells, iterations);
            let elapsed_ns = start.elapsed().as_nanos();
            // Validate outside the timer; neither entry nor pixels may change.
            assert!(!atlas.dirty, "timed lookup rasterized a cached glyph");
            assert_eq!(cache_counts(&atlas), expected_counts);
            assert_eq!(fingerprint(&mut atlas, &cells), expected_fingerprint);
            assert_eq!(pixel_fingerprint(&atlas), expected_pixels);
            assert!(!atlas.dirty);
            println!("glyph-cache sample workload={workload} sample={sample} lookups={} elapsed_ns={elapsed_ns}",
                cells.len() * iterations);
        }
    }
}
