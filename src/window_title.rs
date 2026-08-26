use std::borrow::Cow;

pub(crate) const WINDOW_TITLE: &str = "黒板kokuban";
const MAX_WINDOW_TITLE_BYTES: usize = 1024;

pub(crate) fn sync_window_title_with<F>(
    applied_title: &mut String,
    osc_title: &str,
    set_title: F,
) -> bool
where
    F: FnOnce(&str),
{
    let next_title = normalized_window_title(osc_title);
    if applied_title == next_title.as_ref() {
        return false;
    }

    set_title(next_title.as_ref());
    applied_title.clear();
    applied_title.push_str(next_title.as_ref());
    true
}

pub(crate) fn normalized_window_title(title: &str) -> Cow<'_, str> {
    if title.is_empty() {
        return Cow::Borrowed(WINDOW_TITLE);
    }

    let needs_filter = title.chars().any(char::is_control);
    if !needs_filter && title.len() <= MAX_WINDOW_TITLE_BYTES {
        return Cow::Borrowed(title);
    }

    let mut normalized = String::with_capacity(title.len().min(MAX_WINDOW_TITLE_BYTES));
    for character in title.chars().filter(|character| !character.is_control()) {
        if normalized.len() + character.len_utf8() > MAX_WINDOW_TITLE_BYTES {
            break;
        }
        normalized.push(character);
    }
    if normalized.is_empty() {
        Cow::Borrowed(WINDOW_TITLE)
    } else {
        Cow::Owned(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalized_window_title, sync_window_title_with, MAX_WINDOW_TITLE_BYTES, WINDOW_TITLE,
    };
    use std::cell::RefCell;

    #[test]
    fn sync_applies_osc_changes_once_and_restores_the_default() {
        let mut applied_title = WINDOW_TITLE.to_string();
        let applied = RefCell::new(Vec::new());

        assert!(!sync_window_title_with(&mut applied_title, "", |title| {
            applied.borrow_mut().push(title.to_string())
        },));
        assert!(sync_window_title_with(
            &mut applied_title,
            "htop — 日本",
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert!(!sync_window_title_with(
            &mut applied_title,
            "htop — 日本",
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert!(sync_window_title_with(&mut applied_title, "", |title| {
            applied.borrow_mut().push(title.to_string())
        },));

        assert_eq!(applied_title, WINDOW_TITLE);
        assert_eq!(
            applied.into_inner(),
            ["htop — 日本".to_string(), WINDOW_TITLE.to_string()]
        );
    }

    #[test]
    fn sync_removes_controls_and_truncates_on_utf8_boundaries() {
        let mut applied_title = WINDOW_TITLE.to_string();
        let applied = RefCell::new(Vec::new());

        assert!(sync_window_title_with(
            &mut applied_title,
            "a\0b\nc\u{7f}\u{85}",
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert_eq!(applied_title, "abc");
        assert!(!sync_window_title_with(
            &mut applied_title,
            "a\tb\rc",
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert!(sync_window_title_with(
            &mut applied_title,
            "\0\n\u{7f}",
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert_eq!(applied_title, WINDOW_TITLE);

        let oversized = format!("{}日", "x".repeat(MAX_WINDOW_TITLE_BYTES - 1));
        assert!(sync_window_title_with(
            &mut applied_title,
            &oversized,
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert_eq!(applied_title.len(), MAX_WINDOW_TITLE_BYTES - 1);
        assert!(applied_title.bytes().all(|byte| byte == b'x'));
        assert_eq!(
            applied.borrow().as_slice(),
            ["abc", WINDOW_TITLE, applied_title.as_str()]
        );
    }

    #[test]
    fn normalization_honors_the_exact_utf8_byte_limit() {
        let exact_ascii = "x".repeat(MAX_WINDOW_TITLE_BYTES);
        assert_eq!(normalized_window_title(&exact_ascii), exact_ascii);

        let exact_multibyte = format!("{}日", "x".repeat(MAX_WINDOW_TITLE_BYTES - "日".len()));
        assert_eq!(normalized_window_title(&exact_multibyte), exact_multibyte);

        let split_multibyte = format!("{}日", "x".repeat(MAX_WINDOW_TITLE_BYTES - "日".len() + 1));
        assert_eq!(
            normalized_window_title(&split_multibyte),
            "x".repeat(MAX_WINDOW_TITLE_BYTES - "日".len() + 1)
        );

        let first_suffix = format!("{exact_ascii}first");
        let second_suffix = format!("{exact_ascii}second");
        assert_eq!(
            normalized_window_title(&first_suffix),
            normalized_window_title(&second_suffix)
        );
    }
}
