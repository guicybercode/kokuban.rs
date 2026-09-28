//! Updates from GitHub releases. Network, archive and checksum work goes
//! through the system `curl`, `tar`, `shasum`/`sha256sum` and `hdiutil`, so
//! no HTTP or TLS stack is linked into the terminal.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};

const REPOSITORY: &str = "https://github.com/guicybercode/kokuban.rs";
const CHECK_TIMEOUT_SECONDS: &str = "15";
const DOWNLOAD_TIMEOUT_SECONDS: &str = "600";
/// Pretend to run this version; lets an installed release test the updater.
const FROM_VERSION_ENV: &str = "KOKUBAN_UPDATE_FROM_VERSION";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UpdateError(String);

impl UpdateError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl From<std::io::Error> for UpdateError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl std::fmt::Display for Version {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Parse release versions such as `v0.4`, `v0.4.1` or `0.4.0`. Pre-release
/// and build suffixes are rejected so they are never offered as updates.
pub fn parse_version(text: &str) -> Option<Version> {
    let text = text.strip_prefix('v').unwrap_or(text);
    let mut parts = text.split('.');
    let mut next = |required: bool| match parts.next() {
        Some(part) if !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()) => {
            part.parse().ok()
        }
        None if !required => Some(0),
        _ => None,
    };
    let version = Version {
        major: next(true)?,
        minor: next(true)?,
        patch: next(false)?,
    };
    parts.next().is_none().then_some(version)
}

pub fn current_version() -> Version {
    std::env::var(FROM_VERSION_ENV)
        .ok()
        .and_then(|text| parse_version(&text))
        .or_else(|| parse_version(env!("CARGO_PKG_VERSION")))
        .expect("Cargo package version is a release version")
}

/// Extract the tag from the page GitHub redirects `/releases/latest` to.
pub fn tag_from_release_url(url: &str) -> Option<String> {
    let (_, tag) = url.trim().rsplit_once("/releases/tag/")?;
    // The tag becomes part of URLs and file names; keep it to safe characters.
    let safe = !tag.is_empty()
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'));
    safe.then(|| tag.to_string())
}

/// Target triple of the running binary, as used in release file names.
pub fn release_target() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else {
        "unsupported"
    }
}

fn release_stem(tag: &str) -> String {
    format!("kokuban-{tag}-{}", release_target())
}

fn download_url(tag: &str, file: &str) -> String {
    format!("{REPOSITORY}/releases/download/{tag}/{file}")
}

/// Read the digest for `file` from a `sha256sum`-format line.
pub fn parse_checksum(text: &str, file: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (digest, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start();
        let name = name.strip_prefix('*').unwrap_or(name);
        let valid = digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
        (valid && name == file).then(|| digest.to_ascii_lowercase())
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// `Kokuban.app`, replaced whole from the release disk image.
    AppBundle(PathBuf),
    /// A standalone executable, replaced from the release archive.
    Standalone(PathBuf),
}

pub fn install_kind(executable: &Path) -> InstallKind {
    let macos = executable.parent();
    let contents = macos.and_then(Path::parent);
    let bundle = contents.and_then(Path::parent);
    match (macos, contents, bundle) {
        (Some(macos), Some(contents), Some(bundle))
            if macos.file_name() == Some("MacOS".as_ref())
                && contents.file_name() == Some("Contents".as_ref())
                && bundle.extension() == Some("app".as_ref()) =>
        {
            InstallKind::AppBundle(bundle.to_path_buf())
        }
        _ => InstallKind::Standalone(executable.to_path_buf()),
    }
}

fn run(program: &str, arguments: &[&std::ffi::OsStr]) -> Result<String, UpdateError> {
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => UpdateError::new(format!("{program} is required")),
            _ => UpdateError::new(format!("could not run {program}: {error}")),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(UpdateError::new(if detail.is_empty() {
            format!("{program} failed ({})", output.status)
        } else {
            format!("{program} failed: {detail}")
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn curl(arguments: &[&std::ffi::OsStr]) -> Result<String, UpdateError> {
    let mut all: Vec<&std::ffi::OsStr> = vec!["-fsSL".as_ref(), "--proto".as_ref(), "=https".as_ref()];
    all.extend_from_slice(arguments);
    run("curl", &all)
}

/// Tag of the newest published release (GitHub excludes pre-releases).
pub fn latest_release_tag() -> Result<String, UpdateError> {
    let latest = format!("{REPOSITORY}/releases/latest");
    let effective = curl(&[
        "--max-time".as_ref(),
        CHECK_TIMEOUT_SECONDS.as_ref(),
        "-o".as_ref(),
        "/dev/null".as_ref(),
        "-w".as_ref(),
        "%{url_effective}".as_ref(),
        latest.as_ref(),
    ])?;
    tag_from_release_url(&effective)
        .ok_or_else(|| UpdateError::new(format!("unexpected release page: {}", effective.trim())))
}

/// The newest release tag when it is newer than the running version.
pub fn check() -> Result<Option<String>, UpdateError> {
    let tag = latest_release_tag()?;
    let latest = parse_version(&tag)
        .ok_or_else(|| UpdateError::new(format!("unexpected release tag {tag}")))?;
    Ok((latest > current_version()).then_some(tag))
}

fn download(tag: &str, file: &str, directory: &Path) -> Result<PathBuf, UpdateError> {
    let path = directory.join(file);
    let url = download_url(tag, file);
    curl(&[
        "--max-time".as_ref(),
        DOWNLOAD_TIMEOUT_SECONDS.as_ref(),
        "-o".as_ref(),
        path.as_os_str(),
        url.as_ref(),
    ])?;
    Ok(path)
}

fn sha256(path: &Path) -> Result<String, UpdateError> {
    let output = if cfg!(target_os = "macos") {
        run("shasum", &["-a".as_ref(), "256".as_ref(), path.as_os_str()])?
    } else {
        run("sha256sum", &[path.as_os_str()])?
    };
    output
        .split_whitespace()
        .next()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| UpdateError::new("empty checksum output"))
}

/// Download `file` and its `.sha256`, failing unless the digests match.
fn download_verified(tag: &str, file: &str, directory: &Path) -> Result<PathBuf, UpdateError> {
    let checksum_path = download(tag, &format!("{file}.sha256"), directory)?;
    let expected = parse_checksum(&std::fs::read_to_string(checksum_path)?, file)
        .ok_or_else(|| UpdateError::new(format!("no checksum for {file}")))?;
    let path = download(tag, file, directory)?;
    let actual = sha256(&path)?;
    if actual != expected {
        return Err(UpdateError::new(format!(
            "checksum mismatch for {file}: expected {expected}, got {actual}"
        )));
    }
    Ok(path)
}

/// Removes the download directory however the update ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self, UpdateError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!("kokuban-update-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn not_writable(directory: &Path, error: std::io::Error) -> UpdateError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        UpdateError::new(format!(
            "cannot write to {}; run `sudo kokuban --update`",
            directory.display()
        ))
    } else {
        UpdateError::new(format!("cannot write to {}: {error}", directory.display()))
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.{suffix}"))
}

/// Replace the running executable with the one from the release archive.
fn replace_standalone(tag: &str, executable: &Path) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt;

    let directory = executable
        .parent()
        .ok_or_else(|| UpdateError::new("executable has no parent directory"))?;
    let staged = sibling(executable, "update");
    // Fail on permissions before downloading anything.
    std::fs::File::create(&staged).map_err(|error| not_writable(directory, error))?;

    let result = (|| {
        let temporary = TempDir::new()?;
        let stem = release_stem(tag);
        let archive = download_verified(tag, &format!("{stem}.tar.gz"), &temporary.0)?;
        run("tar", &["-xzf".as_ref(), archive.as_os_str(), "-C".as_ref(), temporary.0.as_os_str()])?;
        let extracted = temporary.0.join(&stem).join("kokuban");
        if !extracted.is_file() {
            return Err(UpdateError::new(format!("{stem}.tar.gz has no kokuban executable")));
        }
        std::fs::copy(&extracted, &staged)?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        // Renaming over the path keeps the running process on the old inode.
        std::fs::rename(&staged, executable).map_err(|error| not_writable(directory, error))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// Replace `Kokuban.app` with the bundle from the release disk image.
#[cfg(target_os = "macos")]
fn replace_app_bundle(tag: &str, bundle: &Path) -> Result<(), UpdateError> {
    let directory = bundle
        .parent()
        .ok_or_else(|| UpdateError::new("app bundle has no parent directory"))?;
    let staged = sibling(bundle, "update");
    let previous = sibling(bundle, "previous");
    for leftover in [&staged, &previous] {
        if leftover.exists() {
            std::fs::remove_dir_all(leftover).map_err(|error| not_writable(directory, error))?;
        }
    }
    std::fs::create_dir(&staged).map_err(|error| not_writable(directory, error))?;
    std::fs::remove_dir(&staged)?;

    let temporary = TempDir::new()?;
    let image = download_verified(tag, &format!("{}.dmg", release_stem(tag)), &temporary.0)?;
    let mount_point = temporary.0.join("mount");
    std::fs::create_dir(&mount_point)?;
    run(
        "hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-mountpoint".as_ref(),
            mount_point.as_os_str(),
            image.as_os_str(),
        ],
    )?;
    let copied = (|| {
        let source = mount_point.join("Kokuban.app");
        if !source.join("Contents/MacOS/kokuban").is_file() {
            return Err(UpdateError::new("disk image has no Kokuban.app"));
        }
        run("ditto", &[source.as_os_str(), staged.as_os_str()]).map(drop)
    })();
    let _ = run("hdiutil", &["detach".as_ref(), "-quiet".as_ref(), mount_point.as_os_str()]);
    if let Err(error) = copied {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(error);
    }

    std::fs::rename(bundle, &previous).map_err(|error| not_writable(directory, error))?;
    if let Err(error) = std::fs::rename(&staged, bundle) {
        let _ = std::fs::rename(&previous, bundle);
        let _ = std::fs::remove_dir_all(&staged);
        return Err(not_writable(directory, error));
    }
    let _ = std::fs::remove_dir_all(&previous);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn replace_app_bundle(_tag: &str, _bundle: &Path) -> Result<(), UpdateError> {
    Err(UpdateError::new("app bundles are only updated on macOS"))
}

/// Install release `tag` over the running installation and return its path.
pub fn install(tag: &str) -> Result<PathBuf, UpdateError> {
    if tag_from_release_url(&format!("/releases/tag/{tag}")).as_deref() != Some(tag) {
        return Err(UpdateError::new(format!("invalid release tag {tag:?}")));
    }
    if release_target() == "unsupported" {
        return Err(UpdateError::new("no release is published for this platform"));
    }
    let executable = std::env::current_exe()?.canonicalize()?;
    match install_kind(&executable) {
        InstallKind::AppBundle(bundle) => replace_app_bundle(tag, &bundle).map(|()| bundle),
        InstallKind::Standalone(path) => replace_standalone(tag, &path).map(|()| path),
    }
}

/// Update progress shown by the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    Available(String),
    Installing(String),
    Installed(String),
    Failed(String),
}

impl UpdateStatus {
    /// Short notice for the status bar or window title.
    pub fn notice(&self) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Checking => Some("checking for updates…".to_string()),
            Self::UpToDate => Some(format!("v{} is up to date", current_version())),
            Self::Available(tag) => Some(format!("{tag} available")),
            Self::Installing(tag) => Some(format!("installing {tag}…")),
            Self::Installed(tag) => Some(format!("{tag} installed · reopen kokuban")),
            Self::Failed(_) => Some("update failed".to_string()),
        }
    }

    pub fn available_tag(&self) -> Option<&str> {
        match self {
            Self::Available(tag) => Some(tag),
            _ => None,
        }
    }
}

pub type SharedUpdateStatus = Arc<Mutex<UpdateStatus>>;

fn set_status(status: &SharedUpdateStatus, value: UpdateStatus) {
    *status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
}

pub fn read_status(status: &SharedUpdateStatus) -> UpdateStatus {
    status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

/// Whether the automatic check at launch should run.
pub fn startup_check_enabled(configured: bool) -> bool {
    configured
        && std::env::var_os("KOKUBAN_NO_UPDATE_CHECK").is_none_or(|value| value.is_empty() || value == "0")
        // Smoke tests and benchmarks exit after one frame and must stay offline.
        && std::env::var_os("KOKUBAN_EXIT_AFTER_FIRST_FRAME").is_none()
}

/// Check in the background; `manual` also reports "up to date" and failures.
pub fn spawn_check(status: SharedUpdateStatus, manual: bool, on_change: impl Fn() + Send + 'static) {
    if manual {
        set_status(&status, UpdateStatus::Checking);
        on_change();
    }
    let spawned = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            let next = match check() {
                Ok(Some(tag)) => UpdateStatus::Available(tag),
                Ok(None) if manual => UpdateStatus::UpToDate,
                Err(error) if manual => UpdateStatus::Failed(error.to_string()),
                Ok(None) => return,
                Err(error) => {
                    log::info!("Update check failed: {error}");
                    return;
                }
            };
            set_status(&status, next);
            on_change();
        });
    if let Err(error) = spawned {
        log::warn!("Could not start the update check: {error}");
    }
}

/// Install `tag` in the background, reporting progress through `status`.
pub fn spawn_install(status: SharedUpdateStatus, tag: String, on_change: impl Fn() + Send + 'static) {
    set_status(&status, UpdateStatus::Installing(tag.clone()));
    on_change();
    let spawned = std::thread::Builder::new()
        .name("update-install".into())
        .spawn(move || {
            let next = match install(&tag) {
                Ok(path) => {
                    log::info!("Installed {tag} at {}", path.display());
                    UpdateStatus::Installed(tag)
                }
                Err(error) => {
                    log::warn!("Update to {tag} failed: {error}");
                    UpdateStatus::Failed(error.to_string())
                }
            };
            set_status(&status, next);
            on_change();
        });
    if let Err(error) = spawned {
        log::warn!("Could not start the update: {error}");
    }
}

/// `kokuban --check-update`
pub fn run_check_command() -> ExitCode {
    match check() {
        Ok(Some(tag)) => {
            println!("kokuban {tag} is available (running {}); run `kokuban --update`", current_version());
            ExitCode::SUCCESS
        }
        Ok(None) => {
            println!("kokuban {} is up to date", current_version());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("kokuban: could not check for updates: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `kokuban --update`
pub fn run_update_command() -> ExitCode {
    let tag = match check() {
        Ok(Some(tag)) => tag,
        Ok(None) => {
            println!("kokuban {} is up to date", current_version());
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("kokuban: could not check for updates: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("Downloading kokuban {tag} for {}…", release_target());
    match install(&tag) {
        Ok(path) => {
            println!("Installed kokuban {tag} at {}. Reopen kokuban to use it.", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("kokuban: update to {tag} failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_tags_and_cargo_versions() {
        let v = |major, minor, patch| Some(Version { major, minor, patch });
        assert_eq!(parse_version("v0.4"), v(0, 4, 0));
        assert_eq!(parse_version("0.4.0"), v(0, 4, 0));
        assert_eq!(parse_version("v1.10.3"), v(1, 10, 3));
        for invalid in ["", "v", "v1", "v0.4-rc1", "v0.4.0+build", "v0.4.0.1", "v0..4", "va.b"] {
            assert_eq!(parse_version(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn orders_versions_numerically() {
        let version = |text| parse_version(text).unwrap();
        assert!(version("v0.4") > version("0.3.0"));
        assert!(version("v0.10") > version("v0.9.9"));
        assert!(version("v1.0") > version("v0.99"));
        assert_eq!(version("v0.4"), version("0.4.0"));
    }

    #[test]
    fn reads_tag_from_latest_release_redirect() {
        assert_eq!(
            tag_from_release_url("https://github.com/guicybercode/kokuban.rs/releases/tag/v0.4\n").as_deref(),
            Some("v0.4")
        );
        for invalid in [
            "https://github.com/guicybercode/kokuban.rs/releases",
            "https://github.com/guicybercode/kokuban.rs/releases/tag/",
            "https://github.com/guicybercode/kokuban.rs/releases/tag/v0.4/../x",
            "https://github.com/guicybercode/kokuban.rs/releases/tag/v0.4?x=1",
        ] {
            assert_eq!(tag_from_release_url(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn release_files_match_the_packaging_scripts() {
        let stem = release_stem("v0.4");
        assert!(stem.starts_with("kokuban-v0.4-"));
        assert!(stem.ends_with(release_target()));
        assert_ne!(release_target(), "unsupported");
        assert_eq!(
            download_url("v0.4", "a.tar.gz"),
            "https://github.com/guicybercode/kokuban.rs/releases/download/v0.4/a.tar.gz"
        );
    }

    #[test]
    fn reads_only_the_matching_sha256_line() {
        let digest = "A".repeat(64);
        let text = format!("{}  other.tar.gz\n{digest}  kokuban.tar.gz\n", "b".repeat(64));
        assert_eq!(parse_checksum(&text, "kokuban.tar.gz"), Some("a".repeat(64)));
        assert_eq!(parse_checksum(&format!("{digest} *kokuban.dmg"), "kokuban.dmg"), Some("a".repeat(64)));
        assert_eq!(parse_checksum("abc  kokuban.tar.gz", "kokuban.tar.gz"), None);
        assert_eq!(parse_checksum(&text, "missing.tar.gz"), None);
    }

    #[test]
    fn recognizes_app_bundles_and_standalone_executables() {
        assert_eq!(
            install_kind(Path::new("/Applications/Kokuban.app/Contents/MacOS/kokuban")),
            InstallKind::AppBundle("/Applications/Kokuban.app".into())
        );
        for standalone in ["/usr/local/bin/kokuban", "/tmp/Contents/MacOS/kokuban", "kokuban"] {
            assert_eq!(
                install_kind(Path::new(standalone)),
                InstallKind::Standalone(standalone.into())
            );
        }
    }

    #[test]
    fn notices_only_what_the_window_should_show() {
        assert_eq!(UpdateStatus::Idle.notice(), None);
        assert_eq!(UpdateStatus::Available("v0.5".into()).notice().as_deref(), Some("v0.5 available"));
        assert_eq!(UpdateStatus::Available("v0.5".into()).available_tag(), Some("v0.5"));
        assert_eq!(UpdateStatus::Installed("v0.5".into()).available_tag(), None);
    }

    #[test]
    fn startup_check_respects_configuration() {
        assert!(!startup_check_enabled(false));
    }
}
