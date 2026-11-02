#!/usr/bin/env python3
"""Split and close panes in a real Linux window under xvfb-run.

Requires xdotool and xwd, and only the Python standard library. Kokuban
starts with `-e /bin/sh`; `Ctrl+Shift+D` must spawn a second shell (a new
child of the terminal process) and present a hanko-red vertical divider in
the middle of the window; `Ctrl+Shift+W` must close that pane, reap its
shell and remove the divider while the window stays alive; a last
`Ctrl+Shift+W` closes the original pane and the terminal exits cleanly.
"""

import argparse
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

        # The window closes on the key press, so the key-up of this last chord
        # can fail to reach it; the clean exit status is the assertion.
        subprocess.run(
            ["xdotool", "key", "--window", self.window, "--clearmodifiers", "--delay", "40",
             "ctrl+shift+w"],
            capture_output=True, timeout=3, check=False,
        )
        try:
            status = self.terminal.wait(timeout=8)
        except subprocess.TimeoutExpired as error:
            raise AssertionError("closing the last pane did not exit the terminal") from error
        if status != 0:
            raise AssertionError(f"terminal failed after the last pane closed: {status}")
        return {
            "result": "passed",
            "first_shell": first_shell,
            "second_shell": second_shell,
            "frame": [split.width, split.height],
            "divider_pixels": self.divider_pixels(split),
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


def check(binary: Path) -> dict:
    with tempfile.TemporaryDirectory(prefix="kokuban-panes-") as temporary:
        directory = Path(temporary)
        (directory / "kokuban.toml").write_text(
            '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
            f"[window]\ncolumns = {COLUMNS}\nrows = {ROWS}\n"
            "[omarchy]\nenabled = false\n",
            encoding="utf-8",
        )
        environment = os.environ.copy()
        environment.pop("KOKUBAN_EXIT_AFTER_FIRST_FRAME", None)
        for key in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR"):
            environment.pop(key, None)
        environment["WINIT_X11_SCALE_FACTOR"] = "1"
        environment["RUST_LOG"] = "warn"
        log_path = directory / "terminal.log"
        with log_path.open("wb") as log:
            terminal = subprocess.Popen(
                [str(binary), "-e", "/bin/sh"],
                cwd=directory, env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
            )
            smoke = PanesSmoke(directory, terminal)
            try:
                return smoke.exercise()
            except Exception as error:
                raise AssertionError(
                    f"Linux panes smoke: {error}\n{log_path.read_text(errors='replace')[-4000:]}"
                ) from error
            finally:
                smoke.cleanup()


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
