#!/usr/bin/env python3
"""Compare prewarmed CPU glyph-cache lookups in isolated release test builds.

This excludes cold font rasterization, frame painting, Metal/GPU work, PTY
processing and input latency. Six alternating process pairs are descriptive;
same-binary controls measure variability with one exact executable path.
"""

import argparse
import hashlib
import json
from pathlib import Path


SIDES = ("before", "after")
SOURCE = "src/glyph_atlas.rs"
FIXTURE = "src/glyph_atlas/lookup_benchmark.rs"
MODULE = b"\n#[cfg(test)]\nmod lookup_benchmark;\n"
TEST = "glyph_atlas::lookup_benchmark::benchmark_warmed_lookups"
WORKLOADS = ("ascii-regular", "ascii-four-styles", "mixed-unicode", "graphemes")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def build_sides(comparison):
    return ("before",) if comparison == "same-binary" else SIDES


def distinct_roots(paths):
    roots = [path.resolve(strict=True) for path in paths]
    require(all(a != b and a not in b.parents and b not in a.parents
                for i, a in enumerate(roots) for b in roots[i + 1:]),
            "source and target roots must be distinct and non-overlapping")
    return roots


def manifest(root):
    return {path.relative_to(root).as_posix(): sha(path)
            for path in sorted(root.rglob("*")) if path.is_file()}


def differences(before, after):
    return sorted(name for name in before.keys() | after.keys() if before.get(name) != after.get(name))


def prepare(args):
    sides = build_sides(args.comparison)
    roots = dict(zip(sides, distinct_roots([getattr(args, side + "_source") for side in sides])))
    output = args.output.resolve()
    fixture_path = args.fixture.resolve(strict=True)
    require(all(root not in output.parents and root != output and output not in root.parents
                and root not in fixture_path.parents for root in roots.values()),
            "fixture and evidence directory must be outside archived sources")
    fixture = fixture_path.read_bytes()
    require(b"fn benchmark_warmed_lookups(" in fixture and b"#[ignore" in fixture,
            "expected the ignored warmed lookup fixture")
    originals = {}
    manifests = {}
    for side, root in roots.items():
        original = (root / SOURCE).read_bytes()
        require(b"lookup_benchmark" not in original and not (root / FIXTURE).exists(),
                "lookup benchmark already present; use fresh unmodified archives")
        originals[side] = original
        manifests[side] = manifest(root)
    changed = differences(manifests["before"], manifests["after"]) if len(sides) == 2 else []
    output.mkdir(parents=True, exist_ok=False)
    (output / "lookup_benchmark.rs").write_bytes(fixture)
    report = {"schema": 1, "comparison": args.comparison, "fixture_sha256": digest(fixture),
              "module_append_sha256": digest(MODULE), "original_source_differences": changed, "sources": {}}
    for side, root in roots.items():
        evidence = output / side
        evidence.mkdir()
        (evidence / "original-glyph_atlas.rs").write_bytes(originals[side])
        (root / FIXTURE).parent.mkdir(exist_ok=True)
        (root / FIXTURE).write_bytes(fixture)
        (root / SOURCE).write_bytes(originals[side] + MODULE)
        (evidence / "injected-glyph_atlas.rs").write_bytes(originals[side] + MODULE)
        prepared = manifest(root)
        require(differences(manifests[side], prepared) == sorted([SOURCE, FIXTURE]),
                "source injection changed unrelated files")
        save(evidence / "original-source-manifest.json", manifests[side])
        save(evidence / "prepared-source-manifest.json", prepared)
        report["sources"][side] = {"root": str(root), "original_sha256": digest(originals[side]),
            "injected_sha256": sha(root / SOURCE), "manifest": prepared,
            "cargo_sha256": {name: sha(root / name) for name in ("Cargo.toml", "Cargo.lock")}}
    if len(sides) == 2:
        require(differences(report["sources"]["before"]["manifest"], report["sources"]["after"]["manifest"]) == changed,
                "common fixture injection changed the revision difference set")
    save(output / "injection.json", report)


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    command = commands.add_parser("prepare", help="append the same ignored test module to fresh source archives")
    command.add_argument("--comparison", choices=("revisions", "same-binary"), default="revisions")
    command.add_argument("--before-source", type=Path, required=True)
    command.add_argument("--after-source", type=Path)
    command.add_argument("--fixture", type=Path, required=True)
    command.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    if (args.comparison == "same-binary") != (args.after_source is None):
        parser.error("same-binary requires only before; revisions requires before and after sources")
    return args


if __name__ == "__main__":
    prepare(parse_args())
