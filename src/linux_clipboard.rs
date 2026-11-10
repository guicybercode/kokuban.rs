//! Linux clipboard ownership and blocking OS calls live on one background worker.
//!
//! Arboard's text-only backend uses X11 or Wayland data-control. Keep its owner
//! alive between requests so another application can retrieve copied text.
//! Both the CLIPBOARD and the PRIMARY selection go through the same worker.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;

pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;
const COMMAND_CAPACITY: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    #[error("clipboard worker queue is full")]
    Busy,
    #[error("clipboard worker is unavailable")]
    Unavailable,
    #[error("clipboard text exceeds the one MiB limit")]
    TooLarge,
    #[error("clipboard backend failed: {0}")]
    Backend(String),
}

/// Which X11/Wayland selection a request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardKind {
    /// Explicit copy/paste (Ctrl+Shift+C/V, OSC 52).
    Clipboard,
    /// The current mouse selection, pasted with the middle button.
    Primary,
}

#[derive(Debug)]
pub enum ClipboardEvent {
    Paste {
        request_id: u64,
        result: Result<String, ClipboardError>,
    },
    CopyFinished {
        kind: ClipboardKind,
        result: Result<(), ClipboardError>,
    },
}

enum ClipboardCommand {
    Copy(ClipboardKind, String),
    Paste(ClipboardKind, u64),
}

pub struct LinuxClipboard {
    commands: SyncSender<ClipboardCommand>,
    stopped: Arc<AtomicBool>,
}

impl LinuxClipboard {
    /// The callback runs on the worker. Forward events to the UI event loop;
    /// do not access a window directly or block waiting for the UI from it.
    pub fn new(on_event: impl Fn(ClipboardEvent) + Send + 'static) -> io::Result<Self> {
        Self::with_backend(
            || arboard::Clipboard::new().map_err(backend_error),
            on_event,
        )
    }

    pub fn copy_text(&self, kind: ClipboardKind, text: String) -> Result<(), ClipboardError> {
        validate_text_size(&text)?;
        self.enqueue(ClipboardCommand::Copy(kind, text))
    }

    /// Preserve the request ID when returning text so the UI can reject stale
    /// results after the target tab closes or its session changes.
    pub fn request_paste(
        &self,
        kind: ClipboardKind,
        request_id: u64,
    ) -> Result<(), ClipboardError> {
        self.enqueue(ClipboardCommand::Paste(kind, request_id))
    }

    fn enqueue(&self, command: ClipboardCommand) -> Result<(), ClipboardError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => ClipboardError::Busy,
                TrySendError::Disconnected(_) => ClipboardError::Unavailable,
            })
    }

    fn with_backend<B: ClipboardBackend + 'static>(
        mut open_backend: impl FnMut() -> Result<B, ClipboardError> + Send + 'static,
        on_event: impl Fn(ClipboardEvent) + Send + 'static,
    ) -> io::Result<Self> {
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let worker = thread::Builder::new()
            .name("kokuban-clipboard".into())
            .spawn(move || {
                let mut backend: Option<B> = None;
                while let Ok(command) = receiver.recv() {
                    if worker_stopped.load(Ordering::Acquire) {
                        break;
                    }
                    // Initialization is also an OS call; delay it until needed
                    // and retry on later requests if no backend was available.
                    let initialized = if backend.is_none() {
                        open_backend().map(|clipboard| {
                            backend = Some(clipboard);
                        })
                    } else {
                        Ok(())
                    };
                    let clipboard = initialized
                        .and_then(|()| backend.as_mut().ok_or(ClipboardError::Unavailable));
                    let event = match command {
                        ClipboardCommand::Copy(kind, text) => ClipboardEvent::CopyFinished {
                            kind,
                            result: clipboard
                                .and_then(|clipboard| clipboard.write_text(kind, text)),
                        },
                        ClipboardCommand::Paste(kind, request_id) => ClipboardEvent::Paste {
                            request_id,
                            result: clipboard.and_then(|clipboard| {
                                let text = clipboard.read_text(kind)?;
                                // Arboard returns an owned String; it has no
                                // streaming byte limit for the underlying OS
                                // transfer. Bound what reaches our UI/PTY.
                                validate_text_size(&text)?;
                                Ok(text)
                            }),
                        },
                    };
                    if worker_stopped.load(Ordering::Acquire) {
                        break;
                    }
                    on_event(event);
                }
                // Clipboard destruction and any backend cleanup happen here,
                // not on the UI thread. Queued work is discarded on shutdown.
            })?;
        // Detach: an unresponsive external clipboard owner must not make window
        // shutdown wait forever. There is at most one worker per instance.
        drop(worker);
        Ok(Self { commands, stopped })
    }
}

impl Drop for LinuxClipboard {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // Dropping our sole sender wakes an idle worker. A worker inside an OS
        // call will release its clipboard once that call returns; never join it.
    }
}

fn validate_text_size(text: &str) -> Result<(), ClipboardError> {
    if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
        Err(ClipboardError::TooLarge)
    } else {
        Ok(())
    }
}

fn backend_error(error: arboard::Error) -> ClipboardError {
    ClipboardError::Backend(error.to_string())
}

trait ClipboardBackend {
    fn read_text(&mut self, kind: ClipboardKind) -> Result<String, ClipboardError>;
    fn write_text(&mut self, kind: ClipboardKind, text: String) -> Result<(), ClipboardError>;
}

fn arboard_kind(kind: ClipboardKind) -> arboard::LinuxClipboardKind {
    match kind {
        ClipboardKind::Clipboard => arboard::LinuxClipboardKind::Clipboard,
        ClipboardKind::Primary => arboard::LinuxClipboardKind::Primary,
    }
}

impl ClipboardBackend for arboard::Clipboard {
    fn read_text(&mut self, kind: ClipboardKind) -> Result<String, ClipboardError> {
        use arboard::GetExtLinux;
        self.get()
            .clipboard(arboard_kind(kind))
            .text()
            .map_err(backend_error)
    }

    fn write_text(&mut self, kind: ClipboardKind, text: String) -> Result<(), ClipboardError> {
        use arboard::SetExtLinux;
        self.set()
            .clipboard(arboard_kind(kind))
            .text(text)
            .map_err(backend_error)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClipboardBackend, ClipboardError, ClipboardEvent, ClipboardKind, LinuxClipboard,
        COMMAND_CAPACITY, MAX_CLIPBOARD_TEXT_BYTES,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    struct FakeClipboard {
        text: String,
        primary: Option<String>,
        writes: Arc<AtomicUsize>,
        read_started: Option<Sender<()>>,
        release_read: Option<Receiver<()>>,
        dropped: Option<Sender<()>>,
    }

    impl FakeClipboard {
        fn new(text: String) -> Self {
            Self {
                text,
                primary: None,
                writes: Arc::new(AtomicUsize::new(0)),
                read_started: None,
                release_read: None,
                dropped: None,
            }
        }
    }

    impl ClipboardBackend for FakeClipboard {
        fn read_text(&mut self, kind: ClipboardKind) -> Result<String, ClipboardError> {
            if let Some(started) = self.read_started.take() {
                started.send(()).unwrap();
            }
            if let Some(release) = self.release_read.take() {
                release.recv().unwrap();
            }
            match kind {
                ClipboardKind::Clipboard => Ok(self.text.clone()),
                // Like Wayland without primary-selection support.
                ClipboardKind::Primary => self
                    .primary
                    .clone()
                    .ok_or_else(|| ClipboardError::Backend("primary unsupported".into())),
            }
        }

        fn write_text(&mut self, kind: ClipboardKind, text: String) -> Result<(), ClipboardError> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            match kind {
                ClipboardKind::Clipboard => self.text = text,
                ClipboardKind::Primary => self.primary = Some(text),
            }
            Ok(())
        }
    }

    impl Drop for FakeClipboard {
        fn drop(&mut self) {
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    fn fake_worker(backend: FakeClipboard) -> (LinuxClipboard, Receiver<ClipboardEvent>) {
        let mut backend = Some(backend);
        let (events, receiver) = mpsc::channel();
        let worker = LinuxClipboard::with_backend(
            move || {
                Ok(backend
                    .take()
                    .expect("the clipboard owner must be retained"))
            },
            move |event| events.send(event).unwrap(),
        )
        .unwrap();
        (worker, receiver)
    }

    #[test]
    fn retains_copy_ownership_and_returns_paste_to_its_request_id() {
        let (dropped, drop_receiver) = mpsc::channel();
        let mut backend = FakeClipboard::new(String::new());
        backend.dropped = Some(dropped);
        let (worker, events) = fake_worker(backend);
        worker
            .copy_text(ClipboardKind::Clipboard, "ação 🦀".into())
            .unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            ClipboardEvent::CopyFinished {
                kind: ClipboardKind::Clipboard,
                result: Ok(())
            }
        ));
        assert!(drop_receiver.try_recv().is_err(), "copy owner was dropped");
        worker.request_paste(ClipboardKind::Clipboard, 41).unwrap();
        match events.recv_timeout(TEST_TIMEOUT).unwrap() {
            ClipboardEvent::Paste {
                request_id: 41,
                result: Ok(text),
            } => {
                assert_eq!(text, "ação 🦀");
            }
            event => panic!("unexpected clipboard event: {event:?}"),
        }
        drop(worker);
        drop_receiver.recv_timeout(TEST_TIMEOUT).unwrap();
    }

    #[test]
    fn bounds_copied_text_before_enqueue_and_pasted_text_before_callback() {
        let (worker, events) =
            fake_worker(FakeClipboard::new("x".repeat(MAX_CLIPBOARD_TEXT_BYTES + 1)));
        assert_eq!(
            worker.copy_text(
                ClipboardKind::Clipboard,
                "x".repeat(MAX_CLIPBOARD_TEXT_BYTES + 1)
            ),
            Err(ClipboardError::TooLarge)
        );
        worker.request_paste(ClipboardKind::Clipboard, 42).unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            ClipboardEvent::Paste {
                request_id: 42,
                result: Err(ClipboardError::TooLarge)
            }
        ));
        worker
            .copy_text(
                ClipboardKind::Clipboard,
                "x".repeat(MAX_CLIPBOARD_TEXT_BYTES),
            )
            .unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            ClipboardEvent::CopyFinished {
                kind: ClipboardKind::Clipboard,
                result: Ok(())
            }
        ));
    }

    #[test]
    fn bounded_queue_and_shutdown_do_not_wait_for_an_external_clipboard_owner() {
        let (started, start_receiver) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel();
        let (dropped, drop_receiver) = mpsc::channel();
        let mut backend = FakeClipboard::new("waiting".into());
        backend.read_started = Some(started);
        backend.release_read = Some(release_receiver);
        backend.dropped = Some(dropped);
        let writes = Arc::clone(&backend.writes);
        let (worker, events) = fake_worker(backend);
        worker.request_paste(ClipboardKind::Clipboard, 1).unwrap();
        start_receiver.recv_timeout(TEST_TIMEOUT).unwrap();
        for _ in 0..COMMAND_CAPACITY {
            worker
                .copy_text(ClipboardKind::Clipboard, "queued".into())
                .unwrap();
        }
        assert_eq!(
            worker.request_paste(ClipboardKind::Clipboard, 2),
            Err(ClipboardError::Busy)
        );
        let (shutdown_done, shutdown_receiver) = mpsc::channel();
        let shutdown = thread::spawn(move || {
            drop(worker);
            shutdown_done.send(()).unwrap();
        });
        let nonblocking_shutdown = shutdown_receiver.recv_timeout(TEST_TIMEOUT);
        // Release even on failure, preventing a broken shutdown implementation
        // from leaving this test's worker permanently blocked.
        release.send(()).unwrap();
        shutdown.join().unwrap();
        assert!(
            nonblocking_shutdown.is_ok(),
            "shutdown waited for the clipboard read"
        );
        drop_receiver.recv_timeout(TEST_TIMEOUT).unwrap();
        assert_eq!(
            writes.load(Ordering::Relaxed),
            0,
            "queued copies ran after shutdown"
        );
        assert!(
            events.try_recv().is_err(),
            "paste callback ran after shutdown"
        );
    }

    #[test]
    fn unavailable_backend_can_be_retried_without_spawning_more_workers() {
        let mut attempts = 0;
        let (events, receiver) = mpsc::channel();
        let worker = LinuxClipboard::with_backend(
            move || {
                attempts += 1;
                if attempts == 1 {
                    Err(ClipboardError::Backend("display unavailable".into()))
                } else {
                    Ok(FakeClipboard::new("available".into()))
                }
            },
            move |event| events.send(event).unwrap(),
        )
        .unwrap();
        worker.request_paste(ClipboardKind::Clipboard, 1).unwrap();
        assert!(matches!(
            receiver.recv_timeout(TEST_TIMEOUT).unwrap(),
            ClipboardEvent::Paste {
                request_id: 1,
                result: Err(ClipboardError::Backend(_))
            }
        ));
        worker.request_paste(ClipboardKind::Clipboard, 2).unwrap();
        match receiver.recv_timeout(TEST_TIMEOUT).unwrap() {
            ClipboardEvent::Paste {
                request_id: 2,
                result: Ok(text),
            } => {
                assert_eq!(text, "available");
            }
            event => panic!("unexpected clipboard event: {event:?}"),
        }
    }

    #[test]
    fn primary_and_clipboard_selections_stay_independent() {
        let (worker, events) = fake_worker(FakeClipboard::new("clipboard".into()));
        worker.request_paste(ClipboardKind::Primary, 1).unwrap();
        assert!(
            matches!(
                events.recv_timeout(TEST_TIMEOUT).unwrap(),
                ClipboardEvent::Paste {
                    request_id: 1,
                    result: Err(ClipboardError::Backend(_))
                }
            ),
            "an unsupported PRIMARY must fail without touching CLIPBOARD"
        );
        worker
            .copy_text(ClipboardKind::Primary, "selected 日本".into())
            .unwrap();
        assert!(matches!(
            events.recv_timeout(TEST_TIMEOUT).unwrap(),
            ClipboardEvent::CopyFinished {
                kind: ClipboardKind::Primary,
                result: Ok(())
            }
        ));
        for (kind, request_id, expected) in [
            (ClipboardKind::Primary, 2, "selected 日本"),
            (ClipboardKind::Clipboard, 3, "clipboard"),
        ] {
            worker.request_paste(kind, request_id).unwrap();
            match events.recv_timeout(TEST_TIMEOUT).unwrap() {
                ClipboardEvent::Paste {
                    request_id: id,
                    result: Ok(text),
                } => {
                    assert_eq!((id, text.as_str()), (request_id, expected));
                }
                event => panic!("unexpected clipboard event: {event:?}"),
            }
        }
        assert_eq!(
            worker.copy_text(
                ClipboardKind::Primary,
                "x".repeat(MAX_CLIPBOARD_TEXT_BYTES + 1)
            ),
            Err(ClipboardError::TooLarge)
        );
    }
}
