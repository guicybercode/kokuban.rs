mod args;
mod terminal;
mod trust;

use anyhow::{bail, Context, Result};
use args::Options;
use russh::client::{self, AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::{
    decode_secret_key, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate,
};
use russh::{Channel, ChannelMsg, Disconnect, MethodKind, Pty};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::timeout;
use zeroize::Zeroizing;

struct Client {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    batch: bool,
}

impl client::Handler for Client {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, server: &PublicKeyOrCertificate) -> Result<bool> {
        if server.certificate().is_some() {
            bail!("host certificates are unsupported; a verified plain server key is required")
        }
        let key = server.public_key();
        match trust::check(&self.known_hosts, &self.host, self.port, &key)? {
            trust::Trust::Known => Ok(true),
            trust::Trust::Unknown if self.batch => bail!("unknown host key for {}:{} in {}; connect interactively after verifying its fingerprint, or install a verified pin", self.host, self.port, self.known_hosts.display()),
            trust::Trust::Unknown => {
                let fingerprint = key.fingerprint(HashAlg::Sha256);
                let question = format!(
                    "The identity of {}:{} is not yet trusted.\r\n{} fingerprint: {fingerprint}\r\nVerify this fingerprint with the server administrator through a trusted channel.\r\nTrust this key and save it? Type yes: ",
                    self.host, self.port, key.algorithm(),
                );
                if terminal::prompt(&question, true).await?.as_str() != "yes" { bail!("host key was not accepted") }
                trust::remember(&self.known_hosts, &self.host, self.port, &key)
                    .context("could not durably save the accepted host key")?;
                Ok(true)
            }
        }
    }

    async fn auth_banner(&mut self, banner: &str, _: &mut client::Session) -> Result<()> {
        eprint!("{}", terminal::clean_message(banner));
        Ok(())
    }
}

fn main() -> ExitCode {
    let options = match args::parse(std::env::args().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            print!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("ssh: {error}");
            return ExitCode::from(255);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("ssh: cannot start runtime: {error}");
            return ExitCode::from(255);
        }
    };
    let result = runtime.block_on(async {
        // Register before prompts/raw mode. Cancellation drops terminal guards;
        // ordinary Ctrl-C in an interactive raw session is sent to the server.
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;
        tokio::select! {
            result = run(options) => result,
            _ = interrupt.recv() => Ok(130),
            _ = terminate.recv() => Ok(143),
            _ = hangup.recv() => Ok(129),
        }
    });
    // Drop russh's remaining tasks, including any cancelled trust prompt, before
    // returning an exit status. process::exit would bypass RAII restoration.
    drop(runtime);
    match result {
        Ok(status) => ExitCode::from(status.min(255) as u8),
        Err(error) => {
            eprintln!("ssh: {error:#}");
            ExitCode::from(255)
        }
    }
}

async fn run(options: Options) -> Result<u32> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is required for SSH keys and host trust")?;
    let known_hosts = options
        .known_hosts
        .clone()
        .unwrap_or_else(|| home.join(".ssh/known_hosts"));
    let handler = Client {
        host: options.host.clone(),
        port: options.port,
        known_hosts,
        batch: options.batch,
    };
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        ..client::Config::default()
    });
    let stream = timeout(
        Duration::from_secs(15),
        tokio::net::TcpStream::connect((options.host.as_str(), options.port)),
    )
    .await
    .context("TCP connection timed out")??;
    stream.set_nodelay(true)?;
    // The handshake includes interactive first-use trust. Its longer timeout
    // allows fingerprint verification while still bounding an unresponsive peer.
    let mut session = timeout(
        Duration::from_secs(180),
        client::connect_stream(config, stream, handler),
    )
    .await
    .context("SSH handshake/host verification timed out")??;
    authenticate(&mut session, &options, &home).await?;
    let result = relay(&mut session, &options).await;
    let _ = timeout(
        Duration::from_secs(2),
        session.disconnect(Disconnect::ByApplication, "Session ended", "en"),
    )
    .await;
    result
}

fn permits(result: &AuthResult, method: MethodKind) -> bool {
    match result {
        AuthResult::Success => false,
        AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods.contains(&method),
    }
}

async fn authenticate(
    session: &mut client::Handle<Client>,
    options: &Options,
    home: &Path,
) -> Result<()> {
    let mut state = session.authenticate_none(options.user.clone()).await?;
    if state.success() {
        return Ok(());
    }
    let identities = if let Some(path) = &options.identity {
        vec![path.clone()]
    } else {
        ["id_ed25519", "id_ecdsa", "id_rsa"]
            .into_iter()
            .map(|name| home.join(".ssh").join(name))
            .filter(|p| p.is_file())
            .collect()
    };
    for identity in identities {
        if !permits(&state, MethodKind::PublicKey) {
            break;
        }
        let key = load_identity(&identity, options.batch).await?;
        let hash = session.best_supported_rsa_hash().await?.flatten();
        state = session
            .authenticate_publickey(
                options.user.clone(),
                PrivateKeyWithHashAlg::new(Arc::new(key), hash),
            )
            .await?;
        if state.success() {
            return Ok(());
        }
    }
    if options.batch {
        bail!("key authentication failed in --batch mode")
    }
    if permits(&state, MethodKind::Password) {
        for attempt in 0..3 {
            let password = terminal::prompt(
                &format!("{}@{} password: ", options.user, options.host),
                false,
            )
            .await?;
            state = session
                .authenticate_password(options.user.clone(), password.to_string())
                .await?;
            if state.success() {
                return Ok(());
            }
            if !permits(&state, MethodKind::Password) {
                break;
            }
            if attempt < 2 {
                eprintln!("Permission denied, try again.");
            }
        }
    }
    if permits(&state, MethodKind::KeyboardInteractive) {
        let mut response = session
            .authenticate_keyboard_interactive_start(options.user.clone(), None)
            .await?;
        // A malicious or misconfigured server must not keep asking for an
        // unbounded list of secrets. Normal password/MFA exchanges use 1-3 rounds.
        for _ in 0..8 {
            match response {
                KeyboardInteractiveAuthResponse::Success => return Ok(()),
                KeyboardInteractiveAuthResponse::Failure { .. } => break,
                KeyboardInteractiveAuthResponse::InfoRequest {
                    name,
                    instructions,
                    prompts,
                } => {
                    if prompts.len() > 8 {
                        bail!("too many keyboard-interactive prompts")
                    }
                    for message in [&name, &instructions] {
                        if !message.is_empty() {
                            eprintln!("{}", terminal::clean_message(message));
                        }
                    }
                    let mut answers = Vec::with_capacity(prompts.len());
                    for prompt in prompts {
                        let answer =
                            terminal::prompt(&terminal::clean_message(&prompt.prompt), prompt.echo)
                                .await?;
                        answers.push(answer.to_string());
                    }
                    response = session
                        .authenticate_keyboard_interactive_respond(answers)
                        .await?;
                }
            }
        }
    }
    bail!("authentication failed")
}

async fn load_identity(path: &Path, batch: bool) -> Result<PrivateKey> {
    let file =
        File::open(path).with_context(|| format!("cannot open private key {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 {
        bail!(
            "private key {} must be a regular file with permissions 0600 or stricter",
            path.display()
        )
    }
    let mut text = Zeroizing::new(String::new());
    file.take(1024 * 1024 + 1).read_to_string(&mut text)?;
    if text.len() > 1024 * 1024 {
        bail!("private key exceeds the 1 MiB limit")
    }
    match decode_secret_key(&text, None) {
        Ok(key) => Ok(key),
        Err(russh::keys::Error::KeyIsEncrypted) => {
            if batch {
                bail!("private key is encrypted; unlock it interactively without --batch")
            }
            let password = terminal::prompt(
                &format!(
                    "Passphrase for {}: ",
                    terminal::clean_message(&path.display().to_string())
                ),
                false,
            )
            .await?;
            decode_secret_key(&text, Some(&password)).context("could not decrypt private key")
        }
        Err(error) => Err(error).context("could not parse private key"),
    }
}

async fn request_accepted(channel: &mut Channel<client::Msg>, request: &str) -> Result<()> {
    timeout(Duration::from_secs(15), async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => return Ok(()),
                Some(ChannelMsg::Failure | ChannelMsg::Close) | None => {
                    bail!("server rejected {request}")
                }
                Some(ChannelMsg::Data { .. } | ChannelMsg::ExtendedData { .. }) => {
                    bail!("server sent data before acknowledging {request}")
                }
                _ => {}
            }
        }
    })
    .await
    .with_context(|| format!("server did not acknowledge {request}"))?
}

async fn relay(session: &mut client::Handle<Client>, options: &Options) -> Result<u32> {
    let mut channel = session.channel_open_session().await?;
    let mut resize = signal(SignalKind::window_change())?;
    if options.pty {
        if !terminal::is_terminal(0) {
            bail!("a PTY session requires terminal input; use -T for pipes")
        }
        let (cols, rows, width, height) = terminal::dimensions(0);
        let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_owned());
        channel
            .request_pty(
                true,
                &term,
                cols,
                rows,
                width,
                height,
                &[
                    (Pty::VINTR, 3),
                    (Pty::VQUIT, 28),
                    (Pty::VERASE, 127),
                    (Pty::VKILL, 21),
                    (Pty::VEOF, 4),
                    (Pty::VSUSP, 26),
                    (Pty::ICRNL, 1),
                    (Pty::IUTF8, 1),
                    (Pty::ISIG, 1),
                    (Pty::ICANON, 1),
                    (Pty::ECHO, 1),
                    (Pty::OPOST, 1),
                    (Pty::ONLCR, 1),
                ],
            )
            .await?;
        request_accepted(&mut channel, "PTY allocation").await?;
    }
    let _raw = if options.pty {
        Some(terminal::ModeGuard::raw(0)?)
    } else {
        None
    };
    let _flags = terminal::StdioFlags::nonblocking()?;
    let input = terminal::Descriptor::from_fd(0)?;
    let output = terminal::Descriptor::from_fd(1)?;
    let error_output = terminal::Descriptor::from_fd(2)?;
    if let Some(command) = &options.command {
        channel.exec(true, command.as_bytes()).await?;
    } else {
        channel.request_shell(true).await?;
    }
    request_accepted(&mut channel, "shell/command execution").await?;
    let mut input_closed = false;
    let mut buffer = [0u8; 8192];
    let mut exit_status = None;
    let mut escape = terminal::EscapeFilter::new();
    loop {
        tokio::select! {
            received = input.read(&mut buffer), if !input_closed => {
                let count = received?;
                if count == 0 {
                    input_closed = true;
                    channel.eof().await?;
                } else if options.pty {
                    let (bytes, disconnect) = escape.apply(&buffer[..count]);
                    if !bytes.is_empty() { channel.data(&bytes[..]).await?; }
                    if disconnect { channel.close().await?; return Ok(0); }
                } else {
                    channel.data(&buffer[..count]).await?;
                }
            }
            _ = resize.recv(), if options.pty => {
                let (cols, rows, width, height) = terminal::dimensions(0);
                channel.window_change(cols, rows, width, height).await?;
            }
            message = channel.wait() => {
                match message {
                    Some(ChannelMsg::Data { data }) => output.write_all(&data).await?,
                    Some(ChannelMsg::ExtendedData { data, ext: 1 }) => error_output.write_all(&data).await?,
                    Some(ChannelMsg::ExitStatus { exit_status: status }) => exit_status = Some(status),
                    Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                        error_output.write_all(format!("\r\nRemote process terminated by {signal_name:?}\r\n").as_bytes()).await?;
                        exit_status = Some(255);
                    }
                    // Keep draining after exit-status/EOF; final output may
                    // already be in flight. Close/None are the terminal states.
                    Some(ChannelMsg::Close) | None => return Ok(exit_status.unwrap_or(255)),
                    _ => {},
                }
            }
        }
    }
}
