//! PTY reader shared by the macOS and Linux windows: waits for output from
//! every pane, decodes it under the pane tree lock and retires panes whose
//! shell exited. What differs between the windows (how a frame is requested,
//! how closed panes and attention reach the main thread) lives behind
//! [`PaneReaderHost`].

use crate::graphics::{process_sixel_event, ImageId, SixelPlacement};
use crate::grid::{Grid, TerminalEvent};
use crate::layout::PaneId;
use crate::notifications::{NotificationController, NotificationRequest};
use crate::pane::focus::{dispatch_pane_focus_transition_locked, publish_focused_window_title};
use crate::pane::pane::Pane;
use crate::pane::PaneTree;
use crate::pty::unix::wait_any_readable_or_woken;
use crate::pty::Pty;
use crate::renderer::image_store::{ImageFormat, ImageStore};
use crate::renderer::kitty_handler::KittyHandler;
use crate::window_title::WindowTitleMailbox;
use std::collections::HashSet;
use std::io::{ErrorKind, Read, Write};
use std::ops::Range;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

const READ_CHUNK_BYTES: usize = 4096;
/// Bound one pane's batch so a flooding pane cannot starve decoding or others.
const MAX_BATCH_BYTES_PER_PANE: usize = 16 * READ_CHUNK_BYTES;
const POLL_FAILURE_BACKOFF: Duration = Duration::from_millis(2);

/// Interrupts a reader blocked in `poll` when panes change or shutdown starts.
pub(crate) struct ReaderWake {
    sender: UnixStream,
    receiver: UnixStream,
}

impl ReaderWake {
    pub(crate) fn new() -> std::io::Result<Self> {
        let (sender, receiver) = UnixStream::pair()?;
        sender.set_nonblocking(true)?;
        receiver.set_nonblocking(true)?;
        Ok(Self { sender, receiver })
    }

    pub(crate) fn wake(&self) {
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

/// Which image protocols the reader applies and how it places their images.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GraphicsPolicy {
    pub(crate) kitty_enabled: bool,
    pub(crate) sixel_enabled: bool,
    pub(crate) sixel_placement: SixelPlacement,
    /// Software renderer: drop placements whose image left the cache and
    /// keep at most this many per grid, so tiny repeated placements cannot
    /// grow metadata without bound.
    pub(crate) placement_limit: Option<usize>,
}

/// State the reader shares with the window.
pub(crate) struct ReaderShared {
    pub(crate) image_store: Arc<Mutex<ImageStore>>,
    pub(crate) window_title: Arc<WindowTitleMailbox>,
    pub(crate) window_is_key: Arc<AtomicBool>,
    pub(crate) should_close: Arc<AtomicBool>,
    pub(crate) graphics: GraphicsPolicy,
    pub(crate) notifications: Arc<NotificationController>,
    /// `[clipboard] osc52_write`: honour OSC 52 clipboard writes.
    pub(crate) osc52_write: bool,
}

/// Window-side effects of the reader. Every method may run on the reader
/// thread, and none may take the pane tree lock (the caller holds it).
pub(crate) trait PaneReaderHost: Send + Sync {
    /// Lock the tree together with the cell size its placements use, so
    /// both belong to the same font metric epoch.
    fn cell_size_then_lock<'a>(
        &self,
        tree: &'a Mutex<PaneTree>,
    ) -> ((f32, f32), MutexGuard<'a, PaneTree>);
    /// Terminal content changed or panes closed: present a frame.
    fn request_frame(&self);
    /// A pane's shell exited and the tree already dropped it; when it was
    /// the last one `ReaderShared::should_close` is set.
    fn pane_closed(&self, id: PaneId);
    /// The focused pane's title changed (already published to the mailbox).
    fn title_changed(&self);
    /// BEL arrived; the window decides whether to request attention.
    fn user_attention(&self);
    /// OSC 52 asked to put this text on the system clipboard.
    fn clipboard_write(&self, text: String);
    /// A detached pane whose writer must be joined outside the tree lock.
    fn pane_retired(&self, pane: Pane);
}

/// How a pane's batch ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PaneEnd {
    Open,
    Closed,
}

/// Run until `shared.should_close`. PTYs are polled and read without locks;
/// the tree is locked once per wakeup to decode every batch.
pub(crate) fn run_reader(
    pane_tree: &Mutex<PaneTree>,
    wake: &ReaderWake,
    shared: &ReaderShared,
    host: &dyn PaneReaderHost,
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

        let (cell_size, mut tree) = host.cell_size_then_lock(pane_tree);
        for (id, range) in batches.drain(..) {
            process_pane_output(&mut tree, id, &pending[range], cell_size, shared, host);
        }
        let any_data = !pending.is_empty();
        pending.clear();
        let closing_panes = !dead_panes.is_empty();
        let closed_ids = std::mem::take(&mut dead_panes);
        let retired_panes = close_dead_panes(&mut tree, &closed_ids, shared, host);
        drop(tree);

        if closing_panes {
            // Drop PTY handles first so retired panes own their final reference.
            targets.clear();
            collect_targets = true;
        }
        for pane in retired_panes {
            host.pane_retired(pane);
        }
        // Closing panes changes layout and titles, and may request shutdown.
        if any_data || closing_panes {
            host.request_frame();
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

/// Mark `id` as wanting attention unless the user is already watching it:
/// focused pane in the key window.
fn flag_pane_attention(tree: &mut PaneTree, id: PaneId, window_is_key: &AtomicBool) {
    if id == tree.focused && window_is_key.load(Ordering::Acquire) {
        return;
    }
    if let Some(pane) = tree.pane_mut(id) {
        pane.attention = true;
    }
}

/// Cache effects of one image protocol event that the caller resolves
/// against every pane before touching the store.
#[derive(Default)]
pub(crate) struct GraphicsEventOutcome {
    pub(crate) response: Option<Vec<u8>>,
    pub(crate) hard_delete_candidates: HashSet<ImageId>,
}

/// Apply a Kitty or Sixel event to one grid. Other events are ignored.
pub(crate) fn apply_graphics_event(
    grid: &mut Grid,
    kitty: &mut KittyHandler,
    store: &mut ImageStore,
    event: TerminalEvent,
    cell_size: (f32, f32),
    policy: &GraphicsPolicy,
) -> GraphicsEventOutcome {
    let (cell_w, cell_h) = cell_size;
    let mut outcome = GraphicsEventOutcome::default();
    match event {
        TerminalEvent::KittyGraphics {
            command,
            cursor_row,
            cursor_col,
        } => {
            if !policy.kitty_enabled {
                return outcome;
            }
            let grid_cols = grid.cols();
            let grid_rows = grid.rows();
            let kitty_outcome = kitty.process(
                command,
                store,
                cursor_row,
                cursor_col,
                cell_w,
                cell_h,
                grid_cols,
                grid_rows,
                &mut grid.image_placements,
            );
            if let Some(image_id) = kitty_outcome.retransmitted_image_id {
                grid.remove_hidden_primary_kitty_placements(image_id);
            }
            // Advance cursor for inline images
            if let Some(advance) = kitty_outcome.advance {
                grid.advance_image_cursor(advance.cols, advance.rows);
            }
            outcome.response = kitty_outcome.response;
            outcome.hard_delete_candidates = kitty_outcome.hard_delete_candidates;
        }
        TerminalEvent::SixelGraphics {
            image,
            cursor_row,
            cursor_col,
        } => {
            if !policy.sixel_enabled {
                return outcome;
            }
            process_sixel_event(
                grid,
                &image,
                (cursor_row, cursor_col),
                cell_size,
                policy.sixel_placement,
                |image| {
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
        TerminalEvent::Response(_)
        | TerminalEvent::Notification { .. }
        | TerminalEvent::Bell
        | TerminalEvent::ClipboardWrite(_) => return outcome,
    }
    if let Some(limit) = policy.placement_limit {
        grid.image_placements
            .retain(|placement| store.get(placement.image_id).is_some());
        let excess = grid.image_placements.len().saturating_sub(limit);
        grid.image_placements.drain(..excess);
    }
    outcome
}

/// Decode one read of PTY output for `id`. The caller holds the tree lock
/// taken with `cell_size` from the same metric epoch.
pub(crate) fn process_pane_output(
    tree: &mut PaneTree,
    id: PaneId,
    bytes: &[u8],
    cell_size: (f32, f32),
    shared: &ReaderShared,
    host: &dyn PaneReaderHost,
) {
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
                TerminalEvent::Notification { title, body } => {
                    shared
                        .notifications
                        .handle_notification(NotificationRequest::new(title, body));
                    flag_pane_attention(tree, id, shared.window_is_key.as_ref());
                }
                TerminalEvent::Bell => {
                    // Dock bounce / attention is applied on the main thread.
                    shared.notifications.handle_bell();
                    flag_pane_attention(tree, id, shared.window_is_key.as_ref());
                    host.user_attention();
                }
                TerminalEvent::ClipboardWrite(text) => {
                    if shared.osc52_write {
                        host.clipboard_write(text);
                    }
                }
                event @ (TerminalEvent::KittyGraphics { .. }
                | TerminalEvent::SixelGraphics { .. }) => {
                    let mut hard_delete_candidates = {
                        let Some(pane) = tree.pane_mut(id) else {
                            continue;
                        };
                        let outcome = {
                            let mut store = shared.image_store.lock().unwrap();
                            apply_graphics_event(
                                &mut pane.grid,
                                &mut pane.kitty_handler,
                                &mut store,
                                event,
                                cell_size,
                                &shared.graphics,
                            )
                        };
                        if let Some(response) = outcome.response {
                            pane.queue_input(response);
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
            }
        }
    }

    let focused_title_changed = id == tree.focused
        && tree
            .pane(id)
            .is_some_and(|pane| pane.grid.title_revision() != title_revision_before);
    if focused_title_changed {
        if publish_focused_window_title(tree, shared.window_title.as_ref()) {
            host.title_changed();
        }
        tree.clear_focused_attention();
    }
}

/// Close panes whose PTY ended or whose input failed, returning the panes to
/// retire after the tree lock is released.
pub(crate) fn close_dead_panes(
    tree: &mut PaneTree,
    dead_panes: &[PaneId],
    shared: &ReaderShared,
    host: &dyn PaneReaderHost,
) -> Vec<Pane> {
    let mut retired_panes = Vec::new();
    let previous_focus = tree.focused_pane().map(|pane| pane.id);
    for &id in dead_panes {
        let outcome = tree.close(id);
        if let Some(pane) = outcome.closed_pane {
            retired_panes.push(pane);
        }
        if outcome.should_terminate {
            shared.should_close.store(true, Ordering::Relaxed);
            host.pane_closed(id);
            break;
        }
        host.pane_closed(id);
    }

    if tree.focused_pane().map(|pane| pane.id) != previous_focus {
        let detached_previous = previous_focus
            .and_then(|previous_focus| retired_panes.iter().find(|pane| pane.id == previous_focus));
        dispatch_pane_focus_transition_locked(
            tree,
            shared.window_is_key.as_ref(),
            previous_focus,
            detached_previous,
        );
        if publish_focused_window_title(tree, shared.window_title.as_ref()) {
            host.title_changed();
        }
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
        assert!(
            result_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
                .woken
        );
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
        assert!(
            result_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
                .woken
        );
        reader.join().unwrap();
    }
}
