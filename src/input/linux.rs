use super::keyboard::{
    encode_key_kitty_disambiguate, encode_terminal_key_with_modifiers, KittyKey, TerminalKey,
    TerminalKeyModifiers,
};
use crate::terminal_writer::TERMINAL_INPUT_LOSSLESS_BYTE_HEADROOM;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

const MAX_IME_COMMIT_BYTES: usize = TERMINAL_INPUT_LOSSLESS_BYTE_HEADROOM;

#[derive(Debug, Clone, Copy)]
pub(crate) struct LinuxKeyPress<'a> {
    named_key: Option<NamedKey>,
    text: Option<&'a str>,
    control_text: Option<&'a str>,
    /// Character the key produces with no modifiers held (kitty codepoint).
    base_char: Option<char>,
    modifiers: ModifiersState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollbackAction {
    PageUp,
    PageDown,
    Top,
    Bottom,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ImeCommitPayload {
    Empty,
    Bytes(Vec<u8>),
    TooLarge { byte_count: usize, limit: usize },
}

pub(crate) fn ime_commit_payload(text: String) -> ImeCommitPayload {
    ime_commit_payload_with_limit(text, MAX_IME_COMMIT_BYTES)
}

fn ime_commit_payload_with_limit(text: String, limit: usize) -> ImeCommitPayload {
    if text.is_empty() {
        return ImeCommitPayload::Empty;
    }

    let byte_count = text.len();
    if byte_count > limit {
        return ImeCommitPayload::TooLarge { byte_count, limit };
    }

    let mut bytes = text.into_bytes();
    if bytes.capacity() > limit {
        bytes = bytes.into_boxed_slice().into_vec();
    }
    debug_assert!(bytes.capacity() <= limit);
    ImeCommitPayload::Bytes(bytes)
}

pub(crate) fn key_press_from_winit<'a>(
    event: &'a KeyEvent,
    is_synthetic: bool,
    modifiers: ModifiersState,
) -> Option<LinuxKeyPress<'a>> {
    if !should_forward_key_event(event.state, is_synthetic, event.repeat, modifiers) {
        return None;
    }

    Some(LinuxKeyPress {
        named_key: named_key_from_logical(event.logical_key.as_ref()),
        text: event.text.as_deref(),
        control_text: event.text_with_all_modifiers(),
        base_char: base_char_from_key(event.key_without_modifiers()),
        modifiers,
    })
}

fn base_char_from_key(key: Key) -> Option<char> {
    match key {
        Key::Character(text) => text.chars().next(),
        _ => None,
    }
}

pub(crate) fn scrollback_action_from_winit(
    event: &KeyEvent,
    is_synthetic: bool,
    modifiers: ModifiersState,
) -> Option<ScrollbackAction> {
    scrollback_action_for_key_event(
        event.logical_key.as_ref(),
        event.state,
        is_synthetic,
        event.repeat,
        modifiers,
    )
}

pub(crate) fn encode_key_press(
    press: LinuxKeyPress<'_>,
    application_cursor_keys: bool,
    kitty_disambiguate: bool,
) -> Option<Vec<u8>> {
    if press.modifiers.super_key() {
        return None;
    }

    if kitty_disambiguate {
        if let Some(bytes) = encode_key_press_kitty(press, application_cursor_keys) {
            return Some(bytes);
        }
        // Plain text falls through to the regular path below.
    }

    // Winit also exposes text for named keys such as Enter. Resolve the named
    // key first so one physical press can never emit both representations.
    let mut bytes = match press
        .named_key
        .and_then(|named_key| terminal_key_from_named(named_key, press.modifiers.shift_key()))
    {
        Some(terminal_key) => {
            return encode_terminal_key_with_modifiers(
                terminal_key,
                application_cursor_keys,
                TerminalKeyModifiers::new(
                    press.modifiers.shift_key(),
                    press.modifiers.alt_key(),
                    press.modifiers.control_key(),
                ),
            );
        }
        None => {
            let text = if press.modifiers.control_key() {
                press.control_text?
            } else {
                press.text?
            };
            if text.is_empty() {
                return None;
            }
            text.as_bytes().to_vec()
        }
    };

    if press.modifiers.alt_key() {
        // Preserve the traditional terminal Meta behavior for text. Named
        // keys use xterm modifier parameters where the protocol defines them.
        let mut prefixed = Vec::with_capacity(bytes.len() + 1);
        prefixed.push(0x1b);
        prefixed.append(&mut bytes);
        bytes = prefixed;
    }

    Some(bytes)
}

/// Kitty "disambiguate escape codes" encoding. Named keys always go through
/// the kitty encoder; text keys only with Ctrl or Alt held, using the
/// unmodified key character (falling back to the produced text) lowercased.
/// Returns `None` for plain text so IME input and shifted characters keep
/// using the legacy text path.
fn encode_key_press_kitty(
    press: LinuxKeyPress<'_>,
    application_cursor_keys: bool,
) -> Option<Vec<u8>> {
    let has_shift = press.modifiers.shift_key();
    let has_alt = press.modifiers.alt_key();
    let has_ctrl = press.modifiers.control_key();
    let modifiers = TerminalKeyModifiers::new(has_shift, has_alt, has_ctrl);

    if let Some(key) = press
        .named_key
        .and_then(|named_key| terminal_key_from_named(named_key, has_shift))
    {
        return encode_key_kitty_disambiguate(
            KittyKey::Named(key),
            application_cursor_keys,
            modifiers,
        );
    }
    if !(has_ctrl || has_alt) {
        return None;
    }
    let codepoint = press
        .base_char
        .or_else(|| press.text.and_then(|text| text.chars().next()))?
        .to_lowercase()
        .next()?;
    encode_key_kitty_disambiguate(KittyKey::Text(codepoint), application_cursor_keys, modifiers)
}

fn named_key_from_logical(logical_key: Key<&str>) -> Option<NamedKey> {
    match logical_key {
        Key::Named(named_key) => Some(named_key),
        _ => None,
    }
}

fn should_forward_key_event(
    state: ElementState,
    is_synthetic: bool,
    _repeat: bool,
    modifiers: ModifiersState,
) -> bool {
    // Terminal key repeat is intentional input and follows the same path as
    // the initial press; only releases and focus-synthesized events are dropped.
    state.is_pressed() && !is_synthetic && !modifiers.super_key()
}

fn scrollback_action_for_key_event(
    logical_key: Key<&str>,
    state: ElementState,
    is_synthetic: bool,
    repeat: bool,
    modifiers: ModifiersState,
) -> Option<ScrollbackAction> {
    if modifiers != ModifiersState::SHIFT
        || !should_forward_key_event(state, is_synthetic, repeat, modifiers)
    {
        return None;
    }

    match logical_key {
        Key::Named(NamedKey::PageUp) => Some(ScrollbackAction::PageUp),
        Key::Named(NamedKey::PageDown) => Some(ScrollbackAction::PageDown),
        Key::Named(NamedKey::Home) => Some(ScrollbackAction::Top),
        Key::Named(NamedKey::End) => Some(ScrollbackAction::Bottom),
        _ => None,
    }
}

fn terminal_key_from_named(named_key: NamedKey, has_shift: bool) -> Option<TerminalKey> {
    match named_key {
        NamedKey::Enter => Some(TerminalKey::Enter),
        NamedKey::Backspace => Some(TerminalKey::Backspace),
        NamedKey::Tab if has_shift => Some(TerminalKey::BackTab),
        NamedKey::Tab => Some(TerminalKey::Tab),
        NamedKey::Escape => Some(TerminalKey::Escape),
        NamedKey::ArrowUp => Some(TerminalKey::Up),
        NamedKey::ArrowDown => Some(TerminalKey::Down),
        NamedKey::ArrowRight => Some(TerminalKey::Right),
        NamedKey::ArrowLeft => Some(TerminalKey::Left),
        NamedKey::Home => Some(TerminalKey::Home),
        NamedKey::End => Some(TerminalKey::End),
        NamedKey::PageUp => Some(TerminalKey::PageUp),
        NamedKey::PageDown => Some(TerminalKey::PageDown),
        NamedKey::Insert => Some(TerminalKey::Insert),
        NamedKey::Delete => Some(TerminalKey::Delete),
        NamedKey::F1 => Some(TerminalKey::Function(1)),
        NamedKey::F2 => Some(TerminalKey::Function(2)),
        NamedKey::F3 => Some(TerminalKey::Function(3)),
        NamedKey::F4 => Some(TerminalKey::Function(4)),
        NamedKey::F5 => Some(TerminalKey::Function(5)),
        NamedKey::F6 => Some(TerminalKey::Function(6)),
        NamedKey::F7 => Some(TerminalKey::Function(7)),
        NamedKey::F8 => Some(TerminalKey::Function(8)),
        NamedKey::F9 => Some(TerminalKey::Function(9)),
        NamedKey::F10 => Some(TerminalKey::Function(10)),
        NamedKey::F11 => Some(TerminalKey::Function(11)),
        NamedKey::F12 => Some(TerminalKey::Function(12)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        encode_key_press, ime_commit_payload_with_limit, named_key_from_logical,
        scrollback_action_for_key_event, should_forward_key_event, terminal_key_from_named,
        ImeCommitPayload, LinuxKeyPress, ScrollbackAction, MAX_IME_COMMIT_BYTES,
    };
    use crate::input::keyboard::TerminalKey;
    use winit::event::ElementState;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    fn press<'a>(
        named_key: Option<NamedKey>,
        text: Option<&'a str>,
        control_text: Option<&'a str>,
        modifiers: ModifiersState,
    ) -> LinuxKeyPress<'a> {
        LinuxKeyPress {
            named_key,
            text,
            control_text,
            base_char: text.and_then(|text| text.chars().next()),
            modifiers,
        }
    }

    #[test]
    fn kitty_disambiguate_routes_linux_keys_through_csi_u() {
        let none = ModifiersState::empty();
        assert_eq!(
            encode_key_press(press(Some(NamedKey::Escape), Some("\x1b"), None, none), false, true)
                .as_deref(),
            Some(b"\x1b[27u".as_slice())
        );
        assert_eq!(
            encode_key_press(press(Some(NamedKey::Enter), Some("\r"), None, none), false, true)
                .as_deref(),
            Some(b"\r".as_slice())
        );
        assert_eq!(
            encode_key_press(
                press(Some(NamedKey::Enter), Some("\r"), None, ModifiersState::SHIFT),
                false,
                true,
            )
            .as_deref(),
            Some(b"\x1b[13;2u".as_slice())
        );
        // Ctrl+I: the unmodified key wins over the control text (`\t`).
        let mut ctrl_i = press(None, Some("i"), Some("\t"), ModifiersState::CONTROL);
        ctrl_i.base_char = Some('i');
        assert_eq!(
            encode_key_press(ctrl_i, false, true).as_deref(),
            Some(b"\x1b[105;5u".as_slice())
        );
        assert_eq!(
            encode_key_press(
                press(None, Some("A"), None, ModifiersState::CONTROL | ModifiersState::SHIFT),
                false,
                true,
            )
            .as_deref(),
            Some(b"\x1b[97;6u".as_slice())
        );
        assert_eq!(
            encode_key_press(press(None, Some("x"), None, ModifiersState::ALT), false, true)
                .as_deref(),
            Some(b"\x1b[120;3u".as_slice())
        );
        // Plain text and arrows are unchanged.
        assert_eq!(
            encode_key_press(press(None, Some("a"), None, none), false, true).as_deref(),
            Some(b"a".as_slice())
        );
        assert_eq!(
            encode_key_press(press(Some(NamedKey::ArrowUp), None, None, none), true, true)
                .as_deref(),
            Some(b"\x1bOA".as_slice())
        );
        // Without the flag, Ctrl+I keeps the legacy control byte.
        assert_eq!(
            encode_key_press(ctrl_i, false, false).as_deref(),
            Some(b"\t".as_slice())
        );
    }

    #[test]
    fn filters_release_and_synthetic_events_but_accepts_repeats() {
        assert!(should_forward_key_event(
            ElementState::Pressed,
            false,
            false,
            ModifiersState::empty(),
        ));
        assert!(should_forward_key_event(
            ElementState::Pressed,
            false,
            true,
            ModifiersState::empty(),
        ));
        assert!(!should_forward_key_event(
            ElementState::Released,
            false,
            false,
            ModifiersState::empty(),
        ));
        assert!(!should_forward_key_event(
            ElementState::Pressed,
            true,
            false,
            ModifiersState::empty(),
        ));
        assert!(!should_forward_key_event(
            ElementState::Pressed,
            false,
            false,
            ModifiersState::SUPER,
        ));
    }

    #[test]
    fn logical_key_preserves_numlock_text_instead_of_modifierless_navigation() {
        assert_eq!(named_key_from_logical(Key::Character("1")), None);
        assert_eq!(
            named_key_from_logical(Key::Named(NamedKey::End)),
            Some(NamedKey::End)
        );
    }

    #[test]
    fn ime_commits_preserve_bytes_and_enforce_the_atomic_limit() {
        assert_eq!(
            ime_commit_payload_with_limit(String::new(), 0),
            ImeCommitPayload::Empty
        );

        let text = "é日本🙂\0".to_string();
        let expected = text.as_bytes().to_vec();
        assert_eq!(
            ime_commit_payload_with_limit(text, expected.len()),
            ImeCommitPayload::Bytes(expected)
        );

        let mut overallocated = String::with_capacity(64);
        overallocated.push_str("small");
        assert!(overallocated.capacity() > overallocated.len());
        match ime_commit_payload_with_limit(overallocated, 8) {
            ImeCommitPayload::Bytes(bytes) => {
                assert_eq!(bytes, b"small");
                assert!(bytes.capacity() <= 8);
            }
            payload => panic!("expected normalized IME bytes, got {payload:?}"),
        }

        assert_eq!(
            ime_commit_payload_with_limit("é".to_string(), 1),
            ImeCommitPayload::TooLarge {
                byte_count: "é".len(),
                limit: 1,
            },
            "the byte limit must never truncate a multibyte character"
        );

        let maximum = "x".repeat(MAX_IME_COMMIT_BYTES);
        assert!(matches!(
            ime_commit_payload_with_limit(maximum, MAX_IME_COMMIT_BYTES),
            ImeCommitPayload::Bytes(bytes)
                if bytes.len() == MAX_IME_COMMIT_BYTES
                    && bytes.capacity() <= MAX_IME_COMMIT_BYTES
        ));
        assert_eq!(
            ime_commit_payload_with_limit(
                "x".repeat(MAX_IME_COMMIT_BYTES + 1),
                MAX_IME_COMMIT_BYTES,
            ),
            ImeCommitPayload::TooLarge {
                byte_count: MAX_IME_COMMIT_BYTES + 1,
                limit: MAX_IME_COMMIT_BYTES,
            }
        );
    }

    #[test]
    fn maps_exact_shift_navigation_to_local_scrollback_actions() {
        let cases = [
            (NamedKey::PageUp, ScrollbackAction::PageUp),
            (NamedKey::PageDown, ScrollbackAction::PageDown),
            (NamedKey::Home, ScrollbackAction::Top),
            (NamedKey::End, ScrollbackAction::Bottom),
        ];

        for (named_key, expected) in cases {
            assert_eq!(
                scrollback_action_for_key_event(
                    Key::Named(named_key),
                    ElementState::Pressed,
                    false,
                    false,
                    ModifiersState::SHIFT,
                ),
                Some(expected)
            );
        }
        assert_eq!(
            scrollback_action_for_key_event(
                Key::Named(NamedKey::ArrowUp),
                ElementState::Pressed,
                false,
                false,
                ModifiersState::SHIFT,
            ),
            None
        );
    }

    #[test]
    fn scrollback_shortcuts_require_a_real_press_and_exact_shift() {
        let action = |state, is_synthetic, repeat, modifiers| {
            scrollback_action_for_key_event(
                Key::Named(NamedKey::PageUp),
                state,
                is_synthetic,
                repeat,
                modifiers,
            )
        };

        assert_eq!(
            action(ElementState::Pressed, false, true, ModifiersState::SHIFT,),
            Some(ScrollbackAction::PageUp),
            "key repeat should keep scrolling"
        );
        assert_eq!(
            action(ElementState::Released, false, false, ModifiersState::SHIFT,),
            None
        );
        assert_eq!(
            action(ElementState::Pressed, true, false, ModifiersState::SHIFT,),
            None
        );

        for modifiers in [
            ModifiersState::empty(),
            ModifiersState::CONTROL,
            ModifiersState::ALT,
            ModifiersState::SHIFT | ModifiersState::CONTROL,
            ModifiersState::SHIFT | ModifiersState::ALT,
            ModifiersState::SHIFT | ModifiersState::SUPER,
        ] {
            assert_eq!(
                action(ElementState::Pressed, false, false, modifiers),
                None,
                "unexpected local shortcut for {modifiers:?}"
            );
        }
    }

    #[test]
    fn non_local_page_up_combinations_keep_their_terminal_sequences() {
        let cases = [
            (ModifiersState::empty(), b"\x1b[5~".as_slice()),
            (ModifiersState::CONTROL, b"\x1b[5;5~".as_slice()),
            (ModifiersState::ALT, b"\x1b[5;3~".as_slice()),
            (
                ModifiersState::SHIFT | ModifiersState::CONTROL,
                b"\x1b[5;6~".as_slice(),
            ),
            (
                ModifiersState::SHIFT | ModifiersState::ALT,
                b"\x1b[5;4~".as_slice(),
            ),
        ];

        for (modifiers, expected) in cases {
            assert_eq!(
                scrollback_action_for_key_event(
                    Key::Named(NamedKey::PageUp),
                    ElementState::Pressed,
                    false,
                    false,
                    modifiers,
                ),
                None,
                "{modifiers:?} PageUp must stay on the terminal input path"
            );
            assert_eq!(
                encode_key_press(press(Some(NamedKey::PageUp), None, None, modifiers), false, false)
                    .as_deref(),
                Some(expected),
                "unexpected terminal sequence for {modifiers:?} PageUp"
            );
        }
    }

    #[test]
    fn maps_every_supported_named_key() {
        let cases = [
            (NamedKey::Enter, TerminalKey::Enter),
            (NamedKey::Backspace, TerminalKey::Backspace),
            (NamedKey::Tab, TerminalKey::Tab),
            (NamedKey::Escape, TerminalKey::Escape),
            (NamedKey::ArrowUp, TerminalKey::Up),
            (NamedKey::ArrowDown, TerminalKey::Down),
            (NamedKey::ArrowRight, TerminalKey::Right),
            (NamedKey::ArrowLeft, TerminalKey::Left),
            (NamedKey::Home, TerminalKey::Home),
            (NamedKey::End, TerminalKey::End),
            (NamedKey::PageUp, TerminalKey::PageUp),
            (NamedKey::PageDown, TerminalKey::PageDown),
            (NamedKey::Insert, TerminalKey::Insert),
            (NamedKey::Delete, TerminalKey::Delete),
        ];
        for (named_key, terminal_key) in cases {
            assert_eq!(
                terminal_key_from_named(named_key, false),
                Some(terminal_key)
            );
        }
        assert_eq!(
            terminal_key_from_named(NamedKey::Tab, true),
            Some(TerminalKey::BackTab)
        );

        let function_keys = [
            NamedKey::F1,
            NamedKey::F2,
            NamedKey::F3,
            NamedKey::F4,
            NamedKey::F5,
            NamedKey::F6,
            NamedKey::F7,
            NamedKey::F8,
            NamedKey::F9,
            NamedKey::F10,
            NamedKey::F11,
            NamedKey::F12,
        ];
        for (index, named_key) in function_keys.into_iter().enumerate() {
            assert_eq!(
                terminal_key_from_named(named_key, false),
                Some(TerminalKey::Function((index + 1) as u8))
            );
        }
        assert_eq!(terminal_key_from_named(NamedKey::Space, false), None);
    }

    #[test]
    fn encodes_every_linux_named_key_to_its_exact_terminal_sequence() {
        let cases = [
            (NamedKey::Enter, b"\r".as_slice()),
            (NamedKey::Backspace, b"\x7f".as_slice()),
            (NamedKey::Tab, b"\t".as_slice()),
            (NamedKey::Escape, b"\x1b".as_slice()),
            (NamedKey::ArrowUp, b"\x1b[A".as_slice()),
            (NamedKey::ArrowDown, b"\x1b[B".as_slice()),
            (NamedKey::ArrowRight, b"\x1b[C".as_slice()),
            (NamedKey::ArrowLeft, b"\x1b[D".as_slice()),
            (NamedKey::Home, b"\x1b[H".as_slice()),
            (NamedKey::End, b"\x1b[F".as_slice()),
            (NamedKey::PageUp, b"\x1b[5~".as_slice()),
            (NamedKey::PageDown, b"\x1b[6~".as_slice()),
            (NamedKey::Insert, b"\x1b[2~".as_slice()),
            (NamedKey::Delete, b"\x1b[3~".as_slice()),
            (NamedKey::F1, b"\x1bOP".as_slice()),
            (NamedKey::F2, b"\x1bOQ".as_slice()),
            (NamedKey::F3, b"\x1bOR".as_slice()),
            (NamedKey::F4, b"\x1bOS".as_slice()),
            (NamedKey::F5, b"\x1b[15~".as_slice()),
            (NamedKey::F6, b"\x1b[17~".as_slice()),
            (NamedKey::F7, b"\x1b[18~".as_slice()),
            (NamedKey::F8, b"\x1b[19~".as_slice()),
            (NamedKey::F9, b"\x1b[20~".as_slice()),
            (NamedKey::F10, b"\x1b[21~".as_slice()),
            (NamedKey::F11, b"\x1b[23~".as_slice()),
            (NamedKey::F12, b"\x1b[24~".as_slice()),
        ];

        for (named_key, expected) in cases {
            assert_eq!(
                encode_key_press(
                    press(
                        Some(named_key),
                        Some("text trap"),
                        Some("control trap"),
                        ModifiersState::empty(),
                    ),
                    false,
                    false,
                )
                .as_deref(),
                Some(expected),
                "unexpected Linux terminal sequence for {named_key:?}"
            );
        }
    }

    #[test]
    fn named_keys_take_precedence_over_their_text_without_duplicates() {
        assert_eq!(
            encode_key_press(
                press(
                    Some(NamedKey::Enter),
                    Some("\r"),
                    Some("\r"),
                    ModifiersState::empty(),
                ),
                false,
                false,
            )
            .as_deref(),
            Some(b"\r".as_slice())
        );
        assert_eq!(
            encode_key_press(
                press(
                    Some(NamedKey::Tab),
                    Some("\t"),
                    Some("\t"),
                    ModifiersState::SHIFT,
                ),
                false,
                false,
            )
            .as_deref(),
            Some(b"\x1b[Z".as_slice())
        );
    }

    #[test]
    fn named_cursor_keys_follow_the_terminal_application_mode() {
        let up = press(Some(NamedKey::ArrowUp), None, None, ModifiersState::empty());
        assert_eq!(
            encode_key_press(up, false, false).as_deref(),
            Some(b"\x1b[A".as_slice())
        );
        assert_eq!(
            encode_key_press(up, true, false).as_deref(),
            Some(b"\x1bOA".as_slice())
        );
    }

    #[test]
    fn text_preserves_unicode_and_control_supplements() {
        assert_eq!(
            encode_key_press(
                press(None, Some("é日本"), None, ModifiersState::empty()),
                false,
                false,
            ),
            Some("é日本".as_bytes().to_vec())
        );
        assert_eq!(
            encode_key_press(
                press(None, Some("a"), Some("\x01"), ModifiersState::CONTROL,),
                false,
                false,
            ),
            Some(vec![0x01])
        );
        assert_eq!(
            encode_key_press(
                press(
                    Some(NamedKey::Space),
                    Some(" "),
                    Some("\0"),
                    ModifiersState::CONTROL,
                ),
                false,
                false,
            ),
            Some(vec![0x00])
        );
        assert_eq!(
            encode_key_press(press(None, Some("a"), None, ModifiersState::CONTROL), false, false),
            None,
            "missing all-modifier text must not leak printable input"
        );
    }

    #[test]
    fn alt_prefixes_text_and_control_but_parameterizes_named_sequences() {
        assert_eq!(
            encode_key_press(
                press(None, Some("é"), Some("é"), ModifiersState::ALT),
                false,
                false,
            ),
            Some([b"\x1b".as_slice(), "é".as_bytes()].concat())
        );
        assert_eq!(
            encode_key_press(
                press(
                    None,
                    Some("a"),
                    Some("\x01"),
                    ModifiersState::CONTROL | ModifiersState::ALT,
                ),
                false,
                false,
            ),
            Some(vec![0x1b, 0x01])
        );
        assert_eq!(
            encode_key_press(
                press(Some(NamedKey::ArrowLeft), None, None, ModifiersState::ALT,),
                false,
                false,
            )
            .as_deref(),
            Some(b"\x1b[1;3D".as_slice())
        );
    }

    #[test]
    fn modified_named_keys_use_xterm_parameters_without_text_duplicates() {
        assert_eq!(
            encode_key_press(
                press(
                    Some(NamedKey::Delete),
                    Some("text trap"),
                    Some("control trap"),
                    ModifiersState::CONTROL | ModifiersState::ALT,
                ),
                false,
                false,
            )
            .as_deref(),
            Some(b"\x1b[3;7~".as_slice())
        );
        assert_eq!(
            encode_key_press(
                press(
                    Some(NamedKey::F1),
                    Some("text trap"),
                    Some("control trap"),
                    ModifiersState::SHIFT,
                ),
                true,
                false,
            )
            .as_deref(),
            Some(b"\x1b[1;2P".as_slice())
        );
    }

    #[test]
    fn tab_keeps_backtab_and_traditional_alt_behavior() {
        let cases = [
            (ModifiersState::empty(), b"\t".as_slice()),
            (ModifiersState::SHIFT, b"\x1b[Z".as_slice()),
            (ModifiersState::CONTROL, b"\t".as_slice()),
            (ModifiersState::ALT, b"\x1b\t".as_slice()),
            (
                ModifiersState::SHIFT | ModifiersState::CONTROL,
                b"\x1b[Z".as_slice(),
            ),
            (
                ModifiersState::SHIFT | ModifiersState::ALT,
                b"\x1b\x1b[Z".as_slice(),
            ),
            (
                ModifiersState::CONTROL | ModifiersState::ALT,
                b"\x1b\t".as_slice(),
            ),
            (
                ModifiersState::SHIFT | ModifiersState::CONTROL | ModifiersState::ALT,
                b"\x1b\x1b[Z".as_slice(),
            ),
        ];

        for (modifiers, expected) in cases {
            assert_eq!(
                encode_key_press(
                    press(Some(NamedKey::Tab), Some("trap"), Some("trap"), modifiers),
                    false,
                    false,
                )
                .as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn modified_numlock_text_stays_text_instead_of_navigation() {
        assert_eq!(
            encode_key_press(
                press(None, Some("1"), Some("1"), ModifiersState::ALT),
                false,
                false,
            )
            .as_deref(),
            Some(b"\x1b1".as_slice())
        );
        assert_eq!(
            encode_key_press(
                press(Some(NamedKey::End), None, None, ModifiersState::CONTROL,),
                false,
                false,
            )
            .as_deref(),
            Some(b"\x1b[1;5F".as_slice())
        );
    }

    #[test]
    fn super_and_empty_text_do_not_reach_the_terminal() {
        assert_eq!(
            encode_key_press(
                press(None, Some("x"), Some("x"), ModifiersState::SUPER),
                false,
                false,
            ),
            None
        );
        assert_eq!(
            encode_key_press(
                press(None, Some(""), Some(""), ModifiersState::empty()),
                false,
                false,
            ),
            None
        );
    }
}
