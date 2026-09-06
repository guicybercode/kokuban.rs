//! Android editor transactions and terminal encoding, independent of JNI.
use crate::input::keyboard::{
    encode_terminal_key_with_modifiers, TerminalKey, TerminalKeyModifiers,
};

/// Bound one editor transaction; this is separate from the writer queue budget.
pub(crate) const MAX_IME_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub(crate) enum ImeEvent {
    Commit(String),
    Preedit(String),
    Finish,
    Delete {
        before: u32,
        after: u32,
    },
    Key {
        code: i32,
        unicode: u32,
        meta: i32,
    },
    /// Visible content bounds in full native-surface pixels (right/bottom exclusive).
    Viewport {
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
        keyboard: bool,
    },
    Clipboard(String),
    /// Native accessibility actions: 0 activate, 1 focus, 2 clear focus.
    Control {
        id: i32,
        action: i32,
    },
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct InputModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

impl InputModifiers {
    pub(crate) const fn new(shift: bool, alt: bool, control: bool) -> Self {
        Self {
            shift,
            alt,
            control,
        }
    }

    fn from_android(meta: i32) -> Self {
        // Android KeyEvent META_*_ON also include side-specific modifier masks.
        Self::new(meta & 0xc1 != 0, meta & 0x32 != 0, meta & 0x7000 != 0)
    }

    fn terminal(self) -> TerminalKeyModifiers {
        TerminalKeyModifiers::new(self.shift, self.alt, self.control)
    }
}

/// Preedit is displayed locally; only committed transactions reach the PTY.
#[derive(Default)]
pub(crate) struct InputState {
    preedit: String,
}

impl InputState {
    pub(crate) fn preedit(&self) -> &str {
        &self.preedit
    }

    pub(crate) fn handle(
        &mut self,
        event: &ImeEvent,
        application_cursor_keys: bool,
        modifiers: InputModifiers,
    ) -> Vec<u8> {
        match event {
            ImeEvent::Preedit(text) => {
                if text.len() <= MAX_IME_BYTES {
                    self.preedit.clone_from(text);
                }
                Vec::new()
            }
            ImeEvent::Commit(text) => {
                self.preedit.clear();
                encode_text(text, modifiers)
            }
            ImeEvent::Finish => encode_text(&std::mem::take(&mut self.preedit), modifiers),
            ImeEvent::Delete { before, after } => {
                let mut bytes = vec![0x7f; (*before).min(4096) as usize];
                for _ in 0..(*after).min(4096) {
                    bytes.extend_from_slice(b"\x1b[3~");
                }
                bytes
            }
            ImeEvent::Key {
                code,
                unicode,
                meta,
            } => {
                let mut key_modifiers = InputModifiers::from_android(*meta);
                key_modifiers.control |= modifiers.control;
                encode_android_key(*code, *unicode, application_cursor_keys, key_modifiers)
            }
            ImeEvent::Viewport { .. } | ImeEvent::Clipboard(_) | ImeEvent::Control { .. } => {
                Vec::new()
            }
        }
    }
}

pub(crate) fn encode_text(text: &str, modifiers: InputModifiers) -> Vec<u8> {
    if text.is_empty() || text.len() > MAX_IME_BYTES {
        return Vec::new();
    }
    let mut result = Vec::with_capacity(text.len() + usize::from(modifiers.alt));
    if modifiers.alt {
        result.push(0x1b);
    }
    if modifiers.control {
        let [byte] = text.as_bytes() else {
            return Vec::new();
        };
        let control = match byte {
            // Some physical keyboards already resolve Ctrl through their layout.
            0x00..=0x1f | 0x7f => *byte,
            b'@'..=b'_' | b'a'..=b'z' => byte & 0x1f,
            b' ' | b'2' => 0,
            b'3'..=b'7' => byte - b'3' + 0x1b,
            b'?' | b'8' => 0x7f,
            _ => return Vec::new(),
        };
        result.push(control);
    } else {
        // IMEs may deliver the editor action as a newline in a text transaction.
        let mut previous_cr = false;
        for byte in text.bytes() {
            if byte == b'\n' {
                if !previous_cr {
                    result.push(b'\r');
                }
            } else {
                result.push(byte);
            }
            previous_cr = byte == b'\r';
        }
    }
    result
}

pub(crate) fn encode_key(
    key: TerminalKey,
    application_cursor_keys: bool,
    modifiers: InputModifiers,
) -> Vec<u8> {
    let key = if key == TerminalKey::Tab && modifiers.shift {
        TerminalKey::BackTab
    } else {
        key
    };
    encode_terminal_key_with_modifiers(key, application_cursor_keys, modifiers.terminal())
        .unwrap_or_default()
}

fn encode_android_key(
    code: i32,
    unicode: u32,
    application: bool,
    modifiers: InputModifiers,
) -> Vec<u8> {
    // KeyEvent codes are stable Android SDK constants; do not confuse them with Linux scancodes.
    let key = match code {
        19 => TerminalKey::Up,
        20 => TerminalKey::Down,
        21 => TerminalKey::Left,
        22 => TerminalKey::Right,
        61 => TerminalKey::Tab,
        66 | 160 => TerminalKey::Enter,
        67 => TerminalKey::Backspace,
        92 => TerminalKey::PageUp,
        93 => TerminalKey::PageDown,
        111 => TerminalKey::Escape,
        112 => TerminalKey::Delete,
        122 => TerminalKey::Home,
        123 => TerminalKey::End,
        124 => TerminalKey::Insert,
        131..=142 => TerminalKey::Function((code - 130) as u8),
        _ => {
            return char::from_u32(unicode)
                .filter(|c| *c != '\0')
                .map(|c| encode_text(c.encode_utf8(&mut [0; 4]), modifiers))
                .unwrap_or_default();
        }
    };
    encode_key(key, application, modifiers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_composition_only_writes_the_final_commit() {
        let mut state = InputState::default();
        let modifiers = InputModifiers::default();
        for text in ["a", "á", "ação"] {
            assert!(state
                .handle(&ImeEvent::Preedit(text.into()), false, modifiers)
                .is_empty());
        }
        assert_eq!(state.preedit(), "ação");
        assert_eq!(
            state.handle(&ImeEvent::Commit("ação".into()), false, modifiers),
            "ação".as_bytes()
        );
        assert!(state.handle(&ImeEvent::Finish, false, modifiers).is_empty());
    }

    #[test]
    fn finish_commits_composition_once_and_empty_preedit_cancels() {
        let mut state = InputState::default();
        let modifiers = InputModifiers::default();
        state.handle(&ImeEvent::Preedit("日本語🙂".into()), false, modifiers);
        assert_eq!(
            state.handle(&ImeEvent::Finish, false, modifiers),
            "日本語🙂".as_bytes()
        );
        assert!(state.handle(&ImeEvent::Finish, false, modifiers).is_empty());
        state.handle(&ImeEvent::Preedit("cancel".into()), false, modifiers);
        state.handle(&ImeEvent::Preedit(String::new()), false, modifiers);
        assert!(state.handle(&ImeEvent::Finish, false, modifiers).is_empty());
    }

    #[test]
    fn maps_enter_delete_and_modified_external_keys_using_shared_encoder() {
        let mut state = InputState::default();
        let plain = InputModifiers::default();
        assert_eq!(
            state.handle(&ImeEvent::Commit("x\r\ny\n".into()), false, plain),
            b"x\ry\r"
        );
        assert_eq!(
            state.handle(
                &ImeEvent::Delete {
                    before: 2,
                    after: 1
                },
                false,
                plain
            ),
            b"\x7f\x7f\x1b[3~"
        );
        assert_eq!(encode_android_key(19, 0, true, plain), b"\x1bOA");
        assert_eq!(
            state.handle(
                &ImeEvent::Key {
                    code: 19,
                    unicode: 0,
                    meta: 0x1000
                },
                true,
                plain
            ),
            b"\x1b[1;5A"
        );
        assert_eq!(
            encode_key(
                TerminalKey::Tab,
                false,
                InputModifiers::new(true, false, false)
            ),
            b"\x1b[Z"
        );
        assert_eq!(
            encode_text("c", InputModifiers::new(false, false, true)),
            b"\x03"
        );
        assert_eq!(
            encode_text("x", InputModifiers::new(false, true, false)),
            b"\x1bx"
        );
    }

    #[test]
    fn bounds_editor_allocations_and_delete_repeats() {
        let mut state = InputState::default();
        let plain = InputModifiers::default();
        assert!(state
            .handle(
                &ImeEvent::Commit("x".repeat(MAX_IME_BYTES + 1)),
                false,
                plain
            )
            .is_empty());
        state.handle(
            &ImeEvent::Preedit("x".repeat(MAX_IME_BYTES + 1)),
            false,
            plain,
        );
        assert!(state.preedit().is_empty());
        assert_eq!(
            state
                .handle(
                    &ImeEvent::Delete {
                        before: u32::MAX,
                        after: u32::MAX
                    },
                    false,
                    plain
                )
                .len(),
            4096 * 5
        );
    }

    #[test]
    fn pretranslated_control_and_empty_alt_transactions_are_not_corrupted() {
        assert_eq!(
            encode_text("\x03", InputModifiers::new(false, false, true)),
            b"\x03"
        );
        assert_eq!(
            encode_text("\x03", InputModifiers::new(false, true, true)),
            b"\x1b\x03"
        );
        assert!(encode_text("", InputModifiers::new(false, true, false)).is_empty());
    }
}
