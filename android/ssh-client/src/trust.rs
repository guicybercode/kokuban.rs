//! Explicit first-use trust and strict host-key pinning, including revocation.
//! We parse OpenSSH records directly because russh's convenience helper treats
//! unreadable files and a different stored key algorithm as an unknown host.

use anyhow::{bail, Context, Result};
use ring::hmac;
use russh::keys::{
    ssh_key::known_hosts::{Entry, HostPatterns, Marker},
    PublicKey,
};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

const MAX_KNOWN_HOSTS_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Known,
    Unknown,
}

pub fn check(path: &Path, host: &str, port: u16, key: &PublicKey) -> Result<Trust> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Trust::Unknown),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let text = read_records(&mut file)?;
    check_records(&text, host, port, key)
}

fn read_records(file: &mut File) -> Result<String> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.mode() & 0o022 != 0 {
        bail!("known_hosts must be a regular file that is not writable by group or others")
    }
    let mut text = String::new();
    file.take(MAX_KNOWN_HOSTS_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_KNOWN_HOSTS_BYTES {
        bail!("known_hosts exceeds the 1 MiB limit")
    }
    Ok(text)
}

pub fn check_records(text: &str, host: &str, port: u16, key: &PublicKey) -> Result<Trust> {
    let target = if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    };
    let mut host_recorded = false;
    let mut key_matches = false;
    for (index, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        // OpenSSH accepts tabs and repeated spaces, while Entry expects a space
        // between fields. Normalize only whitespace, leaving key bytes intact.
        let normalized = line.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
        let entry: Entry = normalized
            .parse()
            .with_context(|| format!("invalid known_hosts record at line {}", index + 1))?;
        if !matches_host(entry.host_patterns(), &target) {
            continue;
        }
        let same_key = entry.public_key().key_data() == key.key_data();
        match entry.marker() {
            Some(Marker::Revoked) if same_key => bail!("server key is revoked in known_hosts at line {}", index + 1),
            Some(Marker::Revoked) => {},
            Some(Marker::CertAuthority) => bail!("host certificate authorities are not supported; install a separately verified plain host key"),
            None => { host_recorded = true; key_matches |= same_key; }
        }
    }
    if key_matches {
        return Ok(Trust::Known);
    }
    if host_recorded {
        bail!("HOST KEY CHANGED for {target}; connection refused. Verify the change through a trusted channel before updating known_hosts")
    }
    Ok(Trust::Unknown)
}

fn matches_host(patterns: &HostPatterns, host: &str) -> bool {
    match patterns {
        HostPatterns::HashedName { salt, hash } => hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, salt),
            host.as_bytes(),
            hash,
        )
        .is_ok(),
        HostPatterns::Patterns(patterns) => {
            let mut matched = false;
            for pattern in patterns {
                if let Some(negative) = pattern.strip_prefix('!') {
                    if glob_matches(negative.as_bytes(), host.as_bytes()) {
                        return false;
                    }
                } else {
                    matched |= glob_matches(pattern.as_bytes(), host.as_bytes());
                }
            }
            matched
        }
    }
}

fn glob_matches(pattern: &[u8], value: &[u8]) -> bool {
    let (mut p, mut v, mut star, mut retry) = (0, 0, None, 0);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p].eq_ignore_ascii_case(&value[v])) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = v;
        } else if let Some(s) = star {
            retry += 1;
            v = retry;
            p = s + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// Called only after the user has explicitly accepted the displayed fingerprint.
pub fn remember(path: &Path, host: &str, port: u16, key: &PublicKey) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    // A second terminal can accept a key concurrently. Lock and recheck before
    // appending so that an intervening mismatch is never silently overwritten.
    // SAFETY: file owns a valid descriptor for the duration of this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let text = read_records(&mut file)?;
    if check_records(&text, host, port, key)? == Trust::Known {
        return Ok(());
    }
    let target = if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    };
    let plain = PublicKey::new(key.key_data().clone(), "");
    let record = format!(
        "{}{} {}\n",
        if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        },
        target,
        plain.to_openssh()?
    );
    if text.len() + record.len() > MAX_KNOWN_HOSTS_BYTES as usize {
        bail!("known_hosts exceeds the 1 MiB limit")
    }
    file.seek(SeekFrom::End(0))?;
    file.write_all(record.as_bytes())?;
    file.sync_all()?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::private::Ed25519Keypair;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn key(seed: u8) -> PublicKey {
        PublicKey::from(Ed25519Keypair::from_seed(&[seed; 32]).public)
    }
    fn record(host: &str, key: &PublicKey) -> String {
        format!("{host} {}\n", key.to_openssh().unwrap())
    }

    #[test]
    fn unknown_known_changed_and_revoked_keys_are_distinct() {
        let original = key(1);
        let changed = key(2);
        assert_eq!(
            check_records("", "host", 22, &original).unwrap(),
            Trust::Unknown
        );
        let records = record("host", &original);
        assert_eq!(
            check_records(&records, "host", 22, &original).unwrap(),
            Trust::Known
        );
        assert!(check_records(&records, "host", 22, &changed)
            .unwrap_err()
            .to_string()
            .contains("CHANGED"));
        let other_algorithm = PublicKey::new(
            russh::keys::ssh_key::public::KeyData::SkEd25519(
                russh::keys::ssh_key::public::SkEd25519::new(
                    Ed25519Keypair::from_seed(&[1; 32]).public,
                    "ssh:",
                ),
            ),
            "",
        );
        assert_ne!(original.algorithm(), other_algorithm.algorithm());
        assert!(check_records(&records, "host", 22, &other_algorithm)
            .unwrap_err()
            .to_string()
            .contains("CHANGED"));
        let revoked = format!("{records}@revoked {}", record("host", &original));
        assert!(check_records(&revoked, "host", 22, &original)
            .unwrap_err()
            .to_string()
            .contains("revoked"));
    }

    #[test]
    fn ports_globs_negations_and_whitespace_cannot_bypass_pinning() {
        let original = key(1);
        let changed = key(2);
        let records =
            record("[*.example.com]:2222,![skip.example.com]:2222", &original).replace(' ', "\t  ");
        assert_eq!(
            check_records(&records, "a.example.com", 2222, &original).unwrap(),
            Trust::Known
        );
        assert_eq!(
            check_records(&records, "skip.example.com", 2222, &original).unwrap(),
            Trust::Unknown
        );
        assert_eq!(
            check_records(&records, "a.example.com", 22, &original).unwrap(),
            Trust::Unknown
        );
        assert!(check_records(&records, "a.example.com", 2222, &changed).is_err());
        assert!(check_records("host ssh-ed25519 INVALID", "host", 22, &original).is_err());
    }

    #[test]
    fn hashed_hostnames_use_verified_hmac_and_revocation() {
        let tag = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, b"salt"),
            b"host",
        );
        let patterns = HostPatterns::HashedName {
            salt: b"salt".to_vec(),
            hash: tag.as_ref().try_into().unwrap(),
        };
        assert!(matches_host(&patterns, "host"));
        assert!(!matches_host(&patterns, "other"));
        let records = format!("@revoked {}", record(&patterns.to_string(), &key(1)));
        assert!(check_records(&records, "host", 22, &key(1)).is_err());
    }

    #[test]
    fn saved_trust_is_private_durable_and_never_replaces_a_changed_key() {
        static SERIAL: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "kokuban-ssh-trust-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let path = root.join(".ssh/known_hosts");
        let original = key(1);
        remember(&path, "host", 2222, &original).unwrap();
        assert_eq!(check(&path, "host", 2222, &original).unwrap(), Trust::Known);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let initial = fs::read(&path).unwrap();
        remember(&path, "host", 2222, &original).unwrap();
        assert_eq!(fs::read(&path).unwrap(), initial);
        assert!(remember(&path, "host", 2222, &key(2)).is_err());
        assert_eq!(fs::read(&path).unwrap(), initial);
        // An existing directory and a symlink must not be mistaken for an
        // absent trust file, even if the pinned key itself is otherwise valid.
        assert!(check(&root, "host", 2222, &original).is_err());
        let link = root.join("linked_hosts");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(check(&link, "host", 2222, &original).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
