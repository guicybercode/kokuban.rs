#!/usr/bin/env python3
"""Split and close panes in a real Linux window under xvfb-run.

Requires xdotool, xwd and libX11, and only the Python standard library.
Kokuban starts with `-e /bin/sh`; `Ctrl+Shift+D` must spawn a second shell
(a new child of the terminal process) and present a hanko-red vertical
divider in the middle of the window; `Ctrl+Shift+W` must close that idle pane
at once, reap its shell and remove the divider while the window stays alive.

A second split then exercises the search strip and the close confirmation:
`Ctrl+Shift+F` must paint a chalk strip over the pane's last row, show the
miss counter in hanko red, keep a long query on one row and keep the query
out of the shell after Escape. A third, narrow split must show the strip too.
With `sleep` in the foreground, `Ctrl+Shift+W` must ask instead of closing;
`n` keeps the pane, `y` closes it, and on the last pane `y` exits cleanly.

A second session runs `sleep` and `tail` in two panes and asks the window to
close through WM_DELETE_WINDOW: the strip must appear, Escape keeps the
terminal and its jobs, Enter (the default N of [y/N]) keeps them too, and y on
the next request exits cleanly.

With --screenshots DIR the frames behind six of those checks are also saved
as PNGs (scaled 2x): a search with matches, no matches, a long query, a
narrow pane, and the close prompts with one and with two jobs.
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
import zlib


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

    def rgb_rows(self, scale: int):
        """Rows of 8-bit RGB bytes, each pixel repeated `scale` times."""
        shifts = [(mask & -mask).bit_length() - 1 for mask in self.masks]
        maxima = [mask >> shift for mask, shift in zip(self.masks, shifts)]
        for y in range(self.height):
            row = self.offset + y * self.stride
            line = bytearray()
            for x in range(self.width):
                offset = row + x * self.pixel_bytes
                pixel = int.from_bytes(self.data[offset:offset + self.pixel_bytes], self.order)
                rgb = bytes(((pixel & mask) >> shift) * 255 // maximum
                            for mask, shift, maximum in zip(self.masks, shifts, maxima))
                line += rgb * scale
            for _ in range(scale):
                yield bytes(line)

    def write_png(self, path: Path, scale: int = 2) -> None:
        def chunk(kind: bytes, payload: bytes) -> bytes:
            body = kind + payload
            return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))

        raw = b"".join(b"\0" + row for row in self.rgb_rows(scale))
        header = struct.pack(">IIBBBBB", self.width * scale, self.height * scale, 8, 2, 0, 0, 0)
        path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
                         + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))

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
    def __init__(self, directory: Path, terminal: subprocess.Popen,
                 screenshots: Path | None) -> None:
        self.directory = directory
        self.terminal = terminal
        self.screenshots = screenshots
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

    def key(self, *keys: str) -> None:
        xdo("key", "--window", self.window, "--clearmodifiers", "--delay", "40", *keys)

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

    def shot(self, name: str, frame: Frame) -> None:
        if self.screenshots is not None:
            self.screenshots.mkdir(parents=True, exist_ok=True)
            frame.write_png(self.screenshots / f"{name}.png")

    def last_row(self, frame: Frame, right_pane: bool | float,
                 rows: int = 1) -> tuple[int, int, int, int]:
        """Last `rows` cell rows of the window, of the right pane after a
        split (True), or of the pane starting at that fraction of the width."""
        cell_height = frame.height // ROWS
        fraction = 0.5 if right_pane is True else float(right_pane)
        left = int(frame.width * fraction) + DIVIDER_THICKNESS if fraction else 0
        return (left, (ROWS - rows) * cell_height, frame.width, ROWS * cell_height)

    def wait_for_strip(self, present: bool, right_pane: bool | float,
                       description: str, rows: int = 1) -> Frame:
        """`rows` > 1 looks for the strip in the last rows of the window: a
        pane below a horizontal divider ends a few pixels above the bottom."""
        def matches():
            frame = self.capture()
            region = self.last_row(frame, right_pane, rows)
            area = (region[2] - region[0]) * (region[3] - region[1])
            chalk = frame.count(CHALK, region)
            self.last_mismatch = f"{chalk} chalk pixels of {area} in the last rows {region}"
            if present:
                return frame if chalk >= area * 0.6 / rows else None
            return frame if chalk <= area * 0.1 / rows else None

        return self.wait(matches, description)

    def type_line(self, text: str) -> None:
        xdo("type", "--window", self.window, "--delay", "20", "--", text)
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

    def type_text(self, text: str) -> None:
        xdo("type", "--window", self.window, "--delay", "20", "--", text)

    def exercise_search(self) -> dict:
        marker = self.directory / "search-marker"
        self.type_line("echo kokuban-needle-in-a-long-haystack")
        self.key("ctrl+shift+f")
        self.wait_for_strip(True, True, "the search strip")
        self.type_text("kokuban-needle")
        found = self.wait_for_strip(True, True, "the strip with matches")
        found_red = found.count(HANKO_RED, self.last_row(found, True))
        self.shot("01-busca-com-resultado", found)
        self.type_text("zz")

        def missing():
            frame = self.capture()
            red = frame.count(HANKO_RED, self.last_row(frame, True))
            self.last_mismatch = f"{red} hanko-red pixels in the strip"
            return (red, frame) if red > found_red else None

        miss_red, miss = self.wait(missing, "the hanko-red 'no matches' counter")
        self.shot("02-sem-resultados", miss)

        # A query wider than the pane keeps its end, after an ellipsis.
        self.key("BackSpace", "BackSpace")
        self.type_text("-in-a-long-haystack")

        def long_query():
            frame = self.capture()
            red = frame.count(HANKO_RED, self.last_row(frame, True))
            self.last_mismatch = f"{red} hanko-red pixels in the strip"
            return frame if red <= found_red else None

        self.shot("03-consulta-longa", self.wait(long_query, "the long query to match"))
        self.wait_for_strip(True, True, "the strip with a long query")
        self.key("Escape")
        self.wait_for_strip(False, True, "the strip to close after Escape")
        # A leaked query would turn this into `kokuban-needlezzecho ...`.
        self.type_line(f"echo ok > {marker}")
        self.wait(lambda: marker.is_file() and marker.read_text().strip() == "ok",
                  "the shell to run the command typed after the search")
        return {"strip_red_with_matches": found_red, "strip_red_without_matches": miss_red}

    def exercise_narrow_pane(self, shells: list[int]) -> None:
        """Split the right pane again and search in the narrow pane."""
        self.key("ctrl+shift+d")
        narrow = next(pid for pid in self.wait_for_shells(3, "the narrow pane's shell")
                      if pid not in shells)
        self.type_line("echo needle")
        self.key("ctrl+shift+f")
        self.wait_for_strip(True, 0.75, "the search strip in the narrow pane")
        self.type_text("needle")
        self.shot("04-painel-estreito",
                  self.wait_for_strip(True, 0.75, "the narrow strip with matches"))
        self.key("Escape")
        self.wait_for_strip(False, 0.75, "the narrow strip to close")
        # An idle shell closes at once and the focus moves to the first
        # pane; Ctrl+Shift+L brings it back to the right one.
        self.key("ctrl+shift+w")
        if self.wait_for_shells(2, "the narrow pane's shell to be reaped") != shells:
            raise AssertionError(f"closing the narrow pane kept {narrow}")
        self.wait_for_divider(True)
        self.key("ctrl+shift+l")

    def exercise_close_confirmation(self, first_shell: int) -> dict:
        self.key("ctrl+shift+d")
        shells = self.wait_for_shells(2, "the second split pane's shell")
        shell = next(pid for pid in shells if pid != first_shell)
        self.wait_for_divider(True)
        search = self.exercise_search()
        self.exercise_narrow_pane(shells)

        self.type_line("sleep 60")
        job = self.wait_for_job(shell, "sleep")
        self.key("ctrl+shift+w")
        self.shot("05-fechar-um-job",
                  self.wait_for_strip(True, True, "the close prompt over the busy pane"))
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
        # A second job in a pane below, so the prompt names both.
        self.key("ctrl+shift+e")
        lower = next(pid for pid in self.wait_for_shells(2, "the lower pane's shell")
                     if pid != shell)
        self.type_line("tail -f /dev/null")
        tail = self.wait_for_job(lower, "tail")

        def both_running() -> bool:
            return (self.terminal.poll() is None and job in child_pids(shell)
                    and tail in child_pids(lower))

        send_wm_delete_window(int(self.window))
        self.shot("06-fechar-dois-jobs",
                  self.wait_for_strip(True, False, "the window close prompt", 2))
        self.stays(both_running, "the window closed without asking")
        self.key("Escape")
        self.wait_for_strip(False, False, "the window prompt to close after Escape", 2)
        self.stays(both_running, "cancelling ended a job")

        # Enter answers the default N of [y/N]: the job keeps running.
        send_wm_delete_window(int(self.window))
        self.wait_for_strip(True, False, "the window close prompt again", 2)
        self.key("Return")
        self.wait_for_strip(False, False, "the window prompt to close after Enter", 2)
        self.stays(both_running, "Enter closed the window or ended a job")

        send_wm_delete_window(int(self.window))
        self.wait_for_strip(True, False, "the window close prompt a third time", 2)
        subprocess.run(
            ["xdotool", "key", "--window", self.window, "--clearmodifiers", "y"],
            capture_output=True, timeout=3, check=False,
        )
        try:
            status = self.terminal.wait(timeout=8)
        except subprocess.TimeoutExpired as error:
            raise AssertionError("confirming the window close did not exit") from error
        if status != 0:
            raise AssertionError(f"terminal failed after the confirmed window close: {status}")
        return {"result": "passed", "shells": [shell, lower]}

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


def run_session(binary: Path, directory: Path, name: str, scenario,
                screenshots: Path | None) -> dict:
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
        smoke = PanesSmoke(directory, terminal, screenshots)
        try:
            return scenario(smoke)
        except Exception as error:
            raise AssertionError(
                f"Linux panes smoke ({name}): {error}\n"
                f"{log_path.read_text(errors='replace')[-4000:]}"
            ) from error
        finally:
            smoke.cleanup()


def check(binary: Path, screenshots: Path | None = None) -> dict:
    with tempfile.TemporaryDirectory(prefix="kokuban-panes-") as temporary:
        directory = Path(temporary)
        (directory / "kokuban.toml").write_text(
            '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
            f"[window]\ncolumns = {COLUMNS}\nrows = {ROWS}\n"
            "[omarchy]\nenabled = false\n",
            encoding="utf-8",
        )
        report = run_session(binary, directory, "panes", PanesSmoke.exercise, screenshots)
        report["window_close"] = run_session(
            binary, directory, "window-close", PanesSmoke.exercise_window_close, screenshots
        )
        return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--screenshots", type=Path,
                        help="directory for PNGs of the strips and prompts")
    arguments = parser.parse_args()
    report = check(arguments.binary.resolve(strict=True), arguments.screenshots)
    encoded = json.dumps(report, indent=2) + "\n"
    if arguments.output:
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(encoded)
    print(encoded, end="")
