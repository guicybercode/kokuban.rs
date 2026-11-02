//! Focus reports (`CSI I` / `CSI O`) when the focused pane changes, and the
//! window title that follows the focused pane. Pure over the pane tree; both
//! windows call these under the tree lock.

use super::pane::Pane;
use super::PaneTree;
use crate::layout::PaneId;
use crate::window_title::WindowTitleMailbox;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) const FOCUS_IN_REPORT: &[u8] = b"\x1b[I";
pub(crate) const FOCUS_OUT_REPORT: &[u8] = b"\x1b[O";

pub(crate) fn focus_report_bytes(focused: bool) -> &'static [u8] {
    if focused {
        FOCUS_IN_REPORT
    } else {
        FOCUS_OUT_REPORT
    }
}

/// Publish the focused pane's OSC title to the mailbox; true when it changed.
pub(crate) fn publish_focused_window_title(
    tree: &PaneTree,
    window_title: &WindowTitleMailbox,
) -> bool {
    let title = tree
        .focused_pane()
        .map(|pane| pane.grid.title())
        .unwrap_or_default();
    window_title.publish(title)
}

pub(crate) fn dispatch_pane_focus_transition_with<T, Enabled, Dispatch>(
    window_is_key: bool,
    previous_focus: Option<PaneId>,
    current_focus: Option<PaneId>,
    previous_pane: Option<&T>,
    current_pane: Option<&T>,
    focus_events_enabled: Enabled,
    mut dispatch: Dispatch,
) -> bool
where
    Enabled: Fn(&T) -> bool,
    Dispatch: FnMut(&T, Vec<u8>),
{
    if !window_is_key || previous_focus == current_focus {
        return false;
    }

    let mut dispatched = false;
    if let Some(previous_pane) = previous_pane.filter(|pane| focus_events_enabled(pane)) {
        dispatch(previous_pane, FOCUS_OUT_REPORT.to_vec());
        dispatched = true;
    }
    if let Some(current_pane) = current_pane.filter(|pane| focus_events_enabled(pane)) {
        dispatch(current_pane, FOCUS_IN_REPORT.to_vec());
        dispatched = true;
    }
    dispatched
}

/// The caller must hold the mutex that owns `tree` for the full call. This
/// serializes pane selection, per-pane mode checks, and nonblocking enqueues.
pub(crate) fn dispatch_pane_focus_transition_locked(
    tree: &PaneTree,
    window_is_key: &AtomicBool,
    previous_focus: Option<PaneId>,
    detached_previous: Option<&Pane>,
) -> bool {
    let current_focus = tree.focused_pane().map(|pane| pane.id);
    let previous_pane = previous_focus.and_then(|previous_focus| {
        detached_previous
            .filter(|pane| pane.id == previous_focus)
            .or_else(|| tree.pane(previous_focus))
    });
    let current_pane = current_focus.and_then(|current_focus| tree.pane(current_focus));

    dispatch_pane_focus_transition_with(
        window_is_key.load(Ordering::Acquire),
        previous_focus,
        current_focus,
        previous_pane,
        current_pane,
        |pane| pane.grid.focus_events,
        |pane, bytes| pane.queue_input(bytes),
    )
}
