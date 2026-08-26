#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKey {
    Enter,
    Backspace,
    Tab,
    BackTab,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Function(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalKeyModifiers {
    shift: bool,
    alt: bool,
    control: bool,
}

impl TerminalKeyModifiers {
    pub const fn new(shift: bool, alt: bool, control: bool) -> Self {
        Self {
            shift,
            alt,
            control,
        }
    }

    fn xterm_parameter(self) -> Option<u8> {
        let parameter =
            1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.control);
        (parameter > 1).then_some(parameter)
    }
}

pub fn encode_terminal_key(key: TerminalKey, application_cursor_keys: bool) -> Option<Vec<u8>> {
    let sequence: &[u8] = match key {
        TerminalKey::Enter => b"\r",
        TerminalKey::Backspace => b"\x7f",
        TerminalKey::Tab => b"\t",
        TerminalKey::BackTab => b"\x1b[Z",
        TerminalKey::Escape => b"\x1b",
        TerminalKey::Up if application_cursor_keys => b"\x1bOA",
        TerminalKey::Down if application_cursor_keys => b"\x1bOB",
        TerminalKey::Right if application_cursor_keys => b"\x1bOC",
        TerminalKey::Left if application_cursor_keys => b"\x1bOD",
        TerminalKey::Home if application_cursor_keys => b"\x1bOH",
        TerminalKey::End if application_cursor_keys => b"\x1bOF",
        TerminalKey::Up => b"\x1b[A",
        TerminalKey::Down => b"\x1b[B",
        TerminalKey::Right => b"\x1b[C",
        TerminalKey::Left => b"\x1b[D",
        TerminalKey::Home => b"\x1b[H",
        TerminalKey::End => b"\x1b[F",
        TerminalKey::PageUp => b"\x1b[5~",
        TerminalKey::PageDown => b"\x1b[6~",
        TerminalKey::Insert => b"\x1b[2~",
        TerminalKey::Delete => b"\x1b[3~",
        TerminalKey::Function(1) => b"\x1bOP",
        TerminalKey::Function(2) => b"\x1bOQ",
        TerminalKey::Function(3) => b"\x1bOR",
        TerminalKey::Function(4) => b"\x1bOS",
        TerminalKey::Function(5) => b"\x1b[15~",
        TerminalKey::Function(6) => b"\x1b[17~",
        TerminalKey::Function(7) => b"\x1b[18~",
        TerminalKey::Function(8) => b"\x1b[19~",
        TerminalKey::Function(9) => b"\x1b[20~",
        TerminalKey::Function(10) => b"\x1b[21~",
        TerminalKey::Function(11) => b"\x1b[23~",
        TerminalKey::Function(12) => b"\x1b[24~",
        TerminalKey::Function(_) => return None,
    };

    Some(sequence.to_vec())
}

pub fn encode_terminal_key_with_modifiers(
    key: TerminalKey,
    application_cursor_keys: bool,
    modifiers: TerminalKeyModifiers,
) -> Option<Vec<u8>> {
    if let Some(parameter) = modifiers.xterm_parameter() {
        if let Some(sequence) = encode_xterm_modified_key(key, parameter) {
            return Some(sequence);
        }
    }

    let mut sequence = encode_terminal_key(key, application_cursor_keys)?;
    if modifiers.alt {
        sequence.insert(0, 0x1b);
    }
    Some(sequence)
}

fn encode_xterm_modified_key(key: TerminalKey, parameter: u8) -> Option<Vec<u8>> {
    let (prefix, final_byte): (&[u8], u8) = match key {
        TerminalKey::Up => (b"\x1b[1;", b'A'),
        TerminalKey::Down => (b"\x1b[1;", b'B'),
        TerminalKey::Right => (b"\x1b[1;", b'C'),
        TerminalKey::Left => (b"\x1b[1;", b'D'),
        TerminalKey::Home => (b"\x1b[1;", b'H'),
        TerminalKey::End => (b"\x1b[1;", b'F'),
        TerminalKey::PageUp => (b"\x1b[5;", b'~'),
        TerminalKey::PageDown => (b"\x1b[6;", b'~'),
        TerminalKey::Insert => (b"\x1b[2;", b'~'),
        TerminalKey::Delete => (b"\x1b[3;", b'~'),
        TerminalKey::Function(1) => (b"\x1b[1;", b'P'),
        TerminalKey::Function(2) => (b"\x1b[1;", b'Q'),
        TerminalKey::Function(3) => (b"\x1b[1;", b'R'),
        TerminalKey::Function(4) => (b"\x1b[1;", b'S'),
        TerminalKey::Function(5) => (b"\x1b[15;", b'~'),
        TerminalKey::Function(6) => (b"\x1b[17;", b'~'),
        TerminalKey::Function(7) => (b"\x1b[18;", b'~'),
        TerminalKey::Function(8) => (b"\x1b[19;", b'~'),
        TerminalKey::Function(9) => (b"\x1b[20;", b'~'),
        TerminalKey::Function(10) => (b"\x1b[21;", b'~'),
        TerminalKey::Function(11) => (b"\x1b[23;", b'~'),
        TerminalKey::Function(12) => (b"\x1b[24;", b'~'),
        _ => return None,
    };

    debug_assert!((2..=8).contains(&parameter));
    let mut sequence = Vec::with_capacity(prefix.len() + 2);
    sequence.extend_from_slice(prefix);
    sequence.push(b'0' + parameter);
    sequence.push(final_byte);
    Some(sequence)
}

#[cfg(test)]
mod tests {
    use super::{
        encode_terminal_key, encode_terminal_key_with_modifiers, TerminalKey, TerminalKeyModifiers,
    };

    #[test]
    fn encodes_cursor_keys_in_normal_and_application_modes() {
        let cases = [
            (TerminalKey::Up, b"\x1b[A".as_slice(), b"\x1bOA".as_slice()),
            (
                TerminalKey::Down,
                b"\x1b[B".as_slice(),
                b"\x1bOB".as_slice(),
            ),
            (
                TerminalKey::Right,
                b"\x1b[C".as_slice(),
                b"\x1bOC".as_slice(),
            ),
            (
                TerminalKey::Left,
                b"\x1b[D".as_slice(),
                b"\x1bOD".as_slice(),
            ),
            (
                TerminalKey::Home,
                b"\x1b[H".as_slice(),
                b"\x1bOH".as_slice(),
            ),
            (TerminalKey::End, b"\x1b[F".as_slice(), b"\x1bOF".as_slice()),
        ];

        for (key, normal, application) in cases {
            assert_eq!(encode_terminal_key(key, false).as_deref(), Some(normal));
            assert_eq!(encode_terminal_key(key, true).as_deref(), Some(application));
        }
    }

    #[test]
    fn encodes_backtab_editing_and_function_keys() {
        assert_eq!(
            encode_terminal_key(TerminalKey::Tab, true).as_deref(),
            Some(b"\t".as_slice())
        );
        assert_eq!(
            encode_terminal_key(TerminalKey::BackTab, false).as_deref(),
            Some(b"\x1b[Z".as_slice())
        );
        for application_cursor_keys in [false, true] {
            assert_eq!(
                encode_terminal_key(TerminalKey::Insert, application_cursor_keys).as_deref(),
                Some(b"\x1b[2~".as_slice())
            );
        }
        assert_eq!(
            encode_terminal_key(TerminalKey::Delete, true).as_deref(),
            Some(b"\x1b[3~".as_slice())
        );

        let expected = [
            b"\x1bOP".as_slice(),
            b"\x1bOQ".as_slice(),
            b"\x1bOR".as_slice(),
            b"\x1bOS".as_slice(),
            b"\x1b[15~".as_slice(),
            b"\x1b[17~".as_slice(),
            b"\x1b[18~".as_slice(),
            b"\x1b[19~".as_slice(),
            b"\x1b[20~".as_slice(),
            b"\x1b[21~".as_slice(),
            b"\x1b[23~".as_slice(),
            b"\x1b[24~".as_slice(),
        ];
        for (index, sequence) in expected.into_iter().enumerate() {
            assert_eq!(
                encode_terminal_key(TerminalKey::Function((index + 1) as u8), false).as_deref(),
                Some(sequence)
            );
        }
        assert_eq!(
            encode_terminal_key(TerminalKey::Function(5), true).as_deref(),
            Some(b"\x1b[15~".as_slice())
        );
        assert_eq!(encode_terminal_key(TerminalKey::Function(0), false), None);
        assert_eq!(encode_terminal_key(TerminalKey::Function(13), false), None);
    }

    #[test]
    fn encodes_every_xterm_modifier_parameter() {
        let cases = [
            (TerminalKeyModifiers::new(true, false, false), b"\x1b[1;2A"),
            (TerminalKeyModifiers::new(false, true, false), b"\x1b[1;3A"),
            (TerminalKeyModifiers::new(true, true, false), b"\x1b[1;4A"),
            (TerminalKeyModifiers::new(false, false, true), b"\x1b[1;5A"),
            (TerminalKeyModifiers::new(true, false, true), b"\x1b[1;6A"),
            (TerminalKeyModifiers::new(false, true, true), b"\x1b[1;7A"),
            (TerminalKeyModifiers::new(true, true, true), b"\x1b[1;8A"),
        ];

        for (modifiers, expected) in cases {
            assert_eq!(
                encode_terminal_key_with_modifiers(TerminalKey::Up, false, modifiers).as_deref(),
                Some(expected.as_slice())
            );
        }
    }

    #[test]
    fn encodes_modified_cursor_editing_and_function_key_families() {
        let control = TerminalKeyModifiers::new(false, false, true);
        let cases = [
            (TerminalKey::Up, b"\x1b[1;5A".as_slice()),
            (TerminalKey::Down, b"\x1b[1;5B".as_slice()),
            (TerminalKey::Right, b"\x1b[1;5C".as_slice()),
            (TerminalKey::Left, b"\x1b[1;5D".as_slice()),
            (TerminalKey::Home, b"\x1b[1;5H".as_slice()),
            (TerminalKey::End, b"\x1b[1;5F".as_slice()),
            (TerminalKey::Insert, b"\x1b[2;5~".as_slice()),
            (TerminalKey::Delete, b"\x1b[3;5~".as_slice()),
            (TerminalKey::PageUp, b"\x1b[5;5~".as_slice()),
            (TerminalKey::PageDown, b"\x1b[6;5~".as_slice()),
            (TerminalKey::Function(1), b"\x1b[1;5P".as_slice()),
            (TerminalKey::Function(2), b"\x1b[1;5Q".as_slice()),
            (TerminalKey::Function(3), b"\x1b[1;5R".as_slice()),
            (TerminalKey::Function(4), b"\x1b[1;5S".as_slice()),
            (TerminalKey::Function(5), b"\x1b[15;5~".as_slice()),
            (TerminalKey::Function(6), b"\x1b[17;5~".as_slice()),
            (TerminalKey::Function(7), b"\x1b[18;5~".as_slice()),
            (TerminalKey::Function(8), b"\x1b[19;5~".as_slice()),
            (TerminalKey::Function(9), b"\x1b[20;5~".as_slice()),
            (TerminalKey::Function(10), b"\x1b[21;5~".as_slice()),
            (TerminalKey::Function(11), b"\x1b[23;5~".as_slice()),
            (TerminalKey::Function(12), b"\x1b[24;5~".as_slice()),
        ];

        for (key, expected) in cases {
            assert_eq!(
                encode_terminal_key_with_modifiers(key, false, control).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn modified_cursor_keys_use_csi_in_both_cursor_modes() {
        let shift_control = TerminalKeyModifiers::new(true, false, true);

        for application_cursor_keys in [false, true] {
            assert_eq!(
                encode_terminal_key_with_modifiers(
                    TerminalKey::Home,
                    application_cursor_keys,
                    shift_control,
                )
                .as_deref(),
                Some(b"\x1b[1;6H".as_slice())
            );
        }
    }

    #[test]
    fn non_parameterized_named_keys_keep_traditional_alt_prefixes() {
        let alt = TerminalKeyModifiers::new(false, true, false);
        let shift_alt = TerminalKeyModifiers::new(true, true, false);

        assert_eq!(
            encode_terminal_key_with_modifiers(TerminalKey::Enter, false, alt).as_deref(),
            Some(b"\x1b\r".as_slice())
        );
        assert_eq!(
            encode_terminal_key_with_modifiers(TerminalKey::BackTab, false, shift_alt).as_deref(),
            Some(b"\x1b\x1b[Z".as_slice())
        );
        assert_eq!(
            encode_terminal_key_with_modifiers(TerminalKey::Function(0), false, alt),
            None
        );
        assert_eq!(
            encode_terminal_key_with_modifiers(TerminalKey::Function(13), false, alt),
            None
        );
    }
}
