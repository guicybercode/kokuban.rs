#!/usr/bin/env python3
"""Compare raw-PTY throughput and protocol RTT on one Linux display session.

DSR acknowledges terminal processing, not frame presentation or input-to-photon
latency. Configurations, payloads, logs, individual samples and limitations are
retained alongside the report. No packages are installed and no terminals ranked.
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
process_stats = HELPERS["process_stats"]
wait_for = HELPERS["wait_for"]
stop_owned_child = HELPERS["stop_owned_child"]
TERMINALS = ("kokuban", "ghostty", "alacritty", "kitty")
FONT = "DejaVu Sans Mono"


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


def controlled_child(directory: Path) -> None:
    record(directory, "child.json", process_stats(os.getpid()))
    original = termios.tcgetattr(0)
    try:
        tty.setraw(0)
        case = wait_for(lambda: json.loads((directory / "case.json").read_text())
                        if (directory / "case.json").exists() else None, "benchmark case")
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
                        "workloads": {}, "term": os.environ.get("TERM")}
        for name, payload in data.items():
            # Warm the parser/font paths outside the timed sample, then clear.
            write_all(payload)
            barrier(b"warm")
            write_all(b"\x1b[3J\x1b[2J\x1b[H")
            barrier(b"start")
            geometry_before = list(terminal_size())
            cpu_before = process_stats(case["terminal_pid"])
            started = time.perf_counter()
            write_all(payload)
            written = time.perf_counter()
            reply = barrier(b"done")
            finished = time.perf_counter()
            cpu_after = process_stats(case["terminal_pid"])
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
    child = [sys.executable, str(Path(__file__).resolve()), "--child-dir", str(directory)]
    # Kokuban's size is logical pixels, the other terminals use points. Convert
    # at 96 dpi; actual cells/pixels are still checked because DPI varies.
    point_size = args.font_pixels * 72 / 96
    if name == "kokuban":
        config = (f'[font]\nfamily = "{FONT}"\nsize = {args.font_pixels}\n'
                  f'[window]\ncolumns = {args.columns}\nrows = {args.rows}\nscrollback_lines = {history}\n'
                  '[images]\nenabled = false\n')
        filename = "kokuban.toml"
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
        else:
            filename = "alacritty.toml"
            config = (f'[font]\nsize = {point_size}\n[font.normal]\nfamily = "{FONT}"\n'
                      '[window]\ndecorations = "None"\n[window.padding]\nx = 0\ny = 0\n'
                      f'[window.dimensions]\ncolumns = {args.columns}\nlines = {args.rows}\n'
                      f'[scrolling]\nhistory = {history}\n')
        command = [str(binary), "--config-file", str(directory / filename), "-e", *child]
    elif name == "kitty":
        filename = "kitty.conf"
        config = (f'font_family {FONT}\nfont_size {point_size}\nscrollback_lines {history}\n'
                  'remember_window_size no\nwindow_padding_width 0\nwindow_margin_width 0\n'
                  f'initial_window_width {args.columns}c\ninitial_window_height {args.rows}c\n'
                  'hide_window_decorations yes\nshell_integration disabled\n'
                  f'linux_display_server {args.backend}\n')
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
        command = [str(binary), "--config-default-files=false",
                   "--config-file=" + str(directory / filename), "-e", *child]
    (directory / filename).write_text(config)
    return command, {"filename": filename, "text": config,
                     "sha256": hashlib.sha256(config.encode()).hexdigest(),
                     "history_limit": history, "history_unit": "bytes" if name == "ghostty" else "lines"}


def execute_sample(name: str, binary: Path, version: str, directory: Path, payload_paths: dict, args) -> dict:
    directory.mkdir()
    command, configuration = terminal_command(name, binary, version, directory, args)
    environment = os.environ.copy()
    for key in ("KOKUBAN_SHELL", "KOKUBAN_EXIT_AFTER_FIRST_FRAME", "WAYLAND_DEBUG"):
        environment.pop(key, None)
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
    sample = {"command": command, "config": configuration}
    process = None
    try:
        with (directory / "terminal.log").open("wb") as log:
            process = subprocess.Popen(command, cwd=directory, env=environment,
                                       stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        record(directory, "case.json", {"payloads": payload_paths, "screen": args.screen,
                                        "terminal_pid": process.pid, "settle_seconds": args.settle_seconds})

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


def summarize(report: dict, args) -> None:
    reasons = []
    geometries = set()
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
                geometry = tuple(item["geometry_before"])
                geometries.add(geometry)
                if any(value <= 0 for value in geometry):
                    reasons.append(f"{name}: missing effective PTY cell/pixel dimensions")
                if item["geometry_after"] != list(geometry) or geometry[:2] != (args.rows, args.columns):
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


def main() -> int:
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
    parser.add_argument("--screen", choices=("alternate", "primary"), default="alternate")
    parser.add_argument("--scrollback-lines", type=int, default=10000)
    parser.add_argument("--ghostty-scrollback-bytes", type=int, default=64 * 1024 * 1024)
    parser.add_argument("--settle-seconds", type=float, default=1.0)
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--environment-note", default="", help="GPU/compositor/display details for this run")
    args = parser.parse_args()
    if args.child_dir:
        controlled_child(args.child_dir)
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
    available = [name for name, terminal in report["terminals"].items() if "path" in terminal]
    for index in range(args.samples):
        # Rotate order between samples to distribute warm machine/cache effects.
        order = available[index % len(available):] + available[:index % len(available)] if available else []
        for name in order:
            terminal = report["terminals"][name]
            report["execution_order"].append([index, name])
            print(f"sample {index + 1}/{args.samples}: {name}", flush=True)
            sample = execute_sample(name, Path(terminal["path"]), terminal["version"],
                                    output / f"{index + 1:02d}-{name}", paths, args)
            terminal["samples"].append(sample)
            record(output, "report.json", report)
    summarize(report, args)
    record(output, "report.json", report)
    print(json.dumps({"report": str(output / "report.json"), "comparability": report["comparability"]}, indent=2))
    return int(any(len(terminal.get("samples", [])) != args.samples or
                   any(sample["status"] != "passed" for sample in terminal["samples"])
                   for terminal in report["terminals"].values()))


if __name__ == "__main__":
    raise SystemExit(main())
