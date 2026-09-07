#!/usr/bin/env python3
"""Exercise real X11 copy, paste and selection under xvfb-run.

Requires xclip, xdotool and xwd. A raw PTY child records every input byte;
DSR replies acknowledge terminal-mode changes before keyboard/mouse injection.
No window manager, Python packages or external clipboard daemon are needed.
"""

import argparse
import json
import os
from pathlib import Path
import re
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import time
import tty
from typing import Optional


COPY_LINE = b"COPY_THIS_TEXT extra"
SHIFT_LINE = b"SHIFT_COPY_TEXT extra"
COPY_TEXT = b"COPY_THIS_TEXT"
SHIFT_TEXT = b"SHIFT_COPY_TEXT"
PASTE_ONE = "ação 🦀\r\nlinha\t2\n終わり".encode("utf-8")
PASTE_TWO = "safe\x1b[201~\n二\tfim".encode("utf-8")
EXPECTED_ONE = b"\x1b[200~" + "ação 🦀\rlinha\t2\r終わり".encode("utf-8") + b"\x1b[201~"
EXPECTED_TWO = b"\x1b[200~" + "safe[201~\r二\tfim".encode("utf-8") + b"\x1b[201~"
DSR_REPLY = re.compile(rb"\x1b\[[0-9]+;[0-9]+R")
MAX_CAPTURE_BYTES = 64 * 1024


def atomic_text(path: Path, value: str) -> None:
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(value, encoding="utf-8")
    temporary.replace(path)


def process_identity(pid: int) -> str:
    """Linux process start time prevents accidentally signaling a reused PID."""
    return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]


def child() -> None:
    directory = Path.cwd()
    tty.setraw(sys.stdin.fileno())
    atomic_text(directory / "child.json", json.dumps({
        "pid": os.getpid(), "identity": process_identity(os.getpid()),
    }))
    current_phase = None
    pending_reply = None
    reply_buffer = bytearray()
    captured_bytes = 0
    with (directory / "input.bin").open("wb", buffering=0) as captured:
        while not (directory / "stop").exists():
            requested = (directory / "phase").read_text().strip()
            if requested != current_phase:
                if pending_reply is not None:
                    raise AssertionError("phase changed before the terminal acknowledged it")
                current_phase = requested
                pending_reply = requested
                mouse_mode = b"\x1b[?1000l\x1b[?1002l\x1b[?1006l"
                line = COPY_LINE
                if requested == "mouse":
                    mouse_mode = b"\x1b[?1002h\x1b[?1006h"
                    line = SHIFT_LINE
                elif requested not in ("paste", "plain", "copy"):
                    raise AssertionError(f"unknown smoke phase: {requested}")
                paste_mode = b"\x1b[?2004l" if requested == "plain" else b"\x1b[?2004h"
                os.write(1, b"\x1b[?25l\x1b[2J\x1b[H" + line
                         + paste_mode + mouse_mode + b"\x1b[6n")
            readable, _, _ = select.select([sys.stdin.fileno()], [], [], 0.02)
            if not readable:
                continue
            incoming = os.read(sys.stdin.fileno(), 4096)
            if not incoming:
                break
            captured_bytes += len(incoming)
            if captured_bytes > MAX_CAPTURE_BYTES:
                raise AssertionError("unexpected excessive terminal input")
            if pending_reply is not None:
                reply_buffer.extend(incoming)
                reply = DSR_REPLY.search(reply_buffer)
                if reply is None:
                    continue
                captured.write(reply_buffer[:reply.start()] + reply_buffer[reply.end():])
                reply_buffer.clear()
                size = os.get_terminal_size(sys.stdin.fileno())
                atomic_text(directory / "ready.json", json.dumps({
                    "phase": pending_reply, "columns": size.columns, "rows": size.lines,
                }))
                pending_reply = None
            else:
                captured.write(incoming)


def run(arguments: list[str], timeout: float = 3) -> subprocess.CompletedProcess:
    return subprocess.run(arguments, check=True, capture_output=True, timeout=timeout)


def xdo(*arguments: str) -> bytes:
    return run(["xdotool", *arguments]).stdout


def stop_process(process: subprocess.Popen) -> None:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=2)


class ClipboardSmoke:
    def __init__(self, directory: Path, terminal: subprocess.Popen, log) -> None:
        self.directory = directory
        self.terminal = terminal
        self.log = log
        self.deadline = time.monotonic() + 40
        self.owners: list[subprocess.Popen] = []
        self.window = ""
        self.cell_width = 0.0
        self.cell_height = 0.0
        self.expected_input = b""
        self.stage = "starting"
        self.current_phase = None
        self.geometry = {}
        self.grid_size = {}
        self.expected_clipboard = None
        self.last_clipboard = None
        self.last_clipboard_bytes = b""

    def wait(self, condition, description: str, timeout: float = 8):
        self.stage = description
        deadline = min(self.deadline, time.monotonic() + timeout)
        while time.monotonic() < deadline:
            if self.terminal.poll() is not None:
                raise AssertionError(f"terminal exited early: {self.terminal.returncode}")
            result = condition()
            if result:
                return result
            time.sleep(0.03)
        raise AssertionError(f"timed out waiting for {description}")

    def phase(self, name: str) -> dict:
        self.current_phase = name
        atomic_text(self.directory / "phase", name)

        def ready():
            path = self.directory / "ready.json"
            if path.exists():
                status = json.loads(path.read_text())
                if status["phase"] == name:
                    return status
            return None

        status = self.wait(ready, f"{name} mode acknowledgment")
        self.grid_size = status
        if self.window:
            self.refresh_geometry()
        return status

    def locate_window(self, size: dict) -> None:
        def search():
            result = subprocess.run(
                ["xdotool", "search", "--onlyvisible", "--pid", str(self.terminal.pid)],
                capture_output=True, timeout=2,
            )
            return result.stdout.decode().splitlines() if result.returncode == 0 else None

        self.window = self.wait(search, "Kokuban X11 window")[0]
        xdo("windowfocus", "--sync", self.window)
        self.grid_size = size
        self.refresh_geometry()

    def refresh_geometry(self) -> None:
        # The mapped window initially uses estimated metrics. Font loading can
        # resize it without changing the PTY's columns/rows, so each gesture
        # must use the current physical size instead of the startup snapshot.
        geometry = dict(line.split("=", 1) for line in xdo(
            "getwindowgeometry", "--shell", self.window
        ).decode().splitlines() if "=" in line)
        self.geometry = geometry
        self.cell_width = int(geometry["WIDTH"]) / self.grid_size["columns"]
        self.cell_height = int(geometry["HEIGHT"]) / self.grid_size["rows"]
        if self.cell_width < 2 or self.cell_height < 2:
            raise AssertionError(f"invalid terminal cell geometry: {geometry}, {self.grid_size}")

    def clipboard(self) -> bytes:
        try:
            result = subprocess.run(
                ["xclip", "-selection", "clipboard", "-out", "-target", "UTF8_STRING"],
                capture_output=True, timeout=2,
            )
        except subprocess.TimeoutExpired as error:
            self.record_clipboard(error.stdout or b"", error.stderr or b"", None, timed_out=True)
            raise
        self.record_clipboard(result.stdout, result.stderr, result.returncode)
        return result.stdout if result.returncode == 0 else b""

    def record_clipboard(self, stdout: bytes, stderr: bytes, returncode, timed_out: bool = False) -> None:
        self.last_clipboard_bytes = stdout[:MAX_CAPTURE_BYTES]
        self.last_clipboard = {
            "returncode": returncode, "timed_out": timed_out,
            "stdout_bytes": len(stdout), "stdout_hex": stdout[:4096].hex(),
            "stdout_preview": repr(stdout[:256]), "stdout_hex_truncated": len(stdout) > 4096,
            "captured_bytes": len(self.last_clipboard_bytes),
            "capture_truncated": len(stdout) > MAX_CAPTURE_BYTES,
            "stderr_bytes": len(stderr), "stderr": stderr[-4096:].decode(errors="replace"),
        }

    def set_clipboard(self, text: bytes) -> None:
        path = self.directory / f"clipboard-{len(self.owners)}.txt"
        path.write_bytes(text)
        # -quiet stays in the foreground, so its owner is tracked and cleaned up.
        owner = subprocess.Popen(
            ["xclip", "-selection", "clipboard", "-in", "-quiet", str(path)],
            stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log,
        )
        self.owners.append(owner)
        self.wait(lambda: self.clipboard() == text, "external clipboard ownership")

    def key(self, keys: str) -> None:
        xdo("key", "--clearmodifiers", "--delay", "20", keys)

    def verify_input(self, appended: bytes = b"") -> None:
        self.expected_input += appended
        path = self.directory / "input.bin"
        self.wait(lambda: path.exists() and len(path.read_bytes()) >= len(self.expected_input),
                  "raw PTY input")
        # Include delayed releases/duplicate shortcut events in the comparison.
        time.sleep(0.2)
        actual = path.read_bytes()
        if actual != self.expected_input:
            raise AssertionError(f"PTY input mismatch\nexpected: {self.expected_input!r}\nactual: {actual!r}")

    def cell_point(self, column: int, row: int = 0) -> tuple[str, str]:
        return (str(int((column + 0.5) * self.cell_width)),
                str(int((row + 0.5) * self.cell_height)))

    def drag(self, columns: int, shift: bool = False) -> None:
        self.refresh_geometry()
        if shift:
            xdo("keydown", "Shift_L")
        try:
            xdo("mousemove", "--sync", "--window", self.window, *self.cell_point(0),
                "mousedown", "1", "sleep", "0.05",
                "mousemove", "--sync", "--window", self.window, *self.cell_point(columns - 1),
                "sleep", "0.05", "mouseup", "1")
        finally:
            if shift:
                xdo("keyup", "Shift_L")

    def row_pixels(self) -> bytes:
        path = self.directory / "selection.xwd"
        run(["xwd", "-id", self.window, "-silent", "-out", str(path)])
        data = path.read_bytes()
        if len(data) < 100:
            raise AssertionError("truncated selection screenshot")
        header = struct.unpack_from(">25I", data)
        size, version, image_format, _, width, height = header[:6]
        pixel_bytes, stride = header[11] // 8, header[12]
        offset = size + header[19] * 12
        if version != 7 or image_format != 2 or pixel_bytes not in (3, 4):
            raise AssertionError("unsupported selection screenshot format")
        if len(data) < offset + stride * height or stride < width * pixel_bytes:
            raise AssertionError("truncated selection screenshot pixels")
        rows = min(height, int(self.cell_height))
        return b"".join(data[offset + row * stride:offset + row * stride + width * pixel_bytes]
                        for row in range(rows))

    def verify_highlight(self, before: bytes, columns: int) -> None:
        minimum_changes = max(40, int(columns * self.cell_width * self.cell_height / 4))
        self.wait(lambda: sum(left != right for left, right in zip(before, self.row_pixels()))
                  >= minimum_changes, "visible selection highlight", timeout=4)

    def copy_selection(self, expected: bytes, shift: bool = False) -> None:
        self.expected_clipboard = expected
        self.stage = "capture before selection"
        self.refresh_geometry()
        before = self.row_pixels()
        self.stage = "drag selection"
        self.drag(len(expected), shift)
        self.verify_highlight(before, len(expected))
        self.key("ctrl+shift+c")
        self.wait(lambda: self.clipboard() == expected, "copied selection text")
        self.verify_input()
        # A second independent client proves the copy owner was retained.
        self.stage = "retained clipboard owner"
        if self.clipboard() != expected:
            raise AssertionError("clipboard owner was dropped after copying")

    def exercise(self) -> None:
        self.locate_window(self.phase("paste"))
        self.set_clipboard(PASTE_ONE)
        self.key("ctrl+shift+v")
        self.verify_input(EXPECTED_ONE)
        self.set_clipboard(PASTE_TWO)
        self.key("shift+Insert")
        self.verify_input(EXPECTED_TWO)
        self.phase("plain")
        self.set_clipboard(PASTE_ONE)
        self.key("ctrl+shift+v")
        self.verify_input(EXPECTED_ONE[6:-6])
        self.phase("copy")
        self.copy_selection(COPY_TEXT)
        self.phase("mouse")
        # Prove mouse reporting is active before checking Shift's local override.
        xdo("mousemove", "--sync", "--window", self.window, *self.cell_point(25, 4),
            "mousedown", "1", "sleep", "0.05", "mouseup", "1")
        self.verify_input(b"\x1b[<0;26;5M\x1b[<0;26;5m")
        self.copy_selection(SHIFT_TEXT, shift=True)

    def failure_diagnostics(self, error: Exception, artifacts: Optional[Path]) -> dict:
        """Observe the failed attempt without repeating its input or copy action."""
        report = {
            "status": "failed", "error": f"{type(error).__name__}: {error}",
            "stage": self.stage, "phase": self.current_phase, "window": self.window,
            "geometry": self.geometry, "cell_size": [self.cell_width, self.cell_height],
            "clipboard": self.last_clipboard,
            "expected_clipboard_hex": (self.expected_clipboard.hex()
                                       if self.expected_clipboard is not None else None),
            "terminal_returncode": self.terminal.poll(),
            "clipboard_owners": [{"pid": owner.pid, "returncode": owner.poll()} for owner in self.owners],
            "expected_input_hex": self.expected_input.hex(),
        }
        diagnostics = {}
        if self.window:
            for name, arguments in (
                    ("geometry", ["xdotool", "getwindowgeometry", "--shell", self.window]),
                    ("pointer", ["xdotool", "getmouselocation", "--shell"]),
                    ("focus", ["xdotool", "getwindowfocus"]),
                    ("screenshot", ["xwd", "-id", self.window, "-silent", "-out",
                                    str(self.directory / "failure.xwd")])):
                try:
                    result = run(arguments)
                    diagnostics[name] = {"returncode": result.returncode,
                                         "stdout": result.stdout[:4096].decode(errors="replace"),
                                         "stderr": result.stderr[-4096:].decode(errors="replace")}
                except Exception as diagnostic_error:
                    diagnostics[name] = {"error": f"{type(diagnostic_error).__name__}: {diagnostic_error}"}
        report["diagnostics"] = diagnostics
        if artifacts is not None:
            artifacts.mkdir(parents=True, exist_ok=True)
            copied = {}
            # Only our raw PTY fixture, clipboard observation, XWD captures and
            # terminal log are retained. Never copy arbitrary temporary files.
            for name in ("input.bin", "selection.xwd", "failure.xwd", "terminal.log"):
                source = self.directory / name
                if source.is_file():
                    size = source.stat().st_size
                    if size <= 4 * 1024 * 1024:
                        shutil.copyfile(source, artifacts / name)
                        copied[name] = {"bytes": size}
                    else:
                        copied[name] = {"bytes": size, "skipped": "exceeds 4 MiB diagnostic limit"}
            (artifacts / "clipboard.bin").write_bytes(self.last_clipboard_bytes)
            copied["clipboard.bin"] = {"bytes": len(self.last_clipboard_bytes)}
            report["artifacts"] = copied
            atomic_text(artifacts / "results.json", json.dumps(report, indent=2) + "\n")
        return report

    def cleanup(self) -> None:
        (self.directory / "stop").touch()
        try:
            for owner in self.owners:
                stop_process(owner)
            # Release injected modifiers/buttons even when an assertion interrupted
            # a gesture, then terminate only our own raw PTY helper if still alive.
            if self.window:
                for arguments in (["keyup", "Shift_L", "Control_L"], ["mouseup", "1"]):
                    try:
                        subprocess.run(["xdotool", *arguments], capture_output=True, timeout=2)
                    except (OSError, subprocess.TimeoutExpired):
                        pass
            child_file = self.directory / "child.json"
            if child_file.exists():
                identity = json.loads(child_file.read_text())
                try:
                    if process_identity(identity["pid"]) == identity["identity"]:
                        os.kill(identity["pid"], signal.SIGTERM)
                except (ProcessLookupError, FileNotFoundError):
                    pass
        finally:
            stop_process(self.terminal)


def main() -> None:
    if sys.argv[1:] == ["--child"]:
        child()
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--artifacts", type=Path, help="preserve bounded diagnostics if a check fails")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    artifacts = args.artifacts.resolve() if args.artifacts is not None else None
    with tempfile.TemporaryDirectory(prefix="kokuban-clipboard-") as temporary:
        directory = Path(temporary)
        (directory / "kokuban.toml").write_text(
            '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
            '[window]\ncolumns = 40\nrows = 20\n'
        )
        atomic_text(directory / "phase", "paste")
        shell = directory / "shell"
        shell.write_text("#!/bin/sh\nexec " + shlex.join(
            [sys.executable, str(Path(__file__).resolve()), "--child"]
        ) + "\n")
        shell.chmod(0o700)
        environment = os.environ.copy()
        for name in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR",
                     "KOKUBAN_EXIT_AFTER_FIRST_FRAME"):
            environment.pop(name, None)
        environment.update(KOKUBAN_SHELL=str(shell), WINIT_X11_SCALE_FACTOR="1")
        log_path = directory / "terminal.log"
        with log_path.open("wb") as log:
            terminal = subprocess.Popen([str(binary)], cwd=directory, env=environment,
                                        stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            smoke = ClipboardSmoke(directory, terminal, log)
            try:
                smoke.exercise()
            except Exception as error:
                try:
                    diagnostic = smoke.failure_diagnostics(error, artifacts)
                except Exception as diagnostic_error:
                    diagnostic = {"diagnostic_error": f"{type(diagnostic_error).__name__}: {diagnostic_error}"}
                raise AssertionError(f"Linux clipboard smoke: {error}\n"
                                     + json.dumps(diagnostic, indent=2) + "\n"
                                     + log_path.read_text(errors="replace")[-4000:]) from error
            finally:
                smoke.cleanup()
        print("PASS clipboard: CtrlShiftV, ShiftInsert, exact UTF-8 paste bytes, "
              "drag copy, retained owner, visible highlight and Shift mouse override",
              flush=True)


if __name__ == "__main__":
    main()
