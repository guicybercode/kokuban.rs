#!/usr/bin/env bash
# Idempotent Cloud Agent bootstrap for kokuban.
# Installs the Linux display/font system libraries and the pinned Rust
# toolchain, then warms the Cargo build cache. Safe to run repeatedly.
set -euo pipefail

# Rust toolchain pinned to match .github/workflows/ci.yml.
RUST_VERSION="1.94.1"

echo "==> Installing Linux display and font system dependencies"
export DEBIAN_FRONTEND=noninteractive
sudo apt-get update
sudo apt-get install --yes --no-install-recommends \
  fonts-dejavu-core \
  libfontconfig1-dev \
  libfreetype6-dev \
  libxkbcommon-x11-0 \
  pkg-config \
  xauth \
  xvfb

echo "==> Ensuring Rust ${RUST_VERSION} with clippy is installed and default"
rustup toolchain install "${RUST_VERSION}" --profile minimal --component clippy
rustup default "${RUST_VERSION}"
rustc --version

echo "==> Fetching and building crate dependencies (locked)"
cargo fetch --locked
cargo build --locked --all-targets

echo "==> kokuban environment ready"
