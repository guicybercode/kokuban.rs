#!/usr/bin/env bash
# Package an already-built release or candidate. This does not compile or publish.
# Usage: bash scripts/package-release.sh v0.1 TARGET --notices FILE --sources-dir DIR [--output-dir DIR]
# Validation only: bash scripts/package-release.sh --check-tag v0.1
# Candidate before tagging: add --candidate; BUILD-INFO.json records the source commit.
set -euo pipefail

release_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
python3 - "$release_root" "$@" <<'PY'
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile

root = Path(sys.argv[1]).resolve()
parser = argparse.ArgumentParser(description="Package a built and tagged Kokuban release")
parser.add_argument("--check-tag", action="store_true")
mode = parser.add_mutually_exclusive_group()
mode.add_argument("--candidate", action="store_true", help="package this commit before creating a tag")
mode.add_argument("--verify-tag", action="store_true", help="require the release tag to identify HEAD (default)")
parser.add_argument("tag")
parser.add_argument("target", nargs="?")
parser.add_argument("--notices", type=Path)
parser.add_argument("--sources-dir", type=Path)
parser.add_argument("--output-dir", type=Path, default=Path("dist"))
arguments = parser.parse_args(sys.argv[2:])

def command(*args):
    return subprocess.check_output(args, cwd=root, text=True).strip()

metadata = json.loads(command("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"))
package = next(item for item in metadata["packages"]
               if Path(item["manifest_path"]).resolve() == root / "Cargo.toml")
version = package["version"]
allowed_tags = {"v" + version}
if re.fullmatch(r"[0-9]+\.[0-9]+\.0", version):
    allowed_tags.add("v" + version.rsplit(".", 1)[0])
if arguments.tag not in allowed_tags:
    raise SystemExit(f"tag {arguments.tag!r} does not match Cargo version {version}; expected {sorted(allowed_tags)}")
if not re.fullmatch(r"v[0-9]+\.[0-9]+(?:\.[0-9]+)?(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", arguments.tag):
    raise SystemExit("unsupported release tag format")
commit = command("git", "rev-parse", "--verify", "HEAD^{commit}")
if not arguments.candidate:
    tag_commit = command("git", "rev-parse", "--verify", f"refs/tags/{arguments.tag}^{{commit}}")
    if commit != tag_commit:
        raise SystemExit("the checked-out commit is not the requested release tag")
if arguments.check_tag:
    print(f"Validated {arguments.tag}: Cargo {version}, source commit {commit}, tag verified={not arguments.candidate}")
    raise SystemExit(0)

targets = {"x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-apple-darwin"}
if arguments.target not in targets or arguments.notices is None or arguments.sources_dir is None:
    parser.error("a supported native target, --notices FILE and --sources-dir DIR are required")
binary = root / "target" / arguments.target / "release" / "kokuban"
if not binary.is_file() or not os.access(binary, os.X_OK):
    raise SystemExit(f"built executable is missing: {binary}")
files = {name: root / name for name in (
    "LICENSE", "README.md", "SECURITY.md", "ABOUT.md", "CONTRIBUTING.md", "CHANGELOG.md", "kokuban.toml",
    "assets/kokuban-icon.png", "assets/io.github.guicybercode.kokuban.desktop", "assets/README.md")}
files["THIRD_PARTY_LICENSES.txt"] = arguments.notices.resolve()
documentation = root / "docs"
if not documentation.is_dir():
    raise SystemExit("the docs directory is required for packaged README links")
for path in sorted(documentation.rglob("*")):
    if path.is_symlink():
        raise SystemExit(f"packaged documentation must not contain symbolic links: {path}")
    if path.is_file():
        files[path.relative_to(root).as_posix()] = path
for name, path in files.items():
    if not path.is_file() or path.stat().st_size == 0:
        raise SystemExit(f"required distribution text is missing or empty: {name} ({path})")
sources = arguments.sources_dir.resolve()
if not sources.is_dir():
    raise SystemExit(f"third-party source directory is missing: {sources}")
source_files = []
for path in sorted(sources.rglob("*")):
    if path.is_symlink():
        raise SystemExit(f"third-party source archive must not contain symbolic links: {path}")
    if path.is_file():
        source_files.append(path)
if not source_files:
    raise SystemExit("third-party source directory is empty")

output = arguments.output_dir.resolve()
output.mkdir(parents=True, exist_ok=True)
stem = f"kokuban-{arguments.tag}-{arguments.target}"
archive = output / (stem + ".tar.gz")
epoch = int(command("git", "show", "-s", "--format=%ct", "HEAD"))
build_info = {
    "package": package["name"], "version": version, "tag": arguments.tag,
    "target": arguments.target, "commit": commit,
    "build_kind": "candidate" if arguments.candidate else "tag",
    "tag_commit_verified": not arguments.candidate,
    "rustc": command("rustc", "--version", "--verbose"),
    "build_command": ["cargo", "build", "--locked", "--release", "--target", arguments.target],
    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
    "runner_os": os.environ.get("RUNNER_OS"), "runner_image": os.environ.get("ImageVersion"),
    "macos_deployment_target": os.environ.get("MACOSX_DEPLOYMENT_TARGET"),
    "release_debug": os.environ.get("CARGO_PROFILE_RELEASE_DEBUG"),
    "distribution": "Native command-line executable; no app bundle, developer signing or notarization.",
}

# Stable gzip/tar timestamps and ownership avoid adding local user metadata.
with archive.open("wb") as destination:
    with gzip.GzipFile(filename="", mode="wb", fileobj=destination, mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as packaged:
            def add(name, content, mode=0o644):
                entry = tarfile.TarInfo(f"{stem}/{name}")
                entry.size, entry.mode, entry.mtime = len(content), mode, epoch
                packaged.addfile(entry, io.BytesIO(content))

            add("kokuban", binary.read_bytes(), 0o755)
            for name, path in sorted(files.items()):
                add(name, path.read_bytes())
            for path in source_files:
                add("THIRD_PARTY_SOURCES/" + path.relative_to(sources).as_posix(), path.read_bytes())
            add("BUILD-INFO.json", (json.dumps(build_info, indent=2) + "\n").encode())
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
checksum = output / (archive.name + ".sha256")
checksum.write_text(f"{digest}  {archive.name}\n", encoding="ascii")
print(archive)
print(checksum)
PY
