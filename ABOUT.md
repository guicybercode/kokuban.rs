# About Kokuban

Kokuban is an open-source terminal emulator written in Rust and maintained at [guicybercode/kokuban.rs](https://github.com/guicybercode/kokuban.rs). The name 黒板 (*kokuban*) means “blackboard” in Japanese.

The project aims to provide a small native terminal for development workflows, interactive shells, SSH and terminal graphics. It contains its own VT/ANSI parser, grid, PTY handling and rendering integration. macOS uses Metal; Linux uses a CPU renderer with winit and softbuffer. The Rust application also relies on native operating-system APIs and font libraries.

The v0.1 desktop release includes static Kitty/Sixel images, Linux native Kitty animation, Linux clipboard/selection and video through an external mpv player. Linux tests exercise SSH with Neovim, tmux and fzf. SSH on desktop uses the system's client; mpv performs video decoding. These external programs are not bundled with Kokuban.

Android APK work and emulator evidence are on the separate `codex/android-native` branch. Android is not included in the v0.1 desktop release. Windows is not supported. Compatibility, Unicode grapheme handling, Wayland behavior and sustained media performance remain active development areas; see the [roadmap](docs/ROADMAP.md) and the measured [Linux release baseline](docs/LINUX_PERFORMANCE.md).

- [Install and run](README.md#installation)
- [Download v0.1](https://github.com/guicybercode/kokuban.rs/releases/tag/v0.1)
- [Contribute](CONTRIBUTING.md)
- [Report a security issue privately](SECURITY.md)
- [MIT license](LICENSE)
