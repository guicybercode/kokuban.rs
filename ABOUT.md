# About Kokuban

**Kokuban is a native Rust terminal for Linux and macOS, built around development workflows, faithful text handling and terminal graphics.** The name 黒板 (*kokuban*) means “blackboard” in Japanese. Source code, releases and project discussions live at [guicybercode/kokuban.rs](https://github.com/guicybercode/kokuban.rs).

Use it for a local shell, an editor or multiplexer, an SSH session, or applications that display Kitty/Sixel graphics. Kokuban provides the terminal; the shell, SSH client, editor and media player remain the programs you choose and install.

## What v0.2 brings

- **Text that survives interaction:** complete Unicode graphemes, font fallback and color emoji, selection that joins soft-wrapped lines, and retained text that reflows on resize.
- **Omarchy integration:** launcher arguments and working directories, compatible copy/paste input, live palette changes without restarting the shell, and the system monospace font on Linux.
- **Native platform rendering:** Metal on macOS, with panes and font zoom; winit and softbuffer on Linux, with asynchronous clipboard access and native Kitty animation.
- **Inspectable performance work:** batched text processing, circular row storage, incremental Linux repainting and an idle PTY reader that sleeps until output or shutdown. Benchmarks preserve both improvements and regressions with reproduction details.

Kokuban implements its own VT/ANSI parser, grid and PTY integration. Its Rust application uses native operating-system APIs and font libraries. Static Kitty/Sixel images work on both desktop platforms; Linux video tests use external mpv for decoding and Kitty output.

## Platforms and evidence

The [v0.2 release](https://github.com/guicybercode/kokuban.rs/releases/tag/v0.2) targets Linux x86_64 and macOS Apple Silicon/Intel. Linux release packages are built on Ubuntu 24.04. macOS packages are standalone executables without signing or notarization; see [installation requirements](README.md#installation).

Automated Linux scenarios exercise actual windows, clipboard contents, rendered pixels and SSH workflows with Neovim, tmux and fzf. Launch arguments are tested under X11 and headless Weston/Wayland. These checks do not establish complete application compatibility or a physical Omarchy/Hyprland session. Broad Wayland interaction, OSC 52, audio and sustained media playback remain open work. Android is developed on the separate `codex/android-native` branch and is not part of this desktop release; Windows is unsupported.

Start with [text preservation](docs/TERMINAL_TEXT.md), [Omarchy integration and validation](docs/OMARCHY.md), [Linux application checks](docs/LINUX_APPS.md), [video checks](docs/LINUX_VIDEO.md) and [performance measurements](docs/LINUX_PERFORMANCE.md). Reports identify the revisions and environments they tested; older measurements are not new v0.2 resource guarantees.

## Participate and cite

Bug reports, focused fixes, tests, documentation and platform feedback are welcome. Read the contribution guide and code of conduct before joining. Security reports use the private reporting channel.

- [Install and run](README.md#installation)
- [Download v0.2](https://github.com/guicybercode/kokuban.rs/releases/tag/v0.2)
- [Read the release announcement in Portuguese](docs/blog/2026-09-13-kokuban-v0.2.md)
- [Cite this repository](CITATION.cff)
- [Contribute](CONTRIBUTING.md)
- [Code of conduct](CODE_OF_CONDUCT.md)
- [Report a security issue privately](SECURITY.md)
- [License](LICENSE)

For a blog, presentation or article, cite **Kokuban v0.2 — guicybercode/kokuban.rs** and link to the [release](https://github.com/guicybercode/kokuban.rs/releases/tag/v0.2). For development builds or benchmark results, include the exact commit. Citation metadata supplements the obligations in [LICENSE](LICENSE). Third-party code, content and assets retain their own licenses; the Contributor Covenant code of conduct remains under CC-BY-4.0. See [third-party licensing](docs/THIRD_PARTY.md).
