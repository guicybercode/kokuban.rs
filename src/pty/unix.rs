use super::PtyError;
use nix::libc;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::pty::openpty;
use nix::sys::signal::{kill, killpg, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{fork, getpgrp, pipe, tcgetpgrp, ForkResult, Pid};
use std::ffi::{CString, OsString};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant};

const WRITE_RETRY_INTERVAL: Duration = Duration::from_millis(1);
const CHILD_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CancellableWriteOutcome {
    Completed,
    /// Shutdown was observed; an earlier partial write may already be visible.
    Cancelled,
}

pub struct Pty {
    master: Option<OwnedFd>,
    pub child_pid: nix::unistd::Pid,
    output_lock: Mutex<()>,
}

impl Pty {
    pub fn spawn(
        cols: u16,
        rows: u16,
        kitty_graphics: bool,
        sixel_graphics: bool,
    ) -> Result<Self, PtyError> {
        let shell_path = resolve_shell();
        let shell = CString::new(shell_path.as_os_str().as_bytes())
            .map_err(|_| PtyError::InvalidShell("shell path contains NUL".to_string()))?;
        let shell_name = shell_path
            .file_name()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| shell_path.as_os_str());
        let mut login_name = Vec::with_capacity(shell_name.as_bytes().len() + 1);
        login_name.push(b'-');
        login_name.extend_from_slice(shell_name.as_bytes());
        let argv = vec![CString::new(login_name)
            .map_err(|_| PtyError::InvalidShell("shell name contains NUL".to_string()))?];
        let environment = child_environment(&shell_path, kitty_graphics, sixel_graphics)?;

        Self::spawn_prepared(cols, rows, shell, argv, environment)
    }

    fn spawn_prepared(
        cols: u16,
        rows: u16,
        program: CString,
        argv: Vec<CString>,
        environment: Vec<CString>,
    ) -> Result<Self, PtyError> {
        if argv.is_empty() {
            return Err(PtyError::InvalidShell(
                "program argument list is empty".to_string(),
            ));
        }

        let win_size = nix::pty::Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        let pty = openpty(Some(&win_size), None)?;
        let master = prepare_child_fd(pty.master)?;
        let slave = prepare_child_fd(pty.slave)?;
        set_nonblocking(master.as_raw_fd())?;

        // A close-on-exec pipe reports setup/exec failures to the parent. On
        // successful exec the kernel closes the child writer and the parent
        // observes EOF before returning a usable PTY.
        let (error_reader, error_writer) = pipe()?;
        let error_reader = prepare_child_fd(error_reader)?;
        let error_writer = prepare_child_fd(error_writer)?;
        set_nonblocking(error_reader.as_raw_fd())?;

        // Everything the child needs is allocated before fork. New panes can
        // be created after renderer and PTY threads exist, so the child must
        // only call async-signal-safe libc functions until execve.
        let mut argv_ptrs: Vec<*const libc::c_char> =
            argv.iter().map(|argument| argument.as_ptr()).collect();
        argv_ptrs.push(std::ptr::null());
        let mut environment_ptrs: Vec<*const libc::c_char> =
            environment.iter().map(|entry| entry.as_ptr()).collect();
        environment_ptrs.push(std::ptr::null());

        match unsafe { fork() } {
            Ok(ForkResult::Child) => unsafe {
                exec_child(
                    master.as_raw_fd(),
                    slave.as_raw_fd(),
                    error_reader.as_raw_fd(),
                    error_writer.as_raw_fd(),
                    &program,
                    &argv_ptrs,
                    &environment_ptrs,
                );
            },
            Ok(ForkResult::Parent { child }) => {
                drop(slave);
                drop(error_writer);

                match child_startup_status(&error_reader) {
                    Ok(None) => {}
                    Ok(Some(stage)) => {
                        drop(master);
                        reap_child(child);
                        return Err(PtyError::ChildSetup(child_stage_name(stage)));
                    }
                    Err(error) => {
                        drop(master);
                        terminate_and_reap(child, None);
                        return Err(error);
                    }
                }
                drop(error_reader);

                log::info!(
                    "PTY spawned: child pid={child}, master fd={}",
                    master.as_raw_fd()
                );

                Ok(Self {
                    master: Some(master),
                    child_pid: child,
                    output_lock: Mutex::new(()),
                })
            }
            Err(e) => Err(PtyError::Fork(e.to_string())),
        }
    }

    pub fn master_fd(&self) -> RawFd {
        self.master().as_raw_fd()
    }

    #[allow(dead_code)]
    pub fn wait_readable(&self, timeout: Duration) -> std::io::Result<bool> {
        let started = Instant::now();
        wait_readable_with(
            timeout,
            || started.elapsed(),
            |poll_timeout| {
                let mut poll_fds = [PollFd::new(self.master().as_fd(), PollFlags::POLLIN)];
                match poll(&mut poll_fds, poll_timeout)? {
                    0 => Ok(ReadPollResult::TimedOut),
                    _ => Ok(ReadPollResult::Events(poll_fds[0].revents())),
                }
            },
        )
    }

    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match nix::unistd::read(self.master().as_raw_fd(), buf) {
                Err(nix::Error::EINTR) => continue,
                Err(nix::Error::EAGAIN) => {
                    return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
                }
                #[cfg(target_os = "linux")]
                Err(nix::Error::EIO) => return Ok(0),
                Err(other) => return Err(other.into()),
                Ok(read) => return Ok(read),
            }
        }
    }

    pub(crate) fn write_all_cancellable(
        &self,
        data: &[u8],
        cancelled: &AtomicBool,
    ) -> std::io::Result<CancellableWriteOutcome> {
        write_all_cancellable_with(
            &self.output_lock,
            data,
            || cancelled.load(Ordering::Acquire),
            |remaining| nix::unistd::write(self.master(), remaining),
            || std::thread::sleep(WRITE_RETRY_INTERVAL),
        )
    }

    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        let win_size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        resize_with_ioctl(
            &win_size,
            |size| unsafe { libc::ioctl(self.master().as_raw_fd(), libc::TIOCSWINSZ, size) },
            std::io::Error::last_os_error,
        )
    }

    fn master(&self) -> &OwnedFd {
        self.master.as_ref().expect("PTY master is available")
    }
}

fn resize_with_ioctl<I, E>(
    win_size: &libc::winsize,
    mut ioctl: I,
    mut last_error: E,
) -> std::io::Result<()>
where
    I: FnMut(&libc::winsize) -> libc::c_int,
    E: FnMut() -> std::io::Error,
{
    loop {
        if ioctl(win_size) >= 0 {
            return Ok(());
        }

        let error = last_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(error);
    }
}

#[cfg(test)]
fn write_all_with<W>(output_lock: &Mutex<()>, data: &[u8], mut write_once: W) -> std::io::Result<()>
where
    W: FnMut(&[u8]) -> nix::Result<usize>,
{
    // Poisoning cannot invalidate the protected unit value. Recovering keeps
    // terminal output usable after an unrelated panic in a writer thread.
    let _guard = match output_lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut written = 0;
    while written < data.len() {
        match write_once(&data[written..]) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
            Ok(n) => written += n,
            Err(nix::Error::EINTR) => continue,
            Err(nix::Error::EAGAIN) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn write_all_cancellable_with<C, W, B>(
    output_lock: &Mutex<()>,
    data: &[u8],
    mut is_cancelled: C,
    mut write_once: W,
    mut backoff: B,
) -> std::io::Result<CancellableWriteOutcome>
where
    C: FnMut() -> bool,
    W: FnMut(&[u8]) -> nix::Result<usize>,
    B: FnMut(),
{
    let _guard = loop {
        if is_cancelled() {
            return Ok(CancellableWriteOutcome::Cancelled);
        }

        match output_lock.try_lock() {
            Ok(guard) => break guard,
            Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => backoff(),
        }
    };

    // Cancellation can race with acquiring the output lock. Re-check while
    // holding it to avoid a write when shutdown is already observable here.
    if is_cancelled() {
        return Ok(CancellableWriteOutcome::Cancelled);
    }

    let mut written = 0;
    while written < data.len() {
        if is_cancelled() {
            return Ok(CancellableWriteOutcome::Cancelled);
        }

        match write_once(&data[written..]) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
            Ok(count) => written += count,
            Err(nix::Error::EINTR) => continue,
            Err(nix::Error::EAGAIN) => backoff(),
            Err(error) => return Err(error.into()),
        }
    }

    Ok(CancellableWriteOutcome::Completed)
}

#[allow(dead_code)]
enum ReadPollResult {
    TimedOut,
    Events(Option<PollFlags>),
}

#[allow(dead_code)]
fn wait_readable_with<P, E>(
    timeout: Duration,
    mut elapsed: E,
    mut poll_once: P,
) -> std::io::Result<bool>
where
    P: FnMut(PollTimeout) -> nix::Result<ReadPollResult>,
    E: FnMut() -> Duration,
{
    let mut attempted = false;
    loop {
        let elapsed = elapsed();
        if attempted && elapsed >= timeout {
            return Ok(false);
        }

        let remaining = timeout.saturating_sub(elapsed);
        attempted = true;
        match poll_once(poll_timeout_for(remaining)) {
            Ok(ReadPollResult::TimedOut) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(ReadPollResult::Events(events)) => {
                classify_readable_events(events)?;
                return Ok(true);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[allow(dead_code)]
fn poll_timeout_for(duration: Duration) -> PollTimeout {
    if duration.is_zero() {
        return PollTimeout::ZERO;
    }

    let rounded_millis = duration.as_nanos().div_ceil(1_000_000);
    PollTimeout::try_from(rounded_millis).unwrap_or(PollTimeout::MAX)
}

#[allow(dead_code)]
fn classify_readable_events(events: Option<PollFlags>) -> std::io::Result<()> {
    let events = events.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "poll returned unknown PTY event flags",
        )
    })?;

    if events.contains(PollFlags::POLLNVAL) {
        return Err(std::io::Error::from_raw_os_error(libc::EBADF));
    }
    if events.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR) {
        return Ok(());
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("poll returned unexpected PTY events: {events:?}"),
    ))
}

impl Drop for Pty {
    fn drop(&mut self) {
        let pid = self.child_pid;
        let foreground_group = self
            .master
            .as_ref()
            .and_then(|master| tcgetpgrp(master).ok());

        // Closing the master triggers the kernel's normal terminal hangup
        // behavior before the bounded cleanup routine escalates signals.
        drop(self.master.take());

        let child_reaped = match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) | Ok(WaitStatus::Stopped(_, _)) => false,
            Err(nix::errno::Errno::EINTR) => false,
            Ok(_) | Err(nix::errno::Errno::ECHILD) => true,
            Err(_) => return,
        };
        if child_reaped && !process_groups_alive(pid, foreground_group) {
            return;
        }

        signal_process_tree(pid, foreground_group, Signal::SIGHUP);
        signal_process_tree(pid, foreground_group, Signal::SIGCONT);

        let reaper = std::thread::Builder::new()
            .name(format!("pty-reaper-{}", pid.as_raw()))
            .spawn(move || terminate_and_reap(pid, foreground_group));
        if reaper.is_err() {
            terminate_and_reap(pid, foreground_group);
        }
    }
}

fn resolve_shell() -> PathBuf {
    select_shell(
        std::env::var_os("KOKUBAN_SHELL"),
        std::env::var_os("SHELL"),
        is_executable_file,
    )
}

fn select_shell<F>(
    override_shell: Option<OsString>,
    user_shell: Option<OsString>,
    mut is_executable: F,
) -> PathBuf
where
    F: FnMut(&Path) -> bool,
{
    [override_shell, user_shell, Some(OsString::from("/bin/sh"))]
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .find(|path| path.is_absolute() && is_executable(path))
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn child_environment(
    shell: &Path,
    kitty_graphics: bool,
    sixel_graphics: bool,
) -> Result<Vec<CString>, PtyError> {
    const OVERRIDDEN: &[&[u8]] = &[
        b"SHELL",
        b"TERM",
        b"COLORTERM",
        b"TERM_PROGRAM",
        b"TERM_PROGRAM_VERSION",
        b"KOKUBAN_VERSION",
        b"KOKUBAN_GRAPHICS",
    ];

    let mut environment = Vec::new();
    for (key, value) in std::env::vars_os() {
        let key = key.as_os_str().as_bytes();
        if OVERRIDDEN.contains(&key) {
            continue;
        }

        let mut entry = Vec::with_capacity(key.len() + value.as_os_str().as_bytes().len() + 1);
        entry.extend_from_slice(key);
        entry.push(b'=');
        entry.extend_from_slice(value.as_os_str().as_bytes());
        if let Ok(entry) = CString::new(entry) {
            environment.push(entry);
        }
    }

    let version = env!("CARGO_PKG_VERSION");
    let shell = shell.as_os_str().as_bytes();
    let graphics = graphics_environment_value(kitty_graphics, sixel_graphics);
    for (key, value) in [
        (b"SHELL".as_slice(), shell),
        (b"TERM".as_slice(), b"xterm-256color".as_slice()),
        (b"COLORTERM".as_slice(), b"truecolor".as_slice()),
        (b"TERM_PROGRAM".as_slice(), b"kokuban".as_slice()),
        (b"TERM_PROGRAM_VERSION".as_slice(), version.as_bytes()),
        (b"KOKUBAN_VERSION".as_slice(), version.as_bytes()),
        (b"KOKUBAN_GRAPHICS".as_slice(), graphics),
    ] {
        let mut entry = Vec::with_capacity(key.len() + value.len() + 1);
        entry.extend_from_slice(key);
        entry.push(b'=');
        entry.extend_from_slice(value);
        environment.push(CString::new(entry).map_err(|_| {
            PtyError::InvalidShell("terminal environment contains NUL".to_string())
        })?);
    }

    Ok(environment)
}

fn graphics_environment_value(kitty: bool, sixel: bool) -> &'static [u8] {
    match (kitty, sixel) {
        (false, false) => b"",
        (true, false) => b"kitty",
        (false, true) => b"sixel",
        (true, true) => b"kitty,sixel",
    }
}

fn set_cloexec(fd: RawFd) -> Result<(), PtyError> {
    let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD)?;
    let mut flags = nix::fcntl::FdFlag::from_bits_truncate(flags);
    flags.insert(nix::fcntl::FdFlag::FD_CLOEXEC);
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFD(flags))?;
    Ok(())
}

fn set_nonblocking(fd: RawFd) -> Result<(), PtyError> {
    let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL)?;
    let mut flags = nix::fcntl::OFlag::from_bits_truncate(flags);
    flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(flags))?;
    Ok(())
}

fn prepare_child_fd(fd: OwnedFd) -> Result<OwnedFd, PtyError> {
    if fd.as_raw_fd() > 2 {
        set_cloexec(fd.as_raw_fd())?;
        return Ok(fd);
    }

    let duplicated = nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_DUPFD_CLOEXEC(3))?;
    drop(fd);
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

fn child_startup_status(error_reader: &OwnedFd) -> Result<Option<u8>, PtyError> {
    let started = Instant::now();
    let mut poll_fds = [PollFd::new(
        error_reader.as_fd(),
        PollFlags::POLLIN | PollFlags::POLLHUP,
    )];

    child_startup_status_with(
        CHILD_STARTUP_TIMEOUT,
        || started.elapsed(),
        |timeout| match poll(&mut poll_fds, timeout)? {
            0 => Ok(ReadPollResult::TimedOut),
            _ => Ok(ReadPollResult::Events(poll_fds[0].revents())),
        },
        |stage| nix::unistd::read(error_reader.as_raw_fd(), stage),
    )
}

fn child_startup_status_with<E, P, R>(
    timeout: Duration,
    mut elapsed: E,
    mut poll_once: P,
    mut read_once: R,
) -> Result<Option<u8>, PtyError>
where
    E: FnMut() -> Duration,
    P: FnMut(PollTimeout) -> nix::Result<ReadPollResult>,
    R: FnMut(&mut [u8]) -> nix::Result<usize>,
{
    let mut stage = [0u8; 1];
    loop {
        let elapsed = elapsed();
        if elapsed >= timeout {
            return Err(PtyError::ChildStartupTimeout);
        }

        let remaining = timeout.saturating_sub(elapsed);
        match poll_once(poll_timeout_for(remaining)) {
            Ok(ReadPollResult::TimedOut) => continue,
            Ok(ReadPollResult::Events(events)) => {
                classify_readable_events(events)?;
                match read_once(&mut stage) {
                    Ok(0) => return Ok(None),
                    Ok(_) => return Ok(Some(stage[0])),
                    Err(nix::errno::Errno::EINTR | nix::errno::Errno::EAGAIN) => continue,
                    Err(error) => return Err(PtyError::OpenPty(error)),
                }
            }
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => return Err(PtyError::OpenPty(error)),
        }
    }
}

fn child_stage_name(stage: u8) -> &'static str {
    match stage {
        1 => "setsid",
        2 => "controlling terminal setup",
        3 => "standard stream setup",
        4 => "exec",
        _ => "unknown child setup stage",
    }
}

fn reap_child(pid: Pid) {
    while let Err(nix::errno::Errno::EINTR) = waitpid(pid, None) {}
}

fn terminate_and_reap(pid: Pid, foreground_group: Option<Pid>) {
    signal_process_tree(pid, foreground_group, Signal::SIGHUP);
    signal_process_tree(pid, foreground_group, Signal::SIGCONT);
    if wait_for_cleanup(pid, foreground_group, Duration::from_millis(250)) {
        return;
    }

    signal_process_tree(pid, foreground_group, Signal::SIGTERM);
    signal_process_tree(pid, foreground_group, Signal::SIGCONT);
    if wait_for_cleanup(pid, foreground_group, Duration::from_millis(500)) {
        return;
    }

    signal_process_tree(pid, foreground_group, Signal::SIGKILL);
    reap_child(pid);
}

fn signal_process_tree(pid: Pid, foreground_group: Option<Pid>, signal: Signal) {
    let own_group = getpgrp();
    let _ = kill(pid, signal);
    for group in [Some(pid), foreground_group].into_iter().flatten() {
        if group.as_raw() > 0 && group != own_group {
            let _ = killpg(group, signal);
        }
    }
}

fn wait_for_cleanup(pid: Pid, foreground_group: Option<Pid>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut child_reaped = false;
    loop {
        if !child_reaped {
            child_reaped = match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) | Ok(WaitStatus::Stopped(_, _)) => false,
                Err(nix::errno::Errno::EINTR) => false,
                Ok(_) | Err(nix::errno::Errno::ECHILD) => true,
                Err(_) => true,
            };
        }

        if child_reaped && !process_groups_alive(pid, foreground_group) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn process_group_exists(group: Pid) -> bool {
    let result = unsafe { libc::kill(-group.as_raw(), 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn process_groups_alive(pid: Pid, foreground_group: Option<Pid>) -> bool {
    let own_group = getpgrp();
    [Some(pid), foreground_group]
        .into_iter()
        .flatten()
        .filter(|group| *group != own_group)
        .any(process_group_exists)
}

unsafe fn exec_child(
    master_fd: RawFd,
    slave_fd: RawFd,
    error_reader_fd: RawFd,
    error_writer_fd: RawFd,
    program: &CString,
    argv: &[*const libc::c_char],
    environment: &[*const libc::c_char],
) -> ! {
    unsafe {
        libc::close(master_fd);
        libc::close(error_reader_fd);
        if libc::setsid() < 0 {
            child_setup_failed(error_writer_fd, 1);
        }
        if libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) < 0 {
            child_setup_failed(error_writer_fd, 2);
        }

        for target in 0..=2 {
            if libc::dup2(slave_fd, target) < 0 {
                child_setup_failed(error_writer_fd, 3);
            }
        }
        libc::close(slave_fd);

        libc::execve(program.as_ptr(), argv.as_ptr(), environment.as_ptr());
        child_setup_failed(error_writer_fd, 4);
    }
}

unsafe fn child_setup_failed(error_writer_fd: RawFd, stage: u8) -> ! {
    unsafe {
        let stage = [stage];
        for _ in 0..8 {
            if libc::write(error_writer_fd, stage.as_ptr().cast(), stage.len()) == 1 {
                break;
            }
        }
        libc::_exit(126);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        child_environment, child_startup_status_with, classify_readable_events, poll_timeout_for,
        resize_with_ioctl, select_shell, set_cloexec, wait_readable_with,
        write_all_cancellable_with, write_all_with, CancellableWriteOutcome, Pty, ReadPollResult,
    };
    use crate::pty::PtyError;
    use nix::fcntl::{fcntl, FcntlArg, FdFlag};
    use nix::libc;
    use nix::poll::{PollFlags, PollTimeout};
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
    use std::ffi::{CString, OsString};
    use std::os::fd::AsRawFd;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Barrier, Mutex, TryLockError};
    use std::thread;
    use std::time::{Duration, Instant};

    fn test_environment() -> Vec<CString> {
        [
            "PATH=/usr/bin:/bin",
            "TERM=xterm-256color",
            "TERM_PROGRAM=kokuban",
        ]
        .into_iter()
        .map(|entry| CString::new(entry).unwrap())
        .collect()
    }

    fn read_until(pty: &Pty, marker: &[u8]) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut buffer = [0; 256];

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !pty
                .wait_readable(remaining)
                .expect("PTY readiness check should succeed")
            {
                break;
            }
            match pty.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    output.extend_from_slice(&buffer[..read]);
                    if output.windows(marker.len()).any(|window| window == marker) {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(error) => panic!("PTY read failed: {error}"),
            }
        }
        output
    }

    fn process_exists(pid: nix::unistd::Pid) -> bool {
        let result = unsafe { libc::kill(pid.as_raw(), 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn pty_can_be_shared_with_a_reader_thread() {
        assert_send_sync::<Pty>();
    }

    #[test]
    fn resize_success_uses_one_ioctl_without_reading_errno() {
        let win_size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let mut ioctl_calls = 0;

        resize_with_ioctl(
            &win_size,
            |_| {
                ioctl_calls += 1;
                0
            },
            || panic!("a successful resize must not read errno"),
        )
        .expect("the first resize ioctl should succeed");

        assert_eq!(ioctl_calls, 1);
    }

    #[test]
    fn resize_retries_only_interrupted_ioctls_with_the_same_window_size() {
        let win_size = libc::winsize {
            ws_row: 43,
            ws_col: 137,
            ws_xpixel: 1920,
            ws_ypixel: 1080,
        };
        let expected = (43, 137, 1920, 1080);
        let mut ioctl_results = [-1, -1, 0].into_iter();
        let mut errors = [libc::EINTR, libc::EINTR].into_iter();
        let mut observed_sizes = Vec::new();
        let mut error_lookups = 0;

        resize_with_ioctl(
            &win_size,
            |actual| {
                observed_sizes.push((
                    actual.ws_row,
                    actual.ws_col,
                    actual.ws_xpixel,
                    actual.ws_ypixel,
                ));
                ioctl_results
                    .next()
                    .expect("scripted resize ioctl should have a result")
            },
            || {
                error_lookups += 1;
                std::io::Error::from_raw_os_error(
                    errors
                        .next()
                        .expect("each failed ioctl should have an errno"),
                )
            },
        )
        .expect("EINTR retries should eventually succeed");

        assert_eq!(observed_sizes, [expected, expected, expected]);
        assert_eq!(error_lookups, 2);
    }

    #[test]
    fn resize_preserves_non_interrupted_errno_without_retrying() {
        let win_size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let mut ioctl_calls = 0;
        let mut error_lookups = 0;

        let error = resize_with_ioctl(
            &win_size,
            |_| {
                ioctl_calls += 1;
                -1
            },
            || {
                error_lookups += 1;
                std::io::Error::from_raw_os_error(libc::EIO)
            },
        )
        .expect_err("non-EINTR resize failures must be returned");

        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert_eq!(ioctl_calls, 1);
        assert_eq!(error_lookups, 1);
    }

    #[test]
    fn resize_returns_an_error_that_follows_an_interruption() {
        let win_size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let mut ioctl_results = [-1, -1].into_iter();
        let mut errors = [libc::EINTR, libc::EIO].into_iter();
        let mut ioctl_calls = 0;
        let mut error_lookups = 0;

        let error = resize_with_ioctl(
            &win_size,
            |_| {
                ioctl_calls += 1;
                ioctl_results
                    .next()
                    .expect("scripted resize ioctl should have a result")
            },
            || {
                error_lookups += 1;
                std::io::Error::from_raw_os_error(
                    errors
                        .next()
                        .expect("each failed ioctl should have an errno"),
                )
            },
        )
        .expect_err("the error following EINTR must be returned");

        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert_eq!(ioctl_calls, 2);
        assert_eq!(error_lookups, 2);
    }

    #[test]
    fn serializes_partial_and_retried_output_as_one_logical_write() {
        let output_lock = Mutex::new(());
        let mut outcomes = [
            Ok(1),
            Err(nix::errno::Errno::EINTR),
            Err(nix::errno::Errno::EAGAIN),
            Ok(2),
        ]
        .into_iter();
        let mut remaining_slices = Vec::new();

        write_all_with(&output_lock, b"abc", |remaining| {
            assert!(matches!(
                output_lock.try_lock(),
                Err(TryLockError::WouldBlock)
            ));
            remaining_slices.push(remaining.to_vec());
            outcomes
                .next()
                .expect("scripted write should have a result")
        })
        .unwrap();

        assert_eq!(remaining_slices, [b"abc".as_slice(), b"bc", b"bc", b"bc"]);
        assert!(output_lock.try_lock().is_ok());
    }

    #[test]
    fn preserves_native_write_errors() {
        let output_lock = Mutex::new(());

        let error = write_all_with(&output_lock, b"x", |_| Err(nix::errno::Errno::EPIPE))
            .expect_err("terminal write should preserve the native error");

        assert_eq!(error.raw_os_error(), Some(libc::EPIPE));
    }

    #[test]
    fn cancellable_write_stops_before_and_while_waiting_for_the_lock() {
        let output_lock = Mutex::new(());
        let held = output_lock.lock().unwrap();
        let cancelled = AtomicBool::new(true);
        let mut write_calls = 0;

        let outcome = write_all_cancellable_with(
            &output_lock,
            b"response",
            || cancelled.load(Ordering::Acquire),
            |_| {
                write_calls += 1;
                Ok(8)
            },
            || panic!("an already-cancelled write must not back off"),
        )
        .unwrap();

        assert_eq!(outcome, CancellableWriteOutcome::Cancelled);
        assert_eq!(write_calls, 0);

        cancelled.store(false, Ordering::Release);
        let backoffs = AtomicUsize::new(0);
        let outcome = write_all_cancellable_with(
            &output_lock,
            b"response",
            || cancelled.load(Ordering::Acquire),
            |_| {
                write_calls += 1;
                Ok(8)
            },
            || {
                backoffs.fetch_add(1, Ordering::Relaxed);
                cancelled.store(true, Ordering::Release);
            },
        )
        .unwrap();

        assert_eq!(outcome, CancellableWriteOutcome::Cancelled);
        assert_eq!(backoffs.load(Ordering::Relaxed), 1);
        assert_eq!(write_calls, 0);
        assert!(matches!(
            output_lock.try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        drop(held);
    }

    #[test]
    fn cancellable_write_rechecks_shutdown_after_acquiring_the_lock() {
        let output_lock = Mutex::new(());
        let cancellation_checks = AtomicUsize::new(0);
        let mut write_calls = 0;

        let outcome = write_all_cancellable_with(
            &output_lock,
            b"response",
            || cancellation_checks.fetch_add(1, Ordering::Relaxed) == 1,
            |_| {
                write_calls += 1;
                Ok(8)
            },
            || panic!("an uncontended lock must not back off"),
        )
        .unwrap();

        assert_eq!(outcome, CancellableWriteOutcome::Cancelled);
        assert_eq!(cancellation_checks.load(Ordering::Relaxed), 2);
        assert_eq!(write_calls, 0);
        assert!(output_lock.try_lock().is_ok());
    }

    #[test]
    fn cancellable_write_stops_after_partial_output_and_would_block() {
        let output_lock = Mutex::new(());
        let cancelled = AtomicBool::new(false);
        let mut outcomes = [Ok(1), Err(nix::errno::Errno::EAGAIN)].into_iter();
        let mut remaining_slices = Vec::new();
        let mut backoffs = 0;

        let outcome = write_all_cancellable_with(
            &output_lock,
            b"abc",
            || cancelled.load(Ordering::Acquire),
            |remaining| {
                assert!(matches!(
                    output_lock.try_lock(),
                    Err(TryLockError::WouldBlock)
                ));
                remaining_slices.push(remaining.to_vec());
                outcomes
                    .next()
                    .expect("cancelled output must not be retried")
            },
            || {
                backoffs += 1;
                cancelled.store(true, Ordering::Release);
            },
        )
        .unwrap();

        assert_eq!(outcome, CancellableWriteOutcome::Cancelled);
        assert_eq!(remaining_slices, [b"abc".as_slice(), b"bc"]);
        assert_eq!(backoffs, 1);
        assert!(output_lock.try_lock().is_ok());
    }

    #[test]
    fn cancellable_write_completes_partial_and_retried_output_atomically() {
        let output_lock = Mutex::new(());
        let mut outcomes = [
            Ok(1),
            Err(nix::errno::Errno::EAGAIN),
            Err(nix::errno::Errno::EINTR),
            Ok(2),
        ]
        .into_iter();
        let mut remaining_slices = Vec::new();
        let mut backoffs = 0;

        let outcome = write_all_cancellable_with(
            &output_lock,
            b"abc",
            || false,
            |remaining| {
                assert!(matches!(
                    output_lock.try_lock(),
                    Err(TryLockError::WouldBlock)
                ));
                remaining_slices.push(remaining.to_vec());
                outcomes.next().expect("scripted write should complete")
            },
            || backoffs += 1,
        )
        .unwrap();

        assert_eq!(outcome, CancellableWriteOutcome::Completed);
        assert_eq!(remaining_slices, [b"abc".as_slice(), b"bc", b"bc", b"bc"]);
        assert_eq!(backoffs, 1);
        assert!(output_lock.try_lock().is_ok());
    }

    #[test]
    fn cancellable_write_retries_interrupts_and_preserves_native_errors() {
        let output_lock = Mutex::new(());
        let mut outcomes =
            [Err(nix::errno::Errno::EINTR), Err(nix::errno::Errno::EPIPE)].into_iter();
        let mut remaining_slices = Vec::new();

        let error = write_all_cancellable_with(
            &output_lock,
            b"response",
            || false,
            |remaining| {
                remaining_slices.push(remaining.to_vec());
                outcomes.next().expect("scripted write should finish")
            },
            || panic!("EINTR and native errors must not back off"),
        )
        .expect_err("native PTY write errors must remain observable");

        assert_eq!(remaining_slices, [b"response".as_slice(), b"response"]);
        assert_eq!(error.raw_os_error(), Some(libc::EPIPE));
        assert!(output_lock.try_lock().is_ok());
    }

    #[test]
    fn concurrent_logical_writes_do_not_interleave() {
        const FIRST_WRITE_ATTEMPTS: usize = 64;

        let output_lock = Arc::new(Mutex::new(()));
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let second_start = Arc::new(Barrier::new(2));
        let (first_entered_tx, first_entered_rx) = mpsc::sync_channel(0);
        let (release_first_tx, release_first_rx) = mpsc::sync_channel(0);
        let (second_ready_tx, second_ready_rx) = mpsc::sync_channel(0);
        let (second_entered_tx, second_entered_rx) = mpsc::sync_channel(1);

        let first_lock = output_lock.clone();
        let first_attempts = attempts.clone();
        let first = thread::spawn(move || {
            let mut is_first_attempt = true;
            write_all_with(&first_lock, &[b'a'; FIRST_WRITE_ATTEMPTS], |_| {
                first_attempts.lock().unwrap().push(b'a');
                if is_first_attempt {
                    is_first_attempt = false;
                    first_entered_tx.send(()).unwrap();
                    release_first_rx.recv().unwrap();
                }
                thread::yield_now();
                Ok(1)
            })
            .unwrap();
        });

        first_entered_rx.recv().unwrap();

        let second_lock = output_lock.clone();
        let second_attempts = attempts.clone();
        let second_barrier = second_start.clone();
        let second = thread::spawn(move || {
            second_ready_tx.send(()).unwrap();
            second_barrier.wait();
            write_all_with(&second_lock, b"b", |_| {
                second_attempts.lock().unwrap().push(b'b');
                second_entered_tx.send(()).unwrap();
                Ok(1)
            })
            .unwrap();
        });

        second_ready_rx.recv().unwrap();
        second_start.wait();
        assert!(matches!(
            second_entered_rx.recv_timeout(Duration::from_millis(250)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        release_first_tx.send(()).unwrap();
        first.join().unwrap();
        second_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        second.join().unwrap();

        let attempts = attempts.lock().unwrap();
        assert_eq!(attempts.len(), FIRST_WRITE_ATTEMPTS + 1);
        assert!(attempts[..FIRST_WRITE_ATTEMPTS]
            .iter()
            .all(|attempt| *attempt == b'a'));
        assert_eq!(attempts[FIRST_WRITE_ATTEMPTS], b'b');
    }

    #[test]
    fn rounds_and_bounds_poll_timeouts() {
        assert_eq!(poll_timeout_for(Duration::ZERO), PollTimeout::ZERO);
        assert_eq!(
            poll_timeout_for(Duration::from_nanos(1)).as_millis(),
            Some(1)
        );
        assert_eq!(
            poll_timeout_for(Duration::from_millis(1)).as_millis(),
            Some(1)
        );
        assert_eq!(
            poll_timeout_for(Duration::from_millis(1) + Duration::from_nanos(1)).as_millis(),
            Some(2)
        );
        assert_eq!(
            poll_timeout_for(Duration::from_millis(i32::MAX as u64) + Duration::from_nanos(1)),
            PollTimeout::MAX
        );
    }

    #[test]
    fn classifies_readable_hangup_error_and_invalid_events() {
        for event in [PollFlags::POLLIN, PollFlags::POLLHUP, PollFlags::POLLERR] {
            classify_readable_events(Some(event)).expect("event should wake a PTY reader");
        }
        classify_readable_events(Some(PollFlags::POLLIN | PollFlags::POLLHUP))
            .expect("pending data must remain readable during hangup");

        let error = classify_readable_events(Some(PollFlags::POLLIN | PollFlags::POLLNVAL))
            .expect_err("invalid descriptor should fail");
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));

        let error = classify_readable_events(Some(PollFlags::empty()))
            .expect_err("empty events should not look readable");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let error = classify_readable_events(None).expect_err("unknown flags should fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn interrupted_polls_preserve_the_original_deadline() {
        let mut elapsed = [
            Duration::ZERO,
            Duration::from_millis(30),
            Duration::from_millis(70),
            Duration::from_millis(100),
        ]
        .into_iter();
        let mut outcomes = [
            Err(nix::errno::Errno::EINTR),
            Err(nix::errno::Errno::EINTR),
            Ok(ReadPollResult::TimedOut),
        ]
        .into_iter();
        let mut observed_timeouts = Vec::new();

        let readable = wait_readable_with(
            Duration::from_millis(100),
            || elapsed.next().expect("scripted clock should have a value"),
            |timeout| {
                observed_timeouts.push(timeout.as_millis());
                outcomes.next().expect("scripted poll should have a result")
            },
        )
        .unwrap();

        assert!(!readable);
        assert_eq!(observed_timeouts, [Some(100), Some(70), Some(30)]);
    }

    #[test]
    fn child_startup_interrupts_share_the_original_deadline() {
        let mut elapsed = [
            Duration::ZERO,
            Duration::from_millis(20),
            Duration::from_millis(50),
            Duration::from_millis(80),
            Duration::from_millis(100),
        ]
        .into_iter();
        let mut poll_outcomes = [
            Err(nix::errno::Errno::EINTR),
            Ok(ReadPollResult::Events(Some(PollFlags::POLLIN))),
            Ok(ReadPollResult::Events(Some(PollFlags::POLLIN))),
            Ok(ReadPollResult::TimedOut),
        ]
        .into_iter();
        let mut read_outcomes = [
            Err(nix::errno::Errno::EINTR),
            Err(nix::errno::Errno::EAGAIN),
        ]
        .into_iter();
        let mut observed_timeouts = Vec::new();
        let mut read_calls = 0;

        let error = child_startup_status_with(
            Duration::from_millis(100),
            || elapsed.next().expect("scripted clock should have a value"),
            |poll_timeout| {
                observed_timeouts.push(poll_timeout.as_millis());
                poll_outcomes
                    .next()
                    .expect("scripted poll should have a result")
            },
            |_| {
                read_calls += 1;
                read_outcomes
                    .next()
                    .expect("scripted read should have a result")
            },
        )
        .expect_err("the shared startup deadline should expire");

        assert!(matches!(error, PtyError::ChildStartupTimeout));
        assert_eq!(observed_timeouts, [Some(100), Some(80), Some(50), Some(20)]);
        assert_eq!(read_calls, 2);
    }

    #[test]
    fn child_startup_read_interrupt_stops_at_the_deadline() {
        let mut elapsed = [Duration::ZERO, Duration::from_millis(100)].into_iter();
        let mut poll_calls = 0;
        let mut read_calls = 0;

        let error = child_startup_status_with(
            Duration::from_millis(100),
            || elapsed.next().expect("scripted clock should have a value"),
            |poll_timeout| {
                poll_calls += 1;
                assert_eq!(poll_timeout.as_millis(), Some(100));
                Ok(ReadPollResult::Events(Some(PollFlags::POLLIN)))
            },
            |_| {
                read_calls += 1;
                Err(nix::errno::Errno::EINTR)
            },
        )
        .expect_err("an interrupted read must not outlive the startup deadline");

        assert!(matches!(error, PtyError::ChildStartupTimeout));
        assert_eq!(poll_calls, 1);
        assert_eq!(read_calls, 1);
    }

    #[test]
    fn child_startup_interrupts_can_finish_before_the_deadline() {
        let mut elapsed = [
            Duration::ZERO,
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(30),
        ]
        .into_iter();
        let mut poll_calls = 0;
        let mut read_calls = 0;

        let status = child_startup_status_with(
            Duration::from_millis(100),
            || elapsed.next().expect("scripted clock should have a value"),
            |_| {
                poll_calls += 1;
                if poll_calls == 1 {
                    Err(nix::errno::Errno::EINTR)
                } else {
                    Ok(ReadPollResult::Events(Some(PollFlags::POLLIN)))
                }
            },
            |stage| {
                read_calls += 1;
                match read_calls {
                    1 => Err(nix::errno::Errno::EINTR),
                    2 => Err(nix::errno::Errno::EAGAIN),
                    _ => {
                        stage[0] = 4;
                        Ok(1)
                    }
                }
            },
        )
        .expect("the child status should arrive within the shared deadline");

        assert_eq!(status, Some(4));
        assert_eq!(poll_calls, 4);
        assert_eq!(read_calls, 3);
    }

    #[test]
    fn child_startup_preserves_native_poll_and_read_errors() {
        let mut poll_calls = 0;
        let mut read_calls = 0;
        let poll_error = child_startup_status_with(
            Duration::from_secs(1),
            || Duration::ZERO,
            |_| {
                poll_calls += 1;
                Err(nix::errno::Errno::EIO)
            },
            |_| {
                read_calls += 1;
                Ok(0)
            },
        )
        .expect_err("native poll failures should be returned");
        assert!(matches!(
            poll_error,
            PtyError::OpenPty(nix::errno::Errno::EIO)
        ));
        assert_eq!(poll_calls, 1);
        assert_eq!(read_calls, 0);

        let mut poll_calls = 0;
        let mut read_calls = 0;
        let read_error = child_startup_status_with(
            Duration::from_secs(1),
            || Duration::ZERO,
            |_| {
                poll_calls += 1;
                Ok(ReadPollResult::Events(Some(PollFlags::POLLIN)))
            },
            |_| {
                read_calls += 1;
                Err(nix::errno::Errno::EIO)
            },
        )
        .expect_err("native read failures should be returned");
        assert!(matches!(
            read_error,
            PtyError::OpenPty(nix::errno::Errno::EIO)
        ));
        assert_eq!(poll_calls, 1);
        assert_eq!(read_calls, 1);
    }

    #[test]
    fn child_startup_hangup_preserves_eof_and_failure_stage_statuses() {
        let success = child_startup_status_with(
            Duration::from_secs(1),
            || Duration::ZERO,
            |_| Ok(ReadPollResult::Events(Some(PollFlags::POLLHUP))),
            |_| Ok(0),
        )
        .expect("EOF should report successful exec");
        assert_eq!(success, None);

        let failure = child_startup_status_with(
            Duration::from_secs(1),
            || Duration::ZERO,
            |_| Ok(ReadPollResult::Events(Some(PollFlags::POLLHUP))),
            |stage| {
                stage[0] = 4;
                Ok(1)
            },
        )
        .expect("a child failure stage should be reported");
        assert_eq!(failure, Some(4));
    }

    #[test]
    fn child_startup_rejects_invalid_poll_events_without_reading() {
        for events in [Some(PollFlags::POLLNVAL), Some(PollFlags::empty()), None] {
            let mut read_calls = 0;
            let error = child_startup_status_with(
                Duration::from_secs(1),
                || Duration::ZERO,
                |_| Ok(ReadPollResult::Events(events)),
                |_| {
                    read_calls += 1;
                    Ok(0)
                },
            )
            .expect_err("invalid poll events should fail the startup handshake");

            assert!(matches!(error, PtyError::Io(_)));
            assert_eq!(read_calls, 0);
        }
    }

    #[test]
    fn wait_readable_times_out_without_output() {
        let program = CString::new("/bin/sh").unwrap();
        let argv = ["sh", "-c", "sleep 5"]
            .into_iter()
            .map(|argument| CString::new(argument).unwrap())
            .collect();
        let pty = Pty::spawn_prepared(40, 4, program, argv, test_environment()).unwrap();

        assert!(!pty.wait_readable(Duration::ZERO).unwrap());
        let started = Instant::now();
        assert!(!pty.wait_readable(Duration::from_millis(25)).unwrap());
        assert!(started.elapsed() >= Duration::from_millis(10));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn wait_readable_reports_pending_output() {
        let program = CString::new("/bin/sh").unwrap();
        let argv = ["sh", "-c", "printf '__KOKUBAN_READABLE__'; sleep 1"]
            .into_iter()
            .map(|argument| CString::new(argument).unwrap())
            .collect();
        let pty = Pty::spawn_prepared(40, 4, program, argv, test_environment()).unwrap();

        assert!(pty.wait_readable(Duration::from_secs(5)).unwrap());
        assert!(pty.wait_readable(Duration::ZERO).unwrap());
        assert_eq!(
            read_until(&pty, b"__KOKUBAN_READABLE__"),
            b"__KOKUBAN_READABLE__"
        );
    }

    #[test]
    fn child_exit_becomes_readable_eof() {
        let program = CString::new("/bin/sh").unwrap();
        let argv = ["sh", "-c", "exit 0"]
            .into_iter()
            .map(|argument| CString::new(argument).unwrap())
            .collect();
        let pty = Pty::spawn_prepared(40, 4, program, argv, test_environment()).unwrap();

        assert!(pty.wait_readable(Duration::from_secs(5)).unwrap());
        assert_eq!(pty.read(&mut [0; 1]).unwrap(), 0);
    }

    #[test]
    fn prefers_kokuban_shell_then_user_shell() {
        let selected = select_shell(
            Some(OsString::from("/custom/fish")),
            Some(OsString::from("/bin/bash")),
            |_| true,
        );
        assert_eq!(selected, Path::new("/custom/fish"));

        let selected = select_shell(
            Some(OsString::from("/missing/fish")),
            Some(OsString::from("/bin/bash")),
            |path| path != Path::new("/missing/fish"),
        );
        assert_eq!(selected, Path::new("/bin/bash"));
    }

    #[test]
    fn rejects_relative_shell_paths() {
        let selected = select_shell(Some(OsString::from("zsh")), None, |_| true);
        assert_eq!(selected, Path::new("/bin/sh"));
    }

    #[test]
    fn reports_only_enabled_graphics_protocols() {
        let cases: [(bool, bool, &[u8]); 4] = [
            (false, false, b""),
            (true, false, b"kitty"),
            (false, true, b"sixel"),
            (true, true, b"kitty,sixel"),
        ];

        for (kitty, sixel, expected) in cases {
            let environment = child_environment(Path::new("/bin/sh"), kitty, sixel).unwrap();
            let values: Vec<&[u8]> = environment
                .iter()
                .filter_map(|entry| entry.as_bytes().strip_prefix(b"KOKUBAN_GRAPHICS="))
                .collect();

            assert_eq!(values, vec![expected]);
        }
    }

    #[test]
    fn marks_descriptors_close_on_exec() {
        let (reader, _writer) = nix::unistd::pipe().expect("pipe should open");
        set_cloexec(reader.as_raw_fd()).expect("FD_CLOEXEC should be set");

        let flags = fcntl(reader.as_raw_fd(), FcntlArg::F_GETFD).expect("flags should read");
        assert!(FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC));
    }

    #[test]
    fn spawns_a_program_with_terminal_environment() {
        let program = CString::new("/bin/sh").unwrap();
        let argv = [
            "sh",
            "-c",
            "printf '__KOKUBAN_PTY__:%s:%s' \"$TERM\" \"$TERM_PROGRAM\"",
        ]
        .into_iter()
        .map(|argument| CString::new(argument).unwrap())
        .collect();
        let pty = Pty::spawn_prepared(40, 4, program, argv, test_environment()).unwrap();
        let expected = b"__KOKUBAN_PTY__:xterm-256color:kokuban";
        let output = read_until(&pty, expected);

        let master = pty.master.as_ref().unwrap();
        let flags = fcntl(master.as_raw_fd(), FcntlArg::F_GETFD).expect("flags should read");
        assert!(FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC));

        assert_eq!(output, expected);
    }

    #[test]
    fn reports_exec_failure_to_the_parent() {
        let program = CString::new("/definitely/not/a/kokuban-program").unwrap();
        let argv = vec![CString::new("missing-program").unwrap()];

        let error = match Pty::spawn_prepared(40, 4, program, argv, test_environment()) {
            Ok(_) => panic!("missing executable unexpectedly started"),
            Err(error) => error,
        };

        assert!(matches!(error, PtyError::ChildSetup("exec")));
    }

    #[test]
    fn drop_terminates_and_reaps_a_child_that_ignores_hup_and_term() {
        let program = CString::new("/bin/sh").unwrap();
        let argv = [
            "sh",
            "-c",
            "trap '' HUP TERM; printf '__KOKUBAN_READY__'; while :; do sleep 1; done",
        ]
        .into_iter()
        .map(|argument| CString::new(argument).unwrap())
        .collect();
        let pty = Pty::spawn_prepared(40, 4, program, argv, test_environment()).unwrap();
        let pid = pty.child_pid;
        let output = read_until(&pty, b"__KOKUBAN_READY__");
        assert!(output
            .windows(b"__KOKUBAN_READY__".len())
            .any(|window| window == b"__KOKUBAN_READY__"));

        drop(pty);

        let deadline = Instant::now() + Duration::from_secs(3);
        while process_exists(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!process_exists(pid), "child process was not reaped");
    }

    #[test]
    fn drop_cleans_descendants_after_the_shell_has_already_exited() {
        let program = CString::new("/bin/sh").unwrap();
        let argv = [
            "sh",
            "-c",
            "(trap '' HUP TERM; while :; do sleep 1; done) & printf '__KOKUBAN_DESC__:%s\\n' \"$!\"",
        ]
        .into_iter()
        .map(|argument| CString::new(argument).unwrap())
        .collect();
        let pty = Pty::spawn_prepared(40, 4, program, argv, test_environment()).unwrap();
        let shell_pid = pty.child_pid;
        let output = read_until(&pty, b"\r\n");
        let output = String::from_utf8_lossy(&output);
        let descendant_pid = output
            .strip_prefix("__KOKUBAN_DESC__:")
            .expect("descendant marker should be present")
            .trim()
            .parse::<i32>()
            .expect("descendant PID should parse");
        let descendant_pid = nix::unistd::Pid::from_raw(descendant_pid);

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match waitpid(shell_pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(WaitStatus::StillAlive) => panic!("shell did not exit"),
                Ok(_) | Err(nix::errno::Errno::ECHILD) => break,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => panic!("waitpid failed: {error}"),
            }
        }

        drop(pty);

        let deadline = Instant::now() + Duration::from_secs(3);
        while process_exists(descendant_pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_exists(descendant_pid),
            "descendant process group survived PTY cleanup"
        );
    }
}
