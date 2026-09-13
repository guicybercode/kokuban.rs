#!/usr/bin/env python3
"""Verify the retained four-terminal evidence without building or benchmarking."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

REV = "d54f801e9dec41a6eb4f159397e45939c367531d"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read_json(path):
    return json.loads(path.read_bytes())


def record(path):
    data = path.read_bytes()
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def file_records(root, exclude=()):
    return {p.relative_to(root).as_posix(): record(p)
            for p in sorted(root.rglob("*"))
            if p.is_file() and p.relative_to(root).as_posix() not in exclude}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--original-artifact", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    evidence = root / "evidence"
    expected = read_json(root / "package-sha256.json")
    require(file_records(root, ("package-sha256.json",)) == expected,
            "Complete package inventory differs from package-sha256.json")
    inventory = read_json(root / "original-download-inventory.json")
    selection = read_json(root / "retention-selection.json")
    retained = selection["originals_retained"]
    omitted = selection["omitted_originals"]
    require(len(inventory) == selection["original_count"] == 837, "Original file count")
    require(sum(v["bytes"] for v in inventory.values()) == selection["original_bytes"] == 46209731,
            "Original total bytes")
    require(set(retained).isdisjoint(omitted), "Retained/omitted overlap")
    require(set(retained) | set(omitted) == set(inventory), "Incomplete original selection")
    require(len(retained) == 214 and len(omitted) == 623, "Selection counts")
    evidence_records = file_records(evidence)
    require(len(evidence_records) == selection["evidence_files"] == 223, "Evidence count")
    require(sum(v["bytes"] for v in evidence_records.values()) == 1400205, "Evidence bytes")
    require(set(evidence_records) == set(retained.values()) | set(selection["supplemental_evidence_files"]),
            "Incomplete evidence mapping")
    for original, destination in retained.items():
        require(evidence_records[destination] == inventory[original], "Original retention: " + original)
    empty_hash = hashlib.sha256(b"").hexdigest()
    cache_count = empty_count = 0
    for original, reason in omitted.items():
        if reason == "generated_graphics_cache":
            require("/cache/" in original, "Unexpected cache path: " + original)
            cache_count += 1
        elif reason == "empty_generated_ghostty_default_config":
            require(original.endswith("/config/ghostty/config.ghostty") and
                    inventory[original] == {"bytes": 0, "sha256": empty_hash},
                    "Unexpected omitted Ghostty config: " + original)
            empty_count += 1
        else:
            raise ValueError("Unknown omission reason: " + reason)
    require(cache_count == 616 and empty_count == 7, "Omission category counts")
    require(sum(inventory[p]["bytes"] for p in omitted) == selection["omitted_bytes"] == 45572191,
            "Omitted bytes")
    manifest = read_json(evidence / "manifest.json")
    require({p: v["sha256"] for p, v in evidence_records.items() if p != "manifest.json"}
            == manifest["evidence_sha256"], "Frozen evidence hashes")
    source = read_json(root / "auditor-source.json")
    require(evidence_records["audit.py"]["sha256"] == source["adapted_sha256"], "Auditor digest")
    pinned_paths = ["Cargo.toml", "Cargo.lock", "scripts/compare-terminal-performance.py",
                    "scripts/linux-resource-smoke.py", "scripts/linux-launch-smoke.py",
                    ".github/workflows/linux-terminal-comparison.yml"]
    for path in pinned_paths:
        data = subprocess.check_output(["git", "-C", str(args.repo), "show", REV + ":" + path])
        local = evidence / path if path in ("Cargo.toml", "Cargo.lock") else evidence / "harness" / path
        require(local.read_bytes() == data, "Pinned Git snapshot: " + path)
    command = [sys.executable, "-B", str(evidence / "audit.py"), "--artifact", str(evidence),
               "--ci-run", str(evidence / "ci-run.json"), "--ci-log", str(evidence / "ci.log"),
               "--repo", str(args.repo.resolve())]
    completed = subprocess.run(command, check=True, capture_output=True, text=True)
    audited = json.loads(completed.stdout)
    require(audited["run"] == 34746364220 and audited["processes"] == 28, "Audited identity/count")
    result = {"status": "PASS", "run": audited["run"], "source_revision": REV,
              "evidence_files": 223, "evidence_bytes": 1400205,
              "original_inventory_files": 837, "originals_retained": 214,
              "omitted_cache_files": 616, "omitted_empty_ghostty_configs": 7,
              "pinned_git_snapshots_verified": len(pinned_paths),
              "auditor_sha256": source["adapted_sha256"],
              "raw_report_sha256": audited["raw_sha256"],
              "auditor_results": audited}
    if (root / "verification.json").exists():
        require(read_json(root / "verification.json") == result, "Recorded verification differs")
    if args.original_artifact:
        require(file_records(args.original_artifact) == inventory, "Complete original download bytes")
        result["original_artifact_verified"] = True
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
