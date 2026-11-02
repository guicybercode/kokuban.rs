use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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

/// Title the focused pane asked for, published off the main thread and
/// applied on it. Keeps native title updates independent from the pane
/// tree lock, which can be held for a long time while decoding output.
pub(crate) struct WindowTitleMailbox {
    desired_title: Mutex<String>,
    pending: AtomicBool,
}

impl WindowTitleMailbox {
    pub(crate) fn new() -> Self {
        Self {
            desired_title: Mutex::new(WINDOW_TITLE.to_string()),
            pending: AtomicBool::new(true),
        }
    }

    /// Title requested with `--title`; OSC 0/2 updates replace it as usual.
    pub(crate) fn set_initial_title(&self, title: &str) {
        self.publish(title);
    }

    pub(crate) fn publish(&self, raw_title: &str) -> bool {
        let next_title = normalized_window_title(raw_title);
        let mut desired_title = self
            .desired_title
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if desired_title.as_str() == next_title.as_ref() {
            return false;
        }

        desired_title.clear();
        desired_title.push_str(next_title.as_ref());
        drop(desired_title);
        self.pending.store(true, Ordering::Release);
        true
    }

    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub(crate) fn has_pending_update(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub(crate) fn take_pending_title(&self) -> Option<String> {
        if !self.pending.swap(false, Ordering::AcqRel) {
            return None;
        }

        Some(
            self.desired_title
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        )
    }
}

/// Apply the mailbox's pending title through `set_title`, if any changed.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn sync_pending_window_title_with<F>(
    window_title: &WindowTitleMailbox,
    applied_title: &mut String,
    set_title: F,
) -> bool
where
    F: FnOnce(&str),
{
    let Some(title) = window_title.take_pending_title() else {
        return false;
    };
    sync_window_title_with(applied_title, &title, set_title)
}

#[cfg(test)]
mod tests {
    use super::{
        normalized_window_title, sync_pending_window_title_with, sync_window_title_with,
        WindowTitleMailbox, MAX_WINDOW_TITLE_BYTES, WINDOW_TITLE,
    };
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::thread;

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

    #[test]
    fn window_title_sync_skips_work_without_a_pending_update() {
        let window_title = WindowTitleMailbox::new();
        let mut applied_title = WINDOW_TITLE.to_string();
        let setters = Cell::new(0);

        assert!(!sync_pending_window_title_with(
            &window_title,
            &mut applied_title,
            |_| setters.set(setters.get() + 1),
        ));
        assert!(!window_title.has_pending_update());

        assert!(!sync_pending_window_title_with(
            &window_title,
            &mut applied_title,
            |_| setters.set(setters.get() + 1),
        ));
        assert_eq!(setters.get(), 0);
    }

    #[test]
    fn window_title_mailbox_coalesces_to_the_latest_normalized_value() {
        let window_title = WindowTitleMailbox::new();
        let mut applied_title = WINDOW_TITLE.to_string();
        let applied = RefCell::new(Vec::new());

        assert!(window_title.publish("first"));
        assert!(window_title.publish("latest\n"));
        assert!(!window_title.publish("latest"));
        assert!(sync_pending_window_title_with(
            &window_title,
            &mut applied_title,
            |title| applied.borrow_mut().push(title.to_string()),
        ));

        assert!(!window_title.has_pending_update());
        assert_eq!(applied.into_inner(), ["latest"]);
    }

    #[test]
    fn window_title_update_after_take_remains_pending_for_the_next_tick() {
        let window_title = WindowTitleMailbox::new();
        let mut applied_title = WINDOW_TITLE.to_string();
        let applied = RefCell::new(Vec::new());

        window_title.publish("first");
        let first = window_title
            .take_pending_title()
            .expect("the first update should be pending");
        assert!(!window_title.has_pending_update());

        window_title.publish("latest");
        assert!(window_title.has_pending_update());
        assert!(sync_window_title_with(
            &mut applied_title,
            &first,
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert!(sync_pending_window_title_with(
            &window_title,
            &mut applied_title,
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert!(!window_title.has_pending_update());
        assert_eq!(applied.into_inner(), ["first", "latest"]);
    }

    #[test]
    fn window_title_publish_between_swap_and_clone_keeps_the_latest_value_pending() {
        let window_title = Arc::new(WindowTitleMailbox::new());
        let mut applied_title = WINDOW_TITLE.to_string();
        let applied = RefCell::new(Vec::new());

        window_title.publish("first");
        let mut desired_title = window_title
            .desired_title
            .lock()
            .expect("the title mailbox should lock");
        let consumer = window_title.clone();
        let take = thread::spawn(move || consumer.take_pending_title());

        while window_title.has_pending_update() {
            thread::yield_now();
        }

        desired_title.clear();
        desired_title.push_str("latest");
        drop(desired_title);
        window_title.pending.store(true, Ordering::Release);

        let taken = take
            .join()
            .expect("the title consumer should not panic")
            .expect("the raced title should be available");
        assert_eq!(taken, "latest");
        assert!(window_title.has_pending_update());

        assert!(sync_window_title_with(
            &mut applied_title,
            &taken,
            |title| applied.borrow_mut().push(title.to_string()),
        ));
        assert!(!sync_pending_window_title_with(
            window_title.as_ref(),
            &mut applied_title,
            |_| panic!("the redundant pending update must be deduplicated"),
        ));
        assert!(!window_title.has_pending_update());
        assert_eq!(applied.into_inner(), ["latest"]);
    }

    #[test]
    fn window_title_setter_runs_after_the_mailbox_lock_is_released() {
        let window_title = WindowTitleMailbox::new();
        let mut applied_title = WINDOW_TITLE.to_string();

        window_title.publish("main thread");
        assert!(sync_pending_window_title_with(
            &window_title,
            &mut applied_title,
            |_| {
                assert!(
                    window_title.desired_title.try_lock().is_ok(),
                    "the title mailbox must be unlocked before calling AppKit"
                );
            },
        ));
    }
}
