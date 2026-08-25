use crate::grid::Grid;
use crate::layout::PixelRect;
use crate::selection::SelectionState;

pub struct PaneRenderData<'a> {
    pub grid: &'a Grid,
    pub rect: PixelRect,
    pub selection: Option<&'a SelectionState>,
    pub is_focused: bool,
    pub pane_index: usize,
    pub cwd: &'a str,
    pub prompt_mark_rows: Vec<usize>,
    pub show_cursor: bool,
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
    pub hanko_dim: (u8, u8, u8),
}
