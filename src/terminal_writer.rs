use crate::pty::{CancellableWriteOutcome, Pty};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use thiserror::Error;

const KEYBOARD_QUEUE_CAPACITY: usize = 256;

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
    #[error("the Linux terminal input queue is full")]
    Full,
    #[error("the Linux terminal input queue is disconnected")]
    Disconnected,
}

#[derive(Debug)]
pub(crate) enum WriterExit {
    Shutdown,
    ChannelClosed,
    WriteFailed(io::Error),
}

pub(crate) struct TerminalWriter {
    sender: Option<SyncSender<Vec<u8>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<WriterExit>>,
}

impl TerminalWriter {
    pub(crate) fn spawn<F>(pty: Arc<Pty>, on_exit: F) -> io::Result<Self>
    where
        F: FnOnce(&WriterExit) + Send + 'static,
    {
        Self::spawn_with_io(pty, KEYBOARD_QUEUE_CAPACITY, on_exit)
    }

    fn spawn_with_io<I, F>(io: Arc<I>, queue_capacity: usize, on_exit: F) -> io::Result<Self>
    where
        I: WriterIo,
        F: FnOnce(&WriterExit) + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(queue_capacity);
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
            shutdown,
            handle: Some(handle),
        })
    }

    pub(crate) fn enqueue(&self, bytes: Vec<u8>) -> Result<(), TerminalWriteQueueError> {
        let sender = self
            .sender
            .as_ref()
            .ok_or(TerminalWriteQueueError::Disconnected)?;
        match sender.try_send(bytes) {
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
    receiver: Receiver<Vec<u8>>,
    shutdown: &AtomicBool,
) -> WriterExit {
    loop {
        let bytes = match receiver.recv() {
            Ok(bytes) => bytes,
            Err(_) if shutdown.load(Ordering::Acquire) => return WriterExit::Shutdown,
            Err(_) => return WriterExit::ChannelClosed,
        };

        if shutdown.load(Ordering::Acquire) {
            return WriterExit::Shutdown;
        }

        match io.write_all_cancellable(&bytes, shutdown) {
            Ok(CancellableWriteOutcome::Completed) => {}
            Ok(CancellableWriteOutcome::Cancelled) => return WriterExit::Shutdown,
            Err(error) => return WriterExit::WriteFailed(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{run_writer, TerminalWriteQueueError, TerminalWriter, WriterExit, WriterIo};
    use crate::pty::CancellableWriteOutcome;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

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

    #[test]
    fn queued_writes_reach_the_worker_in_fifo_order() {
        let (completed_sender, completed_receiver) = mpsc::channel();
        let io = Arc::new(RecordingIo {
            writes: Mutex::new(Vec::new()),
            completed: completed_sender,
        });
        let mut writer = TerminalWriter::spawn_with_io(io.clone(), 4, |_| {})
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
            1,
            |_| {},
        )
        .expect("blocking writer should spawn");

        writer.enqueue(b"blocked".to_vec()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first write should block in the fake I/O");
        writer.enqueue(b"queued".to_vec()).unwrap();
        assert_eq!(
            writer.enqueue(b"overflow".to_vec()),
            Err(TerminalWriteQueueError::Full)
        );

        writer.request_shutdown();
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
            1,
            |_| {},
        )
        .expect("blocking writer should spawn");
        writer.enqueue(b"blocked".to_vec()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("fake write should start");

        writer.request_shutdown();
        assert!(matches!(
            writer.shutdown_and_join().unwrap(),
            WriterExit::Shutdown
        ));
    }

    #[test]
    fn shutdown_wakes_an_idle_writer_and_disconnects_enqueue() {
        let (completed_sender, _completed_receiver) = mpsc::channel();
        let mut writer = TerminalWriter::spawn_with_io(
            Arc::new(RecordingIo {
                writes: Mutex::new(Vec::new()),
                completed: completed_sender,
            }),
            1,
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
            1,
            move |exit| {
                exit_sender
                    .send(matches!(exit, WriterExit::Shutdown))
                    .expect("Drop test should observe the worker exit");
            },
        )
        .expect("blocking writer should spawn");
        writer.enqueue(b"blocked".to_vec()).unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("fake write should start");

        drop(writer);

        assert!(exit_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("Drop should join the cancelled worker"));
    }

    #[test]
    fn native_write_error_reaches_the_callback_and_join_result() {
        let (exit_sender, exit_receiver) = mpsc::sync_channel(1);
        let writer = TerminalWriter::spawn_with_io(Arc::new(FailingIo), 1, move |exit| {
            let raw_error = match exit {
                WriterExit::WriteFailed(error) => error.raw_os_error(),
                _ => None,
            };
            exit_sender
                .send(raw_error)
                .expect("error callback result should be observed");
        })
        .expect("failing writer should spawn");
        writer.enqueue(b"fail".to_vec()).unwrap();

        assert_eq!(
            exit_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("writer callback should report the failure"),
            Some(5)
        );
        let WriterExit::WriteFailed(error) = writer.shutdown_and_join().unwrap() else {
            panic!("join should preserve the native write error");
        };
        assert_eq!(error.raw_os_error(), Some(5));
    }

    #[test]
    fn an_unexpected_sender_disconnect_is_distinct_from_shutdown() {
        let (_sender, receiver) = mpsc::sync_channel::<Vec<u8>>(1);
        drop(_sender);

        assert!(matches!(
            run_writer(&FailingIo, receiver, &AtomicBool::new(false)),
            WriterExit::ChannelClosed
        ));
    }
}
