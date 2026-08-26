use crate::grid::{Grid, MouseEncoding, MouseTracking};
use crate::input::keyboard::{encode_terminal_key, TerminalKey};

pub(crate) const MOUSE_WHEEL_UP: u8 = 64;
pub(crate) const MOUSE_WHEEL_DOWN: u8 = 65;
pub(crate) const MAX_WHEEL_STEPS_PER_EVENT: u32 = 32;
const MOUSE_SHIFT_MODIFIER: u8 = 4;
const MOUSE_META_MODIFIER: u8 = 8;
const MOUSE_CONTROL_MODIFIER: u8 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseWheelRoute {
    Scrollback,
    AlternateScroll { application_cursor_keys: bool },
    Terminal(MouseEncoding),
}

pub(crate) fn mouse_wheel_route(grid: &Grid) -> MouseWheelRoute {
    if grid.mouse_tracking != MouseTracking::None {
        MouseWheelRoute::Terminal(grid.mouse_encoding)
    } else if grid.alternate_scroll && grid.using_alt_screen {
        MouseWheelRoute::AlternateScroll {
            application_cursor_keys: grid.application_cursor_keys,
        }
    } else {
        MouseWheelRoute::Scrollback
    }
}

pub(crate) fn mouse_button_with_modifier_flags(
    button: u8,
    shift: bool,
    meta: bool,
    control: bool,
) -> u8 {
    let mut encoded = button;
    if shift {
        encoded |= MOUSE_SHIFT_MODIFIER;
    }
    if meta {
        encoded |= MOUSE_META_MODIFIER;
    }
    if control {
        encoded |= MOUSE_CONTROL_MODIFIER;
    }
    encoded
}

pub(crate) fn encode_alternate_scroll_steps(
    steps: i32,
    application_cursor_keys: bool,
) -> Vec<u8> {
    let key = if steps.is_positive() {
        TerminalKey::Up
    } else if steps.is_negative() {
        TerminalKey::Down
    } else {
        return Vec::new();
    };
    let sequence = encode_terminal_key(key, application_cursor_keys)
        .expect("cursor keys should always have a terminal encoding");
    sequence.repeat(
        usize::try_from(steps.unsigned_abs().min(MAX_WHEEL_STEPS_PER_EVENT))
            .expect("bounded wheel steps should fit usize"),
    )
}

const LEGACY_VALUE_OFFSET: u8 = 32;
const LEGACY_VALUE_MAX: u8 = u8::MAX - LEGACY_VALUE_OFFSET;
const LEGACY_RELEASE_BUTTON: u8 = 3;
const LEGACY_MODIFIER_MASK: u8 = 0b0001_1100;

pub(crate) fn encode_mouse_event(
    button: u8,
    column: usize,
    row: usize,
    pressed: bool,
    encoding: MouseEncoding,
) -> Vec<u8> {
    let column = column.max(1);
    let row = row.max(1);
    match encoding {
        MouseEncoding::Sgr => {
            let suffix = if pressed { 'M' } else { 'm' };
            format!("\x1b[<{button};{column};{row}{suffix}").into_bytes()
        }
        MouseEncoding::Default => {
            let button = if pressed {
                button.min(LEGACY_VALUE_MAX)
            } else {
                LEGACY_RELEASE_BUTTON | (button & LEGACY_MODIFIER_MASK)
            };
            vec![
                0x1b,
                b'[',
                b'M',
                button + LEGACY_VALUE_OFFSET,
                encode_legacy_coordinate(column),
                encode_legacy_coordinate(row),
            ]
        }
    }
}

fn encode_legacy_coordinate(coordinate: usize) -> u8 {
    u8::try_from(coordinate.min(usize::from(LEGACY_VALUE_MAX))).unwrap_or(LEGACY_VALUE_MAX)
        + LEGACY_VALUE_OFFSET
}

#[cfg(test)]
mod tests {
    use super::{
        encode_alternate_scroll_steps, encode_mouse_event, mouse_button_with_modifier_flags,
        mouse_wheel_route, MouseWheelRoute, MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP,
        MAX_WHEEL_STEPS_PER_EVENT,
    };
    use crate::grid::{Grid, MouseEncoding, MouseTracking};

    #[test]
    fn alternate_scroll_encodes_bounded_cursor_key_repetitions() {
        assert!(encode_alternate_scroll_steps(0, false).is_empty());
        assert_eq!(
            encode_alternate_scroll_steps(2, false),
            b"\x1b[A\x1b[A"
        );
        assert_eq!(
            encode_alternate_scroll_steps(-2, false),
            b"\x1b[B\x1b[B"
        );
        assert_eq!(
            encode_alternate_scroll_steps(2, true),
            b"\x1bOA\x1bOA"
        );
        assert_eq!(
            encode_alternate_scroll_steps(i32::MAX, false),
            b"\x1b[A".repeat(usize::try_from(MAX_WHEEL_STEPS_PER_EVENT).unwrap())
        );
    }

    #[test]
    fn wheel_route_prefers_mouse_tracking_then_active_alternate_scroll() {
        let mut grid = Grid::new(8, 4, 16);
        assert_eq!(mouse_wheel_route(&grid), MouseWheelRoute::Scrollback);

        grid.alternate_scroll = true;
        assert_eq!(mouse_wheel_route(&grid), MouseWheelRoute::Scrollback);

        grid.enter_alt_screen();
        assert_eq!(
            mouse_wheel_route(&grid),
            MouseWheelRoute::AlternateScroll {
                application_cursor_keys: false,
            }
        );

        grid.application_cursor_keys = true;
        assert_eq!(
            mouse_wheel_route(&grid),
            MouseWheelRoute::AlternateScroll {
                application_cursor_keys: true,
            }
        );

        grid.mouse_tracking = MouseTracking::Normal;
        grid.mouse_encoding = MouseEncoding::Sgr;
        assert_eq!(
            mouse_wheel_route(&grid),
            MouseWheelRoute::Terminal(MouseEncoding::Sgr)
        );

        grid.mouse_tracking = MouseTracking::None;
        grid.leave_alt_screen();
        assert!(grid.alternate_scroll);
        assert_eq!(mouse_wheel_route(&grid), MouseWheelRoute::Scrollback);
    }

    #[test]
    fn mouse_modifier_flags_match_xterm_button_bits() {
        for (shift, meta, control, expected) in [
            (false, false, false, 64),
            (true, false, false, 68),
            (false, true, false, 72),
            (false, false, true, 80),
            (true, true, false, 76),
            (true, false, true, 84),
            (false, true, true, 88),
            (true, true, true, 92),
        ] {
            assert_eq!(
                mouse_button_with_modifier_flags(MOUSE_WHEEL_UP, shift, meta, control),
                expected,
            );
        }
        assert_eq!(
            mouse_button_with_modifier_flags(MOUSE_WHEEL_DOWN, true, true, true),
            93,
        );
    }

    #[test]
    fn sgr_encodes_press_release_and_unbounded_coordinates() {
        assert_eq!(
            encode_mouse_event(0, 1, 2, true, MouseEncoding::Sgr),
            b"\x1b[<0;1;2M"
        );
        assert_eq!(
            encode_mouse_event(0, 1, 2, false, MouseEncoding::Sgr),
            b"\x1b[<0;1;2m"
        );
        assert_eq!(
            encode_mouse_event(0, 0, 0, true, MouseEncoding::Sgr),
            b"\x1b[<0;1;1M"
        );
        assert_eq!(
            encode_mouse_event(0, usize::MAX, usize::MAX, true, MouseEncoding::Sgr),
            format!("\x1b[<0;{};{}M", usize::MAX, usize::MAX).into_bytes()
        );
    }

    #[test]
    fn sgr_encodes_wheel_buttons_exactly() {
        assert_eq!(
            encode_mouse_event(MOUSE_WHEEL_UP, 80, 24, true, MouseEncoding::Sgr),
            b"\x1b[<64;80;24M"
        );
        assert_eq!(
            encode_mouse_event(MOUSE_WHEEL_DOWN, 80, 24, true, MouseEncoding::Sgr),
            b"\x1b[<65;80;24M"
        );
    }

    #[test]
    fn legacy_enforces_one_based_coordinates_and_clamps_without_wrapping() {
        assert_eq!(
            encode_mouse_event(0, 0, 0, true, MouseEncoding::Default),
            [0x1b, b'[', b'M', 32, 33, 33]
        );
        assert_eq!(
            encode_mouse_event(0, 223, 223, true, MouseEncoding::Default),
            [0x1b, b'[', b'M', 32, 255, 255]
        );
        assert_eq!(
            encode_mouse_event(0, 256, 300, true, MouseEncoding::Default),
            [0x1b, b'[', b'M', 32, 255, 255]
        );
        assert_eq!(
            encode_mouse_event(0, usize::MAX, usize::MAX, true, MouseEncoding::Default),
            [0x1b, b'[', b'M', 32, 255, 255]
        );
    }

    #[test]
    fn legacy_clamps_pressed_button() {
        assert_eq!(
            encode_mouse_event(u8::MAX, 1, 1, true, MouseEncoding::Default),
            [0x1b, b'[', b'M', 255, 33, 33]
        );
    }

    #[test]
    fn legacy_release_uses_code_three_and_preserves_modifiers() {
        assert_eq!(
            encode_mouse_event(0, 1, 1, false, MouseEncoding::Default),
            [0x1b, b'[', b'M', 35, 33, 33]
        );
        assert_eq!(
            encode_mouse_event(0b0001_1110, 1, 1, false, MouseEncoding::Default),
            [0x1b, b'[', b'M', 63, 33, 33]
        );
    }
}
