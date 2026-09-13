#!/usr/bin/env python3
"""Compare CPU damage calculation and painting, excluding PTY, snapshots,
font rasterization, compositor presentation and input-to-photon latency.

prepare injects the identical final ignored benchmark into archived revisions.
measure consumes fresh, separate Cargo release test builds and retains pixels.
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
CONTENTS = ("ascii", "ascii-after-emoji", "unicode")
CHANGES = ("single-row", "full-screen")
TEST = "linux_window::damage_tests::benchmark_frame_repaint"
SOURCE = "src/linux_window.rs"
MARKER = ('    #[test]\n'
          '    #[ignore = "CPU raster microbenchmark; run release on an idle Linux host with --nocapture"]\n'
          '    fn benchmark_frame_repaint() {\n')
SAMPLE = re.compile(r"frame-repaint sample content=(\S+) change=(\S+) sample=(\d+) "
                    r"incremental=(true|false) frames=(\d+) elapsed_ns=(\d+) ns_per_frame=([0-9.]+)")
FIXTURE = re.compile(r"frame-repaint fixture content=(\S+) change=(\S+) cols=(\d+) rows=(\d+) "
                     r"width=(\d+) height=(\d+) cell_width=(\d+) cell_height=(\d+) warmup=(\d+) "
                     r"samples=(\d+) steps=(\d+) mono_cells=(\d+) color_cells=(\d+) "
                     r"bands=\[\((\d+), (\d+)\), \((\d+), (\d+)\)\] frame0=([0-9a-f]{16}) frame1=([0-9a-f]{16})")


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


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def split_benchmark(data):
    """Accept only the supported final function, preserving the entire prefix."""
    text = data.decode("utf-8")
    require(text.count(MARKER) == 1, "expected one exact ignored benchmark marker")
    prefix, body = text.split(MARKER)
    require(prefix.count("\n#[cfg(test)]\nmod damage_tests {\n") == 1,
            "expected benchmark inside the damage_tests module")
    module_body = prefix.split("\n#[cfg(test)]\nmod damage_tests {\n")[1]
    require(all(not line.strip() or line.startswith("    ") for line in module_body.splitlines()),
            "benchmark is outside the supported damage_tests module")
    lines = body.splitlines(keepends=True)
    require(len(lines) >= 2 and lines[-2:] == ["    }\n", "}\n"],
            "benchmark must be the final function and module at EOF")
    require(all(not line.strip() or line.startswith("        ") for line in lines[:-2]),
            "unsupported benchmark layout or another item after the benchmark")
    return prefix.encode(), (MARKER + body).encode()


def distinct_roots(paths):
    resolved = [path.resolve(strict=True) for path in paths]
    require(all(a != b and a not in b.parents and b not in a.parents
                for i, a in enumerate(resolved) for b in resolved[i + 1:]),
            "source and target roots must be separate and non-overlapping")
    return resolved


def prepare(args):
    roots = dict(zip(SIDES, distinct_roots([args.before_source, args.after_source])))
    harness = (args.harness_source / SOURCE).resolve(strict=True)
    require(all(root not in harness.parents for root in roots.values()),
            "the harness checkout must be separate from both archived sources")
    _, benchmark = split_benchmark(harness.read_bytes())
    prepared = {}
    for side, root in roots.items():
        original = (root / SOURCE).read_bytes()
        prefix, _ = split_benchmark(original)
        prepared[side] = (original, prefix + benchmark, prefix)
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "benchmark.rs.txt").write_bytes(benchmark)
    report = {"schema": 1, "benchmark_sha256": digest(benchmark),
              "harness_source": str(harness), "harness_source_sha256": sha(harness),
              "sources": {}}
    for side, root in roots.items():
        original, injected, prefix = prepared[side]
        evidence = args.output / side
        evidence.mkdir()
        (evidence / "original-linux_window.rs").write_bytes(original)
        (evidence / "injected-linux_window.rs").write_bytes(injected)
        (root / SOURCE).write_bytes(injected)
        report["sources"][side] = {"root": str(root), "original_sha256": digest(original),
                                   "injected_sha256": digest(injected), "preserved_prefix_sha256": digest(prefix),
                                   "cargo_sha256": {name: sha(root / name) for name in ("Cargo.toml", "Cargo.lock")}}
    write_json(args.output / "injection.json", report)


def test_artifact(source, target, messages):
    """Cargo's root artifact must be freshly compiled, not a reused executable."""
    candidates = []
    for line in messages.read_text().splitlines():
        item = json.loads(line)
        if (item.get("reason") == "compiler-artifact" and item["target"]["name"] == "kokuban"
                and "bin" in item["target"]["kind"] and item["profile"].get("test")):
            require(Path(item["manifest_path"]).resolve() == source / "Cargo.toml",
                    "root Cargo manifest does not match the declared source")
            require(item.get("fresh") is False, "Cargo reused a fresh root artifact; reject this build")
            require(item["profile"].get("opt_level") == "3", "expected the release test profile")
            binary = Path(item["executable"]).resolve(strict=True)
            require(binary.parent == target / "release" / "deps", "binary is outside its own release target")
            require(os.access(binary, os.X_OK), "test binary is not executable")
            candidates.append(binary)
    require(len(candidates) == 1, "expected exactly one freshly built kokuban release test executable")
    return candidates[0]


def parse_output(output, steps, warmup):
    require(re.search(r"test result: ok\. 1 passed; 0 failed;", output), "benchmark test did not pass")
    # libtest can print its 'test ...' prefix on the first fixture's line.
    fixture_lines = [line[line.index("frame-repaint fixture"):] for line in output.splitlines()
                     if "frame-repaint fixture" in line]
    fixtures = {}
    for line in fixture_lines:
        match = FIXTURE.fullmatch(line)
        require(match is not None, "malformed fixture line: " + line)
        content, change, *fields = match.groups()
        key = (content, change)
        require(key not in fixtures, "duplicate fixture")
        values = list(map(int, fields[:-2]))
        cols, rows, width, height, cell_w, cell_h, warm, samples, frames, mono, color, a, b, c, d = values
        require((cols, rows, warm, samples, frames) == (120, 40, warmup, 1, steps), "unexpected fixture settings")
        require(cell_w > 0 and cell_h > 0 and (width, height) == (cols * cell_w, rows * cell_h),
                "fixture geometry is inconsistent")
        require(mono > 0 and (color > 0) == (content == "unicode"), "unexpected glyph color coverage")
        require(fields[-2] != fields[-1], "fixture must change pixels")
        for start, end in ((a, b), (c, d)):
            require(0 <= start < end <= height, "invalid damage band")
            require((end - start < height) == (change == "single-row"), "unexpected damage coverage")
        fixtures[key] = {"line": line, "width": width, "height": height}
    require(set(fixtures) == {(c, m) for c in CONTENTS for m in CHANGES}, "expected all six fixtures exactly once")
    samples = []
    for match in SAMPLE.finditer(output):
        content, change, sample, incremental, frames, elapsed, ns = match.groups()
        require(sample == "0" and int(frames) == steps and int(elapsed) > 0, "unexpected sample settings")
        require(math.isclose(float(ns), int(elapsed) / steps, rel_tol=0, abs_tol=0.00051),
                "rounded duration does not match elapsed_ns / frames")
        samples.append({"content": content, "change": change, "incremental": incremental == "true",
                        "frames": int(frames), "elapsed_ns": int(elapsed), "ns_per_frame": int(elapsed) / steps})
    expected = {(c, m, inc) for c in CONTENTS for m in CHANGES for inc in (False, True)}
    require(len(samples) == 12 and {(r["content"], r["change"], r["incremental"]) for r in samples} == expected,
            "expected twelve unique frame timing samples")
    return fixtures, samples


def verify_frames(output, fixtures):
    expected = {f"{c}-{m}-{i}.xrgb8888le" for c in CONTENTS for m in CHANGES for i in (0, 1)}
    # Six fixtures, two reference images each; each side retains all twelve.
    hashes = {}
    for side in SIDES:
        directory = output / (side + "-frames")
        require({p.name for p in directory.iterdir()} == expected, "missing or unexpected frame evidence")
        hashes[side] = {}
        for content in CONTENTS:
            for change in CHANGES:
                fixture = fixtures[(content, change)]
                for index in (0, 1):
                    name = f"{content}-{change}-{index}.xrgb8888le"
                    path = directory / name
                    require(path.stat().st_size == fixture["width"] * fixture["height"] * 4,
                            "frame size does not match fixture geometry")
                    hashes[side][name] = sha(path)
        for change in CHANGES:
            for index in (0, 1):
                require(hashes[side][f"ascii-{change}-{index}.xrgb8888le"] ==
                        hashes[side][f"ascii-after-emoji-{change}-{index}.xrgb8888le"],
                        "warming an unused emoji changed ASCII pixels")
    require(hashes["before"] == hashes["after"], "before/after reference pixels differ")
    return hashes


def summarize(records, pairs):
    summaries = []
    for content in CONTENTS:
        for change in CHANGES:
            for incremental in (False, True):
                selected = [r for r in records if (r["content"], r["change"], r["incremental"]) ==
                            (content, change, incremental)]
                values = {side: [next(r["ns_per_frame"] for r in selected if r["side"] == side and r["pair"] == pair)
                                 for pair in range(1, pairs + 1)] for side in SIDES}
                medians = {side: statistics.median(samples) for side, samples in values.items()}
                changes = [(a / b - 1) * 100 for b, a in zip(values["before"], values["after"])]
                summaries.append({"content": content, "change": change, "incremental": incremental,
                                  "samples_ns": values, "median_ns": medians,
                                  "range_ns": {side: [min(v), max(v)] for side, v in values.items()},
                                  "latency_change_percent": (medians["after"] / medians["before"] - 1) * 100,
                                  "paired_latency_change_percent": changes,
                                  "slower_pairs": sum(value > 0 for value in changes)})
    return summaries


def measure(args):
    args.output.mkdir(parents=True, exist_ok=False)
    report = {"schema": 1, "status": "initializing", "scope": __doc__,
              "environment_note": args.environment_note, "platform": platform.platform(),
              "machine": platform.machine(), "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "runner_sha256": sha(__file__), "records": [], "execution_order": [],
              "summary": None, "pixel_equivalence_verified": False,
              "settings": {"pairs": args.pairs, "steps": args.steps, "warmup": args.warmup, "samples_per_process": 1}}
    path = args.output / "report.json"
    write_json(path, report)
    try:
        require(platform.system() == "Linux", "this runner requires Linux CPU affinity")
        affinity = sorted(os.sched_getaffinity(0))
        require(len(affinity) == 1, "pin the runner to one CPU with taskset")
        report["cpu_affinity"] = affinity
        report["rustc"] = subprocess.check_output(["rustc", "--version", "--verbose"], text=True)
        roots = distinct_roots([args.before_source, args.after_source, args.before_target, args.after_target])
        sources = dict(zip(SIDES, roots[:2]))
        targets = dict(zip(SIDES, roots[2:]))
        injection = json.loads(args.injection.read_text())
        binaries = {}
        report["builds"] = {}
        for side in SIDES:
            source, target = sources[side], targets[side]
            provenance = injection["sources"][side]
            require(source == Path(provenance["root"]), "injection source root differs")
            prefix, benchmark = split_benchmark((source / SOURCE).read_bytes())
            require(sha(source / SOURCE) == provenance["injected_sha256"] and
                    digest(prefix) == provenance["preserved_prefix_sha256"] and
                    digest(benchmark) == injection["benchmark_sha256"], "source changed since benchmark injection")
            require({name: sha(source / name) for name in ("Cargo.toml", "Cargo.lock")} == provenance["cargo_sha256"],
                    "Cargo files changed since source preparation")
            messages = getattr(args, side + "_messages").resolve(strict=True)
            binary = test_artifact(source, target, messages)
            binaries[side] = binary
            report["builds"][side] = {"source": str(source), "target": str(target), "binary": str(binary),
                                      "binary_sha256": sha(binary), "cargo_messages_sha256": sha(messages),
                                      "source_revision": getattr(args, side + "_ref"), **provenance}
        require(report["builds"]["before"]["binary_sha256"] != report["builds"]["after"]["binary_sha256"],
                "the two test binaries are identical")
        report["injection_sha256"] = sha(args.injection)
        report["benchmark_sha256"] = injection["benchmark_sha256"]
        report["status"] = "running"
        fixtures = None
        for pair in range(1, args.pairs + 1):
            for side in SIDES if pair % 2 else reversed(SIDES):
                require(sorted(os.sched_getaffinity(0)) == affinity, "CPU affinity changed")
                require(sha(binaries[side]) == report["builds"][side]["binary_sha256"], "binary changed during measurement")
                environment = {k: v for k, v in os.environ.items() if not k.startswith("KOKUBAN_FRAME_")}
                environment.update(KOKUBAN_FRAME_CONTENT="all", KOKUBAN_FRAME_CHANGE="all",
                                   KOKUBAN_FRAME_SAMPLES="1", KOKUBAN_FRAME_STEPS=str(args.steps),
                                   KOKUBAN_FRAME_WARMUP=str(args.warmup))
                if pair == 1:
                    environment["KOKUBAN_FRAME_OUTPUT_DIR"] = str(args.output.resolve() / (side + "-frames"))
                command = [str(binaries[side]), "--exact", TEST, "--ignored", "--nocapture", "--test-threads=1"]
                report["execution_order"].append({"pair": pair, "side": side, "command": command})
                write_json(path, report)
                log = args.output / f"{pair:02d}-{side}.log"
                with log.open("w") as stream:
                    completed = subprocess.run(command, env=environment, stdout=stream, stderr=subprocess.STDOUT,
                                               timeout=args.timeout, check=False)
                require(completed.returncode == 0, f"benchmark failed; see {log}")
                current, samples = parse_output(log.read_text(), args.steps, args.warmup)
                if fixtures is None:
                    fixtures = current
                    report["fixtures"] = list(current.values())
                require(current == fixtures, "fixture pixels, geometry or damage changed between processes")
                report["records"].extend({"pair": pair, "side": side, **sample} for sample in samples)
                write_json(path, report)
                print(f"pair={pair}/{args.pairs} side={side}: all pixel checks passed", flush=True)
            if pair == 1:
                report["frame_sha256"] = verify_frames(args.output, fixtures)
                report["pixel_equivalence_verified"] = True
                write_json(path, report)
        report["summary"] = summarize(report["records"], args.pairs)
        report["status"] = "completed"
        return 0
    except (Exception, KeyboardInterrupt) as error:
        report["status"] = "failed"
        report["error"] = f"{type(error).__name__}: {error}"
        report["summary"] = None
        print(report["error"], file=sys.stderr)
        return 1
    finally:
        report["finished_utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        write_json(path, report)


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare", help="inject the common final benchmark into archived sources")
    measure_parser = commands.add_parser("measure", help="measure separately built release test binaries")
    for command in (prepare_parser, measure_parser):
        command.add_argument("--before-source", type=Path, required=True)
        command.add_argument("--after-source", type=Path, required=True)
        command.add_argument("--output", type=Path, required=True, help="new evidence directory")
    prepare_parser.add_argument("--harness-source", type=Path, required=True)
    for side in SIDES:
        measure_parser.add_argument(f"--{side}-target", type=Path, required=True)
        measure_parser.add_argument(f"--{side}-messages", type=Path, required=True)
        measure_parser.add_argument(f"--{side}-ref", required=True)
    measure_parser.add_argument("--injection", type=Path, required=True)
    measure_parser.add_argument("--pairs", type=int, default=5)
    measure_parser.add_argument("--steps", type=int, default=300)
    measure_parser.add_argument("--warmup", type=int, default=30)
    measure_parser.add_argument("--timeout", type=float, default=180)
    measure_parser.add_argument("--environment-note", default="")
    args = parser.parse_args(argv)
    if args.command == "measure":
        if not (1 <= args.pairs <= 20 and 1 <= args.steps <= 10000 and 1 <= args.warmup <= 10000):
            parser.error("pairs must be 1..20; steps and warmup must be 1..10000")
        if not math.isfinite(args.timeout) or args.timeout <= 0:
            parser.error("timeout must be positive and finite")
        if not all(getattr(args, side + "_ref").strip() for side in SIDES):
            parser.error("source revision labels cannot be blank")
    return args


if __name__ == "__main__":
    arguments = parse_args()
    if arguments.command == "prepare":
        prepare(arguments)
    else:
        sys.exit(measure(arguments))
