use crate::grid::Grid;
use crate::layout::{PaneId, PixelRect};
use crate::parser::ansi::GraphicsSupport;
use crate::pty::Pty;
use crate::renderer::kitty_handler::{KittyHandler, KittyHandlerOptions};
use crate::selection::SelectionState;
use crate::terminal_decoder::TerminalDecoder;

pub struct Pane {
    pub id: PaneId,
    pub pty: Pty,
    pub decoder: TerminalDecoder,
    pub grid: Grid,
    pub selection: SelectionState,
    pub rect: PixelRect,
    pub kitty_handler: KittyHandler,
}

impl Pane {
    pub fn new(
        id: PaneId,
        cols: u16,
        rows: u16,
        scrollback_max: usize,
        kitty_options: KittyHandlerOptions,
        graphics_support: GraphicsSupport,
    ) -> Result<Self, crate::pty::PtyError> {
        let pty = Pty::spawn(cols, rows, graphics_support.kitty, graphics_support.sixel)?;
        let grid = Grid::new(cols as usize, rows as usize, scrollback_max);
        Ok(Self {
            id,
            pty,
            decoder: TerminalDecoder::new(graphics_support),
            grid,
            selection: SelectionState::default(),
            rect: PixelRect::ZERO,
            kitty_handler: KittyHandler::new(kitty_options),
        })
    }

    pub fn resize_grid(&mut self, cols: usize, rows: usize) {
        if cols > 0 && rows > 0 && (cols != self.grid.cols() || rows != self.grid.rows()) {
            self.grid.resize(cols, rows);
            if let Err(e) = self.pty.resize(cols as u16, rows as u16) {
                log::error!("Failed to resize PTY for pane {}: {e}", self.id);
            }
        }
    }
}
