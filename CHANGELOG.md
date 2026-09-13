# Changelog

## v0.2 — 2026-09-13

Desktop release focused on text preservation, Omarchy integration and measured performance work. The Git tag is `v0.2`; the Cargo package version is `0.2.0`.

### Added

- Complete Unicode grapheme storage and shaping, including combining characters, flags, ZWJ sequences and installed color-emoji font fallback on Linux and macOS.
- Logical-line copying that preserves explicit breaks and joins automatic wraps; resize reflow across retained text and history, with text-aware cursor restoration.
- Persistent XDG configuration after the launch-directory `kokuban.toml` lookup.
- Linux command launching with `-e`/`--execute`/`--`, working-directory, title and app-ID options, plus a desktop entry declaring `xdg-terminal-exec` capabilities.
- Omarchy live palette updates with explicit configuration overrides, invalid-theme recovery and no shell restart. Linux uses the system monospace font by default; explicit fonts remain supported.
- `Ctrl+Insert` copy alongside `Ctrl+Shift+C`, matching Omarchy's universal copy mapping. Existing paste shortcuts and ordinary `Ctrl+C`/`Ctrl+V` application input are preserved.
- Kokuban artwork in the macOS Dock and Linux X11 window, plus a matching Linux application-menu launcher.
- Community code of conduct, updated security guidance, citation metadata and a Portuguese release announcement.

### Changed and fixed

- Batch ASCII and mixed UTF-8 processing, reduce repeated Unicode boundary/width work and glyph-cache allocations, and scroll full-screen rows through a circular origin.
- Repaint changed Linux frame bands with buffer-age tracking; reuse row metadata and bulk pixel operations where applicable.
- Block idle PTY reads until output or explicit shutdown instead of waking periodically.
- Keep macOS selection aligned with complete graphemes; safely truncate Unicode status paths and reject malformed Unicode color values without invalid string slicing.
- Expand regression coverage for Unicode copy after resize, X11/Wayland launching, live theme pixels, retained PTY sessions and system-font selection. Theme tests wait for actual font/window geometry before printing their fixture.
- Add paired revision and cross-terminal benchmark tooling with verified build identities and cell geometry. Retain gains and regressions; revert the history-row reuse experiment after a measured throughput regression.

### Validation scope and limits

- Linux launch checks cover X11 and headless Weston/Wayland; clipboard, theme and other graphical interaction checks use X11. Physical Omarchy/Hyprland validation and broad Wayland interaction remain pending.
- Linux archives are built on Ubuntu 24.04. macOS archives remain unsigned, unnotarized standalone executables with a macOS 11 build target; automated builds/tests run on macOS 15.
- Linux pane management/font zoom, OSC 52, audio and sustained media playback remain incomplete or unverified. Native Kitty animation remains Linux-only; Android is separate and Windows is unsupported.
- Font and Kokuban configuration changes apply to new windows; only the Omarchy palette reloads live. Resource measurements are tied to their documented revisions and workloads.

See the [v0.1…v0.2 comparison](https://github.com/guicybercode/kokuban.rs/compare/v0.1...v0.2), [text behavior](docs/TERMINAL_TEXT.md), [Omarchy guide](docs/OMARCHY.md) and [performance evidence](docs/LINUX_PERFORMANCE.md).

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
