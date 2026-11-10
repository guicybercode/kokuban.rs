# Keybinds

Kokuban ships with a small fixed set of clipboard and scrollback inputs on Linux and a configurable pane, zoom, prompt-navigation and search table on both platforms. The macOS bindings live under `[keybind]` in `kokuban.toml`, the Linux ones under `[keybind.linux]`; see the [example configuration](../kokuban.toml) for the full list of actions and the key syntax.

Linux supports these clipboard and selection actions:

| Action | Linux input |
|--------|-------------|
| Select text | Left-button drag |
| Select text while an application captures the mouse | `Shift` + left-button drag |
| Copy selection | `Ctrl+Shift+C` or `Ctrl+Insert` |
| Paste clipboard | `Ctrl+Shift+V` or `Shift+Insert` |
| Paste the PRIMARY selection | Middle click |
| Paste PRIMARY while an application captures the mouse | `Shift` + middle click |
| Select all retained text | `Ctrl+Shift+A` |
| Scroll history | `Shift+PageUp` / `Shift+PageDown` |
| Oldest/newest retained view | `Shift+Home` / `Shift+End` |
| Open an OSC 8 hyperlink | `Ctrl` + left click |

Ordinary `Ctrl+C` and `Ctrl+V` remain application input. Paste honors bracketed-paste mode, normalizes line endings, removes embedded control characters, and rejects oversized text instead of truncating it. Copy and encoded paste are limited to 1 MiB. Clipboard access runs in the background; accepted repeated paste requests retain their order.

Omarchy maps `Super+C` and `Super+V` to the supported Insert shortcuts; see [Omarchy clipboard bindings](OMARCHY.md#use-the-existing-omarchy-bindings) for the integration and validation scope.

A drag selection also becomes the PRIMARY selection, which other X11 and Wayland applications paste with their middle button; a plain click does not replace it. Set `[clipboard] primary_selection = false` to keep both the selection and the middle button out of PRIMARY. While an application tracks the mouse, the middle button is reported to it unless `Shift` is held.

The Linux clipboard works through X11 or a Wayland compositor exposing a data-control protocol (`ext-data-control-v1` or `wlr-data-control`; Hyprland and sway have it). PRIMARY on Wayland needs `wlr-data-control` version 2 or `ext-data-control`; without it CLIPBOARD keeps working, publishing PRIMARY logs one warning per session and middle-click paste does nothing. On a Wayland desktop without data-control (GNOME, Weston) Kokuban falls back to the X11 clipboard through XWayland, so `DISPLAY` must be set; with neither, copy and paste log a warning and do nothing. Copy joins automatically wrapped rows, preserves explicit line breaks and copies complete graphemes. Resize reflows retained text without cropping it. See [text preservation](TERMINAL_TEXT.md) and [Linux application validation](LINUX_APPS.md). OSC 52 clipboard writes from remote programs are accepted; reads are refused.

These are the macOS defaults for the pane, zoom, prompt-navigation and search actions; the Linux defaults follow in the next section.

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


The pane, zoom, prompt and search bindings can be remapped under `[keybind]` on macOS and `[keybind.linux]` on Linux; the clipboard and scrollback inputs above are fixed.

## Linux (from v0.5)

The Linux window dispatches the `[keybind.linux]` table below: splits, close, focus, resize, zoom, prompt navigation, scrollback search and the attention jump, on the same pane tree as macOS. Super is left to the window manager, and none of the defaults collides with the fixed `Ctrl+Shift+C/V/A`, `Ctrl+Insert`, `Shift+Insert` and `Shift+PageUp/PageDown/Home/End` inputs above. A click focuses the pane under the pointer; dividers next to the focused pane, or to one that received a BEL or notification while unwatched, are drawn in hanko red.

While the search is open, a strip over the pane's last row shows the query, a block caret and `current/total` or `no matches` in hanko red; typed and IME-composed text goes to the query, never to the shell. The strip keeps its row while `Enter`/`Shift+Enter` move between matches: a match that would land under it is scrolled one line clear instead. Only a match on the last line of the live screen, with nothing below to scroll to, moves the strip to the first row, where it stays until the search closes. The strip is drawn above kitty and sixel images.

`Ctrl+Shift+W` and the window's close button ask first when a program other than the shell holds the pane's terminal in the foreground (an editor, `ssh`, a build; detected with `tcgetpgrp` on the PTY), naming it in a strip such as `Close pane (vim)? [y/N]`. Only `y` closes; `n`, `Esc` and `Enter` (the default `N`) cancel and keep every PTY; other keys and clicks are ignored until then. An idle shell closes at once. `[confirm] on_close_pane = false` and `on_quit = false` turn the questions off. A program started with `-e` is the pane's own process and does not count as a job.

Still macOS-only: the status bar, so Linux shows neither the ● attention mark nor the prompt indicator.

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
| Next / previous match (while searching) | `Enter` / `Shift+Enter` |
| Close search        | `Esc`                   |
| Jump to pane with attention | `Ctrl+Shift+N`  |
| Open an OSC 8 hyperlink | `Ctrl` + left click |

Key names are shared by both platforms: a single character, `plus`/`minus`, `f1`..`f12`, `up`/`down`/`left`/`right`, `pageup`/`pagedown`, `home`/`end`, `tab`, `esc`, `enter`, `backspace`, `space`, `insert`, `delete`; modifiers are `ctrl`, `alt`, `shift` and `super` (`cmd` on macOS). With `shift` held, punctuation matches either spelling of the key (`=`/`+`, `-`/`_`).
