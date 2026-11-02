# Keybinds

Kokuban ships with a small fixed set of clipboard and scrollback inputs on Linux and a configurable pane, zoom, prompt-navigation and search table on macOS. The macOS bindings live under `[keybind]` in `kokuban.toml`; see the [example configuration](../kokuban.toml) for the full list of actions and the key syntax.

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
| Open an OSC 8 hyperlink | `Ctrl` + left click |

Ordinary `Ctrl+C` and `Ctrl+V` remain application input. Paste honors bracketed-paste mode, normalizes line endings, removes embedded control characters, and rejects oversized text instead of truncating it. Copy and encoded paste are limited to 1 MiB. Clipboard access runs in the background; accepted repeated paste requests retain their order.

Omarchy maps `Super+C` and `Super+V` to the supported Insert shortcuts; see [Omarchy clipboard bindings](OMARCHY.md#use-the-existing-omarchy-bindings) for the integration and validation scope.

The Linux clipboard works through X11 or a Wayland compositor exposing a data-control protocol. Other Wayland desktops need XWayland clipboard access. Copy joins automatically wrapped rows, preserves explicit line breaks and copies complete graphemes. Resize reflows retained text without cropping it. See [text preservation](TERMINAL_TEXT.md) and [Linux application validation](LINUX_APPS.md). OSC 52 remote clipboard commands remain pending.

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
| Search scrollback   | `Cmd+F`             |
| Next / previous match (while searching) | `Enter` / `Shift+Enter` |
| Close search        | `Esc`               |
| Jump to pane with attention | `Cmd+Shift+A` |
| Open an OSC 8 hyperlink | `Cmd` + left click |

The pane, resize, zoom, prompt-navigation, search and attention bindings in the macOS table are configurable; the search prompt, mouse clicks and `Esc` are fixed. A pane is marked with ● in its status bar after a BEL or a desktop notification arrives while it is not focused, and dividers touching it turn hanko red. Linux clipboard and scrollback shortcuts use their fixed input routes. On macOS, `Cmd+C` copies a selection or sends Ctrl+C when there is none; `Cmd+V` pastes.


The pane, zoom, prompt and search bindings apply to macOS and can be remapped under `[keybind]`; the Linux clipboard and scrollback inputs above are fixed.

## Linux (from v0.5)

The Linux window port of the pane table is in progress. The table below is the `[keybind.linux]` section of `kokuban.toml`, already parsed and validated today, that the Linux window will dispatch once the port lands. Super is left to the window manager, and none of the defaults collides with the fixed `Ctrl+Shift+C/V/A`, `Ctrl+Insert`, `Shift+Insert` and `Shift+PageUp/PageDown/Home/End` inputs above.

| Action              | Default Linux keybind   |
|---------------------|-------------------------|
| Split vertical      | `Ctrl+Shift+D`          |
| Split horizontal    | `Ctrl+Shift+E`          |
| Close pane          | `Ctrl+Shift+W`          |
| Focus left pane     | `Ctrl+Shift+H`          |
| Focus down pane     | `Ctrl+Shift+J`          |
| Focus up pane       | `Ctrl+Shift+K`          |
| Focus right pane    | `Ctrl+Shift+L`          |
| Resize left         | `Ctrl+Alt+Shift+H`      |
| Resize down         | `Ctrl+Alt+Shift+J`      |
| Resize up           | `Ctrl+Alt+Shift+K`      |
| Resize right        | `Ctrl+Alt+Shift+L`      |
| Zoom in             | `Ctrl+Shift++` (`ctrl+shift+plus` or `ctrl+shift+=`) |
| Zoom out            | `Ctrl+Shift+-` (`ctrl+shift+minus`) |
| Reset zoom          | `Ctrl+Shift+0`          |
| Previous prompt     | `Ctrl+Shift+↑`          |
| Next prompt         | `Ctrl+Shift+↓`          |
| Search scrollback   | `Ctrl+Shift+F`          |
| Jump to pane with attention | `Ctrl+Shift+N`  |

Key names are shared by both platforms: a single character, `plus`/`minus`, `f1`..`f12`, `up`/`down`/`left`/`right`, `pageup`/`pagedown`, `home`/`end`, `tab`, `esc`, `enter`, `backspace`, `space`, `insert`, `delete`; modifiers are `ctrl`, `alt`, `shift` and `super` (`cmd` on macOS). With `shift` held, punctuation matches either spelling of the key (`=`/`+`, `-`/`_`).
