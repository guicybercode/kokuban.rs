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
    Delete,
    Function(u8),
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

#[cfg(test)]
mod tests {
    use super::{encode_terminal_key, TerminalKey};

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
    fn encodes_backtab_and_function_keys() {
        assert_eq!(
            encode_terminal_key(TerminalKey::Tab, true).as_deref(),
            Some(b"\t".as_slice())
        );
        assert_eq!(
            encode_terminal_key(TerminalKey::BackTab, false).as_deref(),
            Some(b"\x1b[Z".as_slice())
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
}
