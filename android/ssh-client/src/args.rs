use anyhow::{bail, Context, Result};
use std::path::PathBuf;

pub const USAGE: &str = "Usage: ssh [-p PORT] [-i KEY] [-t|-T] [--batch] [--known-hosts FILE] user@host [command ...]\n\
    -p PORT             Server port (default 22)\n\
    -i KEY              OpenSSH/PEM private key (encrypted keys prompt for passphrase)\n\
    -t / -T             Request / disable remote PTY (default: PTY for a shell)\n\
    --batch             Never prompt; require an existing host pin and usable key\n\
    --known-hosts FILE  Override $HOME/.ssh/known_hosts\n\
    -l USER             Alternate to USER@HOST\n\
    -h, --help          Show this help\n\
Unknown host keys require an explicit 'yes' after fingerprint verification.\n\
Changed/revoked keys are always rejected. Exit a shell normally or type Enter ~ .\n";

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity: Option<PathBuf>,
    pub known_hosts: Option<PathBuf>,
    pub command: Option<String>,
    pub pty: bool,
    pub batch: bool,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Options>> {
    let mut args = args.into_iter();
    let mut port = 22;
    let mut identity = None;
    let mut known_hosts = None;
    let mut username = None;
    let mut pty = None;
    let mut batch = false;
    let mut end_options = false;
    let target = loop {
        let Some(argument) = args.next() else {
            bail!("missing user@host; use --help")
        };
        if !end_options {
            match argument.as_str() {
                "-h" | "--help" => return Ok(None),
                "--" => {
                    end_options = true;
                    continue;
                }
                "-p" => {
                    port = args
                        .next()
                        .context("-p requires a port")?
                        .parse::<u16>()
                        .context("invalid port")?;
                    if port == 0 {
                        bail!("port must be between 1 and 65535")
                    }
                    continue;
                }
                "-i" => {
                    identity = Some(PathBuf::from(
                        args.next().context("-i requires a key path")?,
                    ));
                    continue;
                }
                "--known-hosts" => {
                    known_hosts = Some(PathBuf::from(
                        args.next().context("--known-hosts requires a path")?,
                    ));
                    continue;
                }
                "-l" => {
                    username = Some(args.next().context("-l requires a username")?);
                    continue;
                }
                "-t" => {
                    pty = Some(true);
                    continue;
                }
                "-T" => {
                    pty = Some(false);
                    continue;
                }
                "--batch" => {
                    batch = true;
                    continue;
                }
                option if option.starts_with('-') => {
                    bail!("unsupported option {option}; use --help")
                }
                _ => {}
            }
        }
        break argument;
    };
    let (user, host) = match target.rsplit_once('@') {
        Some((user, host)) => (username.unwrap_or_else(|| user.to_owned()), host.to_owned()),
        None => (
            username.context("specify the remote username as user@host or -l USER")?,
            target,
        ),
    };
    let host = if host.starts_with('[') && host.ends_with(']') {
        host[1..host.len() - 1].to_owned()
    } else {
        host
    };
    if host.is_empty()
        || host.len() > 253
        || !host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_:%".contains(&c))
    {
        bail!("invalid hostname or IP address")
    }
    if user.is_empty()
        || user.len() > 255
        || !user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-\\".contains(&c))
    {
        bail!("invalid remote username")
    }
    let command = args.collect::<Vec<_>>().join(" ");
    let command = if command.is_empty() {
        None
    } else {
        Some(command)
    };
    Ok(Some(Options {
        host: host.to_ascii_lowercase(),
        user,
        port,
        identity,
        known_hosts,
        pty: pty.unwrap_or(command.is_none()),
        command,
        batch,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(values: &[&str]) -> Result<Option<Options>> {
        parse(values.iter().map(|v| v.to_string()))
    }

    #[test]
    fn parses_ports_keys_ipv6_and_remote_shell_commands() {
        let parsed = options(&[
            "-p",
            "2222",
            "-i",
            "/tmp/id",
            "alice@[::1]",
            "printf '%s\\n' hello",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(parsed.host, "::1");
        assert_eq!(parsed.user, "alice");
        assert_eq!(parsed.port, 2222);
        assert_eq!(parsed.identity, Some(PathBuf::from("/tmp/id")));
        assert_eq!(parsed.command.as_deref(), Some("printf '%s\\n' hello"));
        assert!(!parsed.pty);
        assert!(options(&["alice@localhost"]).unwrap().unwrap().pty);
        assert!(
            options(&["-t", "alice@localhost", "vim"])
                .unwrap()
                .unwrap()
                .pty
        );
    }

    #[test]
    fn rejects_malformed_targets_and_unsafe_hostkey_overrides() {
        for input in [
            &["alice@host\nother"][..],
            &["@host"],
            &["alice@"],
            &["-p", "0", "a@h"],
            &["-p", "65536", "a@h"],
            &["-i"],
            &["-o", "StrictHostKeyChecking=no", "a@h"],
            &["host"],
        ] {
            assert!(options(input).is_err(), "{input:?}");
        }
        assert!(options(&["--help"]).unwrap().is_none());
    }
}
