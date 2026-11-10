#!/usr/bin/env python3
"""Exercise Kokuban on native Wayland under a private headless sway.

Requires sway (wlroots headless backend), wl-clipboard (wl-copy/wl-paste) and
the test injector built from scripts/wayland-inject (`cargo build` there).
Optional: grim, for screenshots in --artifacts-dir. Uses only the Python
standard library and never touches the user's own Wayland session.

Checks, at output scale 1.25 unless noted:
- the glyph atlas follows the fractional scale (wp_fractional_scale_v1);
- SGR mouse reports name the cell under the pointer (first, middle, last);
- Ctrl+Shift+V pastes CLIPBOARD from another client, bracketed and exact;
- a drag selection becomes PRIMARY and Ctrl+Shift+C fills CLIPBOARD,
  without sending anything to the PTY;
- the middle button pastes PRIMARY;
- text-input-v3 is enabled with purpose terminal, an input-method-v2 preedit
  never reaches the PTY and its commit arrives exactly once;
- runtime scale changes to 1.5 and 1.0 rebuild the atlas and keep pointer
  mapping on the last cell.

usage: linux-wayland-smoke.py KOKUBAN --inject WAYLAND_INJECT [--artifacts-dir DIR]
"""

import argparse
import json
import os
from pathlib import Path
import re
import select
import shutil
import subprocess
import sys
import tempfile
import time
import tty


DSR = re.compile(rb"\x1b\[([0-9]+);([0-9]+)R")
FIXTURE = b"\x1b[2J\x1b[HPRIMARY_SOURCE_TEXT tail\r\nsecond line\r\n"
MODES = {
    "fixture": FIXTURE + b"\x1b[?2004h",
    "mouse-on": b"\x1b[?1000h\x1b[?1006h",
    "mouse-off": b"\x1b[?1000l\x1b[?1006l",
    "size": b"",
}
SWAY_CONFIG = """output HEADLESS-1 resolution 1600x1000 scale 1.25
xwayland disable
default_border none
focus_follows_mouse no
"""


def child(directory: Path) -> None:
    """Raw PTY child: records input and applies modes the harness requests.

    The harness writes cmd-N; the child applies that mode, asks for a DSR,
    removes the reply from the recorded input and writes ack-N with its size.
    """
    fd = sys.stdin.fileno()
    tty.setraw(fd)
    os.write(1, b"\x1b[?25l")
    handled = 0
    pending = None
    buffer = bytearray()
    with (directory / "input.bin").open("ab", buffering=0) as captured:
        while not (directory / "stop").exists():
            if pending is None:
                command = directory / f"cmd-{handled + 1}"
                if command.exists():
                    handled += 1
                    pending = handled
                    os.write(1, MODES[command.read_text().strip()] + b"\x1b[6n")
            if not select.select([fd], [], [], 0.02)[0]:
                continue
            data = os.read(fd, 4096)
            if not data:
                break
            if pending is None:
                captured.write(data)
                continue
            buffer.extend(data)
            reply = DSR.search(buffer)
            if reply is None:
                continue
            cursor = [int(reply.group(1)), int(reply.group(2))]
            captured.write(bytes(buffer[:reply.start()] + buffer[reply.end():]))
            buffer.clear()
            size = os.get_terminal_size(fd)
            ack = directory / f"ack-{pending}"
            temporary = ack.with_suffix(".tmp")
            temporary.write_text(json.dumps(
                {"columns": size.columns, "rows": size.lines, "cursor": cursor}))
            temporary.replace(ack)
            pending = None


def wait_for(condition, timeout: float = 6.0, step: float = 0.05):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = condition()
        if value:
            return value
        time.sleep(step)
    return None


class Sway:
    """A private headless sway; its runtime directory isolates every client."""

    def __init__(self, runtime: Path, log) -> None:
        self.runtime = runtime
        config = runtime / "sway.config"
        config.write_text(SWAY_CONFIG)
        environment = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS="headless",
                           WLR_RENDERER="pixman", WLR_LIBINPUT_NO_DEVICES="1")
        for name in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY", "SWAYSOCK"):
            environment.pop(name, None)
        self.process = subprocess.Popen(["sway", "-c", str(config)], env=environment,
                                        stdout=log, stderr=log, stdin=subprocess.DEVNULL)
        sockets = wait_for(lambda: list(runtime.glob("sway-ipc.*.sock"))
                           and list(runtime.glob("wayland-*[0-9]")), timeout=10)
        if not sockets or self.process.poll() is not None:
            raise AssertionError("headless sway did not start")
        display = sorted(path.name for path in runtime.glob("wayland-*") if path.is_socket())[0]
        self.environment = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=display,
                                SWAYSOCK=str(next(runtime.glob("sway-ipc.*.sock"))))
        for name in ("DISPLAY", "WAYLAND_SOCKET"):
            self.environment.pop(name, None)

    def message(self, *arguments: str) -> object:
        result = subprocess.run(["swaymsg", "-r", *arguments], capture_output=True, check=True,
                                env=self.environment, timeout=5)
        return json.loads(result.stdout) if result.stdout.strip() else None

    def output(self) -> dict:
        return self.message("-t", "get_outputs")[0]

    def stop(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)


class Injector:
    def __init__(self, binary: Path, extent: tuple[int, int], environment: dict, log) -> None:
        self.log = log
        self.process = subprocess.Popen(
            [str(binary), str(extent[0]), str(extent[1])], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=log, text=True, env=environment, bufsize=1)
        ready = self.process.stdout.readline()
        if not ready.startswith("ready"):
            raise AssertionError(f"wayland-inject did not start: {ready!r}")
        log.write(f"inject: {ready}".encode())

    def send(self, command: str) -> list[str]:
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()
        lines = []
        while True:
            line = self.process.stdout.readline()
            if not line:
                raise AssertionError(f"wayland-inject exited during {command!r}")
            self.log.write(f"inject: {line}".encode())
            lines.append(line.rstrip("\n"))
            if line.startswith("ok "):
                return lines

    def close(self) -> None:
        if self.process.stdin:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()


class WaylandSmoke:
    def __init__(self, binary: Path, sway: Sway, injector: Injector, directory: Path,
                 artifacts: Path | None, log) -> None:
        self.sway = sway
        self.inject = injector
        self.directory = directory
        self.artifacts = artifacts
        self.results: list[tuple[str, bool, str]] = []
        self.commands = 0
        self.offset = 0
        self.size: dict = {}
        self.rect: dict = {}
        (directory / "kokuban.toml").write_text(
            '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
            '[window]\ncolumns = 60\nrows = 20\n')
        environment = dict(sway.environment, RUST_LOG="info,font_kit=off,sctk_adwaita=off")
        environment.pop("KOKUBAN_EXIT_AFTER_FIRST_FRAME", None)
        self.log_path = directory / "terminal.log"
        self.terminal_log = self.log_path.open("wb")
        self.terminal = subprocess.Popen(
            [str(binary), "-e", sys.executable, str(Path(__file__).resolve()), "--child",
             str(directory)],
            cwd=directory, env=environment, stdin=subprocess.DEVNULL,
            stdout=self.terminal_log, stderr=self.terminal_log)
        self.log = log

    def record(self, name: str, ok: bool, detail: str = "") -> None:
        self.results.append((name, ok, detail))
        print(f"{'PASS' if ok else 'FAIL'} {name}: {detail}", flush=True)

    def command(self, name: str) -> dict:
        self.commands += 1
        (self.directory / f"cmd-{self.commands}").write_text(name)
        ack = self.directory / f"ack-{self.commands}"
        if not wait_for(ack.exists, timeout=8):
            raise AssertionError(f"no PTY acknowledgment for {name}")
        self.size = json.loads(ack.read_text())
        return self.size

    def find_window(self) -> dict | None:
        stack = [self.sway.message("-t", "get_tree")]
        while stack:
            node = stack.pop()
            if node.get("pid") == self.terminal.pid:
                return node["rect"]
            stack.extend(node.get("nodes", []) + node.get("floating_nodes", []))
        return None

    def window_rect(self) -> dict:
        rect = wait_for(self.find_window, timeout=5)
        if rect is None:
            raise AssertionError("Kokuban window not found in the sway tree")
        return rect

    def settled_size(self) -> dict:
        """PTY size once the tiled window and the grid agree for a while."""
        previous = None
        for _ in range(40):
            time.sleep(0.25)
            current = (self.command("size"), self.window_rect())
            if current == previous:
                self.rect = current[1]
                return current[0]
            previous = current
        raise AssertionError(f"window size did not settle: {previous}")

    def take_input(self, expected: bytes = b"", timeout: float = 4.0) -> bytes:
        path = self.directory / "input.bin"
        wait_for(lambda: path.exists() and path.stat().st_size - self.offset >= len(expected),
                 timeout=timeout)
        # Include late duplicates in the comparison.
        time.sleep(0.25)
        data = path.read_bytes() if path.exists() else b""
        fresh, self.offset = data[self.offset:], len(data)
        return fresh

    def cell_center(self, column: int, row: int) -> tuple[float, float]:
        width = self.rect["width"] / self.size["columns"]
        height = self.rect["height"] / self.size["rows"]
        return self.rect["x"] + (column + 0.5) * width, self.rect["y"] + (row + 0.5) * height

    def click(self, column: int, row: int, button: str = "left") -> None:
        x, y = self.cell_center(column, row)
        self.inject.send(f"move {x:.1f} {y:.1f}")
        self.inject.send(f"click {button}")

    def screenshot(self, name: str) -> None:
        if self.artifacts and shutil.which("grim"):
            subprocess.run(["grim", str(self.artifacts / name)], env=self.sway.environment,
                           capture_output=True, timeout=10)

    def paste(self, primary: bool) -> bytes:
        result = subprocess.run(["wl-paste", "--no-newline"] + (["--primary"] if primary else []),
                                capture_output=True, env=self.sway.environment, timeout=3)
        return result.stdout if result.returncode == 0 else b""

    def copy(self, text: str, primary: bool) -> None:
        # wl-copy forks a server that keeps ownership; it exits with sway.
        subprocess.run(["wl-copy"] + (["--primary"] if primary else []) + [text],
                       env=self.sway.environment, check=True, timeout=3,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if not wait_for(lambda: self.paste(primary) == text.encode(), timeout=3):
            raise AssertionError(f"wl-copy did not take {'PRIMARY' if primary else 'CLIPBOARD'}")

    def terminal_lines(self, needle: str) -> list[str]:
        self.terminal_log.flush()
        return [line for line in self.log_path.read_text(errors="replace").splitlines()
                if needle in line]

    def check_pointer(self, label: str, cells) -> None:
        self.command("mouse-on")
        self.take_input()
        for column, row in cells:
            self.click(column, row)
            report = f"\x1b[<0;{column + 1};{row + 1}".encode()
            expected = report + b"M" + report + b"m"
            actual = self.take_input(expected)
            self.record(f"pointer cell ({column},{row}) at {label}", actual == expected, repr(actual))
        self.command("mouse-off")
        self.take_input()

    def exercise(self) -> None:
        self.command("fixture")
        wait_for(lambda: self.terminal_lines("scale factor changed to 1.25"), timeout=5)
        size = self.settled_size()
        atlas = self.terminal_lines("scale=1.25")
        self.record("glyph atlas follows scale 1.25", bool(atlas),
                    f"pty {size['columns']}x{size['rows']}, window {self.rect}")
        self.screenshot("scale-1.25.png")
        self.check_pointer("scale 1.25", [(0, 0), (10, 5), (size["columns"] - 1, size["rows"] - 1)])

        self.copy("ação 🦀 clip", primary=False)
        self.inject.send("chord ctrl+shift+v")
        expected = b"\x1b[200~" + "ação 🦀 clip".encode() + b"\x1b[201~"
        actual = self.take_input(expected)
        self.record("Ctrl+Shift+V pastes CLIPBOARD", actual == expected, repr(actual))

        self.copy("clipboard sentinel", primary=False)
        self.copy("primary sentinel", primary=True)
        start, end = self.cell_center(0, 0), self.cell_center(18, 0)
        self.inject.send(f"move {start[0]:.1f} {start[1]:.1f}")
        self.inject.send("press left")
        self.inject.send(f"move {(start[0] + end[0]) / 2:.1f} {end[1]:.1f}")
        self.inject.send(f"move {end[0]:.1f} {end[1]:.1f}")
        self.inject.send("release left")
        selected = b"PRIMARY_SOURCE_TEXT"
        wait_for(lambda: self.paste(True) == selected, timeout=3)
        self.record("drag selection becomes PRIMARY", self.paste(True) == selected,
                    repr(self.paste(True)))
        self.screenshot("selection.png")
        self.inject.send("chord ctrl+shift+c")
        wait_for(lambda: self.paste(False) == selected, timeout=3)
        self.record("Ctrl+Shift+C fills CLIPBOARD", self.paste(False) == selected,
                    repr(self.paste(False)))
        stray = self.take_input()
        self.record("selection and copy send nothing to the PTY", stray == b"", repr(stray))

        self.copy("middle 中 primary", primary=True)
        self.click(5, 3, "middle")
        expected = b"\x1b[200~" + "middle 中 primary".encode() + b"\x1b[201~"
        actual = self.take_input(expected)
        self.record("middle button pastes PRIMARY", actual == expected, repr(actual))

        active = self.inject.send("wait-active 3000")
        self.record("text-input-v3 enabled for the focused window",
                    "wait-active: true" in active, active[-2])
        self.inject.send("preedit にほ 6 6")
        time.sleep(0.4)
        self.screenshot("ime-preedit.png")
        leaked = self.take_input(timeout=0.3)
        self.record("IME preedit stays out of the PTY", leaked == b"", repr(leaked))
        self.inject.send("commit 日本")
        expected = "日本".encode()
        actual = self.take_input(expected)
        self.record("IME commit reaches the PTY once", actual == expected, repr(actual))
        purposes = [line for line in self.log.getvalue().splitlines() if "content_type" in line] \
            if hasattr(self.log, "getvalue") else []
        if purposes:
            self.record("IME content purpose is terminal",
                        any("Terminal" in line for line in purposes), purposes[-1])

        for scale in ("1.5", "1"):
            self.sway.message("output", "HEADLESS-1", "scale", scale)
            output = self.sway.output()
            self.inject.send(f"extent {output['rect']['width']} {output['rect']['height']}")
            changed = wait_for(lambda: self.terminal_lines(f"scale factor changed to {scale}"),
                               timeout=5)
            size = self.settled_size()
            self.record(f"runtime scale {scale} rebuilds the atlas", bool(changed),
                        f"pty {size['columns']}x{size['rows']}, window {self.rect}")
            self.screenshot(f"scale-{scale}.png")
            self.check_pointer(f"scale {scale}", [(size["columns"] - 1, size["rows"] - 1)])

    def stop(self) -> None:
        (self.directory / "stop").touch()
        time.sleep(0.2)
        if self.terminal.poll() is None:
            self.terminal.terminate()
            try:
                self.terminal.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.terminal.kill()
                self.terminal.wait(timeout=5)
        self.terminal_log.close()


class TeeLog:
    """Binary log file that also keeps the injector lines for later checks."""

    def __init__(self, path: Path) -> None:
        self.file = path.open("wb")
        self.lines: list[str] = []

    def write(self, data: bytes) -> None:
        self.file.write(data)
        self.file.flush()
        self.lines.append(data.decode(errors="replace"))

    def fileno(self) -> int:
        return self.file.fileno()

    def getvalue(self) -> str:
        return "".join(self.lines)

    def close(self) -> None:
        self.file.close()


def main() -> int:
    if len(sys.argv) == 3 and sys.argv[1] == "--child":
        child(Path(sys.argv[2]))
        return 0
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("binary", type=Path)
    parser.add_argument("--inject", type=Path, required=True,
                        help="wayland-inject binary built from scripts/wayland-inject")
    parser.add_argument("--artifacts-dir", type=Path)
    arguments = parser.parse_args()
    for tool in ("sway", "swaymsg", "wl-copy", "wl-paste"):
        if not shutil.which(tool):
            raise SystemExit(f"missing required tool: {tool}")
    if arguments.artifacts_dir:
        arguments.artifacts_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="kokuban-wayland-") as temporary:
        root = Path(temporary)
        runtime, directory = root / "runtime", root / "terminal"
        runtime.mkdir(mode=0o700)
        directory.mkdir()
        log = TeeLog(root / "harness.log")
        sway = Sway(runtime, log)
        smoke = None
        injector = None
        try:
            output = sway.output()
            injector = Injector(arguments.inject.resolve(strict=True),
                                (output["rect"]["width"], output["rect"]["height"]),
                                sway.environment, log)
            smoke = WaylandSmoke(arguments.binary.resolve(strict=True), sway, injector,
                                 directory, arguments.artifacts_dir, log)
            smoke.exercise()
        except Exception as error:  # report, then clean up and fail
            if smoke:
                smoke.record("harness", False, repr(error))
            else:
                print(f"FAIL harness: {error!r}", flush=True)
                return 1
        finally:
            if smoke:
                smoke.stop()
            if injector:
                injector.close()
            sway.stop()
            log.close()
            if arguments.artifacts_dir:
                for path in (root / "harness.log", directory / "terminal.log"):
                    if path.exists():
                        shutil.copy(path, arguments.artifacts_dir / path.name)
        failed = [name for name, ok, _ in smoke.results if not ok]
        report = {"result": "failed" if failed else "passed", "failed": failed,
                  "checks": [{"name": name, "ok": ok, "detail": detail}
                             for name, ok, detail in smoke.results]}
        if arguments.artifacts_dir:
            (arguments.artifacts_dir / "report.json").write_text(
                json.dumps(report, ensure_ascii=False, indent=2))
        print(json.dumps({"result": report["result"], "failed": failed}, ensure_ascii=False))
        return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
