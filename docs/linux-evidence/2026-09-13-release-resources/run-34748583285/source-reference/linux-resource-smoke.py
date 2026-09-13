#!/usr/bin/env python3
"""Measure one real Linux terminal process under Xvfb; no performance thresholds.

Idle and post-output phases each sample five seconds without PTY output. A
controlled raw-PTY app writes a finite ASCII workload and validates a DSR cursor
reply after its final marker. Throughput includes the terminal round trip, not
just a successful write or process exit; it does not measure presentation FPS.
CPU/RSS definitions: https://www.kernel.org/doc/html/latest/filesystems/proc.html
"""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
from typing import Any, Callable, Optional
import tty


LINES, LINE_BYTES = 32768, 80
SAMPLE_SECONDS, IDLE_SECONDS = 0.25, 5.0
PROMPT, FINAL_MARKER = b"kokuban resource idle> ", b"RESOURCE_OUTPUT_DONE_32768"
CONFIG = (
    '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
    '[window]\ncolumns = 80\nrows = 24\nscrollback_lines = 10000\n'
    '[images]\nenabled = true\n[images.kitty]\nallow_file_transfer = false\n'
)


def record(directory: Path, name: str, value: Any) -> None:
    pending = directory / (name + ".pending")
    pending.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    pending.replace(directory / name)


def read_json(path: Path) -> Any:
    return json.loads(path.read_text()) if path.exists() else None


def run(arguments: list[str], **kwargs: Any) -> subprocess.CompletedProcess:
    return subprocess.run(arguments, timeout=8, capture_output=True, **kwargs)


def parse_stats(pid: int, stat: str, status: str, ticks_per_second: int) -> dict[str, Any]:
    fields = stat.rsplit(")", 1)[1].split()
    values = {line.split(":", 1)[0]: int(line.split()[1])
              for line in status.splitlines()
              if line.startswith(("VmRSS:", "VmHWM:", "Threads:"))}
    return {"pid": pid, "start_ticks": int(fields[19]),
            "cpu_ticks": int(fields[11]) + int(fields[12]),
            "cpu_seconds": (int(fields[11]) + int(fields[12])) / ticks_per_second,
            "rss_kib": values["VmRSS"], "hwm_kib": values["VmHWM"],
            "threads": values["Threads"], "monotonic_seconds": time.monotonic()}


def process_stats(pid: int) -> dict[str, Any]:
    directory = Path(f"/proc/{pid}")
    return parse_stats(pid, (directory / "stat").read_text(),
                       (directory / "status").read_text(), os.sysconf("SC_CLK_TCK"))


def phase_result(samples: list[dict[str, Any]]) -> dict[str, Any]:
    first, last = samples[0], samples[-1]
    if any((item["pid"], item["start_ticks"]) != (first["pid"], first["start_ticks"])
           for item in samples):
        raise AssertionError("PID identity changed during measurement")
    wall = last["monotonic_seconds"] - first["monotonic_seconds"]
    cpu = last["cpu_seconds"] - first["cpu_seconds"]
    return {"wall_seconds": wall, "cpu_seconds": cpu,
            "cpu_percent_one_core": 100 * cpu / wall,
            "rss_start_kib": first["rss_kib"], "rss_end_kib": last["rss_kib"],
            "rss_observed_max_kib": max(item["rss_kib"] for item in samples),
            "hwm_process_lifetime_kib": max(item["hwm_kib"] for item in samples),
            "samples": samples}


def output_payload() -> bytes:
    filler = (b"abcdefghijklmnopqrstuvwxyz0123456789" * 2)[:71]
    return b"".join(f"{line:06d} ".encode() + filler + b"\r\n" for line in range(LINES))


def write_all(payload: bytes) -> None:
    remaining = memoryview(payload)
    while remaining:
        written = os.write(sys.stdout.fileno(), remaining)
        if written <= 0:
            raise AssertionError("PTY output stopped making progress")
        remaining = remaining[written:]


def terminal_size() -> tuple[int, ...]:
    return struct.unpack(
        "4H", fcntl.ioctl(sys.stdin.fileno(), termios.TIOCGWINSZ, b"\0" * 8))


def barrier(marker: bytes) -> str:
    write_all(b"\x1b[H" + marker + b"\x1b[K\x1b[6n")
    expected = f"\x1b[1;{len(marker) + 1}R".encode()
    reply = bytearray()
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        readable, _, _ = select.select([sys.stdin.fileno()], [], [], 0.1)
        if readable:
            incoming = os.read(sys.stdin.fileno(), 4096)
            if not incoming:
                raise AssertionError("PTY closed before terminal DSR reply")
            reply.extend(incoming)
            if len(reply) > len(expected) or not expected.startswith(reply):
                raise AssertionError(f"incorrect terminal DSR reply: {bytes(reply)!r}")
            if reply == expected:
                return bytes(reply).hex()
    raise AssertionError("terminal did not acknowledge the final marker with DSR")


def wait_for(observe: Callable[[], Any], description: str, timeout: float = 12) -> Any:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = observe()
        if value:
            return value
        time.sleep(0.02)
    raise AssertionError(f"timed out waiting for {description}")


def child(directory: Path) -> None:
    record(directory, "child.json", process_stats(os.getpid()))
    original = termios.tcgetattr(sys.stdin.fileno())
    try:
        tty.setraw(sys.stdin.fileno())
        payload = output_payload()  # Generation is outside the timed output phase.
        wait_for(lambda: (directory / "window-ready").exists(), "visible window")
        write_all(b"\x1b[2J")
        ready = {"reply_hex": barrier(PROMPT), "marker": PROMPT.decode(),
                 "winsize_rows_cols_pixels": terminal_size()}
        record(directory, "ready.json", ready)
        wait_for(lambda: (directory / "output-start").exists(), "output phase")
        # The shell can run before glyph metrics/window initialization finish.
        # Observe again after the parent has allowed the real window to settle.
        settled_size = terminal_size()
        started = time.monotonic()
        write_all(payload)
        written = time.monotonic()
        reply = barrier(FINAL_MARKER)
        acknowledged = time.monotonic()
        record(directory, "output-done.json", {
            "bytes": len(payload), "lines": LINES, "sha256": hashlib.sha256(payload).hexdigest(),
            "write_seconds": written - started, "roundtrip_seconds": acknowledged - written,
            "write_and_terminal_roundtrip_seconds": acknowledged - started,
            "bytes_per_second_including_terminal_roundtrip": len(payload) / (acknowledged - started),
            "marker": FINAL_MARKER.decode(), "reply_hex": reply,
            "winsize_rows_cols_pixels": settled_size,
        })
        wait_for(lambda: (directory / "stop").exists(), "clean shutdown")
        record(directory, "child-exit.json", {"status": 0})
    except BaseException as error:
        record(directory, "child-error.json", {"error": f"{type(error).__name__}: {error}"})
        raise
    finally:
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, original)


def stop_owned_child(directory: Path) -> None:
    child_info = read_json(directory / "child.json")
    if child_info is None:
        return
    for signum in (signal.SIGTERM, signal.SIGKILL):
        try:
            current = process_stats(child_info["pid"])
            if current["start_ticks"] != child_info["start_ticks"]:
                return
            os.kill(child_info["pid"], signum)
        except (OSError, KeyError):
            return
        time.sleep(0.1)


def exercise(binary: Path, directory: Path, report: dict[str, Any]) -> None:
    shell = directory / "shell"
    shell.write_text("#!/bin/sh\nexec " + shlex.join([
        sys.executable, str(Path(__file__).resolve()), "--child-dir", str(directory),
    ]) + "\n")
    shell.chmod(0o700)
    (directory / "kokuban.toml").write_text(CONFIG)
    environment = os.environ.copy()
    for name in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR",
                 "KOKUBAN_EXIT_AFTER_FIRST_FRAME"):
        environment.pop(name, None)
    environment.update(KOKUBAN_SHELL=str(shell), WINIT_X11_SCALE_FACTOR="1", LC_ALL="C.UTF-8")
    with (directory / "terminal.log").open("wb") as log:
        terminal = subprocess.Popen([str(binary)], cwd=directory, env=environment,
                                    stdout=log, stderr=log)
    try:
        def observe(action: Callable[[], Any]) -> Any:
            if terminal.poll() is not None:
                raise AssertionError(f"terminal exited early: {terminal.returncode}")
            failure = read_json(directory / "child-error.json")
            if failure:
                raise AssertionError(f"controlled PTY app failed: {failure}")
            return action()

        def window_search() -> Optional[str]:
            result = run(["xdotool", "search", "--onlyvisible", "--pid", str(terminal.pid)])
            windows = result.stdout.decode().splitlines()
            return windows[0] if result.returncode == 0 and windows else None

        window = wait_for(lambda: observe(window_search), "Kokuban X11 window")
        run(["xdotool", "windowfocus", "--sync", window], check=True)
        (directory / "window-ready").touch()
        report["ready"] = wait_for(lambda: observe(lambda: read_json(directory / "ready.json")),
                                   "initial prompt DSR acknowledgment")
        report["identity"] = process_stats(terminal.pid)

        def sample() -> dict[str, Any]:
            result = observe(lambda: process_stats(terminal.pid))
            if result["start_ticks"] != report["identity"]["start_ticks"]:
                raise AssertionError("terminal PID was reused")
            return result

        def quiet_phase(seconds: float) -> dict[str, Any]:
            samples = [sample()]
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                time.sleep(min(SAMPLE_SECONDS, max(0, deadline - time.monotonic())))
                samples.append(sample())
            return phase_result(samples)

        report["phases"]["startup_settle"] = quiet_phase(1)
        report["phases"]["idle"] = quiet_phase(IDLE_SECONDS)
        geometry = run(["xdotool", "getwindowgeometry", "--shell", window], check=True).stdout.decode()
        dimensions = dict(line.split("=", 1) for line in geometry.splitlines() if "=" in line)
        report["window_pixels"] = [int(dimensions["WIDTH"]), int(dimensions["HEIGHT"])]
        samples = [sample()]
        (directory / "output-start").touch()
        deadline = time.monotonic() + 20
        while not observe(lambda: read_json(directory / "output-done.json")):
            if time.monotonic() >= deadline:
                raise AssertionError("finite output did not reach its terminal barrier")
            time.sleep(0.05)
            samples.append(sample())
        samples.append(sample())
        report["phases"]["output"] = phase_result(samples)
        report["output"] = read_json(directory / "output-done.json")
        expected_reply = f"\x1b[1;{len(FINAL_MARKER) + 1}R".encode().hex()
        if (report["output"]["bytes"] != LINES * LINE_BYTES
                or report["output"]["reply_hex"] != expected_reply):
            raise AssertionError("output length or terminal barrier mismatch")
        if report["output"]["winsize_rows_cols_pixels"] != [24, 80, *report["window_pixels"]]:
            raise AssertionError("settled PTY cells/pixels disagree with the real window")
        report["phases"]["output_settle"] = quiet_phase(1)
        report["phases"]["post_output_idle"] = quiet_phase(IDLE_SECONDS)
        (directory / "stop").touch()
        terminal.wait(timeout=5)
        if terminal.returncode != 0 or read_json(directory / "child-exit.json") != {"status": 0}:
            raise AssertionError(f"unclean resource fixture exit: {terminal.returncode}")
        report["clean_exit"] = {"kokuban": 0, "controlled_pty_app": 0}
    finally:
        stop_owned_child(directory)
        if terminal.poll() is None:
            terminal.terminate()
            try:
                terminal.wait(timeout=2)
            except subprocess.TimeoutExpired:
                terminal.kill()
                terminal.wait(timeout=2)


def metadata(binary: Path) -> dict[str, Any]:
    cpuinfo = Path("/proc/cpuinfo").read_text().splitlines()
    models = sorted({line.split(":", 1)[1].strip() for line in cpuinfo
                     if line.startswith(("model name", "Hardware"))})
    versions = {}
    for name, arguments in (("rustc", ["rustc", "--version"]),
                            ("python", [sys.executable, "--version"]),
                            ("xdotool", ["xdotool", "--version"])):
        result = run(arguments, check=True)
        versions[name] = result.stdout.decode(errors="replace").splitlines()[:1]
    dependencies = run(["ldd", str(binary)], check=True).stdout.decode(errors="replace")
    elf = run(["readelf", "-h", "-d", str(binary)], check=True).stdout.decode(errors="replace")
    return {
        "versions": versions, "command": [str(binary)], "configuration_toml": CONFIG,
        "build_command": ["cargo", "build", "--release", "--locked"],
        "profile_note": "Repository release profile; debug information disabled, no added strip or LTO settings.",
        "binary": {"path": str(binary), "bytes": binary.stat().st_size,
                   "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                   "dynamic_dependencies_ldd": dependencies, "readelf_header_dynamic": elf},
        "hardware": {"kernel": os.uname().release, "architecture": os.uname().machine,
                     "cpu_models": models, "logical_cpus": os.cpu_count(),
                     "cpu_affinity": sorted(os.sched_getaffinity(0)),
                     "mem_total": next(line for line in Path("/proc/meminfo").read_text().splitlines()
                                       if line.startswith("MemTotal:"))},
        "workflow_environment": {key: os.environ.get(key) for key in (
            "GITHUB_SHA", "GITHUB_RUN_ID", "RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion",
            "CARGO_PROFILE_RELEASE_DEBUG", "CARGO_INCREMENTAL")},
        "ticks_per_second": os.sysconf("SC_CLK_TCK"),
    }


def check(binary: Path, artifacts: Path) -> None:
    if sys.platform != "linux" or not binary.is_file():
        raise SystemExit("provide a built Linux terminal binary and run under Xvfb")
    for name in ("xdotool", "ldd", "readelf", "rustc"):
        if shutil.which(name) is None:
            raise SystemExit(f"required program is missing: {name}")
    artifacts.mkdir(parents=True, exist_ok=True)

    def timeout(_signum: int, _frame: Any) -> None:
        raise TimeoutError("resource smoke exceeded its 50-second internal deadline")

    previous_handler = signal.signal(signal.SIGALRM, timeout)
    signal.alarm(50)
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="kokuban-resource-") as temporary:
        directory = Path(temporary)
        report: dict[str, Any] = {
            "status": "running", "build_profile": "release", "phases": {},
            "sampling_interval_seconds": SAMPLE_SECONDS, "output_sampling_interval_seconds": 0.05,
            "limits": [
                "Single run on an Xvfb software display; shared CI hardware is not an isolated benchmark host.",
                "CPU covers only Kokuban utime+stime, not child CPU, Xvfb, mpv or the observer; 100% means one core.",
                "Short phases are limited by kernel CPU tick resolution; RSS is an asynchronous kernel estimate; HWM is process lifetime.",
                "Finite ASCII PTY output plus final marker/DSR proves a terminal round trip; it does not prove every line was presented or measure FPS.",
                "Output can retain the configured 10000-line scrollback; post-output idle measures settling, not a required return to startup RSS.",
                "No CPU, RSS or throughput pass thresholds; functional completion and bounded duration are required.",
            ],
        }
        try:
            report.update(metadata(binary))
            exercise(binary, directory, report)
            report["status"] = "passed"
            print("PASS: real terminal idle/output/settling measurements and acknowledged clean shutdown")
            print(json.dumps({phase: {key: value for key, value in values.items() if key != "samples"}
                              for phase, values in report["phases"].items()}, indent=2))
        except BaseException as error:
            report["status"], report["error"] = "failed", f"{type(error).__name__}: {error}"
            raise
        finally:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, previous_handler)
            report["total_seconds"] = time.monotonic() - started
            record(directory, "report.json", report)
            for path in directory.iterdir():
                if path.is_file():
                    shutil.copy2(path, artifacts / path.name)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, nargs="?")
    parser.add_argument("--artifacts-dir", type=Path)
    parser.add_argument("--child-dir", type=Path, help=argparse.SUPPRESS)
    arguments = parser.parse_args()
    if arguments.child_dir:
        child(arguments.child_dir)
    elif arguments.binary and arguments.artifacts_dir:
        check(arguments.binary.resolve(), arguments.artifacts_dir.resolve())
    else:
        parser.error("a terminal binary and --artifacts-dir are required")


if __name__ == "__main__":
    main()
