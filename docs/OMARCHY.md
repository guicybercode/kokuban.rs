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

## Install and select

Build on the Omarchy machine with the Rust toolchain, compiler tools, pkg-config,
Fontconfig, and FreeType installed. The launcher installation below also uses
`desktop-file-install` from `desktop-file-utils`:

```sh
cargo build --locked --release
```

Install the binary and launcher from the repository root:

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

Omarchy's standard [UWSM environment](https://github.com/basecamp/omarchy/blob/master/config/uwsm/env)
includes `~/.local/bin` in `PATH`. Confirm `command -v kokuban` finds the newly
installed executable. The installed launcher uses its absolute path; with a
customized session, also include the binary directory in the session's `PATH`
to use `kokuban` directly from a shell.

Add the following as the **first terminal entry** in
`${XDG_CONFIG_HOME:-$HOME/.config}/xdg-terminals.list`, preserving the other entries
as fallbacks:

```text
io.github.guicybercode.kokuban.desktop
```

Then verify selection and translated arguments:

```sh
xdg-terminal-exec --print-id
xdg-terminal-exec --print-cmd --app-id=org.omarchy.btop --dir="$HOME" -e btop
```

The first command should report `io.github.guicybercode.kokuban.desktop`; the
second should include `--app-id=org.omarchy.btop`, `--working-directory=...`, and
`-e btop`. Desktop-specific lists such as `hyprland-xdg-terminals.list` take
precedence over the generic list; check them if selection differs. These entry
keys and preference rules follow the
[xdg-terminal-exec specification](https://github.com/Vladimir-csp/xdg-terminal-exec/blob/master/README.md).

The current [Omarchy terminal selector](https://github.com/basecamp/omarchy/blob/master/bin/omarchy-default-terminal)
accepts Alacritty, Foot, Ghostty, and Kitty. Register Kokuban through the preference
file above. Selecting another terminal from Omarchy's menu can replace that file.

## Join Omarchy's terminal window rules

The [terminal rules in Omarchy `f4378f0`](https://github.com/omacom/omarchy/blob/f4378f0de5b44d331ee943746a97872b718a6c18/default/hypr/apps/terminals.conf),
inspected on 2026-09-11, do not include Kokuban's normal window class. Add this
block once to your own `~/.config/hypr/looknfeel.conf`:

```ini
# Kokuban terminal integration
windowrule = tag +terminal, match:class ^(io[.]github[.]guicybercode[.]kokuban)$
```

Omarchy [loads this user file after its defaults](https://github.com/omacom/omarchy/blob/f4378f0de5b44d331ee943746a97872b718a6c18/config/hypr/hyprland.conf).
The rule uses the `.conf` syntax of that Omarchy revision and
[Hyprland 0.54](https://wiki.hypr.land/0.54.0/Configuring/Window-Rules/).
Check the configuration format in your installation before applying it to a
newer Hyprland release. Keep the rule in the user file so Omarchy updates can
replace their own defaults independently.

Reload, check for configuration errors, and open a normal Kokuban window:

```sh
hyprctl reload
hyprctl configerrors
hyprctl clients -j | jq '.[] | select(.class == "io.github.guicybercode.kokuban") |
  {class, initialClass, tags, xwayland}'
```

Confirm the terminal tag appears (dynamic tags may be displayed as `terminal*`),
`xwayland` is `false`, and the window has the expected terminal appearance.
Custom `org.omarchy.<command>` IDs continue to use Omarchy's corresponding TUI
rules. To undo this addition, remove only the block above and reload Hyprland.
These checks require a running Omarchy/Hyprland session; they have not yet been
executed on a physical Omarchy machine for this project.

## Use the existing Omarchy bindings

Omarchy's [terminal binding](https://github.com/basecamp/omarchy/blob/master/config/hypr/bindings.conf)
launches `uwsm-app -- xdg-terminal-exec --dir="$(omarchy-cmd-terminal-cwd)"`.
Its [TUI launcher](https://github.com/basecamp/omarchy/blob/master/bin/omarchy-launch-tui)
passes an `org.omarchy.<command>` app ID and the requested command. Kokuban's
desktop entry translates both forms, and its window class uses the requested
ID on Wayland and X11.

After selecting Kokuban, exercise the existing entry points on Omarchy:

```sh
omarchy-launch-tui btop
xdg-terminal-exec --dir="$HOME" -e nvim
```

Also open a login shell, change directory, and press Super+Return: the new terminal
should inherit that directory. Kokuban's shell is its direct child, which matches
the process relationship inspected by
[omarchy-cmd-terminal-cwd](https://github.com/basecamp/omarchy/blob/master/bin/omarchy-cmd-terminal-cwd).

For persistent settings use `${XDG_CONFIG_HOME:-$HOME/.config}/kokuban/kokuban.toml`.
A `kokuban.toml` in the directory where Kokuban itself was launched takes priority;
the CLI working directory is applied after configuration loading.

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

Linux currently uses software rendering. No same-machine measurement establishes
that Kokuban outperforms Ghostty, Alacritty, or Kitty. See
[Linux performance](LINUX_PERFORMANCE.md) for the benchmark scope and limitations.
