use crate::grid::cell::{CellFlags, Color};

pub(crate) type Rgb = (u8, u8, u8);
pub(crate) const FAINT_OPACITY: u8 = 128;

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

// xterm's 6x6x6 color cube uses non-uniform intensity levels.
const XTERM_COLOR_CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalColors {
    default_foreground: Rgb,
    default_background: Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedCellColors {
    pub(crate) foreground: Rgb,
    pub(crate) background: Rgb,
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

    pub(crate) fn resolve_cell_colors(
        self,
        foreground: Color,
        background: Color,
        flags: CellFlags,
    ) -> ResolvedCellColors {
        let semantic_foreground =
            self.resolve_foreground(foreground, flags.contains(CellFlags::BOLD));
        let semantic_background = self.resolve_background(background);
        let (mut foreground, background) = if flags.contains(CellFlags::REVERSE) {
            (semantic_background, semantic_foreground)
        } else {
            (semantic_foreground, semantic_background)
        };

        if flags.contains(CellFlags::FAINT) {
            foreground = blend_rgb(foreground, background, FAINT_OPACITY);
        }

        ResolvedCellColors {
            foreground,
            background,
        }
    }

    pub(crate) fn default_background(self) -> Rgb {
        self.default_background
    }
}

fn blend_rgb(foreground: Rgb, background: Rgb, opacity: u8) -> Rgb {
    (
        blend_channel(foreground.0, background.0, opacity),
        blend_channel(foreground.1, background.1, opacity),
        blend_channel(foreground.2, background.2, opacity),
    )
}

fn blend_channel(foreground: u8, background: u8, opacity: u8) -> u8 {
    let opacity = u32::from(opacity);
    let inverse_opacity = u32::from(u8::MAX) - opacity;
    let numerator = u32::from(foreground) * opacity
        + u32::from(background) * inverse_opacity
        + u32::from(u8::MAX) / 2;
    let rounded = numerator / u32::from(u8::MAX);
    u8::try_from(rounded).unwrap_or(u8::MAX)
}

fn indexed_color(index: u8) -> Rgb {
    match index {
        0..=15 => ANSI_COLORS[index as usize],
        16..=231 => {
            let cube_index = index - 16;
            let red = XTERM_COLOR_CUBE_LEVELS[(cube_index / 36) as usize];
            let green = XTERM_COLOR_CUBE_LEVELS[((cube_index % 36) / 6) as usize];
            let blue = XTERM_COLOR_CUBE_LEVELS[(cube_index % 6) as usize];
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
    use super::{ResolvedCellColors, TerminalColors, FAINT_OPACITY};
    use crate::grid::cell::{CellFlags, Color};

    const DEFAULT_FOREGROUND: (u8, u8, u8) = (192, 192, 192);
    const DEFAULT_BACKGROUND: (u8, u8, u8) = (26, 26, 46);
    const EXPECTED_XTERM_COLOR_CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
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
    fn resolves_xterm_color_cube_and_grayscale_boundaries() {
        let colors = colors();

        for index in 16_u8..=231 {
            let cube_index = index - 16;
            let expected = (
                EXPECTED_XTERM_COLOR_CUBE_LEVELS[(cube_index / 36) as usize],
                EXPECTED_XTERM_COLOR_CUBE_LEVELS[((cube_index % 36) / 6) as usize],
                EXPECTED_XTERM_COLOR_CUBE_LEVELS[(cube_index % 6) as usize],
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

    #[test]
    fn resolves_default_indexed_and_truecolor_faint_foregrounds() {
        let colors = colors();

        assert_eq!(FAINT_OPACITY, 128);
        assert_eq!(
            colors.resolve_cell_colors(
                Color::Default,
                Color::Default,
                CellFlags::FAINT,
            ),
            ResolvedCellColors {
                foreground: (109, 109, 119),
                background: DEFAULT_BACKGROUND,
            }
        );
        assert_eq!(
            colors.resolve_cell_colors(
                Color::Indexed(1),
                Color::Indexed(4),
                CellFlags::FAINT,
            ),
            ResolvedCellColors {
                foreground: (121, 81, 124),
                background: (36, 114, 200),
            }
        );
        assert_eq!(
            colors.resolve_cell_colors(
                Color::Rgb(255, 0, 128),
                Color::Rgb(0, 255, 64),
                CellFlags::FAINT,
            ),
            ResolvedCellColors {
                foreground: (128, 127, 96),
                background: (0, 255, 64),
            }
        );
    }

    #[test]
    fn faint_blending_rounds_safely_at_channel_extremes() {
        let colors = colors();

        assert_eq!(
            colors.resolve_cell_colors(
                Color::Rgb(0, 0, 0),
                Color::Rgb(255, 255, 255),
                CellFlags::FAINT,
            ),
            ResolvedCellColors {
                foreground: (127, 127, 127),
                background: (255, 255, 255),
            }
        );
        assert_eq!(
            colors.resolve_cell_colors(
                Color::Rgb(255, 255, 255),
                Color::Rgb(0, 0, 0),
                CellFlags::FAINT,
            ),
            ResolvedCellColors {
                foreground: (128, 128, 128),
                background: (0, 0, 0),
            }
        );
    }

    #[test]
    fn bold_bright_and_reverse_are_resolved_before_faint() {
        let colors = colors();

        assert_eq!(
            colors.resolve_cell_colors(
                Color::Indexed(1),
                Color::Default,
                CellFlags::BOLD | CellFlags::FAINT,
            ),
            ResolvedCellColors {
                foreground: (134, 51, 61),
                background: DEFAULT_BACKGROUND,
            }
        );
        assert_eq!(
            colors.resolve_cell_colors(
                Color::Rgb(0, 0, 0),
                Color::Rgb(255, 255, 255),
                CellFlags::REVERSE | CellFlags::FAINT,
            ),
            ResolvedCellColors {
                foreground: (128, 128, 128),
                background: (0, 0, 0),
            }
        );
    }
}
