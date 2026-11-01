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
    // Only `input::linux` maps a physical Insert key; macOS never produces it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
        let parameter = self.kitty_parameter();
        (parameter > 1).then_some(parameter)
    }

    /// Modifier parameter shared by xterm and the kitty protocol:
    /// `1 + shift(1) + alt(2) + ctrl(4)`.
    const fn kitty_parameter(self) -> u8 {
        1 + (self.shift as u8) + 2 * (self.alt as u8) + 4 * (self.control as u8)
    }

    const fn any(self) -> bool {
        self.shift || self.alt || self.control
    }

    const fn with_shift(self) -> Self {
        Self { shift: true, ..self }
    }
}

/// A key press as seen by the kitty keyboard protocol encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyKey {
    /// A key with a legacy escape sequence (Enter, arrows, F-keys, ...).
    Named(TerminalKey),
    /// A text-producing key, identified by the codepoint it produces with no
    /// modifiers held (lowercase / unshifted layout key).
    Text(char),
}

/// Encode a key press under kitty's "disambiguate escape codes" enhancement
/// (progressive enhancement bit 1, see
/// <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>).
///
/// At this level the goal is to make keys that collide in the legacy
/// encoding distinguishable:
///
/// * Escape is always `CSI 27 u` (never a bare `ESC`), so applications no
///   longer need an escape timeout.
/// * Enter, Tab and Backspace stay legacy (`\r`, `\t`, `DEL`) when pressed
///   alone, but become `CSI 13;m u`, `CSI 9;m u`, `CSI 127;m u` with any
///   modifier (so Shift+Enter is distinguishable from Enter).
/// * Text keys held with Ctrl or Alt become `CSI codepoint;m u`, where the
///   codepoint is the unshifted key and Shift is reported in `m`. This is
///   what turns Ctrl+I / Ctrl+M / Ctrl+[ into something other than
///   `\t` / `\r` / `ESC`.
/// * Plain text (no Ctrl/Alt) and the remaining named keys (arrows, Home,
///   F-keys, ...) keep their legacy encoding, as kitty does at this level.
///   Callers should prefer their regular text path for plain text so IME
///   input and shifted characters are preserved.
pub fn encode_key_kitty_disambiguate(
    key: KittyKey,
    application_cursor_keys: bool,
    modifiers: TerminalKeyModifiers,
) -> Option<Vec<u8>> {
    match key {
        KittyKey::Named(TerminalKey::Escape) => Some(csi_u(27, modifiers)),
        KittyKey::Named(TerminalKey::BackTab) => Some(csi_u(9, modifiers.with_shift())),
        KittyKey::Named(named @ (TerminalKey::Enter | TerminalKey::Tab | TerminalKey::Backspace)) => {
            if modifiers.any() {
                let codepoint = match named {
                    TerminalKey::Enter => 13,
                    TerminalKey::Tab => 9,
                    _ => 127,
                };
                Some(csi_u(codepoint, modifiers))
            } else {
                encode_terminal_key(named, application_cursor_keys)
            }
        }
        KittyKey::Named(named) => {
            encode_terminal_key_with_modifiers(named, application_cursor_keys, modifiers)
        }
        KittyKey::Text(codepoint) => {
            if modifiers.control || modifiers.alt {
                Some(csi_u(u32::from(codepoint), modifiers))
            } else {
                Some(codepoint.to_string().into_bytes())
            }
        }
    }
}

fn csi_u(codepoint: u32, modifiers: TerminalKeyModifiers) -> Vec<u8> {
    let parameter = modifiers.kitty_parameter();
    if parameter > 1 {
        format!("\x1b[{codepoint};{parameter}u").into_bytes()
    } else {
        format!("\x1b[{codepoint}u").into_bytes()
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
        encode_key_kitty_disambiguate, encode_terminal_key, encode_terminal_key_with_modifiers,
        KittyKey, TerminalKey, TerminalKeyModifiers,
    };

    #[test]
    fn kitty_disambiguate_encodes_colliding_keys_as_csi_u() {
        const NONE: TerminalKeyModifiers = TerminalKeyModifiers::new(false, false, false);
        const SHIFT: TerminalKeyModifiers = TerminalKeyModifiers::new(true, false, false);
        const ALT: TerminalKeyModifiers = TerminalKeyModifiers::new(false, true, false);
        const CTRL: TerminalKeyModifiers = TerminalKeyModifiers::new(false, false, true);
        const CTRL_SHIFT: TerminalKeyModifiers = TerminalKeyModifiers::new(true, false, true);

        let cases: &[(KittyKey, TerminalKeyModifiers, &[u8])] = &[
            (KittyKey::Named(TerminalKey::Escape), NONE, b"\x1b[27u"),
            (KittyKey::Named(TerminalKey::Escape), ALT, b"\x1b[27;3u"),
            (KittyKey::Text('i'), CTRL, b"\x1b[105;5u"),
            (KittyKey::Text('m'), CTRL, b"\x1b[109;5u"),
            (KittyKey::Text('['), CTRL, b"\x1b[91;5u"),
            (KittyKey::Named(TerminalKey::Tab), NONE, b"\t"),
            (KittyKey::Named(TerminalKey::Tab), CTRL, b"\x1b[9;5u"),
            (KittyKey::Named(TerminalKey::BackTab), NONE, b"\x1b[9;2u"),
            (KittyKey::Named(TerminalKey::Enter), NONE, b"\r"),
            (KittyKey::Named(TerminalKey::Enter), SHIFT, b"\x1b[13;2u"),
            (KittyKey::Named(TerminalKey::Backspace), NONE, b"\x7f"),
            (KittyKey::Named(TerminalKey::Backspace), ALT, b"\x1b[127;3u"),
            (KittyKey::Text('a'), CTRL_SHIFT, b"\x1b[97;6u"),
            (KittyKey::Text('x'), ALT, b"\x1b[120;3u"),
            (KittyKey::Text('a'), NONE, b"a"),
            (KittyKey::Text('\u{e9}'), ALT, b"\x1b[233;3u"),
        ];
        for (key, modifiers, expected) in cases {
            assert_eq!(
                encode_key_kitty_disambiguate(*key, false, *modifiers).as_deref(),
                Some(*expected),
                "{key:?} {modifiers:?}"
            );
        }
    }

    #[test]
    fn kitty_disambiguate_keeps_legacy_sequences_for_other_named_keys() {
        const NONE: TerminalKeyModifiers = TerminalKeyModifiers::new(false, false, false);
        const SHIFT: TerminalKeyModifiers = TerminalKeyModifiers::new(true, false, false);
        assert_eq!(
            encode_key_kitty_disambiguate(KittyKey::Named(TerminalKey::Up), false, NONE).as_deref(),
            Some(b"\x1b[A".as_slice())
        );
        assert_eq!(
            encode_key_kitty_disambiguate(KittyKey::Named(TerminalKey::Up), true, NONE).as_deref(),
            Some(b"\x1bOA".as_slice())
        );
        assert_eq!(
            encode_key_kitty_disambiguate(KittyKey::Named(TerminalKey::Left), false, SHIFT).as_deref(),
            Some(b"\x1b[1;2D".as_slice())
        );
        assert_eq!(
            encode_key_kitty_disambiguate(KittyKey::Named(TerminalKey::Function(5)), false, NONE)
                .as_deref(),
            Some(b"\x1b[15~".as_slice())
        );
        assert_eq!(
            encode_key_kitty_disambiguate(KittyKey::Named(TerminalKey::PageDown), false, NONE)
                .as_deref(),
            Some(b"\x1b[6~".as_slice())
        );
    }

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
