# Changelog

## v0.5 — não lançada

Not released yet: there is no `v0.5` tag, GitHub release or Homebrew update, and the download links in the README still point to v0.4. The Git tag will be `v0.5`; the Cargo package version is already `0.5.0`. Linux panes, search, close confirmation, PRIMARY selection and the Omarchy installer; macOS text input through the system input method.

### Validation scope

- **Em teste** (implemented and covered by automated tests, not yet confirmed on real hardware): macOS IME composition (dead keys, kana/kanji, pinyin, emoji picker), including composed text in the `Cmd+F` search prompt; rendering on Retina displays (backing scale 2×, taken from the display the window opens on); the macOS energy saving of pausing drawing while the window is hidden (no wakeup or battery measurement yet); and the Omarchy integration on a physical Hyprland session (`scripts/omarchy-install.sh`, the `terminal` tag, Super+C/Super+V, fcitx5/IBus).
- Wayland was validated headless only: sway 1.10.1 (fractional scale, pointer cells, CLIPBOARD and PRIMARY through `wlr-data-control`, text-input-v3 with a protocol-level input method) and Weston 14 (integer scale, launch). X11 runs under Xvfb in CI.

### Added

- macOS text input through the system input method: `TerminalView` implements `NSTextInputClient`, so dead keys, kana/kanji, pinyin and the emoji picker commit composed text to the PTY, the pre-edit is drawn underlined at the cursor cell without touching the grid, and the candidate window follows the cursor; `option_as_alt` decides whether Option goes to the input method or acts as Meta. Real typing is **em teste** (see the validation scope above).
- Linux search strip: while `Ctrl+Shift+F` is open, the pane's last row shows the query with a block caret and `current/total` or `no matches`, above any kitty or sixel image; it keeps its row while moving between matches (a match under it is scrolled one line clear, and only a match on the live screen's last line moves it to the first row for the rest of the search); IME commits extend the query instead of reaching the shell.
- Linux close confirmation: closing a pane (`Ctrl+Shift+W`) or the window while a program other than the shell runs in the PTY foreground (`tcgetpgrp`) asks `Close pane (vim)? [y/N]` in a strip over the pane; only `y` closes, `n`/`Esc`/`Enter` keep the PTYs. Idle shells close at once; `[confirm] on_close_pane` and `on_quit` turn it off.
- Panes on Linux: the winit/softbuffer window shares the macOS pane tree. `Ctrl+Shift+D`/`Ctrl+Shift+E` split, `Ctrl+Shift+W` closes, `Ctrl+Shift+H/J/K/L` move the focus, `Ctrl+Alt+Shift+H/J/K/L` resize, `Ctrl+Shift+F` searches the scrollback (matches highlighted in the pane), `Ctrl+Shift+N` jumps to a pane with attention, `Ctrl+Shift+↑/↓` navigate prompt marks and `Ctrl+Shift++/-/0` zoom the font; all remappable under `[keybind.linux]`. Each pane owns its PTY (with its own `TIOCSWINSZ` cells and pixels), a click focuses the pane under the pointer, dividers turn hanko red next to the focused pane or one asking for attention, and a shell exiting closes only its pane. One shared `terminal-reader` thread now serves every pane on both platforms. The status bar remains macOS-only.
- Sextants U+1FB00–U+1FB3B and the one-eighth bars and blocks U+1FB70–U+1FB8B (Symbols for Legacy Computing) are drawn as whole-pixel rectangles like the block elements, so btop graphs and 2×3 pixel art have no seams; rows split at thirds and columns where `▌`/`▐` split.
- Scrollback search on macOS: `Cmd+F` opens a prompt in the status bar, `Enter` and `Shift+Enter` move between case-insensitive matches across soft-wrapped lines, `Esc` closes it; matches use the selection colors and the current one the hanko accent.
- OSC 8 hyperlinks: links are tracked per absolute row and opened with `Cmd`+click on macOS or `Ctrl`+click on Linux through the system `open`/`xdg-open` (http, https, file and mailto only).
- PRIMARY selection on Linux: a mouse drag publishes the selected text as PRIMARY on X11 and Wayland, and the middle button pastes PRIMARY into the pane under the pointer (`Shift`+middle while a program tracks the mouse). Disable with `[clipboard] primary_selection = false`. `scripts/linux-wayland-smoke.py` validates fractional scale, pointer cells, CLIPBOARD, PRIMARY and text-input-v3 under a private headless sway, using the test injector in `scripts/wayland-inject`.
- OSC 52 clipboard writes (reads are refused), base64 payloads up to 1 MiB with control characters stripped; disable with `[clipboard] osc52_write = false`.
- kitty keyboard protocol, progressive-enhancement level 1 (disambiguate escape codes): push/pop/set/query of the flag stack per screen, and `CSI u` encodings for Escape, modified Enter/Tab/Backspace and Ctrl/Alt letters.
- Per-pane attention on macOS: a BEL or desktop notification in an unfocused pane marks it with ● in the status bar and turns its dividers hanko red; `Cmd+Shift+A` jumps to the next marked pane.
- Chalkboard factory theme: `#1f3a2a` board, `#f4f6f0` chalk text and cursor, chalk-inverted selection, window chrome in the same green family and hanko red as the only accent.
- A fresh four-terminal throughput run on `main` (`docs/linux-evidence/2026-10-31-four-terminals/`): Kokuban now leads ASCII (92.4 MiB/s) and ANSI (63.6) on the CI host.
- macOS accepts the same command line as Linux: `-e COMMAND`, `--working-directory`, `--title` and `--help`; Finder's `-psn_` argument is ignored.
- `[input] option_as_alt = "both" | "left" | "right" | "none"` chooses which Option key acts as Meta on macOS; the other side composes accents and symbols.
- `docs/KEYBINDS.md`, `docs/PERFORMANCE.md`, `docs/INSTALL.md`, a generated SVG of the four-terminal throughput comparison and a feature matrix against Terminal.app, kitty, Ghostty and Alacritty.
- `scripts/compare-terminal-performance.py` runs on macOS: `--backend macos` (the default there) launches Kokuban, Alacritty, kitty, Ghostty and, with `--terminals ... terminal`, Terminal.app through AppleScript; the grid is verified without pixel-cell matching, Terminal.app CPU/RSS are application-level deltas, the output gains `summary.json` and per-terminal summaries, and `scripts/render-comparison-chart.py` accepts `--summary`, `--output`, `--title` and `--subtitle`.
- Homebrew tap [`guicybercode/kokuban`](https://github.com/guicybercode/homebrew-kokuban): the formula `kokuban.rs` installs the `kokuban` executable (release archive on macOS Apple Silicon and Intel, source build on Linux x86_64) and the cask `kokuban` installs `Kokuban.app`; both live in `packaging/homebrew/`, `scripts/update-homebrew.sh` points them at a new release, and `docs/RELEASE.md` has the release checklist.
- `scripts/omarchy-install.sh` installs a source build for the current user on Omarchy (binary, icon and desktop entry under `~/.local`), puts Kokuban first in `xdg-terminals.list` keeping the other terminals as fallbacks, and tags Kokuban windows `terminal` in the user's `hyprland.lua` (current Omarchy) or `hyprland.conf` (older releases); `uninstall` removes only what it added. Symlinked configuration files are edited through the link, and a broken link or an unterminated Kokuban block stops the script before it changes anything. On current Omarchy the tag is what makes Super+C/Super+V send `Ctrl+Shift+C/V` instead of `Ctrl+C/V` to the terminal.

### Changed

- The Omarchy palette is read from `~/.local/state/omarchy/current`, where current Omarchy publishes its theme, and falls back to `~/.config/omarchy/current` for older releases; `docs/OMARCHY.md` documents both generations.
- Relicense original project code and documentation from BSD-4-Clause to BSD-3-Clause, dropping the advertising acknowledgment requirement. The published v0.1 MIT license and third-party licenses remain unchanged.
- `kokuban --update`, `--check-update` and the update notice recognize a Homebrew install (an executable under `Cellar/`, or `Kokuban.app` in `/Applications` while the cask is installed) and point to `brew upgrade kokuban.rs` or `brew upgrade --cask kokuban` instead of replacing files that Homebrew tracks.
- A `kokuban.toml` in the launch directory can no longer run a program or open local files: `[notifications] command` and `[images.kitty] allow_file_transfer` are ignored there with a warning and are only read from the user configuration directory; appearance, window and keybind settings are still honored.
- README rewritten around the brand card, the v0.4 video and a three-line install per platform; detailed installation, troubleshooting and performance text moved to `docs/`.
- Block elements (U+2580–U+259F) and box-drawing characters are drawn as pixel-aligned rectangles on both renderers instead of font glyphs, so pixel art and TUI borders have no seams.
- macOS applies the configured ANSI palette and `colors.cursor`; without a cursor color the cursor follows the text color.
- macOS honors `window.opacity` for the default cell background, sizes the first window with the screen's real backing scale instead of a fixed 2×, shows the program running in each pane in the status bar and respects `status_bar.show_shell`, `show_cwd` and `show_pane_index`.
- macOS applies the configured ANSI palette and the configured cursor color follows the text color.
- README states macOS-first and lists platform support explicitly.

### Fixed

- macOS: with scrollback search open, dead keys and input-method text (`´` + `a` on ABNT2, Option+e with `option_as_alt = "none"`, kana to kanji) now reach the query instead of being dropped. Real typing is **em teste**.
- macOS: a new window draws the shell prompt as soon as it becomes visible instead of staying empty until the first key press.
- macOS: the glyph atlas matches the backing scale of the display the window actually opens on, not only the main screen's. Retina hardware is **em teste**.
- macOS: drawing pauses while the window is minimized, fully covered, on another Space or behind a locked screen; titles, attention marks, OSC 52 and shutdown are still handled, and the window redraws when shown again. The energy saving is **em teste**: covered by scheduler tests, not yet measured on a Mac.

## v0.4 — 2026-09-28

Self-update, macOS installer and desktop responsiveness update. The Git tag is `v0.4`; the Cargo package version is `0.4.0`.

### Added

- Update Kokuban from itself. At launch a background check asks GitHub for the latest release; macOS shows `vX available` in the status bar and Linux in the window title. `kokuban --check-update` reports it and `kokuban --update` downloads the archive for the running target (or the disk image for `Kokuban.app`), refuses it unless its `.sha256` matches and replaces the executable or bundle in place. The macOS app menu adds **Check for Updates…** and **Install Update…**. Turn the check off with `[update] check_on_startup = false` or `KOKUBAN_NO_UPDATE_CHECK=1`; it never runs for `KOKUBAN_EXIT_AFTER_FIRST_FRAME` smoke tests. No network library is linked: the system `curl`, `tar`, `shasum`/`sha256sum` and `hdiutil` do the work.
- macOS disk images: each Mac release adds `kokuban-<tag>-<target>.dmg` with `Kokuban.app` (bundle ID `io.github.guicybercode.kokuban`, ad-hoc signed, not notarized) and a drag-to-Applications window. The release workflow checks the image checksum, architecture, bundle version and code signature.
- macOS: `kokuban --version` prints the version instead of opening a window.

### Changed

- Build release binaries with fat LTO and one codegen unit. Linux x86_64 PTY throughput rose 5–35% by workload and warmed glyph lookups up to 72% faster, while x86_64 full-frame CPU repaints became about 1–2% slower and clean release builds take longer; the [release profile report](docs/linux-evidence/2026-09-15-release-lto/README.md) lists every platform and control.
- macOS: the PTY reader blocks in `poll` instead of sleeping 2 ms between scans, reads without holding the atlas and pane-tree locks, and decodes each wakeup under one lock. End-to-end throughput rose 20–393% by workload and idle wakeups fell from 93.8/s to 11.9/s on an Apple M4; the [paired report](docs/macos-evidence/2026-09-15-poll-reader/README.md) keeps A/A controls and limits.
- Wait for PTY writability instead of sleeping 1 ms after each `EAGAIN`. Large pastes into a draining macOS program went from 0.71 to 23.07 MiB/s; cancellation with a full input queue is now observed within 10 ms. See the [write report](docs/macos-evidence/2026-09-15-pty-writable-wait/README.md).
- Refresh the application icon: the same chalkboard artwork without the irregular white border, used for the macOS Dock and the Linux X11 window and launcher.
- macOS: frames are scheduled on the main queue only when content, titles or shutdown need them, still capped at 60 Hz, instead of a repeating 60 Hz timer. Idle wakeups fell from 8.0/s to 0.17/s and idle CPU by about 80%, with PTY throughput unchanged within noise; see the [render scheduling report](docs/macos-evidence/2026-09-15-on-demand-render/README.md).

### Fixed

- macOS: output decoded while a frame was being drawn is no longer left undrawn until more output arrives.
- macOS: `Kokuban.app` opened from Finder, the Dock or Launchpad starts the shell in the home directory instead of `/`.
- macOS: when a split pane's shell exits, the remaining pane grows back to the full window. Its grid and PTY size used to stay at the split size until the next resize, leaving text in part of the window.

### Validation scope and limits

- Release targets remain Linux x86_64, macOS Apple Silicon and macOS Intel, using Rust 1.94.1 and the locked dependency graph. Linux packages are built on Ubuntu 24.04; macOS on macOS 15 with a macOS 11 deployment target.
- macOS executables and `Kokuban.app` are ad-hoc signed only, without Developer ID signing or notarization.
- The updater was exercised end to end against the published v0.3 archive on macOS Apple Silicon. Replacing `Kokuban.app` from a disk image can first be exercised against v0.4, and Linux updating relies on the same code path verified by CI unit tests rather than a live download. v0.3 and older have no updater: install v0.4 manually once.
- Performance figures above come from the linked paired reports on their recorded hosts and revisions; they are not new measurements of the v0.4 binaries.

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
