//! OSC 9 / 777 / 99 notification payload parsing (no desktop delivery).

/// Soft caps applied when constructing a notify request from OSC text.
pub(crate) const MAX_TITLE_CHARS: usize = 100;
pub(crate) const MAX_BODY_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationRequest {
    pub title: String,
    pub body: String,
}

impl NotificationRequest {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: truncate_chars(title.into(), MAX_TITLE_CHARS),
            body: truncate_chars(body.into(), MAX_BODY_CHARS),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.title.is_empty() && self.body.is_empty()
    }
}

fn truncate_chars(mut text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    text = text.chars().take(max_chars).collect();
    text
}

fn append_capped(target: &mut String, chunk: &str, max_chars: usize) {
    let remaining = max_chars.saturating_sub(target.chars().count());
    if remaining == 0 || chunk.is_empty() {
        return;
    }
    if chunk.chars().count() <= remaining {
        target.push_str(chunk);
    } else {
        target.push_str(&chunk.chars().take(remaining).collect::<String>());
    }
}

pub(crate) fn is_osc9_progress(payload: &str) -> bool {
    // ConEmu / iTerm2 progress: OSC 9;4 or OSC 9;4;… — not a notification.
    payload == "4" || payload.starts_with("4;")
}

pub(crate) fn parse_osc777_notify(payload: &str) -> Option<NotificationRequest> {
    // OSC 777;notify;title;body — body may contain ';'.
    let mut parts = payload.splitn(3, ';');
    if parts.next() != Some("notify") {
        return None;
    }
    let title = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("");
    Some(NotificationRequest::new(title, body))
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Osc99Draft {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Osc99Action {
    Ignore,
    Notify(NotificationRequest),
    /// Hold chunks until a later `d=1` (or default done) for this id.
    Hold { id: String, draft: Osc99Draft },
}

pub(crate) fn parse_osc99(payload: &str, prior: Option<Osc99Draft>) -> Osc99Action {
    // payload is everything after "99;" — expects "metadata;text".
    let Some((metadata, text)) = payload.split_once(';') else {
        return Osc99Action::Ignore;
    };

    let mut part = "title"; // default payload type when `p` omitted
    let mut done = true;
    let mut id = String::new();
    let mut base64 = false;

    for entry in metadata.split(':').filter(|entry| !entry.is_empty()) {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        match key {
            "p" => part = value,
            "d" => done = value != "0",
            "i" => id = value.to_string(),
            "e" => base64 = value == "1",
            _ => {}
        }
    }

    if part == "?" {
        return Osc99Action::Ignore;
    }

    let decoded = if base64 {
        match decode_osc99_base64(text) {
            Some(text) => text,
            None => return Osc99Action::Ignore,
        }
    } else {
        text.to_string()
    };

    let mut draft = prior.unwrap_or_default();
    match part {
        // Cap each chunk and the held draft so a flood of d=0 OSC 99 cannot grow unboundedly.
        "title" => append_capped(&mut draft.title, &decoded, MAX_TITLE_CHARS),
        "body" => append_capped(&mut draft.body, &decoded, MAX_BODY_CHARS),
        _ => return Osc99Action::Ignore,
    }

    if !done {
        return Osc99Action::Hold { id, draft };
    }

    let request = NotificationRequest::new(
        std::mem::take(&mut draft.title),
        std::mem::take(&mut draft.body),
    );
    if request.is_empty() {
        Osc99Action::Ignore
    } else {
        Osc99Action::Notify(request)
    }
}

fn decode_osc99_base64(text: &str) -> Option<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.as_bytes())
        .ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc9_progress_is_not_a_notification_payload() {
        assert!(is_osc9_progress("4"));
        assert!(is_osc9_progress("4;1;50"));
        assert!(is_osc9_progress("4;3"));
        assert!(is_osc9_progress("4;0"));
        assert!(!is_osc9_progress("Build finished"));
        assert!(!is_osc9_progress("4ish"));
        assert!(!is_osc9_progress(""));
    }

    #[test]
    fn osc777_parses_title_and_body_with_embedded_semicolons() {
        let request = parse_osc777_notify("notify;Deploy;a;b;c").unwrap();
        assert_eq!(request.title, "Deploy");
        assert_eq!(request.body, "a;b;c");
        assert!(parse_osc777_notify("other;x;y").is_none());
    }

    #[test]
    fn osc99_single_chunk_defaults_to_title_payload() {
        match parse_osc99(";Hello world", None) {
            Osc99Action::Notify(request) => {
                assert_eq!(request.title, "Hello world");
                assert!(request.body.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn osc99_chunks_title_then_body() {
        let hold = match parse_osc99("i=1:d=0:p=title;Build", None) {
            Osc99Action::Hold { id, draft } => {
                assert_eq!(id, "1");
                draft
            }
            other => panic!("unexpected {other:?}"),
        };
        match parse_osc99("i=1:p=body;done", Some(hold)) {
            Osc99Action::Notify(request) => {
                assert_eq!(request.title, "Build");
                assert_eq!(request.body, "done");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn truncates_oversized_title_and_body() {
        let title: String = "t".repeat(MAX_TITLE_CHARS + 40);
        let body: String = "b".repeat(MAX_BODY_CHARS + 40);
        let request = NotificationRequest::new(title, body);
        assert_eq!(request.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(request.body.chars().count(), MAX_BODY_CHARS);
    }

    #[test]
    fn osc99_caps_each_chunk_and_held_draft() {
        let big = "X".repeat(MAX_TITLE_CHARS + 80);
        let hold = match parse_osc99(&format!("i=cap:d=0:p=title;{big}"), None) {
            Osc99Action::Hold { draft, .. } => {
                assert_eq!(draft.title.chars().count(), MAX_TITLE_CHARS);
                draft
            }
            other => panic!("unexpected {other:?}"),
        };
        // A second oversized chunk must not grow the draft past the cap.
        let hold = match parse_osc99(&format!("i=cap:d=0:p=title;{big}"), Some(hold)) {
            Osc99Action::Hold { draft, .. } => {
                assert_eq!(draft.title.chars().count(), MAX_TITLE_CHARS);
                draft
            }
            other => panic!("unexpected {other:?}"),
        };
        match parse_osc99(&format!("i=cap:p=body;{}","Y".repeat(MAX_BODY_CHARS + 20)), Some(hold)) {
            Osc99Action::Notify(request) => {
                assert_eq!(request.title.chars().count(), MAX_TITLE_CHARS);
                assert_eq!(request.body.chars().count(), MAX_BODY_CHARS);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
