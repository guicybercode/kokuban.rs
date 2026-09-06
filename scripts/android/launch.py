#!/usr/bin/env python3
"""Install an APK without clearing its data and wait for its first real frame."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import time

from device import Device
from smoke import eventually


def has_first_frame(logs, pid):
    for line in logs.splitlines():
        fields = line.split(maxsplit=5)
        if len(fields) == 6 and fields[2] == str(pid):
            if re.fullmatch(r"Kokuban\s*:\s*kokuban::android_window:\s*first frame presented", fields[5]):
                return True
    return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial")
    parser.add_argument("--package", default="com.kokuban.terminal")
    parser.add_argument("--apk", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("target/android-evidence/launch"))
    args = parser.parse_args()
    device = Device(args.serial)
    device.wait_boot()
    device.adb("install", "-r", str(args.apk.resolve()), timeout=120)
    component = device.shell("cmd", "package", "resolve-activity", "--brief", args.package).splitlines()[-1]
    if not component.startswith(args.package + "/"):
        raise RuntimeError(f"Unexpected launcher: {component}")
    output = args.output / str(time.time_ns())
    output.mkdir(parents=True, exist_ok=True)
    log_path = output / "launch-logcat.txt"
    result = {"apk": str(args.apk), "component": component, "status": "incomplete"}
    with log_path.open("wb") as log, (output / "logcat-errors.txt").open("wb") as errors:
        capture = subprocess.Popen(device.command + ["logcat", "-v", "threadtime",
            "Kokuban:V", "KokubanHarness:V", "AndroidRuntime:E", "*:S"], stdout=log, stderr=errors)
        try:
            # Establish the stream before starting the app. This test-only tag
            # cannot satisfy the native first-frame predicate below.
            marker = f"launch-capture-{time.monotonic_ns()}"
            device.shell("log", "-t", "KokubanHarness", marker)
            eventually(lambda: marker in log_path.read_text(errors="replace"), True, timeout=5)
            device.shell("am", "force-stop", args.package)
            result["activity_start"] = device.shell("am", "start", "-W", "-n", component)
            pid = device.pid(args.package)
            result["pid"] = pid
            eventually(lambda: has_first_frame(log_path.read_text(errors="replace").partition(marker)[2], pid), True, timeout=30)
            result["status"] = "passed"
        except Exception as error:
            result.update(status="failed", error=str(error))
            try:
                device.screenshot(output / "failure.png")
            except Exception as diagnostic_error:
                result["screenshot_error"] = str(diagnostic_error)
            raise
        finally:
            capture.terminate()
            try:
                capture.wait(timeout=5)
            except subprocess.TimeoutExpired:
                capture.kill()
                capture.wait(timeout=5)
            (output / "results.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"{args.apk}: PID {pid} presented its first frame")


if __name__ == "__main__":
    main()
