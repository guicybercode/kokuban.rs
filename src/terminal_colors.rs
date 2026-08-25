use crate::grid::cell::Color;

pub(crate) type Rgb = (u8, u8, u8);

// Preserve kokuban's existing first 16 terminal colors across renderers.
const ANSI_COLORS: [Rgb; 16] = [
    (0, 0, 0),
    (205, 49, 49),
    (13, 188, 121),
    (229, 229, 16),
    (36, 114, 200),
    (188, 63, 188),
    (17, 168, 205),
    (229, 229, 229),
    (102, 102, 102),
    (241, 76, 76),
    (35, 209, 139),
    (245, 245, 67),
    (59, 142, 234),
    (214, 112, 214),
    (41, 184, 219),
    (229, 229, 229),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalColors {
    default_foreground: Rgb,
    default_background: Rgb,
}

impl TerminalColors {
    pub(crate) fn new(default_foreground: Rgb, default_background: Rgb) -> Self {
        Self {
            default_foreground,
            default_background,
        }
    }

    pub(crate) fn resolve_foreground(self, color: Color, bold: bool) -> Rgb {
        match color {
            Color::Default => self.default_foreground,
            Color::Indexed(index) if bold && index < 8 => ANSI_COLORS[(index + 8) as usize],
            Color::Indexed(index) => indexed_color(index),
            Color::Rgb(red, green, blue) => (red, green, blue),
        }
    }

    pub(crate) fn resolve_background(self, color: Color) -> Rgb {
        match color {
            Color::Default => self.default_background,
            Color::Indexed(index) => indexed_color(index),
            Color::Rgb(red, green, blue) => (red, green, blue),
        }
    }

    pub(crate) fn default_background(self) -> Rgb {
        self.default_background
    }
}

fn indexed_color(index: u8) -> Rgb {
    match index {
        0..=15 => ANSI_COLORS[index as usize],
        16..=231 => {
            let cube_index = index - 16;
            let red = (cube_index / 36) * 51;
            let green = ((cube_index % 36) / 6) * 51;
            let blue = (cube_index % 6) * 51;
            (red, green, blue)
        }
        232..=255 => {
            let level = 8 + (index - 232) * 10;
            (level, level, level)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalColors;
    use crate::grid::cell::Color;

    const DEFAULT_FOREGROUND: (u8, u8, u8) = (192, 192, 192);
    const DEFAULT_BACKGROUND: (u8, u8, u8) = (26, 26, 46);
    const EXPECTED_ANSI_COLORS: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (229, 229, 229),
    ];

    fn colors() -> TerminalColors {
        TerminalColors::new(DEFAULT_FOREGROUND, DEFAULT_BACKGROUND)
    }

    #[test]
    fn resolves_defaults_and_truecolor_by_semantic_role() {
        let colors = colors();

        assert_eq!(
            colors.resolve_foreground(Color::Default, false),
            DEFAULT_FOREGROUND
        );
        assert_eq!(
            colors.resolve_background(Color::Default),
            DEFAULT_BACKGROUND
        );
        assert_eq!(
            colors.resolve_foreground(Color::Rgb(1, 2, 3), true),
            (1, 2, 3)
        );
        assert_eq!(colors.resolve_background(Color::Rgb(4, 5, 6)), (4, 5, 6));
        assert_eq!(colors.default_background(), DEFAULT_BACKGROUND);
    }

    #[test]
    fn preserves_the_existing_ansi_and_bold_bright_palette() {
        let colors = colors();

        for (index, expected) in EXPECTED_ANSI_COLORS.into_iter().enumerate() {
            assert_eq!(
                colors.resolve_foreground(Color::Indexed(index as u8), false),
                expected
            );
            assert_eq!(
                colors.resolve_background(Color::Indexed(index as u8)),
                expected
            );
        }
        for index in 0_u8..8 {
            assert_eq!(
                colors.resolve_foreground(Color::Indexed(index), true),
                EXPECTED_ANSI_COLORS[(index + 8) as usize]
            );
        }
        assert_eq!(
            colors.resolve_foreground(Color::Indexed(8), true),
            EXPECTED_ANSI_COLORS[8]
        );
        assert_eq!(
            colors.resolve_foreground(Color::Default, true),
            DEFAULT_FOREGROUND
        );
        for index in 8_u8..=u8::MAX {
            assert_eq!(
                colors.resolve_foreground(Color::Indexed(index), true),
                colors.resolve_foreground(Color::Indexed(index), false)
            );
        }
    }

    #[test]
    fn preserves_existing_color_cube_and_grayscale_boundaries() {
        let colors = colors();

        for index in 16_u8..=231 {
            let cube_index = index - 16;
            let expected = (
                (cube_index / 36) * 51,
                ((cube_index % 36) / 6) * 51,
                (cube_index % 6) * 51,
            );
            assert_eq!(
                colors.resolve_foreground(Color::Indexed(index), false),
                expected
            );
            assert_eq!(colors.resolve_background(Color::Indexed(index)), expected);
        }

        for index in 232_u8..=u8::MAX {
            let level = 8 + (index - 232) * 10;
            let expected = (level, level, level);
            assert_eq!(
                colors.resolve_foreground(Color::Indexed(index), false),
                expected
            );
            assert_eq!(colors.resolve_background(Color::Indexed(index)), expected);
        }
    }
}
