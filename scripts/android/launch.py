#!/usr/bin/env python3
"""Install an APK without clearing its data and wait for its first real frame."""

import argparse
from pathlib import Path

from device import Device
from smoke import eventually


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial")
    parser.add_argument("--package", default="com.kokuban.terminal")
    parser.add_argument("--apk", type=Path, required=True)
    args = parser.parse_args()
    device = Device(args.serial)
    device.wait_boot()
    device.adb("install", "-r", str(args.apk.resolve()), timeout=120)
    component = device.shell("cmd", "package", "resolve-activity", "--brief", args.package).splitlines()[-1]
    if not component.startswith(args.package + "/"):
        raise RuntimeError(f"Unexpected launcher: {component}")
    device.shell("am", "force-stop", args.package)
    device.shell("am", "start", "-W", "-n", component)
    pid = device.pid(args.package)
    eventually(lambda: "first frame presented" in device.adb("logcat", "-d", "--pid", pid), True, timeout=30)
    print(f"{args.apk}: PID {pid} presented its first frame")


if __name__ == "__main__":
    main()
