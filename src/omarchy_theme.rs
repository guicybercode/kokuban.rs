//! Omarchy publishes a complete palette by replacing its current/theme directory.
//! Observe filesystem events off the UI thread; idle terminals do not poll files.

use crate::config::{ColorConfig, Config};
use crate::grid::cell::Color;
use crate::terminal_colors::{Rgb, TerminalColors};
use nix::errno::Errno;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify, WatchDescriptor};
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::net::Shutdown;
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

const MAX_THEME_BYTES: usize = 64 * 1024;
const COLORS_FILE: &str = "colors.toml";

/// Explicit settings keep their priority when the system palette changes.
pub(crate) struct PaletteOverrides {
    base: TerminalColors,
    foreground: Option<Rgb>,
    background: Option<Rgb>,
    selection_foreground: Option<Rgb>,
    selection_background: Option<Rgb>,
    cursor: Option<Rgb>,
    ansi: Option<[Rgb; 16]>,
}

impl PaletteOverrides {
    pub(crate) fn from_config(config: &Config) -> Self {
        let foreground = ColorConfig::parse_hex(&config.colors.foreground);
        let background = ColorConfig::parse_hex(&config.colors.background);
        Self {
            base: TerminalColors::new(foreground, background),
            foreground: config.colors.explicit_foreground.then_some(foreground),
            background: config.colors.explicit_background.then_some(background),
            selection_foreground: config
                .selection
                .explicit_foreground
                .then(|| ColorConfig::parse_hex(&config.selection.foreground)),
            selection_background: config
                .selection
                .explicit_background
                .then(|| ColorConfig::parse_hex(&config.selection.background)),
            cursor: config.colors.cursor.as_deref().map(ColorConfig::parse_hex),
            ansi: config
                .colors
                .ansi
                .as_ref()
                .map(|ansi| std::array::from_fn(|index| ColorConfig::parse_hex(&ansi[index]))),
        }
    }

    pub(crate) fn resolve(&self, theme: Option<TerminalColors>) -> TerminalColors {
        let colors = theme.unwrap_or(self.base);
        let mut colors = colors.with_defaults(
            self.foreground
                .unwrap_or_else(|| colors.resolve_foreground(Color::Default, false)),
            self.background
                .unwrap_or_else(|| colors.default_background()),
        );
        colors = colors.with_selection(
            Some(
                self.selection_foreground
                    .unwrap_or_else(|| colors.selection_foreground()),
            ),
            Some(
                self.selection_background
                    .unwrap_or_else(|| colors.selection_background()),
            ),
        );
        if let Some(cursor) = self.cursor {
            colors = colors.with_cursor(cursor);
        }
        if let Some(ansi) = self.ansi {
            colors = colors.with_ansi(ansi);
        }
        colors
    }
}

fn invalid_theme(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn parse_theme(contents: &str) -> io::Result<TerminalColors> {
    if contents.len() > MAX_THEME_BYTES {
        return Err(invalid_theme("Omarchy palette exceeds 64 KiB"));
    }
    let table: toml::Table = toml::from_str(contents)
        .map_err(|error| invalid_theme(format!("invalid Omarchy palette TOML: {error}")))?;
    let color = |key: &str| -> io::Result<Rgb> {
        let hex = table
            .get(key)
            .and_then(toml::Value::as_str)
            .and_then(|value| value.strip_prefix('#'))
            .filter(|value| value.len() == 6 && value.is_ascii())
            .ok_or_else(|| invalid_theme(format!("Omarchy {key} must be a #RRGGBB color")))?;
        let channel = |range| {
            u8::from_str_radix(&hex[range], 16)
                .map_err(|_| invalid_theme(format!("Omarchy {key} must be a #RRGGBB color")))
        };
        Ok((channel(0..2)?, channel(2..4)?, channel(4..6)?))
    };
    let foreground = color("foreground")?;
    let background = color("background")?;
    let selection_foreground = color("selection_foreground")?;
    let selection_background = color("selection_background")?;
    let cursor = color("cursor")?;
    let mut ansi = [(0, 0, 0); 16];
    for (index, entry) in ansi.iter_mut().enumerate() {
        *entry = color(&format!("color{index}"))?;
    }
    Ok(TerminalColors::new(foreground, background)
        .with_ansi(ansi)
        .with_selection(Some(selection_foreground), Some(selection_background))
        .with_cursor(cursor))
}

fn read_theme(path: &Path) -> io::Result<TerminalColors> {
    // A theme is a regular text file. NONBLOCK also avoids hanging startup or
    // shutdown if an accidental FIFO replaces it before the metadata check.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid_theme("Omarchy palette must be a regular file"));
    }
    let mut contents = String::new();
    file.take((MAX_THEME_BYTES + 1) as u64)
        .read_to_string(&mut contents)?;
    parse_theme(&contents)
}

fn read_optional_theme(path: &Path) -> Option<TerminalColors> {
    match read_theme(path) {
        Ok(colors) => Some(colors),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            log::warn!(
                "Could not load {}: {error}; keeping existing colors",
                path.display()
            );
            None
        }
    }
}

fn current_directory(home: Option<PathBuf>) -> Option<PathBuf> {
    // Omarchy itself uses HOME here, even when Kokuban's own config is in XDG.
    home.filter(|path| path.is_absolute())
        .map(|home| home.join(".config/omarchy/current"))
}

pub(crate) fn watch_current_theme(
    enabled: bool,
    on_change: impl Fn() -> bool + Send + 'static,
) -> (Option<ThemeWatcher>, Option<TerminalColors>) {
    if !enabled {
        return (None, None);
    }
    let Some(directory) = current_directory(std::env::var_os("HOME").map(PathBuf::from)) else {
        return (None, None);
    };
    if !directory.is_dir() {
        return (None, None);
    }
    match ThemeWatcher::start(directory.clone(), on_change) {
        Ok(watcher) => {
            let initial = watcher.take_update();
            (Some(watcher), initial)
        }
        Err(error) => {
            log::warn!("Omarchy live theme updates unavailable: {error}");
            (
                None,
                read_optional_theme(&directory.join("theme").join(COLORS_FILE)),
            )
        }
    }
}

#[derive(Default)]
struct ThemeUpdate {
    colors: Option<TerminalColors>,
    pending: bool,
}

pub(crate) struct ThemeWatcher {
    update: Arc<Mutex<ThemeUpdate>>,
    cancel: UnixStream,
    worker: Option<JoinHandle<()>>,
}

impl ThemeWatcher {
    fn start(
        directory: PathBuf,
        on_change: impl Fn() -> bool + Send + 'static,
    ) -> io::Result<Self> {
        let mut source = ThemeSource::new(directory)?;
        // Install watches before the first read so startup cannot miss a swap.
        let update = Arc::new(Mutex::new(ThemeUpdate {
            colors: read_optional_theme(&source.palette_path()),
            pending: false,
        }));
        let worker_update = update.clone();
        let (cancel, worker_cancel) = UnixStream::pair()?;
        let worker = thread::Builder::new()
            .name("kokuban-theme".into())
            .spawn(move || {
                if let Err(error) = source.run(worker_cancel, worker_update, on_change) {
                    log::warn!("Omarchy theme watcher stopped: {error}");
                }
            })?;
        Ok(Self {
            update,
            cancel,
            worker: Some(worker),
        })
    }

    pub(crate) fn take_update(&self) -> Option<TerminalColors> {
        let mut update = self
            .update
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        update.pending = false;
        update.colors
    }
}

impl Drop for ThemeWatcher {
    fn drop(&mut self) {
        // poll wakes on the peer's HUP; there is no timeout or idle wakeup.
        let _ = self.cancel.shutdown(Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                log::warn!("Omarchy theme watcher panicked during shutdown");
            }
        }
    }
}

struct ThemeSource {
    inotify: Inotify,
    directory: PathBuf,
    parent_watch: WatchDescriptor,
    theme_watch: Option<WatchDescriptor>,
}

impl ThemeSource {
    fn new(directory: PathBuf) -> io::Result<Self> {
        let inotify = Inotify::init(InitFlags::IN_CLOEXEC | InitFlags::IN_NONBLOCK)?;
        let parent_watch = inotify.add_watch(&directory, Self::watch_flags())?;
        let mut source = Self {
            inotify,
            directory,
            parent_watch,
            theme_watch: None,
        };
        source.watch_theme_directory()?;
        Ok(source)
    }

    fn watch_flags() -> AddWatchFlags {
        AddWatchFlags::IN_ONLYDIR
            | AddWatchFlags::IN_CLOSE_WRITE
            | AddWatchFlags::IN_MOVED_TO
            | AddWatchFlags::IN_MOVED_FROM
            | AddWatchFlags::IN_CREATE
            | AddWatchFlags::IN_DELETE
            | AddWatchFlags::IN_DELETE_SELF
            | AddWatchFlags::IN_MOVE_SELF
            | AddWatchFlags::IN_ATTRIB
    }

    fn palette_path(&self) -> PathBuf {
        self.directory.join("theme").join(COLORS_FILE)
    }

    fn watch_theme_directory(&mut self) -> io::Result<()> {
        let next = match self
            .inotify
            .add_watch(&self.directory.join("theme"), Self::watch_flags())
        {
            Ok(watch) => Some(watch),
            // Keep observing the stable parent during an incomplete publication.
            Err(Errno::ENOENT | Errno::ENOTDIR | Errno::EACCES) => None,
            Err(error) => return Err(error.into()),
        };
        if self.theme_watch != next {
            if let Some(old) = self.theme_watch.take() {
                let _ = self.inotify.rm_watch(old);
            }
            self.theme_watch = next;
        }
        Ok(())
    }

    fn run(
        &mut self,
        cancel: UnixStream,
        update: Arc<Mutex<ThemeUpdate>>,
        on_change: impl Fn() -> bool,
    ) -> io::Result<()> {
        loop {
            let mut descriptors = [
                PollFd::new(cancel.as_fd(), PollFlags::POLLIN),
                PollFd::new(self.inotify.as_fd(), PollFlags::POLLIN),
            ];
            match poll(&mut descriptors, PollTimeout::NONE) {
                Err(Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            if descriptors[0].any() != Some(false) {
                return Ok(());
            }
            if descriptors[1]
                .revents()
                .unwrap_or(PollFlags::POLLERR)
                .intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL)
            {
                return Err(io::Error::other("Omarchy inotify descriptor closed"));
            }
            let mut changed = false;
            // Bound a burst so shutdown remains responsive even under continuous
            // writes. A queued UI notification coalesces subsequent palettes.
            for _ in 0..64 {
                let events = match self.inotify.read_events() {
                    Ok(events) => events,
                    Err(Errno::EAGAIN) => break,
                    Err(Errno::EINTR) => continue,
                    Err(error) => return Err(error.into()),
                };
                for event in events {
                    if event.wd == self.parent_watch
                        && event.mask.intersects(
                            AddWatchFlags::IN_DELETE_SELF
                                | AddWatchFlags::IN_MOVE_SELF
                                | AddWatchFlags::IN_IGNORED
                                | AddWatchFlags::IN_UNMOUNT,
                        )
                    {
                        return Err(io::Error::other("Omarchy current directory was removed"));
                    }
                    if event.mask.contains(AddWatchFlags::IN_Q_OVERFLOW)
                        || (event.wd == self.parent_watch
                            && (event.name.as_deref() == Some(OsStr::new("theme"))
                                || (event.name.is_none()
                                    && event.mask.contains(AddWatchFlags::IN_ATTRIB))))
                        || (Some(event.wd) == self.theme_watch
                            && (event.name.as_deref() == Some(OsStr::new(COLORS_FILE))
                                || event.name.is_none()))
                    {
                        changed = true;
                    }
                }
            }
            if !changed {
                continue;
            }
            self.watch_theme_directory()?;
            let Some(colors) = read_optional_theme(&self.palette_path()) else {
                continue;
            };
            let notify = {
                let mut update = update.lock().unwrap_or_else(|poison| poison.into_inner());
                if update.colors == Some(colors) {
                    false
                } else {
                    update.colors = Some(colors);
                    let notify = !update.pending;
                    update.pending = true;
                    notify
                }
            };
            if notify && !on_change() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn palette(background: &str) -> String {
        let mut text = format!(
            "foreground = '#a9b1d6'\nbackground = '{background}'\n\
             selection_foreground = '#c0caf5'\nselection_background = '#7aa2f7'\ncursor = '#c0caf5'\n"
        );
        for index in 0..16 {
            text.push_str(&format!("color{index} = '#{index:02x}2233'\n"));
        }
        text
    }

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "kokuban-theme-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(path.join("theme")).unwrap();
            Self(path)
        }

        fn replace(&self, contents: &str) {
            let next = self.0.join("next-theme");
            std::fs::create_dir(&next).unwrap();
            std::fs::write(next.join(COLORS_FILE), contents).unwrap();
            std::fs::remove_dir_all(self.0.join("theme")).unwrap();
            std::fs::rename(next, self.0.join("theme")).unwrap();
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn complete_palette_preserves_every_semantic_role_and_ansi_index() {
        let colors = parse_theme(&palette("#1A1b26")).unwrap();
        assert_eq!(colors.default_background(), (26, 27, 38));
        assert_eq!(
            colors.resolve_foreground(Color::Default, false),
            (169, 177, 214)
        );
        assert_eq!(colors.selection_foreground(), (192, 202, 245));
        assert_eq!(colors.selection_background(), (122, 162, 247));
        assert_eq!(colors.cursor(), Some((192, 202, 245)));
        let defaults = TerminalColors::new((1, 2, 3), (4, 5, 6));
        for index in 0..=255 {
            let expected = if index < 16 {
                (index, 34, 51)
            } else {
                defaults.resolve_background(Color::Indexed(index))
            };
            assert_eq!(colors.resolve_background(Color::Indexed(index)), expected);
            assert_eq!(
                colors.resolve_foreground(Color::Indexed(index), false),
                expected
            );
            let bright = if index < 8 {
                (index + 8, 34, 51)
            } else {
                expected
            };
            assert_eq!(
                colors.resolve_foreground(Color::Indexed(index), true),
                bright
            );
        }
        assert_eq!(
            colors.resolve_foreground(Color::Rgb(7, 8, 9), true),
            (7, 8, 9)
        );
    }

    #[test]
    fn partial_invalid_unicode_and_oversized_themes_are_rejected_as_a_whole() {
        let complete = palette("#1a1b26");
        for line in complete.lines() {
            assert!(
                parse_theme(&complete.replace(&format!("{line}\n"), "")).is_err(),
                "missing {line}"
            );
        }
        for invalid in ["#xxxxxx", "#123", "123456", "#你好", "#1234567", "##12345"] {
            assert!(parse_theme(&palette(invalid)).is_err(), "{invalid}");
        }
        assert!(parse_theme(&(complete.clone() + &" ".repeat(MAX_THEME_BYTES))).is_err());
        assert!(parse_theme(&(complete + "\ncolor0 = '#123456'\n")).is_err());
    }

    #[test]
    fn explicit_values_equal_to_defaults_still_override_system_colors() {
        let config: Config = toml::from_str(
            "[colors]\nforeground = '#c0c0c0'\n[selection]\nbackground = '#b4d5fe'\n",
        )
        .unwrap();
        let overrides = PaletteOverrides::from_config(&config);
        for background in ["#010203", "#fafbfc"] {
            let theme = parse_theme(&palette(background)).unwrap();
            let colors = overrides.resolve(Some(theme));
            assert_eq!(
                colors.resolve_foreground(Color::Default, false),
                (192, 192, 192)
            );
            assert_eq!(colors.default_background(), theme.default_background());
            assert_eq!(colors.selection_background(), (180, 213, 254));
            assert_eq!(colors.selection_foreground(), theme.selection_foreground());
            assert_eq!(colors.cursor(), theme.cursor());
        }
    }

    #[test]
    fn optional_cursor_and_ansi_overrides_survive_palette_changes() {
        let mut source = "[colors]\ncursor = '#112233'\nansi = [".to_string();
        for index in 0..16 {
            source.push_str(&format!("'#{index:02x}4455',"));
        }
        source.push_str("]\n");
        let config: Config = toml::from_str(&source).unwrap();
        let overrides = PaletteOverrides::from_config(&config);
        for theme in [None, Some(parse_theme(&palette("#abcdef")).unwrap())] {
            let colors = overrides.resolve(theme);
            assert_eq!(colors.cursor(), Some((17, 34, 51)));
            for index in 0..16 {
                assert_eq!(
                    colors.resolve_background(Color::Indexed(index)),
                    (index, 68, 85)
                );
            }
        }
    }

    #[test]
    fn disabling_omarchy_does_not_read_environment_or_start_a_worker() {
        let config: Config = toml::from_str("[omarchy]\nenabled = false\n").unwrap();
        let (watcher, colors) = watch_current_theme(config.omarchy.enabled, || panic!("disabled"));
        assert!(watcher.is_none());
        assert!(colors.is_none());
        let colors = PaletteOverrides::from_config(&Config::default()).resolve(None);
        assert_eq!(colors.default_background(), (26, 26, 46));
        assert_eq!(colors.selection_foreground(), colors.default_background());
        assert_eq!(colors.selection_background(), (192, 192, 192));
        assert_eq!(colors.cursor(), None);
        assert_eq!(
            current_directory(Some("/home/test".into())).unwrap(),
            Path::new("/home/test/.config/omarchy/current")
        );
        assert!(current_directory(Some("relative-home".into())).is_none());
    }

    #[test]
    fn watches_directory_replacement_and_in_place_writes_while_retaining_invalid_theme() {
        let directory = Directory::new();
        directory.replace(&palette("#010203"));
        let (sender, receiver) = mpsc::channel();
        let watcher =
            ThemeWatcher::start(directory.0.clone(), move || sender.send(()).is_ok()).unwrap();
        assert_eq!(
            watcher.take_update().unwrap().default_background(),
            (1, 2, 3)
        );
        directory.replace(&palette("#fafbfc"));
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            watcher.take_update().unwrap().default_background(),
            (250, 251, 252)
        );
        directory.replace("background = '#000000'\n");
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(
            watcher.take_update().unwrap().default_background(),
            (250, 251, 252)
        );
        std::fs::write(
            directory.0.join("theme").join(COLORS_FILE),
            palette("#040506"),
        )
        .unwrap();
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            watcher.take_update().unwrap().default_background(),
            (4, 5, 6)
        );
        let start = Instant::now();
        drop(watcher);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "idle worker must wake for shutdown"
        );
    }

    #[test]
    fn missing_initial_theme_is_loaded_after_creation() {
        let directory = Directory::new();
        std::fs::remove_dir(directory.0.join("theme")).unwrap();
        let (sender, receiver) = mpsc::channel();
        let watcher =
            ThemeWatcher::start(directory.0.clone(), move || sender.send(()).is_ok()).unwrap();
        assert!(watcher.take_update().is_none());
        std::fs::create_dir(directory.0.join("theme")).unwrap();
        std::fs::write(
            directory.0.join("theme").join(COLORS_FILE),
            palette("#070809"),
        )
        .unwrap();
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            watcher.take_update().unwrap().default_background(),
            (7, 8, 9)
        );
    }

    #[test]
    fn queued_notification_keeps_the_latest_palette_without_queueing_every_update() {
        let directory = Directory::new();
        directory.replace(&palette("#010203"));
        let (sender, receiver) = mpsc::channel();
        let watcher =
            ThemeWatcher::start(directory.0.clone(), move || sender.send(()).is_ok()).unwrap();
        directory.replace(&palette("#040506"));
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        // Receiving a UI wake does not consume its value until take_update.
        directory.replace(&palette("#070809"));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let colors = watcher.update.lock().unwrap().colors.unwrap();
            if colors.default_background() == (7, 8, 9) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not observe the next palette"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            receiver.try_recv().is_err(),
            "only one UI wake may be pending"
        );
        assert_eq!(
            watcher.take_update().unwrap().default_background(),
            (7, 8, 9)
        );
        directory.replace(&palette("#0a0b0c"));
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            watcher.take_update().unwrap().default_background(),
            (10, 11, 12)
        );
    }

    #[test]
    fn invalid_theme_directory_recovers_after_a_valid_replacement() {
        let directory = Directory::new();
        directory.replace(&palette("#010203"));
        let (sender, receiver) = mpsc::channel();
        let watcher =
            ThemeWatcher::start(directory.0.clone(), move || sender.send(()).is_ok()).unwrap();
        std::fs::remove_dir_all(directory.0.join("theme")).unwrap();
        std::fs::write(directory.0.join("theme"), "incomplete publication").unwrap();
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(
            watcher.take_update().unwrap().default_background(),
            (1, 2, 3)
        );
        std::fs::remove_file(directory.0.join("theme")).unwrap();
        std::fs::create_dir(directory.0.join("theme")).unwrap();
        std::fs::write(
            directory.0.join("theme").join(COLORS_FILE),
            palette("#0a0b0c"),
        )
        .unwrap();
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            watcher.take_update().unwrap().default_background(),
            (10, 11, 12)
        );
    }

    #[test]
    fn reader_rejects_oversized_files_and_non_regular_sources() {
        let directory = Directory::new();
        let path = directory.0.join("theme").join(COLORS_FILE);
        std::fs::write(&path, " ".repeat(MAX_THEME_BYTES + 1)).unwrap();
        assert!(read_theme(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        nix::unistd::mkfifo(
            &path,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(read_theme(&path).is_err());
        assert!(read_theme(&directory.0).is_err());
    }
}
