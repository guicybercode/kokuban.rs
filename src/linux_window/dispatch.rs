//! Keyboard, mouse, focus and encoded-input dispatch: each `*_with` function
//! locks the grid only to read its mode flags, then hands the bytes to an
//! injected writer so the callers (and tests) choose the transport.

use super::pointer::{
    accumulate_wheel_steps, apply_wheel_scrollback, mouse_button_code_from_winit,
    mouse_button_with_modifiers, terminal_cell_at_mouse_button, terminal_cell_at_mouse_motion,
    terminal_cell_at_pointer, MouseMotionState, MouseWheelState,
};
use super::sizing::terminal_dimensions_from_locked_grid;
use super::GridAccessError;
use crate::grid::{Grid, MouseTracking};
use crate::input::linux::ScrollbackAction;
use crate::input::mouse::{
    encode_alternate_scroll_steps, encode_mouse_event, mouse_wheel_route, MouseWheelRoute,
    MAX_WHEEL_STEPS_PER_EVENT, MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP,
};
use crate::terminal_writer::TerminalWriteQueueError;
use std::sync::Mutex;
use thiserror::Error;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase};
use winit::keyboard::ModifiersState;

const FOCUS_IN_REPORT: &[u8] = b"\x1b[I";

const FOCUS_OUT_REPORT: &[u8] = b"\x1b[O";

const MOUSE_MOTION_FLAG: u8 = 32;

const MOUSE_NO_BUTTON: u8 = 3;

#[derive(Debug, Error)]
pub(super) enum KeyboardInputError {
    #[error("could not access Linux terminal input state: {0}")]
    Grid(#[from] GridAccessError),
    #[error("could not queue Linux terminal input: {0}")]
    Queue(#[from] TerminalWriteQueueError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyboardInputOutcome {
    Ignored { viewport_changed: bool },
    Forwarded { viewport_changed: bool },
    Scrollback { viewport_changed: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MouseMotionDispatchOutcome {
    Ignored,
    Observed((usize, usize)),
    Deduplicated,
    DroppedFull,
    Enqueued((usize, usize)),
}

impl KeyboardInputOutcome {
    pub(super) fn viewport_changed(self) -> bool {
        match self {
            Self::Ignored { viewport_changed }
            | Self::Forwarded { viewport_changed }
            | Self::Scrollback { viewport_changed } => viewport_changed,
        }
    }
}

pub(super) fn dispatch_keyboard_input_with<E, W>(
    grid: &Mutex<Grid>,
    scrollback_action: Option<ScrollbackAction>,
    encode: E,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    E: FnOnce(bool, bool) -> Option<Vec<u8>>,
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let (application_cursor_keys, kitty_disambiguate) = {
        let mut grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;

        if let Some(action) = scrollback_action {
            let viewport_changed = apply_scrollback_action(&mut grid, action);
            return Ok(KeyboardInputOutcome::Scrollback { viewport_changed });
        }

        (grid.application_cursor_keys, grid.kitty_disambiguate())
    };
    dispatch_encoded_terminal_input_with(
        grid,
        encode(application_cursor_keys, kitty_disambiguate),
        write,
    )
}

#[cfg(test)]
fn dispatch_mouse_button_with<W>(
    grid: &Mutex<Grid>,
    pointer_position: Option<PhysicalPosition<f64>>,
    cell_dimensions: Option<(u16, u16)>,
    state: ElementState,
    button: MouseButton,
    modifiers: ModifiersState,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    dispatch_mouse_button_inner_with(
        grid,
        pointer_position,
        cell_dimensions,
        state,
        button,
        modifiers,
        None,
        write,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_mouse_button_and_motion_with<W>(
    grid: &Mutex<Grid>,
    pointer_position: Option<PhysicalPosition<f64>>,
    cell_dimensions: Option<(u16, u16)>,
    state: ElementState,
    button: MouseButton,
    modifiers: ModifiersState,
    motion_state: &mut MouseMotionState,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    dispatch_mouse_button_inner_with(
        grid,
        pointer_position,
        cell_dimensions,
        state,
        button,
        modifiers,
        Some(motion_state),
        write,
    )
}

#[allow(clippy::too_many_arguments)]
fn dispatch_mouse_button_inner_with<W>(
    grid: &Mutex<Grid>,
    pointer_position: Option<PhysicalPosition<f64>>,
    cell_dimensions: Option<(u16, u16)>,
    state: ElementState,
    button: MouseButton,
    modifiers: ModifiersState,
    mut motion_state: Option<&mut MouseMotionState>,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let Some(button) = mouse_button_code_from_winit(button) else {
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    };
    let Some((pointer_position, cell_dimensions)) = pointer_position.zip(cell_dimensions) else {
        if let Some(motion_state) = motion_state.as_deref_mut() {
            motion_state.reset();
        }
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    };

    let (mouse_tracking, mouse_encoding, terminal_dimensions) = {
        let grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;
        if grid.mouse_tracking == MouseTracking::None && motion_state.is_none() {
            return Ok(KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            });
        }
        (
            grid.mouse_tracking,
            grid.mouse_encoding,
            terminal_dimensions_from_locked_grid(&grid)?,
        )
    };

    let Some((column, row)) = terminal_cell_at_mouse_button(
        pointer_position,
        cell_dimensions,
        terminal_dimensions,
        state,
    ) else {
        if let Some(motion_state) = motion_state.as_deref_mut() {
            motion_state.reset();
        }
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    };

    if mouse_tracking == MouseTracking::None {
        if let Some(motion_state) = motion_state {
            motion_state.anchor((column, row));
        }
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    }

    let button = mouse_button_with_modifiers(button, modifiers);
    let report = encode_mouse_event(button, column, row, state.is_pressed(), mouse_encoding);
    write(report)?;
    if let Some(motion_state) = motion_state {
        motion_state.anchor((column, row));
    }
    Ok(KeyboardInputOutcome::Forwarded {
        viewport_changed: false,
    })
}

pub(super) fn dispatch_mouse_motion_with<W>(
    grid: &Mutex<Grid>,
    pointer_position: PhysicalPosition<f64>,
    cell_dimensions: Option<(u16, u16)>,
    active_button: Option<u8>,
    modifiers: ModifiersState,
    last_cell: Option<(usize, usize)>,
    write: W,
) -> Result<MouseMotionDispatchOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let Some(cell_dimensions) = cell_dimensions else {
        return Ok(MouseMotionDispatchOutcome::Ignored);
    };
    let (mouse_tracking, mouse_encoding, terminal_dimensions) = {
        let grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;
        (
            grid.mouse_tracking,
            grid.mouse_encoding,
            terminal_dimensions_from_locked_grid(&grid)?,
        )
    };
    let Some(cell) = terminal_cell_at_mouse_motion(
        pointer_position,
        cell_dimensions,
        terminal_dimensions,
        active_button.is_some(),
    ) else {
        return Ok(MouseMotionDispatchOutcome::Ignored);
    };
    if last_cell == Some(cell) {
        return Ok(MouseMotionDispatchOutcome::Deduplicated);
    }

    let button = match mouse_tracking {
        MouseTracking::None | MouseTracking::Normal => {
            return Ok(MouseMotionDispatchOutcome::Observed(cell));
        }
        MouseTracking::ButtonEvent => {
            let Some(button) = active_button else {
                return Ok(MouseMotionDispatchOutcome::Observed(cell));
            };
            button
        }
        MouseTracking::AnyEvent => active_button.unwrap_or(MOUSE_NO_BUTTON),
    };
    let button = mouse_button_with_modifiers(button | MOUSE_MOTION_FLAG, modifiers);
    let report = encode_mouse_event(button, cell.0, cell.1, true, mouse_encoding);
    match write(report) {
        Ok(()) => Ok(MouseMotionDispatchOutcome::Enqueued(cell)),
        // Motion is coalescible. Keeping the previous cell makes the next event
        // retry while keyboard, button, wheel, IME, and focus input stay lossless.
        Err(TerminalWriteQueueError::Full) => Ok(MouseMotionDispatchOutcome::DroppedFull),
        Err(error @ TerminalWriteQueueError::Disconnected) => Err(error.into()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_mouse_wheel_with<W>(
    grid: &Mutex<Grid>,
    pointer_position: Option<PhysicalPosition<f64>>,
    cell_dimensions: Option<(u16, u16)>,
    delta: MouseScrollDelta,
    phase: TouchPhase,
    modifiers: ModifiersState,
    state: &mut MouseWheelState,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let (steps, route, terminal_dimensions) = {
        let mut grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;
        let route = mouse_wheel_route(&grid);
        if state.last_route.replace(route) != Some(route) || phase == TouchPhase::Started {
            state.line_remainder = 0.0;
        }
        let steps = accumulate_wheel_steps(
            delta,
            cell_dimensions.map(|(_, height)| height),
            &mut state.line_remainder,
        );
        if matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            state.line_remainder = 0.0;
        }
        if steps == 0 {
            return Ok(KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            });
        }

        let terminal_dimensions = match route {
            MouseWheelRoute::Scrollback => {
                let viewport_changed = apply_wheel_scrollback(&mut grid, steps);
                return Ok(KeyboardInputOutcome::Scrollback { viewport_changed });
            }
            MouseWheelRoute::AlternateScroll { .. } => None,
            MouseWheelRoute::Terminal(_) => Some(terminal_dimensions_from_locked_grid(&grid)?),
        };

        (steps, route, terminal_dimensions)
    };

    if let MouseWheelRoute::AlternateScroll {
        application_cursor_keys,
    } = route
    {
        write(encode_alternate_scroll_steps(
            steps,
            application_cursor_keys,
        ))?;
        return Ok(KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        });
    }

    let MouseWheelRoute::Terminal(mouse_encoding) = route else {
        unreachable!("scrollback wheel input returns while the Grid lock is held");
    };
    let terminal_dimensions =
        terminal_dimensions.expect("terminal mouse reporting should snapshot Grid dimensions");

    let Some((column, row)) =
        pointer_position
            .zip(cell_dimensions)
            .and_then(|(position, dimensions)| {
                terminal_cell_at_pointer(position, dimensions, terminal_dimensions)
            })
    else {
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    };

    let button = if steps.is_positive() {
        MOUSE_WHEEL_UP
    } else {
        MOUSE_WHEEL_DOWN
    };
    let button = mouse_button_with_modifiers(button, modifiers);
    let report = encode_mouse_event(button, column, row, true, mouse_encoding);
    let report_count = usize::try_from(steps.unsigned_abs().min(MAX_WHEEL_STEPS_PER_EVENT))
        .expect("bounded wheel report count should fit usize");
    let mut reports = Vec::with_capacity(report.len().saturating_mul(report_count));
    for _ in 0..report_count {
        reports.extend_from_slice(&report);
    }
    write(reports)?;
    Ok(KeyboardInputOutcome::Forwarded {
        viewport_changed: false,
    })
}

fn focus_report_bytes(focused: bool) -> &'static [u8] {
    if focused {
        FOCUS_IN_REPORT
    } else {
        FOCUS_OUT_REPORT
    }
}

pub(super) fn record_window_focus_change(
    last_window_focus: &mut Option<bool>,
    focused: bool,
) -> bool {
    last_window_focus.replace(focused) != Some(focused)
}

pub(super) fn dispatch_focus_event_with<W>(
    grid: &Mutex<Grid>,
    focused: bool,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let focus_events = grid
        .lock()
        .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?
        .focus_events;
    if !focus_events {
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    }

    write(focus_report_bytes(focused).to_vec())?;
    Ok(KeyboardInputOutcome::Forwarded {
        viewport_changed: false,
    })
}

pub(super) fn dispatch_encoded_terminal_input_with<W>(
    grid: &Mutex<Grid>,
    bytes: Option<Vec<u8>>,
    write: W,
) -> Result<KeyboardInputOutcome, KeyboardInputError>
where
    W: FnOnce(Vec<u8>) -> Result<(), TerminalWriteQueueError>,
{
    let Some(bytes) = bytes else {
        return Ok(KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        });
    };
    let viewport_changed = {
        let mut grid = grid
            .lock()
            .map_err(|_| KeyboardInputError::Grid(GridAccessError::Poisoned))?;
        let viewport_changed = grid.scroll_offset != 0;
        grid.scroll_to_bottom();
        viewport_changed
    };
    write(bytes)?;
    Ok(KeyboardInputOutcome::Forwarded { viewport_changed })
}

fn apply_scrollback_action(grid: &mut Grid, action: ScrollbackAction) -> bool {
    let previous_offset = grid.scroll_offset;
    let page_lines = grid.rows().saturating_sub(1).max(1);

    match action {
        ScrollbackAction::PageUp => grid.scroll_viewport_up(page_lines),
        ScrollbackAction::PageDown => grid.scroll_viewport_down(page_lines),
        ScrollbackAction::Top => grid.scroll_viewport_up(grid.scrollback_len()),
        ScrollbackAction::Bottom => grid.scroll_to_bottom(),
    }

    grid.scroll_offset != previous_offset
}

pub(super) fn modifiers_after_focus_change(
    current: ModifiersState,
    focused: bool,
) -> ModifiersState {
    if focused {
        current
    } else {
        ModifiersState::empty()
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
