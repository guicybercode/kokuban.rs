#!/usr/bin/env python3
"""Audit retained files and all V5 samples; optionally verify reconstructed sources."""
import argparse
import hashlib
import importlib.util
import json
import math
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path)
    args = parser.parse_args()
    inventory = json.loads((ROOT / "inventory.json").read_text())
    actual_files = {str(p.relative_to(ROOT)) for p in ROOT.rglob("*") if p.is_file() and p.name != "inventory.json"}
    require(actual_files == set(inventory), "Inventory file set mismatch")
    for name, expected in inventory.items():
        data = (ROOT / name).read_bytes()
        require({"bytes": len(data), "sha256": sha(data)} == expected, f"File mismatch: {name}")

    raw = json.loads((ROOT / "measurements/screening.json").read_text())
    provenance = json.loads((ROOT / "source-provenance.json").read_text())
    summary = json.loads((ROOT / "summary.json").read_text())
    require(raw["status"] == "completed", "Incomplete screening")
    require(raw["baseline_revision"] == provenance["baseline_revision"], "Baseline mismatch")
    require(sha((ROOT / "candidate.patch").read_bytes()) == raw["candidate_patch_sha256"] == provenance["candidate_patch_sha256"], "Patch mismatch")
    require(raw["binary_sha256"] == provenance["binary_sha256"], "Binary record mismatch")
    require(raw["binary_sha256"]["before"] != raw["binary_sha256"]["after"], "Identical executables")
    for name, expected in raw["harness_sha256"].items():
        require(sha((ROOT / name).read_bytes()) == expected, f"Helper mismatch: {name}")
    for name in ("Cargo.toml", "Cargo.lock"):
        require((ROOT / "harness-baseline" / name).read_bytes() == (ROOT / "harness-candidate" / name).read_bytes(), f"Unequal helper {name}")
    require(raw["payloads"] == json.loads((ROOT / "measurements/payloads.json").read_text()), "Payload records differ")
    spec = importlib.util.spec_from_file_location("payload_generator", ROOT / "generate-payloads.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    for name, data in module.payloads().items():
        require({"bytes": len(data), "sha256": sha(data)} == raw["payloads"][name], f"Payload regeneration mismatch: {name}")

    expected_order = []
    for screen in ("alternate", "primary"):
        for pair in range(1, 6):
            for side in (("before", "after") if pair % 2 else ("after", "before")):
                for workload in ("ascii", "ansi", "unicode", "short_lines"):
                    expected_order.append((screen, pair, side, workload))
    observed_order = [(s["screen"], s["pair"], s["side"], s["workload"]) for s in raw["samples"]]
    require(observed_order == expected_order, "Process order/count mismatch")
    for sample in raw["samples"]:
        values = sample["mib_per_second"]
        require(len(values) == 8 and all(math.isfinite(v) and v > 0 for v in values), "Invalid timed rounds")
        require(statistics.median(values) == sample["median"], "Process median mismatch")
    for screen, workloads in summary.items():
        for workload, expected in workloads.items():
            values = {
                side: [s["median"] for s in raw["samples"] if (s["screen"], s["workload"], s["side"]) == (screen, workload, side)]
                for side in ("before", "after")
            }
            medians = {side: statistics.median(v) for side, v in values.items()}
            deltas = [(after / before - 1) * 100 for before, after in zip(values["before"], values["after"])]
            observed = {
                "median_mib_per_second": medians,
                "delta_percent": (medians["after"] / medians["before"] - 1) * 100,
                "slower_pairs": sum(d < 0 for d in deltas),
                "paired_delta_percent": deltas,
                "process_median_range": {side: [min(v), max(v)] for side, v in values.items()},
            }
            require(observed == expected, f"Summary mismatch: {screen}/{workload}")
            require(medians == raw["summary"][screen][workload], "Raw summary mismatch")

    for name, expected in provenance["source_files"]["candidate"].items():
        require(expected["measured_sha256"] == raw["candidate_source_sha256"][name], f"Candidate source record mismatch: {name}")
    if args.source_root:
        for side, files in provenance["source_files"].items():
            source = args.source_root / side
            actual = {str(p.relative_to(source)) for p in (source / "src").rglob("*.rs")}
            require(actual == set(files), f"Source file set mismatch: {side}")
            for name, hashes in files.items():
                require(sha((source / name).read_bytes()) == hashes["measured_sha256"], f"Source mismatch: {side}/{name}")
    print(f"Verified {len(inventory)} retained files, 80 processes, 640 timed rounds, four payloads and eight summaries")


if __name__ == "__main__":
    main()
