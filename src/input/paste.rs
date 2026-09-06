//! Encode clipboard text as bounded terminal input, preserving paste framing.

pub const MAX_PASTE_BYTES: usize = 1024 * 1024;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PasteError {
    #[error("clipboard text exceeds the terminal paste byte limit")]
    TooLarge,
    #[error("could not allocate terminal paste input")]
    AllocationFailed,
}

/// Convert line endings to terminal Enter bytes and remove embedded controls.
///
/// Tabs and line breaks remain useful text input; ESC, C1 and other controls
/// cannot terminate bracketed paste or turn clipboard content into key events.
/// The byte budget includes both bracketed-paste markers. Oversized pastes are
/// rejected as a whole, so neither UTF-8 characters nor closing markers are cut.
pub fn encode_paste(text: &str, bracketed: bool, max_bytes: usize) -> Result<Vec<u8>, PasteError> {
    let payload_bytes = normalized_chars(text).try_fold(0usize, |length, character| {
        length
            .checked_add(character.len_utf8())
            .filter(|length| *length <= max_bytes)
            .ok_or(PasteError::TooLarge)
    })?;
    if payload_bytes == 0 {
        return Ok(Vec::new());
    }
    let framing_bytes = if bracketed {
        PASTE_START.len() + PASTE_END.len()
    } else {
        0
    };
    let output_bytes = payload_bytes
        .checked_add(framing_bytes)
        .filter(|length| *length <= max_bytes)
        .ok_or(PasteError::TooLarge)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_bytes)
        .map_err(|_| PasteError::AllocationFailed)?;
    if bracketed {
        output.extend_from_slice(PASTE_START);
    }
    let mut encoded = [0u8; 4];
    for character in normalized_chars(text) {
        output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
    }
    if bracketed {
        output.extend_from_slice(PASTE_END);
    }
    Ok(output)
}

fn normalized_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    let mut previous_was_cr = false;
    text.chars().filter_map(move |character| {
        let follows_cr = previous_was_cr;
        previous_was_cr = character == '\r';
        match character {
            '\n' if follows_cr => None,
            '\n' => Some('\r'),
            '\r' | '\t' => Some(character),
            character if character.is_control() => None,
            character => Some(character),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{encode_paste, PasteError, MAX_PASTE_BYTES, PASTE_END, PASTE_START};

    #[test]
    fn empty_or_control_only_text_sends_no_paste_markers() {
        for text in ["", "\0\x1b\x03\x7f\u{009b}"] {
            for bracketed in [false, true] {
                assert_eq!(encode_paste(text, bracketed, 0), Ok(Vec::new()));
            }
        }
    }

    #[test]
    fn preserves_unicode_and_tabs_with_terminal_line_endings() {
        let text = "ação\t日本語\r\n🦀\nnext\rlast";
        let expected = "ação\t日本語\r🦀\rnext\rlast";
        assert_eq!(
            encode_paste(text, false, MAX_PASTE_BYTES).unwrap(),
            expected.as_bytes()
        );
        let bracketed = encode_paste(text, true, MAX_PASTE_BYTES).unwrap();
        assert_eq!(&bracketed[..PASTE_START.len()], PASTE_START);
        assert_eq!(&bracketed[bracketed.len() - PASTE_END.len()..], PASTE_END);
        assert_eq!(
            &bracketed[PASTE_START.len()..bracketed.len() - PASTE_END.len()],
            expected.as_bytes()
        );
    }

    #[test]
    fn rejects_whole_paste_when_utf8_or_framing_exceeds_budget() {
        assert_eq!(encode_paste("🦀", false, 3), Err(PasteError::TooLarge));
        assert_eq!(encode_paste("🦀", false, 4).unwrap(), "🦀".as_bytes());
        assert_eq!(encode_paste("🦀", true, 15), Err(PasteError::TooLarge));
        assert_eq!(encode_paste("🦀", true, 16).unwrap().len(), 16);
        assert_eq!(encode_paste("a", false, 0), Err(PasteError::TooLarge));
        assert_eq!(encode_paste("\r\n", false, 1).unwrap(), b"\r");
    }

    #[test]
    fn clipboard_cannot_inject_early_bracketed_paste_end_or_key_controls() {
        let payload = "safe\x1b[201~\rcommand\x03\u{009b}201~\x1b[200~\x08\x7f";
        let encoded = encode_paste(payload, true, MAX_PASTE_BYTES).unwrap();
        let contents = &encoded[PASTE_START.len()..encoded.len() - PASTE_END.len()];
        assert_eq!(contents, b"safe[201~\rcommand201~[200~");
        assert_eq!(
            encoded
                .windows(PASTE_START.len())
                .filter(|part| *part == PASTE_START)
                .count(),
            1
        );
        assert_eq!(
            encoded
                .windows(PASTE_END.len())
                .filter(|part| *part == PASTE_END)
                .count(),
            1
        );
    }

    #[test]
    fn rejects_oversized_payloads_without_truncating_or_partial_framing() {
        let text = "a".repeat(MAX_PASTE_BYTES);
        assert_eq!(
            encode_paste(&text, true, MAX_PASTE_BYTES),
            Err(PasteError::TooLarge)
        );
        assert_eq!(
            encode_paste(&text, false, MAX_PASTE_BYTES).unwrap().len(),
            MAX_PASTE_BYTES
        );
    }
}
