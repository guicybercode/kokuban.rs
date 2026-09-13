#!/usr/bin/env python3
"""Measure synthetic X11 input to verified window pixels, not input-to-photon.

xdotool injects an XTest key into a focused, controlled terminal. A raw PTY app
repaints a large opaque rectangle; xwd polls until every pixel in it is correct.
Process launch, X11 readback and observer scheduling are included in this upper
bound. On Xvfb this measures software-display behavior, never physical scanout.
"""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import runpy
import select
import shutil
import statistics
import struct
import subprocess
import sys
import termios
import time
import tty


COMPARISON_PATH = Path(__file__).with_name("compare-terminal-performance.py").resolve()
HELPERS = runpy.run_path(str(COMPARISON_PATH))
record = HELPERS["record"]
wait_for = HELPERS["wait_for"]
terminal_size = HELPERS["terminal_size"]
write_all = HELPERS["write_all"]
process_stats = HELPERS["process_stats"]
stop_owned_child = HELPERS["stop_owned_child"]
command_observation = HELPERS["command_observation"]
TERMINALS = HELPERS["TERMINALS"]
COLORS = ((29, 173, 83), (211, 47, 149))


def distribution(values):
    """Nearest-rank percentiles; retain observations and disclose small tails."""
    if not values or any(not math.isfinite(value) or value < 0 for value in values):
        raise ValueError("timings must be a nonempty sequence of finite nonnegative values")
    ordered = sorted(values)
    return {"count": len(values), "median": statistics.median(values),
            "p95": ordered[math.ceil(len(values) * .95) - 1],
            "p99": ordered[math.ceil(len(values) * .99) - 1],
            "min": ordered[0], "max": ordered[-1], "samples": values,
            "percentile_method": "nearest rank", "p99_has_at_least_100_samples": len(values) >= 100}


def rectangle_payload(rows, columns, color):
    # One untouched cell border excludes the window edge and never scrolls.
    background = ";".join(map(str, color))
    return (f"\x1b[48;2;{background}m" + "".join(
        f"\x1b[{row};2H" + " " * (columns - 2) for row in range(2, rows))
        + "\x1b[0m").encode("ascii")


def controlled_child(directory):
    record(directory, "child.json", process_stats(os.getpid()))
    original = termios.tcgetattr(0)
    try:
        tty.setraw(0)
        case = json.loads((directory / "case.json").read_text())
        geometry = case["geometry"]
        frames = [rectangle_payload(*geometry[:2], color) for color in COLORS]
        wait_for(lambda: (directory / "window-ready").exists(), "focused window", case["timeout"])
        wait_for(lambda: list(terminal_size()) == geometry, "settled PTY geometry", case["timeout"])
        write_all(b"\x1b[?1049h\x1b[?25l\x1b[0;40m\x1b[2J" + frames[0])
        record(directory, "ready.json", {"geometry": list(terminal_size()), "color": COLORS[0]})
        index = 0
        deadline = time.monotonic() + case["timeout"]
        while not (directory / "finish").exists():
            if time.monotonic() >= deadline:
                raise TimeoutError("no input or shutdown before child deadline")
            if not select.select([0], [], [], .02)[0]:
                continue
            incoming = os.read(0, 1024)
            received = time.perf_counter_ns()
            expected = b"b" if index % 2 == 0 else b"a"
            if incoming != expected:
                raise AssertionError(f"input {index}: expected {expected!r}, received {incoming!r}")
            before = list(terminal_size())
            if before != geometry:
                raise AssertionError(f"PTY geometry changed before input {index}: {before}")
            state = (index + 1) % 2
            write_all(frames[state])
            written = time.perf_counter_ns()
            after = list(terminal_size())
            record(directory, f"event-{index:04d}.json", {
                "index": index, "input_hex": incoming.hex(), "color": COLORS[state],
                "received_ns": received, "written_ns": written,
                "geometry_before": before, "geometry_after": after})
            if after != geometry:
                raise AssertionError("PTY geometry changed during output")
            index += 1
            deadline = time.monotonic() + case["timeout"]
        record(directory, "child-exit.json", {"events": index, "status": "passed"})
    except BaseException as error:
        record(directory, "child-error.json", {"error": f"{type(error).__name__}: {error}"})
        raise
    finally:
        write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l")
        termios.tcsetattr(0, termios.TCSANOW, original)


class Frame:
    """Strict TrueColor XWD reader; compare RGB and ignore unused alpha/padding."""
    def __init__(self, data):
        if len(data) < 100:
            raise ValueError("truncated XWD header")
        header = struct.unpack_from(">25I", data)
        size, version, format_, _, self.width, self.height, offset, order = header[:8]
        bits, self.stride, visual = header[11:14]
        masks = header[14:17]
        self.pixel_bytes = bits // 8
        self.offset = size + header[19] * 12
        if (size < 100 or version != 7 or format_ != 2 or offset != 0 or visual != 4
                or bits not in (24, 32) or order not in (0, 1)
                or not self.width or not self.height or len(set(masks)) != 3
                or any(mask not in (0xff, 0xff00, 0xff0000, 0xff000000) for mask in masks)):
            raise ValueError("expected a packed 24/32-bit TrueColor ZPixmap XWD")
        if self.stride < self.width * self.pixel_bytes or len(data) < self.offset + self.stride * self.height:
            raise ValueError("truncated XWD pixels")
        shifts = [(mask.bit_length() - 8) // 8 for mask in masks]
        if any(shift >= self.pixel_bytes for shift in shifts):
            raise ValueError("XWD channel mask exceeds pixel storage")
        self.channels = [shift if order == 0 else self.pixel_bytes - 1 - shift for shift in shifts]
        self.data = data

    def matches(self, region, color):
        left, top, right, bottom = region
        if not (0 <= left < right <= self.width and 0 <= top < bottom <= self.height):
            raise ValueError("pixel verification region is outside the window")
        for y in range(top, bottom):
            row = self.offset + y * self.stride
            for channel, component in zip(self.channels, color):
                start = row + left * self.pixel_bytes + channel
                stop = row + right * self.pixel_bytes + channel
                if self.data[start:stop:self.pixel_bytes] != bytes([component]) * (right - left):
                    return False
        return True


def run(command, timeout=10):
    return subprocess.run(command, capture_output=True, check=True, timeout=timeout).stdout


def capture(window, timeout):
    started = time.perf_counter_ns()
    data = run(["xwd", "-silent", "-id", str(window)], timeout)
    finished = time.perf_counter_ns()
    return Frame(data), started, finished


def matching_window(candidates, geometry):
    """Ignore auxiliary X11 windows, but reject ambiguous calibrated windows."""
    matches = [item["window"] for item in candidates if item.get("pixels") == geometry[2:]]
    return matches[0] if len(matches) == 1 else None


def cell_remainder_window(candidates, geometry, cell):
    """Find one startup window with spare pixels that cannot fit another cell."""
    if any(item.get("pixels") == geometry[2:] for item in candidates):
        return None
    matches = [item for item in candidates if "pixels" in item and all(
        wanted <= actual < wanted + size for actual, wanted, size in zip(item["pixels"], geometry[2:], cell))]
    return matches[0] if len(matches) == 1 else None


def verify_event(event, index, geometry, started, finished):
    expected_key = b"b" if index % 2 == 0 else b"a"
    if (event["index"] != index or event["input_hex"] != expected_key.hex()
            or event["color"] != list(COLORS[(index + 1) % 2])
            or event["geometry_before"] != geometry or event["geometry_after"] != geometry
            or not started <= event["received_ns"] <= finished
            or event["written_ns"] < event["received_ns"]):
        raise AssertionError("input acknowledgment, geometry or monotonic event order mismatch")


def execute_sample(name, binary, version, directory, args, cell):
    directory.mkdir()
    command, config = HELPERS["terminal_command"](name, binary, version, directory, args)
    if command.count(str(COMPARISON_PATH)) != 1:
        raise ValueError("shared terminal command no longer has one controlled child path")
    command[command.index(str(COMPARISON_PATH))] = str(Path(__file__).resolve())
    geometry = [args.rows, args.columns, args.columns * cell[0], args.rows * cell[1]]
    region = [cell[0], cell[1], geometry[2] - cell[0], geometry[3] - cell[1]]
    environment = os.environ.copy()
    for key in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "WAYLAND_DEBUG", "KOKUBAN_SHELL",
                "KOKUBAN_EXIT_AFTER_FIRST_FRAME"):
        environment.pop(key, None)
    environment.update(XDG_CONFIG_HOME=str(directory / "config"), XDG_CACHE_HOME=str(directory / "cache"),
                       GDK_BACKEND="x11", WINIT_UNIX_BACKEND="x11", WINIT_X11_SCALE_FACTOR="1", LC_ALL="C.UTF-8")
    sample = {"command": command, "config": config, "geometry": geometry, "region": region,
              "verified_pixels_per_frame": (region[2] - region[0]) * (region[3] - region[1]),
              "warmup": [], "measurements": []}
    process = None
    try:
        record(directory, "case.json", {"geometry": geometry, "timeout": args.timeout})
        with (directory / "terminal.log").open("wb") as log:
            process = subprocess.Popen(command, cwd=directory, env=environment,
                                       stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        sample["terminal_pid"] = process.pid
        resized_windows = set()

        def check():
            failure = directory / "child-error.json"
            if failure.exists():
                raise RuntimeError(failure.read_text())
            if process.poll() is not None:
                raise RuntimeError(f"terminal exited before completion: {process.returncode}")

        def find_window():
            check()
            result = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(process.pid)],
                                    capture_output=True, timeout=args.timeout)
            windows = result.stdout.decode().splitlines()
            candidates = []
            sample["window_search"] = {"status": result.returncode, "candidates": candidates,
                                       "stderr": result.stderr.decode(errors="replace")}
            if result.returncode == 0:
                for candidate in windows:
                    try:
                        frame, _, _ = capture(candidate, args.timeout)
                        candidates.append({"window": candidate, "pixels": [frame.width, frame.height]})
                    except subprocess.CalledProcessError as error:
                        # Auxiliary windows may disappear while startup is in progress.
                        candidates.append({"window": candidate, "capture_error": str(error)})
            matched = matching_window(candidates, geometry)
            if matched is not None:
                return matched
            remainder = cell_remainder_window(candidates, geometry, cell)
            if remainder is not None and remainder["window"] not in resized_windows:
                # Kitty 0.45.0 opens 721x409 for an 80x24 grid of 9x17 cells.
                # Remove unused edge pixels before warmup, then require an
                # exact new XWD and PTY size instead of weakening geometry checks.
                resized_windows.add(remainder["window"])
                sample.setdefault("window_size_adjustments", []).append({
                    "window": remainder["window"], "before_pixels": remainder["pixels"],
                    "requested_pixels": geometry[2:]})
                run(["xdotool", "windowsize", "--sync", remainder["window"],
                     str(geometry[2]), str(geometry[3])], args.timeout)
            return None

        window = wait_for(find_window, "one visible window with calibrated pixels", args.timeout)
        sample["window"] = window
        run(["xdotool", "windowfocus", "--sync", window], args.timeout)
        (directory / "window-ready").touch()

        def wait_color(color):
            observations = []
            # Preserve partial observations on timeout or process failure.
            sample["pending_observations"] = observations
            deadline = time.monotonic() + args.timeout
            while time.monotonic() < deadline:
                check()
                frame, started, finished = capture(window, args.timeout)
                if [frame.width, frame.height] != geometry[2:]:
                    raise AssertionError("window pixel dimensions changed during measurement")
                matches = frame.matches(region, color)
                validated = time.perf_counter_ns()
                observations.append({"capture_started_ns": started, "capture_finished_ns": finished,
                                     "validated_ns": validated, "matches": matches})
                if matches:
                    return frame, observations
                if args.poll_interval:
                    time.sleep(args.poll_interval)
            if observations:
                (directory / "unmatched.xwd").write_bytes(frame.data)
            raise TimeoutError(f"expected color {color} not observed; captures={len(observations)}")

        initial, _ = wait_color(COLORS[0])
        (directory / "initial.xwd").write_bytes(initial.data)
        time.sleep(args.settle_seconds)
        for index in range(args.warmup + args.events):
            check()
            focus = run(["xdotool", "getwindowfocus"], args.timeout).decode().strip()
            if focus != window:
                raise AssertionError("benchmark window lost keyboard focus")
            started = time.perf_counter_ns()
            sample["pending_event"] = {"index": index, "input_started_ns": started}
            run(["xdotool", "key", "--clearmodifiers", "--delay", "0", "b" if index % 2 == 0 else "a"],
                args.timeout)
            injected = time.perf_counter_ns()
            frame, observations = wait_color(COLORS[(index + 1) % 2])
            finished = observations[-1]["capture_finished_ns"]
            event_path = directory / f"event-{index:04d}.json"

            def acknowledgment():
                check()
                return json.loads(event_path.read_text()) if event_path.exists() else None

            event = wait_for(acknowledgment, "controlled child input acknowledgment", args.timeout)
            verify_event(event, index, geometry, started, finished)
            item = {"index": index, "input_started_ns": started, "injection_finished_ns": injected,
                    "input_to_observed_frame_upper_bound_seconds": (finished - started) / 1e9,
                    "injection_seconds": (injected - started) / 1e9,
                    "child": event, "observations": observations,
                    "matched_xwd_sha256": hashlib.sha256(frame.data).hexdigest()}
            if index == 0:
                # Retain both colors even when an even event count returns to
                # the initial color. File I/O is after this warmup interval.
                (directory / "first-transition.xwd").write_bytes(frame.data)
            sample["warmup" if index < args.warmup else "measurements"].append(item)
            sample.pop("pending_event", None)
            sample.pop("pending_observations", None)
            record(directory, "sample.json", sample)
        (directory / "final.xwd").write_bytes(frame.data)
        (directory / "finish").touch()
        if process.wait(timeout=args.timeout) != 0:
            raise RuntimeError(f"terminal exit status: {process.returncode}")
        exit_record = json.loads((directory / "child-exit.json").read_text())
        if exit_record != {"events": args.warmup + args.events, "status": "passed"}:
            raise AssertionError("controlled child event count or shutdown mismatch")
        sample["status"] = "passed"
    except Exception as error:
        sample.update(status="failed", error=f"{type(error).__name__}: {error}")
        if (directory / "terminal.log").exists():
            sample["log_tail"] = (directory / "terminal.log").read_text(errors="replace")[-4000:]
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


def summarize(report, args):
    reasons = []
    calibration = report.get("cell_size_calibration", {})
    if calibration.get("status") != "passed":
        reasons.append("cell calibration did not pass")
    for name, terminal in report["terminals"].items():
        passed = [sample for sample in terminal["samples"] if sample.get("status") == "passed"]
        if len(passed) != args.samples:
            reasons.append(f"{name}: incomplete fresh-process samples ({len(passed)}/{args.samples})")
        events = [event for sample in passed for event in sample["measurements"]]
        if len(events) != args.samples * args.events:
            reasons.append(f"{name}: incomplete verified input events")
        if events:
            terminal["summary"] = {key: distribution([event[key] for event in events]) for key in (
                "input_to_observed_frame_upper_bound_seconds", "injection_seconds")}
            terminal["summary"]["observer_capture_seconds"] = distribution([
                (observation["capture_finished_ns"] - observation["capture_started_ns"]) / 1e9
                for event in events for observation in event["observations"]])
            terminal["summary"]["observer_validation_seconds"] = distribution([
                (observation["validated_ns"] - observation["capture_finished_ns"]) / 1e9
                for event in events for observation in event["observations"]])
    if len(report["terminals"]) < 2:
        reasons.append("fewer than two terminals requested")
    report["comparability"] = {"checks_passed": not reasons, "reasons": reasons, "ranking": None,
                               "scope": "Synthetic XTest input to verified opaque X11 window region only"}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--child-dir", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--terminals", nargs="+", choices=TERMINALS, default=list(TERMINALS))
    for name in TERMINALS:
        parser.add_argument("--" + name, default=name)
    parser.add_argument("--samples", type=int, default=3, help="fresh processes per terminal, minimum 3")
    parser.add_argument("--events", type=int, default=40, help="measured key/frame pairs per process")
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--columns", type=int, default=80)
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--font-pixels", type=float, default=14.0)
    parser.add_argument("--settle-seconds", type=float, default=1.0)
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument("--poll-interval", type=float, default=0.001)
    parser.add_argument("--environment-note", default="")
    args = parser.parse_args(argv)
    if args.child_dir:
        controlled_child(args.child_dir)
        return 0
    if not sys.platform.startswith("linux") or not os.environ.get("DISPLAY"):
        parser.error("requires Linux and an X11 DISPLAY; Xvfb is supported but is not a physical display")
    if (args.output_dir is None or args.samples < 3 or args.events < 1 or args.warmup < 1
            or min(args.rows, args.columns) < 4):
        parser.error("provide output directory, >=3 processes, >=1 event/warmup and >=4 rows/columns")
    if (not all(math.isfinite(value) for value in (
            args.font_pixels, args.settle_seconds, args.timeout, args.poll_interval))
            or min(args.font_pixels, args.timeout) <= 0 or min(args.settle_seconds, args.poll_interval) < 0):
        parser.error("font/timeout must be positive; settle/poll intervals must be finite and nonnegative")
    missing = [tool for tool in ("xdotool", "xwd", "fc-match") if shutil.which(tool) is None]
    if missing:
        parser.error("missing tools: " + ", ".join(missing))
    args.backend, args.screen = "x11", "alternate"
    args.scrollback_lines = args.ghostty_scrollback_bytes = 0
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {"schema": 1, "created_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "platform": platform.platform(), "machine": platform.machine(), "python": sys.version,
              "environment_note": args.environment_note,
              "hardware": command_observation(["lscpu"]),
              "display": command_observation(["xdpyinfo"]),
              "renderer": command_observation(["glxinfo", "-B"]),
              "environment": {key: os.environ.get(key) for key in (
                  "DISPLAY", "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP", "LIBGL_ALWAYS_SOFTWARE",
                  "MESA_LOADER_DRIVER_OVERRIDE", "GDK_SCALE", "GDK_DPI_SCALE")},
              "font_match": command_observation(["fc-match", "-f", "%{family}\n%{file}\n", HELPERS["FONT"]]),
              "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "configuration_harness_sha256": hashlib.sha256(COMPARISON_PATH.read_bytes()).hexdigest(),
              "resource_harness_sha256": hashlib.sha256(
                  COMPARISON_PATH.with_name("linux-resource-smoke.py").read_bytes()).hexdigest(),
              "settings": {key: value for key, value in vars(args).items() if key not in ("output_dir", "child_dir")},
              "clock": "time.perf_counter_ns; common host monotonic clock in parent and child",
              "colors_rgb": COLORS, "status": "running", "terminals": {}, "execution_order": [],
              "limitations": [
                  "Not physical keyboard, compositor presentation, vblank, GPU-only or input-to-photon latency",
                  "Xvfb measurements describe the virtual software X11 display and may differ from a desktop GPU",
                  "Input interval starts before xdotool launch; xwd subprocess, readback and scheduling add overhead",
                  "Capture completion is a conservative upper bound; observer cost is reported, never subtracted",
                  "Opaque background pixels are verified; text rendering, fonts and Unicode fidelity are not evaluated",
                  "Sequential events within a process are correlated; raw per-process samples are retained",
                  "Nearest-rank p99 with fewer than 100 events is the maximum and has weak tail resolution"]}
    record(output, "report.json", report)
    status = 1
    try:
        for name in dict.fromkeys(args.terminals):
            path = shutil.which(getattr(args, name))
            terminal = report["terminals"][name] = {"samples": []}
            if path is None:
                terminal["status"] = "missing"
                continue
            binary = Path(path).resolve()
            version = command_observation([str(binary), "--version"])
            if version.get("status") != 0:
                terminal.update(status="version_failed", version_observation=version)
                continue
            terminal.update(path=str(binary), version=version["output"],
                            sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
        HELPERS["calibrate_cell_size"](report, output, args)
        cell = report["cell_size_calibration"]["target_cell_pixels"]
        names = list(report["terminals"])
        for index in range(args.samples):
            order = names[index % len(names):] + names[:index % len(names)]
            for name in order:
                terminal = report["terminals"][name]
                report["execution_order"].append([index, name])
                report["in_progress"] = {"sample": index + 1, "terminal": name}
                record(output, "report.json", report)
                print(f"frame latency process {index + 1}/{args.samples}: {name}", flush=True)
                terminal["samples"].append(execute_sample(
                    name, Path(terminal["path"]), terminal["version"],
                    output / f"{index + 1:02d}-{name}", args, cell))
                record(output, "report.json", report)
        status = 0
    except KeyboardInterrupt:
        report["error"] = "KeyboardInterrupt"
        status = 130
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        summarize(report, args)
        if not report["comparability"]["checks_passed"] and status == 0:
            status = 1
        report.update(status="interrupted" if status == 130 else "passed" if status == 0 else "failed",
                      in_progress=None)
        record(output, "report.json", report)
    print(json.dumps({"report": str(output / "report.json"), "status": report["status"],
                      "comparability": report["comparability"]}, indent=2))
    return status


if __name__ == "__main__":
    raise SystemExit(main())
