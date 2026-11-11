# Omarchy integration

Kokuban accepts the terminal launch options used by Omarchy:

```sh
kokuban --working-directory="$HOME" --title="My terminal"
kokuban --app-id=org.omarchy.btop -e btop
kokuban -e nvim 'file with spaces.txt'
kokuban -e sh -c 'printf "hello\n"; read answer'
kokuban --help
```

`-e`, `--execute`, or `--` ends option parsing. The remaining arguments go directly
to the program; quoting, pipelines, and expansions require an explicit shell.
Without a command, Kokuban starts the configured login shell. `--dir` is an alias
for `--working-directory`. Long options also accept a separate value. `--title`
sets the initial title; an application can subsequently change it with OSC 0/2.

## Omarchy generations

Omarchy changed its layout in 2026. Kokuban supports both generations; this
guide marks which one each step applies to.

| | Current Omarchy (Lua) | Older Omarchy (`.conf`) |
|---|---|---|
| Reference revision | [`omacom/omarchy@e56f833`](https://github.com/omacom/omarchy/tree/e56f833af7dccc77894b6a0934f4c4eeb559c6ae), default branch `quattro`, inspected 2026-11-10 | [`omacom/omarchy@f4378f0`](https://github.com/omacom/omarchy/tree/f4378f0de5b44d331ee943746a97872b718a6c18), historical `master` |
| Active theme | `~/.local/state/omarchy/current/theme` | `~/.config/omarchy/current/theme` |
| Hyprland entry file | `~/.config/hypr/hyprland.lua` | `~/.config/hypr/hyprland.conf` |
| Super+C / Super+V | `Ctrl+Shift+C` / `Ctrl+Shift+V` to windows tagged `terminal`, `Ctrl+C` / `Ctrl+V` to all others | `Ctrl+Insert` / `Shift+Insert` to every window |
| Default terminal list | `/usr/share/xdg-terminal-exec/hyprland-xdg-terminals.list` (Foot) unless the user has a list | `~/.config/xdg-terminals.list` |

The `basecamp/omarchy` repository address now redirects to `omacom/omarchy`.
Omarchy's [state migration](https://github.com/omacom/omarchy/blob/e56f833af7dccc77894b6a0934f4c4eeb559c6ae/migrations/1781043107.sh)
moves `~/.config/omarchy/current` to `~/.local/state/omarchy/current` and removes
the old directory, without leaving an alias.

## Install and select

Build on the Omarchy machine with the Rust toolchain, compiler tools, pkg-config,
Fontconfig, and FreeType installed. Wayland, xkbcommon and the X11 libraries are
opened at run time and are already part of Omarchy; no `-dev` packages are needed:

```sh
sudo pacman -S --needed rustup base-devel pkgconf fontconfig freetype2
rustup toolchain install 1.94.1 --profile minimal
rustup run 1.94.1 cargo build --locked --release
scripts/omarchy-install.sh
```

Copy and paste go through Hyprland's data-control protocol inside Kokuban; the
`wl-clipboard` package is not required. The steps below have passed headless
checks only; on a physical Omarchy machine they are **em teste** (see
[Validation scope](#validation-scope)).

The script works on both generations and only touches files in your home:

1. It installs `~/.local/bin/kokuban`, the icon under
   `${XDG_DATA_HOME:-$HOME/.local/share}/kokuban/`, and the desktop entry
   `io.github.guicybercode.kokuban.desktop` under
   `${XDG_DATA_HOME:-$HOME/.local/share}/applications/`, with absolute `Exec`
   and `Icon` paths. The entry keeps the `X-TerminalArg*` keys that let
   `xdg-terminal-exec` pass `--app-id`, `--dir`, `--title`, and `-e`.
2. It puts Kokuban first in your `xdg-terminals.list`, keeping the existing
   entries as fallbacks (see [Choose the default terminal](#choose-the-default-terminal)).
3. It adds the Hyprland `terminal` tag for Kokuban windows to your Hyprland
   entry file (see [Join Omarchy's terminal window rules](#join-omarchys-terminal-window-rules)).

Repeating the script changes nothing. Options: `--binary PATH` installs another
executable, `--no-default` leaves the terminal preference alone, `--no-hypr`
skips the Hyprland rule, and `--hypr-format lua|conf` chooses the configuration
generation when both `hyprland.lua` and `hyprland.conf` exist (Lua is chosen
by default). To remove everything it added:

```sh
scripts/omarchy-install.sh uninstall
```

`uninstall` removes the three files, removes only Kokuban's line from the
terminal lists (deleting a list only if the script created it and it is now
empty), and removes only the marked Kokuban block from the Hyprland file.
Omarchy's defaults under `/usr/share/omarchy` or `~/.local/share/omarchy` are
never edited.

Configuration files that are symbolic links (for example into a dotfiles
repository) stay links: the script edits the file they point at. It stops before
changing anything if such a link is broken, or if the Hyprland file has the
`>>> kokuban terminal tag >>>` line without the matching
`<<< kokuban terminal tag <<<` line; the error names the file and line to fix.

Omarchy's session environment includes `~/.local/bin` in `PATH`. Confirm
`command -v kokuban` finds the newly installed executable. The installed
launcher uses its absolute path; with a customized session, also include the
binary directory in the session's `PATH` to use `kokuban` directly from a shell.

### Manual installation

The same steps without the script (`desktop-file-install` comes from
`desktop-file-utils`):

```sh
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

Then follow the two sections below by hand.

## Choose the default terminal

Omarchy's Super+Return runs
[`omarchy-launch-terminal`](https://github.com/omacom/omarchy/blob/e56f833af7dccc77894b6a0934f4c4eeb559c6ae/bin/omarchy-launch-terminal),
and [`omarchy-launch-tui`](https://github.com/omacom/omarchy/blob/e56f833af7dccc77894b6a0934f4c4eeb559c6ae/bin/omarchy-launch-tui)
opens TUIs such as btop; both go through `xdg-terminal-exec`. To select Kokuban,
add this line as the **first terminal entry** in
`${XDG_CONFIG_HOME:-$HOME/.config}/xdg-terminals.list`, preserving the other
entries as fallbacks:

```text
io.github.guicybercode.kokuban.desktop
```

Per the [xdg-terminal-exec specification](https://github.com/Vladimir-csp/xdg-terminal-exec/blob/80d6ea72054089b59356919e0391a1395460788d/README.md),
a desktop-specific `~/.config/hyprland-xdg-terminals.list` outranks the generic
list, and both outrank the lists shipped in `/usr/share/xdg-terminal-exec/`,
where current Omarchy keeps its Foot default. The install script edits the
desktop-specific list if you have one, and the generic list otherwise.

Verify selection and translated arguments:

```sh
xdg-terminal-exec --print-id
xdg-terminal-exec --print-cmd --app-id=org.omarchy.btop --dir="$HOME" -e btop
```

The first command should report `io.github.guicybercode.kokuban.desktop`; the
second should include `--app-id=org.omarchy.btop`, `--working-directory=...`, and
`-e btop`.

Omarchy's terminal menu,
[`omarchy-default-terminal`](https://github.com/omacom/omarchy/blob/e56f833af7dccc77894b6a0934f4c4eeb559c6ae/bin/omarchy-default-terminal),
accepts Alacritty, Foot, Ghostty, and Kitty only, and **rewrites**
`~/.config/xdg-terminals.list` with the chosen terminal alone. To switch away
from Kokuban, pick another terminal there. To come back, run
`scripts/omarchy-install.sh` again, or put the Kokuban line back at the top of
the list; the menu's choice then stays as the fallback.

## Join Omarchy's terminal window rules

Omarchy recognizes terminals by the Hyprland tag `terminal`. Its
[terminal rule](https://github.com/omacom/omarchy/blob/e56f833af7dccc77894b6a0934f4c4eeb559c6ae/default/hypr/apps/terminals.lua)
lists Alacritty, Kitty, Ghostty, Foot, WezTerm, and the `org.omarchy.*` and
`TUI.*` app IDs, but not Kokuban's normal class `io.github.guicybercode.kokuban`.

**On current Omarchy the tag decides how copy and paste reach the terminal.**
Its [clipboard bindings](https://github.com/omacom/omarchy/blob/e56f833af7dccc77894b6a0934f4c4eeb559c6ae/default/hypr/bindings/clipboard.lua)
send `Ctrl+Shift+C` and `Ctrl+Shift+V` for Super+C and Super+V to windows
tagged `terminal`, and plain `Ctrl+C` and `Ctrl+V` to every other window.
Without the tag, Super+C in a Kokuban window reaches the shell as `Ctrl+C` and
interrupts the running program, and Super+V inserts a literal `Ctrl+V`.
Windows opened with an `org.omarchy.*` app ID, such as `omarchy-launch-tui btop`,
already carry the tag through Omarchy's own rule.

For current Omarchy, append this to `~/.config/hypr/hyprland.lua`, which Omarchy
[loads after its defaults](https://github.com/omacom/omarchy/blob/e56f833af7dccc77894b6a0934f4c4eeb559c6ae/config/hypr/hyprland.lua)
and where it suggests personal window rules:

```lua
o.window("io\\.github\\.guicybercode\\.kokuban", { tag = "+terminal" })
```

The pattern uses the same form as Omarchy's own terminal rule: a regular
expression matched against the whole class, with escaped dots.

For older Omarchy, append this to `~/.config/hypr/hyprland.conf`, which
[sources the defaults first](https://github.com/omacom/omarchy/blob/f4378f0de5b44d331ee943746a97872b718a6c18/config/hypr/hyprland.conf)
([Hyprland 0.54 rule syntax](https://wiki.hypr.land/0.54.0/Configuring/Window-Rules/)):

```ini
windowrule = tag +terminal, match:class ^(io[.]github[.]guicybercode[.]kokuban)$
```

On that generation Super+C and Super+V send `Ctrl+Insert` and `Shift+Insert`
([`f4378f0` clipboard bindings](https://github.com/omacom/omarchy/blob/f4378f0de5b44d331ee943746a97872b718a6c18/default/hypr/bindings/clipboard.conf))
to every window, so the tag only affects appearance there.

The install script writes exactly these lines inside a marked
`>>> kokuban terminal tag >>>` block, which `uninstall` removes. Keep the rule
in your own file so Omarchy updates can replace their defaults independently.

Hyprland reloads its configuration when the file changes. Check for errors and
open a normal Kokuban window:

```sh
hyprctl configerrors
hyprctl clients -j | jq '.[] | select(.class == "io.github.guicybercode.kokuban") |
  {class, initialClass, tags, xwayland}'
```

Confirm the terminal tag appears (dynamic tags may be displayed as `terminal*`),
`xwayland` is `false`, and the window has the expected terminal appearance.
These checks require a running Omarchy/Hyprland session; they have not yet been
executed on a physical Omarchy machine for this project.

## Use the existing Omarchy bindings

`omarchy-launch-tui` passes an `org.omarchy.<command>` app ID and the requested
command. Kokuban's desktop entry translates both forms, and its window class
uses the requested ID on Wayland and X11.

After selecting Kokuban, exercise the existing entry points on Omarchy:

```sh
omarchy-launch-tui btop
xdg-terminal-exec --dir="$HOME" -e nvim
```

Also open a login shell, change directory, and press Super+Return: the new terminal
should inherit that directory. Kokuban's shell is its direct child and leads its
own session on the PTY, which matches what
[omarchy-cmd-terminal-cwd](https://github.com/omacom/omarchy/blob/e56f833af7dccc77894b6a0934f4c4eeb559c6ae/bin/omarchy-cmd-terminal-cwd)
inspects. With several panes, that script picks the newest shell process of the
window, which is the most recently opened pane rather than necessarily the
focused one.

Kokuban handles `Ctrl+Shift+C` and `Ctrl+Insert` as copy selection, and
`Ctrl+Shift+V` and `Shift+Insert` as paste, so both Omarchy generations work
once the tag above is in place. The compositor owns the Super bindings; no
additional Kokuban binding is needed. Ordinary `Ctrl+C` and `Ctrl+V` remain
terminal input, and extra modifiers on Insert are not clipboard shortcuts.

The X11 clipboard smoke injects both copy shortcuts and both paste shortcuts,
checks clipboard content against a fresh external owner, and verifies that copy
sends no bytes to the PTY. This tests Kokuban's input routes, not Hyprland's
physical-key remapping. In an Omarchy session, select text and use `Super+C`,
paste it into another application, then copy text there and paste into Kokuban
with `Super+V` to validate the full compositor path.

For persistent settings use `${XDG_CONFIG_HOME:-$HOME/.config}/kokuban/kokuban.toml`.
A `kokuban.toml` in the directory where Kokuban itself was launched takes priority;
the CLI working directory is applied after configuration loading.

## Follow the Omarchy palette

On Linux, Kokuban automatically reads `theme/colors.toml` from Omarchy's
`current` directory when it exists at launch. It looks in two places, in order:

1. `$HOME/.local/state/omarchy/current`, used by current Omarchy
   ([`omarchy-theme-set` at `e56f833`](https://github.com/omacom/omarchy/blob/e56f833af7dccc77894b6a0934f4c4eeb559c6ae/bin/omarchy-theme-set));
2. `$HOME/.config/omarchy/current`, used by older releases.

The first directory that exists wins, so a leftover legacy directory is ignored
once Omarchy has moved to the state directory. Omarchy uses these HOME-based
paths even if `XDG_STATE_HOME` or `XDG_CONFIG_HOME` point elsewhere, and so does
Kokuban. The location is chosen when a window opens: after upgrading Omarchy
across the move, open a new Kokuban window to follow the new location.

The [palette format at Omarchy `f4378f0`](https://github.com/omacom/omarchy/blob/f4378f0de5b44d331ee943746a97872b718a6c18/themes/tokyo-night/colors.toml)
provides foreground, background, cursor, selection foreground/background, and
`color0` through `color15`. Kokuban applies all of these roles, including the
bright ANSI colors. Truecolor and indexed colors 16–255 retain their terminal
meaning. Theme colors are used as supplied.

Changing the Omarchy theme updates existing Kokuban windows without restarting
their shells, clearing text, or changing keyboard bindings. Kokuban observes
filesystem events in the stable `current` directory and the active `theme`
directory, including Omarchy's directory replacement during theme changes (`rm -rf` of
`current/theme`, then `mv` of `current/next-theme` into its place, in both
generations).
The observer sleeps until a filesystem event or shutdown; it does not poll on a
timer. No additional theme hook or terminal restart command is required.

Only complete regular UTF-8 files of at most 64 KiB are accepted. Every required
color must use `#RRGGBB` notation. Missing, partial, or invalid replacements keep
the last valid palette; a later valid publication is applied. If no valid theme
is available initially, Kokuban uses its ordinary colors and explicit settings.

Explicit colors in the selected `kokuban.toml` take priority over the system
theme, including values equal to Kokuban's defaults. Omit the settings you want
Omarchy to control. In particular, copying the repository's sample `[colors]`
and `[selection]` sections pins those colors. These settings can be overridden
independently:

| Setting | Value |
|---------|-------|
| `colors.foreground`, `colors.background` | Text and background colors |
| `colors.cursor` | Cursor color |
| `colors.ansi` | Array of exactly 16 colors, ordered 0–15 |
| `selection.foreground`, `selection.background` | Selected text and background colors |

To disable Omarchy palette loading and observation, add:

```toml
[omarchy]
enabled = false
```

The Omarchy palette reloads live; edits to Kokuban's own configuration are read
when opening a new window.

Run the Linux integration check with `xdotool`, `xwd`, Fontconfig's command-line
tools and DejaVu fonts installed:

```sh
xvfb-run -a python3 scripts/linux-theme-smoke.py target/release/kokuban
```

The check compares actual X11 pixels and OSC 10/11 color replies before and after
directory replacements, including an existing selection, cursor, all 16 ANSI
colors, explicit overrides, and invalid-file recovery. It verifies that the
child PID and start time remain unchanged and that terminal input still works.
This checks the palette path and renderer under X11; changing themes in a
physical Omarchy/Hyprland session remains a separate on-device validation.

The [2026-09-12 CI run](https://github.com/guicybercode/kokuban.rs/actions/runs/34719320120)
passed both automatic-theme and explicit-override scenarios on revision
`4b05e08`. The [retained evidence](linux-evidence/2026-09-12-omarchy-theme/manifest.json)
includes the original artifact with all seven frame phases per scenario and
the [session report](linux-evidence/2026-09-12-omarchy-theme/report.json).
The same revision passed 702 Linux and 588 macOS executable tests, plus 225
example tests on each platform, all-target checks and Clippy.

## Follow the system font

Linux development builds use Fontconfig's `monospace` family when `font.family`
is omitted. Omarchy's [font selector at `f4378f0`](https://github.com/omacom/omarchy/blob/f4378f0de5b44d331ee943746a97872b718a6c18/bin/omarchy-font-set)
updates that alias in `$HOME/.config/fontconfig/fonts.conf`. Open a new Kokuban
window after changing the Omarchy font; existing windows keep their loaded font.
An explicit `font.family` continues to select the requested family. The default
size remains 14, and macOS retains Menlo as its default family.

The integration check also compares four windows under a private Fontconfig
configuration. It verifies that the omitted family matches the monospace
control, explicit families take priority, and no missing-font fallback hides
an incorrect default.

The [retained before/after evidence](linux-evidence/2026-09-12-system-font/README.md)
shows the old default failing this check at `4799708` and the corrected default
passing at `ccbbb6e` on Linux aarch64/Xvfb. Full-frame pixel comparisons confirm
the default follows `monospace` and explicit families remain respected.
The [CI run at `1c90e0c`](https://github.com/guicybercode/kokuban.rs/actions/runs/34722785959)
also passed after the harness was synchronized with the actual font geometry.

## Validation scope

On 2026-09-10, the launcher was checked against `xdg-terminal-exec` commit
[`065925d`](https://github.com/Vladimir-csp/xdg-terminal-exec/tree/065925df9f419008159258ae169018bfd23df71b)
using isolated XDG directories. Selection returned Kokuban's desktop ID, and
`--print-cmd` preserved app ID, title, working directory, and command arguments.
`scripts/linux-launch-smoke.py` also passed on Debian 12 arm64 under Xvfb and
headless Weston 10, including real PTY responses and clean command shutdown.
The first-frame check passed with the X11 display removed in that Wayland session.

The Rust tests cover argument preservation, non-UTF-8 paths, PATH lookup, and
execution through a real PTY. The Linux window uses winit's Wayland backend when
available and supports X11 as a fallback. Exercise the CLI and PTY integration
with the real Linux binary:

```sh
xvfb-run -a python3 scripts/linux-launch-smoke.py target/release/kokuban
# In an existing native Wayland session:
python3 scripts/linux-launch-smoke.py target/release/kokuban --backend wayland
```

The X11 run checks the window class and initial title. Both runs check command
arguments, working directory, terminal environment, and a DSR response through
the real PTY. The Wayland run does not read window properties back from the
compositor. Native Hyprland rendering, scaling,
clipboard, Omarchy window rules, and the bindings above still need an on-device
run; parser or Xvfb tests alone do not establish those behaviors.

On 2026-11-10 a headless validation ran under sway 1.10.1 (wlroots headless
backend, `xwayland disable`), Weston 14 and Xvfb, on Debian 13 x86_64. Under
sway at output scale 1.25, `scripts/linux-wayland-smoke.py` checked that the
glyph atlas follows `wp_fractional_scale_v1`, that SGR mouse reports name the
cell under the pointer (including the last cell), CLIPBOARD copy and paste
through `wlr-data-control` against `wl-copy`/`wl-paste`, PRIMARY from a drag
and middle-click paste, a text-input-v3 session with purpose `terminal` whose
input-method-v2 preedit never reaches the PTY and whose commit arrives once,
and runtime scale changes to 1.5 and 1.0. Weston covered integer scale 2 and
the launch smoke; it has no data-control, so the clipboard is unavailable
there without XWayland. The X11 clipboard smoke covers the same PRIMARY routes.

```sh
cargo build --manifest-path scripts/wayland-inject/Cargo.toml
python3 scripts/linux-wayland-smoke.py target/debug/kokuban \
  --inject scripts/wayland-inject/target/debug/wayland-inject
```

The input method in that run is a protocol-level test client, not fcitx5 or
IBus, and every input device is virtual. On Hyprland, still verify with a
physical keyboard and mouse: fcitx5 (or IBus) composition and candidate window
placement next to the cursor after a split, monitor scale 1.25 or 1.5 and
moving the window between monitors with different scales, `Super+C`/`Super+V`
and middle-click paste against another application, and that
`hyprctl clients` reports `xwayland: false` for Kokuban.

Linux currently uses software rendering. No same-machine measurement establishes
that Kokuban outperforms Ghostty, Alacritty, or Kitty. See
[Linux performance](LINUX_PERFORMANCE.md) for the benchmark scope and limitations.
