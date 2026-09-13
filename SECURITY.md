# Security policy

## Supported versions

| Version | Security fixes |
| --- | --- |
| Latest 0.2.x release | Supported on a best-effort basis |
| `main` | Development fixes; unreleased changes may be unstable |
| Older releases and experimental branches | No separate maintenance commitment |

Kokuban is early-stage software. There is no guaranteed response or remediation timeline.

## Report a vulnerability privately

Use GitHub's [Report a vulnerability](https://github.com/guicybercode/kokuban.rs/security/advisories/new) form. Private vulnerability reporting is enabled for this repository. You need to sign in to GitHub to submit a report.

Please include:

- The affected release or commit, operating system, architecture and display backend.
- Reproduction steps and a minimal terminal sequence, image or test program when relevant.
- The expected and observed behavior, likely impact, and any suggested fix.
- Logs with tokens, SSH credentials, personal data and unrelated command history removed.

Do not post exploitable details in a public issue before maintainers have had an opportunity to investigate and coordinate disclosure. If the private form is unavailable, email [euguigom@gmail.com](mailto:euguigom@gmail.com). Ordinary bugs and feature requests can use [public issues](https://github.com/guicybercode/kokuban.rs/issues).

Maintainers will investigate, discuss a fix and publication timing with the reporter, and credit the reporter in an advisory when requested. Reports are handled on a best-effort basis; there is no bug bounty program.

## Scope and trust boundaries

Relevant reports include memory-safety issues, escape-sequence/parser flaws, unsafe image/file handling, resource-limit bypasses, clipboard injection and errors in child-process or PTY isolation.

Kokuban runs the selected shell and its commands with your account's permissions. It is not a sandbox for those commands or for remote terminal output. Graphics and fonts also use dependencies and native system libraries, whose licenses and updates remain separate from the [project license](LICENSE).

In v0.2, configuration is loaded at startup in this order:

1. `kokuban.toml` in the launch directory.
2. `$XDG_CONFIG_HOME/kokuban/kokuban.toml` when `XDG_CONFIG_HOME` is an absolute path; otherwise `$HOME/.config/kokuban/kokuban.toml` when `HOME` is an absolute path.

The first existing configuration takes precedence; files are not merged. A read or parse error uses defaults instead of trying the next location. Review project-local configuration before launching from an untrusted directory, even when you have configured your user-level file.

Kitty file transfer is enabled by default; to keep image transfers in the PTY byte stream, set:

```toml
[images.kitty]
allow_file_transfer = false
```

Set `images.enabled = false` to disable both image protocols. These settings apply at startup and do not replace keeping the application and system libraries updated. Download releases from this repository and verify the published SHA-256 checksum before running a binary; checksums detect corruption but do not replace publisher identity verification.
