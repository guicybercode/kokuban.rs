/// Check if a character is in the braille range (U+2800-U+28FF).
pub fn is_braille(c: char) -> bool {
    ('\u{2800}'..='\u{28FF}').contains(&c)
}

/// Returns dot positions for a braille character as (col, row) in a 2×4 grid.
/// Braille encoding:
///   Bit 0 → (0,0)  Bit 3 → (1,0)
///   Bit 1 → (0,1)  Bit 4 → (1,1)
///   Bit 2 → (0,2)  Bit 5 → (1,2)
///   Bit 6 → (0,3)  Bit 7 → (1,3)
pub fn braille_dots(c: char) -> Vec<(u8, u8)> {
    let bits = (c as u32).wrapping_sub(0x2800);
    if bits > 0xFF {
        return Vec::new();
    }
    let positions: [(u8, u8); 8] = [
        (0, 0), (0, 1), (0, 2),
        (1, 0), (1, 1), (1, 2),
        (0, 3),
        (1, 3),
    ];
    let mut dots = Vec::new();
    for (i, &pos) in positions.iter().enumerate() {
        if bits & (1 << i) != 0 {
            dots.push(pos);
        }
    }
    dots
}
