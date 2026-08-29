# 黒板 Kokuban

A native GPU terminal emulator built from scratch in Rust.

![Kokuban terminal window](docs/screenshots/hero.png)

Kokuban is a from-scratch terminal emulator with a Metal GPU renderer on macOS and a software rasterizer on Linux. It includes its own VT/ANSI parser, PTY handling, glyph rendering, pane management, and graphics protocol support.

## Features

- **Native rendering**: Metal GPU renderer on macOS; software rasterizer with winit + softbuffer on Linux
- **Built-in parser**: VT/ANSI escape sequence parser with support for complex SGR modes (faint, conceal, styled underlines)
- **Graphics protocols**: Kitty graphics protocol and Sixel image rendering
- **Pane management**: Split windows vertically or horizontally, navigate with vim-style keybinds
- **Zoom**: Dynamic font size adjustment per session
- **Selection**: Mouse-driven text selection with configurable colors
- **Status bar**: Shows shell, working directory, and pane index
- **Prompt marks**: Visual indicators for command boundaries with navigation shortcuts
- **Configuration**: TOML-based config file with font, color, and keybind customization

## Platform Support

- **macOS**: Metal GPU renderer (11.0+)
- **Linux**: Software rasterizer with X11/Wayland via winit

Windows is not supported. The crate will fail to compile on unsupported platforms.

## Installation

### Prerequisites

**Linux** requires:
- libfontconfig
- libfreetype6
- libxkbcommon-x11-0
- X11 or Wayland display server

On Debian/Ubuntu:
```bash
sudo apt install libfontconfig1-dev libfreetype6-dev libxkbcommon-x11-0
```

**macOS** has no additional dependencies beyond Xcode Command Line Tools.

### Building from Source

```bash
# Clone the repository
git clone https://github.com/guicybercode/kokuban.git
cd kokuban

# Build release binary
cargo build --release

# Run
./target/release/kokuban
```

The binary will be created at `target/release/kokuban`.

## Configuration

Kokuban reads its configuration from `kokuban.toml` in the current working directory or `~/.config/kokuban/kokuban.toml`. A default configuration will be used if no file is found.

Example configuration:

```toml
[font]
family = "Menlo"
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

See `kokuban.toml` in the repository root for the complete default configuration.

## Keybinds

Default keybinds use `cmd` on macOS and `Super` (Windows key) on Linux:

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

All keybinds are customizable via the configuration file.

## Development Status

Kokuban is in active development (v0.1.0). Core terminal functionality is stable, but expect changes as features are refined and added.

## Project Name

黒板 (*kokuban*) is the Japanese word for "blackboard" or "chalkboard"—a blank surface for writing and drawing.
