#!/usr/bin/env python3
"""Compare prewarmed Linux CPU frame painting; excludes PTY and presentation.

Both test binaries must contain the same benchmark_frame_repaint harness.
Run on an idle host with both processes pinned to the same CPU. All font,
source, compiler and build provenance must accompany the resulting report.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import time


CONTENTS = ("ascii", "ascii-after-emoji", "unicode")
CHANGES = ("single-row", "full-screen")
SAMPLE = re.compile(
    r"frame-repaint sample content=(\S+) change=(\S+) sample=(\d+) "
    r"incremental=(true|false) frames=(\d+) elapsed_ns=(\d+) ns_per_frame=([0-9.]+)"
)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(directory, report):
    (directory / "report.json").write_text(json.dumps(report, indent=2) + "\n")


def parse_output(output, frames):
    if "1 passed; 0 failed" not in output:
        raise ValueError("the exact ignored test did not pass")
    fixtures = [line[line.index("frame-repaint fixture"):] for line in output.splitlines()
                if "frame-repaint fixture" in line]
    if len(fixtures) != 6:
        raise ValueError("expected six full-frame fixtures")
    records = []
    expected = {(content, change, incremental) for content in CONTENTS for change in CHANGES
                for incremental in (False, True)}
    seen = set()
    for content, change, sample, incremental, count, elapsed, _rounded in SAMPLE.findall(output):
        key = content, change, incremental == "true"
        if key not in expected or key in seen or sample != "0" or int(count) != frames or int(elapsed) <= 0:
            raise ValueError("duplicate, unknown or invalid frame timing")
        seen.add(key)
        records.append({"content": content, "change": change, "incremental": key[2],
                        "frames": int(count), "elapsed_ns": int(elapsed),
                        "ns_per_frame": int(elapsed) / int(count)})
    if seen != expected:
        raise ValueError("missing one or more frame timing modes")
    return fixtures, records


def verify_pixels(directory):
    expected = {f"{content}-{change}-{index}.xrgb8888le" for content in CONTENTS
                for change in CHANGES for index in (0, 1)}
    hashes = {}
    for side in ("before", "after"):
        folder = directory / f"{side}-frames"
        if {path.name for path in folder.iterdir()} != expected:
            raise ValueError(f"{side}: incomplete or unexpected reference frames")
        hashes[side] = {}
        for name in sorted(expected):
            pixels = (folder / name).read_bytes()
            if not pixels or len(pixels) % 4:
                raise ValueError("reference pixels are empty or truncated")
            hashes[side][name] = hashlib.sha256(pixels).hexdigest()
        for name in expected:
            if name.startswith("ascii-") and not name.startswith("ascii-after-emoji-"):
                if hashes[side][name] != hashes[side][name.replace("ascii-", "ascii-after-emoji-", 1)]:
                    raise ValueError("an offscreen emoji changed ASCII reference pixels")
    if hashes["before"] != hashes["after"]:
        raise ValueError("baseline and candidate reference pixels differ")
    return hashes["before"]


def summaries(records, pairs):
    result = []
    for content in CONTENTS:
        for change in CHANGES:
            for incremental in (False, True):
                selected = [record for record in records if (
                    record["content"], record["change"], record["incremental"]
                ) == (content, change, incremental)]
                values = {side: [next(record["ns_per_frame"] for record in selected
                                     if record["side"] == side and record["pair"] == pair)
                                 for pair in range(pairs)] for side in ("before", "after")}
                ratios = [before / after for before, after in zip(values["before"], values["after"])]
                result.append({"content": content, "change": change, "incremental": incremental,
                               "samples_ns_per_frame": values,
                               "median_ns_per_frame": {side: statistics.median(items)
                                                       for side, items in values.items()},
                               "paired_speedups": ratios, "median_paired_speedup": statistics.median(ratios),
                               "median_paired_latency_change_percent": statistics.median([
                                   (1 / ratio - 1) * 100 for ratio in ratios])})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--pairs", type=int, default=5)
    parser.add_argument("--steps", type=int, default=300)
    parser.add_argument("--warmup", type=int, default=30)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    if not (1 <= args.pairs <= 20 and 1 <= args.steps <= 10000
            and 1 <= args.warmup <= 10000 and 1 <= args.timeout <= 600):
        parser.error("pairs must be 1..20; steps/warmup 1..10000; timeout 1..600")
    binaries = {side: getattr(args, side).resolve(strict=True) for side in ("before", "after")}
    for binary in binaries.values():
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"not an executable file: {binary}")
    directory = args.output_dir.resolve()
    directory.mkdir(parents=True, exist_ok=False)
    hashes = {side: sha(binary) for side, binary in binaries.items()}
    report = {"scope": __doc__, "status": "running", "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "binary_sha256": hashes, "binaries": {side: str(path) for side, path in binaries.items()},
              "comparison": "same-binary-control" if len(set(hashes.values())) == 1 else "different-binaries",
              "settings": {key: getattr(args, key) for key in ("pairs", "steps", "warmup", "timeout")},
              "cpu_affinity": sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None,
              "script_sha256": sha(Path(__file__)), "records": [], "execution_order": []}
    save(directory, report)
    fixtures = None
    try:
        for pair in range(args.pairs):
            for side in (("before", "after") if pair % 2 == 0 else ("after", "before")):
                environment = os.environ.copy()
                environment.pop("KOKUBAN_FRAME_OUTPUT_DIR", None)
                environment.update(KOKUBAN_FRAME_CONTENT="all", KOKUBAN_FRAME_CHANGE="all",
                                   KOKUBAN_FRAME_SAMPLES="1", KOKUBAN_FRAME_STEPS=str(args.steps),
                                   KOKUBAN_FRAME_WARMUP=str(args.warmup))
                if pair == 0:
                    environment["KOKUBAN_FRAME_OUTPUT_DIR"] = str(directory / f"{side}-frames")
                command = [str(binaries[side]), "--exact", "linux_window::damage_tests::benchmark_frame_repaint",
                           "--ignored", "--nocapture", "--test-threads=1"]
                log = directory / f"{pair:02d}-{side}.log"
                report["execution_order"].append([pair, side])
                save(directory, report)
                with log.open("wb") as output:
                    subprocess.run(command, env=environment, stdout=output, stderr=subprocess.STDOUT,
                                   timeout=args.timeout, check=True)
                current, records = parse_output(log.read_text(), args.steps)
                if fixtures is None:
                    fixtures = report["fixtures"] = current
                if current != fixtures:
                    raise ValueError("fixture geometry, pixels or damage bands changed between processes")
                report["records"].extend(dict(record, pair=pair, side=side) for record in records)
                save(directory, report)
                print(f"pair={pair + 1}/{args.pairs} side={side}: all modes passed", flush=True)
            if pair == 0:
                report["frame_sha256"] = verify_pixels(directory)
                save(directory, report)
        report["summary"] = summaries(report["records"], args.pairs)
        report["status"] = "passed"
    except BaseException as error:
        report.update(status="failed", error=f"{type(error).__name__}: {error}")
        raise
    finally:
        report["finished_utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        save(directory, report)
    print(json.dumps(report["summary"], indent=2))


if __name__ == "__main__":
    main()
