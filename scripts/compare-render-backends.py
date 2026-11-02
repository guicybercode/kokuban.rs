#!/usr/bin/env python3
"""Compare CPU time per presented frame between the Linux presenters.

Launches the same Kokuban binary with KOKUBAN_RENDER=software and
KOKUBAN_RENDER=gpu, runs an identical scrolling workload through `-e`, and
samples the terminal process's CPU time and peak RSS from /proc until it exits.
The renderer log lines (`Linux renderer: ...` and `presented N frames`) tell
which presenter actually ran and normalize CPU time per frame.

Run inside a display session (xvfb-run, Weston, or a real desktop):
  python3 scripts/compare-render-backends.py target/release/kokuban --runs 3

This measures the terminal process only (not the compositor, not the GPU), so
on a CPU rasterizer such as llvmpipe/lavapipe the "gpu" numbers include Mesa's
CPU work. No thresholds; results are printed and optionally written as JSON.
Uses only the Python standard library.
"""

import argparse
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import sys
import tempfile
import time

RENDERER_LINE = re.compile(r"Linux renderer: (\w+) \((.*)\)")
FRAMES_LINE = re.compile(r"Linux renderer (\w+): presented (\d+) frames")
WORKLOAD = r'''
import sys, time
lines = int(sys.argv[1])
pause_every = int(sys.argv[2])
pause = float(sys.argv[3])
palette = ["\x1b[31m", "\x1b[32m", "\x1b[33m", "\x1b[34m", "\x1b[1m", "\x1b[4m", "\x1b[0m"]
text = "The quick brown fox jumps over the lazy dog 0123456789 ─│┌┐└┘ αβγδ "
out = sys.stdout
for index in range(lines):
    out.write(palette[index % len(palette)] + ("%6d " % index) + text[index % 20:] + text[:index % 20] + "\x1b[0m\n")
    if index % pause_every == 0:
        out.flush()
        time.sleep(pause)
out.flush()
time.sleep(0.3)
'''


def read_cpu_seconds(pid, ticks_per_second):
    """utime+stime of one process in seconds, or None once it is gone."""
    try:
        with open(f"/proc/{pid}/stat", "rb") as handle:
            stat = handle.read()
    except OSError:
        return None
    # The command name may contain spaces; fields start after the last ')'.
    fields = stat[stat.rindex(b")") + 2:].split()
    utime, stime = int(fields[11]), int(fields[12])
    return (utime + stime) / ticks_per_second


def read_peak_rss_kib(pid):
    try:
        with open(f"/proc/{pid}/status", "r", encoding="ascii", errors="replace") as handle:
            for line in handle:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return None


def parse_renderer_log(stderr):
    """Return (backend, description, frames) from Kokuban's stderr."""
    backend = description = None
    frames = None
    for line in stderr.splitlines():
        match = RENDERER_LINE.search(line)
        if match:
            backend, description = match.group(1), match.group(2)
        match = FRAMES_LINE.search(line)
        if match:
            frames = int(match.group(2))
    return backend, description, frames


def run_once(binary, backend, lines, pause_every, pause, timeout, directory):
    env = dict(os.environ)
    env.update({
        "KOKUBAN_RENDER": backend,
        "KOKUBAN_NO_UPDATE_CHECK": "1",
        "RUST_LOG": env.get("RUST_LOG", "info"),
    })
    command = [str(binary), "-e", sys.executable, "-c", WORKLOAD,
               str(lines), str(pause_every), str(pause)]
    ticks = os.sysconf("SC_CLK_TCK")
    started = time.monotonic()
    process = subprocess.Popen(command, cwd=directory, env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    cpu_seconds = 0.0
    peak_rss_kib = None
    deadline = started + timeout
    while process.poll() is None:
        sample = read_cpu_seconds(process.pid, ticks)
        if sample is not None:
            cpu_seconds = sample
        rss = read_peak_rss_kib(process.pid)
        if rss is not None:
            peak_rss_kib = rss
        if time.monotonic() > deadline:
            process.kill()
            process.wait()
            raise RuntimeError(f"{backend}: kokuban did not exit within {timeout}s")
        time.sleep(0.01)
    wall_seconds = time.monotonic() - started
    stderr = process.stderr.read().decode("utf-8", errors="replace")
    ran_backend, description, frames = parse_renderer_log(stderr)
    return {
        "requested_backend": backend,
        "backend": ran_backend,
        "description": description,
        "exit_code": process.returncode,
        "frames": frames,
        "cpu_seconds": cpu_seconds,
        "wall_seconds": wall_seconds,
        "peak_rss_kib": peak_rss_kib,
        "stderr_tail": stderr[-2000:],
    }


def summarize(runs):
    """Medians per requested backend; CPU per frame needs the frame count."""
    summary = {}
    for run in runs:
        summary.setdefault(run["requested_backend"], []).append(run)
    result = {}
    for backend, samples in summary.items():
        ok = [run for run in samples if run["exit_code"] == 0]
        entry = {
            "runs": len(samples),
            "successful_runs": len(ok),
            "backend": sorted({run["backend"] for run in samples if run["backend"]}),
        }
        if ok:
            entry["median_cpu_seconds"] = statistics.median(run["cpu_seconds"] for run in ok)
            entry["median_wall_seconds"] = statistics.median(run["wall_seconds"] for run in ok)
            with_frames = [run for run in ok if run["frames"]]
            if with_frames:
                entry["median_frames"] = statistics.median(run["frames"] for run in with_frames)
                entry["median_cpu_ms_per_frame"] = statistics.median(
                    run["cpu_seconds"] * 1000.0 / run["frames"] for run in with_frames)
            rss = [run["peak_rss_kib"] for run in ok if run["peak_rss_kib"]]
            if rss:
                entry["median_peak_rss_kib"] = statistics.median(rss)
        result[backend] = entry
    return result


def format_table(summary):
    header = f"{'backend':<10}{'ran as':<10}{'ok':>5}{'cpu s':>9}{'frames':>8}{'ms/frame':>10}{'wall s':>8}{'rss MiB':>9}"
    rows = [header, "-" * len(header)]
    for backend, entry in summary.items():
        ran = ",".join(entry["backend"]) or "?"
        rows.append(
            f"{backend:<10}{ran:<10}{entry['successful_runs']:>3}/{entry['runs']:<2}"
            f"{entry.get('median_cpu_seconds', float('nan')):>8.3f}"
            f"{entry.get('median_frames', float('nan')):>8.0f}"
            f"{entry.get('median_cpu_ms_per_frame', float('nan')):>10.3f}"
            f"{entry.get('median_wall_seconds', float('nan')):>8.2f}"
            f"{entry.get('median_peak_rss_kib', float('nan')) / 1024:>9.1f}")
    return "\n".join(rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--backends", nargs="+", default=["software", "gpu"])
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--lines", type=int, default=20000)
    parser.add_argument("--pause-every", type=int, default=200,
                        help="flush and pause after this many lines so frames are produced")
    parser.add_argument("--pause-ms", type=float, default=2.0)
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--output", type=Path, help="write runs and summary as JSON")
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error(f"{args.binary} is not a file")
    runs = []
    with tempfile.TemporaryDirectory(prefix="kokuban-render-compare-") as directory:
        for index in range(args.runs):
            # Alternate backends so drift on the host affects both equally.
            for backend in args.backends:
                run = run_once(args.binary, backend, args.lines, args.pause_every,
                               args.pause_ms / 1000.0, args.timeout, directory)
                run["index"] = index
                runs.append(run)
                print(f"run {index} {backend:<9} -> {run['backend'] or '?':<9} exit {run['exit_code']} "
                      f"cpu {run['cpu_seconds']:.3f}s frames {run['frames']} wall {run['wall_seconds']:.2f}s",
                      flush=True)
                if run["exit_code"] != 0:
                    print(run["stderr_tail"], file=sys.stderr)
    summary = summarize(runs)
    print()
    print(format_table(summary))
    if args.output:
        args.output.write_text(json.dumps({
            "binary": str(args.binary),
            "lines": args.lines,
            "pause_every": args.pause_every,
            "pause_ms": args.pause_ms,
            "runs": runs,
            "summary": summary,
        }, indent=2) + "\n")
        print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
