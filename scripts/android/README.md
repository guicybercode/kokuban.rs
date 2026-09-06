# Android validation tools

Run these commands from the repository root. Application instructions and current
delivery status belong in [`docs/ANDROID.md`](../../docs/ANDROID.md).

## Pinned build

Requirements: Rust 1.94.1, cargo-apk 0.10.0, NDK 27.1.12297006, Android platform
35, build tools, Java 17, Python 3, and an Android device/emulator.

```sh
rustup target add --toolchain 1.94.1 aarch64-linux-android
rustup run 1.94.1 cargo install cargo-apk --version 0.10.0 --locked
scripts/android/build.sh debug aarch64-linux-android --test-signing
scripts/android/build.sh release aarch64-linux-android --test-signing
```

Set `ANDROID_HOME` when the SDK is outside `~/Library/Android/sdk`. The build
script explicitly chooses Rust compiler/doc binaries as well as Cargo, checks
the installed NDK and cargo-apk versions, fetches with `--locked`, builds offline,
and verifies the lockfile has not changed. cargo-apk 0.10.0 does not accept
`--locked` on its packaging subcommand.

The build also compiles the minimal Java OS bridge with `javac --release 8` and
Android's `d8`, then adds `classes.dex`, aligns and re-signs the APK. APK signature
verification and alignment checks must pass before the final file replaces the
packaging output. The Java bridge is required for Android's IME/InputConnection
contract; terminal parsing, rendering and application behavior remain in Rust.
The standalone Rust SSH client is built and packaged as `libkokuban_ssh.so` so
Android can execute it from the extracted native-library directory. Both native
ELFs must expose 16 KiB-aligned LOAD segments; ZIP alignment alone is insufficient.
The build appends the necessary linker flags while preserving existing flags.

Android debug builds default to optimization level 1, because unoptimized pixel
loops are unsuitable for interactive use. This setting is confined to the Android
build process. Measurements from these builds must be labeled `debug-opt1` and
must not be reported as release measurements.

`--test-signing` uses a generated development key under the ignored `target/`
directory. These APKs are for tests. For distribution omit that flag and supply
`CARGO_APK_RELEASE_KEYSTORE` and `CARGO_APK_RELEASE_KEYSTORE_PASSWORD` securely.
Never commit a private signing key. Build ARM64 and x86_64 into separate target
directories if their artifacts need to coexist; the APK basename is shared.
Use `--test-signing` for both debug and release builds to share the same test key
and allow an in-place upgrade without losing the test application's storage.

## Real PTY and lifecycle smoke

```sh
python3 scripts/android/smoke.py --serial emulator-5554 \
  --apk target/debug/apk/kokuban.apk --trace-frames
```

This installs the APK, types a shell command, and verifies its private output
through `run-as`. A shell variable also proves the same shell survives HOME,
resume, and repeated rotation. Screenshots, device identity, and logs are written
under `target/android-evidence/smoke/`. The script restores rotation settings.
Use an isolated test device: installation replaces the matching test package and
the script force-stops it before launching. This requires a debuggable build.

The key-injection helper sends eight-character chunks. Android gives a long
`input text` burst one timestamp, so its later events can become stale on a busy
emulator. Chunking models typing and avoids that harness artifact; it does not
prove acceptable input latency. Screenshots are captured before the first command
and on failure to retain evidence when shell input fails.

The smoke test injects key events through adb; it does **not** prove composition
or any other behavior of a real IME. If a test fails, inspect its screenshots and
logcat before rerunning. Logs/screenshots from personal devices may include private
content; review evidence before attaching it to a public issue or PR.

## Real IME, touch and external keyboard checks

```sh
python3 scripts/android/ime_smoke.py --serial emulator-5554 --require-preedit
python3 scripts/android/controls_smoke.py --serial emulator-5554
```

The automated scenario uses the installed Latin/English IME's actual keys and
accent popup to enter `café`, exercises deletion and Enter, and verifies the
PTY's UTF-8 output. It also requires composing and committed IME callbacks,
checks native accessibility target sizes, and hides/reopens the keyboard. XML,
screenshots and callback counts are retained for diagnosing keyboard differences.
The controls scenario checks exact PTY bytes for toolbar keys, injected keyboard
keys and modifiers, clipboard paste and SGR mouse press/release. It verifies
selection and scroll against presented geometry. These injections do not prove
compatibility with a physical USB/Bluetooth keyboard or mouse.

For script iteration, `Android input checks` reuses an APK artifact from a
recorded full Android run and runs `ime`, `controls`, or `all` without rebuilding
Rust. Its evidence records the APK run/commit/hash separately from the checkout
containing the current test scripts. A pass applies to that APK, not to newer
application code. The full Android matrix remains the application gate.

```sh
gh workflow run android-input.yml --ref codex/android-native \
  -f source_run=34003726906 -f suite=ime
```

[GitHub requires the dispatch workflow on the default branch](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow).
Before this port is merged, changes to the listed input-script PR paths trigger
the fast IME scenario using the explicitly pinned run in the workflow. Update
that pin when selecting a newer APK or when its retained artifact expires.

Use an installed IME such as the emulator's default Gboard. Record its package
and version with `adb shell settings get secure default_input_method` and
`adb shell dumpsys package <ime-package>`. Test the following on the actual soft
keyboard, retaining screenshots before and after commit:

1. Open and close the keyboard through the terminal's touch control. Confirm
   terminal rows follow the usable content area and the prompt remains visible.
2. In the terminal run `cat > "$HOME/ime.txt"`. Type `ação café` with the IME,
   using a composition candidate and an accented character from long press.
   Delete within composition, correct it, and commit. Press Enter and the
   terminal's Ctrl-D control. Read `files/home/ime.txt` with `adb exec-out run-as
   com.kokuban.terminal cat files/home/ime.txt` and compare its UTF-8 bytes to the
   entered text. Do not use `adb shell input text` to supply these characters.
3. Test Enter, Backspace, Esc, Tab, arrows, and Ctrl-C through touch controls.
   Repeat with a connected physical keyboard. Confirm Ctrl-C stops a foreground
   process while the shell stays usable.
4. Generate scrollback, swipe in both directions, select text, copy it, then
   paste at the prompt. Check multi-line paste, Unicode, and selection across a
   wrapped line. Confirm external mouse selection and scrolling still work.
5. Suspend/resume and rotate while the keyboard and a composition are active.
   Verify the composition/commit behavior explicitly; process survival alone is
   insufficient.

The NativeActivity APIs in android-activity 0.6.1 return an empty text-input state
and mark text-state/editor-info setters unsupported. A real IME test must prove
the application's Android bridge supplies the missing composition behavior.

## Measurements

The smoke script's `--trace-frames` creates the opt-in flag before application
launch. That private flag survives a same-key release upgrade. Timing traces
correlate application input with the next PTY-output presentation; unrelated
output can satisfy the correlation. Neither these traces nor `gfxinfo` prove
hardware input-to-scanout latency.

Arrange a scenario in the release application, then run:

```sh
python3 scripts/android/measure.py --scenario release-idle --seconds 20 \
  --settle-seconds 3 \
  --apk target/release/apk/kokuban.apk \
  --output target/android-evidence/release-idle
```

Use `--probe-echo` in an otherwise idle shell for a separate controlled-input
scenario. It injects short `echo` commands while sampling CPU and requires at
least one application input-to-output timing sample. It does not substitute adb
round-trip time for input latency.
The collector records the last observed frame before sampling and excludes it
and earlier frames, because logcat's wall-clock boundary may include earlier
events from the same second. Idle CI scenarios settle for three seconds first.

## SSH and release device validation

On an isolated Linux runner with passwordless sudo, OpenSSH server, Neovim,
tmux, fzf, Git and Rust 1.94.1:

```sh
python3 scripts/android/ssh_smoke.py --serial emulator-5554
scripts/android/build.sh release x86_64-linux-android --test-signing
python3 scripts/android/ssh_smoke.py --serial emulator-5554 \
  --upgrade-apk target/release/apk/kokuban.apk \
  --output target/android-evidence/release-ssh
python3 scripts/android/launch.py --serial emulator-5554 \
  --apk target/release/apk/kokuban.apk
```

The fixture creates an ephemeral sshd with directly verified host-key pins and
temporary client keys. Unknown and changed hosts must fail before executing a
command. Real PTY sessions edit a Rust project in Neovim, use two tmux panes,
select an item with fzf, inspect Git changes, compile/run the edited project,
propagate Android rotation to the remote PTY, and return to the local shell.
The same SSH session displays a pinned NASA photograph, Sixel, native Kitty
animation, and an H.264 video decoded by host FFmpeg. Screenshot pixels are
checked against the photo and animation fixtures; video samples must change in
the image region. Producer frame counts and Android presentation-call timing
are reported separately, with CPU/PSS samples during video.

With `--upgrade-apk`, trust checks and fixture provisioning run in debug first;
interactive tests run after an upgrade to the non-debuggable release APK. Both
APKs must share `--test-signing`. At cleanup, the terminal deletes test keys,
then the harness restores the debug APK to independently verify their removal.
`launch.py` reinstalls release for subsequent measurements without clearing data.
Private credentials remain under ignored `target/android-private`, never in the
uploaded evidence directory, and are deleted after the fixture exits.

Repeat in distinct output directories for a photo, animation, video, active text
output and an interactive development application. Keep the device, orientation,
IME visibility, build type, media dimensions/rate and workload constant when
comparing. Include a settling interval before the idle sample. Emulator guest
CPU is not physical-device efficiency and does not include host emulator costs.

The report includes APK and uncompressed/compressed native-library sizes, PSS
before/after, and samples from Android `top` where 100% means one CPU core. The
first `top` sample is discarded because it spans an unspecified prior interval.
Raw sources are retained. Missing data remains `null`; it is never called zero.
Main-process metrics and the combined process tree (including shell and SSH
children) are reported separately. If processes enter or exit during sampling,
the combined CPU value is unavailable rather than implying a complete reading.

`gfxinfo` is captured for investigation, but NativeActivity/softbuffer may not
populate View frame metrics. End-to-end input latency and presentation cadence
require correlated application tracing or external measurement. The tool does
not mislabel adb round-trip latency as terminal input latency.

Run tooling checks with:

```sh
bash -n scripts/android/build.sh
python3 -m unittest discover -s scripts/android -p 'test_*.py'
```
