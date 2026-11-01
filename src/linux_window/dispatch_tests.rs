use super::{
    apply_scrollback_action, dispatch_encoded_terminal_input_with, dispatch_focus_event_with,
    dispatch_keyboard_input_with, dispatch_mouse_button_and_motion_with,
    dispatch_mouse_button_with, dispatch_mouse_motion_with, dispatch_mouse_wheel_with,
    focus_report_bytes, modifiers_after_focus_change, record_window_focus_change,
    KeyboardInputError, KeyboardInputOutcome, MouseMotionDispatchOutcome,
};
use crate::grid::{Grid, MouseEncoding, MouseTracking};
use crate::input::linux::{ime_commit_payload, ImeCommitPayload, ScrollbackAction};
use crate::linux_window::pointer::{MouseMotionState, MouseWheelState, PointerRouteState};
use crate::linux_window::test_support::grid_with_scrollback;
use crate::linux_window::GridAccessError;
use crate::terminal_writer::TerminalWriteQueueError;
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase};
use winit::keyboard::ModifiersState;

#[test]
fn keyboard_forwarding_releases_the_grid_before_encoding_and_enqueueing() {
    let mut history = grid_with_scrollback(4, 10);
    history.application_cursor_keys = true;
    history.scroll_viewport_up(history.scrollback_len());
    let grid = Mutex::new(history);
    let written = RefCell::new(Vec::new());

    let outcome = dispatch_keyboard_input_with(
        &grid,
        None,
        |application_cursor_keys, _kitty_disambiguate| {
            assert!(application_cursor_keys);
            assert!(
                grid.try_lock().is_ok(),
                "encoder must run without the Grid lock"
            );
            Some(b"\x1bOA".to_vec())
        },
        |bytes| {
            assert!(
                grid.try_lock().is_ok(),
                "queue enqueue must run without the Grid lock"
            );
            written.borrow_mut().extend_from_slice(&bytes);
            Ok(())
        },
    )
    .expect("fake keyboard write should succeed");

    assert_eq!(
        outcome,
        KeyboardInputOutcome::Forwarded {
            viewport_changed: true,
        }
    );
    assert_eq!(written.into_inner(), b"\x1bOA");
    assert_eq!(
        grid.lock()
            .expect("keyboard dispatch should keep the grid available")
            .scroll_offset,
        0
    );
}

#[test]
fn ime_commit_is_queued_once_as_raw_utf8_outside_the_grid_lock() {
    let mut history = grid_with_scrollback(4, 10);
    history.bracketed_paste = true;
    history.scroll_viewport_up(history.scrollback_len());
    let grid = Mutex::new(history);
    let text = "é日本🙂\0".to_string();
    let expected = text.as_bytes().to_vec();
    let ImeCommitPayload::Bytes(bytes) = ime_commit_payload(text) else {
        panic!("normal IME text should fit the commit budget");
    };
    let writes = RefCell::new(Vec::new());

    let outcome = dispatch_encoded_terminal_input_with(&grid, Some(bytes), |bytes| {
        assert!(
            grid.try_lock().is_ok(),
            "IME enqueue must run without the Grid lock"
        );
        writes.borrow_mut().push(bytes);
        Ok(())
    })
    .expect("fake IME write should succeed");

    assert_eq!(
        outcome,
        KeyboardInputOutcome::Forwarded {
            viewport_changed: true,
        }
    );
    assert_eq!(
        writes.into_inner(),
        vec![expected],
        "IME text must not receive modifier or bracketed-paste framing"
    );
    assert_eq!(
        grid.lock()
            .expect("IME dispatch should keep the grid available")
            .scroll_offset,
        0
    );
}

#[test]
fn scrollback_shortcuts_navigate_locally_without_queueing_input() {
    let grid = Mutex::new(grid_with_scrollback(4, 10));
    let encode_calls = AtomicUsize::new(0);
    let enqueue_calls = AtomicUsize::new(0);
    let dispatch = |action| {
        dispatch_keyboard_input_with(
            &grid,
            Some(action),
            |_, _| {
                encode_calls.fetch_add(1, Ordering::Relaxed);
                Some(b"trap".to_vec())
            },
            |_| {
                enqueue_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect("local scrollback action should succeed")
    };

    assert_eq!(
        dispatch(ScrollbackAction::PageUp),
        KeyboardInputOutcome::Scrollback {
            viewport_changed: true,
        }
    );
    assert_eq!(
        grid.lock()
            .expect("scrollback dispatch should keep the grid available")
            .scroll_offset,
        3
    );
    assert_eq!(
        dispatch(ScrollbackAction::PageUp),
        KeyboardInputOutcome::Scrollback {
            viewport_changed: true,
        }
    );
    assert_eq!(
        grid.lock()
            .expect("scrollback dispatch should keep the grid available")
            .scroll_offset,
        6
    );
    assert_eq!(
        dispatch(ScrollbackAction::PageDown),
        KeyboardInputOutcome::Scrollback {
            viewport_changed: true,
        }
    );
    assert_eq!(
        grid.lock()
            .expect("scrollback dispatch should keep the grid available")
            .scroll_offset,
        3
    );
    assert_eq!(
        dispatch(ScrollbackAction::Top),
        KeyboardInputOutcome::Scrollback {
            viewport_changed: true,
        }
    );
    assert_eq!(
        grid.lock()
            .expect("scrollback dispatch should keep the grid available")
            .scroll_offset,
        10
    );
    assert_eq!(
        dispatch(ScrollbackAction::Top),
        KeyboardInputOutcome::Scrollback {
            viewport_changed: false,
        },
        "a shortcut at its limit must still be consumed"
    );
    assert_eq!(
        dispatch(ScrollbackAction::Bottom),
        KeyboardInputOutcome::Scrollback {
            viewport_changed: true,
        }
    );
    assert_eq!(
        grid.lock()
            .expect("scrollback dispatch should keep the grid available")
            .scroll_offset,
        0
    );
    assert_eq!(encode_calls.load(Ordering::Relaxed), 0);
    assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn one_row_scrollback_pages_by_one_line_without_underflow() {
    let mut grid = grid_with_scrollback(1, 3);

    assert!(apply_scrollback_action(&mut grid, ScrollbackAction::PageUp));
    assert_eq!(grid.scroll_offset, 1);
    assert!(apply_scrollback_action(
        &mut grid,
        ScrollbackAction::PageDown
    ));
    assert_eq!(grid.scroll_offset, 0);
}

#[test]
fn pointer_route_isolates_mouse_button_positions_by_device() {
    let mut history = grid_with_scrollback(4, 10);
    history.mouse_tracking = MouseTracking::Normal;
    history.mouse_encoding = MouseEncoding::Sgr;
    history.scroll_viewport_up(2);
    let grid = Mutex::new(history);
    let writes = RefCell::new(Vec::new());
    let mut pointer_route = PointerRouteState::<u8>::default();

    pointer_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
    assert_eq!(
        dispatch_mouse_button_with(
            &grid,
            pointer_route.position_for(2),
            Some((10, 20)),
            ElementState::Pressed,
            MouseButton::Left,
            ModifiersState::empty(),
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("a click from another pointer device should be ignored"),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert!(writes.borrow().is_empty());

    pointer_route.cursor_moved(2, PhysicalPosition::new(25.0, 45.0));
    assert_eq!(pointer_route.position_for(1), None);
    assert_eq!(
        dispatch_mouse_button_with(
            &grid,
            pointer_route.position_for(2),
            Some((10, 20)),
            ElementState::Pressed,
            MouseButton::Left,
            ModifiersState::empty(),
            |bytes| {
                assert!(
                    grid.try_lock().is_ok(),
                    "routed mouse button enqueue must run without the Grid lock"
                );
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("the active pointer device should use its own position"),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(writes.into_inner(), [b"\x1b[<0;3;3M".to_vec()]);
    assert_eq!(
        grid.lock()
            .expect("routed mouse button grid should remain available")
            .scroll_offset,
        2
    );
}

#[test]
fn pointer_route_keeps_a_forwarded_button_captured_until_its_release() {
    let mut history = grid_with_scrollback(4, 10);
    history.mouse_tracking = MouseTracking::Normal;
    history.mouse_encoding = MouseEncoding::Sgr;
    let grid = Mutex::new(history);
    let writes = RefCell::new(Vec::new());
    let mut pointer_route = PointerRouteState::<u8>::default();
    let first_position = PhysicalPosition::new(15.0, 25.0);
    let second_position = PhysicalPosition::new(75.0, 85.0);

    pointer_route.cursor_moved(1, first_position);
    let press = dispatch_mouse_button_with(
        &grid,
        pointer_route.position_for(1),
        Some((10, 20)),
        ElementState::Pressed,
        MouseButton::Left,
        ModifiersState::empty(),
        |bytes| {
            assert!(grid.try_lock().is_ok());
            writes.borrow_mut().push(bytes);
            Ok(())
        },
    )
    .expect("the first device press should be forwarded");
    pointer_route.record_button_dispatch(
        1,
        ElementState::Pressed,
        Some(0),
        matches!(
            press,
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false
            }
        ),
        Some((10, 20)),
    );
    assert_eq!(pointer_route.forwarded_buttons, 1);

    pointer_route.wheel_state.line_remainder = 0.5;
    pointer_route.cursor_entered(2);
    pointer_route.cursor_moved(2, second_position);
    assert_eq!(pointer_route.active_device, Some(1));
    assert_eq!(pointer_route.position_for(1), Some(first_position));
    assert_eq!(pointer_route.position_for(2), None);
    assert_eq!(pointer_route.select_wheel_device(2), None);
    assert_eq!(pointer_route.wheel_state.line_remainder, 0.5);

    let second_press = dispatch_mouse_button_with(
        &grid,
        pointer_route.position_for(2),
        Some((10, 20)),
        ElementState::Pressed,
        MouseButton::Left,
        ModifiersState::empty(),
        |bytes| {
            writes.borrow_mut().push(bytes);
            Ok(())
        },
    )
    .expect("a press from another device should fail closed during capture");
    assert_eq!(
        second_press,
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    pointer_route.record_button_dispatch(2, ElementState::Pressed, Some(0), false, Some((10, 20)));
    assert_eq!(pointer_route.forwarded_buttons, 1);

    pointer_route.cursor_left(1);
    assert!(pointer_route.left_while_captured);
    assert_eq!(pointer_route.position_for(1), Some(first_position));
    pointer_route.scale_factor_changed(Some((10, 20)));
    assert_eq!(pointer_route.active_device, Some(1));
    assert_eq!(pointer_route.forwarded_buttons, 1);
    assert_eq!(pointer_route.position_for(1), Some(first_position));
    assert_eq!(pointer_route.wheel_state.line_remainder, 0.0);
    assert!(pointer_route.position_uses_captured_metrics);
    let stale_wheel_position = pointer_route
        .select_wheel_device(1)
        .expect("the captured wheel device should keep its route");
    assert_eq!(stale_wheel_position, None);
    assert_eq!(
        dispatch_mouse_wheel_with(
            &grid,
            stale_wheel_position,
            Some((20, 40)),
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut pointer_route.wheel_state,
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("tracked wheel input should fail closed with stale DPI geometry"),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert_eq!(writes.borrow().len(), 1);

    grid.lock()
        .expect("DPI wheel grid should disable mouse tracking")
        .mouse_tracking = MouseTracking::None;
    assert_eq!(
        dispatch_mouse_wheel_with(
            &grid,
            stale_wheel_position,
            Some((20, 40)),
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut pointer_route.wheel_state,
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("local scrollback should not require fresh DPI pointer geometry"),
        KeyboardInputOutcome::Scrollback {
            viewport_changed: true,
        }
    );
    {
        let mut grid = grid
            .lock()
            .expect("DPI wheel grid should restore mouse tracking");
        grid.mouse_tracking = MouseTracking::Normal;
        grid.mouse_encoding = MouseEncoding::Sgr;
    }
    let release_cell_dimensions = pointer_route.button_cell_dimensions_for(
        1,
        ElementState::Released,
        Some(0),
        Some((20, 40)),
    );
    assert_eq!(release_cell_dimensions, Some((10, 20)));
    let release = dispatch_mouse_button_with(
        &grid,
        pointer_route.position_for(1),
        release_cell_dimensions,
        ElementState::Released,
        MouseButton::Left,
        ModifiersState::empty(),
        |bytes| {
            assert!(grid.try_lock().is_ok());
            writes.borrow_mut().push(bytes);
            Ok(())
        },
    )
    .expect("the captured device release should use its retained position");
    pointer_route.record_button_dispatch(
        1,
        ElementState::Released,
        Some(0),
        matches!(
            release,
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false
            }
        ),
        release_cell_dimensions,
    );

    assert_eq!(
        writes.into_inner(),
        [b"\x1b[<0;2;2M".to_vec(), b"\x1b[<0;2;2m".to_vec()]
    );
    assert_eq!(pointer_route.active_device, None);
    assert_eq!(pointer_route.position, None);
    assert_eq!(pointer_route.forwarded_buttons, 0);
    assert!(!pointer_route.left_while_captured);
    assert_eq!(pointer_route.captured_cell_dimensions, None);
    assert!(!pointer_route.position_uses_captured_metrics);

    pointer_route.cursor_moved(2, second_position);
    assert_eq!(pointer_route.active_device, Some(2));
    assert_eq!(pointer_route.position_for(2), Some(second_position));
}

#[test]
fn pointer_route_keeps_fractional_wheel_steps_per_active_device() {
    let grid = Mutex::new(grid_with_scrollback(4, 10));
    let mut pointer_route = PointerRouteState::<u8>::default();
    let writes = AtomicUsize::new(0);

    pointer_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
    let first_position = pointer_route
        .select_wheel_device(1)
        .expect("the active wheel device should be accepted");
    assert_eq!(
        dispatch_mouse_wheel_with(
            &grid,
            first_position,
            Some((10, 20)),
            MouseScrollDelta::LineDelta(0.0, 0.5),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut pointer_route.wheel_state,
            |_| {
                writes.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect("the first fractional wheel event should be accumulated"),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert_eq!(pointer_route.wheel_state.line_remainder, 0.5);

    let second_position = pointer_route
        .select_wheel_device(2)
        .expect("a wheel device switch without a capture should be accepted");
    assert_eq!(second_position, None);
    assert_eq!(pointer_route.wheel_state.line_remainder, 0.0);
    assert_eq!(
        dispatch_mouse_wheel_with(
            &grid,
            second_position,
            Some((10, 20)),
            MouseScrollDelta::LineDelta(0.0, 0.5),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut pointer_route.wheel_state,
            |_| {
                writes.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect("a different device should start its own wheel remainder"),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert_eq!(pointer_route.wheel_state.line_remainder, 0.5);
    assert_eq!(
        grid.lock()
            .expect("fractional wheel grid should remain available")
            .scroll_offset,
        0
    );
    assert_eq!(writes.load(Ordering::Relaxed), 0);
}

#[test]
fn pointer_route_requires_matching_position_only_for_tracked_wheel_input() {
    let mut tracked_history = grid_with_scrollback(4, 10);
    tracked_history.mouse_tracking = MouseTracking::Normal;
    tracked_history.mouse_encoding = MouseEncoding::Sgr;
    tracked_history.scroll_viewport_up(2);
    let tracked_grid = Mutex::new(tracked_history);
    let tracked_writes = RefCell::new(Vec::new());
    let mut tracked_route = PointerRouteState::<u8>::default();

    tracked_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
    let mismatched_position = tracked_route
        .select_wheel_device(2)
        .expect("a tracked wheel device switch should be accepted");
    assert_eq!(mismatched_position, None);
    assert_eq!(
        dispatch_mouse_wheel_with(
            &tracked_grid,
            mismatched_position,
            Some((10, 20)),
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut tracked_route.wheel_state,
            |bytes| {
                tracked_writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("tracked wheel input without a matching position should fail closed"),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert!(tracked_writes.borrow().is_empty());
    assert_eq!(
        tracked_grid
            .lock()
            .expect("tracked wheel grid should remain available")
            .scroll_offset,
        2,
        "tracked mismatches must neither enqueue input nor move scrollback"
    );

    tracked_route.cursor_moved(2, PhysicalPosition::new(25.0, 45.0));
    let matching_position = tracked_route
        .select_wheel_device(2)
        .expect("the matching tracked wheel device should be accepted");
    assert_eq!(
        dispatch_mouse_wheel_with(
            &tracked_grid,
            matching_position,
            Some((10, 20)),
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut tracked_route.wheel_state,
            |bytes| {
                assert!(
                    tracked_grid.try_lock().is_ok(),
                    "routed wheel enqueue must run without the Grid lock"
                );
                tracked_writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("matching tracked wheel input should preserve its encoded bytes"),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(tracked_writes.into_inner(), [b"\x1b[<64;3;3M".to_vec()]);

    let local_grid = Mutex::new(grid_with_scrollback(4, 10));
    let local_writes = AtomicUsize::new(0);
    let mut local_route = PointerRouteState::<u8>::default();
    local_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
    let mismatched_position = local_route
        .select_wheel_device(2)
        .expect("local wheel input should select its device without a position");
    assert_eq!(mismatched_position, None);
    assert_eq!(
        dispatch_mouse_wheel_with(
            &local_grid,
            mismatched_position,
            Some((10, 20)),
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut local_route.wheel_state,
            |_| {
                local_writes.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect("local scrollback should not require a pointer position"),
        KeyboardInputOutcome::Scrollback {
            viewport_changed: true,
        }
    );
    assert_eq!(
        local_grid
            .lock()
            .expect("local wheel grid should remain available")
            .scroll_offset,
        1
    );
    assert_eq!(local_writes.load(Ordering::Relaxed), 0);
}

#[test]
fn every_supported_mouse_tracking_mode_forwards_button_press_and_release() {
    for tracking in [
        MouseTracking::Normal,
        MouseTracking::ButtonEvent,
        MouseTracking::AnyEvent,
    ] {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = tracking;
        history.mouse_encoding = MouseEncoding::Sgr;
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let dispatch = |state| {
            dispatch_mouse_button_with(
                &grid,
                Some(PhysicalPosition::new(25.0, 45.0)),
                Some((10, 20)),
                state,
                MouseButton::Left,
                ModifiersState::empty(),
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("supported mouse tracking should forward button input")
        };

        assert_eq!(
            dispatch(ElementState::Pressed),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(
            dispatch(ElementState::Released),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(
            writes.into_inner(),
            [b"\x1b[<0;3;3M".to_vec(), b"\x1b[<0;3;3m".to_vec()]
        );
    }
}

#[test]
fn mouse_button_release_clamps_negative_grab_coordinates_to_the_terminal_edge() {
    let mut history = grid_with_scrollback(4, 10);
    history.mouse_tracking = MouseTracking::Normal;
    history.mouse_encoding = MouseEncoding::Sgr;
    let grid = Mutex::new(history);
    let writes = RefCell::new(Vec::new());
    let dispatch = |state, position| {
        dispatch_mouse_button_with(
            &grid,
            Some(position),
            Some((10, 20)),
            state,
            MouseButton::Left,
            ModifiersState::empty(),
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("implicit-grab mouse button input should be handled")
    };

    assert_eq!(
        dispatch(ElementState::Pressed, PhysicalPosition::new(15.0, 25.0)),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(
        dispatch(ElementState::Released, PhysicalPosition::new(-15.0, -25.0),),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(
        writes.into_inner(),
        [b"\x1b[<0;2;2M".to_vec(), b"\x1b[<0;1;1m".to_vec()]
    );
}

#[test]
fn mouse_buttons_encode_modifiers_without_holding_the_grid_lock_or_scrolling() {
    let mut history = grid_with_scrollback(4, 10);
    history.mouse_tracking = MouseTracking::Normal;
    history.mouse_encoding = MouseEncoding::Sgr;
    history.scroll_viewport_up(2);
    let grid = Mutex::new(history);
    let writes = RefCell::new(Vec::new());
    let modified = ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL;
    let dispatch = |state, button| {
        dispatch_mouse_button_with(
            &grid,
            Some(PhysicalPosition::new(25.0, 45.0)),
            Some((10, 20)),
            state,
            button,
            modified,
            |bytes| {
                assert!(
                    grid.try_lock().is_ok(),
                    "mouse button enqueue must run without the Grid lock"
                );
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("tracked mouse button should be queued")
    };

    assert_eq!(
        dispatch(ElementState::Pressed, MouseButton::Right),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(
        dispatch(ElementState::Released, MouseButton::Right),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );

    grid.lock()
        .expect("mouse button grid should select legacy encoding")
        .mouse_encoding = MouseEncoding::Default;
    assert_eq!(
        dispatch(ElementState::Pressed, MouseButton::Middle),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(
        dispatch(ElementState::Released, MouseButton::Middle),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );

    assert_eq!(
        writes.into_inner(),
        [
            b"\x1b[<30;3;3M".to_vec(),
            b"\x1b[<30;3;3m".to_vec(),
            vec![0x1b, b'[', b'M', 61, 35, 35],
            vec![0x1b, b'[', b'M', 63, 35, 35],
        ]
    );
    assert_eq!(
        grid.lock()
            .expect("mouse button grid should remain available")
            .scroll_offset,
        2,
        "tracked mouse buttons must preserve scrollback"
    );
}

#[test]
fn mouse_buttons_ignore_disabled_unsupported_or_missing_geometry_and_preserve_queue_errors() {
    let grid = Mutex::new(grid_with_scrollback(4, 10));
    let enqueue_calls = AtomicUsize::new(0);

    assert_eq!(
        dispatch_mouse_button_with(
            &grid,
            Some(PhysicalPosition::new(1.0, 1.0)),
            Some((10, 20)),
            ElementState::Pressed,
            MouseButton::Left,
            ModifiersState::empty(),
            |_| {
                enqueue_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect("disabled mouse tracking should ignore button input"),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );

    {
        let mut grid = grid
            .lock()
            .expect("mouse button grid should enable tracking");
        grid.mouse_tracking = MouseTracking::AnyEvent;
        grid.mouse_encoding = MouseEncoding::Sgr;
    }
    for (position, dimensions, button) in [
        (None, Some((10, 20)), MouseButton::Left),
        (
            Some(PhysicalPosition::new(1.0, 1.0)),
            None,
            MouseButton::Left,
        ),
        (
            Some(PhysicalPosition::new(-1.0, 1.0)),
            Some((10, 20)),
            MouseButton::Left,
        ),
        (
            Some(PhysicalPosition::new(1.0, 1.0)),
            Some((0, 20)),
            MouseButton::Left,
        ),
        (
            Some(PhysicalPosition::new(1.0, 1.0)),
            Some((10, 20)),
            MouseButton::Back,
        ),
        (
            Some(PhysicalPosition::new(1.0, 1.0)),
            Some((10, 20)),
            MouseButton::Forward,
        ),
        (
            Some(PhysicalPosition::new(1.0, 1.0)),
            Some((10, 20)),
            MouseButton::Other(9),
        ),
    ] {
        assert_eq!(
            dispatch_mouse_button_with(
                &grid,
                position,
                dimensions,
                ElementState::Pressed,
                button,
                ModifiersState::empty(),
                |_| {
                    enqueue_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .expect("invalid mouse button input should be ignored"),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
    }
    assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

    for queue_error in [
        TerminalWriteQueueError::Full,
        TerminalWriteQueueError::Disconnected,
    ] {
        let error = dispatch_mouse_button_with(
            &grid,
            Some(PhysicalPosition::new(1.0, 1.0)),
            Some((10, 20)),
            ElementState::Released,
            MouseButton::Left,
            ModifiersState::empty(),
            |_| {
                assert!(
                    grid.try_lock().is_ok(),
                    "mouse button queue errors must occur outside the Grid lock"
                );
                Err(queue_error)
            },
        )
        .expect_err("mouse button queue failures should be preserved");
        assert!(matches!(
            error,
            KeyboardInputError::Queue(actual) if actual == queue_error
        ));
    }
}

#[test]
fn mouse_motion_respects_xterm_tracking_modes() {
    let cases = [
        (
            MouseTracking::None,
            None,
            MouseMotionDispatchOutcome::Observed((3, 3)),
            None,
        ),
        (
            MouseTracking::Normal,
            Some(0),
            MouseMotionDispatchOutcome::Observed((3, 3)),
            None,
        ),
        (
            MouseTracking::ButtonEvent,
            None,
            MouseMotionDispatchOutcome::Observed((3, 3)),
            None,
        ),
        (
            MouseTracking::ButtonEvent,
            Some(0),
            MouseMotionDispatchOutcome::Enqueued((3, 3)),
            Some(b"\x1b[<32;3;3M".to_vec()),
        ),
        (
            MouseTracking::AnyEvent,
            None,
            MouseMotionDispatchOutcome::Enqueued((3, 3)),
            Some(b"\x1b[<35;3;3M".to_vec()),
        ),
        (
            MouseTracking::AnyEvent,
            Some(2),
            MouseMotionDispatchOutcome::Enqueued((3, 3)),
            Some(b"\x1b[<34;3;3M".to_vec()),
        ),
    ];

    for (tracking, active_button, expected_outcome, expected_write) in cases {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = tracking;
        history.mouse_encoding = MouseEncoding::Sgr;
        history.scroll_viewport_up(2);
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let outcome = dispatch_mouse_motion_with(
            &grid,
            PhysicalPosition::new(25.0, 45.0),
            Some((10, 20)),
            active_button,
            ModifiersState::empty(),
            None,
            |bytes| {
                assert!(
                    grid.try_lock().is_ok(),
                    "mouse motion enqueue must run without the Grid lock"
                );
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("supported mouse tracking mode should be handled");

        assert_eq!(outcome, expected_outcome, "tracking mode {tracking:?}");
        assert_eq!(
            writes.into_inner(),
            expected_write.into_iter().collect::<Vec<_>>()
        );
        assert_eq!(
            grid.lock()
                .expect("mouse motion grid should remain available")
                .scroll_offset,
            2,
            "mouse motion must preserve scrollback"
        );
    }
}

#[test]
fn mouse_motion_encodes_buttons_modifiers_and_both_encodings() {
    let cases = [
        (
            MouseEncoding::Sgr,
            Some(0),
            ModifiersState::empty(),
            b"\x1b[<32;3;3M".to_vec(),
        ),
        (
            MouseEncoding::Sgr,
            Some(1),
            ModifiersState::empty(),
            b"\x1b[<33;3;3M".to_vec(),
        ),
        (
            MouseEncoding::Sgr,
            Some(2),
            ModifiersState::empty(),
            b"\x1b[<34;3;3M".to_vec(),
        ),
        (
            MouseEncoding::Sgr,
            None,
            ModifiersState::empty(),
            b"\x1b[<35;3;3M".to_vec(),
        ),
        (
            MouseEncoding::Sgr,
            Some(0),
            ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL,
            b"\x1b[<60;3;3M".to_vec(),
        ),
        (
            MouseEncoding::Sgr,
            Some(0),
            ModifiersState::SUPER,
            b"\x1b[<32;3;3M".to_vec(),
        ),
        (
            MouseEncoding::Default,
            Some(0),
            ModifiersState::empty(),
            vec![0x1b, b'[', b'M', 64, 35, 35],
        ),
        (
            MouseEncoding::Default,
            Some(1),
            ModifiersState::empty(),
            vec![0x1b, b'[', b'M', 65, 35, 35],
        ),
        (
            MouseEncoding::Default,
            Some(2),
            ModifiersState::empty(),
            vec![0x1b, b'[', b'M', 66, 35, 35],
        ),
        (
            MouseEncoding::Default,
            None,
            ModifiersState::empty(),
            vec![0x1b, b'[', b'M', 67, 35, 35],
        ),
        (
            MouseEncoding::Default,
            Some(0),
            ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL,
            vec![0x1b, b'[', b'M', 92, 35, 35],
        ),
    ];

    for (encoding, active_button, modifiers, expected) in cases {
        let mut history = grid_with_scrollback(4, 10);
        history.mouse_tracking = MouseTracking::AnyEvent;
        history.mouse_encoding = encoding;
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());

        assert_eq!(
            dispatch_mouse_motion_with(
                &grid,
                PhysicalPosition::new(25.0, 45.0),
                Some((10, 20)),
                active_button,
                modifiers,
                None,
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("mouse motion should encode"),
            MouseMotionDispatchOutcome::Enqueued((3, 3))
        );
        assert_eq!(writes.into_inner(), [expected]);
    }
}

#[test]
fn physical_mouse_buttons_survive_tracking_transitions_and_follow_chord_order() {
    let mut history = grid_with_scrollback(4, 10);
    history.mouse_encoding = MouseEncoding::Sgr;
    let grid = Mutex::new(history);
    let writes = RefCell::new(Vec::new());
    let mut route = PointerRouteState::<u8>::default();
    assert!(route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0)));
    assert!(route.record_physical_button_event(1, ElementState::Pressed, Some(0)));
    assert_eq!(route.active_motion_button(), Some(0));
    assert!(!route.cursor_moved(2, PhysicalPosition::new(15.0, 25.0)));

    grid.lock()
        .expect("motion grid should enable button-event tracking")
        .mouse_tracking = MouseTracking::ButtonEvent;
    for (position, expected_button) in [
        (PhysicalPosition::new(15.0, 25.0), 32),
        (PhysicalPosition::new(25.0, 25.0), 34),
        (PhysicalPosition::new(35.0, 25.0), 33),
        (PhysicalPosition::new(45.0, 25.0), 34),
        (PhysicalPosition::new(55.0, 25.0), 32),
    ] {
        if expected_button == 34 && route.active_motion_button() == Some(0) {
            assert!(route.record_physical_button_event(1, ElementState::Pressed, Some(2),));
        } else if expected_button == 33 {
            assert!(route.record_physical_button_event(1, ElementState::Pressed, Some(1),));
        } else if expected_button == 34 && route.active_motion_button() == Some(1) {
            assert!(route.record_physical_button_event(1, ElementState::Released, Some(1),));
        } else if expected_button == 32 && route.active_motion_button() == Some(2) {
            assert!(route.record_physical_button_event(1, ElementState::Released, Some(2),));
        }
        assert_eq!(
            route.active_motion_button().map(|button| button | 32),
            Some(expected_button)
        );
        let outcome = dispatch_mouse_motion_with(
            &grid,
            position,
            Some((10, 20)),
            route.active_motion_button(),
            ModifiersState::empty(),
            route.motion_state.last_cell,
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("ordered chord motion should be queued");
        let MouseMotionDispatchOutcome::Enqueued(cell) = outcome else {
            panic!("expected an enqueued chord motion, got {outcome:?}");
        };
        route.motion_state.anchor(cell);
    }
    assert!(route.record_physical_button_event(1, ElementState::Released, Some(0)));
    assert_eq!(route.active_motion_button(), None);
    assert_eq!(
        dispatch_mouse_motion_with(
            &grid,
            PhysicalPosition::new(65.0, 25.0),
            Some((10, 20)),
            route.active_motion_button(),
            ModifiersState::empty(),
            route.motion_state.last_cell,
            |_| panic!("1002 without a held button must not enqueue"),
        )
        .expect("button-event hover should be observed"),
        MouseMotionDispatchOutcome::Observed((7, 2))
    );
    assert_eq!(
        writes.into_inner(),
        [
            b"\x1b[<32;2;2M".to_vec(),
            b"\x1b[<34;3;2M".to_vec(),
            b"\x1b[<33;4;2M".to_vec(),
            b"\x1b[<34;5;2M".to_vec(),
            b"\x1b[<32;6;2M".to_vec(),
        ]
    );
}

#[test]
fn mouse_motion_deduplicates_only_committed_cells_and_uses_current_context_next() {
    let mut history = grid_with_scrollback(4, 10);
    history.mouse_tracking = MouseTracking::AnyEvent;
    history.mouse_encoding = MouseEncoding::Sgr;
    let grid = Mutex::new(history);
    let writes = RefCell::new(Vec::new());
    let first_cell = PhysicalPosition::new(25.0, 45.0);
    let second_cell = PhysicalPosition::new(35.0, 45.0);
    let mut motion_state = MouseMotionState::default();

    let dropped = dispatch_mouse_motion_with(
        &grid,
        first_cell,
        Some((10, 20)),
        None,
        ModifiersState::empty(),
        motion_state.last_cell,
        |_| Err(TerminalWriteQueueError::Full),
    )
    .expect("a full queue should drop only mouse motion");
    assert_eq!(dropped, MouseMotionDispatchOutcome::DroppedFull);
    motion_state.record_dispatch(dropped);
    assert_eq!(
        motion_state.last_cell, None,
        "the production reducer must keep a dropped motion retryable"
    );

    let outcome = dispatch_mouse_motion_with(
        &grid,
        first_cell,
        Some((10, 20)),
        None,
        ModifiersState::empty(),
        motion_state.last_cell,
        |bytes| {
            writes.borrow_mut().push(bytes);
            Ok(())
        },
    )
    .expect("the same cell should retry after backpressure");
    assert_eq!(outcome, MouseMotionDispatchOutcome::Enqueued((3, 3)));
    motion_state.record_dispatch(outcome);
    assert_eq!(motion_state.last_cell, Some((3, 3)));

    grid.lock()
        .expect("motion grid should switch encoding")
        .mouse_encoding = MouseEncoding::Default;
    assert_eq!(
        dispatch_mouse_motion_with(
            &grid,
            first_cell,
            Some((10, 20)),
            None,
            ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL,
            motion_state.last_cell,
            |_| panic!("context changes in the same cell must stay deduplicated"),
        )
        .expect("same-cell context change should be handled"),
        MouseMotionDispatchOutcome::Deduplicated
    );

    let modified = ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL;
    let outcome = dispatch_mouse_motion_with(
        &grid,
        second_cell,
        Some((10, 20)),
        None,
        modified,
        motion_state.last_cell,
        |bytes| {
            writes.borrow_mut().push(bytes);
            Ok(())
        },
    )
    .expect("the next cell should use the current encoding and modifiers");
    assert_eq!(outcome, MouseMotionDispatchOutcome::Enqueued((4, 3)));
    motion_state.record_dispatch(outcome);

    assert_eq!(
        dispatch_mouse_motion_with(
            &grid,
            first_cell,
            Some((10, 20)),
            None,
            modified,
            motion_state.last_cell,
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("returning to an earlier cell should report again"),
        MouseMotionDispatchOutcome::Enqueued((3, 3))
    );
    assert_eq!(
        writes.into_inner(),
        [
            b"\x1b[<35;3;3M".to_vec(),
            vec![0x1b, b'[', b'M', 95, 36, 35],
            vec![0x1b, b'[', b'M', 95, 35, 35],
        ]
    );
}

#[test]
fn mouse_button_reports_anchor_motion_without_hiding_button_queue_failures() {
    let mut history = grid_with_scrollback(4, 10);
    history.mouse_tracking = MouseTracking::AnyEvent;
    history.mouse_encoding = MouseEncoding::Sgr;
    let grid = Mutex::new(history);
    let writes = RefCell::new(Vec::new());
    let mut motion_state = MouseMotionState::default();

    assert_eq!(
        dispatch_mouse_button_and_motion_with(
            &grid,
            Some(PhysicalPosition::new(25.0, 45.0)),
            Some((10, 20)),
            ElementState::Pressed,
            MouseButton::Left,
            ModifiersState::empty(),
            &mut motion_state,
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("button press should anchor motion"),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(motion_state.last_cell, Some((3, 3)));
    assert_eq!(
        dispatch_mouse_motion_with(
            &grid,
            PhysicalPosition::new(29.0, 49.0),
            Some((10, 20)),
            Some(0),
            ModifiersState::empty(),
            motion_state.last_cell,
            |_| panic!("same-cell motion after a button press must be deduplicated"),
        )
        .expect("same-cell motion should be handled"),
        MouseMotionDispatchOutcome::Deduplicated
    );

    motion_state.reset();
    let error = dispatch_mouse_button_and_motion_with(
        &grid,
        Some(PhysicalPosition::new(25.0, 45.0)),
        Some((10, 20)),
        ElementState::Pressed,
        MouseButton::Left,
        ModifiersState::empty(),
        &mut motion_state,
        |_| Err(TerminalWriteQueueError::Full),
    )
    .expect_err("button queue backpressure must remain fatal");
    assert!(matches!(
        error,
        KeyboardInputError::Queue(TerminalWriteQueueError::Full)
    ));
    assert_eq!(
        motion_state.last_cell, None,
        "a failed button report must not anchor motion"
    );
    assert_eq!(writes.into_inner(), [b"\x1b[<0;3;3M".to_vec()]);
}

#[test]
fn mouse_motion_clamps_only_finite_drags_and_propagates_disconnects() {
    let mut history = grid_with_scrollback(4, 10);
    history.mouse_tracking = MouseTracking::AnyEvent;
    history.mouse_encoding = MouseEncoding::Sgr;
    let grid = Mutex::new(history);
    let writes = RefCell::new(Vec::new());

    assert_eq!(
        dispatch_mouse_motion_with(
            &grid,
            PhysicalPosition::new(-25.0, -45.0),
            Some((10, 20)),
            Some(0),
            ModifiersState::empty(),
            None,
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("finite implicit-grab motion should clamp"),
        MouseMotionDispatchOutcome::Enqueued((1, 1))
    );
    for (position, button) in [
        (PhysicalPosition::new(-1.0, 1.0), None),
        (PhysicalPosition::new(f64::NAN, 1.0), Some(0)),
        (PhysicalPosition::new(1.0, f64::INFINITY), Some(0)),
    ] {
        assert_eq!(
            dispatch_mouse_motion_with(
                &grid,
                position,
                Some((10, 20)),
                button,
                ModifiersState::empty(),
                None,
                |_| panic!("invalid motion geometry must not enqueue"),
            )
            .expect("invalid motion geometry should be ignored"),
            MouseMotionDispatchOutcome::Ignored
        );
    }
    assert_eq!(
        dispatch_mouse_motion_with(
            &grid,
            PhysicalPosition::new(f64::MAX, f64::MAX),
            Some((10, 20)),
            None,
            ModifiersState::empty(),
            None,
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("positive motion beyond the surface should clamp"),
        MouseMotionDispatchOutcome::Enqueued((8, 4))
    );

    let error = dispatch_mouse_motion_with(
        &grid,
        PhysicalPosition::new(15.0, 25.0),
        Some((10, 20)),
        None,
        ModifiersState::empty(),
        None,
        |_| Err(TerminalWriteQueueError::Disconnected),
    )
    .expect_err("a disconnected motion queue must remain fatal");
    assert!(matches!(
        error,
        KeyboardInputError::Queue(TerminalWriteQueueError::Disconnected)
    ));
    assert_eq!(
        writes.into_inner(),
        [b"\x1b[<32;1;1M".to_vec(), b"\x1b[<35;8;4M".to_vec(),]
    );
}

#[test]
fn wheel_remainder_resets_between_gestures_routes_and_pointer_contexts() {
    let grid = Mutex::new(grid_with_scrollback(4, 10));
    let writes = RefCell::new(Vec::new());
    let mut state = MouseWheelState::default();
    let dispatch = |delta, phase, state: &mut MouseWheelState| {
        dispatch_mouse_wheel_with(
            &grid,
            Some(PhysicalPosition::new(1.0, 1.0)),
            Some((10, 20)),
            MouseScrollDelta::LineDelta(0.0, delta),
            phase,
            ModifiersState::empty(),
            state,
            |bytes| {
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("wheel gesture dispatch should succeed")
    };

    assert_eq!(
        dispatch(0.5, TouchPhase::Started, &mut state),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert_eq!(state.line_remainder, 0.5);
    assert_eq!(
        dispatch(0.0, TouchPhase::Ended, &mut state),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert_eq!(state.line_remainder, 0.0);

    assert_eq!(
        dispatch(0.5, TouchPhase::Moved, &mut state),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    {
        let mut grid = grid
            .lock()
            .expect("wheel grid should switch to SGR terminal reporting");
        grid.mouse_tracking = MouseTracking::Normal;
        grid.mouse_encoding = MouseEncoding::Sgr;
    }
    assert_eq!(
        dispatch(0.5, TouchPhase::Moved, &mut state),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        },
        "a local-scroll remainder must not become terminal input"
    );
    assert_eq!(state.line_remainder, 0.5);
    assert_eq!(
        dispatch(0.5, TouchPhase::Moved, &mut state),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );

    assert_eq!(
        dispatch(0.25, TouchPhase::Started, &mut state),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert_eq!(state.line_remainder, 0.25);
    assert_eq!(
        dispatch(0.25, TouchPhase::Cancelled, &mut state),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert_eq!(state.line_remainder, 0.0);
    assert_eq!(writes.into_inner(), [b"\x1b[<64;1;1M".to_vec()]);

    state.line_remainder = 0.75;
    state.reset();
    assert_eq!(state.line_remainder, 0.0);
    assert_eq!(state.last_route, None);
}

#[test]
fn every_supported_mouse_tracking_mode_forwards_wheel_reports() {
    for tracking in [
        MouseTracking::Normal,
        MouseTracking::ButtonEvent,
        MouseTracking::AnyEvent,
    ] {
        let mut history = grid_with_scrollback(4, 10);
        history.alternate_scroll = true;
        history.enter_alt_screen();
        history.mouse_tracking = tracking;
        history.mouse_encoding = MouseEncoding::Sgr;
        let grid = Mutex::new(history);
        let writes = RefCell::new(Vec::new());
        let mut state = MouseWheelState::default();

        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                Some(PhysicalPosition::new(1.0, 1.0)),
                Some((10, 20)),
                MouseScrollDelta::LineDelta(0.0, 1.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut state,
                |bytes| {
                    writes.borrow_mut().push(bytes);
                    Ok(())
                },
            )
            .expect("supported mouse tracking should forward wheel input"),
            KeyboardInputOutcome::Forwarded {
                viewport_changed: false,
            }
        );
        assert_eq!(writes.into_inner(), [b"\x1b[<64;1;1M".to_vec()]);
    }
}

#[test]
fn alternate_scroll_forwards_unmodified_cursor_keys_without_pointer_geometry() {
    let mut history = grid_with_scrollback(4, 10);
    history.alternate_scroll = true;
    history.enter_alt_screen();
    let grid = Mutex::new(history);
    let writes = RefCell::new(Vec::new());
    let mut state = MouseWheelState::default();
    let modified = ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL;

    assert_eq!(
        dispatch_mouse_wheel_with(
            &grid,
            None,
            None,
            MouseScrollDelta::LineDelta(0.0, 2.0),
            TouchPhase::Moved,
            modified,
            &mut state,
            |bytes| {
                assert!(
                    grid.try_lock().is_ok(),
                    "alternate scroll must enqueue outside the Grid lock"
                );
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("alternate scroll should forward normal cursor keys"),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );

    grid.lock()
        .expect("alternate scroll grid should enable application cursor keys")
        .application_cursor_keys = true;
    assert_eq!(
        dispatch_mouse_wheel_with(
            &grid,
            None,
            None,
            MouseScrollDelta::LineDelta(0.0, -2.0),
            TouchPhase::Moved,
            modified,
            &mut state,
            |bytes| {
                assert!(grid.try_lock().is_ok());
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("alternate scroll should honor application cursor key mode"),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );

    assert_eq!(
        writes.into_inner(),
        [b"\x1b[A\x1b[A".to_vec(), b"\x1bOB\x1bOB".to_vec()]
    );

    for queue_error in [
        TerminalWriteQueueError::Full,
        TerminalWriteQueueError::Disconnected,
    ] {
        let error = dispatch_mouse_wheel_with(
            &grid,
            None,
            None,
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut state,
            |_| {
                assert!(grid.try_lock().is_ok());
                Err(queue_error)
            },
        )
        .expect_err("alternate scroll queue failures should be preserved");
        assert!(matches!(
            error,
            KeyboardInputError::Queue(actual) if actual == queue_error
        ));
    }
}

#[test]
fn wheel_scrolls_locally_or_reports_tracking_without_holding_the_grid_lock() {
    let grid = Mutex::new(grid_with_scrollback(4, 10));
    let enqueue_calls = AtomicUsize::new(0);
    let mut wheel_state = MouseWheelState::default();

    assert_eq!(
        dispatch_mouse_wheel_with(
            &grid,
            None,
            None,
            MouseScrollDelta::LineDelta(0.0, 3.0),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut wheel_state,
            |_| {
                enqueue_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect("local wheel scrolling should succeed"),
        KeyboardInputOutcome::Scrollback {
            viewport_changed: true,
        }
    );
    assert_eq!(
        grid.lock()
            .expect("wheel grid should remain available")
            .scroll_offset,
        3
    );
    assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

    assert_eq!(
        dispatch_mouse_wheel_with(
            &grid,
            None,
            None,
            MouseScrollDelta::LineDelta(0.0, -2.0),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut wheel_state,
            |_| {
                enqueue_calls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect("local wheel scrolling down should succeed"),
        KeyboardInputOutcome::Scrollback {
            viewport_changed: true,
        }
    );
    assert_eq!(
        grid.lock()
            .expect("wheel grid should remain available")
            .scroll_offset,
        1
    );

    {
        let mut grid = grid.lock().expect("wheel grid should enable tracking");
        grid.mouse_tracking = MouseTracking::Normal;
        grid.mouse_encoding = MouseEncoding::Sgr;
    }
    let writes = RefCell::new(Vec::new());
    let modified = ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL;
    assert_eq!(
        dispatch_mouse_wheel_with(
            &grid,
            Some(PhysicalPosition::new(25.0, 45.0)),
            Some((10, 20)),
            MouseScrollDelta::LineDelta(0.0, 2.0),
            TouchPhase::Moved,
            modified,
            &mut wheel_state,
            |bytes| {
                assert!(
                    grid.try_lock().is_ok(),
                    "mouse enqueue must run without the Grid lock"
                );
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("tracked SGR wheel should succeed"),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(
        writes.borrow().as_slice(),
        [b"\x1b[<92;3;3M\x1b[<92;3;3M".to_vec()]
    );
    assert_eq!(
        grid.lock()
            .expect("tracked wheel grid should remain available")
            .scroll_offset,
        1,
        "tracked wheel input must preserve scrollback"
    );

    grid.lock()
        .expect("wheel grid should select legacy encoding")
        .mouse_encoding = MouseEncoding::Default;
    assert_eq!(
        dispatch_mouse_wheel_with(
            &grid,
            Some(PhysicalPosition::new(25.0, 45.0)),
            Some((10, 20)),
            MouseScrollDelta::LineDelta(0.0, -1.0),
            TouchPhase::Moved,
            modified,
            &mut wheel_state,
            |bytes| {
                assert!(grid.try_lock().is_ok());
                writes.borrow_mut().push(bytes);
                Ok(())
            },
        )
        .expect("tracked legacy wheel should succeed"),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(
        writes.into_inner(),
        [
            b"\x1b[<92;3;3M\x1b[<92;3;3M".to_vec(),
            vec![0x1b, b'[', b'M', 125, 35, 35],
        ]
    );
    assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn tracked_wheel_ignores_missing_geometry_and_preserves_queue_errors() {
    let mut history = grid_with_scrollback(4, 10);
    history.mouse_tracking = MouseTracking::AnyEvent;
    let grid = Mutex::new(history);
    let enqueue_calls = AtomicUsize::new(0);
    let mut wheel_state = MouseWheelState::default();

    for (position, cell_dimensions) in [
        (None, Some((10, 20))),
        (Some(PhysicalPosition::new(1.0, 1.0)), None),
        (Some(PhysicalPosition::new(-1.0, 1.0)), Some((10, 20))),
    ] {
        assert_eq!(
            dispatch_mouse_wheel_with(
                &grid,
                position,
                cell_dimensions,
                MouseScrollDelta::LineDelta(0.0, 1.0),
                TouchPhase::Moved,
                ModifiersState::empty(),
                &mut wheel_state,
                |_| {
                    enqueue_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                },
            )
            .expect("missing mouse geometry should be ignored"),
            KeyboardInputOutcome::Ignored {
                viewport_changed: false,
            }
        );
    }
    assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

    for queue_error in [
        TerminalWriteQueueError::Full,
        TerminalWriteQueueError::Disconnected,
    ] {
        let error = dispatch_mouse_wheel_with(
            &grid,
            Some(PhysicalPosition::new(1.0, 1.0)),
            Some((10, 20)),
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
            ModifiersState::empty(),
            &mut wheel_state,
            |_| {
                assert!(
                    grid.try_lock().is_ok(),
                    "mouse queue errors must be returned outside the Grid lock"
                );
                Err(queue_error)
            },
        )
        .expect_err("mouse queue failures should be preserved");
        assert!(matches!(
            error,
            KeyboardInputError::Queue(actual) if actual == queue_error
        ));
    }
}

#[test]
fn focus_loss_clears_stale_keyboard_modifiers() {
    let active = ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SHIFT;

    assert_eq!(modifiers_after_focus_change(active, true), active);
    assert_eq!(
        modifiers_after_focus_change(active, false),
        ModifiersState::empty()
    );
}

#[test]
fn repeated_window_focus_states_are_not_transitions() {
    let mut last_window_focus = None;

    assert!(record_window_focus_change(&mut last_window_focus, false));
    assert_eq!(last_window_focus, Some(false));
    assert!(!record_window_focus_change(&mut last_window_focus, false));
    assert!(record_window_focus_change(&mut last_window_focus, true));
    assert_eq!(last_window_focus, Some(true));
    assert!(!record_window_focus_change(&mut last_window_focus, true));
    assert!(record_window_focus_change(&mut last_window_focus, false));
}

#[test]
fn focus_reporting_is_gated_and_writes_exact_bytes_without_scrolling() {
    assert_eq!(focus_report_bytes(true), b"\x1b[I");
    assert_eq!(focus_report_bytes(false), b"\x1b[O");

    let mut history = grid_with_scrollback(3, 5);
    history.scroll_viewport_up(history.scrollback_len());
    let grid = Mutex::new(history);
    let writes = RefCell::new(Vec::new());
    let dispatch = |focused| {
        dispatch_focus_event_with(&grid, focused, |bytes| {
            assert!(
                grid.try_lock().is_ok(),
                "focus enqueue must run without the Grid lock"
            );
            writes.borrow_mut().push(bytes);
            Ok(())
        })
        .expect("fake focus report should succeed")
    };

    assert_eq!(
        dispatch(true),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert!(writes.borrow().is_empty());

    grid.lock()
        .expect("fresh Grid should enable focus reporting")
        .focus_events = true;
    assert_eq!(
        dispatch(true),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(
        dispatch(false),
        KeyboardInputOutcome::Forwarded {
            viewport_changed: false,
        }
    );
    assert_eq!(
        writes.into_inner(),
        [b"\x1b[I".to_vec(), b"\x1b[O".to_vec()]
    );
    assert_eq!(
        grid.lock()
            .expect("focus reporting should keep the Grid available")
            .scroll_offset,
        5,
        "focus reports must preserve the user's scrollback position"
    );

    for queue_error in [
        TerminalWriteQueueError::Full,
        TerminalWriteQueueError::Disconnected,
    ] {
        let error = dispatch_focus_event_with(&grid, true, |_| {
            assert!(
                grid.try_lock().is_ok(),
                "focus queue errors must be returned outside the Grid lock"
            );
            Err(queue_error)
        })
        .expect_err("focus queue failures should be preserved");
        assert!(matches!(
            error,
            KeyboardInputError::Queue(actual) if actual == queue_error
        ));
    }
}

#[test]
fn keyboard_forwarding_skips_empty_output_and_preserves_queue_errors() {
    let mut history = grid_with_scrollback(4, 6);
    history.scroll_viewport_up(history.scrollback_len());
    let grid = Mutex::new(history);
    let writes = AtomicUsize::new(0);

    assert_eq!(
        dispatch_keyboard_input_with(
            &grid,
            None,
            |_, _| None,
            |_| {
                writes.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .expect("empty keyboard output should be a no-op"),
        KeyboardInputOutcome::Ignored {
            viewport_changed: false,
        }
    );
    assert_eq!(
        grid.lock()
            .expect("keyboard dispatch should keep the grid available")
            .scroll_offset,
        6,
        "ignored keys must preserve the scrollback position"
    );
    assert_eq!(writes.load(Ordering::Relaxed), 0);

    let error = dispatch_keyboard_input_with(
        &grid,
        None,
        |_, _| Some(b"x".to_vec()),
        |_| Err(TerminalWriteQueueError::Full),
    )
    .expect_err("fake queue failure should be preserved");
    assert!(matches!(
        error,
        KeyboardInputError::Queue(TerminalWriteQueueError::Full)
    ));
}

#[test]
fn poisoned_grid_prevents_keyboard_ime_focus_and_mouse_queue_writes() {
    let grid = Arc::new(Mutex::new(Grid::new(80, 24, 0)));
    let poison_target = grid.clone();
    assert!(std::thread::spawn(move || {
        let _guard = poison_target
            .lock()
            .expect("fresh test grid should lock before poisoning");
        panic!("poison the keyboard test grid");
    })
    .join()
    .is_err());
    let encode_calls = AtomicUsize::new(0);
    let enqueue_calls = AtomicUsize::new(0);

    let error = dispatch_keyboard_input_with(
        grid.as_ref(),
        Some(ScrollbackAction::PageUp),
        |_, _| {
            encode_calls.fetch_add(1, Ordering::Relaxed);
            Some(b"x".to_vec())
        },
        |_| {
            enqueue_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
    )
    .expect_err("poisoned Grid must reject keyboard input");
    assert!(matches!(
        error,
        KeyboardInputError::Grid(GridAccessError::Poisoned)
    ));
    assert_eq!(encode_calls.load(Ordering::Relaxed), 0);

    let ime_error =
        dispatch_encoded_terminal_input_with(grid.as_ref(), Some("é".as_bytes().to_vec()), |_| {
            enqueue_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .expect_err("poisoned Grid must reject IME input");
    assert!(matches!(
        ime_error,
        KeyboardInputError::Grid(GridAccessError::Poisoned)
    ));
    assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

    let focus_error = dispatch_focus_event_with(grid.as_ref(), true, |_| {
        enqueue_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .expect_err("poisoned Grid must reject focus reporting");
    assert!(matches!(
        focus_error,
        KeyboardInputError::Grid(GridAccessError::Poisoned)
    ));
    assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

    let mut wheel_state = MouseWheelState::default();
    let mouse_error = dispatch_mouse_wheel_with(
        grid.as_ref(),
        Some(PhysicalPosition::new(1.0, 1.0)),
        Some((10, 20)),
        MouseScrollDelta::LineDelta(0.0, 1.0),
        TouchPhase::Moved,
        ModifiersState::empty(),
        &mut wheel_state,
        |_| {
            enqueue_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
    )
    .expect_err("poisoned Grid must reject mouse input");
    assert!(matches!(
        mouse_error,
        KeyboardInputError::Grid(GridAccessError::Poisoned)
    ));
    assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

    let mouse_button_error = dispatch_mouse_button_with(
        grid.as_ref(),
        Some(PhysicalPosition::new(1.0, 1.0)),
        Some((10, 20)),
        ElementState::Pressed,
        MouseButton::Left,
        ModifiersState::empty(),
        |_| {
            enqueue_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
    )
    .expect_err("poisoned Grid must reject mouse button input");
    assert!(matches!(
        mouse_button_error,
        KeyboardInputError::Grid(GridAccessError::Poisoned)
    ));
    assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);

    let mouse_motion_error = dispatch_mouse_motion_with(
        grid.as_ref(),
        PhysicalPosition::new(1.0, 1.0),
        Some((10, 20)),
        None,
        ModifiersState::empty(),
        None,
        |_| {
            enqueue_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
    )
    .expect_err("poisoned Grid must reject mouse motion");
    assert!(matches!(
        mouse_motion_error,
        KeyboardInputError::Grid(GridAccessError::Poisoned)
    ));
    assert_eq!(enqueue_calls.load(Ordering::Relaxed), 0);
}
