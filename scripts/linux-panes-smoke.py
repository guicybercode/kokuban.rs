#!/usr/bin/env python3
"""Split and close panes in a real Linux window under xvfb-run.

Requires xdotool, xwd and libX11, and only the Python standard library.
Kokuban starts with `-e /bin/sh`; `Ctrl+Shift+D` must spawn a second shell
(a new child of the terminal process) and present a hanko-red vertical
divider in the middle of the window; `Ctrl+Shift+W` must close that idle pane
at once, reap its shell and remove the divider while the window stays alive.

A second split then exercises the search strip and the close confirmation:
`Ctrl+Shift+F` must paint a chalk strip over the pane's last row, show the
miss counter in hanko red, and keep the query out of the shell after Escape.
With `sleep` in the foreground, `Ctrl+Shift+W` must ask instead of closing;
`n` keeps the pane, `y` closes it, and on the last pane `y` exits cleanly.

A second session runs `sleep` and asks the window to close through
WM_DELETE_WINDOW: the strip must appear, Escape keeps the terminal and its
shell, and Enter on the next request exits cleanly.
"""

import argparse
import ctypes
import ctypes.util
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time


COLUMNS = 60
ROWS = 16
HANKO_RED = "#b5312c"
BOARD = "#1f3a2a"
CHALK = "#f4f6f0"
DIVIDER_THICKNESS = 6


def run(*arguments: str) -> bytes:
    return subprocess.run(arguments, check=True, capture_output=True, timeout=3).stdout


def xdo(*arguments: str) -> bytes:
    return run("xdotool", *arguments)


class Frame:
    def __init__(self, data: bytes) -> None:
        if len(data) < 100:
            raise AssertionError("truncated XWD header")
        header = struct.unpack_from(">25I", data)
        size, version, image_format, _, self.width, self.height, xoffset, order = header[:8]
        bits, self.stride, visual = header[11:14]
        self.masks = header[14:17]
        self.pixel_bytes = bits // 8
        self.offset = size + header[19] * 12
        if (version != 7 or image_format != 2 or visual != 4 or xoffset != 0
                or bits not in (24, 32) or order not in (0, 1) or not all(self.masks)):
            raise AssertionError("expected a 24/32-bit TrueColor ZPixmap XWD")
        if (size < 100 or self.stride < self.width * self.pixel_bytes
                or len(data) < self.offset + self.stride * self.height):
            raise AssertionError("truncated XWD pixels")
        self.order = "little" if order == 0 else "big"
        self.mask = self.masks[0] | self.masks[1] | self.masks[2]
        self.data = data

    def packed(self, color: str) -> int:
        rgb = bytes.fromhex(color.removeprefix("#"))
        desired = 0
        for component, mask in zip(rgb, self.masks):
            shift = (mask & -mask).bit_length() - 1
            desired |= (component * (mask >> shift) // 255) << shift
        return desired

    def count(self, color: str, region: tuple[int, int, int, int]) -> int:
        desired = self.packed(color)
        left, top, right, bottom = region
        if not (0 <= left < right <= self.width and 0 <= top < bottom <= self.height):
            raise AssertionError(f"pixel region outside window: {region}")
        matches = 0
        for y in range(top, bottom):
            row = self.offset + y * self.stride
            for x in range(left, right):
                offset = row + x * self.pixel_bytes
                pixel = int.from_bytes(self.data[offset:offset + self.pixel_bytes], self.order)
                matches += pixel & self.mask == desired
        return matches

    def count_not(self, color: str) -> int:
        undesired = self.packed(color)
        matches = 0
        for y in range(self.height):
            row = self.offset + y * self.stride
            for x in range(self.width):
                offset = row + x * self.pixel_bytes
                pixel = int.from_bytes(self.data[offset:offset + self.pixel_bytes], self.order)
                matches += pixel & self.mask != undesired
        return matches


def send_wm_delete_window(window: int) -> None:
    """Ask the window to close as a window manager's close button does."""
    x11 = ctypes.CDLL(ctypes.util.find_library("X11") or "libX11.so.6")
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XInternAtom.restype = ctypes.c_ulong
    x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    x11.XSendEvent.argtypes = [
        ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.c_void_p,
    ]
    x11.XFlush.argtypes = [ctypes.c_void_p]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]

    class ClientMessage(ctypes.Structure):
        _fields_ = [
            ("type", ctypes.c_int), ("serial", ctypes.c_ulong), ("send_event", ctypes.c_int),
            ("display", ctypes.c_void_p), ("window", ctypes.c_ulong),
            ("message_type", ctypes.c_ulong), ("format", ctypes.c_int),
            ("data", ctypes.c_long * 5),
        ]

    class Event(ctypes.Union):
        _fields_ = [("xclient", ClientMessage), ("pad", ctypes.c_long * 24)]

    display = x11.XOpenDisplay(None)
    if not display:
        raise AssertionError("could not open the X display")
    try:
        event = Event()
        event.xclient.type = 33  # ClientMessage
        event.xclient.window = window
        event.xclient.message_type = x11.XInternAtom(display, b"WM_PROTOCOLS", 0)
        event.xclient.format = 32
        event.xclient.data[0] = x11.XInternAtom(display, b"WM_DELETE_WINDOW", 0)
        if not x11.XSendEvent(display, window, 0, 0, ctypes.byref(event)):
            raise AssertionError("XSendEvent rejected WM_DELETE_WINDOW")
        x11.XFlush(display)
    finally:
        x11.XCloseDisplay(display)


def process_name(pid: int) -> str:
    try:
        return (Path("/proc") / str(pid) / "comm").read_text().strip()
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return ""


def child_pids(parent: int) -> list[int]:
    """Direct children of `parent`, from /proc (no psutil on the CI image)."""
    children = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            stat = (entry / "stat").read_text()
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        fields = stat.rsplit(")", 1)[1].split()
        if int(fields[1]) == parent:
            children.append(int(entry.name))
    return sorted(children)


class PanesSmoke:
    def __init__(self, directory: Path, terminal: subprocess.Popen) -> None:
        self.directory = directory
        self.terminal = terminal
        self.deadline = time.monotonic() + 60
        self.window = ""
        self.last_mismatch = ""

    def wait(self, condition, description: str, timeout: float = 10):
        deadline = min(self.deadline, time.monotonic() + timeout)
        while time.monotonic() < deadline:
            if self.terminal.poll() is not None:
                raise AssertionError(f"terminal exited early: {self.terminal.returncode}")
            result = condition()
            if result:
                return result
            time.sleep(0.05)
        raise AssertionError(f"timed out waiting for {description}: {self.last_mismatch}")

    def locate(self) -> None:
        def search():
            found = subprocess.run(
                ["xdotool", "search", "--onlyvisible", "--pid", str(self.terminal.pid)],
                capture_output=True, timeout=2,
            )
            return found.stdout.decode().splitlines() if found.returncode == 0 else None

        self.window = self.wait(search, "Kokuban X11 window")[0]
        xdo("windowfocus", "--sync", self.window)

    def capture(self) -> Frame:
        screenshot = self.directory / "frame.xwd"
        run("xwd", "-id", self.window, "-silent", "-out", str(screenshot))
        return Frame(screenshot.read_bytes())

    def key(self, keys: str) -> None:
        xdo("key", "--window", self.window, "--clearmodifiers", "--delay", "40", keys)

    def shells(self) -> list[int]:
        return child_pids(self.terminal.pid)

    def wait_for_shells(self, count: int, description: str) -> list[int]:
        def settled():
            shells = self.shells()
            self.last_mismatch = f"children of kokuban: {shells}"
            return shells if len(shells) == count else None

        return self.wait(settled, description)

    def divider_pixels(self, frame: Frame) -> int:
        middle = frame.width // 2
        margin = DIVIDER_THICKNESS
        region = (max(middle - margin, 0), 0, min(middle + margin, frame.width), frame.height)
        return frame.count(HANKO_RED, region)

    def wait_for_divider(self, present: bool) -> Frame:
        def matches():
            frame = self.capture()
            pixels = self.divider_pixels(frame)
            # A divider is DIVIDER_THICKNESS pixels wide over the whole height;
            # allow one column of rounding on each side.
            minimum = (DIVIDER_THICKNESS - 2) * frame.height
            self.last_mismatch = (f"{pixels} hanko-red pixels in the middle band of a "
                                  f"{frame.width}x{frame.height} frame")
            if present:
                return frame if pixels >= minimum else None
            return frame if pixels == 0 else None

        return self.wait(matches, "a vertical divider" if present else "no divider")

    def last_row(self, frame: Frame, right_pane: bool) -> tuple[int, int, int, int]:
        """Last cell row of the right pane (after a split) or of the window."""
        cell_height = frame.height // ROWS
        left = frame.width // 2 + DIVIDER_THICKNESS if right_pane else 0
        return (left, (ROWS - 1) * cell_height, frame.width, ROWS * cell_height)

    def wait_for_strip(self, present: bool, right_pane: bool, description: str) -> Frame:
        def matches():
            frame = self.capture()
            region = self.last_row(frame, right_pane)
            area = (region[2] - region[0]) * (region[3] - region[1])
            chalk = frame.count(CHALK, region)
            self.last_mismatch = f"{chalk} chalk pixels of {area} in the last row {region}"
            if present:
                return frame if chalk >= area * 0.6 else None
            return frame if chalk <= area * 0.1 else None

        return self.wait(matches, description)

    def type_line(self, text: str) -> None:
        xdo("type", "--window", self.window, "--delay", "20", text)
        self.key("Return")

    def wait_for_job(self, shell: int, name: str) -> int:
        def running():
            jobs = [pid for pid in child_pids(shell) if process_name(pid) == name]
            self.last_mismatch = f"children of {shell}: {child_pids(shell)}"
            return jobs[0] if jobs else None

        return self.wait(running, f"{name} under shell {shell}")

    def stays(self, condition, description: str, duration: float = 0.6) -> None:
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            if not condition():
                raise AssertionError(description)
            time.sleep(0.05)

    def exercise_search(self) -> dict:
        marker = self.directory / "search-marker"
        self.type_line("echo kokuban-needle")
        self.key("ctrl+shift+f")
        self.wait_for_strip(True, True, "the search strip")
        xdo("type", "--window", self.window, "--delay", "20", "kokuban-needle")
        found = self.wait_for_strip(True, True, "the strip with matches")
        found_red = found.count(HANKO_RED, self.last_row(found, True))
        xdo("type", "--window", self.window, "--delay", "20", "zz")

        def missing():
            frame = self.capture()
            red = frame.count(HANKO_RED, self.last_row(frame, True))
            self.last_mismatch = f"{red} hanko-red pixels in the strip"
            return red if red > found_red else None

        miss_red = self.wait(missing, "the hanko-red 'no matches' counter")
        self.key("Escape")
        self.wait_for_strip(False, True, "the strip to close after Escape")
        # A leaked query would turn this into `kokuban-needlezzecho ...`.
        self.type_line(f"echo ok > {marker}")
        self.wait(lambda: marker.is_file() and marker.read_text().strip() == "ok",
                  "the shell to run the command typed after the search")
        return {"strip_red_with_matches": found_red, "strip_red_without_matches": miss_red}

    def exercise_close_confirmation(self, first_shell: int) -> dict:
        self.key("ctrl+shift+d")
        shells = self.wait_for_shells(2, "the second split pane's shell")
        shell = next(pid for pid in shells if pid != first_shell)
        self.wait_for_divider(True)
        search = self.exercise_search()

        self.type_line("sleep 60")
        job = self.wait_for_job(shell, "sleep")
        self.key("ctrl+shift+w")
        self.wait_for_strip(True, True, "the close prompt over the busy pane")
        self.stays(lambda: len(self.shells()) == 2, "a busy pane closed without asking")
        self.key("n")
        self.wait_for_strip(False, True, "the prompt to close after n")
        self.stays(lambda: self.shells() == shells and job in child_pids(shell),
                   "cancelling the prompt changed the panes")

        self.key("ctrl+shift+w")
        self.wait_for_strip(True, True, "the close prompt again")
        self.key("y")
        if self.wait_for_shells(1, "the confirmed pane's shell to be reaped") != [first_shell]:
            raise AssertionError("confirming closed the wrong pane")
        self.wait_for_divider(False)

        # The last pane: confirming its close ends the terminal.
        self.type_line("sleep 60")
        self.wait_for_job(first_shell, "sleep")
        self.key("ctrl+shift+w")
        self.wait_for_strip(True, False, "the close prompt over the last pane")
        self.stays(lambda: self.terminal.poll() is None, "the last busy pane closed unasked")
        subprocess.run(
            ["xdotool", "key", "--window", self.window, "--clearmodifiers", "y"],
            capture_output=True, timeout=3, check=False,
        )
        try:
            status = self.terminal.wait(timeout=8)
        except subprocess.TimeoutExpired as error:
            raise AssertionError("confirming the last pane did not exit the terminal") from error
        if status != 0:
            raise AssertionError(f"terminal failed after the confirmed close: {status}")
        return {"result": "passed", "confirmed_shell": shell, **search}

    def exercise_window_close(self) -> dict:
        self.locate()
        shell = self.wait_for_shells(1, "the shell")[0]
        self.wait(lambda: self.capture().count_not(BOARD) > 0, "the first prompt")
        self.type_line("sleep 60")
        job = self.wait_for_job(shell, "sleep")
        send_wm_delete_window(int(self.window))
        self.wait_for_strip(True, False, "the window close prompt")
        self.stays(lambda: self.terminal.poll() is None, "the window closed without asking")
        self.key("Escape")
        self.wait_for_strip(False, False, "the window prompt to close after Escape")
        self.stays(lambda: job in child_pids(shell), "cancelling ended the job")

        send_wm_delete_window(int(self.window))
        self.wait_for_strip(True, False, "the window close prompt again")
        subprocess.run(
            ["xdotool", "key", "--window", self.window, "--clearmodifiers", "Return"],
            capture_output=True, timeout=3, check=False,
        )
        try:
            status = self.terminal.wait(timeout=8)
        except subprocess.TimeoutExpired as error:
            raise AssertionError("confirming the window close did not exit") from error
        if status != 0:
            raise AssertionError(f"terminal failed after the confirmed window close: {status}")
        return {"result": "passed", "shell": shell}

    def exercise(self) -> dict:
        self.locate()
        shells = self.wait_for_shells(1, "the first shell")
        first_shell = shells[0]
        # The shell prompt makes the first frame non-empty.
        self.wait(lambda: self.capture().count_not(BOARD) > 0, "the first prompt")
        before = self.capture()
        if self.divider_pixels(before) != 0:
            raise AssertionError("a single pane must not draw a divider")

        self.key("ctrl+shift+d")
        shells = self.wait_for_shells(2, "the split pane's shell")
        if first_shell not in shells:
            raise AssertionError(f"the split replaced the first shell: {shells}")
        second_shell = next(pid for pid in shells if pid != first_shell)
        split = self.wait_for_divider(True)
        if split.count_not(BOARD) <= self.divider_pixels(split):
            raise AssertionError("the split frame shows no pane content")

        self.key("ctrl+shift+w")
        shells = self.wait_for_shells(1, "the closed pane's shell to be reaped")
        if shells != [first_shell]:
            raise AssertionError(f"closing the split pane must keep the first shell: {shells}")
        if second_shell in shells:
            raise AssertionError("the closed pane's shell is still a child")
        self.wait_for_divider(False)
        if self.terminal.poll() is not None:
            raise AssertionError("the window exited while a pane was still open")

        # The window closes on the confirming key press, so its key-up can
        # fail to reach it; the clean exit status is the assertion.
        confirmation = self.exercise_close_confirmation(first_shell)
        return {
            "result": "passed",
            "first_shell": first_shell,
            "second_shell": second_shell,
            "frame": [split.width, split.height],
            "divider_pixels": self.divider_pixels(split),
            "close_confirmation": confirmation,
        }

    def cleanup(self) -> None:
        if self.window:
            for arguments in (["keyup", "Shift_L", "Control_L"],):
                try:
                    subprocess.run(["xdotool", *arguments], capture_output=True, timeout=2)
                except (OSError, subprocess.TimeoutExpired):
                    pass
        if self.terminal.poll() is None:
            self.terminal.terminate()
            try:
                self.terminal.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.terminal.kill()
                self.terminal.wait(timeout=3)


def run_session(binary: Path, directory: Path, name: str, scenario) -> dict:
    environment = os.environ.copy()
    environment.pop("KOKUBAN_EXIT_AFTER_FIRST_FRAME", None)
    for key in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR"):
        environment.pop(key, None)
    environment["WINIT_X11_SCALE_FACTOR"] = "1"
    environment["RUST_LOG"] = "warn"
    # Splits start the same job-control shell as `-e /bin/sh`.
    environment["KOKUBAN_SHELL"] = "/bin/sh"
    log_path = directory / f"{name}.log"
    with log_path.open("wb") as log:
        terminal = subprocess.Popen(
            [str(binary), "-e", "/bin/sh"],
            cwd=directory, env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
        )
        smoke = PanesSmoke(directory, terminal)
        try:
            return scenario(smoke)
        except Exception as error:
            raise AssertionError(
                f"Linux panes smoke ({name}): {error}\n"
                f"{log_path.read_text(errors='replace')[-4000:]}"
            ) from error
        finally:
            smoke.cleanup()


def check(binary: Path) -> dict:
    with tempfile.TemporaryDirectory(prefix="kokuban-panes-") as temporary:
        directory = Path(temporary)
        (directory / "kokuban.toml").write_text(
            '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
            f"[window]\ncolumns = {COLUMNS}\nrows = {ROWS}\n"
            "[omarchy]\nenabled = false\n",
            encoding="utf-8",
        )
        report = run_session(binary, directory, "panes", PanesSmoke.exercise)
        report["window_close"] = run_session(
            binary, directory, "window-close", PanesSmoke.exercise_window_close
        )
        return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    report = check(arguments.binary.resolve(strict=True))
    encoded = json.dumps(report, indent=2) + "\n"
    if arguments.output:
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(encoded)
    print(encoded, end="")
