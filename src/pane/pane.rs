use crate::grid::Grid;
use crate::layout::{PaneId, PixelRect};
use crate::parser::ansi::GraphicsSupport;
use crate::pty::Pty;
use crate::renderer::kitty_handler::{KittyHandler, KittyHandlerOptions};
use crate::selection::SelectionState;
use crate::terminal_decoder::TerminalDecoder;
use crate::terminal_writer::{TerminalWriteQueueError, TerminalWriter, WriterExit};
use std::io;
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

    pub fn max_nonfatal_input_bytes(&self) -> usize {
        self.writer.max_nonfatal_input_bytes()
    }

    fn queue_input_with_policy(
        &self,
        bytes: Vec<u8>,
        full_policy: FullQueuePolicy,
    ) -> Result<(), TerminalWriteQueueError> {
        if self.input_failed.load(Ordering::Acquire) {
            return Err(TerminalWriteQueueError::Disconnected);
        }

        let enqueue_result = match full_policy {
            FullQueuePolicy::FailPane => self.writer.enqueue(bytes),
            FullQueuePolicy::NonFatal => self.writer.enqueue_nonfatal(bytes),
        };
        record_enqueue_result(
            self.input_failed.as_ref(),
            self.id,
            enqueue_result,
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
        let current_cols = self.grid.cols();
        let current_rows = self.grid.rows();
        let pty = Arc::clone(&self.pty);
        if let Err(error) = resize_grid_transactionally(
            current_cols,
            current_rows,
            cols,
            rows,
            move |pty_cols, pty_rows| pty.resize(pty_cols, pty_rows),
            |grid_cols, grid_rows| self.grid.resize(grid_cols, grid_rows),
        ) {
            log::error!("Failed to resize PTY for pane {}: {error}", self.id);
        }
    }
}

fn resize_grid_transactionally<P, G>(
    current_cols: usize,
    current_rows: usize,
    target_cols: usize,
    target_rows: usize,
    mut resize_pty: P,
    mut resize_grid: G,
) -> io::Result<bool>
where
    P: FnMut(u16, u16) -> io::Result<()>,
    G: FnMut(usize, usize),
{
    if target_cols == 0
        || target_rows == 0
        || (target_cols == current_cols && target_rows == current_rows)
    {
        return Ok(false);
    }

    let pty_cols = u16::try_from(target_cols).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("terminal column count {target_cols} exceeds {}", u16::MAX),
        )
    })?;
    let pty_rows = u16::try_from(target_rows).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("terminal row count {target_rows} exceeds {}", u16::MAX),
        )
    })?;

    resize_pty(pty_cols, pty_rows)?;
    resize_grid(target_cols, target_rows);
    Ok(true)
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
        record_enqueue_result, record_writer_exit, FullQueuePolicy, Pane, TerminalWriteQueueError,
        WriterExit,
    };
    use crate::parser::ansi::GraphicsSupport;
    use crate::renderer::kitty_handler::KittyHandlerOptions;
    use std::cell::{Cell, RefCell};
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn pty_resize_completes_before_grid_commit() {
        let events = RefCell::new(Vec::new());

        let resized = super::resize_grid_transactionally(
            80,
            24,
            120,
            40,
            |cols, rows| {
                assert_eq!((cols, rows), (120, 40));
                events.borrow_mut().push("pty");
                Ok(())
            },
            |cols, rows| {
                assert_eq!((cols, rows), (120, 40));
                events.borrow_mut().push("grid");
            },
        )
        .expect("transactional resize should succeed");

        assert!(resized);
        assert_eq!(*events.borrow(), ["pty", "grid"]);
    }

    #[test]
    fn zero_and_unchanged_dimensions_skip_both_resize_callbacks() {
        for (cols, rows) in [(0, 24), (80, 0), (80, 24)] {
            let pty_called = Cell::new(false);
            let grid_called = Cell::new(false);

            let resized = super::resize_grid_transactionally(
                80,
                24,
                cols,
                rows,
                |_, _| {
                    pty_called.set(true);
                    Ok(())
                },
                |_, _| grid_called.set(true),
            )
            .expect("ignored dimensions should remain a successful no-op");

            assert!(!resized);
            assert!(!pty_called.get());
            assert!(!grid_called.get());
        }
    }

    #[test]
    fn failed_pty_resize_preserves_grid_and_same_target_can_retry() {
        let mut grid = crate::grid::Grid::new(80, 24, 1_000);
        let attempts = Cell::new(0);
        let grid_called = Cell::new(false);

        let current_cols = grid.cols();
        let current_rows = grid.rows();
        let failed = super::resize_grid_transactionally(
            current_cols,
            current_rows,
            120,
            40,
            |cols, rows| {
                attempts.set(attempts.get() + 1);
                assert_eq!((cols, rows), (120, 40));
                Err(io::Error::from_raw_os_error(5))
            },
            |cols, rows| {
                grid_called.set(true);
                grid.resize(cols, rows);
            },
        );

        assert_eq!(
            failed.expect_err("PTY resize should fail").raw_os_error(),
            Some(5)
        );
        assert!(!grid_called.get());
        assert_eq!((grid.cols(), grid.rows()), (80, 24));

        let current_cols = grid.cols();
        let current_rows = grid.rows();
        let retried = super::resize_grid_transactionally(
            current_cols,
            current_rows,
            120,
            40,
            |cols, rows| {
                attempts.set(attempts.get() + 1);
                assert_eq!((cols, rows), (120, 40));
                Ok(())
            },
            |cols, rows| grid.resize(cols, rows),
        )
        .expect("the unchanged grid should permit an identical retry");

        assert!(retried);
        assert_eq!(attempts.get(), 2);
        assert_eq!((grid.cols(), grid.rows()), (120, 40));
    }

    #[test]
    fn accepts_pty_dimension_boundaries_and_single_axis_changes() {
        let maximum = usize::from(u16::MAX);

        for (current, target) in [((80, 24), (maximum, maximum)), ((80, 24), (80, 25))] {
            let observed_pty = Cell::new(None);
            let observed_grid = Cell::new(None);

            assert!(super::resize_grid_transactionally(
                current.0,
                current.1,
                target.0,
                target.1,
                |cols, rows| {
                    observed_pty.set(Some((cols, rows)));
                    Ok(())
                },
                |cols, rows| observed_grid.set(Some((cols, rows))),
            )
            .expect("valid PTY dimensions should resize"));

            assert_eq!(
                observed_pty.get(),
                Some((target.0 as u16, target.1 as u16))
            );
            assert_eq!(observed_grid.get(), Some(target));
        }
    }

    #[test]
    fn overflow_is_rejected_before_resize_callbacks() {
        let overflow = usize::from(u16::MAX) + 1;

        for (cols, rows) in [(overflow, 24), (80, overflow)] {
            let pty_called = Cell::new(false);
            let grid_called = Cell::new(false);
            let error = super::resize_grid_transactionally(
                80,
                24,
                cols,
                rows,
                |_, _| {
                    pty_called.set(true);
                    Ok(())
                },
                |_, _| grid_called.set(true),
            )
            .expect_err("dimensions outside the PTY range should fail");

            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(!pty_called.get());
            assert!(!grid_called.get());
        }
    }

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
        let oversized = Vec::with_capacity(pane.max_nonfatal_input_bytes() + 1);

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
