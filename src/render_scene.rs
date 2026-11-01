use crate::grid::Grid;
use crate::layout::PixelRect;
use crate::search::SearchState;
use crate::selection::SelectionState;

pub struct PaneRenderData<'a> {
    pub grid: &'a Grid,
    pub rect: PixelRect,
    pub selection: Option<&'a SelectionState>,
    pub is_focused: bool,
    /// Unwatched BEL / notification: draws a red dot next to the focus marker.
    pub attention: bool,
    pub pane_index: usize,
    /// Program running in the pane (shell or `-e` command), for the status bar.
    pub program: &'a str,
    pub cwd: &'a str,
    pub prompt_mark_rows: Vec<usize>,
    pub show_cursor: bool,
    /// Update notice shown at the right of the status bar.
    pub update_notice: Option<&'a str>,
    /// Busca aberta neste painel; os resultados são destacados e a consulta
    /// aparece na barra de status.
    pub search: Option<&'a SearchState>,
    /// Text being composed by the input method (macOS IME); drawn at the
    /// cursor with an underline and never written to the grid.
    pub preedit: Option<&'a str>,
    /// Underline cells that carry an OSC 8 hyperlink (while the modifier that
    /// opens links is held).
    pub underline_links: bool,
}

/// Which status bar segments `[status_bar]` enables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusBarLayout {
    pub show_shell: bool,
    pub show_cwd: bool,
    pub show_pane_index: bool,
}

pub struct ConfirmOverlayInfo {
    pub region: PixelRect,
    pub title: String,
    pub process_text: Option<String>,
    pub opacity: f32,
}

pub struct ChromeColors {
    pub sumi_dark: (u8, u8, u8),
    pub sumi_medium: (u8, u8, u8),
    pub sumi_light: (u8, u8, u8),
    pub sumi_ghost: (u8, u8, u8),
    pub hanko_red: (u8, u8, u8),
}
