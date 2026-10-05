//! Desktop notification delivery and BEL attention requests.
//!
//! Title and body come from untrusted terminal output. Custom commands must
//! never run through a shell: pass text only as argv or environment variables.

use crate::config::NotificationsConfig;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MIN_INTERVAL: Duration = Duration::from_millis(750);

/// Environment variables passed to `[notifications] command` (never via a shell).
pub const ENV_TITLE: &str = "KOKUBAN_NOTIFICATION_TITLE";
pub const ENV_BODY: &str = "KOKUBAN_NOTIFICATION_BODY";

pub use crate::parser::osc_notify::NotificationRequest;

/// Shared delivery policy: focus suppression, rate limits, and platform backends.
pub struct NotificationController {
    config: NotificationsConfig,
    /// Shared with the window focus flag (`window_is_key` / Linux focus).
    window_focused: Arc<AtomicBool>,
    last_delivered: Mutex<Option<Instant>>,
    /// BEL / urgency: UI thread should call `request_user_attention` when set.
    attention_requested: AtomicBool,
}

impl NotificationController {
    pub fn new(config: NotificationsConfig, window_focused: Arc<AtomicBool>) -> Self {
        Self {
            config,
            window_focused,
            last_delivered: Mutex::new(None),
            attention_requested: AtomicBool::new(false),
        }
    }

    /// Clear a pending attention request when the window becomes focused.
    pub fn on_focus_gained(&self) {
        self.attention_requested.store(false, Ordering::Release);
    }

    pub fn window_focused(&self) -> bool {
        self.window_focused.load(Ordering::Acquire)
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Queue a desktop notification unless disabled, focused, empty, or rate-limited.
    pub fn handle_notification(&self, request: NotificationRequest) {
        if !self.config.enabled || request.is_empty() {
            return;
        }
        if self.window_focused() {
            return;
        }
        if !self.allow_delivery() {
            return;
        }
        deliver(&self.config, &request);
    }

    /// BEL: ask the window to request user attention when unfocused.
    pub fn handle_bell(&self) {
        if !self.config.enabled {
            return;
        }
        if self.window_focused() {
            return;
        }
        self.attention_requested.store(true, Ordering::Release);
    }

    /// UI thread: returns true once per pending attention request.
    pub fn take_attention_request(&self) -> bool {
        self.attention_requested.swap(false, Ordering::AcqRel)
    }

    fn allow_delivery(&self) -> bool {
        let Ok(mut last) = self.last_delivered.lock() else {
            return false;
        };
        let now = Instant::now();
        if let Some(previous) = *last {
            if now.saturating_duration_since(previous) < MIN_INTERVAL {
                return false;
            }
        }
        *last = Some(now);
        true
    }
}

fn deliver(config: &NotificationsConfig, request: &NotificationRequest) {
    let title = sanitize_notification_text(&request.title);
    let body = sanitize_notification_text(&request.body);
    if title.is_empty() && body.is_empty() {
        return;
    }
    if let Some(command) = config.command.as_deref().filter(|command| !command.is_empty()) {
        if let Err(error) = run_notification_command(command, &title, &body) {
            log::warn!("notification command failed: {error}");
        }
        return;
    }
    if let Err(error) = deliver_native(&title, &body) {
        log::warn!("native notification failed: {error}");
    }
}

/// Strip control characters from untrusted OSC title/body text.
pub fn sanitize_notification_text(text: &str) -> String {
    text.chars().filter(|character| !character.is_control()).collect()
}

/// Escape text for notify-send's Pango markup body/summary.
pub fn escape_notify_send_markup(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

/// Spawn a helper and reap it on a background thread so short-lived notify
/// processes do not accumulate as zombies.
fn spawn_and_reap(mut command: Command) -> std::io::Result<()> {
    let child = command.spawn()?;
    reap_in_background(child);
    Ok(())
}

fn reap_in_background(mut child: Child) {
    let _ = std::thread::Builder::new()
        .name("kokuban-notify-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

/// Run the configured command with title/body as env vars only — never `sh -c`.
pub fn run_notification_command(command: &str, title: &str, body: &str) -> std::io::Result<()> {
    let mut cmd = Command::new(command);
    cmd.env(ENV_TITLE, title)
        .env(ENV_BODY, body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    spawn_and_reap(cmd)
}

#[cfg(target_os = "linux")]
fn deliver_native(title: &str, body: &str) -> std::io::Result<()> {
    // notify-send takes title and body as separate argv; `--` stops option parsing.
    // Body/summary are Pango markup on common hosts — escape before argv.
    let display_title = if title.is_empty() { "kokuban" } else { title };
    let mut cmd = Command::new("notify-send");
    cmd.arg("--")
        .arg(escape_notify_send_markup(display_title))
        .arg(escape_notify_send_markup(body))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    spawn_and_reap(cmd)
}

#[cfg(target_os = "macos")]
fn deliver_native(title: &str, body: &str) -> std::io::Result<()> {
    // Notification Center via osascript. Title/body are argv only (no shell).
    // Constant script; untrusted strings never enter `-e`.
    const SCRIPT: &str = r#"on run argv
  set notifTitle to item 1 of argv
  set notifBody to item 2 of argv
  display notification notifBody with title notifTitle
end run"#;
    let display_title = if title.is_empty() { "kokuban" } else { title };
    let mut cmd = Command::new("osascript");
    cmd.arg("-e")
        .arg(SCRIPT)
        .arg("--")
        .arg(display_title)
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    spawn_and_reap(cmd)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn deliver_native(_title: &str, _body: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    fn temp_script(body: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "kokuban-notify-test-{}-{nanos}",
            std::process::id()
        ));
        fs::write(&path, body).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[test]
    fn custom_command_passes_shell_metacharacters_without_evaluation() {
        // Proves title/body with `$(...)` and `;` never reach a shell.
        let out = std::env::temp_dir().join(format!(
            "kokuban-notify-capture-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&out);
        let marker = std::env::temp_dir().join(format!(
            "kokuban-notify-pwned-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&marker);

        let script = temp_script(&format!(
            "#!/bin/sh\nprintf '%s\\n%s\\n' \"$KOKUBAN_NOTIFICATION_TITLE\" \"$KOKUBAN_NOTIFICATION_BODY\" > '{}'\n",
            out.display()
        ));

        let title = format!("$(touch {})", marker.display());
        let body = format!("; touch {}; echo pwned", marker.display());

        run_notification_command(script.to_str().unwrap(), &title, &body).unwrap();

        for _ in 0..50 {
            if out.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        let captured = fs::read_to_string(&out).expect("command should write env capture");
        let mut lines = captured.lines();
        assert_eq!(lines.next(), Some(title.as_str()));
        assert_eq!(lines.next(), Some(body.as_str()));
        assert!(
            !marker.exists(),
            "shell metacharacters in title/body must not be evaluated"
        );

        let _ = fs::remove_file(&out);
        let _ = fs::remove_file(&script);
        let _ = fs::remove_file(&marker);
    }

    #[test]
    fn focused_window_suppresses_notifications_and_bell() {
        let focused = Arc::new(AtomicBool::new(true));
        let controller = NotificationController::new(
            NotificationsConfig {
                enabled: true,
                command: None,
            },
            focused,
        );
        controller.handle_notification(NotificationRequest::new("t", "b"));
        controller.handle_bell();
        assert!(!controller.take_attention_request());
    }

    #[test]
    fn bell_requests_attention_when_unfocused() {
        let focused = Arc::new(AtomicBool::new(false));
        let controller = NotificationController::new(
            NotificationsConfig {
                enabled: true,
                command: None,
            },
            focused,
        );
        controller.handle_bell();
        assert!(controller.take_attention_request());
        assert!(!controller.take_attention_request());
    }

    #[test]
    fn disabled_config_drops_notifications_and_bell() {
        let focused = Arc::new(AtomicBool::new(false));
        let controller = NotificationController::new(
            NotificationsConfig {
                enabled: false,
                command: None,
            },
            focused,
        );
        controller.handle_notification(NotificationRequest::new("t", "b"));
        controller.handle_bell();
        assert!(!controller.take_attention_request());
    }

    #[test]
    fn sanitize_strips_control_characters_from_title_and_body() {
        let cleaned = sanitize_notification_text("ok\u{0007}title\n\tline\u{001b}[0m");
        assert_eq!(cleaned, "oktitleline[0m");
        assert!(!cleaned.chars().any(char::is_control));
    }

    #[test]
    fn notify_send_markup_escapes_ampersand_and_angles() {
        assert_eq!(
            escape_notify_send_markup("a & b <c> \"q\""),
            "a &amp; b &lt;c&gt; \"q\""
        );
        assert_eq!(escape_notify_send_markup("plain"), "plain");
    }

    #[test]
    fn spawn_and_reap_reaps_short_lived_child() {
        // Portable helper: macOS runners do not ship /bin/true.
        let script = temp_script("#!/bin/sh\nexit 0\n");
        let mut cmd = Command::new(&script);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        spawn_and_reap(cmd).unwrap();
        // Give the reaper thread a moment; the important part is spawn succeeds
        // and wait runs without leaving the Child dropped un-waited on this thread.
        thread::sleep(Duration::from_millis(50));
        let _ = fs::remove_file(&script);
    }

    #[test]
    fn deliver_sanitizes_before_invoking_custom_command() {
        let out = std::env::temp_dir().join(format!(
            "kokuban-notify-sanitized-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&out);
        let script = temp_script(&format!(
            "#!/bin/sh\nprintf '%s\\n%s\\n' \"$KOKUBAN_NOTIFICATION_TITLE\" \"$KOKUBAN_NOTIFICATION_BODY\" > '{}'\n",
            out.display()
        ));
        let focused = Arc::new(AtomicBool::new(false));
        let controller = NotificationController::new(
            NotificationsConfig {
                enabled: true,
                command: Some(script.to_string_lossy().into_owned()),
            },
            focused,
        );
        controller.handle_notification(NotificationRequest::new(
            "hi\u{0007}&x",
            "body\n<tag>",
        ));
        for _ in 0..50 {
            if out.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let captured = fs::read_to_string(&out).expect("sanitized capture");
        let mut lines = captured.lines();
        // Controls stripped; markup characters preserved for custom commands
        // (only notify-send escapes Pango).
        assert_eq!(lines.next(), Some("hi&x"));
        assert_eq!(lines.next(), Some("body<tag>"));
        let _ = fs::remove_file(&out);
        let _ = fs::remove_file(&script);
    }
}
