use super::keyboard::{encode_terminal_key, TerminalKey};
use objc2_app_kit::{NSEvent, NSEventModifierFlags};

pub fn translate_key_event(event: &NSEvent, application_cursor_keys: bool) -> Option<Vec<u8>> {
    let key_code = event.keyCode();
    let modifiers = event.modifierFlags();
    let has_ctrl = modifiers.contains(NSEventModifierFlags::Control);
    let has_alt = modifiers.contains(NSEventModifierFlags::Option);
    let has_shift = modifiers.contains(NSEventModifierFlags::Shift);

    if let Some(key) = terminal_key_from_key_code(key_code, has_shift) {
        return encode_terminal_key(key, application_cursor_keys);
    }

    let chars_str = event.characters()?.to_string();
    if chars_str.is_empty() {
        return None;
    }

    if has_ctrl {
        if let Some(c) = chars_str.chars().next() {
            let byte = match c.to_ascii_lowercase() {
                'a'..='z' => Some(c.to_ascii_lowercase() as u8 - b'a' + 1),
                '[' => Some(0x1b),
                '\\' => Some(0x1c),
                ']' => Some(0x1d),
                '^' => Some(0x1e),
                '_' => Some(0x1f),
                ' ' => Some(0x00),
                _ => None,
            };
            if let Some(byte) = byte {
                return Some(vec![byte]);
            }
        }
    }

    if has_alt {
        let mut bytes = vec![0x1b];
        bytes.extend_from_slice(chars_str.as_bytes());
        return Some(bytes);
    }

    Some(chars_str.into_bytes())
}

fn terminal_key_from_key_code(key_code: u16, has_shift: bool) -> Option<TerminalKey> {
    match key_code {
        36 => Some(TerminalKey::Enter),
        51 => Some(TerminalKey::Backspace),
        48 if has_shift => Some(TerminalKey::BackTab),
        48 => Some(TerminalKey::Tab),
        53 => Some(TerminalKey::Escape),
        126 => Some(TerminalKey::Up),
        125 => Some(TerminalKey::Down),
        124 => Some(TerminalKey::Right),
        123 => Some(TerminalKey::Left),
        115 => Some(TerminalKey::Home),
        119 => Some(TerminalKey::End),
        116 => Some(TerminalKey::PageUp),
        121 => Some(TerminalKey::PageDown),
        117 => Some(TerminalKey::Delete),
        122 => Some(TerminalKey::Function(1)),
        120 => Some(TerminalKey::Function(2)),
        99 => Some(TerminalKey::Function(3)),
        118 => Some(TerminalKey::Function(4)),
        96 => Some(TerminalKey::Function(5)),
        97 => Some(TerminalKey::Function(6)),
        98 => Some(TerminalKey::Function(7)),
        100 => Some(TerminalKey::Function(8)),
        101 => Some(TerminalKey::Function(9)),
        109 => Some(TerminalKey::Function(10)),
        103 => Some(TerminalKey::Function(11)),
        111 => Some(TerminalKey::Function(12)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::terminal_key_from_key_code;
    use crate::input::keyboard::TerminalKey;

    #[test]
    fn maps_tab_and_shift_tab() {
        assert_eq!(
            terminal_key_from_key_code(48, false),
            Some(TerminalKey::Tab)
        );
        assert_eq!(
            terminal_key_from_key_code(48, true),
            Some(TerminalKey::BackTab)
        );
    }

    #[test]
    fn maps_macos_function_key_codes() {
        let key_codes = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111];

        for (index, key_code) in key_codes.into_iter().enumerate() {
            assert_eq!(
                terminal_key_from_key_code(key_code, false),
                Some(TerminalKey::Function((index + 1) as u8))
            );
        }
    }
}
