#!/usr/bin/env python3
"""Verify presented synchronized frames, live replies, local redraws and recovery.

Run under Xvfb with the built Kokuban binary. The emitter deliberately splits
frames and waits for the observer; screenshots must retain the prior frame.
"""

import argparse
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import subprocess
import sys
import tempfile
import termios
import time

from linux_clear_fixture import Capture, record, wait_file

RED, GREEN, BLUE, YELLOW = (160, 32, 32), (32, 160, 32), (32, 32, 160), (160, 160, 32)


def paint(color):
    return ("\x1b[48;2;{};{};{}m\x1b[2J\x1b[H".format(*color)).encode()


def emitter(directory):
    mode = termios.tcgetattr(0)
    mode[3] &= ~(termios.ECHO | termios.ICANON)
    mode[6][termios.VMIN], mode[6][termios.VTIME] = 1, 0
    termios.tcsetattr(0, termios.TCSANOW, mode)

    def emit(data):
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()

    emit(b"\x1b[?25l" + paint(RED))
    wait_file(directory / "begin")
    emit(b"\x1b[?2026h" + paint(BLUE) + b"\x1b[?2026$p")
    reply = bytearray()
    deadline = time.monotonic() + 4
    while b"\x1b[?2026;1$y" not in reply:
        if time.monotonic() >= deadline:
            raise AssertionError(f"no live DECRQM reply during sync: {reply!r}")
        if select.select([0], [], [], 0.1)[0]:
            reply.extend(os.read(0, 256))
    record(directory, "partial-ready.json", {"reply_hex": reply.hex()})
    wait_file(directory / "finish")
    emit(paint(GREEN) + b"\x1b[?2026l")
    wait_file(directory / "timeout")
    emit(b"\x1b[?2026h" + paint(YELLOW))
    record(directory, "timeout-ready.json", True)
    # No more PTY bytes until the observer has proved deadline recovery.
    wait_file(directory / "reset")
    emit(b"\x1b[?2026h" + paint(BLUE) + b"\x1b[!p\x1b[?25l")
    wait_file(directory / "done")


def run(command):
    return subprocess.run(command, check=True, capture_output=True, timeout=5).stdout


def check(binary, artifacts):
    with tempfile.TemporaryDirectory(prefix="kokuban-sync-") as temporary:
        directory = Path(temporary)
        (directory / "kokuban.toml").write_text(
            '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
            '[window]\ncolumns = 40\nrows = 20\n'
        )
        shell = directory / "shell"
        shell.write_text("#!/bin/sh\nexec " + shlex.join([
            sys.executable, str(Path(__file__).resolve()), "--emit", str(directory),
        ]) + "\n")
        shell.chmod(0o700)
        environment = os.environ.copy()
        for name in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR", "KOKUBAN_EXIT_AFTER_FIRST_FRAME"):
            environment.pop(name, None)
        environment.update(KOKUBAN_SHELL=str(shell), WINIT_X11_SCALE_FACTOR="1")
        with (directory / "terminal.log").open("wb") as log:
            process = subprocess.Popen([str(binary)], cwd=directory, env=environment, stdout=log, stderr=log)
            window = None
            try:
                def wait_for(label, predicate, timeout=8):
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        if process.poll() is not None:
                            raise AssertionError(f"terminal exited during {label}: {process.returncode}")
                        value = predicate()
                        if value:
                            return value
                        time.sleep(0.03)
                    raise AssertionError(f"timed out: {label}")

                def find_window():
                    result = subprocess.run(['xdotool', 'search', '--pid', str(process.pid)], capture_output=True)
                    return result.stdout.decode().splitlines()[0] if result.returncode == 0 else None

                window = wait_for("window", find_window)
                def capture():
                    return Capture(run(['xwd', '-id', window, '-silent']))
                def color_is(color):
                    frame = capture()
                    return frame if all(frame.pixel(x, y) == color for x, y in (
                        (10, 10), (30, 30), (frame.width // 2, frame.height // 2),
                    )) else None

                wait_for("initial frame", lambda: color_is(RED)).save(directory / "initial.png")
                (directory / "begin").touch()
                wait_for("query while partial frame pending", lambda: (directory / "partial-ready.json").exists())
                # Generate local redraws which bypass the PTY on_update callback.
                run(['xdotool', 'mousemove', '--window', window, '10', '10'])
                run(['xdotool', 'click', '4'])
                initial = capture()
                run(['xdotool', 'windowsize', window, str(initial.width + 20), str(initial.height + 20)])
                samples = 0
                until = time.monotonic() + 0.35
                while time.monotonic() < until:
                    frame = capture()
                    if frame.pixel(30, 30) != RED:
                        frame.save(directory / "partial-failure.png")
                        raise AssertionError("published partial frame during sync/local redraw")
                    samples += 1
                    time.sleep(0.03)
                frame.save(directory / "held.png")
                (directory / "finish").touch()
                wait_for("complete frame", lambda: color_is(GREEN)).save(directory / "complete.png")
                (directory / "timeout").touch()
                wait_for("timeout fixture", lambda: (directory / "timeout-ready.json").exists())
                time.sleep(0.1)
                if not color_is(GREEN):
                    raise AssertionError("timeout frame published before the bounded pause")
                started = time.monotonic()
                wait_for("timeout without further bytes", lambda: color_is(YELLOW), timeout=4).save(directory / "timeout.png")
                elapsed = time.monotonic() - started
                (directory / "reset").touch()
                wait_for("DECSTR releases sync", lambda: color_is(BLUE)).save(directory / "reset.png")
                record(directory, "result.json", {"status": "passed", "held_samples": samples,
                    "timeout_observed_seconds": elapsed, "forced_resize_and_scroll": True,
                    "reply_during_sync": json.loads((directory / "partial-ready.json").read_text())})
                print("PASS synchronized frames: live query, resize/scroll gate, ESU, idle timeout, DECSTR")
            except BaseException as error:
                record(directory, "result.json", {"status": "failed", "error": str(error)})
                if window:
                    Capture(run(['xwd', '-id', window, '-silent'])).save(directory / "failure.png")
                raise
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                if artifacts:
                    artifacts.mkdir(parents=True, exist_ok=True)
                    for path in directory.iterdir():
                        if path.suffix in (".json", ".png", ".log"):
                            shutil.copyfile(path, artifacts / path.name)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, nargs="?")
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--emit", type=Path)
    args = parser.parse_args()
    if args.emit:
        emitter(args.emit)
    elif args.binary:
        check(args.binary.resolve(), args.artifacts)
    else:
        parser.error("binary is required")
