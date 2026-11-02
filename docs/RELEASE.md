# Release checklist

How a Kokuban release goes from `main` to the GitHub release page and the
Homebrew tap. Versions are `vX.Y` Git tags; the Cargo package version is
`X.Y.0`. [release.yml](../.github/workflows/release.yml) only builds and
uploads workflow artifacts; publishing is a manual `gh release` step so the
packages can be reviewed first.

## 1. Prepare `main`

- [ ] Bump the version in `Cargo.toml` (and `Cargo.lock`), `CITATION.cff` and the
      supported-versions table in `SECURITY.md`.
- [ ] Turn `## Unreleased` in `CHANGELOG.md` into `## vX.Y — YYYY-MM-DD` and start
      a fresh empty `## Unreleased` above it.
- [ ] Update the version in the README install commands and in `docs/INSTALL.md`
      (download URLs, `git clone --branch`).
- [ ] Open a PR, let CI pass, merge it.
- [ ] Optional dry run: `gh workflow run release.yml -f release_tag=vX.Y --ref <branch>`
      builds candidate packages from any ref without a tag.

## 2. Tag and build

```sh
git checkout main && git pull
git tag vX.Y && git push origin vX.Y        # triggers release.yml
gh run watch "$(gh run list --workflow release.yml --limit 1 --json databaseId -q '.[0].databaseId')"
```

`release.yml` builds three tarballs (Linux x86_64, macOS arm64, macOS x86_64) and
two macOS disk images, each with a `.sha256`, and uploads them as workflow artifacts.

## 3. Publish the GitHub release

```sh
mkdir -p dist && gh run download <run-id> --dir dist     # one subdirectory per target
find dist -name '*.sha256' -execdir shasum -a 256 --check {} \;   # every line OK
# notes.md: the vX.Y section of CHANGELOG.md
gh release create vX.Y --title "vX.Y" --notes-file notes.md dist/*/kokuban-vX.Y-*
```

Check the release page lists ten assets (five packages, five checksums).
`releases/latest` can keep pointing at the previous tag for about a minute; the
in-app updater (`kokuban --update`) follows that redirect.

## 4. Update the Homebrew tap

```sh
scripts/update-homebrew.sh vX.Y      # downloads the .sha256 files and the source archive, rewrites cask and formula, prints the diffs
git add packaging/homebrew
git commit -m "build: cask e formula do Homebrew para a vX.Y"
git push
```

Then copy both files to the tap repository and push them (`--tap DIR` makes the copy):

```sh
git clone https://github.com/guicybercode/homebrew-kokuban.git /tmp/homebrew-kokuban
scripts/update-homebrew.sh vX.Y --tap /tmp/homebrew-kokuban
git -C /tmp/homebrew-kokuban add Casks/kokuban.rb Formula/kokuban.rs.rb
git -C /tmp/homebrew-kokuban commit -m "kokuban X.Y"
git -C /tmp/homebrew-kokuban push
```

The tap's `brew test-bot` workflow then audits and installs both packages on macOS (Apple Silicon
and Intel) and Linux. Verify from a clean state: `brew update && brew install guicybercode/kokuban/kokuban.rs`
(or `brew upgrade kokuban.rs`) and `kokuban --version`; for the app `brew install --cask guicybercode/kokuban/kokuban`
then `codesign -dv /Applications/Kokuban.app`.
[packaging/homebrew/README.md](../packaging/homebrew/README.md) has the local audit steps to run before pushing.

## Notarization (not yet done)

The disk images are ad-hoc signed by `scripts/package-macos-dmg.sh`, so Gatekeeper
asks users to confirm the first launch (see the cask caveat and
[INSTALL.md](INSTALL.md#homebrew-macos-and-linux)). Notarizing removes that prompt. It needs a paid
Apple Developer Program membership and these pieces:

| Secret / value | What it is |
| --- | --- |
| `MACOS_CERTIFICATE_P12` | Base64 of a **Developer ID Application** certificate exported as `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | Password used when exporting the `.p12` |
| `APPLE_ID` | Apple ID e-mail of the developer account |
| `APPLE_TEAM_ID` | Ten-character Team ID shown in the developer portal |
| `APPLE_APP_SPECIFIC_PASSWORD` | App-specific password created at appleid.apple.com for `notarytool` |

Store them as repository secrets and, in the macOS job of `release.yml`, import
the certificate into a temporary keychain before packaging:

```sh
echo "$MACOS_CERTIFICATE_P12" | base64 --decode > "$RUNNER_TEMP/cert.p12"
security create-keychain -p "" "$RUNNER_TEMP/build.keychain"
security default-keychain -s "$RUNNER_TEMP/build.keychain"
security unlock-keychain -p "" "$RUNNER_TEMP/build.keychain"
security import "$RUNNER_TEMP/cert.p12" -k "$RUNNER_TEMP/build.keychain" \
  -P "$MACOS_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k "" "$RUNNER_TEMP/build.keychain"
```

Then replace the two ad-hoc `codesign --sign -` calls in `scripts/package-macos-dmg.sh`
with a Developer ID signature that enables the hardened runtime and a secure timestamp
(both are required by the notary service), and notarize the finished disk image:

```sh
codesign --force --options runtime --timestamp \
  --sign "Developer ID Application: <Name> ($APPLE_TEAM_ID)" Kokuban.app/Contents/MacOS/kokuban
codesign --force --options runtime --timestamp \
  --sign "Developer ID Application: <Name> ($APPLE_TEAM_ID)" Kokuban.app
codesign --verify --deep --strict Kokuban.app

xcrun notarytool submit dist/kokuban-vX.Y-<target>.dmg \
  --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
  --password "$APPLE_APP_SPECIFIC_PASSWORD" --wait
xcrun stapler staple dist/kokuban-vX.Y-<target>.dmg
spctl --assess --type open --context context:primary-signature -v dist/kokuban-vX.Y-<target>.dmg
```

Compute the `.sha256` files **after** stapling, since stapling changes the image.
Once releases are notarized, drop the `caveats` block from the cask and the ad-hoc
notes in the README and `docs/INSTALL.md`.
