# Changelog

## Unreleased

### Changed

- Build release binaries with fat LTO and one codegen unit. Linux x86_64 PTY throughput rose 5–35% by workload and warmed glyph lookups up to 72% faster, while x86_64 full-frame CPU repaints became about 1–2% slower and clean release builds take longer; the [release profile report](docs/linux-evidence/2026-09-15-release-lto/README.md) lists every platform and control.
- macOS: the PTY reader blocks in `poll` instead of sleeping 2 ms between scans, reads without holding the atlas and pane-tree locks, and decodes each wakeup under one lock. End-to-end throughput rose 20–393% by workload and idle wakeups fell from 93.8/s to 11.9/s on an Apple M4; the [paired report](docs/macos-evidence/2026-09-15-poll-reader/README.md) keeps A/A controls and limits.
- Wait for PTY writability instead of sleeping 1 ms after each `EAGAIN`. Large pastes into a draining macOS program went from 0.71 to 23.07 MiB/s; cancellation with a full input queue is now observed within 10 ms. See the [write report](docs/macos-evidence/2026-09-15-pty-writable-wait/README.md).

### Fixed

- macOS: output decoded while a frame was being drawn is no longer left undrawn until more output arrives.

## v0.3 — 2026-09-13

Desktop performance and distribution update. The Git tag is `v0.3`; the Cargo package version is `0.3.0`.

### Changed

- Store immutable scrollback rows with compact default-cell suffixes while preserving logical width, styled blanks, complete graphemes, selection and resize reflow. The [paired history report](docs/linux-evidence/2026-09-13-compact-history/README.md) records the throughput improvements and regressions by workload.
- Cache glyph color classification to avoid repeating the scan during Linux painting; remove unused grid dirty flags. Preserve complete before/after frame comparisons in the [renderer report](docs/linux-evidence/2026-09-13-glyph-color/summary.md).

- Reduce Linux software alpha-blending work and filter glyph painting against damaged frame bands; preserve pixel-equivalence checks for clipping, overhang and underlines.
- Use direct cache slots for ASCII glyphs across regular, bold, italic and bold-italic styles on Linux and macOS. The [integration report](docs/LINUX_DAMAGE_CACHE_2026-09-13.md) records measured gains, Unicode and grapheme costs, and 14 KiB additional inline atlas storage.
- Add controlled CPU repaint and warmed glyph-lookup measurements, plus an Xlib observer for synthetic X11 input-to-readback tests. Preserve raw measurements and offline integrity checks; each report defines its timing boundaries and hardware limits.

- Exclude development Python scripts and raw benchmark snapshots from binary release archives and Cargo source packages. Keep the tools in the repository for CI and reproducible measurements; archive documentation links to the matching source revision. Building and running the Rust application do not require Python.
- Classify development tools and documentation separately from application code in GitHub language statistics.
- Expand README performance details with process CPU/RSS, short video observations, a four-terminal throughput comparison and optimization tradeoffs. Measurements retain their original commit identities; they are not new measurements of the v0.3 binaries.

### Validation scope and limits

- Release targets remain Linux x86_64, macOS Apple Silicon and macOS Intel, using Rust 1.94.1 and the locked dependency graph.
- Linux packages are built on Ubuntu 24.04. Native macOS tests/builds run on macOS 15 with a macOS 11 deployment target; binaries remain unsigned, unnotarized standalone executables.
- Linux launch checks cover X11 and headless Weston/Wayland; graphical interaction and release rendering checks use X11. Physical Omarchy/Hyprland, broad Wayland interaction, OSC 52, audio and sustained media playback remain incomplete or unverified. Android is not shipped.
- CPU repaint and warmed glyph lookup timings exclude PTY processing and physical display presentation. Retained benchmark gains, regressions and resource figures apply only to their recorded workloads, revisions and hosts.

See the [v0.2…v0.3 comparison](https://github.com/guicybercode/kokuban.rs/compare/v0.2...v0.3), [installation instructions](README.md#installation) and [performance details](README.md#performance).

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

- Change the license for original project code and documentation to BSD-4-Clause starting with v0.2, including its advertising acknowledgment requirement. The published v0.1 MIT license and third-party licenses remain unchanged.
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
