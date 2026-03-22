use objc2_app_kit::NSEvent;
use objc2_app_kit::NSEventModifierFlags;

pub fn translate_key_event(event: &NSEvent) -> Option<Vec<u8>> {
    let key_code = event.keyCode();
    let modifiers = event.modifierFlags();
    let has_ctrl = modifiers.contains(NSEventModifierFlags::Control);
    let has_alt = modifiers.contains(NSEventModifierFlags::Option);

    // Handle special keys by keyCode
    match key_code {
        36 => return Some(b"\r".to_vec()),         // Return
        51 => return Some(b"\x7f".to_vec()),       // Backspace
        48 => return Some(b"\t".to_vec()),         // Tab
        53 => return Some(b"\x1b".to_vec()),       // Escape
        126 => return Some(b"\x1b[A".to_vec()),    // Up
        125 => return Some(b"\x1b[B".to_vec()),    // Down
        124 => return Some(b"\x1b[C".to_vec()),    // Right
        123 => return Some(b"\x1b[D".to_vec()),    // Left
        115 => return Some(b"\x1b[H".to_vec()),    // Home
        119 => return Some(b"\x1b[F".to_vec()),    // End
        116 => return Some(b"\x1b[5~".to_vec()),   // Page Up
        121 => return Some(b"\x1b[6~".to_vec()),   // Page Down
        117 => return Some(b"\x1b[3~".to_vec()),   // Delete (forward)
        122 => return Some(b"\x1bOP".to_vec()),    // F1
        120 => return Some(b"\x1bOQ".to_vec()),    // F2
        99 => return Some(b"\x1bOR".to_vec()),     // F3
        118 => return Some(b"\x1bOS".to_vec()),    // F4
        _ => {}
    }

    // Get characters
    let characters = event.characters();
    let characters = match characters {
        Some(c) => c,
        None => return None,
    };
    let chars_str = characters.to_string();

    if chars_str.is_empty() {
        return None;
    }

    // Ctrl+key combinations
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
            if let Some(b) = byte {
                return Some(vec![b]);
            }
        }
    }

    // Alt/Option key — send ESC prefix
    if has_alt {
        let mut bytes = vec![0x1b];
        bytes.extend_from_slice(chars_str.as_bytes());
        return Some(bytes);
    }

    // Regular characters
    Some(chars_str.into_bytes())
}
