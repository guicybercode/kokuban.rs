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


def pss_kib(memory):
    match = re.search(r"TOTAL PSS:\s*(\d+)", memory)
    if not match:
        match = re.search(r"^\s*TOTAL\s+(\d+)", memory, re.MULTILINE)
    return int(match.group(1)) if match else None


def frame_metrics(output):
    pattern = re.compile(r"frame=(\d+) monotonic_us=(\d+) render_us=(\d+) input_to_output_present_us=(\d+|null)")
    frames = [{"frame": int(number), "monotonic_us": int(timestamp), "render_us": int(render),
               "input_to_output_present_us": None if latency == "null" else int(latency)}
              for number, timestamp, render, latency in pattern.findall(output)]
    intervals = [second["monotonic_us"] - first["monotonic_us"] for first, second in zip(frames, frames[1:])
                 if second["monotonic_us"] > first["monotonic_us"]]
    latencies = [frame["input_to_output_present_us"] for frame in frames if frame["input_to_output_present_us"] is not None]
    return {"frames": frames, "present_intervals_us": intervals,
            "presentation_call_rate_hz": 1_000_000 / statistics.mean(intervals) if intervals else None,
            "max_present_gap_ms": max(intervals) / 1000 if intervals else None,
            "mean_render_ms": statistics.mean(frame["render_us"] for frame in frames) / 1000 if frames else None,
            "input_to_output_present_ms_samples": [value / 1000 for value in latencies]}


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
    processes = device.process_tree(pid)
    if not any(process["pid"] == pid for process in processes):
        raise RuntimeError("Could not enumerate the application process tree")
    result = {"utc": datetime.now(timezone.utc).isoformat(), "scenario": args.scenario,
              "device": device.details(), "pid": pid, "sample_seconds": args.seconds,
              "process_tree_before": processes}
    log_start = device.shell("date", "+%m-%d %H:%M:%S.000")
    for stage in ("before", "after"):
        memory = device.shell("dumpsys", "meminfo", pid)
        (args.output / f"meminfo-{stage}.txt").write_text(memory + "\n")
        result[f"pss_{stage}_kib"] = pss_kib(memory)
        tree_pss = {pid: pss_kib(memory)}
        for process in processes:
            child_pid = process["pid"]
            if child_pid != pid:
                child_memory = device.shell("dumpsys", "meminfo", child_pid, check=False)
                (args.output / f"meminfo-{stage}-{child_pid}.txt").write_text(child_memory + "\n")
                tree_pss[child_pid] = pss_kib(child_memory)
        result[f"pss_{stage}_by_pid_kib"] = tree_pss
        result[f"pss_{stage}_process_tree_kib"] = sum(tree_pss.values()) if all(value is not None for value in tree_pss.values()) else None
        if stage == "before":
            selected_pids = ",".join(process["pid"] for process in processes)
            top = device.shell("top", "-b", "-d", "1", "-n", str(args.seconds + 1), "-p", selected_pids, timeout=args.seconds + 30)
            (args.output / "top.txt").write_text(top + "\n")
    if device.pid(args.package) != pid:
        raise RuntimeError("Application restarted during sampling; results are invalid")
    samples = cpu_samples(top, pid)
    result["cpu_percent_samples"] = samples
    result["cpu_percent_mean_one_core"] = statistics.mean(samples) if samples else None
    result["cpu_percent_max_one_core"] = max(samples) if samples else None
    by_process = {process["pid"]: cpu_samples(top, process["pid"]) for process in processes}
    result["cpu_percent_samples_by_pid"] = by_process
    result["process_tree_after"] = device.process_tree(pid)
    result["process_tree_changed"] = result["process_tree_after"] != processes
    complete_tree = samples and all(len(values) == len(samples) for values in by_process.values()) and not result["process_tree_changed"]
    result["cpu_percent_mean_process_tree_one_core"] = sum(statistics.mean(values) for values in by_process.values()) if complete_tree else None
    result["input_latency_ms"] = None
    result["input_latency_note"] = "Requires correlated input and presentation tracing; adb round trip is not input latency."
    frame_log = device.adb("logcat", "-d", "--pid", pid, "-T", log_start, check=False)
    (args.output / "frame-logcat.txt").write_text(frame_log + "\n")
    result["application_frame_metrics"] = frame_metrics(frame_log)
    result["application_frame_metric_note"] = "Opt-in application input-to-next-output-presentation correlation; unrelated output can satisfy it. Presentation calls are not hardware scanout."
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
    if not samples or result["pss_before_kib"] is None or result["pss_after_kib"] is None:
        raise RuntimeError("Required main-process CPU/PSS measurements are missing; inspect the retained raw data")


if __name__ == "__main__":
    main()
