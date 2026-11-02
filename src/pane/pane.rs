use crate::grid::Grid;
use crate::layout::{PaneId, PixelRect};
use crate::parser::ansi::GraphicsSupport;
use crate::pty::Pty;
use crate::renderer::kitty_handler::{KittyHandler, KittyHandlerOptions};
use crate::search::{SearchKey, SearchKeyOutcome, SearchState};
use crate::selection::SelectionState;
use crate::terminal_decoder::TerminalDecoder;
use crate::terminal_writer::{TerminalWriteQueueError, TerminalWriter, WriterExit};
use std::ffi::OsString;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Called after a pane's input first fails so its owner can close it promptly.
pub type InputFailureNotifier = Arc<dyn Fn() + Send + Sync>;

pub struct Pane {
    pub id: PaneId,
    writer: TerminalWriter,
    pub pty: Arc<Pty>,
    input_failed: Arc<AtomicBool>,
    on_input_failed: InputFailureNotifier,
    pub decoder: TerminalDecoder,
    pub grid: Grid,
    pub selection: SelectionState,
    /// Busca aberta neste painel (`None` quando fechada).
    pub search: Option<SearchState>,
    /// Última consulta usada, reaproveitada ao reabrir a busca.
    pub last_search_query: String,
    pub rect: PixelRect,
    pub kitty_handler: KittyHandler,
    /// BEL or a notification arrived while this pane was not being watched.
    pub attention: bool,
}

#[derive(Clone, Copy)]
enum FullQueuePolicy {
    FailPane,
    NonFatal,
}

impl Pane {
    // Constructor mirrors the PTY/grid/decoder inputs; a params struct would just rename them.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: PaneId,
        cols: u16,
        rows: u16,
        scrollback_max: usize,
        kitty_options: KittyHandlerOptions,
        graphics_support: GraphicsSupport,
        on_input_failed: InputFailureNotifier,
        command: &[OsString],
    ) -> Result<Self, crate::pty::PtyError> {
        // An explicit `-e` command replaces the login shell; splits pass an
        // empty slice and get the shell.
        let pty = if command.is_empty() {
            Pty::spawn(cols, rows, graphics_support.kitty, graphics_support.sixel)?
        } else {
            Pty::spawn_command(
                cols,
                rows,
                graphics_support.kitty,
                graphics_support.sixel,
                command,
            )?
        };
        let pty = Arc::new(pty);
        let input_failed = Arc::new(AtomicBool::new(false));
        let writer_input_failed = input_failed.clone();
        let writer_on_input_failed = on_input_failed.clone();
        let writer = TerminalWriter::spawn(pty.clone(), move |exit| {
            if record_writer_exit(writer_input_failed.as_ref(), id, exit) {
                writer_on_input_failed();
            }
        })?;
        let grid = Grid::new(cols as usize, rows as usize, scrollback_max);
        Ok(Self {
            id,
            writer,
            pty,
            input_failed,
            on_input_failed,
            decoder: TerminalDecoder::new(graphics_support),
            grid,
            selection: SelectionState::default(),
            search: None,
            last_search_query: String::new(),
            rect: PixelRect::ZERO,
            kitty_handler: KittyHandler::new(kitty_options),
            attention: false,
        })
    }

    pub fn queue_input(&self, bytes: Vec<u8>) {
        self.input().queue_input(bytes);
    }

    // The Linux window enqueues motion through `PaneInput` directly.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn queue_motion_input(&self, bytes: Vec<u8>) {
        let _ = self.input().queue_nonfatal_input(bytes);
    }

    pub fn queue_paste_input(&self, bytes: Vec<u8>) -> Result<(), TerminalWriteQueueError> {
        self.input().queue_nonfatal_input(bytes)
    }

    pub fn max_nonfatal_input_bytes(&self) -> usize {
        self.writer.max_nonfatal_input_bytes()
    }

    /// The input queue alone, so the grid can be borrowed mutably alongside
    /// it (mode checks and the enqueue happen in one locked step).
    pub fn input(&self) -> PaneInput<'_> {
        PaneInput {
            id: self.id,
            writer: &self.writer,
            input_failed: &self.input_failed,
            on_input_failed: &self.on_input_failed,
        }
    }

    /// Grid and input queue borrowed apart.
    // The macOS window holds the whole pane while it dispatches.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn grid_and_input(&mut self) -> (&mut Grid, PaneInput<'_>) {
        let input = PaneInput {
            id: self.id,
            writer: &self.writer,
            input_failed: &self.input_failed,
            on_input_failed: &self.on_input_failed,
        };
        (&mut self.grid, input)
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

    /// Entrega uma tecla à busca aberta deste painel. `None` quando não há
    /// busca aberta (a tecla segue para o shell). Ao fechar, guarda a
    /// consulta em `last_search_query`.
    pub fn handle_search_key(&mut self, key: SearchKey) -> Option<SearchKeyOutcome> {
        let search = self.search.as_mut()?;
        let outcome = search.handle_key(key, &mut self.grid);
        if let SearchKeyOutcome::Close { last_query } = &outcome {
            self.last_search_query = last_query.clone();
            self.search = None;
        }
        Some(outcome)
    }

    /// Resize the grid and the PTY together; `cell_w`/`cell_h` (physical
    /// pixels) fill the `TIOCSWINSZ` pixel fields so programs can size images.
    pub fn resize_grid(&mut self, cols: usize, rows: usize, cell_w: f32, cell_h: f32) {
        let current_cols = self.grid.cols();
        let current_rows = self.grid.rows();
        let pty = Arc::clone(&self.pty);
        if let Err(error) = resize_grid_transactionally(
            current_cols,
            current_rows,
            cols,
            rows,
            move |pty_cols, pty_rows| {
                pty.resize_with_pixels(
                    pty_cols,
                    pty_rows,
                    pixel_extent(pty_cols, cell_w),
                    pixel_extent(pty_rows, cell_h),
                )
            },
            |grid_cols, grid_rows| {
                self.grid.resize(grid_cols, grid_rows);
                // Reflow changes retained-row coordinates, as on the Linux path.
                self.selection.clear();
            },
        ) {
            log::error!("Failed to resize PTY for pane {}: {error}", self.id);
        }
    }
}

/// A pane's input queue without its grid.
pub struct PaneInput<'a> {
    id: PaneId,
    writer: &'a TerminalWriter,
    input_failed: &'a AtomicBool,
    on_input_failed: &'a InputFailureNotifier,
}

impl PaneInput<'_> {
    /// Lossless input: a full or closed queue fails the pane.
    pub fn queue_input(&self, bytes: Vec<u8>) {
        let _ = self.queue_input_with_policy(bytes, FullQueuePolicy::FailPane);
    }

    /// Coalescible input (mouse motion, pastes): a full queue only reports
    /// `Full`; a closed queue still fails the pane.
    pub fn queue_nonfatal_input(&self, bytes: Vec<u8>) -> Result<(), TerminalWriteQueueError> {
        self.queue_input_with_policy(bytes, FullQueuePolicy::NonFatal)
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
        let result = record_enqueue_result(self.input_failed, self.id, enqueue_result, full_policy);
        if self.input_failed.load(Ordering::Acquire) {
            (self.on_input_failed)();
        }
        result
    }
}

/// Pixel size of `cells` cells of `cell_size` physical pixels, saturated to
/// the `winsize` range; non-finite or negative sizes report zero.
fn pixel_extent(cells: u16, cell_size: f32) -> u16 {
    let pixels = f32::from(cells) * cell_size;
    if pixels.is_finite() && pixels > 0.0 {
        pixels.round().min(f32::from(u16::MAX)) as u16
    } else {
        0
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
        record_enqueue_result, record_writer_exit, FullQueuePolicy, Pane, SearchKey,
        SearchKeyOutcome, SearchState, TerminalWriteQueueError, WriterExit,
    };
    use crate::parser::ansi::GraphicsSupport;
    use crate::renderer::kitty_handler::KittyHandlerOptions;
    use std::cell::{Cell, RefCell};
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn pixel_extent_rounds_and_saturates() {
        assert_eq!(super::pixel_extent(80, 10.4), 832);
        assert_eq!(super::pixel_extent(24, 20.6), 494);
        assert_eq!(super::pixel_extent(0, 10.0), 0);
        assert_eq!(super::pixel_extent(10, 0.0), 0);
        assert_eq!(super::pixel_extent(10, -1.0), 0);
        assert_eq!(super::pixel_extent(10, f32::NAN), 0);
        assert_eq!(super::pixel_extent(u16::MAX, 10.0), u16::MAX);
    }

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

            assert_eq!(observed_pty.get(), Some((target.0 as u16, target.1 as u16)));
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
    fn search_keys_reach_only_an_open_search_and_escape_remembers_the_query() {
        let mut pane = Pane::new(
            17,
            80,
            24,
            1_000,
            KittyHandlerOptions::from_megabytes(1, false),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            std::sync::Arc::new(|| {}),
            &[],
        )
        .expect("search key test should spawn its shell");
        assert_eq!(pane.handle_search_key(SearchKey::Char('x')), None);

        pane.search = Some(SearchState::new(String::new()));
        assert_eq!(
            pane.handle_search_key(SearchKey::Char('q')),
            Some(SearchKeyOutcome::Changed)
        );
        assert_eq!(pane.search.as_ref().unwrap().query, "q");
        assert_eq!(
            pane.handle_search_key(SearchKey::Escape),
            Some(SearchKeyOutcome::Close {
                last_query: "q".to_string()
            })
        );
        assert!(pane.search.is_none());
        assert_eq!(pane.last_search_query, "q");
        assert_eq!(pane.handle_search_key(SearchKey::Enter), None);
        pane.retire();
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
            std::sync::Arc::new(|| {}),
            &[],
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
