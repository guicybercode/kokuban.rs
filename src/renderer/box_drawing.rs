//! Built-in rendering of block elements (U+2580-U+259F), box drawing
//! characters (U+2500-U+257F) and the rectangular Symbols for Legacy
//! Computing: sextants (U+1FB00-U+1FB3B) and the one-eighth bars and blocks
//! of U+1FB70-U+1FB8B.
//!
//! Font glyphs for these characters rarely cover the cell exactly, so their
//! antialiased edges blend with neighbouring cells: pixel art shows a faint
//! grid and `─` or `│` look dashed. Instead, each character becomes a list of
//! integer rectangles relative to the cell's top-left pixel, which renderers
//! fill with the foreground colour without any texture sampling.
//!
//! Layout rules, shared by both renderers:
//!
//! * Block boundaries sit at `floor(n * k / 8)` pixels. Halves therefore split
//!   at `floor(n / 2)`: in a 17px cell `▀` covers rows 0..8 and `▄` covers
//!   rows 8..17, so on odd sizes the lower (or right) half gets the extra row.
//!   Sextant rows split at `floor(n * k / 3)` and their columns at the same
//!   `floor(n / 2)` as the half blocks, so `▌` and a sextant's left column
//!   line up. Complementary blocks tile the cell with no gaps or overlaps.
//! * Light strokes are `max(1, round(scale))` pixels thick and heavy strokes
//!   three times that. A double line is two light strokes separated by a
//!   light-stroke gap, so it spans exactly the width of a heavy stroke.
//! * A stroke of thickness `t` starts at `(n - t) / 2` in every cell, so lines
//!   meet at the same row or column across cells, and arms of a corner, tee
//!   or cross meet on the same centre line.

/// A solid rectangle in cell-local pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Foreground coverage: 255 for solid ink, lower for the shade blocks.
    pub alpha: u8,
}

/// Rectangles that draw `c` in a `cell_width` x `cell_height` pixel cell, or
/// `None` when `c` is not drawn by this module and should use the font.
/// `scale` is the display scale factor used to size line strokes.
pub fn builtin_rects(
    c: char,
    cell_width: u32,
    cell_height: u32,
    scale: f32,
) -> Option<Vec<CellRect>> {
    let mut canvas = Canvas {
        width: cell_width,
        height: cell_height,
        rects: Vec::new(),
    };
    match c {
        '\u{2580}'..='\u{259F}' => block_element(c, &mut canvas),
        '\u{2500}'..='\u{257F}' => box_drawing(c, &mut canvas, light_thickness(scale)),
        '\u{1FB00}'..='\u{1FB3B}' | '\u{1FB70}'..='\u{1FB8B}' => legacy_computing(c, &mut canvas),
        _ => return None,
    }
    Some(canvas.rects)
}

/// Light stroke thickness in pixels for a display scale factor.
pub fn light_thickness(scale: f32) -> u32 {
    if scale.is_finite() && scale > 0.0 {
        // `as` saturates, so absurd scales cannot overflow.
        (scale.round() as u32).max(1)
    } else {
        1
    }
}

struct Canvas {
    width: u32,
    height: u32,
    rects: Vec<CellRect>,
}

impl Canvas {
    /// Fill `x0..x1` x `y0..y1`, clamped to the cell.
    fn fill(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        self.fill_alpha(x0, y0, x1, y1, u8::MAX);
    }

    fn fill_alpha(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, alpha: u8) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        if x0 < x1 && y0 < y1 {
            self.rects.push(CellRect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
                alpha,
            });
        }
    }
}

/// Boundary `k` eighths of the way along `n` pixels.
fn eighth(n: u32, k: u32) -> u32 {
    (u64::from(n) * u64::from(k) / 8) as u32
}

fn block_element(c: char, canvas: &mut Canvas) {
    let (w, h) = (canvas.width, canvas.height);
    let code = c as u32;
    match c {
        // Upper half and upper one eighth.
        '\u{2580}' => canvas.fill(0, 0, w, eighth(h, 4)),
        '\u{2594}' => canvas.fill(0, 0, w, eighth(h, 1)),
        // Lower one eighth (U+2581) through lower seven eighths (U+2587);
        // U+2584 is the lower half.
        '\u{2581}'..='\u{2587}' => canvas.fill(0, eighth(h, 8 - (code - 0x2580)), w, h),
        '\u{2588}' => canvas.fill(0, 0, w, h),
        // Left seven eighths (U+2589) through left one eighth (U+258F);
        // U+258C is the left half.
        '\u{2589}'..='\u{258F}' => canvas.fill(0, 0, eighth(w, 0x2590 - code), h),
        // Right half and right one eighth.
        '\u{2590}' => canvas.fill(eighth(w, 4), 0, w, h),
        '\u{2595}' => canvas.fill(eighth(w, 7), 0, w, h),
        // Light, medium and dark shade as uniform 25%, 50% and 75% coverage.
        '\u{2591}'..='\u{2593}' => {
            let alpha = [64, 128, 191][(code - 0x2591) as usize];
            canvas.fill_alpha(0, 0, w, h, alpha);
        }
        '\u{2596}'..='\u{259F}' => {
            const UPPER_LEFT: u8 = 1;
            const UPPER_RIGHT: u8 = 2;
            const LOWER_LEFT: u8 = 4;
            const LOWER_RIGHT: u8 = 8;
            const QUADRANTS: [u8; 10] = [
                LOWER_LEFT,                             // ▖
                LOWER_RIGHT,                            // ▗
                UPPER_LEFT,                             // ▘
                UPPER_LEFT | LOWER_LEFT | LOWER_RIGHT,  // ▙
                UPPER_LEFT | LOWER_RIGHT,               // ▚
                UPPER_LEFT | UPPER_RIGHT | LOWER_LEFT,  // ▛
                UPPER_LEFT | UPPER_RIGHT | LOWER_RIGHT, // ▜
                UPPER_RIGHT,                            // ▝
                UPPER_RIGHT | LOWER_LEFT,               // ▞
                UPPER_RIGHT | LOWER_LEFT | LOWER_RIGHT, // ▟
            ];
            let quadrants = QUADRANTS[(code - 0x2596) as usize];
            let (mid_x, mid_y) = (eighth(w, 4), eighth(h, 4));
            for (bit, x0, y0, x1, y1) in [
                (UPPER_LEFT, 0, 0, mid_x, mid_y),
                (UPPER_RIGHT, mid_x, 0, w, mid_y),
                (LOWER_LEFT, 0, mid_y, mid_x, h),
                (LOWER_RIGHT, mid_x, mid_y, w, h),
            ] {
                if quadrants & bit != 0 {
                    canvas.fill(x0, y0, x1, y1);
                }
            }
        }
        _ => {}
    }
}

/// Boundary `k` thirds of the way along `n` pixels.
fn third(n: u32, k: u32) -> u32 {
    (u64::from(n) * u64::from(k) / 3) as u32
}

/// The 2x3 cells of a sextant U+1FB00-U+1FB3B as a bit set: bit 0 is the
/// top-left cell, bit 1 the top-right, then row by row down to bit 5 for the
/// bottom-right. The block lists the combinations in increasing order of
/// this value, from 1 to 62, skipping the two that already exist as the left
/// (`▌`, 0b010101) and right (`▐`, 0b101010) half blocks.
fn sextant_pattern(c: char) -> u8 {
    let mut pattern = (c as u32 - 0x1FB00 + 1) as u8;
    if pattern >= 0b010101 {
        pattern += 1;
    }
    if pattern >= 0b101010 {
        pattern += 1;
    }
    pattern
}

/// Eighths covered by the upper (U+1FB82-U+1FB86) and right (U+1FB87-U+1FB8B)
/// one quarter, three eighths, five eighths, three quarters and seven eighths
/// blocks; the remaining fractions live in the block elements.
const LARGE_EIGHTHS: [u32; 5] = [2, 3, 5, 6, 7];

fn legacy_computing(c: char, canvas: &mut Canvas) {
    let (w, h) = (canvas.width, canvas.height);
    let code = c as u32;
    match c {
        '\u{1FB00}'..='\u{1FB3B}' => {
            let pattern = sextant_pattern(c);
            let mid_x = eighth(w, 4);
            for row in 0..3 {
                let (y0, y1) = (third(h, row), third(h, row + 1));
                let left = pattern & (1 << (2 * row)) != 0;
                let right = pattern & (1 << (2 * row + 1)) != 0;
                match (left, right) {
                    (true, true) => canvas.fill(0, y0, w, y1),
                    (true, false) => canvas.fill(0, y0, mid_x, y1),
                    (false, true) => canvas.fill(mid_x, y0, w, y1),
                    (false, false) => {}
                }
            }
        }
        // Vertical one eighth block-2 through block-7: the k-th eighth column
        // (block-1 and block-8 are ▏ and ▕).
        '\u{1FB70}'..='\u{1FB75}' => {
            let k = code - 0x1FB70 + 2;
            canvas.fill(eighth(w, k - 1), 0, eighth(w, k), h);
        }
        // Horizontal one eighth block-2 through block-7: the k-th eighth row
        // (block-1 and block-8 are ▔ and ▁).
        '\u{1FB76}'..='\u{1FB7B}' => {
            let k = code - 0x1FB76 + 2;
            canvas.fill(0, eighth(h, k - 1), w, eighth(h, k));
        }
        // Left and lower, left and upper, right and upper, right and lower one
        // eighth blocks: an L of a full-height column and the rest of a row.
        '\u{1FB7C}'..='\u{1FB7F}' => {
            let left = matches!(c, '\u{1FB7C}' | '\u{1FB7D}');
            let upper = matches!(c, '\u{1FB7D}' | '\u{1FB7E}');
            let (x0, x1) = if left {
                (0, eighth(w, 1))
            } else {
                (eighth(w, 7), w)
            };
            let (y0, y1) = if upper {
                (0, eighth(h, 1))
            } else {
                (eighth(h, 7), h)
            };
            canvas.fill(x0, 0, x1, h);
            if left {
                canvas.fill(x1, y0, w, y1);
            } else {
                canvas.fill(0, y0, x0, y1);
            }
        }
        // Upper and lower one eighth block.
        '\u{1FB80}' => {
            canvas.fill(0, 0, w, eighth(h, 1));
            canvas.fill(0, eighth(h, 7), w, h);
        }
        // Horizontal one eighth block-1358.
        '\u{1FB81}' => {
            for k in [1, 3, 5, 8] {
                canvas.fill(0, eighth(h, k - 1), w, eighth(h, k));
            }
        }
        '\u{1FB82}'..='\u{1FB86}' => {
            canvas.fill(0, 0, w, eighth(h, LARGE_EIGHTHS[(code - 0x1FB82) as usize]));
        }
        '\u{1FB87}'..='\u{1FB8B}' => {
            canvas.fill(
                eighth(w, 8 - LARGE_EIGHTHS[(code - 0x1FB87) as usize]),
                0,
                w,
                h,
            );
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Weight {
    None,
    Light,
    Heavy,
    Double,
}

/// The four arms of a line character, each reaching from the cell centre to
/// one edge.
#[derive(Debug, Clone, Copy)]
struct Arms {
    left: Weight,
    right: Weight,
    up: Weight,
    down: Weight,
}

const fn arms(left: Weight, right: Weight, up: Weight, down: Weight) -> Arms {
    Arms {
        left,
        right,
        up,
        down,
    }
}

fn box_drawing(c: char, canvas: &mut Canvas, light: u32) {
    use Weight::{Double as D, Heavy as H, Light as L, None as N};

    let shape = match c {
        '─' => arms(L, L, N, N),
        '━' => arms(H, H, N, N),
        '│' => arms(N, N, L, L),
        '┃' => arms(N, N, H, H),
        '┄' | '┈' | '╌' | '┅' | '┉' | '╍' => {
            let weight = if matches!(c, '┄' | '┈' | '╌') {
                L
            } else {
                H
            };
            let count = match c {
                '┄' | '┅' => 3,
                '┈' | '┉' => 4,
                _ => 2,
            };
            return dashes(
                canvas,
                true,
                count,
                stroke_span(canvas.height, weight, light),
            );
        }
        '┆' | '┊' | '╎' | '┇' | '┋' | '╏' => {
            let weight = if matches!(c, '┆' | '┊' | '╎') {
                L
            } else {
                H
            };
            let count = match c {
                '┆' | '┇' => 3,
                '┊' | '┋' => 4,
                _ => 2,
            };
            return dashes(
                canvas,
                false,
                count,
                stroke_span(canvas.width, weight, light),
            );
        }
        '┌' => arms(N, L, N, L),
        '┍' => arms(N, H, N, L),
        '┎' => arms(N, L, N, H),
        '┏' => arms(N, H, N, H),
        '┐' => arms(L, N, N, L),
        '┑' => arms(H, N, N, L),
        '┒' => arms(L, N, N, H),
        '┓' => arms(H, N, N, H),
        '└' => arms(N, L, L, N),
        '┕' => arms(N, H, L, N),
        '┖' => arms(N, L, H, N),
        '┗' => arms(N, H, H, N),
        '┘' => arms(L, N, L, N),
        '┙' => arms(H, N, L, N),
        '┚' => arms(L, N, H, N),
        '┛' => arms(H, N, H, N),
        '├' => arms(N, L, L, L),
        '┝' => arms(N, H, L, L),
        '┞' => arms(N, L, H, L),
        '┟' => arms(N, L, L, H),
        '┠' => arms(N, L, H, H),
        '┡' => arms(N, H, H, L),
        '┢' => arms(N, H, L, H),
        '┣' => arms(N, H, H, H),
        '┤' => arms(L, N, L, L),
        '┥' => arms(H, N, L, L),
        '┦' => arms(L, N, H, L),
        '┧' => arms(L, N, L, H),
        '┨' => arms(L, N, H, H),
        '┩' => arms(H, N, H, L),
        '┪' => arms(H, N, L, H),
        '┫' => arms(H, N, H, H),
        '┬' => arms(L, L, N, L),
        '┭' => arms(H, L, N, L),
        '┮' => arms(L, H, N, L),
        '┯' => arms(H, H, N, L),
        '┰' => arms(L, L, N, H),
        '┱' => arms(H, L, N, H),
        '┲' => arms(L, H, N, H),
        '┳' => arms(H, H, N, H),
        '┴' => arms(L, L, L, N),
        '┵' => arms(H, L, L, N),
        '┶' => arms(L, H, L, N),
        '┷' => arms(H, H, L, N),
        '┸' => arms(L, L, H, N),
        '┹' => arms(H, L, H, N),
        '┺' => arms(L, H, H, N),
        '┻' => arms(H, H, H, N),
        '┼' => arms(L, L, L, L),
        '┽' => arms(H, L, L, L),
        '┾' => arms(L, H, L, L),
        '┿' => arms(H, H, L, L),
        '╀' => arms(L, L, H, L),
        '╁' => arms(L, L, L, H),
        '╂' => arms(L, L, H, H),
        '╃' => arms(H, L, H, L),
        '╄' => arms(L, H, H, L),
        '╅' => arms(H, L, L, H),
        '╆' => arms(L, H, L, H),
        '╇' => arms(H, H, H, L),
        '╈' => arms(H, H, L, H),
        '╉' => arms(H, L, H, H),
        '╊' => arms(L, H, H, H),
        '╋' => arms(H, H, H, H),
        '═' => arms(D, D, N, N),
        '║' => arms(N, N, D, D),
        '╒' => arms(N, D, N, L),
        '╓' => arms(N, L, N, D),
        '╔' => arms(N, D, N, D),
        '╕' => arms(D, N, N, L),
        '╖' => arms(L, N, N, D),
        '╗' => arms(D, N, N, D),
        '╘' => arms(N, D, L, N),
        '╙' => arms(N, L, D, N),
        '╚' => arms(N, D, D, N),
        '╛' => arms(D, N, L, N),
        '╜' => arms(L, N, D, N),
        '╝' => arms(D, N, D, N),
        '╞' => arms(N, D, L, L),
        '╟' => arms(N, L, D, D),
        '╠' => arms(N, D, D, D),
        '╡' => arms(D, N, L, L),
        '╢' => arms(L, N, D, D),
        '╣' => arms(D, N, D, D),
        '╤' => arms(D, D, N, L),
        '╥' => arms(L, L, N, D),
        '╦' => arms(D, D, N, D),
        '╧' => arms(D, D, L, N),
        '╨' => arms(L, L, D, N),
        '╩' => arms(D, D, D, N),
        '╪' => arms(D, D, L, L),
        '╫' => arms(L, L, D, D),
        '╬' => arms(D, D, D, D),
        '╭' => return rounded_corner(canvas, light, true, true),
        '╮' => return rounded_corner(canvas, light, false, true),
        '╯' => return rounded_corner(canvas, light, false, false),
        '╰' => return rounded_corner(canvas, light, true, false),
        '╱' => return diagonals(canvas, light, true, false),
        '╲' => return diagonals(canvas, light, false, true),
        '╳' => return diagonals(canvas, light, true, true),
        '╴' => arms(L, N, N, N),
        '╵' => arms(N, N, L, N),
        '╶' => arms(N, L, N, N),
        '╷' => arms(N, N, N, L),
        '╸' => arms(H, N, N, N),
        '╹' => arms(N, N, H, N),
        '╺' => arms(N, H, N, N),
        '╻' => arms(N, N, N, H),
        '╼' => arms(L, H, N, N),
        '╽' => arms(N, N, L, H),
        '╾' => arms(H, L, N, N),
        '╿' => arms(N, N, H, L),
        _ => return,
    };
    draw_arms(canvas, shape, light);
}

/// A stroke `thickness` pixels thick, centred across `n` pixels.
fn centred(n: u32, thickness: u32) -> (u32, u32) {
    let thickness = thickness.min(n);
    let start = (n - thickness) / 2;
    (start, start + thickness)
}

/// The span a light or heavy stroke occupies across `n` pixels. A double
/// stroke spans the same pixels as a heavy one.
fn stroke_span(n: u32, weight: Weight, light: u32) -> (u32, u32) {
    match weight {
        Weight::Light => centred(n, light),
        _ => centred(n, light.saturating_mul(3)),
    }
}

/// The parallel strokes of an arm across `n` pixels: one for light and heavy
/// arms, two for a double arm (the one nearer pixel 0 first).
fn strokes(n: u32, weight: Weight, light: u32) -> ([(u32, u32); 2], usize) {
    if weight != Weight::Double {
        let span = stroke_span(n, weight, light);
        return ([span, span], 1);
    }
    // Shrink the stroke when three of them do not fit, so both lines remain.
    let thickness = if light.saturating_mul(3) > n {
        (n / 3).max(1)
    } else {
        light
    };
    let (start, _) = centred(n, thickness * 3);
    (
        [
            (start, start + thickness),
            (start + 2 * thickness, start + 3 * thickness),
        ],
        2,
    )
}

/// How the arms crossing an axis occupy it, as seen by the arms along it.
#[derive(Debug, Clone, Copy)]
enum Crossing {
    /// Nothing crosses; arms meet at the cell centre.
    None,
    /// One light or heavy stroke.
    Single((u32, u32)),
    /// A double stroke: the band nearer pixel 0, then the other one.
    Double((u32, u32), (u32, u32)),
}

/// Describe the perpendicular arms `before` (up or left) and `after` (down or
/// right) across an axis of `n` pixels.
fn crossing(n: u32, before: Weight, after: Weight, light: u32) -> Crossing {
    if before == Weight::Double || after == Weight::Double {
        let (bands, _) = strokes(n, Weight::Double, light);
        return Crossing::Double(bands[0], bands[1]);
    }
    let heaviest = if before == Weight::Heavy || after == Weight::Heavy {
        Weight::Heavy
    } else if before == Weight::Light || after == Weight::Light {
        Weight::Light
    } else {
        return Crossing::None;
    };
    Crossing::Single(stroke_span(n, heaviest, light))
}

/// Which other arms surround an arm: the perpendicular arm `before` it (up
/// or left), the one `after` it (down or right), and the arm `opposite` it on
/// its own axis.
#[derive(Debug, Clone, Copy)]
struct Neighbours {
    before: bool,
    after: bool,
    opposite: bool,
}

/// Where an arm meets the centre along its own axis of `n` pixels. Returns
/// the inner end of the arm: its start for a right or down arm (`positive`),
/// its end for a left or up arm.
///
/// `stroke` is 0 for the stroke of a double arm nearer the `before`
/// perpendicular arm, 1 for the other.
fn meeting_point(
    n: u32,
    crossing: Crossing,
    weight: Weight,
    stroke: usize,
    neighbours: Neighbours,
    positive: bool,
) -> u32 {
    let Neighbours {
        before,
        after,
        opposite,
    } = neighbours;
    match crossing {
        Crossing::None => n / 2,
        Crossing::Single((start, end)) => {
            if positive {
                start
            } else {
                end
            }
        }
        Crossing::Double(near, far) => {
            // Stop at the nearer line where the double line continues past
            // this stroke, leaving its inside open; otherwise wrap around to
            // the outer line so the strokes join into a corner.
            let stop_at_near_line = if weight == Weight::Double {
                [before, after][stroke]
            } else {
                // A single arm crossing a double line ends at it, unless the
                // single line itself carries on through (as in `╫`).
                before && after && !opposite
            };
            match (positive, stop_at_near_line) {
                (true, true) => far.0,
                (true, false) => near.0,
                (false, true) => near.1,
                (false, false) => far.1,
            }
        }
    }
}

fn draw_arms(canvas: &mut Canvas, shape: Arms, light: u32) {
    let (w, h) = (canvas.width, canvas.height);
    let present = |weight: Weight| weight != Weight::None;
    let vertical = crossing(w, shape.up, shape.down, light);
    let horizontal = crossing(h, shape.left, shape.right, light);

    for (weight, positive, opposite) in [
        (shape.left, false, shape.right),
        (shape.right, true, shape.left),
    ] {
        if !present(weight) {
            continue;
        }
        let (bands, count) = strokes(h, weight, light);
        for (index, &(y0, y1)) in bands[..count].iter().enumerate() {
            let neighbours = Neighbours {
                before: present(shape.up),
                after: present(shape.down),
                opposite: present(opposite),
            };
            let x = meeting_point(w, vertical, weight, index, neighbours, positive);
            if positive {
                canvas.fill(x, y0, w, y1)
            } else {
                canvas.fill(0, y0, x, y1)
            }
        }
    }
    for (weight, positive, opposite) in
        [(shape.up, false, shape.down), (shape.down, true, shape.up)]
    {
        if !present(weight) {
            continue;
        }
        let (bands, count) = strokes(w, weight, light);
        for (index, &(x0, x1)) in bands[..count].iter().enumerate() {
            let neighbours = Neighbours {
                before: present(shape.left),
                after: present(shape.right),
                opposite: present(opposite),
            };
            let y = meeting_point(h, horizontal, weight, index, neighbours, positive);
            if positive {
                canvas.fill(x0, y, x1, h)
            } else {
                canvas.fill(x0, 0, x1, y)
            }
        }
    }
}

/// Dashed lines: `count` dashes per cell, each centred in an equal share of
/// the cell so the gaps stay even across cell boundaries.
fn dashes(canvas: &mut Canvas, horizontal: bool, count: u32, (s0, s1): (u32, u32)) {
    let n = if horizontal {
        canvas.width
    } else {
        canvas.height
    };
    for index in 0..count {
        let start = n * index / count;
        let length = n * (index + 1) / count - start;
        let gap = if length > 1 { (length / 3).max(1) } else { 0 };
        let (d0, d1) = (start + gap / 2, start + length - (gap - gap / 2));
        if horizontal {
            canvas.fill(d0, s0, d1, s1);
        } else {
            canvas.fill(s0, d0, s1, d1);
        }
    }
}

/// Pixel coverage for curved and diagonal strokes, emitted as row runs.
struct Bitmap {
    width: u32,
    height: u32,
    pixels: Vec<bool>,
}

impl Bitmap {
    fn new(width: u32, height: u32) -> Self {
        let len = width as usize * height as usize;
        Self {
            width,
            height,
            pixels: vec![false; len],
        }
    }

    /// Set the `size` x `size` square centred on (`x`, `y`), snapped to whole
    /// pixels and clipped to the cell.
    fn stamp(&mut self, x: f32, y: f32, size: u32) {
        let half = size as f32 / 2.0;
        let (left, top) = ((x - half).round() as i64, (y - half).round() as i64);
        let clamp = |value: i64, limit: u32| value.clamp(0, i64::from(limit)) as u32;
        let (x0, x1) = (
            clamp(left, self.width),
            clamp(left + i64::from(size), self.width),
        );
        let (y0, y1) = (
            clamp(top, self.height),
            clamp(top + i64::from(size), self.height),
        );
        for row in y0..y1 {
            let start = row as usize * self.width as usize;
            self.pixels[start + x0 as usize..start + x1 as usize].fill(true);
        }
    }

    /// Stamp along the straight segment from `from` to `to`.
    fn line(&mut self, from: (f32, f32), to: (f32, f32), size: u32) {
        let length = (to.0 - from.0).abs().max((to.1 - from.1).abs());
        let steps = (length * 4.0).ceil().max(1.0) as u32;
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            self.stamp(
                from.0 + (to.0 - from.0) * t,
                from.1 + (to.1 - from.1) * t,
                size,
            );
        }
    }

    fn emit(&self, canvas: &mut Canvas) {
        for y in 0..self.height {
            let row = &self.pixels[(y * self.width) as usize..((y + 1) * self.width) as usize];
            let mut x = 0;
            while x < self.width {
                if !row[x as usize] {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < self.width && row[x as usize] {
                    x += 1;
                }
                canvas.fill(start, y, x, y + 1);
            }
        }
    }
}

/// `╭ ╮ ╯ ╰`: light arms joined by a quarter circle. `right` and `down` give
/// the directions the arms leave the corner.
fn rounded_corner(canvas: &mut Canvas, light: u32, right: bool, down: bool) {
    let (w, h) = (canvas.width, canvas.height);
    let (x0, x1) = centred(w, light);
    let (y0, y1) = centred(h, light);
    // The largest radius that keeps the arc inside the cell in every
    // orientation, so all four corners share one shape.
    let radius = x0.min(w - x1).min(y0).min(h - y1);
    if radius == 0 {
        return draw_arms(
            canvas,
            arms(
                if right { Weight::None } else { Weight::Light },
                if right { Weight::Light } else { Weight::None },
                if down { Weight::None } else { Weight::Light },
                if down { Weight::Light } else { Weight::None },
            ),
            light,
        );
    }

    // Straight arms from the arc ends to the cell edges.
    if right {
        canvas.fill(x0 + radius, y0, w, y1)
    } else {
        canvas.fill(0, y0, x1 - radius, y1)
    }
    if down {
        canvas.fill(x0, y0 + radius, x1, h)
    } else {
        canvas.fill(x0, 0, x1, y1 - radius)
    }

    let half = light as f32 / 2.0;
    let (centre_x, centre_y) = (x0 as f32 + half, y0 as f32 + half);
    let (sign_x, sign_y) = (
        if right { 1.0 } else { -1.0 },
        if down { 1.0 } else { -1.0 },
    );
    let r = radius as f32;
    let (arc_x, arc_y) = (centre_x + sign_x * r, centre_y + sign_y * r);
    let mut bitmap = Bitmap::new(w, h);
    // Sample the stroke centre often enough that stamps overlap.
    let steps = (r * std::f32::consts::FRAC_PI_2 * 4.0).ceil().max(1.0) as u32;
    for step in 0..=steps {
        let angle = step as f32 / steps as f32 * std::f32::consts::FRAC_PI_2;
        bitmap.stamp(
            arc_x - sign_x * r * angle.cos(),
            arc_y - sign_y * r * angle.sin(),
            light,
        );
    }
    bitmap.emit(canvas);
}

/// `╱ ╲ ╳`: corner-to-corner diagonals, so they join across cells.
fn diagonals(canvas: &mut Canvas, light: u32, rising: bool, falling: bool) {
    let (w, h) = (canvas.width as f32, canvas.height as f32);
    let mut bitmap = Bitmap::new(canvas.width, canvas.height);
    if rising {
        bitmap.line((0.0, h), (w, 0.0), light);
    }
    if falling {
        bitmap.line((0.0, 0.0), (w, h), light);
    }
    bitmap.emit(canvas);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: [(u32, u32, f32); 2] = [(9, 17, 1.0), (18, 34, 2.0)];

    /// Coverage count per pixel; panics if a rectangle leaves the cell.
    fn coverage(c: char, w: u32, h: u32, scale: f32) -> Vec<u32> {
        let mut counts = vec![0; (w * h) as usize];
        for rect in builtin_rects(c, w, h, scale).expect("built-in character") {
            assert!(rect.width > 0 && rect.height > 0, "{c:?}: empty {rect:?}");
            assert!(
                rect.x + rect.width <= w && rect.y + rect.height <= h,
                "{c:?}: {rect:?} leaves the {w}x{h} cell"
            );
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    counts[(y * w + x) as usize] += 1;
                }
            }
        }
        counts
    }

    fn inked(c: char, w: u32, h: u32, scale: f32) -> Vec<bool> {
        coverage(c, w, h, scale)
            .into_iter()
            .map(|count| count > 0)
            .collect()
    }

    /// Rows (or columns) with any ink in column `x` (or row `y`).
    fn inked_rows_at_column(c: char, w: u32, h: u32, scale: f32, x: u32) -> Vec<u32> {
        let ink = inked(c, w, h, scale);
        (0..h).filter(|&y| ink[(y * w + x) as usize]).collect()
    }

    fn inked_columns_at_row(c: char, w: u32, h: u32, scale: f32, y: u32) -> Vec<u32> {
        let ink = inked(c, w, h, scale);
        (0..w).filter(|&x| ink[(y * w + x) as usize]).collect()
    }

    #[test]
    fn halves_split_at_floor_with_the_extra_row_below() {
        for (w, h, scale) in SIZES {
            let upper = builtin_rects('▀', w, h, scale).unwrap();
            let lower = builtin_rects('▄', w, h, scale).unwrap();
            assert_eq!(
                upper,
                [CellRect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h / 2,
                    alpha: 255
                }]
            );
            assert_eq!(
                lower,
                [CellRect {
                    x: 0,
                    y: h / 2,
                    width: w,
                    height: h - h / 2,
                    alpha: 255
                }]
            );
            assert_eq!(
                builtin_rects('█', w, h, scale).unwrap(),
                [CellRect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                    alpha: 255
                }]
            );

            // Upper and lower halves tile the cell exactly once.
            let upper = coverage('▀', w, h, scale);
            let lower = coverage('▄', w, h, scale);
            assert!(upper.iter().zip(&lower).all(|(a, b)| a + b == 1));
        }
        // The documented odd-height rule: 17 rows split 0..8 and 8..17.
        let upper = builtin_rects('▀', 9, 17, 1.0).unwrap()[0];
        let lower = builtin_rects('▄', 9, 17, 1.0).unwrap()[0];
        assert_eq!((upper.y, upper.y + upper.height), (0, 8));
        assert_eq!((lower.y, lower.y + lower.height), (8, 17));
    }

    #[test]
    fn complementary_blocks_tile_without_gaps_or_overlaps() {
        let pairs = [
            ('▀', '▄'),
            ('▌', '▐'),
            ('▔', '▇'),
            ('▉', '▕'),
            ('▘', '▟'),
            ('▝', '▙'),
            ('▖', '▜'),
            ('▗', '▛'),
            ('▚', '▞'),
        ];
        for (w, h, scale) in SIZES {
            for (a, b) in pairs {
                let a_cov = coverage(a, w, h, scale);
                let b_cov = coverage(b, w, h, scale);
                assert!(
                    a_cov.iter().zip(&b_cov).all(|(x, y)| x + y == 1),
                    "{a:?} and {b:?} do not tile a {w}x{h} cell"
                );
            }
            // Every block element stays within its cell and never overlaps itself.
            for c in '\u{2580}'..='\u{259F}' {
                assert!(
                    coverage(c, w, h, scale).iter().all(|&count| count <= 1),
                    "{c:?}"
                );
            }
        }
    }

    #[test]
    fn eighths_grow_monotonically() {
        for (w, h, scale) in SIZES {
            let lower: Vec<u32> = ('\u{2581}'..='\u{2588}')
                .map(|c| builtin_rects(c, w, h, scale).unwrap()[0].height)
                .collect();
            assert!(lower.windows(2).all(|pair| pair[0] <= pair[1]), "{lower:?}");
            let left: Vec<u32> = ('\u{2589}'..='\u{258F}')
                .map(|c| builtin_rects(c, w, h, scale).unwrap()[0].width)
                .collect();
            assert!(left.windows(2).all(|pair| pair[0] >= pair[1]), "{left:?}");
        }
    }

    /// The sextant for a 2x3 bit pattern, inverting `sextant_pattern`.
    fn sextant(pattern: u8) -> char {
        assert!((1..63).contains(&pattern) && pattern != 0b010101 && pattern != 0b101010);
        let index =
            u32::from(pattern) - 1 - u32::from(pattern > 0b010101) - u32::from(pattern > 0b101010);
        char::from_u32(0x1FB00 + index).unwrap()
    }

    #[test]
    fn sextant_patterns_follow_the_unicode_order() {
        // Cells are numbered 1..6 row by row in the character names, so
        // BLOCK SEXTANT-146 is bits 0, 3 and 5.
        let known = [
            ('\u{1FB00}', 0b000001), // SEXTANT-1
            ('\u{1FB01}', 0b000010), // SEXTANT-2
            ('\u{1FB02}', 0b000011), // SEXTANT-12
            ('\u{1FB03}', 0b000100), // SEXTANT-3
            ('\u{1FB13}', 0b010100), // SEXTANT-35
            ('\u{1FB14}', 0b010110), // SEXTANT-235: SEXTANT-135 is ▌
            ('\u{1FB27}', 0b101001), // SEXTANT-146
            ('\u{1FB28}', 0b101011), // SEXTANT-1246: SEXTANT-246 is ▐
            ('\u{1FB3B}', 0b111110), // SEXTANT-23456
        ];
        for (c, pattern) in known {
            assert_eq!(sextant_pattern(c), pattern, "{c:?}");
            assert_eq!(sextant(pattern), c);
        }
        let patterns: Vec<u8> = ('\u{1FB00}'..='\u{1FB3B}').map(sextant_pattern).collect();
        assert_eq!(patterns.len(), 60);
        assert!(patterns.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(!patterns.contains(&0) && !patterns.contains(&63));
        assert!(!patterns.contains(&0b010101) && !patterns.contains(&0b101010));
    }

    #[test]
    fn sextant_rows_split_in_thirds_with_the_extra_rows_below() {
        // 17 rows split 0..5, 5..11 and 11..17; 34 split 0..11, 11..22, 22..34.
        for ((w, h, scale), (first, second)) in SIZES.into_iter().zip([(5, 11), (11, 22)]) {
            let rows = |c: char| {
                let rect = builtin_rects(c, w, h, scale).unwrap()[0];
                (rect.y, rect.y + rect.height)
            };
            assert_eq!(rows(sextant(0b000011)), (0, first));
            assert_eq!(rows(sextant(0b001100)), (first, second));
            assert_eq!(rows(sextant(0b110000)), (second, h));
            // A full row is a single rectangle across the cell.
            assert_eq!(
                builtin_rects(sextant(0b000011), w, h, scale).unwrap(),
                [CellRect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: first,
                    alpha: 255
                }]
            );
            // Columns split where the half blocks do (SEXTANT-35 and -46 are
            // the half blocks minus their top row).
            for y in third(h, 1)..h {
                assert_eq!(
                    inked_columns_at_row(sextant(0b010100), w, h, scale, y),
                    inked_columns_at_row('▌', w, h, scale, y)
                );
                assert_eq!(
                    inked_columns_at_row(sextant(0b101000), w, h, scale, y),
                    inked_columns_at_row('▐', w, h, scale, y)
                );
            }
        }
    }

    #[test]
    fn sextants_and_their_complements_tile_the_cell() {
        for (w, h, scale) in SIZES {
            for c in '\u{1FB00}'..='\u{1FB3B}' {
                let pattern = sextant_pattern(c);
                let complement = sextant(63 - pattern);
                let a = coverage(c, w, h, scale);
                let b = coverage(complement, w, h, scale);
                assert!(
                    a.iter().zip(&b).all(|(x, y)| x + y == 1),
                    "{c:?} and {complement:?} do not tile a {w}x{h} cell"
                );
                // Each lit cell holds its share of the pixels: a row is a
                // third of the height and a column half of the width.
                let lit = a.iter().filter(|&&count| count == 1).count() as u32;
                let expected: u32 = (0..3)
                    .map(|row| {
                        let height = third(h, row + 1) - third(h, row);
                        let left = if pattern & (1 << (2 * row)) != 0 {
                            w / 2
                        } else {
                            0
                        };
                        let right = if pattern & (1 << (2 * row + 1)) != 0 {
                            w - w / 2
                        } else {
                            0
                        };
                        (left + right) * height
                    })
                    .sum();
                assert_eq!(lit, expected, "{c:?} in a {w}x{h} cell");
            }
        }
    }

    #[test]
    fn legacy_eighth_bars_tile_with_the_block_elements() {
        for (w, h, scale) in SIZES {
            // The eight one-eighth columns ▏, U+1FB70..U+1FB75, ▕ and the eight
            // one-eighth rows ▔, U+1FB76..U+1FB7B, ▁ each cover the cell once.
            let columns: Vec<char> = std::iter::once('▏')
                .chain('\u{1FB70}'..='\u{1FB75}')
                .chain(['▕'])
                .collect();
            let rows: Vec<char> = std::iter::once('▔')
                .chain('\u{1FB76}'..='\u{1FB7B}')
                .chain(['▁'])
                .collect();
            for set in [columns, rows] {
                let mut total = vec![0; (w * h) as usize];
                for c in set {
                    for (sum, count) in total.iter_mut().zip(coverage(c, w, h, scale)) {
                        *sum += count;
                    }
                }
                assert!(total.iter().all(|&count| count == 1), "{w}x{h}");
            }

            // Upper and right fractions complement the lower and left ones.
            let pairs = [
                ('\u{1FB82}', '▆'),
                ('\u{1FB83}', '▅'),
                ('\u{1FB84}', '▃'),
                ('\u{1FB85}', '▂'),
                ('\u{1FB86}', '▁'),
                ('\u{1FB87}', '▊'),
                ('\u{1FB88}', '▋'),
                ('\u{1FB89}', '▍'),
                ('\u{1FB8A}', '▎'),
                ('\u{1FB8B}', '▏'),
            ];
            for (a, b) in pairs {
                let a_cov = coverage(a, w, h, scale);
                let b_cov = coverage(b, w, h, scale);
                assert!(
                    a_cov.iter().zip(&b_cov).all(|(x, y)| x + y == 1),
                    "{a:?} and {b:?} do not tile a {w}x{h} cell"
                );
            }

            // The combined bars are the union of the single ones, drawn once.
            let union = |parts: &[char]| -> Vec<bool> {
                let mut ink = vec![false; (w * h) as usize];
                for &c in parts {
                    for (pixel, lit) in ink.iter_mut().zip(inked(c, w, h, scale)) {
                        *pixel |= lit;
                    }
                }
                ink
            };
            let combined = [
                ('\u{1FB7C}', &['▏', '▁'][..]),
                ('\u{1FB7D}', &['▏', '▔']),
                ('\u{1FB7E}', &['▕', '▔']),
                ('\u{1FB7F}', &['▕', '▁']),
                ('\u{1FB80}', &['▔', '▁']),
                ('\u{1FB81}', &['▔', '\u{1FB77}', '\u{1FB79}', '▁']),
            ];
            for (c, parts) in combined {
                assert!(
                    coverage(c, w, h, scale).iter().all(|&count| count <= 1),
                    "{c:?}"
                );
                assert_eq!(inked(c, w, h, scale), union(parts), "{c:?}");
            }
        }
    }

    #[test]
    fn shades_are_uniform_partial_coverage() {
        for (c, alpha) in [('░', 64), ('▒', 128), ('▓', 191)] {
            assert_eq!(
                builtin_rects(c, 9, 17, 1.0).unwrap(),
                [CellRect {
                    x: 0,
                    y: 0,
                    width: 9,
                    height: 17,
                    alpha
                }]
            );
        }
    }

    #[test]
    fn stroke_thickness_follows_scale() {
        assert_eq!(light_thickness(1.0), 1);
        assert_eq!(light_thickness(1.25), 1);
        assert_eq!(light_thickness(2.0), 2);
        assert_eq!(light_thickness(0.5), 1);
        assert_eq!(light_thickness(f32::NAN), 1);
        for (w, h, scale) in SIZES {
            let light = light_thickness(scale);
            assert_eq!(
                inked_rows_at_column('─', w, h, scale, 0).len() as u32,
                light
            );
            assert_eq!(
                inked_rows_at_column('━', w, h, scale, 0).len() as u32,
                3 * light
            );
            assert_eq!(
                inked_columns_at_row('│', w, h, scale, 0).len() as u32,
                light
            );
            assert_eq!(
                inked_columns_at_row('┃', w, h, scale, 0).len() as u32,
                3 * light
            );
        }
    }

    #[test]
    fn horizontal_and_vertical_lines_span_the_cell() {
        for (w, h, scale) in SIZES {
            let rows = inked_rows_at_column('─', w, h, scale, 0);
            for x in 0..w {
                assert_eq!(
                    inked_rows_at_column('─', w, h, scale, x),
                    rows,
                    "─ at x={x}"
                );
            }
            let columns = inked_columns_at_row('│', w, h, scale, 0);
            for y in 0..h {
                assert_eq!(
                    inked_columns_at_row('│', w, h, scale, y),
                    columns,
                    "│ at y={y}"
                );
            }
        }
    }

    /// Render `text` (rows of characters) into one bitmap of adjacent cells.
    fn render(text: &[&str], w: u32, h: u32, scale: f32) -> (Vec<bool>, u32, u32) {
        let columns = text[0].chars().count() as u32;
        let (width, height) = (columns * w, text.len() as u32 * h);
        let mut pixels = vec![false; (width * height) as usize];
        for (row, line) in text.iter().enumerate() {
            for (column, c) in line.chars().enumerate() {
                let Some(rects) = builtin_rects(c, w, h, scale) else {
                    continue;
                };
                for rect in rects {
                    for y in rect.y..rect.y + rect.height {
                        for x in rect.x..rect.x + rect.width {
                            let (x, y) = (column as u32 * w + x, row as u32 * h + y);
                            pixels[(y * width + x) as usize] = true;
                        }
                    }
                }
            }
        }
        (pixels, width, height)
    }

    #[test]
    fn lines_continue_across_adjacent_cells() {
        for (w, h, scale) in SIZES {
            let light = light_thickness(scale);
            let (pixels, width, _) = render(&["────"], w, h, scale);
            let (y0, y1) = centred(h, light);
            for y in 0..h {
                let expected = (y0..y1).contains(&y);
                for x in 0..width {
                    assert_eq!(
                        pixels[(y * width + x) as usize],
                        expected,
                        "──── at ({x}, {y}), scale {scale}"
                    );
                }
            }

            let (pixels, width, height) = render(&["│", "│", "│", "│"], w, h, scale);
            let (x0, x1) = centred(w, light);
            for y in 0..height {
                for x in 0..width {
                    assert_eq!(
                        pixels[(y * width + x) as usize],
                        (x0..x1).contains(&x),
                        "│ stack at ({x}, {y}), scale {scale}"
                    );
                }
            }
        }
    }

    #[test]
    fn junctions_share_the_line_centres() {
        for (w, h, scale) in SIZES {
            let horizontal = inked_rows_at_column('─', w, h, scale, 0);
            let vertical = inked_columns_at_row('│', w, h, scale, 0);
            for c in ['┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '╴', '╶'] {
                let ink = inked(c, w, h, scale);
                for edge_x in [0, w - 1] {
                    let rows: Vec<u32> =
                        (0..h).filter(|&y| ink[(y * w + edge_x) as usize]).collect();
                    assert!(
                        rows.is_empty() || rows == horizontal,
                        "{c:?} meets x={edge_x} at rows {rows:?}, not {horizontal:?}"
                    );
                }
                for edge_y in [0, h - 1] {
                    let columns: Vec<u32> =
                        (0..w).filter(|&x| ink[(edge_y * w + x) as usize]).collect();
                    assert!(
                        columns.is_empty() || columns == vertical,
                        "{c:?} meets y={edge_y} at columns {columns:?}, not {vertical:?}"
                    );
                }
            }
            // Heavy and double lines are centred on the light line too.
            let light_centre = horizontal[0] * 2 + horizontal.len() as u32;
            for c in ['━', '═'] {
                let rows = inked_rows_at_column(c, w, h, scale, 0);
                assert_eq!(
                    rows[0] * 2 + (rows[rows.len() - 1] + 1 - rows[0]),
                    light_centre,
                    "{c:?}"
                );
            }
        }
    }

    #[test]
    fn tees_and_crosses_connect_to_neighbouring_lines() {
        for (w, h, scale) in SIZES {
            // ├─┤ over ┼, then │ below: every junction must be 4-connected.
            let (pixels, width, height) = render(&["┌─┬─┐", "├─┼─┤", "└─┴─┘"], w, h, scale);
            let total = pixels.iter().filter(|&&p| p).count();
            let start = pixels.iter().position(|&p| p).unwrap();
            let mut seen = vec![false; pixels.len()];
            let mut stack = vec![start];
            seen[start] = true;
            let mut reached = 0;
            while let Some(index) = stack.pop() {
                reached += 1;
                let (x, y) = (index as u32 % width, index as u32 / width);
                let mut neighbours = Vec::new();
                if x > 0 {
                    neighbours.push(index - 1);
                }
                if x + 1 < width {
                    neighbours.push(index + 1);
                }
                if y > 0 {
                    neighbours.push(index - width as usize);
                }
                if y + 1 < height {
                    neighbours.push(index + width as usize);
                }
                for next in neighbours {
                    if pixels[next] && !seen[next] {
                        seen[next] = true;
                        stack.push(next);
                    }
                }
            }
            assert_eq!(reached, total, "box grid is not connected at scale {scale}");
        }
    }

    #[test]
    fn double_lines_have_two_strokes_and_open_corners() {
        for (w, h, scale) in SIZES {
            let light = light_thickness(scale);
            let rows = inked_rows_at_column('═', w, h, scale, 0);
            assert_eq!(rows.len() as u32, 2 * light);
            let columns = inked_columns_at_row('║', w, h, scale, 0);
            assert_eq!(columns.len() as u32, 2 * light);
            // ╬ leaves the centre gap clear, so the double lines read as two.
            let ink = inked('╬', w, h, scale);
            let (x, y) = (columns[0] + light, rows[0] + light);
            assert!(!ink[(y * w + x) as usize], "╬ filled its centre");
            // ╔ joins its outer strokes and its inner strokes.
            let (pixels, width, _) = render(&["╔═", "║ "], w, h, scale);
            let outer_corner = (rows[0] * width + columns[0]) as usize;
            assert!(pixels[outer_corner]);
            let inner_corner = ((rows[0] + 2 * light) * width + columns[0] + 2 * light) as usize;
            assert!(pixels[inner_corner]);
        }
    }

    #[test]
    fn rounded_corners_join_their_neighbours() {
        for (w, h, scale) in SIZES {
            let horizontal = inked_rows_at_column('─', w, h, scale, 0);
            let vertical = inked_columns_at_row('│', w, h, scale, 0);
            for (c, edge_x, edge_y) in [
                ('╭', w - 1, h - 1),
                ('╮', 0, h - 1),
                ('╯', 0, 0),
                ('╰', w - 1, 0),
            ] {
                assert_eq!(
                    inked_rows_at_column(c, w, h, scale, edge_x),
                    horizontal,
                    "{c:?}"
                );
                assert_eq!(
                    inked_columns_at_row(c, w, h, scale, edge_y),
                    vertical,
                    "{c:?}"
                );
            }
            let (pixels, width, height) = render(&["╭╮", "╰╯"], w, h, scale);
            // A closed loop: each inked pixel has at least two 8-neighbours.
            for y in 0..height {
                for x in 0..width {
                    if !pixels[(y * width + x) as usize] {
                        continue;
                    }
                    let mut neighbours = 0;
                    for dy in -1i32..=1 {
                        for dx in -1i32..=1 {
                            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                            if (dx, dy) != (0, 0)
                                && nx >= 0
                                && ny >= 0
                                && (nx as u32) < width
                                && (ny as u32) < height
                                && pixels[(ny as u32 * width + nx as u32) as usize]
                            {
                                neighbours += 1;
                            }
                        }
                    }
                    assert!(neighbours >= 2, "loose end at ({x}, {y}), scale {scale}");
                }
            }
        }
    }

    #[test]
    fn dashes_keep_even_gaps_and_the_line_row() {
        for (w, h, scale) in SIZES {
            let rows = inked_rows_at_column('─', w, h, scale, 0);
            for c in ['┄', '┈', '╌'] {
                let ink = inked(c, w, h, scale);
                let inked_columns = (0..w).filter(|&x| ink[(rows[0] * w + x) as usize]).count();
                assert!(inked_columns > 0 && (inked_columns as u32) < w, "{c:?}");
                for y in 0..h {
                    let any = (0..w).any(|x| ink[(y * w + x) as usize]);
                    assert_eq!(any, rows.contains(&y), "{c:?} row {y}");
                }
            }
        }
    }

    #[test]
    fn every_line_character_stays_inside_the_cell() {
        for (w, h, scale) in SIZES
            .into_iter()
            .chain([(1, 1, 1.0), (2, 3, 3.0), (5, 9, 1.5)])
        {
            for c in '\u{2500}'..='\u{259F}' {
                let ink = coverage(c, w, h, scale);
                if w >= 9 {
                    assert!(ink.iter().any(|&count| count > 0), "{c:?} drew nothing");
                }
            }
        }
    }

    #[test]
    fn other_characters_use_the_font() {
        // U+1FB3C-U+1FB6F are triangles and slopes, U+1FB8C onwards shades
        // and legacy glyphs; both stay on the font.
        for c in [
            'a',
            ' ',
            '\u{24FF}',
            '\u{25A0}',
            '⠿',
            '■',
            '\u{1FB3C}',
            '\u{1FB6F}',
            '\u{1FB8C}',
        ] {
            assert_eq!(builtin_rects(c, 9, 17, 1.0), None, "{c:?}");
        }
    }
}
