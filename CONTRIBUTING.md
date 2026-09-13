# Contributing to Kokuban

Bug reports, focused fixes, tests, documentation and platform feedback are welcome. Read the [README](README.md) for setup and the [roadmap](docs/ROADMAP.md) for current capabilities. Report vulnerabilities through [SECURITY.md](SECURITY.md), not a public issue. All community participation follows our [Code of Conduct](CODE_OF_CONDUCT.md).

## Set up a development checkout

Install Git, Rust through [rustup](https://rustup.rs/) and the native dependencies listed in the README. CI uses Rust **1.94.1**:

```sh
git clone https://github.com/guicybercode/kokuban.rs.git
cd kokuban.rs
rustup toolchain install 1.94.1 --profile minimal --component clippy,rustfmt
git switch -c fix/short-description
rustup run 1.94.1 cargo build --locked
./target/debug/kokuban
```

Fork the repository first if you do not have push access. Use your fork as the push destination. Run the application from a graphical desktop session; a headless SSH shell without a display cannot open a window.

## Make a focused change

- Keep changes small and preserve unrelated work. Avoid whole-file formatting of legacy code when only a few lines change.
- Keep platform-specific dependencies behind their target configuration. Shared parser, grid, image, input and PTY changes must preserve macOS and Linux behavior.
- Add meaningful regression coverage for behavior changes, especially parser input, memory limits, subprocess cleanup, Unicode and image handling. Documentation-only changes need link and command review rather than implementation-mirroring tests.
- Keep configuration, README examples and platform claims consistent with the actual implementation. Distinguish compilation from runtime validation.
- Do not commit credentials, private signing keys, SDKs, `target/`, personal logs or unreviewed generated output. Preserve third-party notices.

Android development currently lives on `codex/android-native`. Coordinate shared API changes using the [Android integration handoff](docs/ANDROID_SHARED_INTEGRATION.md); an experimental branch is not part of the desktop v0.2 release.

## Validate your change

From the repository root:

```sh
rustup run 1.94.1 cargo check --locked --all-targets
rustup run 1.94.1 cargo test --locked --all-targets
rustup run 1.94.1 cargo clippy --locked --all-targets
git diff --check
```

Format the code you change with rustfmt while avoiding unrelated churn. Run the checks supported by your platform and let CI verify the other platform. For Linux interaction, clipboard and SSH checks, see [Linux application validation](docs/LINUX_APPS.md); for images/video and measurements, see [video validation](docs/LINUX_VIDEO.md) and [release measurements](docs/LINUX_PERFORMANCE.md). Report exactly what ran and what remains untested.

## Open a pull request

Explain the problem, resulting behavior, relevant checks and any limitations. Link the related issue when one exists. Include a screenshot or reproducible terminal sequence when it helps review a visual change. Changes affecting architecture or platform contracts should be discussed before a large refactor.

Use descriptive commits. Preserve actual contributor attribution and do not add automatic Codex or other AI coauthor trailers. Push normally; avoid rewriting shared branches. Treat reviewers and other contributors respectfully, and keep discussions focused on the work.

## Licensing

By submitting original code or documentation intended for inclusion in this repository, you agree to license your contribution under [BSD-4-Clause](LICENSE), the project license starting with v0.2. Read its redistribution, advertising acknowledgment and endorsement conditions before contributing. Only submit work you have the right to contribute.

Third-party code, content and assets keep their own licenses and required notices. In particular, the derived Unicode tables retain their MIT notices and the Contributor Covenant code of conduct retains CC-BY-4.0; the project license does not relicense them. The published v0.1 release remains under MIT. See [third-party licensing](docs/THIRD_PARTY.md).
