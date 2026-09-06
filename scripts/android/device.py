"""Small adb boundary shared by Android validation scripts (Python stdlib only)."""

import os
from pathlib import Path
import shlex
import shutil
import subprocess
import time


class Device:
    def __init__(self, serial=None):
        sdk = os.environ.get("ANDROID_HOME", os.environ.get("ANDROID_SDK_ROOT", str(Path.home() / "Library/Android/sdk")))
        executable = shutil.which("adb") or str(Path(sdk) / "platform-tools/adb")
        self.command = [executable]
        serial = serial or os.environ.get("ANDROID_SERIAL")
        if serial:
            self.command.extend(["-s", serial])

    def adb(self, *args, binary=False, timeout=30, check=True):
        result = subprocess.run(self.command + list(args), capture_output=True, text=not binary, timeout=timeout)
        if check and result.returncode:
            raise RuntimeError(f"adb command failed: {args!r}: {result.stderr!r} {result.stdout!r}")
        return result.stdout if binary else result.stdout.strip()

    def shell(self, *args, **kwargs):
        return self.adb("shell", shlex.join(args), **kwargs)

    def wait_boot(self, timeout=180):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                if self.shell("getprop", "sys.boot_completed", timeout=10) == "1":
                    return
            except (RuntimeError, subprocess.TimeoutExpired):
                pass
            time.sleep(1)
        raise RuntimeError("Android did not finish booting before the deadline")

    def pid(self, package):
        pids = self.shell("pidof", package, check=False).split()
        if len(pids) != 1:
            raise RuntimeError(f"Expected one running process for {package}, got {pids}")
        return pids[0]

    def screenshot(self, destination):
        data = self.adb("exec-out", "screencap", "-p", binary=True)
        if not data.startswith(b"\x89PNG\r\n\x1a\n"):
            raise RuntimeError("screencap did not return a PNG")
        Path(destination).write_bytes(data)

    def details(self):
        return {name: self.shell("getprop", prop) for name, prop in {
            "model": "ro.product.model", "manufacturer": "ro.product.manufacturer",
            "android": "ro.build.version.release", "api": "ro.build.version.sdk",
            "abi": "ro.product.cpu.abi", "fingerprint": "ro.build.fingerprint",
        }.items()}
