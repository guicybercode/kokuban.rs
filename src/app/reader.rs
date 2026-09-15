//! macOS PTY output handling: decodes pane output and retires ended panes.

use super::window::{self, WindowTitleMailbox};
use super::{process_sixel_event, PaneCleanup};
use crate::grid::TerminalEvent;
use crate::layout::PaneId;
use crate::pane::pane::Pane;
use crate::pane::PaneTree;
use crate::renderer::image_store::{ImageFormat, ImageStore};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
