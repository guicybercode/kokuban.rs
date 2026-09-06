#!/usr/bin/env python3
"""Install and exercise the real shell across Android lifecycle changes.

This uses adb key injection; it does NOT validate IME composition. The marker is
written by commands entered into the terminal, not by the test harness.
Requires a debuggable APK for run-as verification of private storage.
"""

import argparse
import json
from pathlib import Path
import shlex
import time

from device import Device


def eventually(read, expected, timeout=15):
    deadline = time.monotonic() + timeout
    actual = None
    while time.monotonic() < deadline:
        actual = read()
        if actual == expected:
            return
        time.sleep(0.2)
    raise AssertionError(f"Expected {expected!r}, got {actual!r}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apk", type=Path, required=True)
    parser.add_argument("--serial")
    parser.add_argument("--package", default="com.kokuban.terminal")
    parser.add_argument("--trace-frames", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("target/android-evidence/smoke"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    device = Device(args.serial)
    device.wait_boot()
    device.adb("install", "-r", str(args.apk.resolve()), timeout=120)
    package = args.package
    component = device.shell("cmd", "package", "resolve-activity", "--brief", package).splitlines()[-1]
    if not component.startswith(package + "/"):
        raise RuntimeError(f"No launcher activity found for {package}: {component}")
    private = device.shell("run-as", package, "pwd")
    if args.trace_frames:
        device.shell("run-as", package, "mkdir", "-p", "files/config/kokuban")
        device.shell("run-as", package, "touch", "files/config/kokuban/trace-frames")
    marker = f"{private}/files/kokuban-smoke-result.txt"
    device.shell("run-as", package, "rm", "-f", marker)
    old_auto = device.shell("settings", "get", "system", "accelerometer_rotation")
    old_rotation = device.shell("settings", "get", "system", "user_rotation")
    start_time = device.shell("date", "+%m-%d %H:%M:%S.000")
    results = {"device": device.details(), "apk": str(args.apk), "checks": [], "ime_composition": "not tested by this script"}

    def enter(command):
        # Android input replaces %s with spaces; quote the entire argument for
        # the device shell so > and other shell operators reach the terminal.
        device.type_text(command)
        device.shell("input", "keyevent", "KEYCODE_ENTER")

    def assert_shell(expected, command):
        enter(command)
        eventually(lambda: device.shell("run-as", package, "cat", marker, check=False), expected)

    try:
        device.shell("input", "keyevent", "KEYCODE_WAKEUP")
        device.shell("input", "keyevent", "KEYCODE_MENU")
        device.shell("am", "force-stop", package)
        device.shell("am", "start", "-W", "-n", component)
        process = device.pid(package)
        eventually(lambda: "first frame presented" in device.adb("logcat", "-d", "--pid", process), True, timeout=30)
        device.screenshot(args.output / "00-shell-ready.png")
        assert_shell("OPEN", f"KOKUBAN_SMOKE_SESSION=kept;printf OPEN > {shlex.quote(marker)}")
        device.screenshot(args.output / "01-shell.png")
        results["checks"].append("PTY executes a command entered through key events")
        device.shell("input", "keyevent", "KEYCODE_HOME")
        time.sleep(1)
        if device.pid(package) != process:
            raise AssertionError("Application process changed during suspension")
        device.shell("am", "start", "-W", "-n", component)
        time.sleep(1)
        assert_shell("OPEN_RESUME_kept", f"printf _RESUME_$KOKUBAN_SMOKE_SESSION >> {shlex.quote(marker)}")
        results["checks"].append("same process and PTY survive home/resume")
        device.screenshot(args.output / "02-resumed.png")
        device.shell("settings", "put", "system", "accelerometer_rotation", "0")
        for rotation in (1, 0, 1, 0):
            device.shell("settings", "put", "system", "user_rotation", str(rotation))
            time.sleep(1)
            if device.pid(package) != process:
                raise AssertionError("Application process changed during rotation")
            def rotated():
                width, height = device.screenshot(args.output / f"03-rotation-{len(results['checks'])}.png")
                return width > height if rotation == 1 else height > width
            eventually(rotated, True)
            results["checks"].append(f"process survives actual screen rotation {rotation}")
        assert_shell("OPEN_RESUME_kept_ROTATE_kept", f"printf _ROTATE_$KOKUBAN_SMOKE_SESSION >> {shlex.quote(marker)}")
        results["checks"].append("PTY executes commands after repeated rotation")
        results["status"] = "passed"
    finally:
        if results.get("status") != "passed":
            device.screenshot(args.output / "failure.png")
        for name, value in (("accelerometer_rotation", old_auto), ("user_rotation", old_rotation)):
            if value == "null":
                device.shell("settings", "delete", "system", name)
            else:
                device.shell("settings", "put", "system", name, value)
        (args.output / "logcat.txt").write_text(device.adb("logcat", "-d", "-T", start_time, check=False))
        (args.output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
