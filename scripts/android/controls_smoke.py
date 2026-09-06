#!/usr/bin/env python3
"""Verify native Android controls against bytes read from the real terminal PTY.

Uses Android input injection and native accessibility bounds, not USB hardware.
The staged shell fixture only advances when the harness releases a stage; actual
key/clipboard results are written by dd reading the application's PTY. Requires
a debuggable APK with opt-in geometry tracing. No third-party Python packages.
"""

import argparse
from dataclasses import dataclass
import json
from pathlib import Path
import re
import shlex
import time
import xml.etree.ElementTree as ET

from device import Device
from ime_smoke import center, find_node, node_bounds
from smoke import eventually


COPY_TEXT = "KOKUBAN-COPY-42"
TOOLBAR_KEYS = [
    ("Escape", b"\x1b"), ("Tab", b"\t"),
    ("Left arrow", b"\x1b[D"), ("Down arrow", b"\x1b[B"),
    ("Up arrow", b"\x1b[A"), ("Right arrow", b"\x1b[C"),
    ("Control modifier", b""), ("Left arrow", b"\x1b[1;5D"),
    ("Left arrow", b"\x1b[D"), ("Page up", b"\x1b[5~"),
    ("Page down", b"\x1b[6~"), ("Home", b"\x1b[H"),
    ("End", b"\x1b[F"), ("Insert", b"\x1b[2~"), ("Delete", b"\x1b[3~"),
]
EXTERNAL_KEYS = [
    ("MOVE_HOME", b"\x1b[H"), ("MOVE_END", b"\x1b[F"),
    ("PAGE_UP", b"\x1b[5~"), ("PAGE_DOWN", b"\x1b[6~"),
    ("INSERT", b"\x1b[2~"), ("FORWARD_DEL", b"\x1b[3~"),
    ("DEL", b"\x7f"), ("TAB", b"\t"), ("ESCAPE", b"\x1b"),
    ("DPAD_LEFT", b"\x1b[D"), ("DPAD_DOWN", b"\x1b[B"),
    ("DPAD_UP", b"\x1b[A"), ("DPAD_RIGHT", b"\x1b[C"),
    ("F1", b"\x1bOP"), ("F2", b"\x1bOQ"), ("F3", b"\x1bOR"), ("F4", b"\x1bOS"),
    ("F5", b"\x1b[15~"), ("F6", b"\x1b[17~"), ("F7", b"\x1b[18~"),
    ("F8", b"\x1b[19~"), ("F9", b"\x1b[20~"), ("F10", b"\x1b[21~"),
    ("F11", b"\x1b[23~"), ("F12", b"\x1b[24~"), ("ENTER", b"\r"),
]
MODIFIER_KEYS = [
    (["CTRL_LEFT", "A"], b"\x01"),
    (["SHIFT_LEFT", "TAB"], b"\x1b[Z"),
    (["ALT_LEFT", "X"], b"\x1bx"),
    (["CTRL_LEFT", "SHIFT_LEFT", "DPAD_UP"], b"\x1b[1;6A"),
]
MOUSE_BYTES = b"\x1b[<0;3;3M\x1b[<0;3;3m"
GEOMETRY_PATTERN = re.compile(
    r"metric geometry left=(\d+) top=(\d+) right=(\d+) bottom=(\d+) "
    r"cell_width=(\d+) cell_height=(\d+) columns=(\d+) rows=(\d+) scroll_offset=(\d+)"
)


@dataclass(frozen=True)
class Geometry:
    left: int
    top: int
    right: int
    bottom: int
    cell_width: int
    cell_height: int
    columns: int
    rows: int
    scroll_offset: int

    def cell_center(self, column, row, origin=(0, 0)):
        if not (0 <= column < self.columns and 0 <= row < self.rows):
            raise ValueError("Test cell falls outside the presented terminal")
        return (origin[0] + self.left + column * self.cell_width + self.cell_width // 2,
                origin[1] + self.top + row * self.cell_height + self.cell_height // 2)


def latest_geometry(log):
    matches = list(GEOMETRY_PATTERN.finditer(log))
    if not matches:
        raise LookupError("No presented geometry trace; enable trace-frames before launching the APK")
    geometry = Geometry(*map(int, matches[-1].groups()))
    if (min(geometry.cell_width, geometry.cell_height, geometry.columns, geometry.rows) < 1
            or geometry.right <= geometry.left or geometry.bottom <= geometry.top
            or geometry.columns * geometry.cell_width > geometry.right - geometry.left
            or geometry.rows * geometry.cell_height > geometry.bottom - geometry.top):
        raise ValueError("Invalid presented terminal geometry")
    return geometry


def screen_origin(root, package, geometry):
    # Virtual control rectangles originate in the same native surface as the
    # terminal. Their Android screen bounds account for status bars/window origin.
    bounds = [node_bounds(node) for node in root.iter("node")
              if node.get("package") == package and node.get("class") == "android.widget.Button"]
    if not bounds:
        raise LookupError("Native toolbar accessibility bounds are missing")
    return min(rect[0] for rect in bounds) - geometry.left, min(rect[1] for rect in bounds) - geometry.bottom


def shell_fixture(directory, captures):
    """Create controlled data only; test results can only come from PTY reads."""
    lines = ["#!/system/bin/sh", "set -eu", f"cd {shlex.quote(directory)}",
             "saved=$(stty -g)",
             "trap 'stty \"$saved\"; printf \"\\033[?1000l\\033[?1006l\\033[?2004l\"' EXIT",
             "printf '\\033[?1l\\033[?1000l\\033[?1006l\\033[?2004l'",
             "stty raw -echo",
             'release() { while [ ! -f "$1.release" ]; do sleep 0.1; done; }']
    for name, expected in captures:
        if name == "clipboard":
            lines.append(f"printf '\\033[2J\\033[H{COPY_TEXT}\\033[3;1H'")
        if name == "mouse":
            lines.append("printf '\\033[?1000h\\033[?1006h'")
        lines.extend([f"printf READY > {name}.ready",
                      f"dd bs=1 count={len(expected)} of={name}.bin 2>{name}.stderr",
                      # Drain any duplicate events/extra newline before declaring
                      # success. Each stage has a separate release barrier.
                      "stty min 0 time 10",
                      f"dd bs=1 count=4096 >> {name}.bin 2>>{name}.stderr",
                      "stty min 1 time 0", f"printf DONE > {name}.done", f"release {name}"])
        if name == "clipboard":
            lines.extend(["i=0; while [ $i -lt 400 ]; do printf 'scroll-%04d\\r\\n' \"$i\"; i=$((i+1)); done",
                          "printf READY > scroll.ready", "release scroll"])
        if name == "mouse":
            lines.append("printf '\\033[?1000l\\033[?1006l'")
    lines.append("printf DONE > finished")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial")
    parser.add_argument("--package", default="com.kokuban.terminal")
    parser.add_argument("--output", type=Path, default=Path("target/android-evidence/controls"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    device = Device(args.serial)
    device.wait_boot()
    package = args.package
    component = device.shell("cmd", "package", "resolve-activity", "--brief", package).splitlines()[-1]
    if not component.startswith(package + "/"):
        raise RuntimeError(f"No Kokuban launcher: {component}")
    private = device.shell("run-as", package, "pwd")
    directory = f"{private}/files/controls-smoke-{time.time_ns()}"
    trace = "files/config/kokuban/trace-frames"
    had_trace = device.shell("run-as", package, "sh", "-c", f"test -f {trace} && echo yes", check=False) == "yes"
    preferences = {(space, key): device.shell("settings", "get", space, key) for space, key in [
        ("system", "accelerometer_rotation"), ("system", "user_rotation"),
        ("secure", "show_ime_with_hard_keyboard")
    ]}
    input_help = device.shell("input", "--help", check=False)
    (args.output / "android-input-help.txt").write_text(input_help + "\n")
    has_mouse = bool(re.search(r"\bmouse\b", input_help))
    has_combinations = "keycombination" in input_help
    captures = [("toolbar", b"".join(value for _, value in TOOLBAR_KEYS)),
                ("keyboard", b"".join(value for _, value in EXTERNAL_KEYS)),
                ("escape_ime", b"\x1b"), ("clipboard", COPY_TEXT.encode())]
    if has_mouse:
        captures.append(("mouse", MOUSE_BYTES))
    if has_combinations:
        captures.append(("modifiers", b"".join(value for _, value in MODIFIER_KEYS)))
    results = {"device": device.details(), "status": "failed", "checks": [],
               "input_source": "Android input shell injection", "physical_usb": "not exercised",
               "mouse": "pending" if has_mouse else "not exercised: Android input help has no mouse source",
               "external_modifiers": "pending" if has_combinations else "not exercised: keycombination unavailable"}
    start_time = device.shell("date", "+%m-%d %H:%M:%S.000")
    remote_xml = "/sdcard/kokuban-controls-ui.xml"
    remote_script = f"/data/local/tmp/kokuban-controls-{time.time_ns()}.sh"
    capture_index = 0
    process = None

    def logcat():
        return device.adb("logcat", "-d", "--pid", process, "-T", start_time, check=False)

    def dump(label):
        nonlocal capture_index
        capture_index += 1
        device.shell("uiautomator", "dump", "--compressed", remote_xml, timeout=30)
        xml = device.shell("cat", remote_xml)
        (args.output / f"{capture_index:02d}-{label}.xml").write_text(xml + "\n")
        return ET.fromstring(xml)

    def control(label):
        for _ in range(18):
            root = dump("control-" + label.lower().replace(" ", "-"))
            try:
                return find_node(root, [label], package)
            except LookupError:
                for node in root.iter("node"):
                    if node.get("package") == package and label in (node.get("text"), node.get("content-desc")):
                        raise AssertionError(f"Control is disabled: {label}")
                x, y = center(find_node(root, ["More terminal controls"], package))
                device.shell("input", "tap", str(x), str(y))
        raise LookupError(f"Control not reachable through toolbar pages: {label}")

    def tap(label):
        x, y = center(control(label))
        device.shell("input", "tap", str(x), str(y))

    def ime_visible():
        state = device.shell("dumpsys", "input_method")
        return "mInputShown=true" in state or "isInputViewShown=true" in state

    def keyboard(visible):
        if ime_visible() != visible:
            tap("Show or hide keyboard")
            eventually(ime_visible, visible, 30)

    def marker(name):
        return device.shell("run-as", package, "cat", f"{directory}/{name}", check=False)

    def ready(name):
        eventually(lambda: marker(name + ".ready"), "READY", 45)

    def release(name):
        device.shell("run-as", package, "touch", f"{directory}/{name}.release")

    def verify(name):
        expected = dict(captures)[name]
        eventually(lambda: marker(name + ".done"), "DONE", 45)
        actual = device.adb("exec-out", "run-as", package, "cat", f"{directory}/{name}.bin", binary=True)
        (args.output / f"{name}.bin").write_bytes(actual)
        results["checks"].append({"name": name, "expected_hex": expected.hex(), "actual_hex": actual.hex(),
                                  "status": "passed" if actual == expected else "failed"})
        if actual != expected:
            raise AssertionError(f"{name}: PTY received {actual!r}, expected {expected!r}")
        device.screenshot(args.output / f"{name}.png")

    def geometry():
        return latest_geometry(logcat())

    try:
        device.shell("settings", "put", "system", "accelerometer_rotation", "0")
        device.shell("settings", "put", "system", "user_rotation", "0")
        device.shell("settings", "put", "secure", "show_ime_with_hard_keyboard", "1")
        device.shell("run-as", package, "mkdir", "-p", "files/config/kokuban", directory)
        device.shell("run-as", package, "touch", trace)
        fixture = args.output / "session.sh"
        fixture.write_text(shell_fixture(directory, captures))
        device.adb("push", str(fixture.resolve()), remote_script)
        device.shell("run-as", package, "sh", "-c",
                     f"cat {shlex.quote(remote_script)} > {shlex.quote(directory + '/session.sh')}")
        device.shell("am", "force-stop", package)
        device.shell("am", "start", "-W", "-n", component)
        process = device.pid(package)
        eventually(lambda: "first frame presented" in logcat(), True, 45)
        keyboard(False)
        device.type_text(f"sh {shlex.quote(directory + '/session.sh')}")
        device.shell("input", "keyevent", "KEYCODE_ENTER")

        ready("toolbar")
        for label, _ in TOOLBAR_KEYS:
            tap(label)
            if label == "Control modifier" and control(label).get("checked") != "true":
                raise AssertionError("Control latch was not exposed as checked")
        if control("Control modifier").get("checked") != "false":
            raise AssertionError("Control modifier remained latched after a key")
        verify("toolbar")
        release("toolbar")

        ready("keyboard")
        for key, _ in EXTERNAL_KEYS:
            device.shell("input", "keyboard", "keyevent", "KEYCODE_" + key)
        verify("keyboard")
        release("keyboard")

        ready("escape_ime")
        keyboard(True)
        device.screenshot(args.output / "escape-ime-open.png")
        device.shell("input", "keyboard", "keyevent", "KEYCODE_ESCAPE")
        verify("escape_ime")
        if not ime_visible():
            raise AssertionError("Hardware Escape was consumed to dismiss the IME")
        keyboard(False)
        release("escape_ime")

        ready("clipboard")
        tap("Select terminal text")
        if control("Select terminal text").get("checked") != "true":
            raise AssertionError("Selection mode did not become checked")
        layout = geometry()
        origin = screen_origin(dump("selection-bounds"), package, layout)
        start = layout.cell_center(0, 0, origin)
        end = layout.cell_center(len(COPY_TEXT) - 1, 0, origin)
        device.shell("input", "swipe", *map(str, (*start, *end)), "600")
        device.screenshot(args.output / "selection-drag.png")
        tap("Copy selection")
        tap("Select terminal text")
        tap("Paste clipboard")
        verify("clipboard")
        release("clipboard")

        ready("scroll")
        eventually(lambda: geometry().scroll_offset, 0)
        layout = geometry()
        origin = screen_origin(dump("scroll-bounds"), package, layout)
        start = layout.cell_center(layout.columns // 2, layout.rows // 4, origin)
        end = layout.cell_center(layout.columns // 2, layout.rows * 3 // 4, origin)
        device.screenshot(args.output / "scroll-before.png")
        device.shell("input", "swipe", *map(str, (*start, *end)), "600")
        eventually(lambda: geometry().scroll_offset > 0, True)
        results["scroll_offset_after_drag"] = geometry().scroll_offset
        device.screenshot(args.output / "scroll-up.png")
        if ime_visible():
            raise AssertionError("Scroll gesture unexpectedly opened the IME")
        for _ in range(4):
            if geometry().scroll_offset == 0:
                break
            device.shell("input", "swipe", *map(str, (*end, *start)), "600")
        eventually(lambda: geometry().scroll_offset, 0)
        device.screenshot(args.output / "scroll-restored.png")
        results["checks"].append({"name": "touch scroll", "status": "passed",
                                  "evidence": "presented scroll offset increased, then returned to zero; IME stayed hidden"})
        release("scroll")

        if has_mouse:
            ready("mouse")
            layout = geometry()
            origin = screen_origin(dump("mouse-bounds"), package, layout)
            x, y = layout.cell_center(2, 2, origin)
            device.shell("input", "mouse", "tap", str(x), str(y))
            verify("mouse")
            results["mouse"] = "Android SOURCE_MOUSE press/release produced exact SGR PTY events; no USB device used"
            release("mouse")
        if has_combinations:
            ready("modifiers")
            for keys, _ in MODIFIER_KEYS:
                device.shell("input", "keyboard", "keycombination", "-t", "100", *("KEYCODE_" + key for key in keys))
            verify("modifiers")
            results["external_modifiers"] = "Android key combinations produced Ctrl, Shift and Alt sequences; no USB device used"
            release("modifiers")
        eventually(lambda: marker("finished"), "DONE", 45)
        dump("finished")
        results["status"] = "passed"
    except Exception as error:
        results["error"] = str(error)
        raise
    finally:
        # Preserve partial raw captures even if an event was lost and dd is still
        # waiting. The following force-stop releases the fixture and its PTY.
        errors = []
        for name, _ in captures:
            try:
                data = device.adb("exec-out", "run-as", package, "cat", f"{directory}/{name}.bin", binary=True, check=False)
                (args.output / f"{name}.bin").write_bytes(data)
            except Exception as error:
                errors.append(str(error))
        for action in [lambda: device.screenshot(args.output / "final.png"), lambda: dump("final"),
                       lambda: (args.output / "logcat.txt").write_text(device.adb("logcat", "-d", "-T", start_time, check=False)),
                       lambda: device.shell("am", "force-stop", package)]:
            try:
                action()
            except Exception as error:
                errors.append(str(error))
        for (space, key), value in preferences.items():
            device.shell("settings", "delete", space, key, check=False) if value == "null" else device.shell("settings", "put", space, key, value, check=False)
        if not had_trace:
            device.shell("run-as", package, "rm", "-f", trace, check=False)
        device.shell("rm", "-f", remote_xml, remote_script, check=False)
        device.shell("run-as", package, "rm", "-rf", directory, check=False)
        if errors:
            results["capture_errors"] = errors
        (args.output / "results.json").write_text(json.dumps(results, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(results, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
