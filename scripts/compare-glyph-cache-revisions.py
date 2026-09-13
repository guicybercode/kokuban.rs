#!/usr/bin/env python3
"""Compare prewarmed CPU glyph-cache lookups in isolated release test builds.

This excludes cold font rasterization, frame painting, Metal/GPU work, PTY
processing and input latency. Six alternating process pairs are descriptive;
same-binary controls measure variability with one exact executable path.
"""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys
import time


SIDES = ("before", "after")
SOURCE = "src/glyph_atlas.rs"
FIXTURE = "src/glyph_atlas/lookup_benchmark.rs"
MODULE = b"\n#[cfg(test)]\nmod lookup_benchmark;\n"
TEST = "glyph_atlas::lookup_benchmark::benchmark_warmed_lookups"
WORKLOADS = ("ascii-regular", "ascii-four-styles", "mixed-unicode", "unicode-scalars", "graphemes")
COUNTS = ("lookups_per_iteration", "iterations", "warmup", "samples", "atlas_width", "atlas_height",
          "scalar_glyphs", "grapheme_glyphs", "atlas_bytes", "scalar_cache_bytes")
FINGERPRINTS = ("fingerprint", "pixels_fingerprint")
FOOTPRINTS = ("atlas_bytes", "scalar_cache_bytes")


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


def test_artifact(source, target, messages):
    candidates = []
    for line in messages.read_text().splitlines():
        item = json.loads(line)
        if (item.get("reason") == "compiler-artifact" and item["target"]["name"] == "kokuban"
                and "bin" in item["target"]["kind"] and item["profile"].get("test")):
            require(Path(item["manifest_path"]).resolve() == source / "Cargo.toml", "wrong Cargo source manifest")
            require(item.get("fresh") is False, "root artifact was reused; require a new target and build")
            require(item["profile"].get("opt_level") == "3", "expected release optimization level 3")
            binary = Path(item["executable"]).resolve(strict=True)
            require(binary.parent == target / "release" / "deps" and os.access(binary, os.X_OK),
                    "binary must be executable inside its own release target")
            candidates.append(binary)
    require(len(candidates) == 1, "expected one freshly compiled kokuban release test artifact")
    return candidates[0]


def fields(line, marker, expected):
    require(marker in line, "missing benchmark marker")
    words = line[line.index(marker) + len(marker):].split()
    require(all(word.count("=") == 1 for word in words), "malformed benchmark fields")
    items = [word.split("=", 1) for word in words]
    result = dict(items)
    require(len(items) == len(result) and set(result) == set(expected), "duplicate, missing or unexpected fields")
    return result


def parse_output(output, iterations, warmup):
    require(re.search(r"test result: ok\. 1 passed; 0 failed;", output), "exact warmed lookup test did not pass")
    fixtures, samples = {}, {}
    for line in output.splitlines():
        if "glyph-cache fixture " in line:
            row = fields(line, "glyph-cache fixture ", (*COUNTS, *FINGERPRINTS, "workload", "font_name_hex"))
            workload = row["workload"]
            require(workload in WORKLOADS and workload not in fixtures, "duplicate or unknown workload fixture")
            for name in COUNTS:
                require(re.fullmatch(r"\d+", row[name]), "invalid fixture count")
                row[name] = int(row[name])
            require((row["lookups_per_iteration"], row["iterations"], row["warmup"], row["samples"])
                    == (384, iterations, warmup, 1), "unexpected fixture settings")
            require(all(row[name] > 0 for name in ("atlas_width", "atlas_height", *FOOTPRINTS))
                    and row["scalar_glyphs"] + row["grapheme_glyphs"] > 0, "empty atlas or cache")
            require(all(re.fullmatch(r"[0-9a-f]{16}", row[name]) for name in FINGERPRINTS), "invalid fingerprint")
            require(re.fullmatch(r"(?:[0-9a-f]{2})+", row["font_name_hex"]), "invalid font name encoding")
            row["font_name"] = bytes.fromhex(row["font_name_hex"]).decode("utf-8")
            fixtures[workload] = row
        elif "glyph-cache sample " in line:
            row = fields(line, "glyph-cache sample ", ("workload", "sample", "lookups", "elapsed_ns"))
            workload = row["workload"]
            require(workload in WORKLOADS and workload not in samples, "duplicate or unknown timing workload")
            for name in ("sample", "lookups", "elapsed_ns"):
                require(re.fullmatch(r"\d+", row[name]), "invalid timing count")
                row[name] = int(row[name])
            require(row["sample"] == 0 and row["lookups"] == 384 * iterations and row["elapsed_ns"] > 0,
                    "unexpected timing settings")
            row["ns_per_lookup"] = row["elapsed_ns"] / row["lookups"]
            samples[workload] = row
    require(set(fixtures) == set(samples) == set(WORKLOADS), "expected all five fixtures and timing samples")
    return fixtures, list(samples.values())


def comparable_fixtures(fixtures):
    return {workload: {key: value for key, value in row.items() if key not in FOOTPRINTS}
            for workload, row in fixtures.items()}


def summarize(records, pairs):
    result = []
    for workload in WORKLOADS:
        values = {side: [] for side in SIDES}
        changes = []
        for pair in range(1, pairs + 1):
            selected = [row for row in records if row["workload"] == workload and row["pair"] == pair]
            require(len(selected) == 2 and {row["side"] for row in selected} == set(SIDES), "incomplete paired sample")
            elapsed = {row["side"]: row["elapsed_ns"] for row in selected}
            changes.append((elapsed["after"] / elapsed["before"] - 1) * 100)
            for row in selected:
                values[row["side"]].append(row["ns_per_lookup"])
        result.append({"workload": workload, "samples_ns_per_lookup": values,
                       "median_ns_per_lookup": {side: statistics.median(items) for side, items in values.items()},
                       "paired_time_change_percent": changes,
                       "median_paired_time_change_percent": statistics.median(changes),
                       "faster_pairs": sum(value < 0 for value in changes),
                       "slower_pairs": sum(value > 0 for value in changes)})
    return result


def measure(args):
    args.output.mkdir(parents=True, exist_ok=False)
    path = args.output / "report.json"
    report = {"schema": 1, "status": "initializing", "scope": __doc__, "comparison": args.comparison,
              "platform": platform.platform(), "machine": platform.machine(), "runner_sha256": sha(__file__),
              "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "settings": {name: getattr(args, name) for name in ("pairs", "iterations", "warmup", "timeout")},
              "footprint_scope": "Rust size_of_val inline storage; excludes backing heap allocations and process RSS",
              "records": [], "execution_order": [], "fixtures": {}, "summary": None}
    save(path, report)
    try:
        system = platform.system()
        require(system in ("Linux", "Darwin"), "requires Linux or macOS")
        affinity = sorted(os.sched_getaffinity(0)) if system == "Linux" else None
        require(system != "Linux" or len(affinity) == 1, "pin the Linux runner to one CPU with taskset")
        report["cpu_affinity"] = affinity
        report["affinity_note"] = ("One Linux CPU; inherited by each process" if affinity is not None else
                                   "macOS affinity is not enforced; scheduling and core migration may affect timings")
        report["rustc"] = subprocess.check_output(["rustc", "--version", "--verbose"], text=True)
        sides = build_sides(args.comparison)
        roots = distinct_roots([getattr(args, side + suffix) for suffix in ("_source", "_target") for side in sides])
        sources, targets = dict(zip(sides, roots[:len(sides)])), dict(zip(sides, roots[len(sides):]))
        injection = json.loads(args.injection.read_text())
        require(injection["comparison"] == args.comparison and set(injection["sources"]) == set(sides),
                "comparison differs from source preparation")
        report["injection_sha256"] = sha(args.injection)
        report["fixture_sha256"] = injection["fixture_sha256"]
        report["original_source_differences"] = injection["original_source_differences"]
        binaries = {}
        report["builds"] = {}
        for side in sides:
            source, target = sources[side], targets[side]
            provenance = injection["sources"][side]
            require(str(source) == provenance["root"] and manifest(source) == provenance["manifest"],
                    "source changed after preparation")
            original = (source / SOURCE).read_bytes()
            require(original.endswith(MODULE) and digest(original[:-len(MODULE)]) == provenance["original_sha256"]
                    and sha(source / FIXTURE) == injection["fixture_sha256"], "common fixture injection changed")
            messages = getattr(args, side + "_messages").resolve(strict=True)
            binary = test_artifact(source, target, messages)
            binaries[side] = binary
            report["builds"][side] = {**provenance, "target": str(target), "binary": str(binary),
                                      "binary_sha256": sha(binary), "cargo_messages_sha256": sha(messages),
                                      "source_revision": getattr(args, side + "_ref")}
        if args.comparison == "same-binary":
            binaries["after"] = binaries["before"]
        else:
            require(sha(binaries["before"]) != sha(binaries["after"]), "identical binaries require same-binary mode")
        report["execution_binaries"] = {side: {"path": str(binary), "sha256": sha(binary)} for side, binary in binaries.items()}
        report["status"] = "running"
        for pair in range(1, args.pairs + 1):
            for side in SIDES if pair % 2 else reversed(SIDES):
                if affinity is not None:
                    require(sorted(os.sched_getaffinity(0)) == affinity, "CPU affinity changed")
                require(sha(binaries[side]) == report["execution_binaries"][side]["sha256"], "binary changed during measurement")
                environment = {key: value for key, value in os.environ.items() if not key.startswith("KOKUBAN_GLYPH_LOOKUP_")}
                environment.update(KOKUBAN_GLYPH_LOOKUP_SAMPLES="1", KOKUBAN_GLYPH_LOOKUP_ITERATIONS=str(args.iterations),
                                   KOKUBAN_GLYPH_LOOKUP_WARMUP=str(args.warmup))
                command = [str(binaries[side]), "--exact", TEST, "--ignored", "--nocapture", "--test-threads=1"]
                report["execution_order"].append({"pair": pair, "side": side, "command": command})
                save(path, report)
                log = args.output / f"{pair:02d}-{side}.log"
                with log.open("w") as output:
                    process = subprocess.run(command, env=environment, stdout=output, stderr=subprocess.STDOUT,
                                             timeout=args.timeout, check=False)
                require(process.returncode == 0, f"benchmark process failed; see {log}")
                fixtures, samples = parse_output(log.read_text(), args.iterations, args.warmup)
                if side in report["fixtures"]:
                    require(fixtures == report["fixtures"][side], "fixture or footprint changed between processes")
                if report["fixtures"]:
                    first = next(iter(report["fixtures"].values()))
                    require(comparable_fixtures(fixtures) == comparable_fixtures(first),
                            "lookup fingerprints, font, pixels or glyph counts differ")
                report["fixtures"][side] = fixtures
                report["records"].extend({"pair": pair, "side": side, **sample} for sample in samples)
                save(path, report)
                print(f"pair={pair}/{args.pairs} side={side}: all five warmed fixtures passed", flush=True)
        report["summary"] = summarize(report["records"], args.pairs)
        report["status"] = "completed"
        return 0
    except (Exception, KeyboardInterrupt) as error:
        report.update(status="failed", error=f"{type(error).__name__}: {error}", summary=None)
        print(report["error"], file=sys.stderr)
        return 1
    finally:
        report["finished_utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        save(path, report)


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare", help="append the same ignored test module to fresh source archives")
    measure_parser = commands.add_parser("measure", help="compare prewarmed lookup CPU time in fresh release test builds")
    for command in (prepare_parser, measure_parser):
        command.add_argument("--comparison", choices=("revisions", "same-binary"), default="revisions")
        command.add_argument("--before-source", type=Path, required=True)
        command.add_argument("--after-source", type=Path)
        command.add_argument("--output", type=Path, required=True)
    prepare_parser.add_argument("--fixture", type=Path, required=True)
    for side in SIDES:
        for suffix in ("target", "messages"):
            measure_parser.add_argument(f"--{side}-{suffix}", type=Path, required=side == "before")
        measure_parser.add_argument(f"--{side}-ref", required=side == "before")
    measure_parser.add_argument("--injection", type=Path, required=True)
    measure_parser.add_argument("--pairs", type=int, default=6)
    measure_parser.add_argument("--iterations", type=int, default=1000)
    measure_parser.add_argument("--warmup", type=int, default=100)
    measure_parser.add_argument("--timeout", type=float, default=180)
    args = parser.parse_args(argv)
    after_fields = ["after_source"] + (["after_target", "after_messages", "after_ref"] if args.command == "measure" else [])
    if args.comparison == "same-binary":
        if any(getattr(args, field) is not None for field in after_fields):
            parser.error("same-binary uses only before; omit all after arguments")
    elif any(getattr(args, field) is None for field in after_fields):
        parser.error("revisions requires all before and after build arguments")
    if args.command == "measure":
        if not (1 <= args.pairs <= 20 and 1 <= args.iterations <= 100000 and 1 <= args.warmup <= 100000):
            parser.error("pairs must be 1..20; iterations and warmup must be 1..100000")
        if not math.isfinite(args.timeout) or args.timeout <= 0:
            parser.error("timeout must be positive and finite")
        if not all(getattr(args, side + "_ref").strip() for side in build_sides(args.comparison)):
            parser.error("revision labels cannot be blank")
    return args


if __name__ == "__main__":
    arguments = parse_args()
    if arguments.command == "prepare":
        prepare(arguments)
    else:
        sys.exit(measure(arguments))
