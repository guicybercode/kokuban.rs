#!/usr/bin/env python3
"""Verify live Omarchy colors in a real X11 window under xvfb-run.

Requires xdotool and xwd, and only the Python standard library. The PTY child
prints its fixture once. Theme reload must update an idle window, including an
existing selection, before any subsequent terminal output or input is sent.
"""

import argparse
import json
import os
from pathlib import Path
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import time
import tty


DSR_REPLY = re.compile(rb"\x1b\[([0-9]+);([0-9]+)R")
OSC_REPLY = re.compile(
    rb"\x1b\](10|11);rgb:([0-9a-fA-F]{4})/([0-9a-fA-F]{4})/([0-9a-fA-F]{4})(?:\x07|\x1b\\)"
)
SELECTION = "SELECT_ME      "
INPUT = b"theme-session-survived"
MAX_INPUT = 64 * 1024


def palette(second: bool = False) -> dict[str, str]:
    result = dict(zip(
        ("foreground", "background", "cursor", "selection_foreground", "selection_background"),
        ("#bfe6da", "#2e1634", "#54b8f3", "#351424", "#eed267") if second else
        ("#e4d7c1", "#132538", "#fe921d", "#122144", "#7edaad"),
    ))
    for index in range(16):
        channels = ((37 + 13 * index + 83 * second) % 256,
                    (61 + 29 * index + 47 * second) % 256,
                    (89 + 17 * index + 101 * second) % 256)
        result[f"color{index}"] = "#{:02x}{:02x}{:02x}".format(*channels)
    return result


def atomic_text(path: Path, value: str) -> None:
    temporary = path.with_name(path.name + ".pending")
    temporary.write_text(value, encoding="utf-8")
    temporary.replace(path)


def identity(pid: int) -> str:
    """Use the Linux start time to distinguish a living process from PID reuse."""
    return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]


def fixture() -> bytes:
    content = "\x1b[?25l\x1b[0m\x1b[2J\x1b[HMMMMMMMMMM\x1b[2;1H" + SELECTION
    for index in range(16):
        foreground = 30 + index if index < 8 else 90 + index - 8
        background = foreground + 10
        row, column = 4 + index // 8, 1 + 4 * (index % 8)
        content += f"\x1b[{row};{column}H\x1b[{foreground}m██\x1b[{background}m  \x1b[0m"
    content += ("\x1b[7;1H\x1b[48;5;196m    \x1b[0m "
                "\x1b[48;2;17;102;187m    \x1b[0m\x1b[9;2H\x1b[2 q\x1b[?25h")
    return content.encode("utf-8")


def child() -> None:
    directory = Path.cwd()
    tty.setraw(0)
    atomic_text(directory / "child.json", json.dumps({
        "pid": os.getpid(), "identity": identity(os.getpid()), "parent_pid": os.getppid(),
    }))
    os.write(1, fixture())
    phase = None
    pending = None
    response = bytearray()
    captured = 0
    deadline = time.monotonic() + 70
    with (directory / "input.bin").open("wb", buffering=0) as incoming_file:
        while not (directory / "stop").exists():
            if time.monotonic() > deadline:
                raise AssertionError("theme smoke driver timed out")
            requested = (directory / "phase").read_text()
            if requested != phase:
                if pending is not None:
                    raise AssertionError("probe changed before its replies arrived")
                phase = pending = requested
                os.write(1, b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[6n")
            if not select.select([0], [], [], 0.02)[0]:
                continue
            incoming = os.read(0, 4096)
            if not incoming:
                break
            captured += len(incoming)
            if captured > MAX_INPUT:
                raise AssertionError("unexpected excessive PTY input")
            if pending is None:
                incoming_file.write(incoming)
                continue
            response.extend(incoming)
            cursor = DSR_REPLY.search(response)
            colors = list(OSC_REPLY.finditer(response))
            if cursor is None or {match[1] for match in colors} != {b"10", b"11"}:
                continue
            extras = DSR_REPLY.sub(b"", OSC_REPLY.sub(b"", response))
            incoming_file.write(extras)
            size = os.get_terminal_size(0)
            atomic_text(directory / "ready.json", json.dumps({
                "phase": pending, "columns": size.columns, "rows": size.lines,
                "cursor": [int(value) for value in cursor.groups()],
                "colors": {match[1].decode(): [int(value, 16) for value in match.groups()[1:]]
                           for match in colors},
                "pid": os.getpid(), "identity": identity(os.getpid()),
            }))
            response.clear()
            pending = None


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

    def count(self, color: str, region: tuple[int, int, int, int]) -> int:
        rgb = bytes.fromhex(color.removeprefix("#"))
        desired = 0
        for component, mask in zip(rgb, self.masks):
            shift = (mask & -mask).bit_length() - 1
            desired |= (component * (mask >> shift) // 255) << shift
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


class ThemeSmoke:
    def __init__(self, directory: Path, terminal: subprocess.Popen, artifacts: Path | None) -> None:
        self.directory = directory
        self.terminal = terminal
        self.artifacts = artifacts
        self.window = ""
        self.probes = 0
        self.cell_width = self.cell_height = 0
        self.child = None
        self.last_mismatch = ""

    def wait(self, condition, description: str, timeout: float = 8):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.terminal.poll() is not None:
                raise AssertionError(f"terminal exited early: {self.terminal.returncode}")
            result = condition()
            if result:
                return result
            time.sleep(0.03)
        raise AssertionError(f"timed out waiting for {description}: {self.last_mismatch}")

    def probe(self) -> dict:
        self.probes += 1
        phase = str(self.probes)
        atomic_text(self.directory / "phase", phase)

        def ready():
            path = self.directory / "ready.json"
            if path.exists():
                result = json.loads(path.read_text())
                if result["phase"] == phase:
                    return result
            return None

        result = self.wait(ready, "OSC10/11 and cursor replies")
        if result["cursor"] != [9, 2]:
            raise AssertionError(f"theme reload changed the cursor/content: {result}")
        if self.child is None:
            self.child = json.loads((self.directory / "child.json").read_text())
            if self.child["parent_pid"] != self.terminal.pid:
                raise AssertionError(f"unexpected PTY parent: {self.child}")
        if (result["pid"] != self.child["pid"] or result["identity"] != self.child["identity"]
                or identity(result["pid"]) != self.child["identity"]):
            raise AssertionError("theme reload replaced the PTY child")
        return result

    def check_defaults(self, expected: dict[str, str]) -> None:
        status = self.probe()
        colors = {number: [component * 257 for component in bytes.fromhex(expected[field][1:])]
                  for number, field in (("10", "foreground"), ("11", "background"))}
        if status["colors"] != colors:
            raise AssertionError(f"OSC defaults: expected {colors}, got {status['colors']}")

    def locate(self) -> None:
        def search():
            found = subprocess.run(
                ["xdotool", "search", "--onlyvisible", "--pid", str(self.terminal.pid)],
                capture_output=True, timeout=2,
            )
            return found.stdout.decode().splitlines() if found.returncode == 0 else None

        self.window = self.wait(search, "Kokuban X11 window")[0]
        xdo("windowfocus", "--sync", self.window)
        # Do not replace phase zero while the initial OSC/DSR bytes are still
        # arriving: a visible window alone does not acknowledge the PTY fixture.
        self.wait(lambda: (self.directory / "ready.json").exists(), "initial PTY replies")
        status = self.probe()
        geometry = dict(line.split("=", 1) for line in xdo(
            "getwindowgeometry", "--shell", self.window).decode().splitlines() if "=" in line)
        self.cell_width = int(geometry["WIDTH"]) // status["columns"]
        self.cell_height = int(geometry["HEIGHT"]) // status["rows"]
        if (status["columns"] < 32 or status["rows"] < 12
                or self.cell_width < 4 or self.cell_height < 4):
            raise AssertionError(f"invalid fixture geometry: {status}, {geometry}")

    def region(self, column: int, row: int, columns: int = 1) -> tuple[int, int, int, int]:
        return (column * self.cell_width, row * self.cell_height,
                (column + columns) * self.cell_width, (row + 1) * self.cell_height)

    def capture(self) -> Frame:
        screenshot = self.directory / "frame.xwd"
        run("xwd", "-id", self.window, "-silent", "-out", str(screenshot))
        return Frame(screenshot.read_bytes())

    def matches(self, expected: dict[str, str], selected: bool) -> bool:
        frame = self.capture()
        area = self.cell_width * self.cell_height
        checks = [("foreground", self.region(0, 0, 10), 10),
                  ("background", self.region(0, 10, 10), area * 9),
                  ("cursor", self.region(1, 8), area // 2)]
        if selected:
            checks.extend([("selection_foreground", self.region(0, 1, 9), 5),
                           ("selection_background", self.region(10, 1, 3), area * 2)])
        for index in range(16):
            # Check both foreground glyph ink and the solid background half.
            column, row = (index % 8) * 4, 3 + index // 8
            checks.extend([(f"color{index}", self.region(column, row, 2), 5),
                           (f"color{index}", self.region(column + 2, row, 2), area)])
        for field, region, minimum in checks:
            actual = frame.count(expected[field], region)
            if actual < minimum:
                self.last_mismatch = f"{field} {expected[field]} at {region}: {actual} pixels < {minimum}"
                return False
        for color, region in (("#ff0000", self.region(0, 6, 4)),
                              ("#1166bb", self.region(5, 6, 4))):
            if frame.count(color, region) < area * 3:
                self.last_mismatch = f"indexed/truecolor fixture changed: {color} at {region}"
                return False
        return True

    def check_frame(self, expected: dict[str, str], label: str, selected: bool = True) -> None:
        # XWD reads pixels; it sends no input, resize, expose request or PTY output.
        self.wait(lambda: self.matches(expected, selected), f"presented {label} palette")
        if self.artifacts:
            shutil.copyfile(self.directory / "frame.xwd", self.artifacts / f"{label}.xwd")

    def select(self) -> None:
        y = str(self.cell_height + self.cell_height // 2)
        xdo("mousemove", "--sync", "--window", self.window, str(self.cell_width // 2), y,
            "mousedown", "1", "sleep", "0.05", "mousemove", "--sync", "--window", self.window,
            str((len(SELECTION) - 1) * self.cell_width + self.cell_width // 2), y,
            "sleep", "0.05", "mouseup", "1")

    def replace_theme(self, contents: str) -> None:
        current = self.directory / "home/.config/omarchy/current"
        replacement = current / "theme.next"
        replacement.mkdir()
        (replacement / "colors.toml").write_text(contents, encoding="utf-8")
        # Omarchy removes the current theme directory and replaces it. The watch
        # must belong to the stable parent, not to the removed colors.toml inode.
        shutil.rmtree(current / "theme")
        replacement.rename(current / "theme")

    def retain_invalid(self, expected: dict[str, str], contents: str, label: str) -> None:
        self.replace_theme(contents)
        deadline = time.monotonic() + 0.6
        while time.monotonic() < deadline:
            if self.terminal.poll() is not None or not self.matches(expected, True):
                raise AssertionError(f"{label} theme changed the frame: {self.last_mismatch}")
            time.sleep(0.03)
        self.check_frame(expected, label)
        self.check_defaults(expected)

    def exercise(self, overrides: bool) -> dict:
        first, second = palette(), palette(True)
        expected_second = first if overrides else second
        self.locate()
        self.check_frame(first, "initial", selected=False)
        self.check_defaults(first)
        self.select()
        self.check_frame(first, "selected")
        self.replace_theme(theme_text(second))
        self.check_frame(expected_second, "reloaded")
        self.check_defaults(expected_second)
        self.retain_invalid(expected_second, 'foreground = "#ffffff"\n', "incomplete")
        self.retain_invalid(expected_second, theme_text(second).replace(
            'cursor = "#54b8f3"', 'cursor = "#badhex"'), "invalid")
        # A later valid replacement proves the observer survives invalid files.
        self.replace_theme(theme_text(first))
        self.check_frame(first, "recovered")
        self.check_defaults(first)
        input_file = self.directory / "input.bin"
        if input_file.read_bytes() != b"":
            raise AssertionError(f"unexpected PTY bytes before input: {input_file.read_bytes()!r}")
        xdo("type", "--clearmodifiers", "--delay", "1", INPUT.decode())
        self.wait(lambda: len(input_file.read_bytes()) >= len(INPUT), "live child keyboard input")
        self.check_defaults(first)
        if input_file.read_bytes() != INPUT:
            raise AssertionError(f"PTY input changed: {input_file.read_bytes()!r}")
        self.check_frame(first, "input-survived", selected=False)
        return {"result": "passed", "explicit_overrides": overrides, "child": self.child,
                "probes": self.probes, "input_hex": INPUT.hex()}

    def cleanup(self) -> None:
        (self.directory / "stop").touch()
        if self.window:
            try:
                xdo("mouseup", "1", "keyup", "Control_L", "Shift_L")
            except (OSError, subprocess.SubprocessError):
                pass
        path = self.directory / "child.json"
        if path.exists():
            child = json.loads(path.read_text())
            try:
                if identity(child["pid"]) == child["identity"]:
                    os.kill(child["pid"], signal.SIGTERM)
            except (FileNotFoundError, ProcessLookupError):
                pass
        if self.terminal.poll() is None:
            self.terminal.terminate()
            try:
                self.terminal.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.terminal.kill()
                self.terminal.wait(timeout=2)


def theme_text(colors: dict[str, str]) -> str:
    return "".join(f'{name} = "{value}"\n' for name, value in colors.items())


def check(binary: Path, overrides: bool, artifacts: Path | None) -> dict:
    with tempfile.TemporaryDirectory(prefix="kokuban-theme-") as temporary:
        directory = Path(temporary)
        home = directory / "home"
        theme = home / ".config/omarchy/current/theme"
        theme.mkdir(parents=True)
        colors = palette()
        (theme / "colors.toml").write_text(theme_text(colors), encoding="utf-8")
        xdg = directory / "xdg"
        (xdg / "kokuban").mkdir(parents=True)
        config = ('[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
                  '[window]\ncolumns = 40\nrows = 16\n')
        if overrides:
            config += ('[colors]\n' + theme_text({name: colors[name] for name in
                       ("foreground", "background", "cursor")})
                       + "ansi = " + json.dumps([colors[f"color{i}"] for i in range(16)])
                       + '\n[selection]\nforeground = "' + colors["selection_foreground"]
                       + '"\nbackground = "' + colors["selection_background"] + '"\n')
        (xdg / "kokuban/kokuban.toml").write_text(config, encoding="utf-8")
        atomic_text(directory / "phase", "0")
        environment = os.environ.copy()
        for name in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR",
                     "KOKUBAN_EXIT_AFTER_FIRST_FRAME", "KOKUBAN_SHELL"):
            environment.pop(name, None)
        environment.update(HOME=str(home), XDG_CONFIG_HOME=str(xdg), WINIT_X11_SCALE_FACTOR="1")
        if artifacts:
            artifacts = artifacts / ("overrides" if overrides else "automatic")
            artifacts.mkdir(parents=True, exist_ok=True)
        log_path = directory / "terminal.log"
        with log_path.open("wb") as log:
            terminal = subprocess.Popen(
                [str(binary), "-e", sys.executable, str(Path(__file__).resolve()), "--child"],
                cwd=directory, env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
            )
            smoke = ThemeSmoke(directory, terminal, artifacts)
            try:
                result = smoke.exercise(overrides)
                (directory / "stop").touch()
                if terminal.wait(timeout=5) != 0:
                    raise AssertionError(f"terminal failed after child exit: {terminal.returncode}")
                return result
            except Exception as error:
                raise AssertionError(f"Linux theme smoke (overrides={overrides}): {error}\n"
                                     + log_path.read_text(errors="replace")[-4000:]) from error
            finally:
                smoke.cleanup()
                if artifacts:
                    shutil.copyfile(log_path, artifacts / "terminal.log")
                    if (directory / "frame.xwd").exists():
                        shutil.copyfile(directory / "frame.xwd", artifacts / "last-frame.xwd")


def main() -> None:
    if sys.argv[1:] == ["--child"]:
        child()
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--artifacts-dir", type=Path)
    arguments = parser.parse_args()
    binary = arguments.binary.resolve(strict=True)
    results = [check(binary, overrides, arguments.artifacts_dir) for overrides in (False, True)]
    encoded = json.dumps(results, indent=2) + "\n"
    if arguments.artifacts_dir:
        (arguments.artifacts_dir / "report.json").write_text(encoded, encoding="utf-8")
    print(encoded, end="")


if __name__ == "__main__":
    main()
