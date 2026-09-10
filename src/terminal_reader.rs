use crate::grid::{Grid, TerminalEvent};
use crate::parser::ansi::GraphicsSupport;
use crate::pty::{CancellableWriteOutcome, Pty};
use crate::terminal_decoder::TerminalDecoder;
use std::io;
use std::net::Shutdown;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

const READ_BUFFER_SIZE: usize = 4096;
const MAX_READS_PER_BATCH: usize = 16;

trait ReaderIo: Send + Sync + 'static {
    fn wait_readable(&self, cancellation: BorrowedFd<'_>) -> io::Result<bool>;
    fn read(&self, buffer: &mut [u8]) -> io::Result<usize>;
    fn write_all_cancellable(
        &self,
        data: &[u8],
        cancelled: &AtomicBool,
    ) -> io::Result<CancellableWriteOutcome>;
}

impl ReaderIo for Pty {
    fn wait_readable(&self, cancellation: BorrowedFd<'_>) -> io::Result<bool> {
        Pty::wait_readable_or_cancelled(self, cancellation)
    }

    fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        Pty::read(self, buffer)
    }

    fn write_all_cancellable(
        &self,
        data: &[u8],
        cancelled: &AtomicBool,
    ) -> io::Result<CancellableWriteOutcome> {
        Pty::write_all_cancellable(self, data, cancelled)
    }
}

#[derive(Debug)]
pub(crate) enum ReaderExit {
    Shutdown,
    Eof,
    WaitFailed(io::Error),
    ReadFailed(io::Error),
    ResponseWriteFailed(io::Error),
    GridPoisoned,
    DecoderStalled,
    UnexpectedProtocolEvent,
}

/// Handle for a single background PTY reader.
pub(crate) struct TerminalReader {
    shutdown: Arc<AtomicBool>,
    shutdown_signal: UnixStream,
    handle: Option<JoinHandle<ReaderExit>>,
}

impl TerminalReader {
    pub(crate) fn spawn_text<U, X>(
        pty: Arc<Pty>,
        grid: Arc<Mutex<Grid>>,
        on_update: U,
        on_exit: X,
    ) -> io::Result<Self>
    where
        U: FnMut() + Send + 'static,
        X: FnOnce(&ReaderExit) + Send + 'static,
    {
        Self::spawn_with_io(
            pty,
            grid,
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            on_update,
            on_exit,
        )
    }

    /// Start a reader with ordered graphics handling under the grid lock.
    /// The callback returns an optional PTY reply, written after releasing the lock.
    pub(crate) fn spawn_with_graphics<G, U, X>(
        pty: Arc<Pty>,
        grid: Arc<Mutex<Grid>>,
        graphics_support: GraphicsSupport,
        on_graphics: G,
        on_update: U,
        on_exit: X,
    ) -> io::Result<Self>
    where
        G: FnMut(TerminalEvent, &mut Grid) -> Result<Option<Vec<u8>>, ReaderExit> + Send + 'static,
        U: FnMut() + Send + 'static,
        X: FnOnce(&ReaderExit) + Send + 'static,
    {
        Self::spawn_with_graphics_io(
            pty,
            grid,
            graphics_support,
            on_graphics,
            on_update,
            on_exit,
        )
    }

    fn spawn_with_io<I, U, X>(
        io: Arc<I>,
        grid: Arc<Mutex<Grid>>,
        graphics_support: GraphicsSupport,
        on_update: U,
        on_exit: X,
    ) -> io::Result<Self>
    where
        I: ReaderIo,
        U: FnMut() + Send + 'static,
        X: FnOnce(&ReaderExit) + Send + 'static,
    {
        Self::spawn_with_graphics_io(
            io,
            grid,
            graphics_support,
            |_, _| Err(ReaderExit::UnexpectedProtocolEvent),
            on_update,
            on_exit,
        )
    }

    fn spawn_with_graphics_io<I, G, U, X>(
        io: Arc<I>,
        grid: Arc<Mutex<Grid>>,
        graphics_support: GraphicsSupport,
        mut on_graphics: G,
        mut on_update: U,
        on_exit: X,
    ) -> io::Result<Self>
    where
        I: ReaderIo,
        G: FnMut(TerminalEvent, &mut Grid) -> Result<Option<Vec<u8>>, ReaderExit> + Send + 'static,
        U: FnMut() + Send + 'static,
        X: FnOnce(&ReaderExit) + Send + 'static,
    {
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let (shutdown_signal, cancellation) = UnixStream::pair()?;
        let handle = thread::Builder::new()
            .name("terminal-reader".to_string())
            .spawn(move || {
                let mut decoder = TerminalDecoder::new(graphics_support);
                let exit = run_reader(
                    io.as_ref(),
                    grid.as_ref(),
                    worker_shutdown.as_ref(),
                    cancellation.as_fd(),
                    &mut decoder,
                    &mut on_graphics,
                    &mut on_update,
                );
                on_exit(&exit);
                exit
            })?;

        Ok(Self {
            shutdown,
            shutdown_signal,
            handle: Some(handle),
        })
    }

    pub(crate) fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        // EOF persistently wakes poll, including shutdown racing with entry to
        // the wait. No writes, buffer capacity, or blocking locks are involved.
        // A disconnected socket means the worker has already exited.
        let _ = self.shutdown_signal.shutdown(Shutdown::Write);
    }

    pub(crate) fn shutdown_and_join(mut self) -> thread::Result<ReaderExit> {
        self.request_shutdown();
        match self.handle.take() {
            Some(handle) => handle.join(),
            None => Ok(ReaderExit::Shutdown),
        }
    }
}

impl Drop for TerminalReader {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

fn run_reader<I, G, U>(
    io: &I,
    grid: &Mutex<Grid>,
    shutdown: &AtomicBool,
    cancellation: BorrowedFd<'_>,
    decoder: &mut TerminalDecoder,
    on_graphics: &mut G,
    on_update: &mut U,
) -> ReaderExit
where
    I: ReaderIo,
    G: FnMut(TerminalEvent, &mut Grid) -> Result<Option<Vec<u8>>, ReaderExit>,
    U: FnMut(),
{
    let mut buffer = [0u8; READ_BUFFER_SIZE];

    loop {
        if shutdown.load(Ordering::Acquire) {
            return ReaderExit::Shutdown;
        }

        match io.wait_readable(cancellation) {
            Ok(false) => return ReaderExit::Shutdown,
            Ok(true) => {}
            Err(error) => return ReaderExit::WaitFailed(error),
        }

        let (changed, exit) =
            read_ready_batch(io, grid, shutdown, decoder, on_graphics, &mut buffer);
        if changed {
            on_update();
        }
        if let Some(exit) = exit {
            return exit;
        }
    }
}

fn read_ready_batch<I, G>(
    io: &I,
    grid: &Mutex<Grid>,
    shutdown: &AtomicBool,
    decoder: &mut TerminalDecoder,
    on_graphics: &mut G,
    buffer: &mut [u8; READ_BUFFER_SIZE],
) -> (bool, Option<ReaderExit>)
where
    I: ReaderIo,
    G: FnMut(TerminalEvent, &mut Grid) -> Result<Option<Vec<u8>>, ReaderExit>,
{
    let mut changed = false;

    for _ in 0..MAX_READS_PER_BATCH {
        if shutdown.load(Ordering::Acquire) {
            return (changed, Some(ReaderExit::Shutdown));
        }

        match io.read(buffer) {
            Ok(0) => return (changed, Some(ReaderExit::Eof)),
            Ok(read) => {
                changed = true;
                if let Err(exit) =
                    process_bytes(io, grid, shutdown, decoder, on_graphics, &buffer[..read])
                {
                    return (changed, Some(exit));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return (changed, None),
            Err(error) => return (changed, Some(ReaderExit::ReadFailed(error))),
        }
    }

    (changed, None)
}

fn process_bytes<I, G>(
    io: &I,
    grid: &Mutex<Grid>,
    shutdown: &AtomicBool,
    decoder: &mut TerminalDecoder,
    on_graphics: &mut G,
    input: &[u8],
) -> Result<(), ReaderExit>
where
    I: ReaderIo,
    G: FnMut(TerminalEvent, &mut Grid) -> Result<Option<Vec<u8>>, ReaderExit>,
{
    let mut offset = 0;
    while offset < input.len() {
        if shutdown.load(Ordering::Acquire) {
            return Err(ReaderExit::Shutdown);
        }

        let (consumed, responses) = {
            let mut grid = grid.lock().map_err(|_| ReaderExit::GridPoisoned)?;
            let step = decoder.feed_until_event(&input[offset..], &mut grid);
            let remaining = input.len() - offset;
            if step.consumed == 0 || step.consumed > remaining {
                return Err(ReaderExit::DecoderStalled);
            }

            let mut responses = Vec::new();
            for event in step.events {
                let response = match event {
                    TerminalEvent::Response(response) => Some(response),
                    graphics_event => on_graphics(graphics_event, &mut grid)?,
                };
                if let Some(response) = response {
                    responses.push(response);
                }
            }
            (step.consumed, responses)
        };
        offset += consumed;

        for response in responses {
            match io.write_all_cancellable(&response, shutdown) {
                Ok(CancellableWriteOutcome::Completed) => {}
                Ok(CancellableWriteOutcome::Cancelled) => return Err(ReaderExit::Shutdown),
                Err(error) => return Err(ReaderExit::ResponseWriteFailed(error)),
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ReaderExit, ReaderIo, TerminalReader, MAX_READS_PER_BATCH};
    use crate::grid::cell::Color;
    use crate::grid::{Grid, TerminalEvent};
    use crate::parser::ansi::GraphicsSupport;
    use crate::pty::CancellableWriteOutcome;
    use crate::terminal_decoder::TerminalDecoder;
    use nix::libc;
    use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
    use std::collections::VecDeque;
    use std::io;
    use std::os::fd::{AsFd, BorrowedFd};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    struct BlockingGate {
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl BlockingGate {
        fn wait(&self) {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .expect("test gate was not released");
        }
    }

    fn blocking_gate() -> (Arc<BlockingGate>, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        (
            Arc::new(BlockingGate {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            }),
            entered_rx,
            release_tx,
        )
    }

    enum WaitAction {
        Ready,
        Cancelled,
        WaitForCancellation(mpsc::SyncSender<()>),
        Error(i32),
    }

    enum ReadAction {
        Data(Vec<u8>),
        WouldBlock,
        BlockedWouldBlock(Arc<BlockingGate>),
        Eof,
        Error(i32),
    }

    type WriteCheck = Arc<dyn Fn(&[u8]) + Send + Sync>;

    struct FakeIo {
        waits: Mutex<VecDeque<WaitAction>>,
        reads: Mutex<VecDeque<ReadAction>>,
        writes: Mutex<Vec<Vec<u8>>>,
        write_check: Option<WriteCheck>,
        write_error: Option<i32>,
        cancel_response_write: bool,
        blocked_cancelled_response_write: Option<Arc<BlockingGate>>,
        wait_calls: AtomicUsize,
        read_calls: AtomicUsize,
    }

    impl FakeIo {
        fn new(waits: Vec<WaitAction>, reads: Vec<ReadAction>) -> Self {
            Self {
                waits: Mutex::new(waits.into()),
                reads: Mutex::new(reads.into()),
                writes: Mutex::new(Vec::new()),
                write_check: None,
                write_error: None,
                cancel_response_write: false,
                blocked_cancelled_response_write: None,
                wait_calls: AtomicUsize::new(0),
                read_calls: AtomicUsize::new(0),
            }
        }

        fn with_write_check(mut self, write_check: WriteCheck) -> Self {
            self.write_check = Some(write_check);
            self
        }

        fn with_write_error(mut self, error: i32) -> Self {
            self.write_error = Some(error);
            self
        }

        fn with_cancelled_response_write(mut self) -> Self {
            self.cancel_response_write = true;
            self
        }

        fn with_blocked_cancelled_response_write(mut self, gate: Arc<BlockingGate>) -> Self {
            self.blocked_cancelled_response_write = Some(gate);
            self
        }

        fn writes(&self) -> Vec<Vec<u8>> {
            self.writes.lock().unwrap().clone()
        }
    }

    impl ReaderIo for FakeIo {
        fn wait_readable(&self, cancellation: BorrowedFd<'_>) -> io::Result<bool> {
            self.wait_calls.fetch_add(1, Ordering::Relaxed);
            match self.waits.lock().unwrap().pop_front() {
                Some(WaitAction::Ready) => Ok(true),
                Some(WaitAction::Cancelled) => Ok(false),
                Some(WaitAction::WaitForCancellation(entered)) => {
                    let mut fds = [PollFd::new(cancellation, PollFlags::POLLIN)];
                    entered.send(()).unwrap();
                    loop {
                        match poll(&mut fds, PollTimeout::NONE) {
                            Err(nix::errno::Errno::EINTR) => continue,
                            Ok(1) => break,
                            result => panic!("cancellation wait failed: {result:?}"),
                        }
                    }
                    Ok(false)
                }
                Some(WaitAction::Error(error)) => Err(io::Error::from_raw_os_error(error)),
                None => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "fake wait script exhausted",
                )),
            }
        }

        fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
            self.read_calls.fetch_add(1, Ordering::Relaxed);
            match self.reads.lock().unwrap().pop_front() {
                Some(ReadAction::Data(data)) => {
                    assert!(data.len() <= buffer.len());
                    buffer[..data.len()].copy_from_slice(&data);
                    Ok(data.len())
                }
                Some(ReadAction::WouldBlock) => Err(io::Error::from(io::ErrorKind::WouldBlock)),
                Some(ReadAction::BlockedWouldBlock(gate)) => {
                    gate.wait();
                    Err(io::Error::from(io::ErrorKind::WouldBlock))
                }
                Some(ReadAction::Eof) => Ok(0),
                Some(ReadAction::Error(error)) => Err(io::Error::from_raw_os_error(error)),
                None => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "fake read script exhausted",
                )),
            }
        }

        fn write_all_cancellable(
            &self,
            data: &[u8],
            cancelled: &AtomicBool,
        ) -> io::Result<CancellableWriteOutcome> {
            if let Some(check) = &self.write_check {
                check(data);
            }
            if let Some(error) = self.write_error {
                return Err(io::Error::from_raw_os_error(error));
            }
            if self.cancel_response_write {
                cancelled.store(true, Ordering::Release);
                return Ok(CancellableWriteOutcome::Cancelled);
            }
            if let Some(gate) = &self.blocked_cancelled_response_write {
                gate.wait();
                if cancelled.load(Ordering::Acquire) {
                    return Ok(CancellableWriteOutcome::Cancelled);
                }
            }
            self.writes.lock().unwrap().push(data.to_vec());
            Ok(CancellableWriteOutcome::Completed)
        }
    }

    fn text_decoder() -> TerminalDecoder {
        TerminalDecoder::new(GraphicsSupport {
            kitty: false,
            sixel: false,
        })
    }

    fn grid() -> Arc<Mutex<Grid>> {
        Arc::new(Mutex::new(Grid::new(80, 8, 32)))
    }

    fn run_reader<I: ReaderIo, U: FnMut()>(
        io: &I,
        grid: &Mutex<Grid>,
        shutdown: &AtomicBool,
        decoder: &mut TerminalDecoder,
        on_update: &mut U,
    ) -> ReaderExit {
        run_reader_with_graphics(
            io,
            grid,
            shutdown,
            decoder,
            &mut |_, _| Err(ReaderExit::UnexpectedProtocolEvent),
            on_update,
        )
    }

    fn run_reader_with_graphics<I, G, U>(
        io: &I,
        grid: &Mutex<Grid>,
        shutdown: &AtomicBool,
        decoder: &mut TerminalDecoder,
        on_graphics: &mut G,
        on_update: &mut U,
    ) -> ReaderExit
    where
        I: ReaderIo,
        G: FnMut(TerminalEvent, &mut Grid) -> Result<Option<Vec<u8>>, ReaderExit>,
        U: FnMut(),
    {
        let (_signal, cancellation) = UnixStream::pair().unwrap();
        super::run_reader(
            io,
            grid,
            shutdown,
            cancellation.as_fd(),
            decoder,
            on_graphics,
            on_update,
        )
    }

    #[test]
    fn preserves_utf8_and_ansi_state_across_reads() {
        let fake = FakeIo::new(
            vec![WaitAction::Ready],
            vec![
                ReadAction::Data(b"\x1b[31m\xe6\x97".to_vec()),
                ReadAction::Data(b"\xa5".to_vec()),
                ReadAction::Eof,
            ],
        );
        let grid = grid();
        let shutdown = AtomicBool::new(false);
        let mut decoder = text_decoder();
        let mut updates = 0;

        let exit = run_reader(&fake, &grid, &shutdown, &mut decoder, &mut || updates += 1);

        assert!(matches!(exit, ReaderExit::Eof));
        assert_eq!(updates, 1);
        let grid = grid.lock().unwrap();
        assert_eq!(grid.buffer.cell(0, 0).c, '日');
        assert_eq!(grid.buffer.cell(0, 0).fg, Color::Indexed(1));
        assert_eq!(grid.cursor_col, 2);
    }

    #[test]
    fn writes_responses_outside_the_grid_lock_before_trailing_text() {
        let grid = grid();
        let checked_grid = grid.clone();
        let fake = FakeIo::new(
            vec![WaitAction::Ready],
            vec![ReadAction::Data(b"A\x1b[6nB".to_vec()), ReadAction::Eof],
        )
        .with_write_check(Arc::new(move |response| {
            assert_eq!(response, b"\x1b[1;2R");
            let grid = checked_grid
                .try_lock()
                .expect("response writes must not hold the grid lock");
            assert_eq!(grid.buffer.cell(0, 0).c, 'A');
            assert_eq!(grid.buffer.cell(0, 1).c, ' ');
        }));
        let shutdown = AtomicBool::new(false);
        let mut decoder = text_decoder();

        let exit = run_reader(&fake, &grid, &shutdown, &mut decoder, &mut || {});

        assert!(matches!(exit, ReaderExit::Eof));
        assert_eq!(fake.writes(), [b"\x1b[1;2R".to_vec()]);
        assert_eq!(grid.lock().unwrap().buffer.cell(0, 1).c, 'B');
    }

    #[test]
    fn graphics_callbacks_advance_before_later_events_and_write_replies_without_grid_lock() {
        const KITTY_REPLY: &[u8] = b"\x1b_Gi=7;OK\x1b\\";
        let grid = grid();
        let checked_grid = grid.clone();
        let callback_grid = grid.clone();
        let fake = FakeIo::new(
            vec![WaitAction::Ready],
            vec![
                ReadAction::Data(
                    b"\x1b[2;3H\x1bPq~\x1b\\\x1b_Ga=p,i=7\x1b\\\x1b[6nX".to_vec(),
                ),
                ReadAction::Eof,
            ],
        )
        .with_write_check(Arc::new(move |response| {
            assert!(response == KITTY_REPLY || response == b"\x1b[4;5R");
            let grid = checked_grid
                .try_lock()
                .expect("graphics and cursor replies must release the grid lock");
            assert_eq!((grid.cursor_row, grid.cursor_col), (3, 4));
            assert_eq!(grid.buffer.cell(3, 4).c, ' ');
        }));
        let mut decoder = TerminalDecoder::new(GraphicsSupport {
            kitty: true,
            sixel: true,
        });
        let mut events = Vec::new();
        let mut updates = 0;

        let exit = run_reader_with_graphics(
            &fake,
            &grid,
            &AtomicBool::new(false),
            &mut decoder,
            &mut |event, grid| {
                assert!(matches!(
                    callback_grid.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ));
                match event {
                    TerminalEvent::SixelGraphics {
                        image,
                        cursor_row,
                        cursor_col,
                    } => {
                        assert_eq!((image.width, image.height), (1, 6));
                        assert_eq!((cursor_row, cursor_col), (1, 2));
                        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 2));
                        grid.newline();
                        events.push("sixel");
                        Ok(None)
                    }
                    TerminalEvent::KittyGraphics {
                        command,
                        cursor_row,
                        cursor_col,
                    } => {
                        assert_eq!(command.image_id, Some(7));
                        assert_eq!((cursor_row, cursor_col), (2, 2));
                        assert_eq!((grid.cursor_row, grid.cursor_col), (2, 2));
                        grid.set_cursor_pos(3, 4);
                        events.push("kitty");
                        Ok(Some(KITTY_REPLY.to_vec()))
                    }
                    TerminalEvent::Response(_) => panic!("responses bypass the graphics callback"),
                }
            },
            &mut || updates += 1,
        );

        assert!(matches!(exit, ReaderExit::Eof));
        assert_eq!(events, ["sixel", "kitty"]);
        assert_eq!(updates, 1);
        assert_eq!(fake.writes(), [KITTY_REPLY.to_vec(), b"\x1b[4;5R".to_vec()]);
        assert_eq!(grid.lock().unwrap().buffer.cell(3, 4).c, 'X');
    }

    #[test]
    fn disabled_graphics_skip_callbacks_and_keep_text_and_capability_responses() {
        let fake = FakeIo::new(
            vec![WaitAction::Ready],
            vec![
                ReadAction::Data(b"\x1bPq~\x1b\\\x1b_Ga=p,i=7\x1b\\X\x1b[c".to_vec()),
                ReadAction::Eof,
            ],
        );
        let grid = grid();
        let exit = run_reader_with_graphics(
            &fake,
            &grid,
            &AtomicBool::new(false),
            &mut text_decoder(),
            &mut |_, _| panic!("disabled graphics must not reach the callback"),
            &mut || {},
        );

        assert!(matches!(exit, ReaderExit::Eof));
        assert_eq!(fake.writes(), [b"\x1b[?62;22c".to_vec()]);
        assert_eq!(grid.lock().unwrap().buffer.cell(0, 0).c, 'X');
    }

    #[test]
    fn graphics_callback_error_stops_before_trailing_text_and_releases_grid() {
        let fake = FakeIo::new(
            vec![WaitAction::Ready],
            vec![ReadAction::Data(b"\x1bPq~\x1b\\X".to_vec())],
        );
        let grid = grid();
        let mut decoder = TerminalDecoder::new(GraphicsSupport {
            kitty: false,
            sixel: true,
        });
        let mut updates = 0;

        let exit = run_reader_with_graphics(
            &fake,
            &grid,
            &AtomicBool::new(false),
            &mut decoder,
            &mut |_, _| Err(ReaderExit::GridPoisoned),
            &mut || updates += 1,
        );

        assert!(matches!(exit, ReaderExit::GridPoisoned));
        assert_eq!(updates, 1);
        assert!(fake.writes().is_empty());
        assert_eq!(grid.try_lock().unwrap().buffer.cell(0, 0).c, ' ');
    }

    #[test]
    fn graphics_reply_cancellation_or_failure_stops_before_trailing_text() {
        for cancelled in [true, false] {
            let fake = FakeIo::new(
                vec![WaitAction::Ready],
                vec![ReadAction::Data(b"\x1b_Ga=p,i=7\x1b\\X".to_vec())],
            );
            let fake = if cancelled {
                fake.with_cancelled_response_write()
            } else {
                fake.with_write_error(libc::EPIPE)
            };
            let grid = grid();
            let shutdown = AtomicBool::new(false);
            let mut decoder = TerminalDecoder::new(GraphicsSupport {
                kitty: true,
                sixel: false,
            });

            let exit = run_reader_with_graphics(
                &fake,
                &grid,
                &shutdown,
                &mut decoder,
                &mut |_, grid| {
                    grid.newline();
                    Ok(Some(b"\x1b_Gi=7;OK\x1b\\".to_vec()))
                },
                &mut || {},
            );

            if cancelled {
                assert!(matches!(exit, ReaderExit::Shutdown));
                assert!(shutdown.load(Ordering::Acquire));
            } else {
                assert!(matches!(exit, ReaderExit::ResponseWriteFailed(error)
                    if error.raw_os_error() == Some(libc::EPIPE)));
            }
            assert!(fake.writes().is_empty());
            let grid = grid.try_lock().unwrap();
            assert_eq!(grid.cursor_row, 1);
            assert_eq!(grid.buffer.cell(1, 0).c, ' ');
        }
    }

    #[test]
    fn cancelled_response_write_exits_before_trailing_text() {
        let fake = FakeIo::new(
            vec![WaitAction::Ready],
            vec![ReadAction::Data(b"A\x1b[6nB".to_vec())],
        )
        .with_cancelled_response_write();
        let grid = grid();
        let shutdown = AtomicBool::new(false);
        let mut decoder = text_decoder();
        let mut updates = 0;

        let exit = run_reader(&fake, &grid, &shutdown, &mut decoder, &mut || updates += 1);

        assert!(matches!(exit, ReaderExit::Shutdown));
        assert!(shutdown.load(Ordering::Acquire));
        assert_eq!(updates, 1);
        assert!(fake.writes().is_empty());
        let grid = grid.lock().unwrap();
        assert_eq!(grid.buffer.cell(0, 0).c, 'A');
        assert_eq!(grid.buffer.cell(0, 1).c, ' ');
    }

    #[test]
    fn request_shutdown_cancels_a_blocked_response_before_trailing_text() {
        let (write_gate, entered_write, release_write) = blocking_gate();
        let fake = Arc::new(
            FakeIo::new(
                vec![WaitAction::Ready],
                vec![ReadAction::Data(b"A\x1b[6nB".to_vec())],
            )
            .with_blocked_cancelled_response_write(write_gate),
        );
        let grid = grid();
        let events = Arc::new(Mutex::new(Vec::new()));
        let update_events = events.clone();
        let exit_events = events.clone();

        let reader = TerminalReader::spawn_with_io(
            fake.clone(),
            grid.clone(),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            move || update_events.lock().unwrap().push("update"),
            move |exit| {
                assert!(matches!(exit, ReaderExit::Shutdown));
                exit_events.lock().unwrap().push("exit");
            },
        )
        .unwrap();

        entered_write
            .recv_timeout(Duration::from_secs(2))
            .expect("reader did not enter the blocked response write");
        reader.request_shutdown();
        release_write.send(()).unwrap();
        let exit = reader.shutdown_and_join().unwrap();

        assert!(matches!(exit, ReaderExit::Shutdown));
        assert_eq!(*events.lock().unwrap(), ["update", "exit"]);
        assert!(fake.writes().is_empty());
        let grid = grid.lock().unwrap();
        assert_eq!(grid.buffer.cell(0, 0).c, 'A');
        assert_eq!(grid.buffer.cell(0, 1).c, ' ');
    }

    #[test]
    fn final_update_precedes_exit_callback() {
        let fake = Arc::new(FakeIo::new(
            vec![WaitAction::Ready],
            vec![ReadAction::Data(b"X".to_vec()), ReadAction::Eof],
        ));
        let grid = grid();
        let events = Arc::new(Mutex::new(Vec::new()));
        let update_events = events.clone();
        let exit_events = events.clone();
        let update_grid = grid.clone();
        let exit_grid = grid.clone();
        let (exited_tx, exited_rx) = mpsc::sync_channel(1);

        let reader = TerminalReader::spawn_with_io(
            fake,
            grid,
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            move || {
                let _grid = update_grid
                    .try_lock()
                    .expect("update callback must not hold the grid lock");
                update_events.lock().unwrap().push("update");
            },
            move |exit| {
                let _grid = exit_grid
                    .try_lock()
                    .expect("exit callback must not hold the grid lock");
                assert!(matches!(exit, ReaderExit::Eof));
                exit_events.lock().unwrap().push("exit");
                exited_tx.send(()).unwrap();
            },
        )
        .unwrap();

        exited_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let exit = reader.shutdown_and_join().unwrap();

        assert!(matches!(exit, ReaderExit::Eof));
        assert_eq!(*events.lock().unwrap(), ["update", "exit"]);
    }

    #[test]
    fn final_update_precedes_read_error_callback() {
        let fake = Arc::new(FakeIo::new(
            vec![WaitAction::Ready],
            vec![
                ReadAction::Data(b"X".to_vec()),
                ReadAction::Error(libc::EIO),
            ],
        ));
        let grid = grid();
        let events = Arc::new(Mutex::new(Vec::new()));
        let update_events = events.clone();
        let exit_events = events.clone();
        let update_grid = grid.clone();
        let exit_grid = grid.clone();
        let (exited_tx, exited_rx) = mpsc::sync_channel(1);

        let reader = TerminalReader::spawn_with_io(
            fake,
            grid,
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            move || {
                let _grid = update_grid
                    .try_lock()
                    .expect("update callback must not hold the grid lock");
                update_events.lock().unwrap().push("update");
            },
            move |exit| {
                let _grid = exit_grid
                    .try_lock()
                    .expect("exit callback must not hold the grid lock");
                let ReaderExit::ReadFailed(error) = exit else {
                    panic!("unexpected reader exit: {exit:?}");
                };
                assert_eq!(error.raw_os_error(), Some(libc::EIO));
                exit_events.lock().unwrap().push("exit");
                exited_tx.send(()).unwrap();
            },
        )
        .unwrap();

        exited_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let exit = reader.shutdown_and_join().unwrap();

        let ReaderExit::ReadFailed(error) = exit else {
            panic!("unexpected reader exit: {exit:?}");
        };
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert_eq!(*events.lock().unwrap(), ["update", "exit"]);
    }

    #[test]
    fn cancelled_wait_exits_without_reading() {
        let shutdown = AtomicBool::new(false);
        let fake = FakeIo::new(vec![WaitAction::Cancelled], Vec::new());
        let mut decoder = text_decoder();
        let mut updates = 0;

        let exit = run_reader(&fake, &grid(), &shutdown, &mut decoder, &mut || {
            updates += 1
        });

        assert!(matches!(exit, ReaderExit::Shutdown));
        assert_eq!(updates, 0);
        assert_eq!(fake.wait_calls.load(Ordering::Relaxed), 1);
        assert_eq!(fake.read_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn request_shutdown_flushes_the_batch_before_exiting() {
        let (read_gate, entered_read, release_read) = blocking_gate();
        let fake = Arc::new(FakeIo::new(
            vec![WaitAction::Ready],
            vec![
                ReadAction::Data(b"X".to_vec()),
                ReadAction::BlockedWouldBlock(read_gate),
            ],
        ));
        let grid = grid();
        let events = Arc::new(Mutex::new(Vec::new()));
        let update_events = events.clone();
        let exit_events = events.clone();
        let update_grid = grid.clone();
        let exit_grid = grid.clone();

        let reader = TerminalReader::spawn_with_io(
            fake,
            grid.clone(),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            move || {
                let _grid = update_grid
                    .try_lock()
                    .expect("update callback must not hold the grid lock");
                update_events.lock().unwrap().push("update");
            },
            move |exit| {
                let _grid = exit_grid
                    .try_lock()
                    .expect("exit callback must not hold the grid lock");
                assert!(matches!(exit, ReaderExit::Shutdown));
                exit_events.lock().unwrap().push("exit");
            },
        )
        .unwrap();

        entered_read
            .recv_timeout(Duration::from_secs(2))
            .expect("reader did not reach the blocked read");
        reader.request_shutdown();
        release_read.send(()).unwrap();
        let exit = reader.shutdown_and_join().unwrap();

        assert!(matches!(exit, ReaderExit::Shutdown));
        assert_eq!(*events.lock().unwrap(), ["update", "exit"]);
        assert_eq!(grid.lock().unwrap().buffer.cell(0, 0).c, 'X');
    }

    #[test]
    fn drop_requests_shutdown_for_a_waiting_reader() {
        let (entered_tx, entered_wait) = mpsc::sync_channel(1);
        let fake = Arc::new(FakeIo::new(
            vec![WaitAction::WaitForCancellation(entered_tx)],
            Vec::new(),
        ));
        let (exited_tx, exited_rx) = mpsc::sync_channel(1);

        let reader = TerminalReader::spawn_with_io(
            fake,
            grid(),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            || panic!("an idle reader must not emit an update"),
            move |exit| {
                exited_tx
                    .send(matches!(exit, ReaderExit::Shutdown))
                    .unwrap();
            },
        )
        .unwrap();

        entered_wait
            .recv_timeout(Duration::from_secs(2))
            .expect("reader did not reach the blocked wait");
        drop(reader);

        assert!(exited_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    }

    #[test]
    fn idle_reader_stays_asleep_until_shutdown_and_joins() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let fake = Arc::new(FakeIo::new(
            vec![WaitAction::WaitForCancellation(entered_tx)],
            Vec::new(),
        ));
        let (exited_tx, exited_rx) = mpsc::sync_channel(1);
        let reader = TerminalReader::spawn_with_io(
            fake.clone(),
            grid(),
            GraphicsSupport {
                kitty: false,
                sixel: false,
            },
            || panic!("an idle reader must not emit an update"),
            move |exit| {
                exited_tx
                    .send(matches!(exit, ReaderExit::Shutdown))
                    .unwrap();
            },
        )
        .unwrap();

        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            exited_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(fake.wait_calls.load(Ordering::Relaxed), 1);
        assert_eq!(fake.read_calls.load(Ordering::Relaxed), 0);

        // Repeated shutdown does not queue bytes or require the worker to drain
        // a pipe. This also exercises the request immediately before join.
        reader.request_shutdown();
        reader.request_shutdown();
        assert!(exited_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(matches!(
            reader.shutdown_and_join().unwrap(),
            ReaderExit::Shutdown
        ));
        assert_eq!(Arc::strong_count(&fake), 1);
    }

    #[test]
    fn would_block_and_cancellation_do_not_emit_false_updates() {
        let shutdown = AtomicBool::new(false);
        let fake = FakeIo::new(
            vec![WaitAction::Ready, WaitAction::Cancelled],
            vec![ReadAction::WouldBlock],
        );
        let mut decoder = text_decoder();
        let mut updates = 0;

        let exit = run_reader(&fake, &grid(), &shutdown, &mut decoder, &mut || {
            updates += 1
        });

        assert!(matches!(exit, ReaderExit::Shutdown));
        assert_eq!(updates, 0);
        assert_eq!(fake.wait_calls.load(Ordering::Relaxed), 2);
        assert_eq!(fake.read_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn limits_and_coalesces_reads_per_batch() {
        let reads = (0..MAX_READS_PER_BATCH + 1)
            .map(|_| ReadAction::Data(b"x".to_vec()))
            .chain(std::iter::once(ReadAction::Eof))
            .collect();
        let fake = FakeIo::new(vec![WaitAction::Ready, WaitAction::Ready], reads);
        let shutdown = AtomicBool::new(false);
        let mut decoder = text_decoder();
        let mut updates = 0;

        let exit = run_reader(&fake, &grid(), &shutdown, &mut decoder, &mut || {
            updates += 1
        });

        assert!(matches!(exit, ReaderExit::Eof));
        assert_eq!(updates, 2);
        assert_eq!(
            fake.read_calls.load(Ordering::Relaxed),
            MAX_READS_PER_BATCH + 2
        );
    }

    #[test]
    fn preserves_wait_read_and_response_write_errors() {
        let cases = [
            (
                FakeIo::new(vec![WaitAction::Error(libc::EIO)], Vec::new()),
                "wait",
            ),
            (
                FakeIo::new(vec![WaitAction::Ready], vec![ReadAction::Error(libc::EIO)]),
                "read",
            ),
            (
                FakeIo::new(
                    vec![WaitAction::Ready],
                    vec![ReadAction::Data(b"\x1b[6n".to_vec())],
                )
                .with_write_error(libc::EPIPE),
                "write",
            ),
        ];

        for (fake, expected) in cases {
            let mut decoder = text_decoder();
            let exit = run_reader(
                &fake,
                &grid(),
                &AtomicBool::new(false),
                &mut decoder,
                &mut || {},
            );

            match (expected, exit) {
                ("wait", ReaderExit::WaitFailed(error))
                | ("read", ReaderExit::ReadFailed(error)) => {
                    assert_eq!(error.raw_os_error(), Some(libc::EIO));
                }
                ("write", ReaderExit::ResponseWriteFailed(error)) => {
                    assert_eq!(error.raw_os_error(), Some(libc::EPIPE));
                }
                (_, other) => panic!("unexpected reader exit: {other:?}"),
            }
        }
    }

    #[test]
    fn reports_poisoned_grid_without_panicking() {
        let grid = grid();
        let poisoned_grid = grid.clone();
        let poison_result = std::thread::spawn(move || {
            let _guard = poisoned_grid.lock().unwrap();
            panic!("poison grid for reader test");
        })
        .join();
        assert!(poison_result.is_err());
        let fake = FakeIo::new(
            vec![WaitAction::Ready],
            vec![ReadAction::Data(b"X".to_vec())],
        );
        let mut decoder = text_decoder();
        let mut updates = 0;

        let exit = run_reader(
            &fake,
            &grid,
            &AtomicBool::new(false),
            &mut decoder,
            &mut || updates += 1,
        );

        assert!(matches!(exit, ReaderExit::GridPoisoned));
        assert_eq!(updates, 1);
    }

    #[test]
    fn rejects_unexpected_graphics_events() {
        let fake = FakeIo::new(
            vec![WaitAction::Ready],
            vec![ReadAction::Data(b"\x1b_Ga=p,i=7,c=1,r=1\x1b\\".to_vec())],
        );
        let mut decoder = TerminalDecoder::new(GraphicsSupport {
            kitty: true,
            sixel: false,
        });

        let exit = run_reader(
            &fake,
            &grid(),
            &AtomicBool::new(false),
            &mut decoder,
            &mut || {},
        );

        assert!(matches!(exit, ReaderExit::UnexpectedProtocolEvent));
    }

    #[test]
    fn rejects_unexpected_sixel_events() {
        let fake = FakeIo::new(
            vec![WaitAction::Ready],
            vec![ReadAction::Data(b"\x1bPq~\x1b\\".to_vec())],
        );
        let mut decoder = TerminalDecoder::new(GraphicsSupport {
            kitty: false,
            sixel: true,
        });

        let exit = run_reader(
            &fake,
            &grid(),
            &AtomicBool::new(false),
            &mut decoder,
            &mut || {},
        );

        assert!(matches!(exit, ReaderExit::UnexpectedProtocolEvent));
    }
}
