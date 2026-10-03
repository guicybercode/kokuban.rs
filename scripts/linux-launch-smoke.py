#!/usr/bin/env python3
"""Exercise Omarchy launch arguments through a real terminal process.

Run under xvfb-run (requires xdotool and xprop), or pass --backend wayland in
an existing Wayland session. Uses only the Python standard library.
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time


APP_ID = "org.omarchy.kokuban-launch-test"
TITLE = "Kokuban launch test"
ARGUMENTS = ["file with spaces.txt", "a;$(false)`false`", "--title=child argument", ""]
DRIVER = r'''
import json, os, select, sys, termios, time, tty
from pathlib import Path
directory = Path(sys.argv[1])
attributes = termios.tcgetattr(0)
response = b""
try:
    tty.setraw(0)
    os.write(1, b"\x1b[6n")
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and not response.endswith(b"R"):
        if select.select([0], [], [], max(0, deadline - time.monotonic()))[0]:
            response += os.read(0, 64)
finally:
    termios.tcsetattr(0, termios.TCSANOW, attributes)
observations = {
    "cwd": os.getcwd(), "pwd": os.environ.get("PWD"),
    "argv": sys.argv[2:], "parent_pid": os.getppid(),
    "tty": [os.isatty(fd) for fd in (0, 1, 2)],
    "term": os.environ.get("TERM"), "term_program": os.environ.get("TERM_PROGRAM"),
    "cursor_response": response.decode("ascii", errors="replace"),
}
pending = directory / "child.pending"
pending.write_text(json.dumps(observations))
pending.replace(directory / "child.json")
deadline = time.monotonic() + 15
while not (directory / "finish").exists():
    if time.monotonic() > deadline:
        raise RuntimeError("launch smoke did not acknowledge the child")
    time.sleep(0.02)
'''


def headless_checks(binary: Path, directory: Path, environment: dict) -> None:
    environment = environment.copy()
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR"):
        environment.pop(key, None)
    for arguments, status, marker in [
        (["--help"], 0, "Usage: kokuban"),
        (["--version"], 0, "kokuban "),
        (["--unknown-option"], 2, "unknown option"),
        (["-e"], 2, "expected a command"),
        (["--dir=" + str(directory / "missing")], 1, "could not open working directory"),
    ]:
        result = subprocess.run([str(binary), *arguments], cwd=directory, env=environment,
                                capture_output=True, text=True, timeout=5)
        output = result.stdout + result.stderr
        if result.returncode != status or marker not in output:
            raise AssertionError(f"headless {arguments}: status={result.returncode}, output={output!r}")


def window_properties(process: subprocess.Popen) -> dict | None:
    result = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(process.pid)],
                            capture_output=True, text=True, timeout=2)
    if result.returncode != 0 or not result.stdout.strip():
        return None
    window = result.stdout.splitlines()[0]
    result = subprocess.run(["xprop", "-id", window, "WM_CLASS", "_NET_WM_NAME"],
                            check=True, capture_output=True, text=True, timeout=2)
    lines = result.stdout.splitlines()
    class_line = next((line for line in lines if line.startswith("WM_CLASS")), "")
    title_line = next((line for line in lines if line.startswith("_NET_WM_NAME")), "")
    if re.findall(r'"([^"\\]*)"', class_line) != ["kokuban", APP_ID]:
        raise AssertionError(f"unexpected X11 class: {class_line}")
    if re.findall(r'"([^"\\]*)"', title_line) != [TITLE]:
        raise AssertionError(f"unexpected X11 title: {title_line}")
    return {"class": APP_ID, "title": TITLE}


def check(binary: Path, backend: str) -> dict:
    with tempfile.TemporaryDirectory(prefix="kokuban-launch-") as temporary:
        directory = Path(temporary)
        cwd = directory / "cwd with spaces"
        cwd.mkdir()
        (directory / "kokuban.toml").write_text(
            '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
            '[window]\ncolumns = 40\nrows = 12\n', encoding="utf-8")
        environment = os.environ.copy()
        environment.pop("KOKUBAN_EXIT_AFTER_FIRST_FRAME", None)
        if backend == "x11":
            for key in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR"):
                environment.pop(key, None)
            environment["WINIT_X11_SCALE_FACTOR"] = "1"
        else:
            environment.pop("DISPLAY", None)
        headless_checks(binary, directory, environment)
        log_path = directory / "terminal.log"
        with log_path.open("wb") as log:
            process = subprocess.Popen([
                str(binary), "--dir=" + str(cwd), "--app-id=" + APP_ID, "--title=" + TITLE,
                "-e", sys.executable, "-c", DRIVER, str(directory), *ARGUMENTS,
            ], cwd=directory, env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 12
                properties = None
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise AssertionError(f"terminal exited early: {process.returncode}")
                    if backend == "x11" and properties is None:
                        properties = window_properties(process)
                    if (directory / "child.json").exists() and (backend == "wayland" or properties):
                        break
                    time.sleep(0.05)
                else:
                    raise AssertionError("timed out waiting for terminal launch observations")
                child = json.loads((directory / "child.json").read_text())
                expected = {
                    "cwd": str(cwd), "pwd": str(cwd), "argv": ARGUMENTS,
                    "parent_pid": process.pid, "tty": [True, True, True],
                    "term": "xterm-256color", "term_program": "kokuban",
                }
                for key, value in expected.items():
                    if child.get(key) != value:
                        raise AssertionError(f"child {key}: expected {value!r}, got {child.get(key)!r}")
                if not re.fullmatch(r"\x1b\[\d+;\d+R", child["cursor_response"]):
                    raise AssertionError(f"terminal did not answer DSR: {child['cursor_response']!r}")
                (directory / "finish").touch()
                if process.wait(timeout=5) != 0:
                    raise AssertionError(f"terminal failed after command exit: {process.returncode}")
            except Exception as error:
                raise AssertionError(f"{error}\n{log_path.read_text(errors='replace')[-4000:]}") from error
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=3)
        return {"backend": backend, "headless_cli": "passed", "child": child,
                "window_properties": properties, "result": "passed"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--backend", choices=("x11", "wayland"), default="x11")
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    report = check(arguments.binary.resolve(strict=True), arguments.backend)
    encoded = json.dumps(report, indent=2) + "\n"
    if arguments.output:
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(encoded)
    print(encoded, end="")
