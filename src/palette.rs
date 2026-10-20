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
