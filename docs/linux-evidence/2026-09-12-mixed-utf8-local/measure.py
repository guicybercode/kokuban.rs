import hashlib
import json
import os
import statistics
import subprocess
import time
from pathlib import Path

root = Path(__file__).resolve().parent
out = root / "measurements"
sha = lambda data: hashlib.sha256(data).hexdigest()
payloads = json.loads((out / "payloads.json").read_text())
assert (root / "harness-baseline/Cargo.lock").read_bytes() == (root / "harness-candidate/Cargo.lock").read_bytes()
assert (root / "harness-baseline/Cargo.toml").read_bytes() == (root / "harness-candidate/Cargo.toml").read_bytes()
for name, expected in payloads.items():
    data = (out / (name + ".bin")).read_bytes()
    assert {"bytes": len(data), "sha256": sha(data)} == expected

report = {
    "scope": "macOS decoder-only screening; no PTY, renderer or terminal ranking",
    "design": "5 alternating pairs per screen; 8 rounds x approximately 4MiB; fresh process per workload; one untimed warm decode; 16KiB input chunks; no concurrent owned builds or benchmark generation",
    "baseline_revision": (root / "baseline-revision.txt").read_text().strip(),
    "candidate_patch_sha256": sha((root / "candidate.patch").read_bytes()),
    "binary_sha256": {side: sha((out / side).read_bytes()) for side in ("before", "after")},
    "candidate_source_sha256": {str(p.relative_to(root / "candidate")): sha(p.read_bytes()) for p in sorted((root / "candidate/src").rglob("*.rs"))},
    "harness_sha256": {str(p.relative_to(root)): sha(p.read_bytes()) for side in ("baseline", "candidate") for p in sorted((root / ("harness-" + side)).iterdir()) if p.is_file()},
    "rustc": subprocess.check_output(["rustc", "--version", "--verbose"], text=True),
    "platform": os.uname()._asdict() if hasattr(os.uname(), "_asdict") else list(os.uname()),
    "payloads": payloads,
    "loadavg_before": os.getloadavg(),
    "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "status": "running",
    "samples": [],
}
assert report["binary_sha256"]["before"] != report["binary_sha256"]["after"], "Refuse identical binaries"
for screen in ("alternate", "primary"):
    for pair in range(1, 6):
        for side in (("before", "after") if pair % 2 else ("after", "before")):
            for workload in ("ascii", "ansi", "unicode", "short_lines"):
                values = json.loads(subprocess.check_output([str(out / side), str(out / (workload + ".bin")), "8", screen], text=True))
                assert len(values) == 8 and all(value > 0 for value in values)
                report["samples"].append({"screen": screen, "pair": pair, "side": side, "workload": workload, "mib_per_second": values, "median": statistics.median(values)})
        (out / "screening.json").write_text(json.dumps(report, indent=2) + "\n")
        print(screen, pair, flush=True)
report["summary"] = {screen: {workload: {side: statistics.median(s["median"] for s in report["samples"] if (s["screen"], s["workload"], s["side"]) == (screen, workload, side)) for side in ("before", "after")} for workload in payloads} for screen in ("alternate", "primary")}
report["finished_utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
report["loadavg_after"] = os.getloadavg()
report["status"] = "completed"
(out / "screening.json").write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps(report["summary"], indent=2))
