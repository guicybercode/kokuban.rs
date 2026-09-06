# Android validation tools

Run these commands from the repository root. Application instructions and current
delivery status belong in [`docs/ANDROID.md`](../../docs/ANDROID.md).

## Pinned build

Requirements: Rust 1.94.1, cargo-apk 0.10.0, NDK 27.1.12297006, Android platform
35, build tools, Java 17, Python 3, and an Android device/emulator.

```sh
rustup target add --toolchain 1.94.1 aarch64-linux-android
rustup run 1.94.1 cargo install cargo-apk --version 0.10.0 --locked
scripts/android/build.sh debug aarch64-linux-android
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

`--test-signing` uses a generated development key under the ignored `target/`
directory. These APKs are for tests. For distribution omit that flag and supply
`CARGO_APK_RELEASE_KEYSTORE` and `CARGO_APK_RELEASE_KEYSTORE_PASSWORD` securely.
Never commit a private signing key. Build ARM64 and x86_64 into separate target
directories if their artifacts need to coexist; the APK basename is shared.

## Real PTY and lifecycle smoke

```sh
python3 scripts/android/smoke.py --serial emulator-5554 \
  --apk target/debug/apk/kokuban.apk
```

This installs the APK, types a shell command, and verifies its private output
through `run-as`. A shell variable also proves the same shell survives HOME,
resume, and repeated rotation. Screenshots, device identity, and logs are written
under `target/android-evidence/smoke/`. The script restores rotation settings.
Use an isolated test device: installation replaces the matching test package and
the script force-stops it before launching. This requires a debuggable build.

The smoke test injects key events through adb; it does **not** prove composition
or any other behavior of a real IME. If a test fails, inspect its screenshots and
logcat before rerunning. Logs/screenshots from personal devices may include private
content; review evidence before attaching it to a public issue or PR.

## Real IME, touch and external keyboard checks

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

Arrange a scenario in the release application, then run:

```sh
python3 scripts/android/measure.py --scenario release-idle --seconds 20 \
  --apk target/release/apk/kokuban.apk \
  --output target/android-evidence/release-idle
```

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
