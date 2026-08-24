use super::PtyError;
use nix::libc;
use nix::poll::{poll, PollFd, PollFlags};
use nix::pty::openpty;
use nix::sys::signal::{kill, killpg, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{fork, getpgrp, pipe, tcgetpgrp, ForkResult, Pid};
use std::ffi::{CString, OsString};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Pty {
    master: Option<OwnedFd>,
    pub child_pid: nix::unistd::Pid,
}

impl Pty {
    pub fn spawn(cols: u16, rows: u16) -> Result<Self, PtyError> {
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
        let environment = child_environment(&shell_path)?;

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
                })
            }
            Err(e) => Err(PtyError::Fork(e.to_string())),
        }
    }

    pub fn master_fd(&self) -> RawFd {
        self.master().as_raw_fd()
    }

    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match nix::unistd::read(self.master().as_raw_fd(), buf) {
                Err(nix::Error::EINTR) => continue,
                Err(nix::Error::EAGAIN) => {
                    return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
                }
                Err(other) => return Err(std::io::Error::other(other)),
                Ok(read) => return Ok(read),
            }
        }
    }

    pub fn write_all(&self, data: &[u8]) -> std::io::Result<()> {
        let mut written = 0;
        while written < data.len() {
            match nix::unistd::write(self.master(), &data[written..]) {
                Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
                Ok(n) => written += n,
                Err(nix::Error::EINTR) => continue,
                Err(nix::Error::EAGAIN) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(e) => return Err(std::io::Error::other(e)),
            }
        }
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        let win_size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let ret = unsafe { libc::ioctl(self.master().as_raw_fd(), libc::TIOCSWINSZ, &win_size) };
        if ret < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn master(&self) -> &OwnedFd {
        self.master.as_ref().expect("PTY master is available")
    }
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

fn child_environment(shell: &Path) -> Result<Vec<CString>, PtyError> {
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
    for (key, value) in [
        (b"SHELL".as_slice(), shell),
        (b"TERM".as_slice(), b"xterm-256color".as_slice()),
        (b"COLORTERM".as_slice(), b"truecolor".as_slice()),
        (b"TERM_PROGRAM".as_slice(), b"kokuban".as_slice()),
        (b"TERM_PROGRAM_VERSION".as_slice(), version.as_bytes()),
        (b"KOKUBAN_VERSION".as_slice(), version.as_bytes()),
        (b"KOKUBAN_GRAPHICS".as_slice(), b"kitty,sixel".as_slice()),
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
    let mut poll_fds = [PollFd::new(
        error_reader.as_fd(),
        PollFlags::POLLIN | PollFlags::POLLHUP,
    )];
    loop {
        match poll(&mut poll_fds, 5_000u16) {
            Ok(0) => return Err(PtyError::ChildStartupTimeout),
            Ok(_) => break,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => return Err(PtyError::OpenPty(error)),
        }
    }

    let mut stage = [0u8; 1];
    loop {
        match nix::unistd::read(error_reader.as_raw_fd(), &mut stage) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(stage[0])),
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
    use super::{select_shell, set_cloexec, Pty};
    use crate::pty::PtyError;
    use nix::fcntl::{fcntl, FcntlArg, FdFlag};
    use nix::libc;
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
    use std::ffi::{CString, OsString};
    use std::os::fd::AsRawFd;
    use std::path::Path;
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
            match pty.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    output.extend_from_slice(&buffer[..read]);
                    if output.windows(marker.len()).any(|window| window == marker) {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("PTY read failed: {error}"),
            }
        }
        output
    }

    fn process_exists(pid: nix::unistd::Pid) -> bool {
        let result = unsafe { libc::kill(pid.as_raw(), 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
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
