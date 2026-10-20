//! Configured terminal colors, shared by the Linux and macOS renderers so both
//! apply kokuban.toml the same way.

use crate::config::{ColorConfig, Config};
use crate::grid::cell::Color;
use crate::terminal_colors::{Rgb, TerminalColors};

/// Explicit settings keep their priority when the system palette changes.
pub(crate) struct PaletteOverrides {
    base: TerminalColors,
    foreground: Option<Rgb>,
    background: Option<Rgb>,
    selection_foreground: Option<Rgb>,
    selection_background: Option<Rgb>,
    cursor: Option<Rgb>,
    ansi: Option<[Rgb; 16]>,
}

impl PaletteOverrides {
    pub(crate) fn from_config(config: &Config) -> Self {
        let foreground = ColorConfig::parse_hex(&config.colors.foreground);
        let background = ColorConfig::parse_hex(&config.colors.background);
        Self {
            base: TerminalColors::new(foreground, background),
            foreground: config.colors.explicit_foreground.then_some(foreground),
            background: config.colors.explicit_background.then_some(background),
            selection_foreground: config
                .selection
                .explicit_foreground
                .then(|| ColorConfig::parse_hex(&config.selection.foreground)),
            selection_background: config
                .selection
                .explicit_background
                .then(|| ColorConfig::parse_hex(&config.selection.background)),
            cursor: config.colors.cursor.as_deref().map(ColorConfig::parse_hex),
            ansi: config
                .colors
                .ansi
                .as_ref()
                .map(|ansi| std::array::from_fn(|index| ColorConfig::parse_hex(&ansi[index]))),
        }
    }

    pub(crate) fn resolve(&self, theme: Option<TerminalColors>) -> TerminalColors {
        let colors = theme.unwrap_or(self.base);
        let mut colors = colors.with_defaults(
            self.foreground
                .unwrap_or_else(|| colors.resolve_foreground(Color::Default, false)),
            self.background
                .unwrap_or_else(|| colors.default_background()),
        );
        colors = colors.with_selection(
            Some(
                self.selection_foreground
                    .unwrap_or_else(|| colors.selection_foreground()),
            ),
            Some(
                self.selection_background
                    .unwrap_or_else(|| colors.selection_background()),
            ),
        );
        if let Some(cursor) = self.cursor {
            colors = colors.with_cursor(cursor);
        }
        if let Some(ansi) = self.ansi {
            colors = colors.with_ansi(ansi);
        }
        colors
    }
}

/// Colors for a renderer without a system palette, such as Metal on macOS.
#[cfg_attr(all(target_os = "linux", not(test)), allow(dead_code))]
pub(crate) fn configured_colors(config: &Config) -> TerminalColors {
    PaletteOverrides::from_config(config).resolve(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_palette() -> (Config, [Rgb; 16]) {
        let ansi: [Rgb; 16] =
            std::array::from_fn(|index| (index as u8 * 16, 0x40, 0x80 + index as u8));
        let mut source = "[colors]\nforeground = '#a1b2c3'\nbackground = '#0d0e0f'\n\
                          cursor = '#112233'\nansi = ["
            .to_string();
        for (red, green, blue) in ansi {
            source.push_str(&format!("'#{red:02x}{green:02x}{blue:02x}',"));
        }
        source.push_str("]\n");
        (toml::from_str(&source).unwrap(), ansi)
    }

    #[test]
    fn configured_colors_use_the_custom_ansi_palette() {
        let (config, ansi) = custom_palette();
        let colors = configured_colors(&config);
        for (index, expected) in ansi.into_iter().enumerate() {
            let color = Color::Indexed(index as u8);
            assert_eq!(
                colors.resolve_foreground(color, false),
                expected,
                "color{index}"
            );
            assert_eq!(colors.resolve_background(color), expected, "color{index}");
        }
        assert_eq!(colors.resolve_foreground(Color::Indexed(1), true), ansi[9]);
        assert_eq!(
            colors.resolve_foreground(Color::Default, false),
            (0xa1, 0xb2, 0xc3)
        );
        assert_eq!(colors.default_background(), (0x0d, 0x0e, 0x0f));
        assert_eq!(colors.cursor(), Some((0x11, 0x22, 0x33)));
    }

    #[test]
    fn configured_colors_match_the_linux_resolution_without_a_system_theme() {
        let (custom, _) = custom_palette();
        let selection: Config =
            toml::from_str("[selection]\nforeground = '#010101'\nbackground = '#fefefe'\n")
                .unwrap();
        for config in [Config::default(), custom, selection] {
            assert_eq!(
                configured_colors(&config),
                PaletteOverrides::from_config(&config).resolve(None)
            );
        }
    }
}
