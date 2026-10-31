//! Geometric rendering for Unicode block elements (U+2580–U+259F).
//!
//! Pixel art and fastfetch logos are drawn with half blocks, eighth blocks and
//! quadrants. Rasterizing those through the font leaves seams between cells
//! and anti-aliased edges, so renderers draw them as exact rectangles that
//! share edges with the neighbouring cells instead.

/// A rectangle expressed as fractions of the cell: `[x0, y0, x1, y1]` in 0..=1,
/// with the origin at the top-left corner of the cell.
pub type CellRect = [f32; 4];

/// Rectangles to fill with the foreground color for a block element, plus the
/// alpha (0..=255) to apply to that foreground. Shade characters (░▒▓) cover the
/// whole cell at partial alpha. Returns `None` for any other character.
pub fn block_element_rects(c: char) -> Option<(Vec<CellRect>, u8)> {
    const FULL: CellRect = [0.0, 0.0, 1.0, 1.0];
    let eighth = |n: u32| n as f32 / 8.0;
    let rects = match c {
        '\u{2580}' => vec![[0.0, 0.0, 1.0, 0.5]], // ▀ upper half
        '\u{2581}'..='\u{2588}' => {
            // ▁▂▃▄▅▆▇█ lower n/8 blocks
            let n = c as u32 - 0x2580;
            vec![[0.0, 1.0 - eighth(n), 1.0, 1.0]]
        }
        '\u{2589}'..='\u{258F}' => {
            // ▉▊▋▌▍▎▏ left n/8 blocks, 7/8 down to 1/8
            let n = 0x2590 - c as u32;
            vec![[0.0, 0.0, eighth(n), 1.0]]
        }
        '\u{2590}' => vec![[0.5, 0.0, 1.0, 1.0]], // ▐ right half
        '\u{2591}' => return Some((vec![FULL], 64)), // ░ light shade
        '\u{2592}' => return Some((vec![FULL], 128)), // ▒ medium shade
        '\u{2593}' => return Some((vec![FULL], 192)), // ▓ dark shade
        '\u{2594}' => vec![[0.0, 0.0, 1.0, eighth(1)]], // ▔ upper one eighth
        '\u{2595}' => vec![[1.0 - eighth(1), 0.0, 1.0, 1.0]], // ▕ right one eighth
        '\u{2596}'..='\u{259F}' => quadrant_rects(c),
        _ => return None,
    };
    Some((rects, 255))
}

/// Quadrant blocks U+2596–U+259F as combinations of the four cell quarters.
fn quadrant_rects(c: char) -> Vec<CellRect> {
    const UL: CellRect = [0.0, 0.0, 0.5, 0.5];
    const UR: CellRect = [0.5, 0.0, 1.0, 0.5];
    const LL: CellRect = [0.0, 0.5, 0.5, 1.0];
    const LR: CellRect = [0.5, 0.5, 1.0, 1.0];
    match c {
        '\u{2596}' => vec![LL],         // ▖
        '\u{2597}' => vec![LR],         // ▗
        '\u{2598}' => vec![UL],         // ▘
        '\u{2599}' => vec![UL, LL, LR], // ▙
        '\u{259A}' => vec![UL, LR],     // ▚
        '\u{259B}' => vec![UL, UR, LL], // ▛
        '\u{259C}' => vec![UL, UR, LR], // ▜
        '\u{259D}' => vec![UR],         // ▝
        '\u{259E}' => vec![UR, LL],     // ▞
        '\u{259F}' => vec![UR, LL, LR], // ▟
        _ => Vec::new(),
    }
}

/// True for characters handled by [`block_element_rects`].
pub fn is_block_element(c: char) -> bool {
    ('\u{2580}'..='\u{259F}').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(rects: &[CellRect]) -> f32 {
        rects.iter().map(|r| (r[2] - r[0]) * (r[3] - r[1])).sum()
    }

    #[test]
    fn half_blocks_share_the_cell_midline() {
        let (upper, _) = block_element_rects('▀').unwrap();
        let (lower, _) = block_element_rects('▄').unwrap();
        assert_eq!(upper, vec![[0.0, 0.0, 1.0, 0.5]]);
        assert_eq!(lower, vec![[0.0, 0.5, 1.0, 1.0]]);
        let (left, _) = block_element_rects('▌').unwrap();
        let (right, _) = block_element_rects('▐').unwrap();
        assert_eq!(left, vec![[0.0, 0.0, 0.5, 1.0]]);
        assert_eq!(right, vec![[0.5, 0.0, 1.0, 1.0]]);
    }

    #[test]
    fn eighth_blocks_grow_from_the_bottom_and_the_left() {
        for n in 1..=8u32 {
            let c = char::from_u32(0x2580 + n).unwrap();
            let (rects, alpha) = block_element_rects(c).unwrap();
            assert_eq!(alpha, 255);
            assert!((area(&rects) - n as f32 / 8.0).abs() < 1e-6, "{c}");
            assert_eq!(rects[0][3], 1.0, "{c} touches the bottom edge");
        }
        for (c, n) in [
            ('▉', 7),
            ('▊', 6),
            ('▋', 5),
            ('▌', 4),
            ('▍', 3),
            ('▎', 2),
            ('▏', 1),
        ] {
            let (rects, _) = block_element_rects(c).unwrap();
            assert!((area(&rects) - n as f32 / 8.0).abs() < 1e-6, "{c}");
            assert_eq!(rects[0][0], 0.0, "{c} touches the left edge");
        }
    }

    #[test]
    fn full_block_covers_the_whole_cell_opaquely() {
        let (rects, alpha) = block_element_rects('█').unwrap();
        assert_eq!(rects, vec![[0.0, 0.0, 1.0, 1.0]]);
        assert_eq!(alpha, 255);
    }

    #[test]
    fn shades_cover_the_cell_with_increasing_alpha() {
        let alphas: Vec<u8> = ['░', '▒', '▓']
            .iter()
            .map(|c| {
                let (rects, alpha) = block_element_rects(*c).unwrap();
                assert_eq!(rects, vec![[0.0, 0.0, 1.0, 1.0]]);
                alpha
            })
            .collect();
        assert!(alphas[0] < alphas[1] && alphas[1] < alphas[2]);
    }

    #[test]
    fn quadrants_cover_the_expected_number_of_quarters() {
        let expected = [
            ('▖', 1),
            ('▗', 1),
            ('▘', 1),
            ('▙', 3),
            ('▚', 2),
            ('▛', 3),
            ('▜', 3),
            ('▝', 1),
            ('▞', 2),
            ('▟', 3),
        ];
        for (c, quarters) in expected {
            let (rects, _) = block_element_rects(c).unwrap();
            assert_eq!(rects.len(), quarters, "{c}");
            assert!((area(&rects) - quarters as f32 / 4.0).abs() < 1e-6, "{c}");
        }
    }

    #[test]
    fn ignores_characters_outside_the_block() {
        assert!(block_element_rects('─').is_none());
        assert!(block_element_rects('a').is_none());
        assert!(block_element_rects('\u{25A0}').is_none());
        assert!(is_block_element('▀'));
        assert!(!is_block_element('\u{25A0}'));
    }
}
