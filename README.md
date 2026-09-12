# 黒板 Kokuban

A native terminal emulator written in Rust for Linux and macOS.

[Download v0.1](https://github.com/guicybercode/kokuban.rs/releases/tag/v0.1) · [Run Kokuban](#installation) · [MIT license](LICENSE) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md) · [About](ABOUT.md)

![Kokuban brand](docs/screenshots/kokuban-brand.png)


Kokuban is a from-scratch terminal emulator with a Metal GPU renderer on macOS and a software rasterizer on Linux. It includes its own VT/ANSI parser, PTY handling, glyph rendering, pane management, and graphics protocol support.

An Ubuntu/Xvfb release run measured a 7.98 MiB executable and 11.93 MiB idle RSS. A short 320×180 video test observed all 72 source frames with 8.75% of one CPU core used by Kokuban. These are bounded CI scenarios, not hardware-independent guarantees; see the [resource measurements and reproduction steps](docs/LINUX_PERFORMANCE.md).

## Features

- **Native rendering**: Metal GPU renderer on macOS; software rasterizer with winit + softbuffer on Linux
- **Built-in parser**: VT/ANSI escape sequence parser with support for complex SGR modes (faint, conceal, styled underlines)
- **Graphics protocols**: Static Kitty PNG/RGB/RGBA images and Sixel on macOS and Linux; Linux also supports native Kitty animation with a bounded CPU cache
- **Pane management (macOS)**: Split windows vertically or horizontally, navigate with vim-style keybinds
- **Zoom (macOS)**: Dynamic font size adjustment per session
- **Selection and clipboard**: Mouse-driven selection on macOS and Linux; Linux copy/paste shortcuts with an asynchronous system clipboard
- **Status bar (macOS)**: Shows shell, working directory, and pane index
- **Prompt marks (macOS)**: Visual indicators for command boundaries with navigation shortcuts
- **Configuration**: TOML-based config file with font, color, and keybind customization

## Platform Support

- **macOS**: Metal GPU renderer; release archives for Apple Silicon and Intel
- **Linux**: Software rasterizer with X11/Wayland via winit

Android APK builds and emulator validation are being developed on the separate `codex/android-native` branch; Android is not yet integrated into `main`. Windows is not supported. The crate will fail to compile on unsupported platforms. See the [delivery roadmap](docs/ROADMAP.md), [Android integration handoff](docs/ANDROID_SHARED_INTEGRATION.md) and [second-session prompt](docs/SECOND_SESSION_PROMPT.md) for the remaining work and evidence.

## Installation

### Run a downloaded release

The [v0.1 release](https://github.com/guicybercode/kokuban.rs/releases/tag/v0.1) is an early desktop release. Download the archive for your computer and its matching `.sha256` file. Rust is not required to run the binary.

| Computer | Archive |
| --- | --- |
| Linux x86_64, built on Ubuntu 24.04 | `kokuban-v0.1-x86_64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `kokuban-v0.1-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `kokuban-v0.1-x86_64-apple-darwin.tar.gz` |

The Linux binary requires compatible glibc and system libraries; Ubuntu 24.04 is the tested distribution. Build from source for another architecture or an older distribution. macOS binaries target macOS 11 at build time. Release builds and automated Rust tests run on macOS 15 CI runners; graphical compatibility with older systems is not established. Archives contain standalone executables, not signed or notarized `.app` bundles. If macOS blocks a download, review it through Privacy & Security or build from source; do not disable system-wide protections.

**Linux (Ubuntu 24.04):** install the runtime libraries and a monospace font:

```sh
sudo apt update
sudo apt install --yes libfontconfig1 libfreetype6 fonts-dejavu-core \
  libx11-6 libxcursor1 libx11-xcb1 libxi6 libxkbcommon-x11-0
```

In the directory containing the downloaded archive and checksum:

```sh
sha256sum --check kokuban-v0.1-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf kokuban-v0.1-x86_64-unknown-linux-gnu.tar.gz
cd kokuban-v0.1-x86_64-unknown-linux-gnu
./kokuban
```

**macOS (Apple Silicon):** in the download directory:

```sh
shasum -a 256 --check kokuban-v0.1-aarch64-apple-darwin.tar.gz.sha256
tar -xzf kokuban-v0.1-aarch64-apple-darwin.tar.gz
cd kokuban-v0.1-aarch64-apple-darwin
./kokuban
```

On Intel Macs, replace `aarch64-apple-darwin` with `x86_64-apple-darwin` in those commands. Verify that the checksum reports `OK` before extraction. The archives include the project license and third-party license notices/source required for distribution.

Run Kokuban from a graphical desktop session. A new terminal window should open with your login shell. Type `pwd` or `echo hello` to try it, and `exit` to end the shell session. SSH, editors and media players are external programs: for example, type `ssh user@host` inside Kokuban to connect using your installed SSH client.

### Build and run from source

Install Git and Rust through [rustup](https://rustup.rs/). The verified toolchain is **Rust 1.94.1**.

On Linux, install the runtime dependencies above plus the build tools:

```sh
sudo apt install --yes build-essential pkg-config libfontconfig1-dev libfreetype6-dev
```

On macOS, install Xcode Command Line Tools if needed:

```sh
xcode-select --install
```

Then build the release source and launch it:

```sh
git clone --branch v0.1 --depth 1 https://github.com/guicybercode/kokuban.rs.git
cd kokuban.rs
rustup toolchain install 1.94.1 --profile minimal
rustup run 1.94.1 cargo build --release --locked
./target/release/kokuban
```

To work on the development version, clone `main` instead; see [CONTRIBUTING.md](CONTRIBUTING.md). The executable is `target/release/kokuban`. You can select a different shell with an absolute executable path:

```sh
KOKUBAN_SHELL=/bin/bash ./target/release/kokuban
```

Kokuban selects `KOKUBAN_SHELL`, then `SHELL`, then `/bin/sh`. It reads `kokuban.toml` from the directory where you launch it. Development builds also search the user configuration directory described below. For Linux, a minimal file can select the installed font:

```toml
[font]
family = "DejaVu Sans Mono"
size = 14.0

[images.kitty]
allow_file_transfer = false
```

### Omarchy and Linux commands (development builds)

Build `main` to use Omarchy's terminal launcher, working-directory inheritance,
and commands opened directly in the terminal:

```sh
kokuban --working-directory="$HOME" -e nvim
kokuban --app-id=org.omarchy.btop -e btop
kokuban --help
```

See [Omarchy installation and bindings](docs/OMARCHY.md) to register Kokuban with
`xdg-terminal-exec`. Command arguments after `-e` are passed unchanged; use
`sh -c` explicitly for pipelines or shell expansion. CI checks command launching
under X11 and headless Weston/Wayland; Hyprland integration still needs an
on-device run.

### Troubleshooting startup

- **No Linux display:** start it inside an X11 or Wayland desktop session. A headless SSH session without a display cannot open the window. X11 has automated runtime coverage; Wayland remains less validated.
- **Missing Linux library:** install the runtime packages above. Libraries opened dynamically may not appear in `ldd` output.
- **Missing font:** install `fonts-dejavu-core` on Linux and use the font configuration above. macOS includes Menlo.
- **Shell fails to start:** set `KOKUBAN_SHELL` to an absolute, executable shell path without command-line arguments.
- **Configuration seems ignored:** a local `kokuban.toml` takes priority. Builds from `main` also read `${XDG_CONFIG_HOME:-$HOME/.config}/kokuban/kokuban.toml`; published v0.1 binaries only read the local file. Invalid configuration falls back to defaults and logs a warning.

### Application icon (development builds)

Builds from `main` after v0.1 embed the [Kokuban artwork](assets/kokuban-icon.png).
It appears in the macOS Dock while the application runs and as the Linux X11
window icon. The published v0.1 binaries predate this addition. A standalone
macOS executable retains its ordinary Finder file icon.

For a Linux application-menu launcher and Wayland desktop icon, build `main`
and run the following from the repository root. This installs into your user
account; `desktop-file-utils` provides `desktop-file-install`:

```sh
sudo apt install --yes desktop-file-utils
kokuban_data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
install -Dm755 target/release/kokuban "$HOME/.local/bin/kokuban"
install -Dm644 assets/kokuban-icon.png "$kokuban_data_dir/kokuban/kokuban-icon.png"
mkdir -p "$kokuban_data_dir/applications"
desktop-file-install --dir="$kokuban_data_dir/applications" \
  --set-key=Exec --set-value="\"$HOME/.local/bin/kokuban\"" \
  --set-key=Path --set-value="$HOME" \
  --set-icon="$kokuban_data_dir/kokuban/kokuban-icon.png" \
  assets/io.github.guicybercode.kokuban.desktop
```

Launch **Kokuban** from the application menu. The launcher starts in your home
directory, so a `kokuban.toml` there supplies its configuration. Wayland icon
display depends on the compositor's desktop-entry support.

### Trying images

Run these commands **inside a Kokuban terminal**, from the repository directory:

```bash
cargo run --example graphics -- kitty
cargo run --example graphics -- sixel
cargo run --example graphics -- /path/to/photo.png
cargo run --example graphics -- stream
cargo run --example graphics -- animate  # Linux
```

The Rust example sends PNG images in Kitty chunks or a Sixel color pattern. `stream` replaces the same image for 120 frames with a requested rate of 30 FPS. `animate` uploads 30 frames and exits; Linux continues playback for three loops using Kitty animation controls. These examples are not performance benchmarks. Linux also plays video through external mpv's direct Kitty output; a 320×180 lossless clip, pause/resume and all 72 visible frames passed the [real-player test](docs/LINUX_VIDEO.md). Audio and sustained playback remain unverified. The macOS renderer returns `ENOTSUP` for animation commands.

Images follow terminal scrolling. Image placements crossing a partial scroll region are discarded to avoid painting over fixed text. The Linux cache limits decoded bytes, image count (4096), retained frames across the cache (4096), and frames per image (256). Animation frames use complete RGBA canvases, including delta uploads, and count against the byte limit. Snapshots share pixel buffers, so concurrent upload and rendering can temporarily retain more memory than the cache limit. Hidden animations schedule no timer and catch up when visible again. See the [shared animation contract](docs/ANIMATION.md). Configuration applies at startup:

```toml
[images]
enabled = true
max_memory_mb = 64
kitty_enabled = true
sixel_enabled = true

[images.kitty]
max_image_size_mb = 16
allow_file_transfer = false
```

These are example limits; defaults remain 256 MiB for the cache and 50 MiB per Kitty upload. `allow_file_transfer = false` keeps transfers in the terminal byte stream, including over SSH. Set `images.enabled = false` to disable both image protocols. Kokuban starts the shell configured through `KOKUBAN_SHELL` or `SHELL`; SSH on Linux runs through the system's `ssh` command in that shell.

## Configuration

Development builds read `kokuban.toml` in the launch directory first, then
`$XDG_CONFIG_HOME/kokuban/kokuban.toml`, or `$HOME/.config/kokuban/kokuban.toml`
when `XDG_CONFIG_HOME` is unset, empty, or relative. The first existing file wins;
files are not merged. Configuration is loaded before the CLI working directory
is applied. Published v0.1 binaries only read the local file. Missing, unreadable
or invalid configuration falls back to defaults; parse/read errors are logged.
Review local configuration before launching from an unfamiliar directory.

In development builds, omitting `font.family` uses the system monospace font on
Linux and Menlo on macOS. An explicit family overrides that choice. Open a new
window after changing the system font or Kokuban's font settings.

Example configuration:

```toml
[font]
# Optional explicit font; omit to keep the platform default.
# family = "DejaVu Sans Mono"
size = 14.0
zoom_step = 1.0

[window]
columns = 80
rows = 24
opacity = 1.0

[colors]
foreground = "#c0c0c0"
background = "#1a1a2e"

[selection]
foreground = "#000000"
background = "#b4d5fe"

[status_bar]
enabled = true
show_shell = true
show_cwd = true

[prompt_marks]
enabled = true
show_indicator = true
indicator_color = "#b5312c"

[keybind]
split_vertical = "cmd+d"
split_horizontal = "cmd+shift+d"
close_pane = "cmd+w"
focus_left = "cmd+h"
focus_down = "cmd+j"
focus_up = "cmd+k"
focus_right = "cmd+l"
zoom_in = "cmd+="
zoom_out = "cmd+-"
zoom_reset = "cmd+0"
```

See `kokuban.toml` in the repository root for an example configuration; omitted settings use the defaults defined in `src/config.rs`.

## Keybinds

Linux supports these clipboard and selection actions:

| Action | Linux input |
|--------|-------------|
| Select text | Left-button drag |
| Select text while an application captures the mouse | `Shift` + left-button drag |
| Copy selection | `Ctrl+Shift+C` or `Ctrl+Insert` |
| Paste clipboard | `Ctrl+Shift+V` or `Shift+Insert` |
| Select all retained text | `Ctrl+Shift+A` |
| Scroll history | `Shift+PageUp` / `Shift+PageDown` |
| Oldest/newest retained view | `Shift+Home` / `Shift+End` |

Ordinary `Ctrl+C` and `Ctrl+V` remain application input. Paste honors bracketed-paste mode, normalizes line endings, removes embedded control characters, and rejects oversized text instead of truncating it. Copy and encoded paste are limited to 1 MiB. Clipboard access runs in the background; accepted repeated paste requests retain their order.

Omarchy maps `Super+C` and `Super+V` to the supported Insert shortcuts; see [Omarchy clipboard bindings](docs/OMARCHY.md#use-the-existing-omarchy-bindings) for the integration and validation scope.

The Linux clipboard works through X11 or a Wayland compositor exposing a data-control protocol. Other Wayland desktops need XWayland clipboard access. Copy joins automatically wrapped rows, preserves explicit line breaks and copies complete graphemes. Resize reflows retained text without cropping it. See [text preservation](docs/TERMINAL_TEXT.md) and [Linux application validation](docs/LINUX_APPS.md). OSC 52 remote clipboard commands remain pending.

These pane, zoom and prompt-navigation keybinds currently apply to macOS. Linux provides terminal keyboard/IME and mouse input and scrollback; it does not yet implement this pane shortcut table.

| Action              | Keybind             |
|---------------------|---------------------|
| Split vertical      | `Cmd+D`             |
| Split horizontal    | `Cmd+Shift+D`       |
| Close pane          | `Cmd+W`             |
| Focus left pane     | `Cmd+H`             |
| Focus down pane     | `Cmd+J`             |
| Focus up pane       | `Cmd+K`             |
| Focus right pane    | `Cmd+L`             |
| Resize left         | `Cmd+Shift+H`       |
| Resize down         | `Cmd+Shift+J`       |
| Resize up           | `Cmd+Shift+K`       |
| Resize right        | `Cmd+Shift+L`       |
| Zoom in             | `Cmd+=`             |
| Zoom out            | `Cmd+-`             |
| Reset zoom          | `Cmd+0`             |
| Previous prompt     | `Cmd+↑`             |
| Next prompt         | `Cmd+↓`             |

The pane, resize, zoom and prompt-navigation bindings in the macOS table are configurable. Linux clipboard and scrollback shortcuts use their fixed input routes. On macOS, `Cmd+C` copies a selection or sends Ctrl+C when there is none; `Cmd+V` pastes.

## Development Status

Kokuban v0.1 (Cargo version 0.1.0) is an early desktop release. Linux tests cover SSH with Neovim, tmux and fzf, clipboard, images, animation and short mpv playback. Terminal text supports extended graphemes, soft-wrap reconstruction and resize reflow. OSC 52, broad Wayland coverage, audio and sustained media playback remain incomplete or unverified. Android is developed separately and is not shipped in this release. The application is written in Rust, but uses native platform APIs and font libraries; low memory, CPU and battery consumption must be measured rather than inferred from the language.

## Project Name

黒板 (*kokuban*) is the Japanese word for "blackboard" or "chalkboard"—a blank surface for writing and drawing.

## Contributing, security and license

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and checks. Send vulnerabilities through the private channel in [SECURITY.md](SECURITY.md); ordinary bugs can use [GitHub issues](https://github.com/guicybercode/kokuban.rs/issues).

Kokuban's original code and documentation are licensed under [MIT](LICENSE). Dependencies and third-party assets retain their own licenses; release archives include their notices. Learn more in [ABOUT.md](ABOUT.md) and [third-party licensing](docs/THIRD_PARTY.md).
