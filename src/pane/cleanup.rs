//! Retires closed panes on a worker thread so joining their writer never
//! blocks the thread that holds the pane tree lock.

use super::pane::Pane;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

pub(crate) enum CleanupCommand<T> {
    Retire(Box<T>),
    Shutdown,
}

#[derive(Clone)]
pub(crate) struct PaneCleanup {
    sender: mpsc::Sender<CleanupCommand<Pane>>,
    fallback: Arc<Mutex<Vec<Pane>>>,
}

impl PaneCleanup {
    pub(crate) fn spawn() -> (Self, JoinHandle<()>) {
        let (sender, receiver) = mpsc::channel();
        let fallback = Arc::new(Mutex::new(Vec::new()));
        let handle = std::thread::Builder::new()
            .name("pane-cleanup".to_string())
            .spawn(move || run_cleanup(receiver, Pane::retire))
            .expect("Failed to spawn pane cleanup thread");

        (Self { sender, fallback }, handle)
    }

    pub(crate) fn retire(&self, pane: Pane) {
        if let Err(error) = self.sender.send(CleanupCommand::Retire(Box::new(pane))) {
            let CleanupCommand::Retire(pane) = error.0 else {
                unreachable!("retire sends only retire commands");
            };
            log::error!("Pane cleanup worker is unavailable; deferring local cleanup");
            match self.fallback.lock() {
                Ok(mut fallback) => fallback.push(*pane),
                Err(poisoned) => poisoned.into_inner().push(*pane),
            }
        }
    }

    pub(crate) fn shutdown(&self) {
        if self.sender.send(CleanupCommand::Shutdown).is_err() {
            log::warn!("Pane cleanup worker stopped before shutdown");
        }
    }

    pub(crate) fn take_fallback(&self) -> Vec<Pane> {
        let mut fallback = match self.fallback.lock() {
            Ok(fallback) => fallback,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *fallback)
    }
}

pub(crate) fn run_cleanup<T, F>(receiver: mpsc::Receiver<CleanupCommand<T>>, mut retire: F)
where
    F: FnMut(T),
{
    while let Ok(command) = receiver.recv() {
        match command {
            CleanupCommand::Retire(value) => retire(*value),
            CleanupCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{run_cleanup, CleanupCommand};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    struct LockingDropProbe {
        lock: Arc<Mutex<()>>,
        started: mpsc::Sender<std::thread::ThreadId>,
        finished: mpsc::Sender<()>,
    }

    impl Drop for LockingDropProbe {
        fn drop(&mut self) {
            self.started
                .send(std::thread::current().id())
                .expect("cleanup test should observe the retire thread");
            let _guard = self.lock.lock().unwrap();
            self.finished
                .send(())
                .expect("cleanup test should observe retirement completion");
        }
    }

    #[test]
    fn cleanup_retires_before_shutdown_without_blocking_the_callers_lock() {
        let (sender, receiver) = mpsc::channel();
        let (started_sender, started_receiver) = mpsc::channel();
        let (finished_sender, finished_receiver) = mpsc::channel();
        let lock = Arc::new(Mutex::new(()));
        let caller_guard = lock.lock().unwrap();
        let caller_thread = std::thread::current().id();
        let worker = std::thread::spawn(move || run_cleanup(receiver, drop));

        sender
            .send(CleanupCommand::Retire(Box::new(LockingDropProbe {
                lock: lock.clone(),
                started: started_sender,
                finished: finished_sender,
            })))
            .unwrap();
        sender.send(CleanupCommand::Shutdown).unwrap();

        let retire_thread = started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("retirement should start on the cleanup worker");
        assert_ne!(retire_thread, caller_thread);
        assert!(finished_receiver.try_recv().is_err());

        drop(caller_guard);
        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("retirement should finish after the caller releases its lock");
        worker
            .join()
            .expect("cleanup worker should process shutdown");
    }
}
