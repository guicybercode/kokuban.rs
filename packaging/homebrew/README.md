# Homebrew packages

`Casks/kokuban.rb` (the macOS app) and `Formula/kokuban.rs.rb` (the `kokuban` executable
for macOS and Linux) are kept here so they are reviewed and updated together with the
release, then copied to the public tap
[`guicybercode/homebrew-kokuban`](https://github.com/guicybercode/homebrew-kokuban)
(the repository name must start with `homebrew-` so that `brew tap guicybercode/kokuban`
resolves it). The copies in the tap are not edited by hand.

- The **cask** installs `Kokuban.app` from the release disk image for the current
  architecture, checks the SHA-256 that the release publishes next to each image, and
  prints a caveat about the ad-hoc signature. `brew uninstall --zap` also removes
  `~/.config/kokuban`.
- The **formula** installs the executable from the release archive on macOS (Apple Silicon
  and Intel), with the example `kokuban.toml` and assets under `share/kokuban.rs/` and the
  documentation and third-party notices under `share/doc/kokuban.rs/`. On Linux x86_64 it
  builds the GitHub source archive of the tag with Homebrew's Rust, fontconfig and FreeType
  (the release binary needs glibc 2.39) and installs the desktop entry and icon. Homebrew
  reads the version from the download URL, so the tag appears literally in each `url`.

## Updating for a new release

After `gh release create vX.Y ...` has published the assets and their `.sha256` files:

```sh
scripts/update-homebrew.sh vX.Y                                   # rewrites both files, prints the diffs
scripts/update-homebrew.sh vX.Y --tap /path/to/homebrew-kokuban   # also copies them into the tap checkout
```

Commit the change in this repository, then commit and push the copies in the tap.
Users get the new version with `brew upgrade kokuban.rs` or `brew upgrade --cask kokuban`.

## Validating locally before pushing to the tap

With the tap cloned at `$(brew --repository guicybercode/kokuban)`, which is what `brew tap`
does (Homebrew 7 refuses to load a third-party tap until `brew trust guicybercode/kokuban`):

```sh
brew style guicybercode/kokuban
brew audit --strict --online guicybercode/kokuban/kokuban.rs
brew audit --strict --online --os linux --arch intel guicybercode/kokuban/kokuban.rs
brew audit --cask --strict --online guicybercode/kokuban/kokuban
brew install guicybercode/kokuban/kokuban.rs && kokuban --version && brew test kokuban.rs
brew livecheck --newer-only guicybercode/kokuban/kokuban.rs      # prints nothing when current
brew install --cask guicybercode/kokuban/kokuban
brew uninstall kokuban.rs && brew uninstall --cask kokuban
```

The Linux build is exercised by the tap's `brew test-bot` workflow on `ubuntu-24.04`.

Full release steps are in [docs/RELEASE.md](../../docs/RELEASE.md).
