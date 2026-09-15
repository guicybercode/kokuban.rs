#!/usr/bin/env python3
"""Rebuild summary.json from the retained paired reports (standard library only).

The runner rejects a whole report when any sample saw a different grid. On this
host the tiling window manager occasionally resized a window, so the summary
also recomputes paired changes using only pairs whose two samples observed the
same stable rows/columns. Both views are kept; nothing is dropped silently.
"""

import json
from pathlib import Path
import statistics

HERE = Path(__file__).resolve().parent
REPORTS = {"ab": "ab-report.json", "aa_after": "aa-after-report.json"}


def distribution(values):
    return {"median": statistics.median(values), "min": min(values), "max": max(values),
            "samples": values} if values else None


def stable_geometry(sample, workloads):
    measured = sample["measurements"]
    return all(measured["workloads"][name]["geometry_before"] == measured["workloads"][name]["geometry_after"]
               == measured["initial_geometry"] for name in workloads)


def summarize(report):
    workloads = list(report["payloads"])
    pairs = list(zip(report["terminals"]["before"]["samples"], report["terminals"]["after"]["samples"]))
    matched = [(before, after) for before, after in pairs
               if before["status"] == after["status"] == "passed"
               and before["measurements"]["initial_geometry"] == after["measurements"]["initial_geometry"]
               and stable_geometry(before, workloads) and stable_geometry(after, workloads)]
    result = {
        "runner_status": report["status"], "runner_reasons": report["comparability"]["reasons"],
        "binaries": {side: report["terminals"][side]["sha256"] for side in ("before", "after")},
        "source_refs": {side: report["terminals"][side]["source_ref"] for side in ("before", "after")},
        "pairs": len(pairs), "geometry_matched_pairs": [before["pair"] for before, _ in matched],
        "geometries": {side: [sample["measurements"]["initial_geometry"] for sample in report["terminals"][side]["samples"]]
                       for side in ("before", "after")},
        "throughput_change_percent": {}, "throughput_mib_per_second": {}, "idle": {},
    }
    for name in workloads:
        changes = [(after["measurements"]["workloads"][name]["mib_per_second"]
                    / before["measurements"]["workloads"][name]["mib_per_second"] - 1) * 100
                   for before, after in matched]
        result["throughput_change_percent"][name] = {
            **distribution(changes), "pairs_slower": sum(change < 0 for change in changes)}
        result["throughput_mib_per_second"][name] = {
            side: distribution([sample["measurements"]["workloads"][name]["mib_per_second"]
                                for sample in report["terminals"][side]["samples"]]) for side in ("before", "after")}
    for metric in ("terminal_cpu_seconds", "idle_wakeups_per_second"):
        differences = [after["measurements"]["idle"][metric] - before["measurements"]["idle"][metric]
                       for before, after in matched
                       if before["measurements"].get("idle", {}).get(metric) is not None
                       and after["measurements"].get("idle", {}).get(metric) is not None]
        result["idle"][metric] = {
            side: distribution([sample["measurements"]["idle"][metric]
                                for sample in report["terminals"][side]["samples"]
                                if sample["measurements"].get("idle", {}).get(metric) is not None])
            for side in ("before", "after")}
        result["idle"][metric]["after_minus_before_matched_pairs"] = distribution(differences)
    return result


def main():
    summary = {name: summarize(json.loads((HERE / filename).read_text())) for name, filename in REPORTS.items()}
    (HERE / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")


if __name__ == "__main__":
    main()
