#!/usr/bin/env bash
# Point packaging/homebrew/Casks/kokuban.rb at a published release tag.
#
# Usage: scripts/update-homebrew-cask.sh vX.Y
#
# Downloads the two macOS disk-image checksums that release.yml publishes next
# to each asset, rewrites `version` and both `sha256` stanzas in the cask and
# prints the resulting diff. Nothing is committed; review the diff, commit it,
# then copy the cask to the guicybercode/homebrew-tap repository.
set -euo pipefail

tag="${1:-}"
if [[ ! "$tag" =~ ^v[0-9]+(\.[0-9]+)*$ ]]; then
  echo "usage: $0 vX.Y   (for example v0.5)" >&2
  exit 2
fi
version="${tag#v}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cask="$repo_root/packaging/homebrew/Casks/kokuban.rb"
base_url="https://github.com/guicybercode/kokuban.rs/releases/download/$tag"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# fetch_sha <arch>: print the SHA-256 published for kokuban-<tag>-<arch>-apple-darwin.dmg.
fetch_sha() {
  local arch="$1"
  local asset="kokuban-$tag-$arch-apple-darwin.dmg"
  local out="$tmp/$arch.sha256"
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

arm_sha="$(fetch_sha aarch64)"
intel_sha="$(fetch_sha x86_64)"

awk -v version="$version" -v arm_sha="$arm_sha" -v intel_sha="$intel_sha" '
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

if cmp -s "$cask" "$tmp/kokuban.rb"; then
  echo "$cask already matches $tag (version $version); nothing to change."
  exit 0
fi

cp "$cask" "$tmp/before.rb"
cp "$tmp/kokuban.rb" "$cask"
echo "Updated $cask to $tag:"
diff -u --label "a/packaging/homebrew/Casks/kokuban.rb" --label "b/packaging/homebrew/Casks/kokuban.rb" \
  "$tmp/before.rb" "$cask" || true
