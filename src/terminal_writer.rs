use crate::pty::{CancellableWriteOutcome, Pty};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use thiserror::Error;

const TERMINAL_INPUT_QUEUE_CAPACITY: usize = 256;
// Match 256 queue slots against the largest explicitly bounded atomic input
// (the 64 KiB Linux IME commit) while bounding retained payload allocations
// per PTY.
const TERMINAL_INPUT_QUEUE_MAX_BYTES: usize = TERMINAL_INPUT_QUEUE_CAPACITY * 64 * 1024;

#[derive(Clone, Copy)]
struct QueueLimits {
    messages: usize,
    bytes: usize,
}

const TERMINAL_INPUT_QUEUE_LIMITS: QueueLimits = QueueLimits {
    messages: TERMINAL_INPUT_QUEUE_CAPACITY,
    bytes: TERMINAL_INPUT_QUEUE_MAX_BYTES,
};

trait WriterIo: Send + Sync + 'static {
    fn write_all_cancellable(
        &self,
        data: &[u8],
        shutdown: &AtomicBool,
    ) -> io::Result<CancellableWriteOutcome>;
}

impl WriterIo for Pty {
    fn write_all_cancellable(
        &self,
        data: &[u8],
        shutdown: &AtomicBool,
    ) -> io::Result<CancellableWriteOutcome> {
        Pty::write_all_cancellable(self, data, shutdown)
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalWriteQueueError {
    #[error("the terminal input queue is full")]
    Full,
    #[error("the terminal input queue is disconnected")]
    Disconnected,
}

#[derive(Debug)]
pub(crate) enum WriterExit {
    Shutdown,
    ChannelClosed,
    WriteFailed(io::Error),
}

struct ByteReservation {
    outstanding_bytes: Arc<AtomicUsize>,
    reserved_bytes: usize,
}

impl ByteReservation {
    fn try_acquire(
        outstanding_bytes: &Arc<AtomicUsize>,
        reserved_bytes: usize,
        max_bytes: usize,
    ) -> Option<Self> {
        outstanding_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(reserved_bytes)
                    .filter(|next| *next <= max_bytes)
            })
            .ok()?;
        Some(Self {
            outstanding_bytes: outstanding_bytes.clone(),
            reserved_bytes,
        })
    }
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        let previous = self
            .outstanding_bytes
            .fetch_sub(self.reserved_bytes, Ordering::AcqRel);
        debug_assert!(previous >= self.reserved_bytes);
    }
}

struct BudgetedInput {
    // Keep payload first so its allocation is freed before the reservation.
    bytes: Vec<u8>,
    _reservation: ByteReservation,
}

pub(crate) struct TerminalWriter {
    sender: Option<SyncSender<BudgetedInput>>,
    outstanding_bytes: Arc<AtomicUsize>,
    max_outstanding_bytes: usize,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<WriterExit>>,
}

impl TerminalWriter {
    pub(crate) fn spawn<F>(pty: Arc<Pty>, on_exit: F) -> io::Result<Self>
    where
        F: FnOnce(&WriterExit) + Send + 'static,
    {
        Self::spawn_with_io(pty, TERMINAL_INPUT_QUEUE_LIMITS, on_exit)
    }

    fn spawn_with_io<I, F>(io: Arc<I>, limits: QueueLimits, on_exit: F) -> io::Result<Self>
    where
        I: WriterIo,
        F: FnOnce(&WriterExit) + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(limits.messages);
        let outstanding_bytes = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let handle = thread::Builder::new()
            .name("terminal-writer".to_string())
            .spawn(move || {
                let exit = run_writer(io.as_ref(), receiver, worker_shutdown.as_ref());
                on_exit(&exit);
                exit
            })?;

        Ok(Self {
            sender: Some(sender),
            outstanding_bytes,
            max_outstanding_bytes: limits.bytes,
            shutdown,
            handle: Some(handle),
        })
    }

    pub(crate) fn enqueue(&self, bytes: Vec<u8>) -> Result<(), TerminalWriteQueueError> {
        let sender = self
            .sender
            .as_ref()
            .ok_or(TerminalWriteQueueError::Disconnected)?;
        let reserved_bytes = bytes.capacity();
        let reservation = ByteReservation::try_acquire(
            &self.outstanding_bytes,
            reserved_bytes,
            self.max_outstanding_bytes,
        )
        .ok_or(TerminalWriteQueueError::Full)?;
        let input = BudgetedInput {
            bytes,
            _reservation: reservation,
        };
        match sender.try_send(input) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(TerminalWriteQueueError::Full),
            Err(TrySendError::Disconnected(_)) => Err(TerminalWriteQueueError::Disconnected),
        }
    }

    pub(crate) fn request_shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.sender.take();
    }

    pub(crate) fn shutdown_and_join(mut self) -> thread::Result<WriterExit> {
        self.request_shutdown();
        match self.handle.take() {
            Some(handle) => handle.join(),
            None => Ok(WriterExit::Shutdown),
        }
    }
}

impl Drop for TerminalWriter {
    fn drop(&mut self) {
        self.request_shutdown();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_writer<I: WriterIo>(
    io: &I,
    receiver: Receiver<BudgetedInput>,
    shutdown: &AtomicBool,
) -> WriterExit {
    loop {
        let input = match receiver.recv() {
            Ok(input) => input,
            Err(_) if shutdown.load(Ordering::Acquire) => return WriterExit::Shutdown,
            Err(_) => return WriterExit::ChannelClosed,
        };

        if shutdown.load(Ordering::Acquire) {
            return WriterExit::Shutdown;
        }

        match io.write_all_cancellable(&input.bytes, shutdown) {
            Ok(CancellableWriteOutcome::Completed) => {}
            Ok(CancellableWriteOutcome::Cancelled) => return WriterExit::Shutdown,
            Err(error) => return WriterExit::WriteFailed(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        run_writer, BudgetedInput, ByteReservation, QueueLimits, TerminalWriteQueueError,
        TerminalWriter, WriterExit, WriterIo,
    };
    use crate::pty::CancellableWriteOutcome;
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn limits(messages: usize, bytes: usize) -> QueueLimits {
        QueueLimits { messages, bytes }
    }

    fn wait_for_outstanding_bytes(writer: &TerminalWriter, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while writer.outstanding_bytes.load(Ordering::Acquire) != expected
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(writer.outstanding_bytes.load(Ordering::Acquire), expected);
    }

    struct RecordingIo {
        writes: Mutex<Vec<Vec<u8>>>,
        completed: mpsc::Sender<()>,
    }

    impl WriterIo for RecordingIo {
        fn write_all_cancellable(
            &self,
            data: &[u8],
            _shutdown: &AtomicBool,
        ) -> io::Result<CancellableWriteOutcome> {
            self.writes
                .lock()
                .expect("recording writer should remain available")
                .push(data.to_vec());
            self.completed
                .send(())
                .expect("FIFO test should still be waiting for writes");
            Ok(CancellableWriteOutcome::Completed)
        }
    }

    struct BlockingIo {
        started: mpsc::SyncSender<()>,
    }

    impl WriterIo for BlockingIo {
        fn write_all_cancellable(
            &self,
            _data: &[u8],
            shutdown: &AtomicBool,
        ) -> io::Result<CancellableWriteOutcome> {
            self.started
                .send(())
                .expect("blocking test should wait for the writer to start");
            while !shutdown.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            Ok(CancellableWriteOutcome::Cancelled)
        }
    }

    struct FailingIo;

    impl WriterIo for FailingIo {
        fn write_all_cancellable(
            &self,
            _data: &[u8],
            _shutdown: &AtomicBool,
        ) -> io::Result<CancellableWriteOutcome> {
            Err(io::Error::from_raw_os_error(5))
        }
    }

    struct GatedFailingIo {
        started: mpsc::SyncSender<()>,
        fail_now: AtomicBool,
    }

    impl WriterIo for GatedFailingIo {
        fn write_all_cancellable(
            &self,
            _data: &[u8],
            shutdown: &AtomicBool,
        ) -> io::Result<CancellableWriteOutcome> {
            self.started
                .send(())
                .expect("failure test should wait for the write to start");
            while !self.fail_now.load(Ordering::Acquire) {
                if shutdown.load(Ordering::Acquire) {
                    return Ok(CancellableWriteOutcome::Cancelled);
                }
                std::thread::yield_now();
            }
            Err(io::Error::from_raw_os_error(5))
        }
    }

    struct PanickingIo {
        started: mpsc::SyncSender<()>,
    }

    impl WriterIo for PanickingIo {
        fn write_all_cancellable(
            &self,
            _data: &[u8],
            _shutdown: &AtomicBool,
        ) -> io::Result<CancellableWriteOutcome> {
            self.started
                .send(())
                .expect("panic test should wait for the write to start");
            panic!("simulated writer panic");
        }
    }

    #[test]
    fn queued_writes_reach_the_worker_in_fifo_order() {
        let (completed_sender, completed_receiver) = mpsc::channel();
        let io = Arc::new(RecordingIo {
            writes: Mutex::new(Vec::new()),
            completed: completed_sender,
        });
        let mut writer = TerminalWriter::spawn_with_io(io.clone(), limits(4, 64), |_| {})
            .expect("recording writer should spawn");

        writer.enqueue(b"first".to_vec()).unwrap();
        writer.enqueue(b"second".to_vec()).unwrap();
        writer.enqueue(b"third".to_vec()).unwrap();
        for _ in 0..3 {
            completed_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("queued write should complete");
        }

        writer.request_shutdown();
        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
        assert_eq!(
            *io.writes.lock().unwrap(),
            [b"first".to_vec(), b"second".to_vec(), b"third".to_vec()]
        );
    }

    #[test]
    fn full_queue_returns_immediately_without_dropping_the_error() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let mut writer = TerminalWriter::spawn_with_io(
            Arc::new(BlockingIo {
                started: started_sender,
            }),
            limits(1, 64),
            |_| {},
        )
        .expect("blocking writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();

        let blocked = b"blocked".to_vec();
        let blocked_bytes = blocked.capacity();
        writer.enqueue(blocked).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first write should block in the fake I/O");
        let queued = b"queued".to_vec();
        let queued_bytes = queued.capacity();
        writer.enqueue(queued).unwrap();
        assert_eq!(
            writer.enqueue(b"overflow".to_vec()),
            Err(TerminalWriteQueueError::Full)
        );
        assert_eq!(
            outstanding_bytes.load(Ordering::Acquire),
            blocked_bytes + queued_bytes,
            "a channel-full send must release its byte reservation"
        );

        writer.request_shutdown();
        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn byte_budget_counts_in_flight_and_queued_allocations() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let first = vec![b'a'; 7];
        let second = vec![b'b'; 1];
        let byte_limit = first.capacity() + second.capacity();
        let writer = TerminalWriter::spawn_with_io(
            Arc::new(BlockingIo {
                started: started_sender,
            }),
            limits(4, byte_limit),
            |_| {},
        )
        .expect("byte-budget writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();

        writer.enqueue(first).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first budgeted write should remain in flight");
        writer.enqueue(second).unwrap();
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), byte_limit);
        assert_eq!(
            writer.enqueue(vec![b'c']),
            Err(TerminalWriteQueueError::Full)
        );
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), byte_limit);

        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn oversized_capacity_is_rejected_without_consuming_budget_or_slot() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let fitting = vec![0_u8; 8];
        let byte_limit = fitting.capacity();
        let writer = TerminalWriter::spawn_with_io(
            Arc::new(BlockingIo {
                started: started_sender,
            }),
            limits(1, byte_limit),
            |_| {},
        )
        .expect("oversized-input writer should spawn");

        let oversized = Vec::with_capacity(byte_limit + 1);
        assert_eq!(
            writer.enqueue(oversized),
            Err(TerminalWriteQueueError::Full)
        );
        wait_for_outstanding_bytes(&writer, 0);

        writer.enqueue(fitting).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("a fitting input should retain the unused channel slot");
        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
    }

    #[test]
    fn completed_write_releases_budget_for_the_next_input() {
        let (completed_sender, completed_receiver) = mpsc::channel();
        let first = b"first".to_vec();
        let byte_limit = first.capacity();
        let writer = TerminalWriter::spawn_with_io(
            Arc::new(RecordingIo {
                writes: Mutex::new(Vec::new()),
                completed: completed_sender,
            }),
            limits(1, byte_limit),
            |_| {},
        )
        .expect("budget-release writer should spawn");

        writer.enqueue(first).unwrap();
        completed_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first write should complete");
        wait_for_outstanding_bytes(&writer, 0);

        writer.enqueue(b"again".to_vec()).unwrap();
        completed_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("released budget should accept the next write");
        wait_for_outstanding_bytes(&writer, 0);
        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
    }

    #[test]
    fn shutdown_cancels_a_blocked_write_and_joins() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let mut writer = TerminalWriter::spawn_with_io(
            Arc::new(BlockingIo {
                started: started_sender,
            }),
            limits(1, 64),
            |_| {},
        )
        .expect("blocking writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();
        writer.enqueue(b"blocked".to_vec()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("fake write should start");

        writer.request_shutdown();
        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn shutdown_wakes_an_idle_writer_and_disconnects_enqueue() {
        let (completed_sender, _completed_receiver) = mpsc::channel();
        let mut writer = TerminalWriter::spawn_with_io(
            Arc::new(RecordingIo {
                writes: Mutex::new(Vec::new()),
                completed: completed_sender,
            }),
            limits(1, 64),
            |_| {},
        )
        .expect("idle writer should spawn");

        writer.request_shutdown();
        assert_eq!(
            writer.enqueue(b"late".to_vec()),
            Err(TerminalWriteQueueError::Disconnected)
        );
        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
    }

    #[test]
    fn drop_cancels_and_joins_a_blocked_write() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let (exit_sender, exit_receiver) = mpsc::sync_channel(1);
        let writer = TerminalWriter::spawn_with_io(
            Arc::new(BlockingIo {
                started: started_sender,
            }),
            limits(1, 64),
            move |exit| {
                exit_sender
                    .send(matches!(exit, WriterExit::Shutdown))
                    .expect("Drop test should observe the worker exit");
            },
        )
        .expect("blocking writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();
        writer.enqueue(b"blocked".to_vec()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("fake write should start");

        drop(writer);

        assert!(exit_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("Drop should join the cancelled worker"));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn native_write_error_reaches_the_callback_and_join_result() {
        let (exit_sender, exit_receiver) = mpsc::sync_channel(1);
        let writer =
            TerminalWriter::spawn_with_io(Arc::new(FailingIo), limits(1, 64), move |exit| {
                let raw_error = match exit {
                    WriterExit::WriteFailed(error) => error.raw_os_error(),
                    _ => None,
                };
                exit_sender
                    .send(raw_error)
                    .expect("error callback result should be observed");
            })
            .expect("failing writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();
        writer.enqueue(b"fail".to_vec()).unwrap();

        assert_eq!(
            exit_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("writer callback should report the failure"),
            Some(5)
        );
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        assert_eq!(
            writer.enqueue(b"late".to_vec()),
            Err(TerminalWriteQueueError::Disconnected)
        );
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        let WriterExit::WriteFailed(error) = writer.shutdown_and_join().unwrap() else {
            panic!("join should preserve the native write error");
        };
        assert_eq!(error.raw_os_error(), Some(5));
    }

    #[test]
    fn write_failure_releases_in_flight_and_queued_budget() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let io = Arc::new(GatedFailingIo {
            started: started_sender,
            fail_now: AtomicBool::new(false),
        });
        let writer = TerminalWriter::spawn_with_io(io.clone(), limits(2, 64), |_| {})
            .expect("gated failing writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();

        writer.enqueue(b"failing".to_vec()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("failing write should start");
        writer.enqueue(b"queued".to_vec()).unwrap();
        assert!(outstanding_bytes.load(Ordering::Acquire) > 0);

        io.fail_now.store(true, Ordering::Release);
        let WriterExit::WriteFailed(error) = writer.shutdown_and_join().unwrap() else {
            panic!("gated writer should preserve its native failure");
        };
        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn panic_unwind_releases_in_flight_budget() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let writer = TerminalWriter::spawn_with_io(
            Arc::new(PanickingIo {
                started: started_sender,
            }),
            limits(1, 64),
            |_| {},
        )
        .expect("panicking writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();

        writer.enqueue(b"panic".to_vec()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("panicking write should start");
        assert!(writer.shutdown_and_join().is_err());
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn byte_reservation_rejects_arithmetic_overflow() {
        let outstanding_bytes = Arc::new(AtomicUsize::new(0));
        let reservation = ByteReservation::try_acquire(&outstanding_bytes, usize::MAX, usize::MAX)
            .expect("the exact byte limit should be reservable");
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), usize::MAX);
        assert!(ByteReservation::try_acquire(&outstanding_bytes, 1, usize::MAX,).is_none());
        drop(reservation);
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn an_unexpected_sender_disconnect_is_distinct_from_shutdown() {
        let (_sender, receiver) = mpsc::sync_channel::<BudgetedInput>(1);
        drop(_sender);

        assert!(matches!(
            run_writer(&FailingIo, receiver, &AtomicBool::new(false)),
            WriterExit::ChannelClosed
        ));
    }
}
