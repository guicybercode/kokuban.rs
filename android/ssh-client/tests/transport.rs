//! Exercise the actual CLI process over an authenticated SSH transport. The
//! in-process fixture has deterministic test-only keys and binds only loopback.
use russh::keys::{
    ssh_key::private::{Ed25519Keypair, KeypairData},
    PrivateKey, PublicKey,
};
use russh::server::{self, Server as _, Session};
use russh::{Channel, ChannelId, Pty};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

fn key(seed: u8) -> PrivateKey {
    PrivateKey::new(
        KeypairData::Ed25519(Ed25519Keypair::from_seed(&[seed; 32])),
        "test-only",
    )
    .unwrap()
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static SERIAL: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "kokuban-ssh-process-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Clone)]
struct Fixture {
    authenticated: Arc<AtomicUsize>,
    size: Arc<Mutex<(u32, u32)>>,
    input: Vec<u8>,
}

impl server::Server for Fixture {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        self.clone()
    }
}

impl server::Handler for Fixture {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<server::Auth, Self::Error> {
        if user == "tester" && public_key.key_data() == key(2).public_key().key_data() {
            self.authenticated.fetch_add(1, Ordering::Relaxed);
            Ok(server::Auth::Accept)
        } else {
            Ok(server::Auth::reject())
        }
    }

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        if user == "tester" && password == "fixture-password" {
            self.authenticated.fetch_add(1, Ordering::Relaxed);
            Ok(server::Auth::Accept)
        } else {
            Ok(server::Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        assert_eq!(command, b"fixture command");
        session.channel_success(channel)?;
        session.data(channel, b"before-status\n".as_slice())?;
        session.extended_data(channel, 1, b"stderr-output\n".as_slice())?;
        session.exit_status_request(channel, 7)?;
        // The client must drain this after exit-status instead of dropping it.
        session.data(channel, b"after-status\n".as_slice())?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        columns: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        *self.size.lock().unwrap() = (columns, rows);
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, b"fixture-shell> ".as_slice())?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _: ChannelId,
        columns: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        *self.size.lock().unwrap() = (columns, rows);
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        bytes: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.input.extend_from_slice(bytes);
        if self.input.ends_with(b"exit\r") {
            session.exit_status_request(channel, 0)?;
            session.eof(channel)?;
            session.close(channel)?;
        } else {
            session.data(channel, bytes.to_vec())?;
        }
        Ok(())
    }
}

async fn start_server() -> (u16, Fixture, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let fixture = Fixture {
        authenticated: Arc::new(AtomicUsize::new(0)),
        size: Arc::new(Mutex::new((0, 0))),
        input: Vec::new(),
    };
    let mut server = fixture.clone();
    let config = Arc::new(server::Config {
        keys: vec![key(1)],
        auth_rejection_time: Duration::ZERO,
        ..server::Config::default()
    });
    let task = tokio::spawn(async move {
        server.run_on_socket(config, &listener).await.unwrap();
    });
    (port, fixture, task)
}

fn client(directory: &Path, port: u16) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kokuban-ssh"));
    command
        .env("HOME", directory)
        .env("TERM", "xterm-256color")
        .args(["-p", &port.to_string()]);
    command
}

async fn wait(child: &mut Child) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "SSH client did not exit while stdin remained open"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn output(command: &mut Command) -> Output {
    let mut child = Process(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    // Keep stdin open intentionally: remote close must cancel its idle read.
    let status = wait(&mut child.0).await;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    Output {
        status,
        stdout,
        stderr,
    }
}

#[tokio::test]
async fn pinned_key_exec_drains_stdout_stderr_and_rejects_untrusted_hosts_before_auth() {
    let home = Directory::new();
    fs::create_dir(home.0.join(".ssh")).unwrap();
    let identity = home.0.join(".ssh/id_ed25519");
    let secret = key(2)
        .to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&identity)
        .unwrap()
        .write_all(secret.as_bytes())
        .unwrap();
    let (port, fixture, server) = start_server().await;
    let pin = home.0.join(".ssh/known_hosts");
    fs::write(
        &pin,
        format!(
            "[127.0.0.1]:{port} {}\n",
            key(1).public_key().to_openssh().unwrap()
        ),
    )
    .unwrap();
    let result =
        output(client(&home.0, port).args(["--batch", "tester@127.0.0.1", "fixture command"]))
            .await;
    assert_eq!(
        result.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"before-status\nafter-status\n");
    assert_eq!(result.stderr, b"stderr-output\n");
    assert_eq!(fixture.authenticated.load(Ordering::Relaxed), 1);

    fs::write(
        &pin,
        format!(
            "[127.0.0.1]:{port} {}\n",
            key(3).public_key().to_openssh().unwrap()
        ),
    )
    .unwrap();
    let rejected =
        output(client(&home.0, port).args(["--batch", "tester@127.0.0.1", "fixture command"]))
            .await;
    assert_eq!(rejected.status.code(), Some(255));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("HOST KEY CHANGED"));
    assert_eq!(fixture.authenticated.load(Ordering::Relaxed), 1);

    fs::remove_file(&pin).unwrap();
    let unknown =
        output(client(&home.0, port).args(["--batch", "tester@127.0.0.1", "fixture command"]))
            .await;
    assert_eq!(unknown.status.code(), Some(255));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown host key"));
    assert!(!pin.exists());
    assert_eq!(fixture.authenticated.load(Ordering::Relaxed), 1);
    server.abort();
}

struct PtyPair {
    master: File,
    slave: File,
}
impl PtyPair {
    fn new() -> Self {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: output pointers are valid; optional name/termios/winsize are null.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let pair = Self {
            master: unsafe { File::from_raw_fd(master) },
            slave: unsafe { File::from_raw_fd(slave) },
        };
        // SAFETY: master is owned by the pair until test completion.
        assert_eq!(
            unsafe { libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK) },
            0
        );
        pair.resize(80, 24);
        pair
    }
    fn resize(&self, cols: u16, rows: u16) {
        let size = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: ioctl receives a live descriptor and initialized winsize.
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) },
            0
        );
    }
    fn modes(&self) -> libc::termios {
        let mut modes = std::mem::MaybeUninit::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(self.master.as_raw_fd(), modes.as_mut_ptr()) },
            0
        );
        unsafe { modes.assume_init() }
    }
    fn spawn(&self, command: &mut Command) -> Process {
        command
            .stdin(self.slave.try_clone().unwrap())
            .stdout(self.slave.try_clone().unwrap())
            .stderr(self.slave.try_clone().unwrap());
        // SAFETY: child closure performs only async-signal-safe libc syscalls
        // between fork and exec, establishing its own controlling terminal.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0
                    || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0
                    || libc::tcsetpgrp(0, libc::getpgrp()) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Process(command.spawn().unwrap())
    }
    async fn read_until(&mut self, needle: &str, output: &mut String) {
        let start = Instant::now();
        while !output.contains(needle) {
            let mut bytes = [0u8; 4096];
            match self.master.read(&mut bytes) {
                Ok(count) => output.push_str(&String::from_utf8_lossy(&bytes[..count])),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("PTY read failed: {error}; {output}"),
            }
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "missing {needle:?} in {output:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test]
async fn interactive_trust_password_no_echo_resize_and_terminal_restoration() {
    let home = Directory::new();
    let (port, fixture, server) = start_server().await;
    let mut pty = PtyPair::new();
    let original = pty.modes();
    let mut child = pty.spawn(client(&home.0, port).arg("tester@127.0.0.1"));
    let mut transcript = String::new();
    pty.read_until("Type yes:", &mut transcript).await;
    assert!(transcript.contains("SHA256:"));
    pty.master.write_all(b"yes\n").unwrap_or_else(|error| {
        panic!(
            "trust input failed: {error}; status={:?}; transcript={transcript:?}",
            child.0.try_wait()
        );
    });
    pty.read_until("password:", &mut transcript).await;
    assert_eq!(pty.modes().c_lflag & libc::ECHO, 0);
    pty.master.write_all(b"fixture-password\n").unwrap();
    pty.read_until("fixture-shell>", &mut transcript).await;
    assert!(
        !transcript.contains("fixture-password"),
        "password was echoed"
    );
    assert_eq!(fixture.authenticated.load(Ordering::Relaxed), 1);
    assert!(home.0.join(".ssh/known_hosts").is_file());
    pty.resize(120, 40);
    let start = Instant::now();
    while *fixture.size.lock().unwrap() != (120, 40) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "resize did not reach SSH server"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    pty.master.write_all("olá\r".as_bytes()).unwrap();
    pty.read_until("olá", &mut transcript).await;
    pty.master.write_all(b"exit\r").unwrap();
    assert_eq!(wait(&mut child.0).await.code(), Some(0));
    assert_eq!(pty.modes().c_lflag, original.c_lflag);
    assert_eq!(pty.modes().c_iflag, original.c_iflag);
    assert_eq!(pty.modes().c_oflag, original.c_oflag);
    server.abort();
}
