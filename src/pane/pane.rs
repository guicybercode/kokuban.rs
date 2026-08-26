use crate::grid::Grid;
use crate::layout::{PaneId, PixelRect};
use crate::parser::ansi::GraphicsSupport;
use crate::pty::Pty;
use crate::renderer::kitty_handler::{KittyHandler, KittyHandlerOptions};
use crate::selection::SelectionState;
use crate::terminal_decoder::TerminalDecoder;
use crate::terminal_writer::{TerminalWriteQueueError, TerminalWriter, WriterExit};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Pane {
    pub id: PaneId,
    writer: TerminalWriter,
    pub pty: Arc<Pty>,
    input_failed: Arc<AtomicBool>,
    pub decoder: TerminalDecoder,
    pub grid: Grid,
    pub selection: SelectionState,
    pub rect: PixelRect,
    pub kitty_handler: KittyHandler,
}

#[derive(Clone, Copy)]
enum FullQueuePolicy {
    FailPane,
    NonFatal,
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
        let pty = Arc::new(Pty::spawn(
            cols,
            rows,
            graphics_support.kitty,
            graphics_support.sixel,
        )?);
        let input_failed = Arc::new(AtomicBool::new(false));
        let writer_input_failed = input_failed.clone();
        let writer = TerminalWriter::spawn(pty.clone(), move |exit| {
            record_writer_exit(writer_input_failed.as_ref(), id, exit);
        })?;
        let grid = Grid::new(cols as usize, rows as usize, scrollback_max);
        Ok(Self {
            id,
            writer,
            pty,
            input_failed,
            decoder: TerminalDecoder::new(graphics_support),
            grid,
            selection: SelectionState::default(),
            rect: PixelRect::ZERO,
            kitty_handler: KittyHandler::new(kitty_options),
        })
    }

    pub fn queue_input(&self, bytes: Vec<u8>) {
        let _ = self.queue_input_with_policy(bytes, FullQueuePolicy::FailPane);
    }

    pub fn queue_motion_input(&self, bytes: Vec<u8>) {
        let _ = self.queue_input_with_policy(bytes, FullQueuePolicy::NonFatal);
    }

    pub fn queue_paste_input(&self, bytes: Vec<u8>) -> Result<(), TerminalWriteQueueError> {
        self.queue_input_with_policy(bytes, FullQueuePolicy::NonFatal)
    }

    pub fn max_input_bytes(&self) -> usize {
        self.writer.max_input_bytes()
    }

    fn queue_input_with_policy(
        &self,
        bytes: Vec<u8>,
        full_policy: FullQueuePolicy,
    ) -> Result<(), TerminalWriteQueueError> {
        if self.input_failed.load(Ordering::Acquire) {
            return Err(TerminalWriteQueueError::Disconnected);
        }

        record_enqueue_result(
            self.input_failed.as_ref(),
            self.id,
            self.writer.enqueue(bytes),
            full_policy,
        )
    }

    pub fn input_failed(&self) -> bool {
        self.input_failed.load(Ordering::Acquire)
    }

    pub fn retire(self) {
        match self.writer.shutdown_and_join() {
            Ok(WriterExit::Shutdown) => {}
            Ok(WriterExit::ChannelClosed) => {
                log::error!(
                    "Terminal writer channel closed unexpectedly for pane {}",
                    self.id
                );
            }
            Ok(WriterExit::WriteFailed(error)) => {
                log::error!("Terminal writer failed for pane {}: {error}", self.id);
            }
            Err(_) => {
                log::error!("Terminal writer thread panicked for pane {}", self.id);
            }
        }
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

fn record_enqueue_result(
    input_failed: &AtomicBool,
    pane_id: PaneId,
    result: Result<(), TerminalWriteQueueError>,
    full_policy: FullQueuePolicy,
) -> Result<(), TerminalWriteQueueError> {
    match result {
        Ok(()) => Ok(()),
        Err(TerminalWriteQueueError::Full) if matches!(full_policy, FullQueuePolicy::NonFatal) => {
            Err(TerminalWriteQueueError::Full)
        }
        Err(TerminalWriteQueueError::Full) => {
            mark_input_failed(input_failed, pane_id, "terminal input queue is full");
            Err(TerminalWriteQueueError::Full)
        }
        Err(TerminalWriteQueueError::Disconnected) => {
            mark_input_failed(input_failed, pane_id, "terminal input queue disconnected");
            Err(TerminalWriteQueueError::Disconnected)
        }
    }
}

fn record_writer_exit(input_failed: &AtomicBool, pane_id: PaneId, exit: &WriterExit) -> bool {
    match exit {
        WriterExit::Shutdown => false,
        WriterExit::ChannelClosed => {
            mark_input_failed(input_failed, pane_id, "terminal input queue disconnected")
        }
        WriterExit::WriteFailed(error) => {
            mark_input_failed(input_failed, pane_id, &format!("PTY write failed: {error}"))
        }
    }
}

fn mark_input_failed(input_failed: &AtomicBool, pane_id: PaneId, message: &str) -> bool {
    let transitioned = !input_failed.swap(true, Ordering::AcqRel);
    if transitioned {
        log::error!("Terminal input failed for pane {pane_id}: {message}");
    }
    transitioned
}

#[cfg(test)]
mod tests {
    use super::{
        record_enqueue_result, record_writer_exit, FullQueuePolicy, Pane,
        TerminalWriteQueueError, WriterExit,
    };
    use crate::parser::ansi::GraphicsSupport;
    use crate::renderer::kitty_handler::KittyHandlerOptions;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn lossless_queue_errors_mark_input_failed_once() {
        for error in [
            TerminalWriteQueueError::Full,
            TerminalWriteQueueError::Disconnected,
        ] {
            let failed = AtomicBool::new(false);
            assert_eq!(
                record_enqueue_result(&failed, 7, Err(error), FullQueuePolicy::FailPane),
                Err(error),
            );
            assert!(failed.load(Ordering::Acquire));
            assert_eq!(
                record_enqueue_result(&failed, 7, Err(error), FullQueuePolicy::FailPane),
                Err(error),
            );
        }
    }

    #[test]
    fn nonfatal_input_policy_drops_only_full_backpressure() {
        let failed = AtomicBool::new(false);
        assert_eq!(
            record_enqueue_result(
                &failed,
                9,
                Err(TerminalWriteQueueError::Full),
                FullQueuePolicy::NonFatal,
            ),
            Err(TerminalWriteQueueError::Full),
        );
        assert!(!failed.load(Ordering::Acquire));

        assert_eq!(
            record_enqueue_result(
                &failed,
                9,
                Err(TerminalWriteQueueError::Disconnected),
                FullQueuePolicy::NonFatal,
            ),
            Err(TerminalWriteQueueError::Disconnected),
        );
        assert!(failed.load(Ordering::Acquire));
    }

    #[test]
    fn paste_queue_keeps_capacity_backpressure_nonfatal() {
        let pane = Pane::new(
            13,
            80,
            24,
            1_000,
            KittyHandlerOptions::from_megabytes(1, false),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
        )
        .expect("paste policy test should spawn its shell");
        let oversized = Vec::with_capacity(pane.max_input_bytes() + 1);

        assert_eq!(
            pane.queue_paste_input(oversized),
            Err(TerminalWriteQueueError::Full)
        );
        assert!(!pane.input_failed());
        pane.retire();
    }

    #[test]
    fn writer_failures_are_observable_but_shutdown_is_healthy() {
        let failed = AtomicBool::new(false);
        assert!(!record_writer_exit(&failed, 11, &WriterExit::Shutdown));
        assert!(!failed.load(Ordering::Acquire));

        let exit = WriterExit::WriteFailed(io::Error::from_raw_os_error(5));
        assert!(record_writer_exit(&failed, 11, &exit));
        assert!(failed.load(Ordering::Acquire));
        assert!(!record_writer_exit(&failed, 11, &WriterExit::ChannelClosed));
    }
}
