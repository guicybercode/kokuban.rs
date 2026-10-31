# Installation

This page keeps the full installation reference that used to live in the README: the release table, dependencies, checksum verification, updating, building from source, Omarchy commands, troubleshooting, the Linux desktop entry and the image examples. The README has the three-line summary.

## Run a downloaded release

The [v0.4 release](https://github.com/guicybercode/kokuban.rs/releases/tag/v0.4) is an early desktop release. Download the archive (or, on a Mac, the disk image) for your computer and its matching `.sha256` file. Neither Rust nor Python is required to run the binary. Python is used only by development tools retained in the source repository; those tools and raw benchmark snapshots are omitted from the application archives.

| Computer | Archive |
| --- | --- |
| Linux x86_64, built on Ubuntu 24.04 | `kokuban-v0.4-x86_64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `kokuban-v0.4-aarch64-apple-darwin.dmg` or `.tar.gz` |
| macOS Intel | `kokuban-v0.4-x86_64-apple-darwin.dmg` or `.tar.gz` |

The Linux binary requires compatible glibc and system libraries; Ubuntu 24.04 is the tested distribution. Build from source for another architecture or an older distribution. macOS binaries target macOS 11 at build time. Release builds and automated Rust tests run on macOS 15 CI runners; graphical compatibility with older systems is not established. Archives contain standalone executables; disk images contain `Kokuban.app`, ad-hoc signed but not Developer ID signed or notarized. If macOS blocks a download, review it through Privacy & Security or build from source; do not disable system-wide protections.

**Linux (Ubuntu 24.04):** install the runtime libraries and a monospace font:

```sh
sudo apt update
sudo apt install --yes libfontconfig1 libfreetype6 fonts-dejavu-core \
  libx11-6 libxcursor1 libx11-xcb1 libxi6 libxkbcommon-x11-0
```

In the directory containing the downloaded archive and checksum:

```sh
sha256sum --check kokuban-v0.4-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf kokuban-v0.4-x86_64-unknown-linux-gnu.tar.gz
cd kokuban-v0.4-x86_64-unknown-linux-gnu
./kokuban
```

**macOS (Apple Silicon):** in the download directory:

```sh
shasum -a 256 --check kokuban-v0.4-aarch64-apple-darwin.tar.gz.sha256
tar -xzf kokuban-v0.4-aarch64-apple-darwin.tar.gz
cd kokuban-v0.4-aarch64-apple-darwin
./kokuban
```

**macOS app:** check the disk image, open it and drag **Kokuban** to **Applications**:

```sh
shasum -a 256 --check kokuban-v0.4-aarch64-apple-darwin.dmg.sha256
open kokuban-v0.4-aarch64-apple-darwin.dmg
```

The first launch of a downloaded app may need **Open** from the Finder context menu or approval in Privacy & Security.

On Intel Macs, replace `aarch64-apple-darwin` with `x86_64-apple-darwin` in those commands. Verify that the checksum reports `OK` before extraction. The archives include the project license and third-party license notices/source required for distribution.

Run Kokuban from a graphical desktop session. A new terminal window should open with your login shell. Type `pwd` or `echo hello` to try it, and `exit` to end the shell session. SSH, editors and media players are external programs: for example, type `ssh user@host` inside Kokuban to connect using your installed SSH client.

## Updating

Starting with v0.4, Kokuban checks the [latest GitHub release](https://github.com/guicybercode/kokuban.rs/releases/latest) in the background when it opens. When a newer version exists, macOS shows `vX available` at the right of the status bar and Linux adds it to the window title. Nothing is downloaded until you ask:

```sh
kokuban --check-update   # report whether a newer release exists
kokuban --update         # download, verify and install it
```

On macOS the app menu also has **Check for Updates…** and **Install Update…**. The update downloads the archive for your computer (or, for `Kokuban.app`, the disk image) with its `.sha256`, refuses it unless the checksum matches, and replaces the executable or app bundle in place. Reopen Kokuban afterwards. If the executable lives in a directory you cannot write, such as `/usr/local/bin`, run `sudo kokuban --update`. Kokuban links no network library: checks and downloads run the system `curl`, `tar` and `shasum`/`sha256sum` (plus `hdiutil` for the app bundle).

Turn the automatic check off with `check_on_startup = false` under `[update]` in `kokuban.toml`, or set `KOKUBAN_NO_UPDATE_CHECK=1`.

## Build and run from source

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
git clone --branch v0.4 --depth 1 https://github.com/guicybercode/kokuban.rs.git
cd kokuban.rs
rustup toolchain install 1.94.1 --profile minimal
rustup run 1.94.1 cargo build --release --locked
./target/release/kokuban
```

To work on the development version, clone `main` instead; see [CONTRIBUTING.md](../CONTRIBUTING.md). The executable is `target/release/kokuban`. You can select a different shell with an absolute executable path:

```sh
KOKUBAN_SHELL=/bin/bash ./target/release/kokuban
```

Kokuban selects `KOKUBAN_SHELL`, then `SHELL`, then `/bin/sh`. It searches the launch directory and then the user configuration directory described below. This minimal configuration keeps the platform's default font and disables Kitty file transfers:

```toml
[font]
# family = "DejaVu Sans Mono" # Optional explicit font.
size = 14.0

[images.kitty]
allow_file_transfer = false
```

## Omarchy and Linux commands

Kokuban v0.4 supports Omarchy's terminal launcher, working-directory inheritance,
and commands opened directly in the terminal:

```sh
kokuban --working-directory="$HOME" -e nvim
kokuban --app-id=org.omarchy.btop -e btop
kokuban --help
```

See [Omarchy installation and bindings](OMARCHY.md) to register Kokuban with
`xdg-terminal-exec`. Command arguments after `-e` are passed unchanged; use
`sh -c` explicitly for pipelines or shell expansion. CI checks command launching
under X11 and headless Weston/Wayland; Hyprland integration still needs an
on-device run.

On Linux, Omarchy palette changes update existing windows while preserving the
shell session. Explicit Kokuban colors take priority. The default font follows
the system monospace alias; open a new window after changing the system font.
See the [palette and font behavior](OMARCHY.md#follow-the-omarchy-palette)
before copying explicit colors or a font family into your configuration.

## Troubleshooting startup

- **No Linux display:** start it inside an X11 or Wayland desktop session. A headless SSH session without a display cannot open the window. X11 has automated runtime coverage; Wayland remains less validated.
- **Missing Linux library:** install the runtime packages above. Libraries opened dynamically may not appear in `ldd` output.
- **Missing font:** install `fonts-dejavu-core` on Linux. Install `fonts-noto-color-emoji` for the emoji fallback exercised by CI. macOS includes Menlo and Apple Color Emoji.
- **Shell fails to start:** set `KOKUBAN_SHELL` to an absolute, executable shell path without command-line arguments.
- **Configuration seems ignored:** a local `kokuban.toml` takes priority over the user configuration file. Settings are read when a window opens; the Omarchy palette has its own live reload. Invalid configuration falls back to defaults and logs a warning.

## Application icon

Kokuban v0.4 embeds the [Kokuban artwork](../assets/kokuban-icon.png).
It appears in the macOS Dock while the application runs and as the Linux X11
window icon. A standalone macOS executable retains its ordinary Finder file icon.

For a Linux application-menu launcher and Wayland desktop icon, build from source
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

## Trying images

Run these commands **inside a Kokuban terminal**, from the repository directory:

```bash
cargo run --example graphics -- kitty
cargo run --example graphics -- sixel
cargo run --example graphics -- /path/to/photo.png
cargo run --example graphics -- stream
cargo run --example graphics -- animate  # Linux
```

The Rust example sends PNG images in Kitty chunks or a Sixel color pattern. `stream` replaces the same image for 120 frames with a requested rate of 30 FPS. `animate` uploads 30 frames and exits; Linux continues playback for three loops using Kitty animation controls. These examples are not performance benchmarks. Linux also plays video through external mpv's direct Kitty output; a 320×180 lossless clip, pause/resume and all 72 visible frames passed the [real-player test](LINUX_VIDEO.md). Audio and sustained playback remain unverified. The macOS renderer returns `ENOTSUP` for animation commands.

Images follow terminal scrolling. Image placements crossing a partial scroll region are discarded to avoid painting over fixed text. The Linux cache limits decoded bytes, image count (4096), retained frames across the cache (4096), and frames per image (256). Animation frames use complete RGBA canvases, including delta uploads, and count against the byte limit. Snapshots share pixel buffers, so concurrent upload and rendering can temporarily retain more memory than the cache limit. Hidden animations schedule no timer and catch up when visible again. See the [shared animation contract](ANIMATION.md). Configuration applies at startup:

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
