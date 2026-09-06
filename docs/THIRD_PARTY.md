# Third-party software in release packages

Kokuban's own code is offered under the [MIT license](../LICENSE). Dependencies
keep their own licenses. Each release archive includes
`THIRD_PARTY_LICENSES.txt` and `THIRD_PARTY_SOURCES/option-ext-0.2.0.crate` beside
the executable. Keep these files when redistributing that archive.

## Generation and scope

Run the generator after the locked release build, using Python 3.11 or newer:

```sh
python3 scripts/third-party-notices.py \
  --target x86_64-unknown-linux-gnu \
  --output /tmp/kokuban-release/THIRD_PARTY_LICENSES.txt \
  --sources-dir /tmp/kokuban-release/THIRD_PARTY_SOURCES
```

Use an empty source output directory. `--offline` prevents Cargo from fetching
missing packages. `--metadata-json FILE` accepts metadata already generated for
the supplied target, for tests or repeatable inspection; the caller must keep the
target, workspace and metadata consistent. The release workflow runs Cargo
directly, without this override.

The script runs `cargo metadata --locked --format-version 1 --filter-platform
TARGET`, follows the root package's normal and build dependencies using package
IDs, and excludes edges used only for development. Cargo evaluates platform
conditions. Packages used by build scripts and procedural macros remain in the
list conservatively; this is a dependency inventory, not a report of symbols
retained by the linker. Cargo's feature resolution can also include more than
the final executable uses. The native release jobs use the same default feature
configuration for building and generating notices. Changing features, dependency
patches, build hosts or vendored libraries requires checking this scope again.
[Cargo documents the metadata graph and filtering behavior here.](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html)

The output preserves declared expressions and available `LICENSE*`, `LICENCE*`,
`COPYING*`, `NOTICE*`, `COPYRIGHT*` and `UNLICENSE*` files, including nested files.
It also retains copyright and permission comment blocks from source files.
Original notice text is not rewritten. FreeType's license index references
additional files with other names; those are included explicitly. This can
include notices for crate documentation, examples or bundled fallback sources
that the executable does not use.

Generation fails for unrecognized license identifiers or exceptions, absent
license declarations or texts, unsupported non-registry sources, mismatched
supplements, and missing or modified MPL sources. It does not substitute a
generic license when an unknown crate has omitted its terms. All review happens
before the notices file is replaced; a failed invocation must block packaging.
The script needs access to the Cargo registry source and archive cache, not just
the compiled executable. It does not build software or download license texts
at release time.

## Current dependency observations

The v0.1 lockfile inventory selects **148 packages for Linux x86_64** and **86
packages for each macOS architecture**, excluding Kokuban itself and including
the conservative build dependency closure described above.

Most declare MIT and/or Apache-2.0. The selected graphs also contain BSD-2-Clause,
BSD-3-Clause, ISC, Zlib, 0BSD, Unlicense, Unicode-3.0, MPL-2.0, and an optional
Apache-2.0 LLVM exception. Legacy Cargo spellings such as `MIT/Apache-2.0` mean
alternatives; the generated output retains the original spelling. `AND` requires
both components: `unicode-ident` declares `(MIT OR Apache-2.0) AND Unicode-3.0`,
and `dpi` declares `Apache-2.0 AND MIT`. Their separate license texts are included.
The Unicode notices in `unicode-width` and `regex-syntax` are preserved too.

`Cargo.lock` additionally contains platform packages outside these release
graphs: `dwrote` declares MPL-2.0 for Windows, while `r-efi` offers
`MIT OR Apache-2.0 OR LGPL-2.1-or-later`. Their presence in the lockfile does not
mean either package is included in a Linux or macOS release. New release targets
need a fresh inventory. These are observations of upstream declarations, not a
legal opinion about the complete application.

### Included MPL source

`option-ext 0.2.0` declares MPL-2.0 and is selected on all three release targets.
The package includes its original published `.crate` source archive, the full
MPL text, a relative path to that source, and its SHA-256. Recipients can unpack
the archive with `tar -xzf THIRD_PARTY_SOURCES/option-ext-0.2.0.crate` and exercise
the rights granted by the MPL on that covered source. Kokuban does not change
those source files or relicense them under MIT.

Before copying the archive, the generator checks its SHA-256 against the exact
package entry in `Cargo.lock`, compares every archived file with the extracted
crate used by Cargo, and rejects added source files. Modified covered code
requires a corresponding source package and a reviewed generator change; an
unchanged upstream archive is not an acceptable substitute. The executable/source
distribution requirements are in [MPL 2.0 sections 3.1–3.4](https://www.mozilla.org/en-US/MPL/2.0/).

### Reviewed supplements for omitted license files

Eighteen selected macOS crates omit complete license files from their published
registry packages; two of those crates are also selected on Linux. The pinned
[supplement manifest](third-party-licenses/manifest.json) records exact crate
versions, declared licenses, published VCS commits, source URLs and SHA-256
hashes for the additional text. Updates that stop matching these records fail.

- `pathfinder_simd 0.5.5`: MIT and Apache texts come from its exact published VCS
  commit. Its source copyright notices are also preserved.
- `pathfinder_geometry 0.5.1`: the published VCS commit was unavailable from the
  upstream repository during this review. The published source explicitly
  offers Apache-2.0/MIT and links to the canonical terms. The supplement uses
  that referenced Apache-2.0 text and retains the actual source copyright
  notices. It does not attribute another release's license file to this version.
- `objc2`, `objc2-encode`, `block2`, `dispatch2`, and twelve framework crates:
  the exact upstream `LICENSE.md` offers MIT, or MIT as one option. It points to
  the OSI MIT text instead of reproducing the terms. Both the upstream notice and
  the referenced MIT permission/disclaimer are included. The generic copyright
  placeholder on the OSI page is not filled with an invented holder or year;
  actual source notices are retained separately.

The objc2 upstream notice also raises a question about licensing implications
of bindings derived from Apple SDKs. **This upstream caveat is preserved in the
macOS notices; this review does not resolve it or grant additional Apple SDK
rights.** See the [version-pinned upstream notice](https://raw.githubusercontent.com/madsmtm/objc2/8852b424193ca41602281b3d7540d7c8ed51e49a/LICENSE.md).

### System libraries and fonts

The release archive does not copy macOS frameworks, Linux system shared
libraries, or installed fonts into the package. Their presence on the build or
runtime system is distinct from the Rust package inventory. Linux currently
uses system FreeType/fontconfig; the `freetype-sys` crate also contains fallback
FreeType sources. The notices retain their FreeType License and attribution:

> Portions of this software are copyright (c) The FreeType Project
> (www.freetype.org). All rights reserved.

Any future static bundling of native libraries or fonts needs its own review
and distribution notices. The generated Rust dependency list alone does not
establish the licensing scope of such a bundle.
