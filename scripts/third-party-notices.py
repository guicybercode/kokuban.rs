#!/usr/bin/env python3
"""Generate release notices from Cargo's target-filtered dependency graph.

Python 3.11+; standard library only. No build is performed. See docs/THIRD_PARTY.md.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / "docs" / "third-party-licenses"
KNOWN_LICENSES = {
    "0BSD", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "MIT",
    "MPL-2.0", "Unicode-3.0", "Unlicense", "Zlib",
}
NOTICE_PREFIXES = ("license", "licence", "copying", "notice", "copyright", "unlicense")
SOURCE_SUFFIXES = {".rs", ".c", ".h", ".cpp", ".cc", ".m", ".mm"}
COMMENT = re.compile(r"/\*.*?\*/|(?:^[ \t]*//[^\n]*\n?)+", re.M | re.S)


class ReviewRequired(Exception):
    """An incomplete or unreviewed dependency must block packaging."""


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def expression_licenses(expression: str) -> set[str]:
    """Validate a small SPDX grammar, retaining AND/OR rather than flattening it."""
    normalized = expression.replace("/", " OR ")  # Legacy Cargo syntax.
    tokens = re.findall(r"[A-Za-z0-9.+-]+|[()]", normalized)
    if re.sub(r"\s+", "", normalized) != "".join(tokens):
        raise ReviewRequired(f"invalid license expression: {expression!r}")
    position = 0
    found: set[str] = set()

    def primary() -> None:
        nonlocal position
        if position >= len(tokens):
            raise ReviewRequired(f"incomplete license expression: {expression}")
        token = tokens[position]
        position += 1
        if token == "(":
            either()
            if position >= len(tokens) or tokens[position] != ")":
                raise ReviewRequired(f"unbalanced license expression: {expression}")
            position += 1
        elif token in KNOWN_LICENSES:
            found.add(token)
            if position < len(tokens) and tokens[position] == "WITH":
                position += 1
                if (token != "Apache-2.0" or position >= len(tokens)
                        or tokens[position] != "LLVM-exception"):
                    raise ReviewRequired(f"unreviewed exception: {expression}")
                position += 1
        else:
            raise ReviewRequired(f"unreviewed license identifier: {token}")

    def both() -> None:
        nonlocal position
        primary()
        while position < len(tokens) and tokens[position] == "AND":
            position += 1
            primary()

    def either() -> None:
        nonlocal position
        both()
        while position < len(tokens) and tokens[position] == "OR":
            position += 1
            both()

    either()
    if position != len(tokens):
        raise ReviewRequired(f"invalid license expression: {expression}")
    return found


def dependencies(metadata: dict) -> list[dict]:
    resolve = metadata.get("resolve")
    if not resolve or not resolve.get("root"):
        raise ReviewRequired("metadata needs a resolved, non-virtual root package")
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in resolve["nodes"]}
    root = resolve["root"]
    pending, visited = [root], set()
    while pending:
        package_id = pending.pop()
        if package_id in visited:
            continue
        if package_id not in nodes or package_id not in packages:
            raise ReviewRequired(f"incomplete dependency graph: {package_id}")
        visited.add(package_id)
        for dep in nodes[package_id]["deps"]:
            kinds = dep.get("dep_kinds")
            if not kinds or any(k.get("kind") not in (None, "build", "dev")
                                for k in kinds):
                raise ReviewRequired(f"incomplete dependency kinds: {dep}")
            if any(k["kind"] in (None, "build") for k in kinds):
                pending.append(dep["pkg"])
    return sorted((packages[i] for i in visited if i != root),
                  key=lambda p: (p["name"], p["version"], p["id"]))


def checked_text(path: Path) -> str:
    if path.is_symlink() or not path.is_file():
        raise ReviewRequired(f"missing or non-regular notice file: {path}")
    text = path.read_text(encoding="utf-8")
    if not text.strip() or "\0" in text:
        raise ReviewRequired(f"empty or binary notice file: {path}")
    return text


def package_notices(package: dict, supplements: dict) -> list[tuple[str, str]]:
    directory = Path(package["manifest_path"]).parent
    paths = set()
    for path in directory.rglob("*"):
        if (path.is_file() and path.suffix.lower() not in SOURCE_SUFFIXES
                and path.name.lower().startswith(NOTICE_PREFIXES)):
            paths.add(path)
    if package.get("license_file"):
        declared = (directory / package["license_file"]).resolve()
        if not declared.is_relative_to(directory.resolve()):
            raise ReviewRequired(f"license_file escapes crate: {package['name']}")
        paths.add(declared)
    # The vendored FreeType license index explicitly references these files.
    if package["name"] == "freetype-sys":
        paths.update(directory / relative for relative in (
            "freetype2/docs/FTL.TXT", "freetype2/docs/GPLv2.TXT",
            "freetype2/src/bdf/README", "freetype2/src/pcf/README",
        ))
    notices = [(str(p.relative_to(directory)), checked_text(p))
               for p in sorted(paths)]
    record = next((p for p in supplements["packages"]
                   if (p["name"], p["version"]) ==
                   (package["name"], package["version"])), None)
    if record:
        vcs = json.loads((directory / ".cargo_vcs_info.json").read_text())
        if (record["license"] != package["license"]
                or record["vcs_commit"] != vcs["git"]["sha1"]):
            raise ReviewRequired(f"supplement no longer matches {package['name']}")
        notices.append(("Reviewed license supplement", record["reason"] +
                        "\nSelected license: " + record["selected_license"] + "\n"))
        for filename in record["files"]:
            source = supplements["files"][filename]
            path = DATA / filename
            if path.parent != DATA or digest(path.read_bytes()) != source["sha256"]:
                raise ReviewRequired(f"supplement checksum mismatch: {filename}")
            notices.append((f"{filename}\nSource: {source['url']}\n"
                            f"SHA-256: {source['sha256']}", checked_text(path)))
    if not paths and not record:
        raise ReviewRequired("no license text; review and pin upstream terms for "
                             f"{package['name']} {package['version']}")
    # Preserve attribution/license comment blocks, including Pathfinder's actual
    # copyright notices. Identical blocks are emitted once with all source paths.
    comments: dict[str, list[str]] = {}
    for path in sorted(directory.rglob("*")):
        if not path.is_file() or path.suffix.lower() not in SOURCE_SUFFIXES:
            continue
        if path.is_symlink():
            raise ReviewRequired(f"symlink in crate sources: {path}")
        source = path.read_text(encoding="utf-8", errors="replace")
        for match in COMMENT.finditer(source):
            block = match.group()
            if ("copyright" in block.lower() or
                    "permission is hereby granted" in block.lower()):
                comments.setdefault(block, []).append(str(path.relative_to(directory)))
    notices.extend(("Source attribution: " + ", ".join(paths), block)
                   for block, paths in comments.items())
    return notices


def original_source(package: dict, lock: dict) -> tuple[str, bytes, str]:
    """Verify the original archive and the covered sources used by Cargo."""
    entry = next((p for p in lock["package"]
                  if (p["name"], p["version"], p.get("source")) ==
                  (package["name"], package["version"], package["source"])), None)
    if not entry or not entry.get("checksum"):
        raise ReviewRequired(f"no locked source checksum for {package['name']}")
    directory = Path(package["manifest_path"]).parent
    filename = f"{package['name']}-{package['version']}.crate"
    if directory.parent.parent.name != "src":
        raise ReviewRequired(f"unsupported Cargo registry layout: {directory}")
    archive = directory.parents[2] / "cache" / directory.parent.name / filename
    if not archive.is_file() or archive.is_symlink():
        raise ReviewRequired(f"original MPL source archive is missing: {archive}")
    data = archive.read_bytes()
    if digest(data) != entry["checksum"]:
        raise ReviewRequired(f"MPL source checksum differs from Cargo.lock: {archive}")
    archive_paths = set()
    with tarfile.open(archive, "r:gz") as source:
        for member in source:
            path = Path(member.name)
            if (not path.parts or path.parts[0] != directory.name
                    or path.is_absolute() or ".." in path.parts):
                raise ReviewRequired(f"invalid MPL source archive path: {member.name}")
            if member.isdir():
                continue
            if not member.isfile():
                raise ReviewRequired(f"non-regular MPL source: {member.name}")
            relative = Path(*path.parts[1:])
            archive_paths.add(relative)
            actual = directory / relative
            stream = source.extractfile(member)
            if (stream is None or actual.is_symlink() or not actual.is_file()
                    or actual.read_bytes() != stream.read()):
                raise ReviewRequired(
                    f"modified MPL source needs its own archive: {actual}")
    extras = {p.relative_to(directory) for p in directory.rglob("*") if p.is_file()}
    extras -= archive_paths | {Path(".cargo-ok"), Path(".cargo-checksum.json")}
    if extras:
        raise ReviewRequired(
            f"additional MPL source files need review: {sorted(extras)}")
    return filename, data, entry["checksum"]


def render(metadata: dict, target: str) -> tuple[str, dict[str, bytes], int]:
    packages = dependencies(metadata)
    supplements = json.loads((DATA / "manifest.json").read_text())
    lock_path = Path(metadata["workspace_root"]) / "Cargo.lock"
    lock = tomllib.loads(lock_path.read_text())
    parts = ["Kokuban third-party licenses and notices\n",
             f"Target: {target}\n"
             f"Cargo.lock SHA-256: {digest(lock_path.read_bytes())}\n",
             f"Dependency packages: {len(packages)}\n",
             "Scope: target-filtered normal and build dependency closure; dev-only "
             "edges excluded. Build/proc-macro packages are included conservatively.\n"
             "Declared license expressions and upstream notices remain unchanged. "
             "Kokuban's project license does not replace third-party licenses.\n"]
    sources = {}
    for package in packages:
        name = f"{package['name']} {package['version']}"
        if not (package.get("source") or "").startswith("registry+"):
            raise ReviewRequired(f"non-registry dependency needs review: {name}")
        if not package.get("license"):
            raise ReviewRequired(f"missing declared license expression: {name}")
        licenses = expression_licenses(package["license"])
        parts.append(f"\n{'=' * 78}\n{name}\nLicense: {package['license']}\n"
                     f"Source: {package['source']}\n"
                     f"Repository: {package.get('repository') or '(not declared)'}\n")
        if "MPL-2.0" in licenses:
            filename, data, checksum = original_source(package, lock)
            sources[filename] = data
            parts.append("MPL-covered source is included, unmodified, at "
                         f"THIRD_PARTY_SOURCES/{filename}\nSHA-256: {checksum}\n"
                         "You may use, modify and distribute that covered source "
                         "under the Mozilla Public License 2.0 reproduced below.\n")
        if package["name"] == "freetype-sys":
            parts.append("FreeType is used under the FreeType License (FTL). "
                         "Portions of this software are copyright (c) The FreeType "
                         "Project (www.freetype.org). All rights reserved.\n")
        for label, content in package_notices(package, supplements):
            parts.append(f"\n--- {label} ---\n{content}")
            if not content.endswith("\n"):
                parts.append("\n")
    return "".join(parts), sources, len(packages)


def atomic_write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as handle:
        temporary = Path(handle.name)
        try:
            handle.write(data)
            handle.close()
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--sources-dir", type=Path)
    parser.add_argument("--metadata-json", type=Path,
                        help="Use precomputed, target-filtered metadata (tests/audit)")
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    try:
        if args.metadata_json:
            metadata = json.loads(args.metadata_json.read_text())
        else:
            command = ["cargo", "metadata", "--locked", "--format-version", "1",
                       "--filter-platform", args.target]
            if args.offline:
                command.append("--offline")
            metadata = json.loads(subprocess.run(command, cwd=ROOT, check=True,
                                                capture_output=True, text=True).stdout)
        notices, sources, count = render(metadata, args.target)
        if sources and not args.sources_dir:
            raise ReviewRequired("MPL sources require --sources-dir for packaging")
        if args.sources_dir:
            if args.sources_dir.is_symlink():
                raise ReviewRequired("source output directory must not be a symlink")
            if args.sources_dir.exists() and any(args.sources_dir.iterdir()):
                raise ReviewRequired(
                    "source output directory must be empty (no stale files)")
            args.sources_dir.mkdir(parents=True, exist_ok=True)
            for filename, data in sources.items():
                atomic_write(args.sources_dir / filename, data)
        atomic_write(args.output, notices.encode("utf-8"))
        print(f"Wrote {count} dependency notices and {len(sources)} source archive(s) "
              f"for {args.target}")
        return 0
    except subprocess.CalledProcessError as error:
        print(f"cargo metadata failed: {error.stderr.strip()}", file=sys.stderr)
    except (ReviewRequired, OSError, ValueError, KeyError, tarfile.TarError) as error:
        print(f"third-party notices: {error}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
