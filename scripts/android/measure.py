#!/usr/bin/env python3
"""Capture raw Android memory/CPU/frame evidence and APK sizes for a named scenario.

Run after arranging the scenario in the terminal. Does not inject input. CPU is
process CPU (100% = one core) as reported by Android top, not host emulator CPU.
Frame stats may be unavailable for NativeActivity; missing data stays missing.
"""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import statistics
import zipfile

from device import Device


def cpu_samples(output, pid):
    samples = []
    cpu_index = None
    for line in output.splitlines():
        # toybox highlights the sorted column as S[%CPU], without a space.
        columns = line.replace("[", " ").replace("]", " ").split()
        if "PID" in columns and "%CPU" in columns:
            cpu_index = columns.index("%CPU")
        elif cpu_index is not None and columns and columns[0] == str(pid):
            try:
                samples.append(float(columns[cpu_index].rstrip("%")))
            except (ValueError, IndexError):
                continue
    # First top sample covers an unspecified prior interval, not our window.
    return samples[1:]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial")
    parser.add_argument("--package", default="com.kokuban.terminal")
    parser.add_argument("--scenario", required=True)
    parser.add_argument("--seconds", type=int, default=15)
    parser.add_argument("--apk", type=Path)
    parser.add_argument("--output", type=Path, default=Path("target/android-evidence/measurements"))
    args = parser.parse_args()
    if not 2 <= args.seconds <= 300:
        parser.error("--seconds must be between 2 and 300")
    args.output.mkdir(parents=True, exist_ok=True)
    device = Device(args.serial)
    pid = device.pid(args.package)
    result = {"utc": datetime.now(timezone.utc).isoformat(), "scenario": args.scenario,
              "device": device.details(), "pid": pid, "sample_seconds": args.seconds}
    for stage in ("before", "after"):
        memory = device.shell("dumpsys", "meminfo", args.package)
        (args.output / f"meminfo-{stage}.txt").write_text(memory + "\n")
        pss = re.search(r"TOTAL PSS:\s*(\d+)", memory)
        if not pss:
            pss = re.search(r"^\s*TOTAL\s+(\d+)", memory, re.MULTILINE)
        result[f"pss_{stage}_kib"] = int(pss.group(1)) if pss else None
        if stage == "before":
            top = device.shell("top", "-b", "-d", "1", "-n", str(args.seconds + 1), "-p", pid, timeout=args.seconds + 30)
            (args.output / "top.txt").write_text(top + "\n")
    if device.pid(args.package) != pid:
        raise RuntimeError("Application restarted during sampling; results are invalid")
    samples = cpu_samples(top, pid)
    result["cpu_percent_samples"] = samples
    result["cpu_percent_mean_one_core"] = statistics.mean(samples) if samples else None
    result["cpu_percent_max_one_core"] = max(samples) if samples else None
    result["input_latency_ms"] = None
    result["input_latency_note"] = "Requires correlated input and presentation tracing; adb round trip is not input latency."
    frames = device.shell("dumpsys", "gfxinfo", args.package, "framestats", check=False)
    (args.output / "gfxinfo.txt").write_text(frames + "\n")
    result["frame_timing_note"] = "Raw gfxinfo only; NativeActivity softbuffer may not emit Android View frame stats."
    if args.apk:
        result["apk_bytes"] = args.apk.stat().st_size
        with zipfile.ZipFile(args.apk) as apk:
            result["native_libraries"] = [{"path": item.filename, "bytes": item.file_size,
                                           "compressed_bytes": item.compress_size}
                                          for item in apk.infolist() if item.filename.endswith(".so")]
    device.screenshot(args.output / "scenario.png")
    (args.output / "results.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
