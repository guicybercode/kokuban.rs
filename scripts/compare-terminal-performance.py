#!/usr/bin/env python3
"""Compare raw-PTY throughput and protocol RTT on one Linux display session.

DSR acknowledges terminal processing, not frame presentation or input-to-photon
latency. Configurations, payloads, logs, individual samples and limitations are
retained alongside the report. No packages are installed and no terminals ranked.
The PTY driver and Kokuban launch also run on macOS for paired Kokuban revisions.
"""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import runpy
import shlex
import shutil
import statistics
import subprocess
import sys
import termios
import time
import tty


HELPERS = runpy.run_path(str(Path(__file__).with_name("linux-resource-smoke.py")))
record = HELPERS["record"]
barrier = HELPERS["barrier"]
write_all = HELPERS["write_all"]
terminal_size = HELPERS["terminal_size"]
linux_process_stats = HELPERS["process_stats"]
wait_for = HELPERS["wait_for"]
stop_owned_child = HELPERS["stop_owned_child"]
TERMINALS = ("kokuban", "ghostty", "alacritty", "kitty")
FONT = "Menlo" if sys.platform == "darwin" else "DejaVu Sans Mono"


def parse_ps_cpu_time(text: str) -> float:
    """Parse BSD ps cumulative time: [dd-][hh:]mm:ss.cc."""
    days, _, clock = text.strip().rpartition("-")
    parts = clock.split(":")
    if not 2 <= len(parts) <= 3 or not all(parts):
        raise ValueError(f"unrecognized ps CPU time: {text!r}")
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + float(part)
    return seconds + (int(days) * 86400 if days else 0)


def parse_ps_stats(pid: int, text: str) -> dict:
    fields = text.split()
    if len(fields) != 2:
        raise ValueError(f"ps output for PID {pid} is not 'time rss': {text!r}")
    return {"pid": pid, "cpu_seconds": parse_ps_cpu_time(fields[0]), "rss_kib": int(fields[1]),
            "source": "ps", "monotonic_seconds": time.monotonic()}


def darwin_process_stats(pid: int) -> dict:
    # BSD ps reports user+system time at 10 ms resolution; RSS is in KiB.
    result = subprocess.run(["ps", "-o", "time=,rss=", "-p", str(pid)],
                            capture_output=True, text=True, timeout=10, check=True)
    return parse_ps_stats(pid, result.stdout)


def process_stats(pid: int) -> dict:
    if sys.platform == "darwin":
        return darwin_process_stats(pid)
    return linux_process_stats(pid)


def parse_top_idle_wakeups(text: str, pid: int) -> list[int]:
    """Cumulative IDLEW values from `top -l N -stats pid,idlew` logging samples."""
    values = []
    for line in text.splitlines():
        fields = line.split()
        if len(fields) == 2 and fields[0] == str(pid):
            values.append(int(fields[1].rstrip("+")))
    return values


def idle_wakeups(pid: int, seconds: int) -> dict:
    """Wait for `seconds`, observing macOS idle wakeups when top exposes them."""
    if sys.platform != "darwin":
        time.sleep(seconds)
        return {"status": "unavailable", "reason": "idle wakeups are read only from macOS top"}
    command = ["top", "-l", "2", "-s", str(seconds), "-pid", str(pid), "-stats", "pid,idlew"]
    result = subprocess.run(command, capture_output=True, text=True, timeout=seconds + 30)
    observation = {"command": command, "status": "unavailable", "returncode": result.returncode}
    values = parse_top_idle_wakeups(result.stdout, pid)
    if result.returncode == 0 and len(values) >= 2 and values[-1] >= values[0]:
        observation.update(status="read", samples=values, delta=values[-1] - values[0])
    else:
        observation["output"] = (result.stdout + result.stderr)[-2000:]
    return observation


def observe_idle(pid: int, seconds: int) -> dict:
    """CPU and idle wakeups of a terminal with a hidden cursor and no PTY output."""
    before = process_stats(pid)
    started = time.monotonic()
    wakeups = idle_wakeups(pid, seconds)
    elapsed = time.monotonic() - started
    after = process_stats(pid)
    cpu = after["cpu_seconds"] - before["cpu_seconds"]
    delta = wakeups.get("delta")
    return {"requested_seconds": seconds, "elapsed_seconds": elapsed, "terminal_cpu_seconds": cpu,
            "cpu_percent_one_core": 100 * cpu / elapsed if elapsed > 0 else None,
            "idle_wakeups": delta,
            "idle_wakeups_per_second": delta / seconds if delta is not None else None,
            "idle_wakeups_observation": wakeups,
            "limits": ["CPU counters are quantized (10 ms from BSD ps).",
                       "top samples bracket the requested interval, not the monotonic elapsed time."]}


def payloads(byte_count: int) -> dict[str, bytes]:
    lines = {
        "ascii": b"abcdefghijklmnopqrstuvwxyz0123456789 " * 2 + b"\r\n",
        "ansi": b"\x1b[31mred\x1b[0m \x1b[1;34mblue\x1b[0m \x1b[38;2;80;200;90mtruecolor\x1b[0m\r\n",
        "unicode": ("café 日本 λ e\u0301 " * 3 + "\r\n").encode(),
        "short_lines": b"x\r\n",
    }
    return {name: line * max(1, byte_count // len(line)) for name, line in lines.items()}


def distribution(values: list[float]) -> dict:
    median = statistics.median(values)
    return {"count": len(values), "median": median, "min": min(values), "max": max(values),
            "median_absolute_deviation": statistics.median(abs(value - median) for value in values),
            "samples": values}


def command_observation(command: list[str]) -> dict:
    """Capture provenance outside the timed phases; missing tools are explicit."""
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=10)
        return {"command": command, "status": result.returncode,
                "output": (result.stdout + result.stderr).strip()}
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"command": command, "error": str(error)}


def parse_thread_stat(text: str, pid: int, tid: int) -> dict:
    """Parse proc stat without splitting spaces or parentheses inside comm."""
    opening, closing = text.find(" ("), text.rfind(")")
    if opening <= 0 or closing <= opening or int(text[:opening]) != tid:
        raise ValueError("thread stat TID/name prefix is invalid")
    name = text[opening + 2:closing]
    fields = text[closing + 1:].split()
    if len(fields) < 20:
        raise ValueError("thread stat is truncated")
    user, system, start = (int(fields[index]) for index in (11, 12, 19))
    if min(user, system, start) < 0:
        raise ValueError("thread stat has negative counters")
    return {"tid": tid, "name": name, "start_ticks": start, "user_ticks": user,
            "system_ticks": system, "cpu_ticks": user + system,
            "is_main_thread": tid == pid, "is_terminal_reader": name == "terminal-reader"}


def thread_cpu_snapshot(pid: int, proc_root: Path = Path("/proc"), ticks_per_second=None) -> dict:
    snapshot = {"pid": pid, "clock": "time.monotonic", "read_started_seconds": time.monotonic(),
                "ticks_per_second": os.sysconf("SC_CLK_TCK") if ticks_per_second is None else ticks_per_second,
                "enumeration_complete": False, "listed_tids": [], "threads": []}
    try:
        task = proc_root / str(pid) / "task"
        tids = sorted(int(path.name) for path in task.iterdir() if path.name.isdecimal())
        snapshot.update(enumeration_complete=True, listed_tids=tids)
        for tid in tids:
            row = {"tid": tid, "read_started_seconds": time.monotonic()}
            try:
                row["stat"] = (task / str(tid) / "stat").read_text()
                row.update(parse_thread_stat(row["stat"], pid, tid), status="read")
            except (OSError, ValueError) as error:
                row.update(status="unavailable", error=f"{type(error).__name__}: {error}")
            row["read_finished_seconds"] = time.monotonic()
            snapshot["threads"].append(row)
    except OSError as error:
        snapshot["enumeration_error"] = f"{type(error).__name__}: {error}"
    snapshot["read_finished_seconds"] = time.monotonic()
    snapshot["status"] = ("complete" if snapshot["enumeration_complete"] and snapshot["threads"]
                          and all(row["status"] == "read" for row in snapshot["threads"]) else "partial")
    return snapshot


def thread_cpu_deltas(before: dict, after: dict) -> dict:
    """Only identical readable TID/start-time pairs receive CPU deltas."""
    first = {row["tid"]: row for row in before["threads"]}
    last = {row["tid"]: row for row in after["threads"]}
    rows = []
    for tid in sorted(set(first) | set(last)):
        old, new = first.get(tid), last.get(tid)
        row = {"tid": tid, "status": "unavailable", "cpu_ticks_delta": None, "cpu_seconds_delta": None}
        if old is not None and new is not None and old["status"] == new["status"] == "read":
            row.update(before_name=old["name"], after_name=new["name"],
                       before_start_ticks=old["start_ticks"], after_start_ticks=new["start_ticks"],
                       is_main_thread=old["is_main_thread"],
                       terminal_reader_before=old["is_terminal_reader"], terminal_reader_after=new["is_terminal_reader"])
            if before["pid"] != after["pid"] or old["start_ticks"] != new["start_ticks"]:
                row["status"] = "identity_changed"
            elif before["ticks_per_second"] != after["ticks_per_second"] or before["ticks_per_second"] <= 0:
                row["status"] = "clock_ticks_changed"
            elif new["user_ticks"] < old["user_ticks"] or new["system_ticks"] < old["system_ticks"]:
                row["status"] = "counter_regressed"
            else:
                delta = new["cpu_ticks"] - old["cpu_ticks"]
                row.update(status="matched", cpu_ticks_delta=delta,
                           cpu_seconds_delta=delta / before["ticks_per_second"],
                           user_ticks_delta=new["user_ticks"] - old["user_ticks"],
                           system_ticks_delta=new["system_ticks"] - old["system_ticks"],
                           read_window_seconds={"minimum": new["read_started_seconds"] - old["read_finished_seconds"],
                                                "maximum": new["read_finished_seconds"] - old["read_started_seconds"]})
        elif old is None and before["enumeration_complete"] and new["status"] == "read":
            row.update(status="newly_observed", after_name=new["name"], after_start_ticks=new["start_ticks"])
        elif new is None and after["enumeration_complete"] and old["status"] == "read":
            row.update(status="disappeared", before_name=old["name"], before_start_ticks=old["start_ticks"])
        rows.append(row)
    return {"before": before, "after": after, "threads": rows,
            "newly_observed_tids": [row["tid"] for row in rows if row["status"] == "newly_observed"],
            "disappeared_tids": [row["tid"] for row in rows if row["status"] == "disappeared"],
            "limits": ["Sequential proc reads are not atomic; each thread has its own read window.",
                       "Thread snapshots surround the existing process CPU reads and write-to-DSR clock; observer overhead is outside that clock.",
                       "Counters are quantized; a zero matched delta does not prove no work. Missing/new/reused threads have null deltas.",
                       "Do not sum missing threads as zero or expect thread deltas to equal the process CPU delta."]}


def controlled_child(directory: Path, thread_cpu: bool = False) -> None:
    record(directory, "child.json", process_stats(os.getpid()))
    original = termios.tcgetattr(0)
    try:
        tty.setraw(0)
        case = wait_for(lambda: json.loads((directory / "case.json").read_text())
                        if (directory / "case.json").exists() else None, "benchmark case")
        if bool(case.get("thread_cpu", False)) != thread_cpu:
            raise ValueError("thread CPU flag differs between driver argv and benchmark case")
        # Pre-load payloads before any measured interval.
        data = {name: Path(path).read_bytes() for name, path in case["payloads"].items()}
        time.sleep(case["settle_seconds"])
        write_all(b"\x1b[?25l")
        if case["screen"] == "alternate":
            write_all(b"\x1b[?1049h")
        write_all(b"\x1b[2J\x1b[H")
        barrier(b"ready")
        rtts = []
        for index in range(35):
            started = time.perf_counter()
            barrier(b"rtt")
            elapsed = time.perf_counter() - started
            if index >= 5:
                rtts.append(elapsed)
        observations = {"initial_geometry": list(terminal_size()), "protocol_rtt_seconds": rtts,
                        "workloads": {}, "term": os.environ.get("TERM"), "thread_cpu_enabled": thread_cpu}
        for name, payload in data.items():
            # Warm the parser/font paths outside the timed sample, then clear.
            write_all(payload)
            barrier(b"warm")
            write_all(b"\x1b[3J\x1b[2J\x1b[H")
            barrier(b"start")
            geometry_before = list(terminal_size())
            if thread_cpu:
                threads_before = thread_cpu_snapshot(case["terminal_pid"])
            cpu_before = process_stats(case["terminal_pid"])
            started = time.perf_counter()
            write_all(payload)
            written = time.perf_counter()
            reply = barrier(b"done")
            finished = time.perf_counter()
            cpu_after = process_stats(case["terminal_pid"])
            if thread_cpu:
                threads_after = thread_cpu_snapshot(case["terminal_pid"])
            observations["workloads"][name] = {
                "bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest(),
                "write_seconds": written - started, "drain_rtt_seconds": finished - written,
                "write_and_dsr_seconds": finished - started,
                "mib_per_second": len(payload) / (1024 * 1024) / (finished - started),
                "reply_hex": reply, "geometry_before": geometry_before,
                "geometry_after": list(terminal_size()),
                "terminal_cpu_seconds": cpu_after["cpu_seconds"] - cpu_before["cpu_seconds"],
                "terminal_rss_before_kib": cpu_before["rss_kib"],
                "terminal_rss_after_kib": cpu_after["rss_kib"],
            }
            if thread_cpu:
                diagnostic = thread_cpu_deltas(threads_before, threads_after)
                diagnostic.update(process_cpu_before=cpu_before, process_cpu_after=cpu_after,
                                  write_and_dsr_started_perf_counter=started,
                                  write_and_dsr_finished_perf_counter=finished)
                observations["workloads"][name]["thread_cpu"] = diagnostic
        idle_seconds = case.get("idle_seconds", 0)
        if idle_seconds:
            write_all(b"\x1b[3J\x1b[2J\x1b[H")
            barrier(b"idle")
            observations["idle"] = observe_idle(case["terminal_pid"], idle_seconds)
        record(directory, "result.json", observations)
        wait_for(lambda: (directory / "finish").exists(), "benchmark shutdown")
    except BaseException as error:
        record(directory, "child-error.json", {"error": f"{type(error).__name__}: {error}"})
        raise
    finally:
        write_all(b"\x1b[?1049l\x1b[?25h")
        termios.tcsetattr(0, termios.TCSANOW, original)


def terminal_command(name: str, binary: Path, version: str, directory: Path, args) -> tuple[list, dict]:
    history = 0 if args.screen == "alternate" else args.scrollback_lines
    width_offset, height_offset = getattr(args, "cell_adjustments", {}).get(name, (0, 0))
    child = [sys.executable, str(Path(__file__).resolve()), "--child-dir", str(directory)]
    if getattr(args, "thread_cpu", False):
        child.append("--thread-cpu")
    # Kokuban's size is logical pixels, the other terminals use points. Convert
    # at 96 dpi; actual cells/pixels are still checked because DPI varies.
    point_size = args.font_pixels * 72 / 96
    if name == "kokuban":
        if width_offset or height_offset:
            raise ValueError("Kokuban does not expose cell spacing adjustments")
        config = (f'[font]\nfamily = "{FONT}"\nsize = {args.font_pixels}\n'
                  f'[window]\ncolumns = {args.columns}\nrows = {args.rows}\nscrollback_lines = {history}\n'
                  '[images]\nenabled = false\n')
        filename = "kokuban.toml"
        launch_shell = None
        if sys.platform == "darwin":
            # The macOS app takes no arguments; KOKUBAN_SHELL selects the PTY program.
            launch_shell = directory / "kokuban-shell.sh"
            launch_shell.write_text("#!/bin/sh\nexec " + shlex.join(child) + "\n")
            launch_shell.chmod(0o755)
            command = [str(binary)]
        else:
            command = [str(binary), "-e", *child]
    elif name == "alacritty":
        match = re.search(r"alacritty (\d+)\.(\d+)", version)
        legacy = bool(match and tuple(map(int, match.groups())) < (0, 13))
        if legacy:
            filename = "alacritty.yml"
            config = (f'font:\n  normal:\n    family: "{FONT}"\n  size: {point_size}\n'
                      f'window:\n  dimensions:\n    columns: {args.columns}\n    lines: {args.rows}\n'
                      '  padding:\n    x: 0\n    y: 0\n  decorations: none\n'
                      f'scrolling:\n  history: {history}\n')
            if width_offset or height_offset:
                config = config.replace('  normal:\n',
                                        f'  offset:\n    x: {width_offset}\n    y: {height_offset}\n  normal:\n', 1)
        else:
            filename = "alacritty.toml"
            config = (f'[font]\nsize = {point_size}\n[font.normal]\nfamily = "{FONT}"\n'
                      '[window]\ndecorations = "None"\n[window.padding]\nx = 0\ny = 0\n'
                      f'[window.dimensions]\ncolumns = {args.columns}\nlines = {args.rows}\n'
                      f'[scrolling]\nhistory = {history}\n')
            if width_offset or height_offset:
                config += f'[font.offset]\nx = {width_offset}\ny = {height_offset}\n'
        command = [str(binary), "--config-file", str(directory / filename), "-e", *child]
    elif name == "kitty":
        filename = "kitty.conf"
        config = (f'font_family {FONT}\nfont_size {point_size}\nscrollback_lines {history}\n'
                  'remember_window_size no\nwindow_padding_width 0\nwindow_margin_width 0\n'
                  f'initial_window_width {args.columns}c\ninitial_window_height {args.rows}c\n'
                  'hide_window_decorations yes\nshell_integration disabled\n'
                  f'linux_display_server {args.backend}\n')
        if width_offset or height_offset:
            config += (f'modify_font cell_width {width_offset}px\n'
                       f'modify_font cell_height {height_offset}px\n')
        command = [str(binary), "--config", "NONE"]
        for line in config.splitlines():
            key, value = line.split(" ", 1)
            command += ["--override", f"{key}={value}"]
        command += child
    else:
        filename = "ghostty.config"
        # Ghostty has a byte budget, not a line limit. For the primary-screen
        # profile retain a disclosed budget rather than claiming equivalence.
        history = 0 if args.screen == "alternate" else args.ghostty_scrollback_bytes
        config = (f'font-family = {FONT}\nfont-size = {point_size}\nscrollback-limit = {history}\n'
                  f'window-width = {args.columns}\nwindow-height = {args.rows}\n'
                  'window-padding-x = 0\nwindow-padding-y = 0\nwindow-decoration = none\n'
                  'shell-integration = none\ngtk-single-instance = false\n')
        if width_offset or height_offset:
            config += f'adjust-cell-width = {width_offset}\nadjust-cell-height = {height_offset}\n'
        command = [str(binary), "--config-default-files=false",
                   "--config-file=" + str(directory / filename), "-e", *child]
    (directory / filename).write_text(config)
    configuration = {"filename": filename, "text": config,
                     "sha256": hashlib.sha256(config.encode()).hexdigest(),
                     "history_limit": history, "history_unit": "bytes" if name == "ghostty" else "lines"}
    if name == "kokuban" and launch_shell is not None:
        configuration["launch_shell"] = str(launch_shell)
    return command, configuration


def execute_sample(name: str, binary: Path, version: str, directory: Path, payload_paths: dict, args) -> dict:
    directory.mkdir()
    command, configuration = terminal_command(name, binary, version, directory, args)
    environment = os.environ.copy()
    for key in ("KOKUBAN_SHELL", "KOKUBAN_EXIT_AFTER_FIRST_FRAME", "WAYLAND_DEBUG"):
        environment.pop(key, None)
    if configuration.get("launch_shell"):
        environment["KOKUBAN_SHELL"] = configuration["launch_shell"]
    # Isolate user settings/cache, but keep fonts, compositor and runtime sockets.
    environment.update(XDG_CONFIG_HOME=str(directory / "config"),
                       XDG_CACHE_HOME=str(directory / "cache"), LC_ALL="C.UTF-8")
    if args.backend == "x11":
        for key in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET"):
            environment.pop(key, None)
        environment.update(GDK_BACKEND="x11", WINIT_UNIX_BACKEND="x11", WINIT_X11_SCALE_FACTOR="1")
    else:
        environment.pop("DISPLAY", None)
        environment.update(GDK_BACKEND="wayland", WINIT_UNIX_BACKEND="wayland")
    sample = {"command": command, "config": configuration, "thread_cpu_enabled": getattr(args, "thread_cpu", False)}
    process = None
    try:
        with (directory / "terminal.log").open("wb") as log:
            process = subprocess.Popen(command, cwd=directory, env=environment,
                                       stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        record(directory, "case.json", {"payloads": payload_paths, "screen": args.screen,
                                        "terminal_pid": process.pid, "settle_seconds": args.settle_seconds,
                                        "thread_cpu": getattr(args, "thread_cpu", False),
                                        "idle_seconds": getattr(args, "idle_seconds", 0)})

        def observe():
            error = directory / "child-error.json"
            if error.exists():
                raise RuntimeError(error.read_text())
            result = directory / "result.json"
            if result.exists():
                return json.loads(result.read_text())
            if process.poll() is not None:
                raise RuntimeError(f"terminal exited before measurements: {process.returncode}")
            return None
        sample["measurements"] = wait_for(observe, f"{name} measurements", args.timeout)
        (directory / "finish").touch()
        if process.wait(timeout=10) != 0:
            raise RuntimeError(f"terminal exit status: {process.returncode}")
        sample["status"] = "passed"
    except Exception as error:
        sample.update(status="failed", error=str(error))
        try:
            sample["log_tail"] = (directory / "terminal.log").read_text(errors="replace")[-4000:]
        except OSError as log_error:
            sample["log_error"] = str(log_error)
    finally:
        if process is not None:
            try:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=3)
            finally:
                stop_owned_child(directory)
    record(directory, "sample.json", sample)
    return sample


def cell_dimensions(geometry, args):
    """Require actual integer pixel cells, not just a requested rows/cols count."""
    if not getattr(args, "pixel_geometry", True):
        # The macOS app sizes its own window and reports no pixels; callers still
        # require one observed rows/columns geometry across every sample.
        if (not isinstance(geometry, (list, tuple)) or len(geometry) != 4
                or any(type(value) is not int or value <= 0 for value in geometry[:2])):
            raise ValueError("missing or invalid effective PTY rows/columns")
        return None
    if (not isinstance(geometry, (list, tuple)) or len(geometry) != 4
            or any(type(value) is not int or value <= 0 for value in geometry)):
        raise ValueError("missing or invalid effective PTY cell/pixel dimensions")
    rows, columns, width, height = geometry
    if (rows, columns) != (args.rows, args.columns):
        raise ValueError("effective PTY rows/columns differ from the requested geometry")
    if width % columns or height % rows:
        raise ValueError("effective PTY pixels do not describe integer cell dimensions")
    return width // columns, height // rows


def calibrated_sample_size(sample: dict, probe: dict, args) -> tuple[int, int]:
    if sample.get("status") != "passed":
        raise ValueError(f"calibration launch failed: {sample.get('error', sample.get('status'))}")
    measurements = sample["measurements"]
    geometry = measurements["initial_geometry"]
    size = cell_dimensions(geometry, args)
    workload = measurements["workloads"]["geometry_probe"]
    if any(workload[key] != geometry for key in ("geometry_before", "geometry_after")):
        raise ValueError("effective PTY geometry changed during calibration")
    if workload["sha256"] != probe["sha256"] or workload["bytes"] != probe["bytes"]:
        raise ValueError("calibration payload differs from the recorded probe")
    return size


def calibrate_cell_size(report: dict, output: Path, args) -> None:
    """Two untimed preflights: observe natural cells, then verify spacing deltas."""
    calibration = {"status": "running", "included_in_timed_statistics": False,
                   "rendering_equivalence_verified": False,
                   "preflight": {"baseline": {}, "adjusted": {}},
                   "option_sources": {
                       "ghostty": "https://github.com/ghostty-org/ghostty/blob/v1.0.0/src/config/Config.zig#L219-L244",
                       "alacritty": "https://github.com/alacritty/alacritty/blob/v0.10.0/alacritty/src/display/mod.rs#L776-L782",
                       "kitty": "https://github.com/kovidgoyal/kitty/blob/v0.26.0/kitty/fonts.c#L303-L337"}}
    report["cell_size_calibration"] = calibration
    try:
        # Conservative supported baselines, not claims of introduction versions.
        minimum_versions = {"ghostty": (1, 0, 0), "alacritty": (0, 10, 0), "kitty": (0, 26, 0)}
        calibration["minimum_supported_versions"] = minimum_versions
        for name, terminal in report["terminals"].items():
            if "path" not in terminal:
                raise ValueError(f"{name}: calibration requires an available executable and version")
            if name in minimum_versions:
                match = re.search(rf"(?im)^{name}\s+(\d+)\.(\d+)\.(\d+)", terminal["version"])
                if not match or tuple(map(int, match.groups())) < minimum_versions[name]:
                    raise ValueError(f"{name}: unsupported or unknown version for cell spacing calibration")
        probe_payload = payloads(1024)["ascii"]
        probe_path = output / "geometry-probe.bin"
        probe_path.write_bytes(probe_payload)
        probe = {"path": str(probe_path), "bytes": len(probe_payload),
                 "sha256": hashlib.sha256(probe_payload).hexdigest()}
        calibration["payload"] = probe
        args.cell_adjustments = {}
        natural_sizes = {}
        for phase in ("baseline", "adjusted"):
            for name, terminal in report["terminals"].items():
                report["in_progress"] = {"calibration": phase, "terminal": name}
                record(output, "report.json", report)
                print(f"cell calibration {phase}: {name}", flush=True)
                sample = execute_sample(name, Path(terminal["path"]), terminal["version"],
                                        output / f"preflight-{phase}-{name}",
                                        {"geometry_probe": str(probe_path)}, args)
                calibration["preflight"][phase][name] = sample
                record(output, "report.json", report)
                size = calibrated_sample_size(sample, probe, args)
                if phase == "baseline":
                    natural_sizes[name] = size
                elif list(size) != calibration["target_cell_pixels"]:
                    raise ValueError(f"{name}: spacing adjustments did not reach the target cell dimensions")
            if phase == "baseline":
                target = [max(size[axis] for size in natural_sizes.values()) for axis in (0, 1)]
                calibration["target_cell_pixels"] = target
                args.cell_adjustments = {name: [target[axis] - size[axis] for axis in (0, 1)]
                                         for name, size in natural_sizes.items()}
                calibration["spacing_adjustments_pixels"] = args.cell_adjustments
                if any(args.cell_adjustments.get("kokuban", (0, 0))):
                    raise ValueError("target cell dimensions require unsupported Kokuban spacing adjustments")
                if any(value > 127 for value in args.cell_adjustments.get("alacritty", ())):
                    raise ValueError("Alacritty spacing adjustments exceed its signed 8-bit limits")
                if "kitty" in natural_sizes and not (2 <= target[0] <= 1000 and 4 <= target[1] <= 1000):
                    raise ValueError("target cell dimensions exceed Kitty's supported metric limits")
        calibration["status"] = "passed"
        report["in_progress"] = None
    except BaseException as error:
        calibration.update(status="interrupted" if isinstance(error, KeyboardInterrupt) else "failed",
                           error=f"{type(error).__name__}: {error}")
        raise
    finally:
        record(output, "report.json", report)


def summarize(report: dict, args) -> None:
    reasons = []
    geometries = set()
    calibration = report.get("cell_size_calibration")
    if calibration and calibration["status"] != "passed":
        reasons.append("cell size calibration did not pass")
    for name, terminal in report["terminals"].items():
        samples = terminal.get("samples", [])
        passed = [sample["measurements"] for sample in samples if sample["status"] == "passed"]
        if len(passed) != args.samples:
            reasons.append(f"{name}: incomplete samples ({len(passed)}/{args.samples})")
        if not passed:
            continue
        terminal["summary"] = {}
        for workload in report["payloads"]:
            measured = [sample["workloads"][workload] for sample in passed]
            for item in measured:
                try:
                    size = cell_dimensions(item["geometry_before"], args)
                    geometries.add(tuple(item["geometry_before"]))
                    if calibration and list(size) != calibration.get("target_cell_pixels"):
                        reasons.append(f"{name}: measured cells differ from the calibrated target")
                except ValueError as error:
                    reasons.append(f"{name}: {error}")
                if item["geometry_after"] != item["geometry_before"]:
                    reasons.append(f"{name}: requested geometry not stable in {workload}")
                if item["sha256"] != report["payloads"][workload]["sha256"]:
                    reasons.append(f"{name}: payload mismatch in {workload}")
            terminal["summary"][workload] = {
                metric: distribution([item[metric] for item in measured])
                for metric in ("write_and_dsr_seconds", "mib_per_second", "terminal_cpu_seconds")}
        terminal["protocol_rtt_seconds"] = distribution([
            value for sample in passed for value in sample["protocol_rtt_seconds"]])
    if len(geometries) != 1:
        reasons.append("actual PTY cell/pixel geometries differ or are missing")
    if args.screen == "primary" and "ghostty" in report["terminals"]:
        reasons.append("Ghostty byte-limited scrollback is not equivalent to the configured line limits")
    if len(report["terminals"]) < 2:
        reasons.append("fewer than two terminals requested")
    report["comparability"] = {"geometry_and_history_checks_passed": not reasons,
                               "reasons": sorted(set(reasons)), "geometries_rows_cols_pixels": sorted(geometries),
                               "rendering_equivalence_verified": False,
                               "rendering_limitations": [
                                   "DSR validates the final marker reply, not workload text or rendered pixels",
                                   "Unicode grapheme and font fallback fidelity must be checked separately from this throughput workload",
                                   "Installed fallback fonts can differ between terminals and affect rendered output",
                               ],
                               "ranking": None,
                               "scope": "Processing throughput and protocol RTT only; no overall performance ranking"}


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--child-dir", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--terminals", nargs="+", choices=TERMINALS, default=list(TERMINALS))
    for name in TERMINALS:
        parser.add_argument("--" + name, default=name, help=f"path to {name} executable")
    parser.add_argument("--backend", choices=("x11", "wayland"), default="wayland")
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--bytes", type=int, default=2 * 1024 * 1024)
    parser.add_argument("--columns", type=int, default=80)
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--font-pixels", type=float, default=14.0)
    parser.add_argument("--match-cell-size", action="store_true",
                        help="calibrate equal integer pixel cells using untimed spacing preflights")
    parser.add_argument("--screen", choices=("alternate", "primary"), default="alternate")
    parser.add_argument("--scrollback-lines", type=int, default=10000)
    parser.add_argument("--ghostty-scrollback-bytes", type=int, default=64 * 1024 * 1024)
    parser.add_argument("--settle-seconds", type=float, default=1.0)
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--environment-note", default="", help="GPU/compositor/display details for this run")
    parser.add_argument("--thread-cpu", action="store_true", help="observe per-thread proc CPU counters outside workload clocks")
    args = parser.parse_args(argv)
    if args.child_dir:
        controlled_child(args.child_dir, args.thread_cpu)
        return 0
    if not sys.platform.startswith("linux"):
        parser.error("the benchmark runner requires Linux")
    if args.output_dir is None or args.samples < 3 or args.bytes < 1024:
        parser.error("--output-dir, at least 3 samples, and at least 1024 bytes are required")
    if (min(args.columns, args.rows, args.font_pixels, args.timeout) <= 0 or args.settle_seconds < 0
            or not all(math.isfinite(value) for value in (args.font_pixels, args.timeout, args.settle_seconds))):
        parser.error("dimensions, font size and timeout must be positive; settle time cannot be negative")
    if args.scrollback_lines < 0 or args.ghostty_scrollback_bytes < 0:
        parser.error("history limits cannot be negative")
    if args.backend == "x11" and not os.environ.get("DISPLAY"):
        parser.error("X11 requires DISPLAY (use xvfb-run or an existing session)")
    if args.backend == "wayland" and not (os.environ.get("WAYLAND_DISPLAY") or os.environ.get("WAYLAND_SOCKET")):
        parser.error("Wayland requires an existing native Wayland session")
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    data = payloads(args.bytes)
    paths = {}
    for name, payload in data.items():
        path = output / (name + ".bin")
        path.write_bytes(payload)
        paths[name] = str(path)
    report = {"schema": 1, "created_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "platform": platform.platform(), "machine": platform.machine(), "cpu_count": os.cpu_count(),
              "cpu_affinity": sorted(os.sched_getaffinity(0)),
              "environment_note": args.environment_note,
              "thread_cpu_enabled": args.thread_cpu,
              "hardware": command_observation(["lscpu"]),
              "font_match": command_observation(["fc-match", "-f", "%{family}\n%{file}\n", FONT]),
              "python": sys.version, "backend": args.backend, "screen": args.screen,
              "requested_geometry": [args.rows, args.columns], "font_pixels": args.font_pixels,
              "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "environment": {key: os.environ.get(key) for key in (
                  "DISPLAY", "WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE",
                  "LIBGL_ALWAYS_SOFTWARE", "MESA_LOADER_DRIVER_OVERRIDE", "GDK_SCALE",
                  "GDK_DPI_SCALE", "WINIT_X11_SCALE_FACTOR")},
              "payloads": {name: {"bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}
                           for name, payload in data.items()}, "terminals": {}, "execution_order": []}
    report["status"] = "running"
    report["match_cell_size"] = args.match_cell_size
    record(output, "report.json", report)
    result = 1
    try:
        for name in dict.fromkeys(args.terminals):
            path = shutil.which(getattr(args, name))
            if path is None:
                report["terminals"][name] = {"status": "missing", "samples": []}
                continue
            binary = Path(path).resolve()
            version = command_observation([str(binary), "--version"])
            if version.get("status") != 0:
                report["terminals"][name] = {"status": "version_failed", "version_observation": version, "samples": []}
                continue
            report["terminals"][name] = {"path": str(binary), "version": version["output"],
                                         "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "samples": []}
            record(output, "report.json", report)
        if args.match_cell_size:
            calibrate_cell_size(report, output, args)
        available = [name for name, terminal in report["terminals"].items() if "path" in terminal]
        for index in range(args.samples):
            # Rotate order between samples to distribute warm machine/cache effects.
            order = available[index % len(available):] + available[:index % len(available)] if available else []
            for name in order:
                terminal = report["terminals"][name]
                report["execution_order"].append([index, name])
                report["in_progress"] = {"sample": index + 1, "terminal": name}
                record(output, "report.json", report)
                print(f"sample {index + 1}/{args.samples}: {name}", flush=True)
                sample = execute_sample(name, Path(terminal["path"]), terminal["version"],
                                        output / f"{index + 1:02d}-{name}", paths, args)
                terminal["samples"].append(sample)
                report["in_progress"] = None
                record(output, "report.json", report)
        result = 0
    except KeyboardInterrupt:
        report["error"] = "KeyboardInterrupt"
        result = 130
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        summarize(report, args)
        if not report["comparability"]["geometry_and_history_checks_passed"] and result == 0:
            result = 1
        report["status"] = "interrupted" if result == 130 else "passed" if result == 0 else "failed"
        record(output, "report.json", report)
    print(json.dumps({"report": str(output / "report.json"), "comparability": report["comparability"]}, indent=2))
    return result


if __name__ == "__main__":
    raise SystemExit(main())
