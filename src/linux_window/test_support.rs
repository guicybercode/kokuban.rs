//! Shared fixtures for the `linux_window` test modules.

use crate::glyph_atlas::GlyphAtlas;
use crate::grid::Grid;
use crate::terminal_colors::TerminalColors;

pub(super) const DEFAULT_FOREGROUND: (u8, u8, u8) = (192, 192, 192);

pub(super) const DEFAULT_BACKGROUND: (u8, u8, u8) = (26, 26, 46);

pub(super) fn test_colors() -> TerminalColors {
    TerminalColors::new(DEFAULT_FOREGROUND, DEFAULT_BACKGROUND)
}

pub(super) fn test_atlas() -> GlyphAtlas {
    GlyphAtlas::new("kokuban-test-font-that-does-not-exist", 14.0, 1.0)
        .expect("system monospace fallback should be available")
}

pub(super) fn grid_with_scrollback(rows: usize, history_lines: usize) -> Grid {
    let mut grid = Grid::new(8, rows, history_lines + rows);
    grid.cursor_row = rows - 1;
    for _ in 0..history_lines {
        grid.newline();
    }
    assert_eq!(grid.scrollback_len(), history_lines);
    grid
}
