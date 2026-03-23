use super::PtyError;
use nix::libc;
use nix::pty::openpty;
use nix::unistd::{dup2, execvp, fork, setsid, ForkResult};
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

pub struct Pty {
    master: OwnedFd,
    pub child_pid: nix::unistd::Pid,
}

impl Pty {
    pub fn spawn(cols: u16, rows: u16) -> Result<Self, PtyError> {
        let win_size = nix::pty::Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        let pty = openpty(Some(&win_size), None)?;

        match unsafe { fork() } {
            Ok(ForkResult::Child) => {
                // Close master in child
                drop(pty.master);

                // Create new session
                setsid().ok();

                // Set controlling terminal
                unsafe {
                    libc::ioctl(pty.slave.as_raw_fd(), libc::TIOCSCTTY as _, 0);
                }

                // Dup slave to stdin/stdout/stderr
                let slave_fd = pty.slave.as_raw_fd();
                dup2(slave_fd, 0).ok();
                dup2(slave_fd, 1).ok();
                dup2(slave_fd, 2).ok();
                if slave_fd > 2 {
                    drop(pty.slave);
                }

                // Set up environment
                std::env::set_var("TERM", "xterm-256color");
                std::env::set_var("COLORTERM", "truecolor");
                std::env::set_var("TERM_PROGRAM", "kokuban");
                std::env::set_var("TERM_PROGRAM_VERSION", "0.1.0");

                // Exec shell
                let shell = CString::new("/bin/zsh").unwrap();
                let args = [shell.clone()];
                let _ = execvp(&shell, &args);

                // If exec fails, exit
                std::process::exit(1);
            }
            Ok(ForkResult::Parent { child }) => {
                // Close slave in parent
                drop(pty.slave);

                // Set master to non-blocking
                let flags = nix::fcntl::fcntl(pty.master.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL)
                    .map_err(PtyError::OpenPty)?;
                let mut flags = nix::fcntl::OFlag::from_bits_truncate(flags);
                flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
                nix::fcntl::fcntl(
                    pty.master.as_raw_fd(),
                    nix::fcntl::FcntlArg::F_SETFL(flags),
                )
                .map_err(PtyError::OpenPty)?;

                log::info!("PTY spawned: child pid={child}, master fd={}", pty.master.as_raw_fd());

                Ok(Self {
                    master: pty.master,
                    child_pid: child,
                })
            }
            Err(e) => Err(PtyError::Fork(e.to_string())),
        }
    }

    pub fn master_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    pub fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        nix::unistd::read(self.master.as_raw_fd(), buf).map_err(|e| match e {
            nix::Error::EAGAIN => std::io::Error::new(std::io::ErrorKind::WouldBlock, "EAGAIN"),
            other => std::io::Error::new(std::io::ErrorKind::Other, other),
        })
    }

    pub fn write_all(&self, data: &[u8]) -> std::io::Result<()> {
        let mut written = 0;
        while written < data.len() {
            match nix::unistd::write(&self.master, &data[written..]) {
                Ok(n) => written += n,
                Err(nix::Error::EAGAIN) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(e) => return Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
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
        let ret = unsafe {
            libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &win_size)
        };
        if ret < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
