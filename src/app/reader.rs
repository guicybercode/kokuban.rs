//! macOS PTY reader: waits for pane output, decodes it and retires ended panes.

use super::window::{self, WindowTitleMailbox};
use super::{process_sixel_event, snapshot_then_lock, PaneCleanup};
use crate::glyph_atlas::GlyphAtlas;
use crate::grid::TerminalEvent;
use crate::layout::PaneId;
use crate::pane::pane::Pane;
use crate::pane::PaneTree;
use crate::pty::unix::wait_any_readable_or_woken;
use crate::pty::Pty;
use crate::renderer::image_store::{ImageFormat, ImageStore};
use std::io::{ErrorKind, Read, Write};
use std::ops::Range;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const READ_CHUNK_BYTES: usize = 4096;
/// Bound one pane's batch so a flooding pane cannot starve decoding or others.
const MAX_BATCH_BYTES_PER_PANE: usize = 16 * READ_CHUNK_BYTES;
const POLL_FAILURE_BACKOFF: Duration = Duration::from_millis(2);

/// Interrupts a reader blocked in `poll` when panes change or shutdown starts.
pub(super) struct ReaderWake {
    sender: UnixStream,
    receiver: UnixStream,
}

impl ReaderWake {
    pub(super) fn new() -> std::io::Result<Self> {
        let (sender, receiver) = UnixStream::pair()?;
        sender.set_nonblocking(true)?;
        receiver.set_nonblocking(true)?;
        Ok(Self { sender, receiver })
    }

    pub(super) fn wake(&self) {
        loop {
            match (&self.sender).write(&[1]) {
                // A full socket already guarantees the reader will wake.
                Ok(_) => return,
                Err(error) if error.kind() == ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => {
                    log::error!("Could not wake the PTY reader: {error}");
                    return;
                }
            }
        }
    }

    fn receiver(&self) -> BorrowedFd<'_> {
        self.receiver.as_fd()
    }

    fn drain(&self) {
        let mut buffer = [0u8; 64];
        loop {
            match (&self.receiver).read(&mut buffer) {
                Ok(0) => return,
                Ok(_) => continue,
                Err(error) if error.kind() == ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => {
                    log::error!("Could not drain PTY reader wakeups: {error}");
                    return;
                }
            }
        }
    }
}

/// How a pane's batch ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PaneEnd {
    Open,
    Closed,
}

/// Run until `shared.should_close`. PTYs are polled and read without locks;
/// the atlas and tree are locked once per wakeup to decode every batch.
pub(super) fn run_reader(
    atlas: &Mutex<GlyphAtlas>,
    pane_tree: &Mutex<PaneTree>,
    dirty: &AtomicBool,
    wake: &ReaderWake,
    shared: &ReaderShared,
) {
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    let mut targets: Vec<(PaneId, Arc<Pty>)> = Vec::new();
    let mut collect_targets = true;
    let mut pending = Vec::new();
    let mut batches: Vec<(PaneId, Range<usize>)> = Vec::new();
    let mut dead_panes = Vec::new();

    while !shared.should_close.load(Ordering::Relaxed) {
        if collect_targets {
            // Pane changes wake the reader, so targets are refreshed only then.
            targets.clear();
            let tree = pane_tree.lock().unwrap();
            for id in tree.pane_ids() {
                let Some(pane) = tree.pane(id) else {
                    continue;
                };
                if pane.input_failed() {
                    dead_panes.push(id);
                } else {
                    targets.push((id, Arc::clone(&pane.pty)));
                }
            }
            collect_targets = false;
        }

        if dead_panes.is_empty() {
            let ptys: Vec<&Pty> = targets.iter().map(|(_, pty)| pty.as_ref()).collect();
            match wait_any_readable_or_woken(&ptys, wake.receiver()) {
                Ok(readiness) => {
                    if readiness.woken {
                        wake.drain();
                        collect_targets = true;
                    }
                    for index in readiness.invalid {
                        let id = targets[index].0;
                        log::error!("PTY poll rejected pane {id}");
                        dead_panes.push(id);
                    }
                    for index in readiness.readable {
                        let (id, pty) = &targets[index];
                        let start = pending.len();
                        let end = read_batch(pty, *id, &mut chunk, &mut pending);
                        if pending.len() > start {
                            batches.push((*id, start..pending.len()));
                        }
                        if end == PaneEnd::Closed {
                            dead_panes.push(*id);
                        }
                    }
                }
                Err(error) => {
                    log::error!("PTY reader poll failed: {error}");
                    collect_targets = true;
                    std::thread::sleep(POLL_FAILURE_BACKOFF);
                }
            }
        }

        if batches.is_empty() && dead_panes.is_empty() {
            continue;
        }

        // Lock atlas FIRST (canonical order: atlas → tree → image_store).
        // Keep it locked until the tree snapshot belongs to the same metric epoch.
        let (cell_size, mut tree) = snapshot_then_lock(atlas, pane_tree, |atlas| {
            (atlas.cell_width, atlas.cell_height)
        });
        for (id, range) in batches.drain(..) {
            process_pane_output(&mut tree, id, &pending[range], cell_size, shared);
        }
        let any_data = !pending.is_empty();
        pending.clear();
        let closing_panes = !dead_panes.is_empty();
        let retired_panes = close_dead_panes(&mut tree, std::mem::take(&mut dead_panes), shared);
        drop(tree);

        if closing_panes {
            // Drop PTY handles first so retired panes own their final reference.
            targets.clear();
            collect_targets = true;
        }
        for pane in retired_panes {
            shared.pane_cleanup.retire(pane);
        }
        if any_data {
            dirty.store(true, Ordering::Relaxed);
        }
    }

    log::info!("PTY reader thread exiting");
}

/// Append available output from one PTY to `pending`, up to the batch limit.
fn read_batch(pty: &Pty, id: PaneId, chunk: &mut [u8], pending: &mut Vec<u8>) -> PaneEnd {
    let limit = pending.len() + MAX_BATCH_BYTES_PER_PANE;
    while pending.len() < limit {
        match pty.read(chunk) {
            Ok(0) => {
                log::info!("PTY EOF for pane {id}");
                return PaneEnd::Closed;
            }
            Ok(read) => pending.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) => {
                log::error!("PTY read error for pane {id}: {error}");
                return PaneEnd::Closed;
            }
        }
    }
    PaneEnd::Open
}

/// State the reader shares with the window and cleanup worker.
pub(super) struct ReaderShared {
    pub(super) image_store: Arc<Mutex<ImageStore>>,
    pub(super) window_title: Arc<WindowTitleMailbox>,
    pub(super) window_is_key: Arc<AtomicBool>,
    pub(super) should_close: Arc<AtomicBool>,
    pub(super) pane_cleanup: PaneCleanup,
    pub(super) kitty_enabled: bool,
    pub(super) sixel_enabled: bool,
}

/// Decode one read of PTY output for `id`. The caller holds the tree lock
/// taken after snapshotting `cell_size` from the atlas.
pub(super) fn process_pane_output(
    tree: &mut PaneTree,
    id: PaneId,
    bytes: &[u8],
    cell_size: (f32, f32),
    shared: &ReaderShared,
) {
    let (cell_w, cell_h) = cell_size;
    let title_revision_before = tree
        .pane(id)
        .map(|pane| pane.grid.title_revision())
        .unwrap_or_default();

    let mut parsed_bytes = 0;
    while parsed_bytes < bytes.len() {
        let (consumed, terminal_events) = {
            let Some(pane) = tree.pane_mut(id) else {
                break;
            };
            let step = pane
                .decoder
                .feed_until_event(&bytes[parsed_bytes..], &mut pane.grid);
            (step.consumed, step.events)
        };
        debug_assert!(consumed > 0);
        parsed_bytes += consumed;

        for event in terminal_events {
            match event {
                TerminalEvent::Response(response) => {
                    if let Some(pane) = tree.pane(id) {
                        pane.queue_input(response);
                    }
                }
                TerminalEvent::KittyGraphics {
                    command,
                    cursor_row,
                    cursor_col,
                } => {
                    if !shared.kitty_enabled {
                        continue;
                    }
                    let mut hard_delete_candidates = {
                        let Some(pane) = tree.pane_mut(id) else {
                            continue;
                        };
                        let grid_cols = pane.grid.cols();
                        let grid_rows = pane.grid.rows();
                        let outcome = {
                            let mut store = shared.image_store.lock().unwrap();
                            pane.kitty_handler.process(
                                command,
                                &mut store,
                                cursor_row,
                                cursor_col,
                                cell_w,
                                cell_h,
                                grid_cols,
                                grid_rows,
                                &mut pane.grid.image_placements,
                            )
                        };
                        if let Some(image_id) = outcome.retransmitted_image_id {
                            pane.grid.remove_hidden_primary_kitty_placements(image_id);
                        }
                        if let Some(response) = outcome.response {
                            pane.queue_input(response);
                        }
                        // Advance cursor for inline images
                        if let Some(adv) = outcome.advance {
                            pane.grid.advance_image_cursor(adv.cols, adv.rows);
                        }
                        outcome.hard_delete_candidates
                    };

                    if !hard_delete_candidates.is_empty() {
                        tree.retain_unreferenced_image_ids(&mut hard_delete_candidates);
                        if !hard_delete_candidates.is_empty() {
                            let mut store = shared.image_store.lock().unwrap();
                            for image_id in hard_delete_candidates {
                                store.remove(image_id);
                            }
                        }
                    }
                }
                TerminalEvent::SixelGraphics {
                    image,
                    cursor_row,
                    cursor_col,
                } => {
                    if !shared.sixel_enabled {
                        continue;
                    }
                    let Some(pane) = tree.pane_mut(id) else {
                        continue;
                    };
                    process_sixel_event(
                        &mut pane.grid,
                        &image,
                        (cursor_row, cursor_col),
                        (cell_w, cell_h),
                        |image| {
                            let mut store = shared.image_store.lock().unwrap();
                            let image_id = store.next_id();
                            store.store(
                                &image.pixels,
                                image.width,
                                image.height,
                                ImageFormat::Rgba,
                                Some(image_id),
                            )
                        },
                    );
                }
            }
        }
    }

    let focused_title_changed = id == tree.focused
        && tree
            .pane(id)
            .is_some_and(|pane| pane.grid.title_revision() != title_revision_before);
    if focused_title_changed {
        window::publish_focused_window_title(tree, shared.window_title.as_ref());
    }
}

/// Close panes whose PTY ended or whose input failed, returning the panes to
/// retire after the tree lock is released.
pub(super) fn close_dead_panes(
    tree: &mut PaneTree,
    dead_panes: Vec<PaneId>,
    shared: &ReaderShared,
) -> Vec<Pane> {
    let mut retired_panes = Vec::new();
    let previous_focus = tree.focused_pane().map(|pane| pane.id);
    for id in dead_panes {
        let outcome = tree.close(id);
        if let Some(pane) = outcome.closed_pane {
            retired_panes.push(pane);
        }
        if outcome.should_terminate {
            shared.should_close.store(true, Ordering::Relaxed);
            break;
        }
    }

    if tree.focused_pane().map(|pane| pane.id) != previous_focus {
        let detached_previous = previous_focus
            .and_then(|previous_focus| retired_panes.iter().find(|pane| pane.id == previous_focus));
        window::dispatch_pane_focus_transition_locked(
            tree,
            shared.window_is_key.as_ref(),
            previous_focus,
            detached_previous,
        );
        window::publish_focused_window_title(tree, shared.window_title.as_ref());
    }
    retired_panes
}

#[cfg(test)]
mod tests {
    use super::ReaderWake;
    use crate::pty::unix::wait_any_readable_or_woken;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    #[test]
    fn wake_interrupts_a_blocked_wait_and_drains_repeated_wakeups() {
        let wake = Arc::new(ReaderWake::new().unwrap());
        let reader_wake = wake.clone();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            result_tx
                .send(wait_any_readable_or_woken(&[], reader_wake.receiver()))
                .unwrap();
        });
        assert!(matches!(
            result_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        // Wakes never block, even after the socket buffer fills.
        for _ in 0..100_000 {
            wake.wake();
        }
        assert!(result_rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap().woken);
        reader.join().unwrap();

        wake.drain();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let reader_wake = wake.clone();
        let reader = std::thread::spawn(move || {
            result_tx
                .send(wait_any_readable_or_woken(&[], reader_wake.receiver()))
                .unwrap();
        });
        assert!(
            matches!(
                result_rx.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "drained wakeups must not wake the next wait"
        );
        wake.wake();
        assert!(result_rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap().woken);
        reader.join().unwrap();
    }
}
