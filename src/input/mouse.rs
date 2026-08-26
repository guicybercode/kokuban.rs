use crate::grid::MouseEncoding;

pub(crate) const MOUSE_WHEEL_UP: u8 = 64;
pub(crate) const MOUSE_WHEEL_DOWN: u8 = 65;

const LEGACY_VALUE_OFFSET: u8 = 32;
const LEGACY_VALUE_MAX: u8 = u8::MAX - LEGACY_VALUE_OFFSET;
const LEGACY_RELEASE_BUTTON: u8 = 3;

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
                LEGACY_RELEASE_BUTTON
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
    use super::{encode_mouse_event, MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP};
    use crate::grid::MouseEncoding;

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
    fn legacy_clamps_pressed_button_and_uses_release_code_three() {
        assert_eq!(
            encode_mouse_event(u8::MAX, 1, 1, true, MouseEncoding::Default),
            [0x1b, b'[', b'M', 255, 33, 33]
        );
        assert_eq!(
            encode_mouse_event(u8::MAX, 1, 1, false, MouseEncoding::Default),
            [0x1b, b'[', b'M', 35, 33, 33]
        );
    }
}
