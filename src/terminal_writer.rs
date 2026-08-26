use crate::pty::{CancellableWriteOutcome, Pty};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use thiserror::Error;

const TERMINAL_INPUT_QUEUE_CAPACITY: usize = 256;
pub(crate) const TERMINAL_INPUT_LOSSLESS_BYTE_HEADROOM: usize = 64 * 1024;
// Match 256 queue slots against the largest explicitly bounded atomic input
// (the 64 KiB Linux IME commit) while bounding retained payload allocations
// per PTY.
const TERMINAL_INPUT_QUEUE_MAX_BYTES: usize =
    TERMINAL_INPUT_QUEUE_CAPACITY * TERMINAL_INPUT_LOSSLESS_BYTE_HEADROOM;
const TERMINAL_INPUT_NONFATAL_MESSAGE_HEADROOM: usize = 1;
const TERMINAL_INPUT_NONFATAL_BYTE_HEADROOM: usize = TERMINAL_INPUT_LOSSLESS_BYTE_HEADROOM;

#[derive(Clone, Copy)]
struct QueueLimits {
    messages: usize,
    bytes: usize,
    nonfatal_message_headroom: usize,
    nonfatal_byte_headroom: usize,
}

impl QueueLimits {
    fn max_nonfatal_messages(self) -> usize {
        self.messages.saturating_sub(self.nonfatal_message_headroom)
    }

    fn max_nonfatal_bytes(self) -> usize {
        self.bytes.saturating_sub(self.nonfatal_byte_headroom)
    }
}

const TERMINAL_INPUT_QUEUE_LIMITS: QueueLimits = QueueLimits {
    messages: TERMINAL_INPUT_QUEUE_CAPACITY,
    bytes: TERMINAL_INPUT_QUEUE_MAX_BYTES,
    nonfatal_message_headroom: TERMINAL_INPUT_NONFATAL_MESSAGE_HEADROOM,
    nonfatal_byte_headroom: TERMINAL_INPUT_NONFATAL_BYTE_HEADROOM,
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

struct MessageReservation {
    outstanding_messages: Arc<AtomicUsize>,
}

impl MessageReservation {
    fn try_acquire(
        outstanding_messages: &Arc<AtomicUsize>,
        max_messages: Option<usize>,
    ) -> Option<Self> {
        outstanding_messages
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1).filter(|next| {
                    max_messages
                        .map(|max_messages| *next <= max_messages)
                        .unwrap_or(true)
                })
            })
            .ok()?;
        Some(Self {
            outstanding_messages: outstanding_messages.clone(),
        })
    }
}

impl Drop for MessageReservation {
    fn drop(&mut self) {
        let previous = self.outstanding_messages.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous >= 1);
    }
}

struct QueueReservation {
    _message: MessageReservation,
    _bytes: ByteReservation,
}

impl QueueReservation {
    fn try_acquire(
        outstanding_messages: &Arc<AtomicUsize>,
        max_messages: Option<usize>,
        outstanding_bytes: &Arc<AtomicUsize>,
        reserved_bytes: usize,
        max_bytes: usize,
    ) -> Option<Self> {
        let message = MessageReservation::try_acquire(outstanding_messages, max_messages)?;
        let bytes = ByteReservation::try_acquire(outstanding_bytes, reserved_bytes, max_bytes)?;
        Some(Self {
            _message: message,
            _bytes: bytes,
        })
    }
}

struct BudgetedInput {
    // Keep payload first so its allocation is freed before the reservation.
    bytes: Vec<u8>,
    _reservation: QueueReservation,
}

pub(crate) struct TerminalWriter {
    sender: Option<SyncSender<BudgetedInput>>,
    outstanding_bytes: Arc<AtomicUsize>,
    outstanding_messages: Arc<AtomicUsize>,
    max_outstanding_bytes: usize,
    max_nonfatal_outstanding_bytes: usize,
    max_nonfatal_outstanding_messages: usize,
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
        let outstanding_messages = Arc::new(AtomicUsize::new(0));
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
            outstanding_messages,
            max_outstanding_bytes: limits.bytes,
            max_nonfatal_outstanding_bytes: limits.max_nonfatal_bytes(),
            max_nonfatal_outstanding_messages: limits.max_nonfatal_messages(),
            shutdown,
            handle: Some(handle),
        })
    }

    pub(crate) fn enqueue(&self, bytes: Vec<u8>) -> Result<(), TerminalWriteQueueError> {
        self.enqueue_with_limits(bytes, self.max_outstanding_bytes, None)
    }

    pub(crate) fn enqueue_nonfatal(&self, bytes: Vec<u8>) -> Result<(), TerminalWriteQueueError> {
        self.enqueue_with_limits(
            bytes,
            self.max_nonfatal_outstanding_bytes,
            Some(self.max_nonfatal_outstanding_messages),
        )
    }

    fn enqueue_with_limits(
        &self,
        bytes: Vec<u8>,
        max_bytes: usize,
        max_messages: Option<usize>,
    ) -> Result<(), TerminalWriteQueueError> {
        let sender = self
            .sender
            .as_ref()
            .ok_or(TerminalWriteQueueError::Disconnected)?;
        let reserved_bytes = bytes.capacity();
        let reservation = QueueReservation::try_acquire(
            &self.outstanding_messages,
            max_messages,
            &self.outstanding_bytes,
            reserved_bytes,
            max_bytes,
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

    #[cfg(target_os = "macos")]
    pub(crate) fn max_nonfatal_input_bytes(&self) -> usize {
        self.max_nonfatal_outstanding_bytes
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
        TerminalWriter, WriterExit, WriterIo, TERMINAL_INPUT_NONFATAL_BYTE_HEADROOM,
        TERMINAL_INPUT_NONFATAL_MESSAGE_HEADROOM, TERMINAL_INPUT_QUEUE_CAPACITY,
        TERMINAL_INPUT_QUEUE_LIMITS, TERMINAL_INPUT_QUEUE_MAX_BYTES,
    };
    use crate::pty::CancellableWriteOutcome;
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn limits(messages: usize, bytes: usize) -> QueueLimits {
        limits_with_headroom(messages, bytes, 0, 0)
    }

    fn limits_with_headroom(
        messages: usize,
        bytes: usize,
        nonfatal_message_headroom: usize,
        nonfatal_byte_headroom: usize,
    ) -> QueueLimits {
        QueueLimits {
            messages,
            bytes,
            nonfatal_message_headroom,
            nonfatal_byte_headroom,
        }
    }

    #[test]
    fn production_nonfatal_limits_reserve_one_slot_and_64_kib() {
        assert_eq!(TERMINAL_INPUT_NONFATAL_MESSAGE_HEADROOM, 1);
        assert_eq!(TERMINAL_INPUT_NONFATAL_BYTE_HEADROOM, 64 * 1024);
        assert_eq!(
            TERMINAL_INPUT_QUEUE_LIMITS.max_nonfatal_messages(),
            TERMINAL_INPUT_QUEUE_CAPACITY - 1
        );
        assert_eq!(
            TERMINAL_INPUT_QUEUE_LIMITS.max_nonfatal_bytes(),
            TERMINAL_INPUT_QUEUE_MAX_BYTES - TERMINAL_INPUT_NONFATAL_BYTE_HEADROOM
        );
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

    fn wait_for_outstanding_messages(writer: &TerminalWriter, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while writer.outstanding_messages.load(Ordering::Acquire) != expected
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(
            writer.outstanding_messages.load(Ordering::Acquire),
            expected
        );
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

    struct GatedPanickingIo {
        started: mpsc::SyncSender<()>,
        about_to_panic: mpsc::SyncSender<()>,
        panic_now: AtomicBool,
    }

    impl WriterIo for GatedPanickingIo {
        fn write_all_cancellable(
            &self,
            _data: &[u8],
            shutdown: &AtomicBool,
        ) -> io::Result<CancellableWriteOutcome> {
            self.started
                .send(())
                .expect("panic test should wait for the write to start");
            while !self.panic_now.load(Ordering::Acquire) {
                if shutdown.load(Ordering::Acquire) {
                    return Ok(CancellableWriteOutcome::Cancelled);
                }
                std::thread::yield_now();
            }
            self.about_to_panic
                .send(())
                .expect("panic test should wait for the committed panic");
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
        wait_for_outstanding_bytes(&writer, 0);
        wait_for_outstanding_messages(&writer, 0);

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
    fn lossless_queue_accepts_one_in_flight_plus_all_channel_slots() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let mut writer = TerminalWriter::spawn_with_io(
            Arc::new(BlockingIo {
                started: started_sender,
            }),
            limits(3, 64),
            |_| {},
        )
        .expect("blocking writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();
        let outstanding_messages = writer.outstanding_messages.clone();

        let blocked = b"blocked".to_vec();
        let blocked_bytes = blocked.capacity();
        writer.enqueue(blocked).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first write should block in the fake I/O");
        let queued_one = b"queued-1".to_vec();
        let queued_one_bytes = queued_one.capacity();
        writer.enqueue(queued_one).unwrap();
        let queued_two = b"queued-2".to_vec();
        let queued_two_bytes = queued_two.capacity();
        writer.enqueue(queued_two).unwrap();
        let queued_three = b"queued-3".to_vec();
        let queued_three_bytes = queued_three.capacity();
        writer.enqueue(queued_three).unwrap();
        assert_eq!(
            writer.enqueue(b"overflow".to_vec()),
            Err(TerminalWriteQueueError::Full)
        );
        assert_eq!(
            outstanding_bytes.load(Ordering::Acquire),
            blocked_bytes + queued_one_bytes + queued_two_bytes + queued_three_bytes,
            "a channel-full send must release its byte reservation"
        );
        assert_eq!(
            outstanding_messages.load(Ordering::Acquire),
            4,
            "lossless input may use one in-flight write plus every channel slot"
        );

        writer.request_shutdown();
        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
    }

    #[test]
    fn nonfatal_queue_leaves_physical_capacity_for_lossless_input() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let writer = TerminalWriter::spawn_with_io(
            Arc::new(BlockingIo {
                started: started_sender,
            }),
            limits_with_headroom(3, 64, 1, 0),
            |_| {},
        )
        .expect("headroom writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();
        let outstanding_messages = writer.outstanding_messages.clone();

        writer.enqueue_nonfatal(vec![1]).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first nonfatal write should remain in flight");
        writer.enqueue_nonfatal(vec![2]).unwrap();
        assert_eq!(
            writer.enqueue_nonfatal(vec![3]),
            Err(TerminalWriteQueueError::Full)
        );
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 2);

        writer.enqueue(vec![4]).unwrap();
        writer.enqueue(vec![5]).unwrap();
        assert_eq!(writer.enqueue(vec![6]), Err(TerminalWriteQueueError::Full));
        assert_eq!(
            outstanding_messages.load(Ordering::Acquire),
            4,
            "lossless input should retain access to the physical channel capacity"
        );

        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
    }

    #[test]
    fn nonfatal_byte_limit_preserves_configured_headroom_for_lossless_input() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let writer = TerminalWriter::spawn_with_io(
            Arc::new(BlockingIo {
                started: started_sender,
            }),
            limits_with_headroom(3, 10, 0, 4),
            |_| {},
        )
        .expect("byte-headroom writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();
        let outstanding_messages = writer.outstanding_messages.clone();

        let nonfatal = vec![1; 6];
        assert_eq!(nonfatal.capacity(), 6);
        writer.enqueue_nonfatal(nonfatal).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("nonfatal write should retain its byte reservation");
        assert_eq!(
            writer.enqueue_nonfatal(vec![2]),
            Err(TerminalWriteQueueError::Full)
        );
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 6);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 1);

        let lossless = vec![3; 4];
        assert_eq!(lossless.capacity(), 4);
        writer.enqueue(lossless).unwrap();
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 10);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 2);

        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
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
        let outstanding_messages = writer.outstanding_messages.clone();

        writer.enqueue(first).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first budgeted write should remain in flight");
        writer.enqueue(second).unwrap();
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), byte_limit);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 2);
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
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
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
        wait_for_outstanding_messages(&writer, 0);

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
        wait_for_outstanding_messages(&writer, 0);

        writer.enqueue(b"again".to_vec()).unwrap();
        completed_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("released budget should accept the next write");
        wait_for_outstanding_bytes(&writer, 0);
        wait_for_outstanding_messages(&writer, 0);
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
        let outstanding_messages = writer.outstanding_messages.clone();
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
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
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
        let outstanding_bytes = writer.outstanding_bytes.clone();
        let outstanding_messages = writer.outstanding_messages.clone();

        writer.request_shutdown();
        assert_eq!(
            writer.enqueue(b"late".to_vec()),
            Err(TerminalWriteQueueError::Disconnected)
        );
        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
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
        let outstanding_messages = writer.outstanding_messages.clone();
        writer.enqueue(b"blocked".to_vec()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("fake write should start");

        drop(writer);

        assert!(exit_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("Drop should join the cancelled worker"));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
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
        let outstanding_messages = writer.outstanding_messages.clone();
        writer.enqueue(b"fail".to_vec()).unwrap();

        assert_eq!(
            exit_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("writer callback should report the failure"),
            Some(5)
        );
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
        assert_eq!(
            writer.enqueue(b"late".to_vec()),
            Err(TerminalWriteQueueError::Disconnected)
        );
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
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
        let outstanding_messages = writer.outstanding_messages.clone();

        writer.enqueue(b"failing".to_vec()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("failing write should start");
        writer.enqueue(b"queued".to_vec()).unwrap();
        assert!(outstanding_bytes.load(Ordering::Acquire) > 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 2);

        io.fail_now.store(true, Ordering::Release);
        let WriterExit::WriteFailed(error) = writer.shutdown_and_join().unwrap() else {
            panic!("gated writer should preserve its native failure");
        };
        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
    }

    #[test]
    fn panic_unwind_releases_in_flight_and_queued_budgets() {
        let (started_sender, started_receiver) = mpsc::sync_channel(0);
        let (about_to_panic_sender, about_to_panic_receiver) = mpsc::sync_channel(0);
        let io = Arc::new(GatedPanickingIo {
            started: started_sender,
            about_to_panic: about_to_panic_sender,
            panic_now: AtomicBool::new(false),
        });
        let writer = TerminalWriter::spawn_with_io(io.clone(), limits(1, 64), |_| {})
            .expect("panicking writer should spawn");
        let outstanding_bytes = writer.outstanding_bytes.clone();
        let outstanding_messages = writer.outstanding_messages.clone();

        writer.enqueue(b"panic".to_vec()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("panicking write should start");
        writer.enqueue(b"queued".to_vec()).unwrap();
        assert!(outstanding_bytes.load(Ordering::Acquire) > 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 2);

        io.panic_now.store(true, Ordering::Release);
        about_to_panic_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("writer should commit to panicking before shutdown begins");
        assert!(writer.shutdown_and_join().is_err());
        assert_eq!(outstanding_bytes.load(Ordering::Acquire), 0);
        assert_eq!(outstanding_messages.load(Ordering::Acquire), 0);
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
