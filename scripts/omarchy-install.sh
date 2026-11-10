#!/usr/bin/env bash
# Install Kokuban for the current user on Omarchy and make it the default terminal.
#
# Usage:
#   scripts/omarchy-install.sh [install] [--binary PATH] [--no-default] [--no-hypr]
#                              [--hypr-format lua|conf]
#   scripts/omarchy-install.sh uninstall
#
# install (the default) does three independent things, each reversible by
# `uninstall` and each safe to repeat:
#
# 1. Copies the executable to ~/.local/bin/kokuban, the icon to
#    $XDG_DATA_HOME/kokuban/ and the desktop entry, with absolute Exec and Icon
#    paths, to $XDG_DATA_HOME/applications/. The entry keeps its X-TerminalArg*
#    keys, which xdg-terminal-exec uses to pass --app-id, --dir, --title and -e.
# 2. Puts io.github.guicybercode.kokuban.desktop first in the user's
#    xdg-terminals.list (or in an existing desktop-specific list such as
#    hyprland-xdg-terminals.list, which takes precedence), keeping every other
#    entry as a fallback. Omarchy's Super+Return and omarchy-launch-tui go
#    through xdg-terminal-exec.
# 3. Tags Kokuban windows as `terminal` in the user's Hyprland entry file:
#    ~/.config/hypr/hyprland.lua on current Omarchy (Lua configuration), or
#    ~/.config/hypr/hyprland.conf on older releases. With the Lua configuration,
#    Omarchy's Super+C/Super+V send Ctrl+Shift+C/V to windows with that tag and
#    plain Ctrl+C/V to every other window, so without it Super+C would reach the
#    shell as an interrupt. Omarchy's own defaults are never edited.
#
# Build first with `cargo build --locked --release` or pass --binary.
set -euo pipefail

readonly DESKTOP_ID="io.github.guicybercode.kokuban.desktop"
readonly MARK_BEGIN=">>> kokuban terminal tag >>>"
readonly MARK_END="<<< kokuban terminal tag <<<"
readonly LIST_MARK="# Created by Kokuban scripts/omarchy-install.sh"

repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
action="install"
binary="$repository/target/release/kokuban"
set_default=true
set_hypr=true
hypr_format=""

usage() {
  sed -n '3,8p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

fail() {
  echo "error: $*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    install | uninstall) action="$1"; shift ;;
    --binary)
      [[ $# -ge 2 ]] || usage
      binary="$2"; shift 2 ;;
    --binary=*) binary="${1#--binary=}"; shift ;;
    --no-default) set_default=false; shift ;;
    --no-hypr) set_hypr=false; shift ;;
    --hypr-format)
      [[ $# -ge 2 ]] || usage
      hypr_format="$2"; shift 2 ;;
    --hypr-format=*) hypr_format="${1#--hypr-format=}"; shift ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
case "$hypr_format" in "" | lua | conf) ;; *) fail "--hypr-format must be lua or conf" ;; esac

[[ "$(uname -s)" == Linux ]] || fail "Omarchy integration is Linux-only"
[[ -n "${HOME:-}" && "$HOME" == /* ]] || fail "HOME must be an absolute path"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
[[ "$config_home" == /* ]] || config_home="$HOME/.config"
[[ "$data_home" == /* ]] || data_home="$HOME/.local/share"

bin_path="$HOME/.local/bin/kokuban"
icon_dir="$data_home/kokuban"
icon_path="$icon_dir/kokuban-icon.png"
desktop_path="$data_home/applications/$DESKTOP_ID"
hypr_dir="$config_home/hypr"

# Desktop entry values cannot hold these unescaped; such a HOME is not supported.
case "$bin_path$icon_path" in
  *[\"\`\$\\%]* | *$'\n'*) fail "install paths must not contain quotes, \$, \\, % or newlines" ;;
esac

# Configuration files are often symbolic links into a dotfiles repository.
# Print the file to edit: the link's final target, so the link itself survives.
# A link to a missing file, to a directory or in a loop is refused.
edit_target() {
  local path="$1" target
  if [[ ! -L "$path" ]]; then
    printf '%s\n' "$path"
    return
  fi
  if ! target="$(readlink -f -- "$path")" || [[ ! -f "$target" ]]; then
    fail "$path is a symbolic link to $(readlink -- "$path"), which is not an existing regular file; nothing was changed. Fix the link (or replace it with a file) and run this script again."
  fi
  printf '%s\n' "$target"
}

# Write a file through a temporary sibling so readers never see half a file.
# A symbolic link is kept and the file it points at is replaced instead.
replace_file() {
  local path content="$2" temporary
  path="$(edit_target "$1")" || exit 1
  temporary="$(mktemp "$path.kokuban-XXXXXX")"
  printf '%s' "$content" >"$temporary"
  if [[ -e "$path" ]]; then
    chmod --reference="$path" "$temporary" 2>/dev/null || true
  fi
  mv -f "$temporary" "$path"
}

# --- xdg-terminal-exec preference -----------------------------------------

terminal_lists() {
  # Desktop-specific lists for $XDG_CURRENT_DESKTOP, in its order, then the
  # generic list; this is xdg-terminal-exec's order inside one directory.
  # Outside the graphical session (a TTY or SSH) assume Omarchy's Hyprland.
  local desktop
  local IFS=:
  for desktop in ${XDG_CURRENT_DESKTOP:-Hyprland}; do
    [[ -n "$desktop" ]] && printf '%s\n' "$config_home/${desktop,,}-xdg-terminals.list"
  done
  printf '%s\n' "$config_home/xdg-terminals.list"
}

preference_target() {
  local list
  while IFS= read -r list; do
    if [[ -f "$list" ]]; then
      printf '%s\n' "$list"
      return
    fi
  done < <(terminal_lists)
  printf '%s\n' "$config_home/xdg-terminals.list"
}

is_entry() {
  local line="$1"
  line="${line#"${line%%[![:space:]]*}"}"
  line="${line%"${line##*[![:space:]]}"}"
  [[ "$line" == "$DESKTOP_ID" ]]
}

prefer_kokuban() {
  local list content="" line inserted=false
  list="$(preference_target)"
  mkdir -p "$(dirname "$list")"
  if [[ ! -e "$list" ]]; then
    content="# Terminal emulator preference order for xdg-terminal-exec"$'\n'"$LIST_MARK"$'\n'
  else
    # Keep the leading comment block, then Kokuban, then everything else.
    while IFS= read -r line || [[ -n "$line" ]]; do
      if is_entry "$line"; then
        continue
      fi
      if ! $inserted && [[ -n "$line" && "$line" != \#* ]]; then
        content+="$DESKTOP_ID"$'\n'
        inserted=true
      fi
      content+="$line"$'\n'
    done <"$list"
  fi
  $inserted || content+="$DESKTOP_ID"$'\n'
  replace_file "$list" "$content"
  echo "Default terminal: $DESKTOP_ID is first in $list"
}

forget_preference() {
  local list content line entries
  # Every user list, whichever desktop the installation ran under.
  for list in "$config_home"/*-xdg-terminals.list "$config_home/xdg-terminals.list"; do
    [[ -f "$list" ]] || continue
    grep -qF "$DESKTOP_ID" "$list" || continue
    content=""
    entries=0
    while IFS= read -r line || [[ -n "$line" ]]; do
      is_entry "$line" && continue
      [[ -n "$line" && "$line" != \#* ]] && entries=$((entries + 1))
      content+="$line"$'\n'
    done <"$list"
    if [[ $entries -eq 0 && ! -L "$list" ]] && grep -qxF "$LIST_MARK" "$list"; then
      rm -f "$list"
      echo "Removed $list"
    else
      replace_file "$list" "$content"
      echo "Removed $DESKTOP_ID from $list"
    fi
  done
}

# --- Hyprland terminal tag -------------------------------------------------

hypr_entry() {
  local format="$hypr_format"
  if [[ -z "$format" ]]; then
    # -L too: a broken link is reported by the preflight, not silently skipped.
    if [[ -f "$hypr_dir/hyprland.lua" || -L "$hypr_dir/hyprland.lua" ]]; then
      format=lua
    elif [[ -f "$hypr_dir/hyprland.conf" || -L "$hypr_dir/hyprland.conf" ]]; then
      format=conf
    else
      return 1
    fi
  fi
  printf '%s %s\n' "$format" "$hypr_dir/hyprland.$format"
}

hypr_block() {
  if [[ "$1" == lua ]]; then
    cat <<'LUA'
-- >>> kokuban terminal tag >>>
-- Omarchy's Super+C/Super+V send Ctrl+Shift+C/V only to windows tagged
-- "terminal" (default/hypr/bindings/clipboard.lua). Remove this block with
-- scripts/omarchy-install.sh uninstall from the Kokuban repository.
o.window("io\\.github\\.guicybercode\\.kokuban", { tag = "+terminal" })
-- <<< kokuban terminal tag <<<
LUA
  else
    cat <<'CONF'
# >>> kokuban terminal tag >>>
# Apply Omarchy's terminal window rules to Kokuban. Remove this block with
# scripts/omarchy-install.sh uninstall from the Kokuban repository.
windowrule = tag +terminal, match:class ^(io[.]github[.]guicybercode[.]kokuban)$
# <<< kokuban terminal tag <<<
CONF
  fi
}

# Print the file without a previously installed block (and the blank line the
# installer put before it).
strip_block() {
  awk -v begin="$MARK_BEGIN" -v end="$MARK_END" '
    index($0, begin) { skipping = 1; held = 0; next }
    skipping { if (index($0, end)) skipping = 0; next }
    { if (held) { print ""; held = 0 }
      if ($0 == "") { held = 1; next }
      print }
    END { if (held) print "" }
  ' "$1"
}

tag_kokuban() {
  local entry format path content
  if ! entry="$(hypr_entry)"; then
    echo "Hyprland: no hyprland.lua or hyprland.conf in $hypr_dir; skipped the terminal tag"
    return
  fi
  format="${entry%% *}"
  path="${entry#* }"
  content="$(strip_block "$path")"
  [[ -n "$content" ]] && content+=$'\n\n'
  content+="$(hypr_block "$format")"$'\n'
  replace_file "$path" "$content"
  echo "Hyprland ($format): Kokuban windows tagged terminal in $path"
}

untag_kokuban() {
  local format path content
  for format in lua conf; do
    path="$hypr_dir/hyprland.$format"
    [[ -f "$path" ]] && grep -qF "$MARK_BEGIN" "$path" || continue
    content="$(strip_block "$path")"
    [[ -n "$content" ]] && content+=$'\n'
    replace_file "$path" "$content"
    echo "Hyprland ($format): removed the Kokuban block from $path"
  done
}

# --- Files -----------------------------------------------------------------

install_files() {
  [[ -f "$binary" && -x "$binary" ]] ||
    fail "$binary is not an executable; run cargo build --locked --release or pass --binary"
  install -Dm755 "$binary" "$bin_path"
  install -Dm644 "$repository/assets/kokuban-icon.png" "$icon_path"
  mkdir -p "$(dirname "$desktop_path")"
  local content
  content="$(awk -v exec_line="Exec=\"$bin_path\"" -v icon_line="Icon=$icon_path" \
                 -v path_line="Path=$HOME" '
    /^\[/ { in_entry = ($0 == "[Desktop Entry]") }
    in_entry && /^Exec=/ { print exec_line; next }
    in_entry && /^Icon=/ { print icon_line; next }
    in_entry && /^Path=/ { next }
    { print }
    $0 == "[Desktop Entry]" { print path_line }
  ' "$repository/assets/$DESKTOP_ID")"
  replace_file "$desktop_path" "$content"$'\n'
  chmod 644 "$desktop_path"
  if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$desktop_path"
  fi
  echo "Installed $bin_path, $icon_path and $desktop_path"
}

remove_files() {
  rm -f "$bin_path" "$icon_path" "$desktop_path"
  rmdir "$icon_dir" 2>/dev/null || true
  echo "Removed $bin_path, $icon_path and $desktop_path"
}

# --- Preflight -------------------------------------------------------------

# Refuse, before the first change, every file the action could not edit safely.
preflight() {
  local entry path
  [[ "$action" == install ]] || return 0
  if $set_default; then
    edit_target "$(preference_target)" >/dev/null || exit 1
  fi
  if $set_hypr && entry="$(hypr_entry)"; then
    path="${entry#* }"
    path="$(edit_target "$path")" || exit 1
    [[ -f "$path" ]] || fail "$path does not exist; it must be Hyprland's entry file"
  fi
}

preflight
if [[ "$action" == install ]]; then
  install_files
  $set_default && prefer_kokuban
  $set_hypr && tag_kokuban
  cat <<EOF

Check the selection and the Hyprland rule from the Omarchy session:
  xdg-terminal-exec --print-id
  hyprctl configerrors
  hyprctl clients -j | jq '.[] | select(.class == "io.github.guicybercode.kokuban") | {class, tags}'
Selecting another terminal in Omarchy's menu rewrites xdg-terminals.list;
run this script again to choose Kokuban.
EOF
else
  forget_preference
  untag_kokuban
  remove_files
fi
