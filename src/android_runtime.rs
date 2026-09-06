//! Per-application Android storage and per-session shell setup.
//!
//! The NativeActivity supplies its internal data directory. No process-global
//! environment or current-directory mutation is needed, so startup is safe
//! after the JVM and Android event threads have started.

use crate::pty::{Pty, PtyError};
use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

const ANDROID_PATH: &str =
    "/system/bin:/system/xbin:/vendor/bin:/product/bin:/apex/com.android.runtime/bin";

pub struct AndroidRuntime {
    home: PathBuf,
    config_path: PathBuf,
    environment: Vec<(OsString, OsString)>,
}

impl AndroidRuntime {
    pub fn prepare(private_data_dir: &Path) -> io::Result<Self> {
        if !private_data_dir.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Android internal data directory must be absolute",
            ));
        }
        // Android owns and creates this directory; fail if the supplied root
        // is missing, rather than accidentally creating an unrelated tree.
        let base = fs::canonicalize(private_data_dir)?;
        if !base.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "Android internal data path is not a directory",
            ));
        }
        let home = base.join("home");
        let config = base.join("config");
        let cache = base.join("cache");
        let data = base.join("data");
        let temp = base.join("tmp");
        for directory in [&home, &config, &cache, &data, &temp] {
            private_directory(directory)?;
        }
        private_directory(&home.join(".ssh"))?;
        private_directory(&config.join("kokuban"))?;

        let environment = [
            ("HOME", home.as_os_str()),
            ("XDG_CONFIG_HOME", config.as_os_str()),
            ("XDG_CACHE_HOME", cache.as_os_str()),
            ("XDG_DATA_HOME", data.as_os_str()),
            ("TMPDIR", temp.as_os_str()),
            ("PATH", std::ffi::OsStr::new(ANDROID_PATH)),
            ("LANG", std::ffi::OsStr::new("C.UTF-8")),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), value.to_owned()))
        .collect();

        Ok(Self {
            home,
            config_path: config.join("kokuban/kokuban.toml"),
            environment,
        })
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn home_dir(&self) -> &Path {
        &self.home
    }

    /// Keep the returned PTY alive across surface suspension and rotation.
    /// Dropping it intentionally hangs up and reaps the shell process group.
    pub fn spawn_shell(
        &self,
        cols: u16,
        rows: u16,
        kitty_graphics: bool,
        sixel_graphics: bool,
    ) -> Result<Pty, PtyError> {
        Pty::spawn_with_environment(
            cols,
            rows,
            kitty_graphics,
            sixel_graphics,
            &self.home,
            &self.environment,
        )
    }
}

fn private_directory(path: &Path) -> io::Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            // A linked .ssh/config/cache directory should never escape the
            // application's private storage or change another path's mode.
            if !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "private storage path is not a directory: {}",
                        path.display()
                    ),
                ));
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::AndroidRuntime;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "kokuban-android-runtime-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn private_layout_survives_restart_without_changing_global_state() {
        let directory = TestDirectory::new();
        let previous_home = std::env::var_os("HOME");
        let previous_cwd = std::env::current_dir().unwrap();
        let runtime = AndroidRuntime::prepare(&directory.0).unwrap();
        let config_path = runtime.config_path().to_path_buf();
        fs::write(&config_path, "# keep user settings").unwrap();
        fs::write(runtime.home_dir().join("project.txt"), "project").unwrap();

        let restarted = AndroidRuntime::prepare(&directory.0).unwrap();
        assert_eq!(
            fs::read_to_string(restarted.config_path()).unwrap(),
            "# keep user settings"
        );
        assert_eq!(
            fs::read_to_string(restarted.home_dir().join("project.txt")).unwrap(),
            "project"
        );
        assert_eq!(std::env::var_os("HOME"), previous_home);
        assert_eq!(std::env::current_dir().unwrap(), previous_cwd);
        for name in [
            "home",
            "home/.ssh",
            "config",
            "config/kokuban",
            "tmp",
            "cache",
            "data",
        ] {
            assert_eq!(
                fs::metadata(directory.0.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        let home = runtime
            .environment
            .iter()
            .find(|(name, _)| name == "HOME")
            .unwrap();
        assert_eq!(Path::new(&home.1), runtime.home_dir());
    }

    #[test]
    fn session_starts_in_its_home_and_accepts_shell_input() {
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};

        let directory = TestDirectory::new();
        let runtime = AndroidRuntime::prepare(&directory.0).unwrap();
        let pty = runtime.spawn_shell(80, 24, false, false).unwrap();
        // Escape the marker so the terminal's echoed command is not confused
        // with the shell's actual result.
        pty.write_all_cancellable(
            b"printf '\\137\\137RUNTIME:%s:%s:END\\137\\137' \"$HOME\" \"$PWD\"\n",
            &AtomicBool::new(false),
        )
        .unwrap();
        let mut output = Vec::new();
        let mut buffer = [0; 4096];
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !pty
                .wait_readable(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                break;
            }
            match pty.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => output.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(error) => panic!("PTY read failed: {error}"),
            }
            if output.windows(b":END__".len()).any(|part| part == b":END__") {
                break;
            }
        }
        let output = String::from_utf8_lossy(&output);
        let expected = format!("__RUNTIME:{0}:{0}:END__", runtime.home_dir().display());
        assert!(output.contains(&expected), "shell output: {output}");
    }

    #[test]
    fn rejects_paths_that_do_not_identify_private_storage() {
        assert!(AndroidRuntime::prepare(Path::new("relative")).is_err());
        let directory = TestDirectory::new();
        assert!(AndroidRuntime::prepare(&directory.0.join("missing")).is_err());
        let file = directory.0.join("file");
        fs::write(&file, "").unwrap();
        assert!(AndroidRuntime::prepare(&file).is_err());
    }

    #[test]
    fn rejects_linked_private_subdirectories() {
        let directory = TestDirectory::new();
        let outside = TestDirectory::new();
        symlink(&outside.0, directory.0.join("home")).unwrap();
        assert!(AndroidRuntime::prepare(&directory.0).is_err());
        assert!(!outside.0.join(".ssh").exists());
    }
}
