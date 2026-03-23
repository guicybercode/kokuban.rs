/// Returns line segments for a box drawing character (U+2500-U+257F).
/// Each segment is (x0, y0, x1, y1) relative to cell origin.
/// Returns None if not a box drawing char.
pub fn box_drawing_lines(c: char, w: f32, h: f32) -> Option<Vec<(f32, f32, f32, f32)>> {
    let cx = w / 2.0;
    let cy = h / 2.0;
    let segs = match c {
        // Single light lines
        '─' => vec![(0.0, cy, w, cy)],
        '│' => vec![(cx, 0.0, cx, h)],
        '┌' => vec![(cx, cy, w, cy), (cx, cy, cx, h)],
        '┐' => vec![(0.0, cy, cx, cy), (cx, cy, cx, h)],
        '└' => vec![(cx, 0.0, cx, cy), (cx, cy, w, cy)],
        '┘' => vec![(cx, 0.0, cx, cy), (0.0, cy, cx, cy)],
        '├' => vec![(cx, 0.0, cx, h), (cx, cy, w, cy)],
        '┤' => vec![(cx, 0.0, cx, h), (0.0, cy, cx, cy)],
        '┬' => vec![(0.0, cy, w, cy), (cx, cy, cx, h)],
        '┴' => vec![(0.0, cy, w, cy), (cx, 0.0, cx, cy)],
        '┼' => vec![(0.0, cy, w, cy), (cx, 0.0, cx, h)],
        // Double lines
        '═' => {
            let d = 2.0;
            vec![(0.0, cy - d, w, cy - d), (0.0, cy + d, w, cy + d)]
        }
        '║' => {
            let d = 2.0;
            vec![(cx - d, 0.0, cx - d, h), (cx + d, 0.0, cx + d, h)]
        }
        '╔' => {
            let d = 2.0;
            vec![
                (cx - d, cy - d, w, cy - d), (cx - d, cy - d, cx - d, h),
                (cx + d, cy + d, w, cy + d), (cx + d, cy + d, cx + d, h),
            ]
        }
        '╗' => {
            let d = 2.0;
            vec![
                (0.0, cy - d, cx + d, cy - d), (cx + d, cy - d, cx + d, h),
                (0.0, cy + d, cx - d, cy + d), (cx - d, cy + d, cx - d, h),
            ]
        }
        '╚' => {
            let d = 2.0;
            vec![
                (cx - d, 0.0, cx - d, cy + d), (cx - d, cy + d, w, cy + d),
                (cx + d, 0.0, cx + d, cy - d), (cx + d, cy - d, w, cy - d),
            ]
        }
        '╝' => {
            let d = 2.0;
            vec![
                (cx + d, 0.0, cx + d, cy + d), (0.0, cy + d, cx + d, cy + d),
                (cx - d, 0.0, cx - d, cy - d), (0.0, cy - d, cx - d, cy - d),
            ]
        }
        '╠' => {
            let d = 2.0;
            vec![
                (cx - d, 0.0, cx - d, h),
                (cx + d, 0.0, cx + d, cy - d), (cx + d, cy - d, w, cy - d),
                (cx + d, cy + d, cx + d, h), (cx + d, cy + d, w, cy + d),
            ]
        }
        '╣' => {
            let d = 2.0;
            vec![
                (cx + d, 0.0, cx + d, h),
                (cx - d, 0.0, cx - d, cy - d), (0.0, cy - d, cx - d, cy - d),
                (cx - d, cy + d, cx - d, h), (0.0, cy + d, cx - d, cy + d),
            ]
        }
        '╦' => {
            let d = 2.0;
            vec![
                (0.0, cy - d, w, cy - d),
                (cx - d, cy + d, cx - d, h), (0.0, cy + d, cx - d, cy + d),
                (cx + d, cy + d, cx + d, h), (cx + d, cy + d, w, cy + d),
            ]
        }
        '╩' => {
            let d = 2.0;
            vec![
                (0.0, cy + d, w, cy + d),
                (cx - d, 0.0, cx - d, cy - d), (0.0, cy - d, cx - d, cy - d),
                (cx + d, 0.0, cx + d, cy - d), (cx + d, cy - d, w, cy - d),
            ]
        }
        '╬' => {
            let d = 2.0;
            vec![
                (cx - d, 0.0, cx - d, cy - d), (0.0, cy - d, cx - d, cy - d),
                (cx + d, 0.0, cx + d, cy - d), (cx + d, cy - d, w, cy - d),
                (cx - d, cy + d, cx - d, h), (0.0, cy + d, cx - d, cy + d),
                (cx + d, cy + d, cx + d, h), (cx + d, cy + d, w, cy + d),
            ]
        }
        // Rounded corners
        '╭' => vec![(cx, cy, w, cy), (cx, cy, cx, h)],
        '╮' => vec![(0.0, cy, cx, cy), (cx, cy, cx, h)],
        '╯' => vec![(cx, 0.0, cx, cy), (0.0, cy, cx, cy)],
        '╰' => vec![(cx, 0.0, cx, cy), (cx, cy, w, cy)],
        // Light dashes
        '╌' | '┄' | '┈' => vec![(0.0, cy, w, cy)],
        '╎' | '┆' | '┊' => vec![(cx, 0.0, cx, h)],
        // Heavy lines
        '━' => vec![(0.0, cy, w, cy)],
        '┃' => vec![(cx, 0.0, cx, h)],
        '┏' => vec![(cx, cy, w, cy), (cx, cy, cx, h)],
        '┓' => vec![(0.0, cy, cx, cy), (cx, cy, cx, h)],
        '┗' => vec![(cx, 0.0, cx, cy), (cx, cy, w, cy)],
        '┛' => vec![(cx, 0.0, cx, cy), (0.0, cy, cx, cy)],
        '┣' => vec![(cx, 0.0, cx, h), (cx, cy, w, cy)],
        '┫' => vec![(cx, 0.0, cx, h), (0.0, cy, cx, cy)],
        '┳' => vec![(0.0, cy, w, cy), (cx, cy, cx, h)],
        '┻' => vec![(0.0, cy, w, cy), (cx, 0.0, cx, cy)],
        '╋' => vec![(0.0, cy, w, cy), (cx, 0.0, cx, h)],
        _ => return None,
    };
    Some(segs)
}

/// Check if char is in box drawing range
pub fn is_box_drawing(c: char) -> bool {
    ('\u{2500}'..='\u{257F}').contains(&c)
        || ('\u{256D}'..='\u{2570}').contains(&c) // rounded corners
}
