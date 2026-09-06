# Kokuban SSH

Small native SSH CLI for the Android terminal. The application/client logic is
Rust. `russh` is pinned to **0.63.2**, with its `ring` and RSA features; `ring`
**0.17.14** contains C/assembly crypto implementations. Android's libc supplies
PTY, descriptor and signal operations. This is not a claim that every dependency
is Rust.

```sh
ssh alice@example.com
ssh -p 2222 -i "$HOME/.ssh/id_ed25519" alice@example.com
ssh alice@example.com 'git --version; uname -a'
ssh -t alice@example.com 'tmux new-session -A -s work'
ssh --batch alice@example.com 'cargo test'
```

Options must precede `user@host`; `--help` lists them. As with OpenSSH, command
arguments are joined and interpreted by the remote shell. Quote a complete
remote command when it includes shell syntax. With no command, a remote shell
and PTY are requested. Use `-T` when passing input through a pipe. In an
interactive session, Ctrl/Alt bytes, UTF-8 input and SIGWINCH resizes are forwarded
to the remote terminal. Exit the remote shell normally, or type Enter followed
by `~.` to disconnect locally; `~~` at the start of a line sends one literal `~`.

## Host identity and authentication

The first connection displays the SHA-256 fingerprint and requires the exact
answer `yes`. Verify the fingerprint with the administrator through an existing
trusted channel before accepting it. The accepted key is appended to
`$HOME/.ssh/known_hosts` under an exclusive file lock and synced to storage before
authentication proceeds. Existing mismatched keys, revoked keys, unreadable or
malformed trust files fail closed. Changed keys have no bypass flag; investigate
and update the pin only after separately verifying the replacement.

Exact hosts, ports, comma-separated patterns, `*`/`?`, negations and OpenSSH
hashed hostnames are supported. `@revoked` is enforced. Host certificate
authorities/certificates are rejected explicitly. Trust files must be regular
files without group/other write permission; symlink files are rejected.

The client tries the specified key, or `.ssh/id_ed25519`, `id_ecdsa`, and `id_rsa`,
then password and keyboard-interactive/MFA if the server offers them. Private
keys must have mode `0600` or stricter. Encrypted key passphrases, passwords and
non-echo MFA answers are read from a terminal with echo disabled, never from
command arguments or password environment variables. `--batch` disables all
prompts and requires a previously verified host pin and usable private key.
`--known-hosts FILE` selects an alternate verified trust file.

Keys are not generated, downloaded or copied from a desktop automatically.
Provision a dedicated key into the app's private `.ssh` directory, or use
password/MFA authentication when allowed by the server. Debug `adb run-as` file
copy is a development workflow; a production key-import UI is separate work.

## Build and APK contract

From the repository root, using the pinned Rust toolchain:

```sh
export PATH="$HOME/.rustup/toolchains/1.94.1-aarch64-apple-darwin/bin:$PATH"
cargo test --locked --manifest-path android/ssh-client/Cargo.toml \
  --target-dir target/ssh-client
cargo clippy --locked --manifest-path android/ssh-client/Cargo.toml \
  --target-dir target/ssh-client --all-targets -- -D warnings
```

The PATH example is for the macOS ARM64 development host; on other hosts use
the directory containing `rustup which --toolchain 1.94.1 rustc`. Pin both Cargo
and rustc, since a Homebrew rustc earlier in PATH can otherwise be selected.

The Android packaging script builds the `kokuban-ssh` **PIE executable**, copies
it into APK `lib/<abi>/libkokuban_ssh.so`, then aligns and signs the APK. The `.so`
name is Android packaging convention: this file is executed as a program, not
loaded with `dlopen`. The APK must use `android:extractNativeLibs="true"`.
At runtime the shell's `ssh` function invokes the extracted executable in
`ApplicationInfo.nativeLibraryDir`. Do not copy the executable into `$HOME` and
try to execute it there: Android's application-data execution restrictions apply.
The package requires Android API 26 or newer and INTERNET permission.

Source and API references: [russh 0.63.2](https://docs.rs/russh/0.63.2/russh/),
[client host-key validation](https://docs.rs/russh/0.63.2/russh/client/trait.Handler.html),
[ssh-key OpenSSH trust parser](https://docs.rs/ssh-key/0.7.0-rc.11/ssh_key/known_hosts/index.html).

## Validation and scope

Unit tests exercise argument parsing, known-host changes/revocation, hashed
hosts, wildcard negations, durable file permissions, escape sequences and
nonpollable EOF. Process tests use a loopback SSH server with deterministic
test-only keys to check real key authentication, command stdout/stderr and exit
status, final output after exit-status, rejection before authentication, an
idle stdin that stays open after remote close, explicit first-use trust,
password echo suppression, UTF-8, PTY resize and terminal restoration.

These tests do not prove Android execution; APK/device SSH evidence belongs in
`docs/ANDROID.md`. Normal PTYs use Tokio `AsyncFd` readiness without an idle stdin
thread. A Unix device alias that cannot be registered falls back to cancellable
10 ms waits only while its nonblocking I/O reports `WouldBlock`.

This client provides shell/exec sessions, not the full OpenSSH command suite:
no SSH agent, SSH config file, jump host, forwarding, SFTP/SCP, certificate auth,
or session reconnection is implemented. Development tools run on the SSH server;
the client does not turn Android's system shell into a local Linux distribution.
EOF, remote exit, link failure and normal termination restore local terminal
flags; SIGKILL and device/process destruction cannot run Rust destructors.
