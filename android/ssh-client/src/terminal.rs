use anyhow::{bail, Context, Result};
use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use tokio::io::unix::AsyncFd;
use zeroize::Zeroizing;

fn duplicate(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: fcntl duplicates a borrowed descriptor; the returned descriptor
    // has unique ownership and is closed by OwnedFd.
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

/// Restore all inherited file status flags together. stdin/stdout/stderr can
/// share an open file description, so capture every original before changing
/// any descriptor. Independent per-fd guards would restore in the wrong order.
pub struct StdioFlags(Vec<(OwnedFd, libc::c_int)>);

impl StdioFlags {
    pub fn nonblocking() -> io::Result<Self> {
        let mut saved = Self(Vec::new());
        for fd in [0, 1, 2] {
            let owned = duplicate(fd)?;
            // SAFETY: owned is a valid, live descriptor.
            let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            saved.0.push((owned, flags));
        }
        for (fd, flags) in &saved.0 {
            // SAFETY: fd is owned until the guard is dropped.
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, *flags | libc::O_NONBLOCK) } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(saved)
    }
}

impl Drop for StdioFlags {
    fn drop(&mut self) {
        for (fd, flags) in &self.0 {
            // SAFETY: the saved descriptor remains live during Drop.
            unsafe {
                libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, *flags);
            }
        }
    }
}

pub enum Descriptor {
    Pollable(AsyncFd<OwnedFd>),
    // Regular files and /dev/null cannot be registered with epoll. Their I/O
    // completes directly, so no uncancellable stdin thread is needed.
    Direct(OwnedFd),
}

impl Descriptor {
    pub fn from_fd(fd: RawFd) -> io::Result<Self> {
        let owned = duplicate(fd)?;
        match AsyncFd::new(duplicate(fd)?) {
            Ok(fd) => Ok(Self::Pollable(fd)),
            Err(error) if matches!(error.raw_os_error(), Some(libc::EPERM | libc::EINVAL)) => {
                Ok(Self::Direct(owned))
            }
            Err(error) => Err(error),
        }
    }

    pub async fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            let read = match self {
                Self::Pollable(fd) => {
                    let mut ready = fd.readable().await?;
                    match ready.try_io(|fd| read_fd(fd.get_ref().as_raw_fd(), buffer)) {
                        Ok(result) => result,
                        Err(_) => continue,
                    }
                }
                Self::Direct(fd) => read_fd(fd.as_raw_fd(), buffer),
            };
            match read {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    // Some Unix kernels cannot register the /dev/tty alias.
                    // Keep this fallback cancellable; ordinary PTYs use AsyncFd.
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                other => return other,
            }
        }
    }

    pub async fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let written = match self {
                Self::Pollable(fd) => {
                    let mut ready = fd.writable().await?;
                    match ready.try_io(|fd| write_fd(fd.get_ref().as_raw_fd(), bytes)) {
                        Ok(result) => result,
                        Err(_) => continue,
                    }
                }
                Self::Direct(fd) => write_fd(fd.as_raw_fd(), bytes),
            };
            match written {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

fn read_fd(fd: RawFd, buffer: &mut [u8]) -> io::Result<usize> {
    // SAFETY: buffer is writable for its length; fd is borrowed for the syscall.
    let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if count < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(count as usize)
    }
}

fn write_fd(fd: RawFd, bytes: &[u8]) -> io::Result<usize> {
    // SAFETY: bytes is readable for its length; fd is borrowed for the syscall.
    let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    if count < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(count as usize)
    }
}

pub fn is_terminal(fd: RawFd) -> bool {
    // SAFETY: isatty only queries the descriptor and handles invalid fds itself.
    unsafe { libc::isatty(fd) == 1 }
}

pub struct ModeGuard {
    fd: OwnedFd,
    original: libc::termios,
}

impl ModeGuard {
    fn change(fd: RawFd, change: impl FnOnce(&mut libc::termios)) -> io::Result<Self> {
        let fd = duplicate(fd)?;
        let mut original = std::mem::MaybeUninit::uninit();
        // SAFETY: tcgetattr writes a complete termios on success.
        if unsafe { libc::tcgetattr(fd.as_raw_fd(), original.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let original = unsafe { original.assume_init() };
        let mut current = original;
        change(&mut current);
        // SAFETY: current is a fully initialized termios for this descriptor.
        if unsafe { libc::tcsetattr(fd.as_raw_fd(), libc::TCSANOW, &current) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, original })
    }

    pub fn raw(fd: RawFd) -> io::Result<Self> {
        Self::change(fd, |modes| {
            // SAFETY: modes is a valid termios value owned by the guard.
            unsafe {
                libc::cfmakeraw(modes);
            }
        })
    }
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        // SAFETY: self owns the descriptor and the original termios value.
        unsafe {
            libc::tcsetattr(self.fd.as_raw_fd(), libc::TCSANOW, &self.original);
        }
    }
}

pub async fn prompt(message: &str, echo: bool) -> Result<Zeroizing<String>> {
    // Always use the controlling terminal, never piped command input. There is
    // deliberately no environment-variable or command-line password option.
    let mut device = "/dev/tty".to_owned();
    for fd in [0, 1, 2] {
        let mut name = [0u8; 1024];
        // SAFETY: successful ttyname_r writes a NUL-terminated name within the
        // supplied buffer. Prefer the actual device over the /dev/tty alias.
        if unsafe { libc::ttyname_r(fd, name.as_mut_ptr().cast(), name.len()) } == 0 {
            let end = name
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(name.len());
            if let Ok(path) = std::str::from_utf8(&name[..end]) {
                device = path.to_owned();
                break;
            }
        }
    }
    let tty = OpenOptions::new().read(true).write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC).open(device)
        .context("authentication/trust requires a controlling terminal; use a verified known_hosts and key in --batch mode")?;
    let io = Descriptor::from_fd(tty.as_raw_fd())?;
    let _mode = ModeGuard::change(tty.as_raw_fd(), |mode| {
        mode.c_lflag |= libc::ICANON | libc::ISIG;
        mode.c_lflag &= !(libc::ECHO | libc::ECHONL);
        if echo {
            mode.c_lflag |= libc::ECHO;
        }
    })?;
    io.write_all(message.as_bytes()).await?;
    let mut answer = Zeroizing::new(Vec::new());
    let mut buffer = Zeroizing::new([0u8; 256]);
    loop {
        let count = io.read(&mut buffer[..]).await?;
        if count == 0 {
            bail!("prompt cancelled")
        }
        if let Some(end) = buffer[..count]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
        {
            answer.extend_from_slice(&buffer[..end]);
            if answer.len() > 4096 {
                bail!("prompt response exceeds 4096 bytes")
            }
            break;
        }
        answer.extend_from_slice(&buffer[..count]);
        if answer.len() > 4096 {
            bail!("prompt response exceeds 4096 bytes")
        }
    }
    if !echo {
        io.write_all(b"\r\n").await?;
    }
    let answer = std::str::from_utf8(&answer).context("response is not valid UTF-8")?;
    Ok(Zeroizing::new(answer.to_owned()))
}

pub fn dimensions(fd: RawFd) -> (u32, u32, u32, u32) {
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: size points to space for the complete winsize response.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, size.as_mut_ptr()) } == 0 {
        let size = unsafe { size.assume_init() };
        if size.ws_col > 0 && size.ws_row > 0 {
            return (
                u32::from(size.ws_col),
                u32::from(size.ws_row),
                u32::from(size.ws_xpixel),
                u32::from(size.ws_ypixel),
            );
        }
    }
    (80, 24, 0, 0)
}

pub fn clean_message(message: &str) -> String {
    message
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .take(8192)
        .collect()
}

#[derive(Default)]
pub struct EscapeFilter {
    line_start: bool,
    pending_tilde: bool,
}

impl EscapeFilter {
    pub fn new() -> Self {
        Self {
            line_start: true,
            pending_tilde: false,
        }
    }

    pub fn apply(&mut self, input: &[u8]) -> (Vec<u8>, bool) {
        let mut output = Vec::with_capacity(input.len() + 1);
        for &byte in input {
            if self.pending_tilde {
                self.pending_tilde = false;
                if byte == b'.' {
                    return (output, true);
                }
                output.push(b'~');
                self.line_start = false;
                if byte == b'~' {
                    continue;
                }
            } else if self.line_start && byte == b'~' {
                self.pending_tilde = true;
                continue;
            }
            output.push(byte);
            self.line_start = matches!(byte, b'\r' | b'\n');
        }
        (output, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_disconnect_handles_split_reads_without_corrupting_literal_tildes() {
        let mut filter = EscapeFilter::new();
        assert_eq!(filter.apply(b"echo ~.\r~"), (b"echo ~.\r".to_vec(), false));
        assert_eq!(filter.apply(b"."), (Vec::new(), true));
        let mut filter = EscapeFilter::new();
        assert_eq!(
            filter.apply(b"~~hello\r~x"),
            (b"~hello\r~x".to_vec(), false)
        );
    }

    #[tokio::test]
    async fn nonpollable_eof_returns_without_an_uncancellable_stdin_thread() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let input = Descriptor::from_fd(file.as_raw_fd()).unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            input.read(&mut [0; 16]),
        )
        .await;
        assert_eq!(result.unwrap().unwrap(), 0);
    }

    #[test]
    fn authentication_messages_cannot_inject_terminal_control_sequences() {
        assert_eq!(clean_message("hello\x1b[31m\x07\n"), "hello[31m\n");
    }
}
