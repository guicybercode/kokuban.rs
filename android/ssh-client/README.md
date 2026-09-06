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
printf 'hello\n' | ssh -T alice@example.com 'cat'
```

Inside Kokuban, `ssh` is the shell function that invokes `$KOKUBAN_SSH`, the
extracted executable. For a host build, use `target/ssh-client/debug/kokuban-ssh`
in place of `ssh`; invoking the host's ordinary `ssh` tests a different client.

| Option | Behavior |
| --- | --- |
| `-p PORT` | Port 1–65535; default 22. |
| `-i KEY` | Explicit private-key file. |
| `-l USER` | Alternate to the username in `user@host`. |
| `-t` / `-T` | Enable / disable the remote PTY; `-t` requires terminal stdin. |
| `--batch` | Disable all trust, password and passphrase prompts. |
| `--known-hosts FILE` | Use this trust file instead of `$HOME/.ssh/known_hosts`. |
| `-h`, `--help` | Print usage and exit successfully. |

Options must precede `user@host`. IPv6 accepts `alice@[::1]`. As with OpenSSH, command
arguments are joined and interpreted by the remote shell. Quote a complete
remote command when it includes shell syntax. With no command, a remote shell
and PTY are requested. Use `-T` when passing input through a pipe. In an
interactive session, Ctrl/Alt bytes, UTF-8 input and SIGWINCH resizes are forwarded
to the remote terminal. Exit the remote shell normally, or type Enter followed
by `~.` to disconnect locally; `~~` at the start of a line sends one literal `~`.
Exec mode forwards stdout and stderr separately and returns the remote exit
status, clamped to 255; connection/authentication errors return 255. The client
drains pending output after remote `exit-status` until the channel closes.
SIGWINCH sends SSH `window-change` with the latest terminal rows/columns and
pixel dimensions. It does not create a new connection or restart the shell.

## Host identity and authentication

The first connection displays the SHA-256 fingerprint and requires the exact
answer `yes`. Verify the fingerprint with the administrator through an existing
trusted channel before accepting it. For example, the administrator can run
`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` on the server and provide that
result through the trusted channel. The accepted key is appended to
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

Ed25519, ECDSA P-256/P-384/P-521 and RSA implementations are enabled. The decoder
accepts OpenSSH private keys and unencrypted PEM PKCS#8, RSA PKCS#1 and EC SEC1
keys; unencrypted PuTTY PPK decoding is also available. Use **encrypted OpenSSH**
for the documented passphrase workflow. Encrypted PKCS#8/PPK containers can
currently fail before a passphrase prompt; convert them to OpenSSH using trusted
local tooling instead of removing their encryption. The host transport tests
cover Ed25519 key and password authentication; they do not certify every
algorithm/container combination. Identity and trust files each have a 1 MiB cap.

Keys are not generated, downloaded or copied from a desktop automatically.
Provision a dedicated key into the app's private `.ssh` directory, or use
password/MFA authentication when allowed by the server. Debug `adb run-as` file
copy is a development workflow; a production key-import UI is separate work.

## Build and APK contract

From the repository root, using the pinned Rust toolchain:

```sh
ssh_toolchain_bin="$(dirname "$(rustup which --toolchain 1.94.1 rustc)")"
export PATH="$ssh_toolchain_bin:$HOME/.cargo/bin:$PATH"
cargo build --locked --manifest-path android/ssh-client/Cargo.toml \
  --bin kokuban-ssh --target-dir target/ssh-client
target/ssh-client/debug/kokuban-ssh --help
cargo test --locked --manifest-path android/ssh-client/Cargo.toml \
  --target-dir target/ssh-client
cargo clippy --locked --manifest-path android/ssh-client/Cargo.toml \
  --target-dir target/ssh-client --all-targets -- -D warnings
```

The toolchain must already be installed. Pin both Cargo and rustc, since a
Homebrew rustc earlier in PATH can otherwise be selected.

The complete APK requires Java 17, Android SDK platform 35, build-tools 35.0.0,
NDK **27.1.12297006**, cargo-apk **0.10.0** and the selected Rust Android target.
With those prerequisites installed and `ANDROID_HOME` pointing to the SDK:

```sh
bash scripts/android/build.sh debug aarch64-linux-android --test-signing
# APK: target/debug/apk/kokuban.apk
# SSH ELF: target/ssh-client/aarch64-linux-android/debug/kokuban-ssh
adb install -r target/debug/apk/kokuban.apk
adb shell am start -n com.kokuban.terminal/.KokubanActivity
```

Use `x86_64-linux-android` for the x86_64 emulator, or replace `debug` with
`release` for the optimized APK. `--test-signing` uses a local test key and is
for development. Distribution signing is configured through the existing
`CARGO_APK_RELEASE_KEYSTORE` and `CARGO_APK_RELEASE_KEYSTORE_PASSWORD` variables.
See [Android build and validation](../../docs/ANDROID.md) for SDK/device setup.

For an isolated SSH cross-build using the same NDK wrapper:

```sh
export ANDROID_NDK_ROOT="$ANDROID_HOME/ndk/27.1.12297006"
RUSTFLAGS='-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384' \
  cargo apk -- build --locked --manifest-path android/ssh-client/Cargo.toml \
  --bin kokuban-ssh --target aarch64-linux-android --target-dir target/ssh-client
```

The APK wrapper applies the same 16 KiB page-alignment flags, checks ELF LOAD
alignment, adds the SSH executable and Android bridge bytecode, and signs the
final package. Use the complete wrapper when producing an installable APK.

The Android packaging script builds the `kokuban-ssh` **PIE executable**, copies
it into APK `lib/<abi>/libkokuban_ssh.so`, then aligns and signs the APK. The `.so`
name is Android packaging convention: this file is executed as a program, not
loaded with `dlopen`. The APK must use `android:extractNativeLibs="true"`.
At runtime the shell's `ssh` function invokes the extracted executable in
`ApplicationInfo.nativeLibraryDir`. Do not copy the executable into `$HOME` and
try to execute it there: Android's application-data execution restrictions apply.
The package requires Android API 26 or newer and INTERNET permission.

Inspect the locked dependency graph with
`cargo tree --locked --manifest-path android/ssh-client/Cargo.toml -e normal`.
The lock currently includes `ssh-key 0.7.0-rc.11`, Tokio 1.53.1 and RustCrypto
dependencies in addition to russh/ring. It contains no aws-lc, OpenSSL or libssh
backend. This inventory covers the SSH executable; the complete app also uses
Android's window/input APIs and its small Java-to-Rust OS bridge.

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
[docs/ANDROID.md](../../docs/ANDROID.md). The reproducible Android SSH smoke is
`python3 scripts/android/ssh_smoke.py --serial emulator-5554` on an isolated Linux
runner with a launched debug APK, an emulator, noninteractive sudo, OpenSSH
server, Neovim, tmux, fzf, Git and Rust. Its default `10.0.2.2` host address is the
emulator's route to the runner, not a physical phone's address. The script writes
results under `target/android-evidence/ssh`; its existence is not a passing
device result.

Normal PTYs use Tokio `AsyncFd` readiness without an idle stdin
thread. A Unix device alias that cannot be registered falls back to cancellable
10 ms waits only while its nonblocking I/O reports `WouldBlock`.

This client provides shell/exec sessions, not the full OpenSSH command suite:
no SSH agent, SSH config file, jump host, forwarding, SFTP/SCP, certificate auth,
or session reconnection is implemented. In particular, OpenSSH `-o`/`-F` options
are not accepted, and this is not yet a drop-in Git SSH transport for local Git.
Run Git, editors, multiplexers and project builds **inside the remote shell**.
They must already be installed and permitted for the remote account; their
versions and compatibility require runtime verification.

Locally, Kokuban launches Android's `/system/bin/sh` with the device's system
commands and private writable `$HOME`. The APK does not bundle Git, Cargo, a
package manager or a Linux userspace. Downloading desktop/Linux ELF binaries into
that home directory does not supply a supported executable environment.
EOF, remote exit, link failure and normal termination restore local terminal
flags; SIGKILL and device/process destruction cannot run Rust destructors.
