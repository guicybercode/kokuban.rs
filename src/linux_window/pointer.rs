//! Pointer state per device (capture, wheel remainders, motion cells) and the
//! arithmetic that maps winit pointer input onto terminal cells and buttons.

use super::dispatch::MouseMotionDispatchOutcome;
use super::sizing::TerminalDimensions;
use crate::grid::Grid;
use crate::input::mouse::{
    mouse_button_with_modifier_flags, MouseWheelRoute, MAX_WHEEL_STEPS_PER_EVENT,
};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::ModifiersState;

#[derive(Debug, Default)]
pub(super) struct MouseWheelState {
    pub(super) line_remainder: f64,
    pub(super) last_route: Option<MouseWheelRoute>,
}

impl MouseWheelState {
    pub(super) fn reset(&mut self) {
        self.line_remainder = 0.0;
        self.last_route = None;
    }
}

#[derive(Debug, Default)]
pub(super) struct MouseMotionState {
    // Xterm reports character-cell transitions, not raw subpixel movement.
    pub(super) last_cell: Option<(usize, usize)>,
}

impl MouseMotionState {
    pub(super) fn reset(&mut self) {
        self.last_cell = None;
    }

    pub(super) fn anchor(&mut self, cell: (usize, usize)) {
        self.last_cell = Some(cell);
    }

    pub(super) fn record_dispatch(&mut self, outcome: MouseMotionDispatchOutcome) {
        if let MouseMotionDispatchOutcome::Observed(cell)
        | MouseMotionDispatchOutcome::Enqueued(cell) = outcome
        {
            self.anchor(cell);
        }
    }
}

#[derive(Debug)]
pub(super) struct PointerRouteState<D: Copy + Eq> {
    pub(super) active_device: Option<D>,
    pub(super) position: Option<PhysicalPosition<f64>>,
    pub(super) wheel_state: MouseWheelState,
    pub(super) motion_state: MouseMotionState,
    // Physical state is separate from successfully forwarded presses so 1002 can
    // become active while a button that was pressed under another mode is held.
    pub(super) held_buttons: u8,
    pub(super) held_button_order: [u8; 3],
    pub(super) held_button_count: u8,
    pub(super) forwarded_buttons: u8,
    pub(super) left_while_captured: bool,
    pub(super) captured_cell_dimensions: Option<(u16, u16)>,
    pub(super) position_uses_captured_metrics: bool,
}

impl<D: Copy + Eq> Default for PointerRouteState<D> {
    fn default() -> Self {
        Self {
            active_device: None,
            position: None,
            wheel_state: MouseWheelState::default(),
            motion_state: MouseMotionState::default(),
            held_buttons: 0,
            held_button_order: [0; 3],
            held_button_count: 0,
            forwarded_buttons: 0,
            left_while_captured: false,
            captured_cell_dimensions: None,
            position_uses_captured_metrics: false,
        }
    }
}

impl<D: Copy + Eq> PointerRouteState<D> {
    pub(super) fn reset(&mut self) {
        self.active_device = None;
        self.position = None;
        self.wheel_state.reset();
        self.motion_state.reset();
        self.held_buttons = 0;
        self.held_button_order = [0; 3];
        self.held_button_count = 0;
        self.forwarded_buttons = 0;
        self.left_while_captured = false;
        self.captured_cell_dimensions = None;
        self.position_uses_captured_metrics = false;
    }

    pub(super) fn select_device(&mut self, device_id: D) -> bool {
        if self.active_device == Some(device_id) {
            return true;
        }
        if self.has_pointer_capture() {
            return false;
        }
        self.reset();
        self.active_device = Some(device_id);
        true
    }

    pub(super) fn cursor_entered(&mut self, device_id: D) {
        if self.select_device(device_id) {
            self.left_while_captured = false;
        }
    }

    pub(super) fn cursor_moved(&mut self, device_id: D, position: PhysicalPosition<f64>) -> bool {
        if !self.select_device(device_id) {
            return false;
        }
        self.position = Some(position);
        self.position_uses_captured_metrics = false;
        true
    }

    pub(super) fn cursor_left(&mut self, device_id: D) {
        if self.active_device == Some(device_id) {
            self.motion_state.reset();
            if !self.has_pointer_capture() {
                self.reset();
            } else {
                self.left_while_captured = true;
            }
        }
    }

    pub(super) fn position_for(&self, device_id: D) -> Option<PhysicalPosition<f64>> {
        if self.active_device == Some(device_id) {
            self.position
        } else {
            None
        }
    }

    pub(super) fn select_wheel_device(
        &mut self,
        device_id: D,
    ) -> Option<Option<PhysicalPosition<f64>>> {
        if !self.select_device(device_id) {
            return None;
        }
        Some(if self.position_uses_captured_metrics {
            None
        } else {
            self.position
        })
    }

    pub(super) fn button_cell_dimensions_for(
        &self,
        device_id: D,
        state: ElementState,
        button: Option<u8>,
        current: Option<(u16, u16)>,
    ) -> Option<(u16, u16)> {
        if self.active_device != Some(device_id) || !self.position_uses_captured_metrics {
            return current;
        }
        let captured_release = state == ElementState::Released
            && button
                .filter(|button| *button <= 2)
                .is_some_and(|button| self.forwarded_buttons & (1 << button) != 0);
        if captured_release {
            self.captured_cell_dimensions.or(current)
        } else {
            None
        }
    }

    pub(super) fn scale_factor_changed(&mut self, current_cell_dimensions: Option<(u16, u16)>) {
        self.wheel_state.reset();
        self.motion_state.reset();
        if !self.has_pointer_capture() {
            self.reset();
        } else if self.forwarded_buttons != 0 {
            if !self.position_uses_captured_metrics {
                self.captured_cell_dimensions =
                    current_cell_dimensions.or(self.captured_cell_dimensions);
            }
            self.position_uses_captured_metrics = true;
        } else {
            self.position = None;
            self.captured_cell_dimensions = None;
            self.position_uses_captured_metrics = false;
        }
    }

    pub(super) fn record_physical_button_event(
        &mut self,
        device_id: D,
        state: ElementState,
        button: Option<u8>,
    ) -> bool {
        let Some(button) = button.filter(|button| *button <= 2) else {
            return self.active_device == Some(device_id);
        };
        match state {
            ElementState::Pressed => {
                if !self.select_device(device_id) {
                    return false;
                }
                self.remove_held_button(button);
                let index = usize::from(self.held_button_count);
                self.held_button_order[index] = button;
                self.held_button_count += 1;
                self.held_buttons |= 1 << button;
            }
            ElementState::Released => {
                if self.active_device != Some(device_id) {
                    return false;
                }
                self.remove_held_button(button);
            }
        }
        true
    }

    pub(super) fn active_motion_button(&self) -> Option<u8> {
        self.held_button_count
            .checked_sub(1)
            .map(|index| self.held_button_order[usize::from(index)])
    }

    pub(super) fn has_pointer_capture(&self) -> bool {
        self.held_buttons != 0 || self.forwarded_buttons != 0
    }

    pub(super) fn remove_held_button(&mut self, button: u8) {
        let count = usize::from(self.held_button_count);
        let Some(index) = self.held_button_order[..count]
            .iter()
            .position(|held| *held == button)
        else {
            self.held_buttons &= !(1 << button);
            return;
        };
        self.held_button_order.copy_within(index + 1..count, index);
        self.held_button_count -= 1;
        self.held_button_order[usize::from(self.held_button_count)] = 0;
        self.held_buttons &= !(1 << button);
    }

    pub(super) fn record_button_dispatch(
        &mut self,
        device_id: D,
        state: ElementState,
        button: Option<u8>,
        forwarded: bool,
        cell_dimensions: Option<(u16, u16)>,
    ) {
        let Some(button) = button.filter(|button| *button <= 2) else {
            return;
        };
        let mask = 1 << button;
        match state {
            ElementState::Pressed if forwarded && self.active_device == Some(device_id) => {
                self.forwarded_buttons |= mask;
                self.captured_cell_dimensions = cell_dimensions;
            }
            ElementState::Released if self.active_device == Some(device_id) => {
                self.forwarded_buttons &= !mask;
                if !self.has_pointer_capture() {
                    if self.left_while_captured {
                        self.reset();
                    } else {
                        if self.position_uses_captured_metrics {
                            self.position = None;
                            self.motion_state.reset();
                        }
                        self.captured_cell_dimensions = None;
                        self.position_uses_captured_metrics = false;
                    }
                }
            }
            _ => {}
        }
    }
}

pub(super) fn accumulate_wheel_steps(
    delta: MouseScrollDelta,
    cell_height: Option<u16>,
    remainder: &mut f64,
) -> i32 {
    let line_delta = match delta {
        MouseScrollDelta::LineDelta(_, vertical) => f64::from(vertical),
        MouseScrollDelta::PixelDelta(position) => {
            let Some(cell_height) = cell_height.filter(|height| *height != 0) else {
                *remainder = 0.0;
                return 0;
            };
            position.y / f64::from(cell_height)
        }
    };
    if !line_delta.is_finite() {
        *remainder = 0.0;
        return 0;
    }

    let total = *remainder + line_delta;
    if !total.is_finite() {
        *remainder = 0.0;
        return 0;
    }
    let whole_steps = total.trunc();
    let limit = f64::from(MAX_WHEEL_STEPS_PER_EVENT);
    if whole_steps.abs() > limit {
        *remainder = 0.0;
        return if whole_steps.is_sign_positive() {
            i32::try_from(MAX_WHEEL_STEPS_PER_EVENT).expect("wheel step limit should fit i32")
        } else {
            -i32::try_from(MAX_WHEEL_STEPS_PER_EVENT).expect("wheel step limit should fit i32")
        };
    }

    *remainder = total - whole_steps;
    whole_steps as i32
}

pub(super) fn terminal_cell_at_pointer(
    position: PhysicalPosition<f64>,
    cell_dimensions: (u16, u16),
    terminal_dimensions: TerminalDimensions,
) -> Option<(usize, usize)> {
    if !position.x.is_finite()
        || !position.y.is_finite()
        || position.x < 0.0
        || position.y < 0.0
        || cell_dimensions.0 == 0
        || cell_dimensions.1 == 0
        || terminal_dimensions.columns == 0
        || terminal_dimensions.rows == 0
    {
        return None;
    }

    let last_column = terminal_dimensions.columns - 1;
    let last_row = terminal_dimensions.rows - 1;
    let column = (position.x / f64::from(cell_dimensions.0))
        .floor()
        .min(f64::from(last_column)) as usize;
    let row = (position.y / f64::from(cell_dimensions.1))
        .floor()
        .min(f64::from(last_row)) as usize;
    Some((column + 1, row + 1))
}

pub(super) fn terminal_cell_at_mouse_button(
    position: PhysicalPosition<f64>,
    cell_dimensions: (u16, u16),
    terminal_dimensions: TerminalDimensions,
    state: ElementState,
) -> Option<(usize, usize)> {
    let position =
        if state == ElementState::Released && position.x.is_finite() && position.y.is_finite() {
            PhysicalPosition::new(position.x.max(0.0), position.y.max(0.0))
        } else {
            position
        };
    terminal_cell_at_pointer(position, cell_dimensions, terminal_dimensions)
}

pub(super) fn terminal_cell_at_mouse_motion(
    position: PhysicalPosition<f64>,
    cell_dimensions: (u16, u16),
    terminal_dimensions: TerminalDimensions,
    button_held: bool,
) -> Option<(usize, usize)> {
    let position = if button_held && position.x.is_finite() && position.y.is_finite() {
        PhysicalPosition::new(position.x.max(0.0), position.y.max(0.0))
    } else {
        position
    };
    terminal_cell_at_pointer(position, cell_dimensions, terminal_dimensions)
}

pub(super) fn apply_wheel_scrollback(grid: &mut Grid, steps: i32) -> bool {
    let previous_offset = grid.scroll_offset;
    let lines = usize::try_from(steps.unsigned_abs().min(MAX_WHEEL_STEPS_PER_EVENT))
        .expect("bounded wheel steps should fit usize");
    if steps.is_positive() {
        grid.scroll_viewport_up(lines);
    } else {
        grid.scroll_viewport_down(lines);
    }
    grid.scroll_offset != previous_offset
}

pub(super) fn mouse_button_with_modifiers(button: u8, modifiers: ModifiersState) -> u8 {
    mouse_button_with_modifier_flags(
        button,
        modifiers.shift_key(),
        modifiers.alt_key(),
        modifiers.control_key(),
    )
}

pub(super) fn mouse_button_code_from_winit(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        accumulate_wheel_steps, mouse_button_code_from_winit, mouse_button_with_modifiers,
        terminal_cell_at_pointer, PointerRouteState,
    };
    use crate::input::mouse::{MouseWheelRoute, MAX_WHEEL_STEPS_PER_EVENT, MOUSE_WHEEL_UP};
    use crate::linux_window::sizing::TerminalDimensions;
    use winit::dpi::PhysicalPosition;
    use winit::event::{ElementState, MouseButton, MouseScrollDelta};
    use winit::keyboard::ModifiersState;

    #[test]
    fn wheel_delta_accumulates_fractional_lines_and_bounds_each_event() {
        let mut remainder = 0.0;

        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::LineDelta(7.0, 0.5),
                Some(20),
                &mut remainder,
            ),
            0,
            "horizontal motion must not affect vertical wheel steps"
        );
        assert_eq!(remainder, 0.5);
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::LineDelta(0.0, 0.5),
                Some(20),
                &mut remainder,
            ),
            1
        );
        assert_eq!(remainder, 0.0);

        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -5.0)),
                Some(20),
                &mut remainder,
            ),
            0
        );
        assert_eq!(remainder, -0.25);
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -15.0)),
                Some(20),
                &mut remainder,
            ),
            -1
        );
        assert_eq!(remainder, 0.0);

        remainder = 0.75;
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 10.0)),
                None,
                &mut remainder,
            ),
            0
        );
        assert_eq!(remainder, 0.0);

        remainder = 0.5;
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::LineDelta(0.0, f32::NAN),
                Some(20),
                &mut remainder,
            ),
            0
        );
        assert_eq!(remainder, 0.0);
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, f64::INFINITY)),
                Some(20),
                &mut remainder,
            ),
            0
        );
        assert_eq!(remainder, 0.0);

        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::LineDelta(0.0, f32::MAX),
                Some(20),
                &mut remainder,
            ),
            i32::try_from(MAX_WHEEL_STEPS_PER_EVENT).unwrap()
        );
        assert_eq!(remainder, 0.0);
        assert_eq!(
            accumulate_wheel_steps(
                MouseScrollDelta::LineDelta(0.0, -f32::MAX),
                Some(20),
                &mut remainder,
            ),
            -i32::try_from(MAX_WHEEL_STEPS_PER_EVENT).unwrap()
        );
        assert_eq!(remainder, 0.0);
    }

    #[test]
    fn mouse_wheel_modifiers_match_xterm_button_bits() {
        for (modifiers, expected) in [
            (ModifiersState::empty(), MOUSE_WHEEL_UP),
            (ModifiersState::SUPER, MOUSE_WHEEL_UP),
            (ModifiersState::SHIFT, MOUSE_WHEEL_UP | 4),
            (ModifiersState::ALT, MOUSE_WHEEL_UP | 8),
            (ModifiersState::CONTROL, MOUSE_WHEEL_UP | 16),
            (
                ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CONTROL,
                MOUSE_WHEEL_UP | 4 | 8 | 16,
            ),
        ] {
            assert_eq!(
                mouse_button_with_modifiers(MOUSE_WHEEL_UP, modifiers),
                expected
            );
        }
    }

    #[test]
    fn maps_only_primary_winit_mouse_buttons_to_xterm_codes() {
        for (button, expected) in [
            (MouseButton::Left, Some(0)),
            (MouseButton::Middle, Some(1)),
            (MouseButton::Right, Some(2)),
            (MouseButton::Back, None),
            (MouseButton::Forward, None),
            (MouseButton::Other(0), None),
            (MouseButton::Other(u16::MAX), None),
        ] {
            assert_eq!(mouse_button_code_from_winit(button), expected);
        }
    }

    #[test]
    fn pointer_route_releases_chords_even_when_the_release_is_not_forwarded() {
        let mut pointer_route = PointerRouteState::<u8>::default();
        pointer_route.cursor_moved(1, PhysicalPosition::new(15.0, 25.0));
        pointer_route.record_button_dispatch(
            1,
            ElementState::Pressed,
            Some(0),
            true,
            Some((10, 20)),
        );
        pointer_route.record_button_dispatch(
            1,
            ElementState::Pressed,
            Some(2),
            true,
            Some((10, 20)),
        );
        assert_eq!(pointer_route.forwarded_buttons, 0b101);

        pointer_route.scale_factor_changed(Some((10, 20)));
        assert_eq!(pointer_route.select_wheel_device(1), Some(None));
        let moved_position = PhysicalPosition::new(45.0, 65.0);
        pointer_route.cursor_moved(1, moved_position);
        assert_eq!(
            pointer_route.select_wheel_device(1),
            Some(Some(moved_position))
        );
        assert!(!pointer_route.position_uses_captured_metrics);

        pointer_route.cursor_left(1);
        pointer_route.record_button_dispatch(
            2,
            ElementState::Released,
            Some(2),
            false,
            Some((10, 20)),
        );
        assert_eq!(
            pointer_route.forwarded_buttons, 0b101,
            "another device must not release a captured button"
        );

        pointer_route.record_button_dispatch(
            1,
            ElementState::Released,
            Some(0),
            false,
            Some((10, 20)),
        );
        assert_eq!(pointer_route.active_device, Some(1));
        assert_eq!(pointer_route.forwarded_buttons, 0b100);
        assert!(pointer_route.left_while_captured);

        pointer_route.record_button_dispatch(
            1,
            ElementState::Released,
            Some(2),
            false,
            Some((10, 20)),
        );
        assert_eq!(pointer_route.active_device, None);
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.forwarded_buttons, 0);
        assert!(!pointer_route.left_while_captured);
    }

    #[test]
    fn pointer_route_drops_stale_dpi_position_after_the_last_release() {
        let mut pointer_route = PointerRouteState::<u8>::default();
        let old_position = PhysicalPosition::new(15.0, 25.0);
        pointer_route.cursor_moved(1, old_position);
        pointer_route.record_button_dispatch(
            1,
            ElementState::Pressed,
            Some(0),
            true,
            Some((10, 20)),
        );
        pointer_route.scale_factor_changed(Some((10, 20)));
        assert_eq!(
            pointer_route.button_cell_dimensions_for(
                1,
                ElementState::Released,
                Some(0),
                Some((20, 40)),
            ),
            Some((10, 20))
        );

        pointer_route.record_button_dispatch(
            1,
            ElementState::Released,
            Some(0),
            false,
            Some((10, 20)),
        );
        assert_eq!(pointer_route.active_device, Some(1));
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.forwarded_buttons, 0);
        assert_eq!(pointer_route.captured_cell_dimensions, None);
        assert!(!pointer_route.position_uses_captured_metrics);

        let new_position = PhysicalPosition::new(35.0, 45.0);
        pointer_route.cursor_moved(1, new_position);
        assert_eq!(pointer_route.position_for(1), Some(new_position));
    }

    #[test]
    fn pointer_route_resets_on_device_changes_matching_leaves_and_scale_changes() {
        let mut pointer_route = PointerRouteState::<u8>::default();
        let second_position = PhysicalPosition::new(25.0, 45.0);

        pointer_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
        pointer_route.wheel_state.line_remainder = 0.5;
        pointer_route.wheel_state.last_route = Some(MouseWheelRoute::Scrollback);
        pointer_route.cursor_entered(2);
        assert_eq!(pointer_route.active_device, Some(2));
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.0);
        assert_eq!(pointer_route.wheel_state.last_route, None);

        pointer_route.cursor_moved(2, second_position);
        pointer_route.wheel_state.line_remainder = 0.75;
        pointer_route.wheel_state.last_route = Some(MouseWheelRoute::Scrollback);
        pointer_route.cursor_entered(2);
        pointer_route.cursor_left(1);
        assert_eq!(pointer_route.active_device, Some(2));
        assert_eq!(pointer_route.position, Some(second_position));
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.75);
        assert_eq!(
            pointer_route.wheel_state.last_route,
            Some(MouseWheelRoute::Scrollback)
        );

        pointer_route.cursor_left(2);
        assert_eq!(pointer_route.active_device, None);
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.0);
        assert_eq!(pointer_route.wheel_state.last_route, None);

        pointer_route.cursor_moved(1, PhysicalPosition::new(5.0, 5.0));
        pointer_route.wheel_state.line_remainder = 0.5;
        pointer_route.wheel_state.last_route = Some(MouseWheelRoute::Scrollback);
        pointer_route.scale_factor_changed(Some((10, 20)));
        assert_eq!(pointer_route.active_device, None);
        assert_eq!(pointer_route.position, None);
        assert_eq!(pointer_route.wheel_state.line_remainder, 0.0);
        assert_eq!(pointer_route.wheel_state.last_route, None);
        assert_eq!(pointer_route.forwarded_buttons, 0);
        assert_eq!(pointer_route.captured_cell_dimensions, None);
        assert!(!pointer_route.position_uses_captured_metrics);
    }

    #[test]
    fn pointer_route_preserves_physical_drag_identity_across_leave_and_dpi() {
        let mut route = PointerRouteState::<u8>::default();
        route.cursor_moved(1, PhysicalPosition::new(15.0, 25.0));
        route.motion_state.anchor((2, 2));
        assert!(route.record_physical_button_event(1, ElementState::Pressed, Some(0)));
        assert_eq!(route.active_motion_button(), Some(0));

        route.cursor_left(2);
        assert_eq!(route.active_device, Some(1));
        assert_eq!(route.motion_state.last_cell, Some((2, 2)));
        route.cursor_left(1);
        assert!(route.left_while_captured);
        assert_eq!(route.motion_state.last_cell, None);
        assert!(!route.cursor_moved(2, PhysicalPosition::new(55.0, 65.0)));
        assert!(!route.record_physical_button_event(2, ElementState::Pressed, Some(2)));
        assert!(!route.record_physical_button_event(2, ElementState::Released, Some(0)));
        assert_eq!(route.active_motion_button(), Some(0));

        route.scale_factor_changed(Some((10, 20)));
        assert_eq!(route.active_device, Some(1));
        assert_eq!(route.active_motion_button(), Some(0));
        assert_eq!(route.position, None);
        assert!(!route.position_uses_captured_metrics);
        assert_eq!(route.motion_state.last_cell, None);

        let new_position = PhysicalPosition::new(35.0, 45.0);
        route.cursor_entered(1);
        assert!(route.cursor_moved(1, new_position));
        assert_eq!(route.position_for(1), Some(new_position));
        assert!(!route.left_while_captured);
        assert!(route.record_physical_button_event(1, ElementState::Released, Some(0)));
        route.record_button_dispatch(1, ElementState::Released, Some(0), false, Some((20, 40)));
        assert_eq!(route.active_motion_button(), None);
        assert!(route.cursor_moved(2, PhysicalPosition::new(55.0, 65.0)));
        assert_eq!(route.active_device, Some(2));
    }

    #[test]
    fn pointer_positions_map_to_one_based_clamped_terminal_cells() {
        let terminal = TerminalDimensions {
            columns: 80,
            rows: 24,
        };

        assert_eq!(
            terminal_cell_at_pointer(PhysicalPosition::new(0.0, 0.0), (10, 20), terminal),
            Some((1, 1))
        );
        assert_eq!(
            terminal_cell_at_pointer(PhysicalPosition::new(9.999, 19.999), (10, 20), terminal,),
            Some((1, 1))
        );
        assert_eq!(
            terminal_cell_at_pointer(PhysicalPosition::new(10.0, 20.0), (10, 20), terminal),
            Some((2, 2))
        );
        assert_eq!(
            terminal_cell_at_pointer(
                PhysicalPosition::new(f64::MAX, f64::MAX),
                (10, 20),
                terminal,
            ),
            Some((80, 24)),
            "positions beyond a stale surface must clamp to the terminal edge"
        );

        for invalid in [
            terminal_cell_at_pointer(PhysicalPosition::new(-0.1, 0.0), (10, 20), terminal),
            terminal_cell_at_pointer(PhysicalPosition::new(0.0, -0.1), (10, 20), terminal),
            terminal_cell_at_pointer(PhysicalPosition::new(f64::NAN, 0.0), (10, 20), terminal),
            terminal_cell_at_pointer(
                PhysicalPosition::new(0.0, f64::INFINITY),
                (10, 20),
                terminal,
            ),
            terminal_cell_at_pointer(PhysicalPosition::new(0.0, 0.0), (0, 20), terminal),
            terminal_cell_at_pointer(PhysicalPosition::new(0.0, 0.0), (10, 0), terminal),
            terminal_cell_at_pointer(
                PhysicalPosition::new(0.0, 0.0),
                (10, 20),
                TerminalDimensions {
                    columns: 0,
                    rows: 24,
                },
            ),
            terminal_cell_at_pointer(
                PhysicalPosition::new(0.0, 0.0),
                (10, 20),
                TerminalDimensions {
                    columns: 80,
                    rows: 0,
                },
            ),
        ] {
            assert_eq!(invalid, None);
        }
    }
}
