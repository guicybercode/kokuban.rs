#!/usr/bin/env python3
"""Verify the compact package and recompute all thread/paired observations."""

import importlib.util
import json
from pathlib import Path
import statistics
import zipfile


ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("thread_evidence_audit", ROOT / "audit.py")
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


def verify():
    manifest = json.loads((ROOT / "manifest.json").read_text())
    for filename, expected in manifest["package_files"].items():
        data = (ROOT / filename).read_bytes()
        assert len(data) == expected["bytes"] and AUDIT.sha(data) == expected["sha256"], filename
    retained = json.loads((ROOT / "audit.json").read_text())
    with zipfile.ZipFile(ROOT / "raw-evidence.zip") as archive:
        names = archive.namelist()
        assert len(names) == len(set(names)) and set(names) == set(manifest["zip_members"])
        for name, expected in manifest["zip_members"].items():
            assert not name.startswith("/") and ".." not in Path(name).parts
            data = archive.read(name)
            assert len(data) == expected["bytes"] and AUDIT.sha(data) == expected["sha256"], name
        measured = AUDIT.audit_measurements(archive.read)
        assert all(retained[k] == v for k, v in measured.items())
        # Omitted binary/archive hashes were checked against the complete download
        # by audit.py; their bytes are deliberately not claimed to be in this ZIP.
        for side, build in retained["builds"].items():
            omitted = manifest["omitted_large_files"]
            assert omitted[AUDIT.PREFIX + side + "-build/kokuban"]["sha256"] == build["binary_sha256"]
            assert omitted[AUDIT.PREFIX + side + "-source.tar.gz"]["sha256"] == build["archive_sha256"]
        source_manifests = json.loads(archive.read("source-file-manifests.json"))
        for side, build in retained["builds"].items():
            assert len(source_manifests[side]) == build["source_files_git_verified"]
        old = json.loads(archive.read("prior-run-34749172188-report.json"))
        current = json.loads(archive.read(AUDIT.PREFIX + "measurements/report.json"))
        for side in ("before", "after"):
            assert old["terminals"][side]["sha256"] == current["terminals"][side]["sha256"]
            assert old["terminals"][side]["source_ref"] == current["terminals"][side]["source_ref"]
        old_changes = [(after["measurements"]["workloads"]["short_lines"]["write_and_dsr_seconds"] /
                        before["measurements"]["workloads"]["short_lines"]["write_and_dsr_seconds"] - 1) * 100
                       for before, after in zip(old["terminals"]["before"]["samples"], old["terminals"]["after"]["samples"])]
        AUDIT.close(statistics.median(old_changes), 9.127406231683821)
    print(f"PASS: {len(names)} retained files, 40 workloads, 320 raw stats, 160 thread deltas; original full-artifact audit retained.")


if __name__ == "__main__":
    verify()
