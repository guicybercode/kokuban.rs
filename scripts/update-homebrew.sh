#!/usr/bin/env bash
# Point the Homebrew cask and formula in packaging/homebrew at a published release tag.
#
# Usage: scripts/update-homebrew.sh vX.Y [--tap DIR]
#
# Downloads the four checksums that release.yml publishes next to each asset
# (two macOS disk images for the cask, two macOS tarballs for the formula),
# hashes the GitHub source archive of the tag (the formula builds from source
# on Linux), rewrites `version`, the download URLs and every `sha256` stanza,
# and prints the resulting diffs. Nothing is committed; review the diffs, commit them,
# then copy both files to the guicybercode/homebrew-kokuban repository
# (`--tap DIR` copies them into that checkout for you, still uncommitted).
set -euo pipefail

tag=""
tap_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tap)
      [[ $# -ge 2 ]] || { echo "error: --tap needs a directory" >&2; exit 2; }
      tap_dir="$2"
      shift 2
      ;;
    -*)
      echo "usage: $0 vX.Y [--tap DIR]   (for example v0.5)" >&2
      exit 2
      ;;
    *)
      if [[ -n "$tag" ]]; then
        echo "usage: $0 vX.Y [--tap DIR]   (for example v0.5)" >&2
        exit 2
      fi
      tag="$1"
      shift
      ;;
  esac
done
if [[ ! "$tag" =~ ^v[0-9]+(\.[0-9]+)*$ ]]; then
  echo "usage: $0 vX.Y [--tap DIR]   (for example v0.5)" >&2
  exit 2
fi
version="${tag#v}"
if [[ -n "$tap_dir" && ! -d "$tap_dir" ]]; then
  echo "error: --tap $tap_dir is not a directory" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cask="$repo_root/packaging/homebrew/Casks/kokuban.rb"
formula="$repo_root/packaging/homebrew/Formula/kokuban.rs.rb"
base_url="https://github.com/guicybercode/kokuban.rs/releases/download/$tag"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# fetch_sha <asset>: print the SHA-256 published for kokuban-<tag>-<asset>.
fetch_sha() {
  local asset="kokuban-$tag-$1"
  local out="$tmp/$1.sha256"
  if ! curl -fsSL -o "$out" "$base_url/$asset.sha256"; then
    echo "error: could not download $base_url/$asset.sha256 (is $tag published with its assets?)" >&2
    exit 1
  fi
  local sum
  sum="$(awk 'NR == 1 { print $1 }' "$out")"
  if [[ ! "$sum" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: $asset.sha256 does not start with a SHA-256 digest" >&2
    exit 1
  fi
  if ! grep -q "$asset" "$out"; then
    echo "error: $asset.sha256 does not name $asset" >&2
    exit 1
  fi
  printf '%s\n' "$sum"
}

dmg_arm_sha="$(fetch_sha aarch64-apple-darwin.dmg)"
dmg_intel_sha="$(fetch_sha x86_64-apple-darwin.dmg)"
tar_mac_arm_sha="$(fetch_sha aarch64-apple-darwin.tar.gz)"
tar_mac_intel_sha="$(fetch_sha x86_64-apple-darwin.tar.gz)"
source_url="https://github.com/guicybercode/kokuban.rs/archive/refs/tags/$tag.tar.gz"
if ! curl -fsSL -o "$tmp/source.tar.gz" "$source_url"; then
  echo "error: could not download $source_url (does the tag exist?)" >&2
  exit 1
fi
source_sha="$(shasum -a 256 "$tmp/source.tar.gz" | awk '{ print $1 }')"

# The cask keeps `version` and builds its URLs from it.
awk -v version="$version" -v arm_sha="$dmg_arm_sha" -v intel_sha="$dmg_intel_sha" '
  /^  version "/ { sub(/"[^"]*"/, "\"" version "\"") }
  /^  on_arm do$/ { block = "arm" }
  /^  on_intel do$/ { block = "intel" }
  /^  end$/ { block = "" }
  /^    sha256 "/ {
    if (block == "arm") sub(/"[^"]*"/, "\"" arm_sha "\"")
    else if (block == "intel") sub(/"[^"]*"/, "\"" intel_sha "\"")
  }
  { print }
' "$cask" > "$tmp/kokuban.rb"

# The formula has no `version` stanza (Homebrew reads it from the URL), so the
# tag appears literally in each `url` inside on_macos/on_linux + on_arm/on_intel:
# release tarballs on macOS, the source archive of the tag on Linux.
awk -v tag="$tag" -v mac_arm="$tar_mac_arm_sha" -v mac_intel="$tar_mac_intel_sha" \
    -v linux_intel="$source_sha" '
  /^  on_macos do$/ { os = "macos" }
  /^  on_linux do$/ { os = "linux" }
  /^  end$/ { os = "" }
  /^    on_arm do$/ { arch = "arm" }
  /^    on_intel do$/ { arch = "intel" }
  /^    end$/ { arch = "" }
  /^      url "/ {
    sub(/download\/v[0-9.]+\/kokuban-v[0-9.]+-/, "download/" tag "/kokuban-" tag "-")
    sub(/archive\/refs\/tags\/v[0-9.]+\.tar\.gz/, "archive/refs/tags/" tag ".tar.gz")
  }
  /^      sha256 "/ {
    if (os == "macos" && arch == "arm") sub(/"[^"]*"/, "\"" mac_arm "\"")
    else if (os == "macos" && arch == "intel") sub(/"[^"]*"/, "\"" mac_intel "\"")
    else if (os == "linux" && arch == "intel") sub(/"[^"]*"/, "\"" linux_intel "\"")
  }
  { print }
' "$formula" > "$tmp/kokuban.rs.rb"

if [[ "$(grep -c "download/$tag/kokuban-$tag-" "$tmp/kokuban.rs.rb")" -ne 2 ]] \
  || ! grep -q "archive/refs/tags/$tag.tar.gz" "$tmp/kokuban.rs.rb"; then
  echo "error: the url lines in $formula do not match the expected layout" >&2
  exit 1
fi

changed=0
# apply <file> <rendered> <label>: install the rendered file and print its diff.
apply() {
  local file="$1" rendered="$2" label="$3"
  if cmp -s "$file" "$rendered"; then
    echo "$label already matches $tag; nothing to change."
    return
  fi
  cp "$file" "$tmp/before"
  cp "$rendered" "$file"
  changed=1
  echo "Updated $label to $tag:"
  diff -u --label "a/$label" --label "b/$label" "$tmp/before" "$file" || true
}
apply "$cask" "$tmp/kokuban.rb" "packaging/homebrew/Casks/kokuban.rb"
apply "$formula" "$tmp/kokuban.rs.rb" "packaging/homebrew/Formula/kokuban.rs.rb"

if [[ -n "$tap_dir" ]]; then
  mkdir -p "$tap_dir/Casks" "$tap_dir/Formula"
  cp "$cask" "$tap_dir/Casks/kokuban.rb"
  cp "$formula" "$tap_dir/Formula/kokuban.rs.rb"
  echo "Copied both files into $tap_dir (not committed)."
fi

if [[ "$changed" -eq 1 ]]; then
  echo "Next: git add packaging/homebrew && git commit, then commit the copies in the tap."
fi
