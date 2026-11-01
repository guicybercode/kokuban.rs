# Homebrew cask

`Casks/kokuban.rb` is the Homebrew cask for the macOS app. It is kept here so
it is reviewed and updated together with the release, then copied to the
public tap `guicybercode/homebrew-tap` (repository name must start with
`homebrew-` so that `brew tap guicybercode/tap` resolves it).

The cask installs `Kokuban.app` from the release disk image for the current
architecture (`aarch64` on Apple Silicon, `x86_64` on Intel), checks the SHA-256
that the release publishes next to each image, and prints a caveat about the
ad-hoc signature. `brew uninstall --zap` also removes `~/.config/kokuban`.

## Publishing the tap (first time)

1. Create an empty public GitHub repository named `homebrew-tap` under the
   `guicybercode` account. No README is needed.
2. Copy the cask into it, keeping the `Casks/` directory:

   ```sh
   git clone https://github.com/guicybercode/homebrew-tap.git
   mkdir -p homebrew-tap/Casks
   cp packaging/homebrew/Casks/kokuban.rb homebrew-tap/Casks/
   cd homebrew-tap && git add Casks/kokuban.rb && git commit -m "kokuban 0.4" && git push
   ```

3. Install from anywhere:

   ```sh
   brew install --cask guicybercode/tap/kokuban
   ```

## Updating for a new release

After `gh release create vX.Y ...` has published the two `.dmg.sha256` files:

```sh
scripts/update-homebrew-cask.sh vX.Y   # rewrites version and sha256, prints the diff
```

Commit the change in this repository, then copy `Casks/kokuban.rb` to the tap
repository and push it. Users get the new version with `brew upgrade --cask kokuban`.

## Validating locally before pushing to the tap

```sh
brew tap-new local/kokuban
cp packaging/homebrew/Casks/kokuban.rb "$(brew --repository)/Library/Taps/local/homebrew-kokuban/Casks/"
brew audit --cask --strict local/kokuban/kokuban
brew style --cask local/kokuban/kokuban
brew install --cask local/kokuban/kokuban
brew uninstall --cask local/kokuban/kokuban
brew untap local/kokuban
```

Full release steps are in [docs/RELEASE.md](../../docs/RELEASE.md).
