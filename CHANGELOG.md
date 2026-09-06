# Changelog

## v0.1 — 2026-09-06

First public desktop release of Kokuban. The Git tag is `v0.1`; the Cargo package version is `0.1.0`. This is early-stage software with the platform limits below.

### Included

- A native Rust terminal with VT/ANSI parsing, a scrollback grid, PTY processes and TOML configuration.
- Metal rendering, pane management and font zoom on macOS.
- Software rendering on Linux, keyboard/IME input, mouse reporting, selection and an asynchronous clipboard.
- Static Kitty RGB/RGBA/PNG and Sixel images on desktop; bounded native Kitty animation on Linux.
- Linux playback through external mpv's direct Kitty output, with real-frame, pause/resume and EOF checks.
- Verified Linux SSH workflows with Neovim, tmux and fzf. The system SSH client and development tools are external programs.
- Release measurements for idle memory/CPU, finite terminal output and short video playback, with reproducible evidence in `docs/`.

### Project and distribution

- MIT license for Kokuban's original code and documentation, package metadata and a GitHub About description/topics.
- Security policy with private GitHub vulnerability reporting and a contribution guide.
- Downloadable Linux x86_64 and macOS Apple Silicon/Intel archives, matching SHA-256 checksums, build information and third-party notices/source.
- README instructions for downloading, checking, extracting, building and running the application, including the actual launch-directory configuration behavior.

### Known limits

- Linux archives are built on Ubuntu 24.04 against its system libraries. Other distributions may need a source build; X11 is the runtime-tested Linux backend.
- macOS archives are standalone executables without Developer ID signing or notarization. The build target is macOS 11; builds and automated Rust tests use macOS 15, without an end-to-end graphical compatibility claim for older versions.
- Android remains on the separate development branch and is not included. Windows is unsupported.
- Complete Unicode graphemes, soft-wrap reconstruction and OSC 52 are not implemented. Wayland clipboard access depends on a data-control protocol or XWayland.
- Native animation is Linux-only. Video checks use a short silent 320×180 clip; audio, automatic mpv sizing and prolonged playback are not established by those tests.
- v0.1 reads only `kokuban.toml` in the current working directory. Kitty file transfer is enabled by default; see [SECURITY.md](SECURITY.md) for configuration and reporting.

See [README.md](README.md) for setup, [Linux application checks](docs/LINUX_APPS.md), [video evidence](docs/LINUX_VIDEO.md) and [release measurements](docs/LINUX_PERFORMANCE.md) for the scope of validation.
