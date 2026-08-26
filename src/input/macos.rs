use super::keyboard::{encode_terminal_key_with_modifiers, TerminalKey, TerminalKeyModifiers};
use super::keybind::KeyModifiers;
use super::mouse::mouse_button_with_modifier_flags;
use objc2_app_kit::{NSEvent, NSEventModifierFlags};

pub fn translate_key_event(event: &NSEvent, application_cursor_keys: bool) -> Option<Vec<u8>> {
    let key_code = event.keyCode();
    let modifiers = event.modifierFlags();

    if let Some(sequence) = encode_macos_terminal_key(key_code, modifiers, application_cursor_keys)
    {
        return Some(sequence);
    }

    let has_ctrl = modifiers.contains(NSEventModifierFlags::Control);
    let has_alt = modifiers.contains(NSEventModifierFlags::Option);

    let characters = event.characters().map(|characters| characters.to_string());
    let characters_ignoring_modifiers = has_alt
        .then(|| event.charactersIgnoringModifiers())
        .flatten()
        .map(|characters| characters.to_string());

    encode_macos_text_input(
        characters.as_deref(),
        characters_ignoring_modifiers.as_deref(),
        has_ctrl,
        has_alt,
    )
}

fn encode_macos_text_input(
    characters: Option<&str>,
    characters_ignoring_modifiers: Option<&str>,
    has_control: bool,
    has_option: bool,
) -> Option<Vec<u8>> {
    let text = if has_option {
        characters_ignoring_modifiers
    } else {
        characters
    }?;
    if text.is_empty() {
        return None;
    }

    let payload = if has_control {
        control_byte(text).map_or_else(|| text.as_bytes().to_vec(), |byte| vec![byte])
    } else {
        text.as_bytes().to_vec()
    };

    if !has_option {
        return Some(payload);
    }

    let mut sequence = Vec::with_capacity(payload.len() + 1);
    sequence.push(0x1b);
    sequence.extend_from_slice(&payload);
    Some(sequence)
}

fn control_byte(text: &str) -> Option<u8> {
    let character = text.chars().next()?.to_ascii_lowercase();
    match character {
        'a'..='z' => Some(character as u8 - b'a' + 1),
        '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        ' ' => Some(0x00),
        _ => None,
    }
}

pub(crate) fn mouse_button_with_appkit_modifiers(
    button: u8,
    modifiers: NSEventModifierFlags,
) -> u8 {
    mouse_button_with_modifier_flags(
        button,
        modifiers.contains(NSEventModifierFlags::Shift),
        modifiers.contains(NSEventModifierFlags::Option),
        modifiers.contains(NSEventModifierFlags::Control),
    )
}

pub(crate) fn key_modifiers_from_appkit(modifiers: NSEventModifierFlags) -> KeyModifiers {
    let mut key_modifiers = KeyModifiers::empty();
    if modifiers.contains(NSEventModifierFlags::Command) {
        key_modifiers.insert(KeyModifiers::CMD);
    }
    if modifiers.contains(NSEventModifierFlags::Shift) {
        key_modifiers.insert(KeyModifiers::SHIFT);
    }
    if modifiers.contains(NSEventModifierFlags::Control) {
        key_modifiers.insert(KeyModifiers::CTRL);
    }
    if modifiers.contains(NSEventModifierFlags::Option) {
        key_modifiers.insert(KeyModifiers::ALT);
    }
    key_modifiers
}

fn encode_macos_terminal_key(
    key_code: u16,
    modifiers: NSEventModifierFlags,
    application_cursor_keys: bool,
) -> Option<Vec<u8>> {
    let has_shift = modifiers.contains(NSEventModifierFlags::Shift);
    let key = terminal_key_from_key_code(key_code, has_shift)?;

    encode_terminal_key_with_modifiers(
        key,
        application_cursor_keys,
        TerminalKeyModifiers::new(
            has_shift,
            modifiers.contains(NSEventModifierFlags::Option),
            modifiers.contains(NSEventModifierFlags::Control),
        ),
    )
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
    use super::{
        encode_macos_terminal_key, encode_macos_text_input, key_modifiers_from_appkit,
        mouse_button_with_appkit_modifiers, terminal_key_from_key_code,
    };
    use crate::input::keybind::KeyModifiers;
    use crate::input::keyboard::TerminalKey;
    use crate::input::mouse::{MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP};
    use objc2_app_kit::NSEventModifierFlags;

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

    #[test]
    fn encodes_macos_modifiers_for_named_terminal_keys() {
        for application_cursor_keys in [false, true] {
            assert_eq!(
                encode_macos_terminal_key(
                    123,
                    NSEventModifierFlags::Control,
                    application_cursor_keys,
                )
                .as_deref(),
                Some(b"\x1b[1;5D".as_slice())
            );
        }
        assert_eq!(
            encode_macos_terminal_key(96, NSEventModifierFlags::Option, false).as_deref(),
            Some(b"\x1b[15;3~".as_slice())
        );
        assert_eq!(
            encode_macos_terminal_key(
                123,
                NSEventModifierFlags::Shift
                    | NSEventModifierFlags::Option
                    | NSEventModifierFlags::Control,
                false,
            )
            .as_deref(),
            Some(b"\x1b[1;8D".as_slice())
        );
        assert_eq!(
            encode_macos_terminal_key(48, NSEventModifierFlags::Shift, false).as_deref(),
            Some(b"\x1b[Z".as_slice())
        );
    }

    #[test]
    fn option_text_uses_characters_ignoring_modifiers() {
        assert_eq!(
            encode_macos_text_input(Some("∫"), Some("b"), false, true).as_deref(),
            Some(b"\x1bb".as_slice()),
        );
    }

    #[test]
    fn option_dead_key_uses_nonempty_base_character() {
        assert_eq!(
            encode_macos_text_input(Some(""), Some("e"), false, true).as_deref(),
            Some(b"\x1be".as_slice()),
        );
    }

    #[test]
    fn shift_option_preserves_shifted_base_character() {
        assert_eq!(
            encode_macos_text_input(Some("ı"), Some("B"), false, true).as_deref(),
            Some(b"\x1bB".as_slice()),
        );
    }

    #[test]
    fn text_without_option_preserves_unicode_and_composition() {
        assert_eq!(
            encode_macos_text_input(Some("é日本"), Some("e"), false, false).as_deref(),
            Some("é日本".as_bytes()),
        );
    }

    #[test]
    fn control_option_encodes_base_before_prefixing_escape() {
        assert_eq!(
            encode_macos_text_input(Some("å"), Some("a"), true, true).as_deref(),
            Some(b"\x1b\x01".as_slice()),
        );
        assert_eq!(
            encode_macos_text_input(Some("\0"), Some("@"), true, true).as_deref(),
            Some(b"\x1b\0".as_slice()),
        );
    }

    #[test]
    fn missing_or_empty_text_emits_nothing() {
        for (characters, characters_ignoring_modifiers, has_option) in [
            (None, None, false),
            (Some(""), None, false),
            (Some("∫"), None, true),
            (Some("∫"), Some(""), true),
        ] {
            assert_eq!(
                encode_macos_text_input(
                    characters,
                    characters_ignoring_modifiers,
                    false,
                    has_option,
                ),
                None,
            );
        }
    }

    #[test]
    fn maps_appkit_key_modifiers_and_ignores_irrelevant_flags() {
        let irrelevant = NSEventModifierFlags::CapsLock
            | NSEventModifierFlags::Function
            | NSEventModifierFlags::NumericPad;

        for mask in 0_u8..16 {
            let mut appkit = irrelevant;
            let mut expected = KeyModifiers::empty();
            for (bit, appkit_flag, key_modifier) in [
                (0, NSEventModifierFlags::Command, KeyModifiers::CMD),
                (1, NSEventModifierFlags::Shift, KeyModifiers::SHIFT),
                (2, NSEventModifierFlags::Control, KeyModifiers::CTRL),
                (3, NSEventModifierFlags::Option, KeyModifiers::ALT),
            ] {
                if mask & (1 << bit) != 0 {
                    appkit.insert(appkit_flag);
                    expected.insert(key_modifier);
                }
            }

            assert_eq!(
                key_modifiers_from_appkit(appkit),
                expected,
                "unexpected mapping for relevant modifier mask {mask:04b}",
            );
        }
    }

    #[test]
    fn macos_mouse_modifiers_map_exact_xterm_bits() {
        for (modifiers, expected) in [
            (NSEventModifierFlags::empty(), 64),
            (NSEventModifierFlags::Shift, 68),
            (NSEventModifierFlags::Option, 72),
            (NSEventModifierFlags::Control, 80),
            (NSEventModifierFlags::Shift | NSEventModifierFlags::Option, 76),
            (NSEventModifierFlags::Shift | NSEventModifierFlags::Control, 84),
            (NSEventModifierFlags::Option | NSEventModifierFlags::Control, 88),
            (
                NSEventModifierFlags::Shift
                    | NSEventModifierFlags::Option
                    | NSEventModifierFlags::Control,
                92,
            ),
        ] {
            assert_eq!(
                mouse_button_with_appkit_modifiers(MOUSE_WHEEL_UP, modifiers),
                expected,
            );
        }

        let ignored = NSEventModifierFlags::Command
            | NSEventModifierFlags::CapsLock
            | NSEventModifierFlags::Function
            | NSEventModifierFlags::NumericPad;
        assert_eq!(
            mouse_button_with_appkit_modifiers(MOUSE_WHEEL_UP, ignored),
            MOUSE_WHEEL_UP,
        );
        assert_eq!(
            mouse_button_with_appkit_modifiers(
                MOUSE_WHEEL_DOWN,
                ignored
                    | NSEventModifierFlags::Shift
                    | NSEventModifierFlags::Option
                    | NSEventModifierFlags::Control,
            ),
            93,
        );
    }
}
