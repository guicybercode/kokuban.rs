"""Small adb boundary shared by Android validation scripts (Python stdlib only)."""

import os
from pathlib import Path
import shlex
import shutil
import struct
import subprocess
import time


def output_summary(output):
    rendered = repr(output)
    if len(rendered) <= 4096:
        return rendered
    return f"{rendered[:2048]} ... [{len(rendered) - 4096} characters omitted] ... {rendered[-2048:]}"


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
            raise RuntimeError(f"adb command failed (exit {result.returncode}): {args!r}: "
                               f"stderr={output_summary(result.stderr)} stdout={output_summary(result.stdout)}")
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

    def type_text(self, text, chunk_size=8):
        # Android input gives a text command's generated events one timestamp.
        # Long bursts can become stale while a busy emulator processes them.
        if "%s" in text:
            raise ValueError("adb input text cannot preserve a literal %s")
        for offset in range(0, len(text), chunk_size):
            self.shell("input", "text", text[offset:offset + chunk_size].replace(" ", "%s"))

    def screenshot(self, destination):
        data = self.adb("exec-out", "screencap", "-p", binary=True)
        if not data.startswith(b"\x89PNG\r\n\x1a\n"):
            raise RuntimeError("screencap did not return a PNG")
        Path(destination).write_bytes(data)
        return struct.unpack(">II", data[16:24])

    def process_tree(self, root_pid):
        rows = self.shell("ps", "-A", "-o", "PID,PPID,NAME").splitlines()
        processes = []
        for row in rows:
            fields = row.split(maxsplit=2)
            if len(fields) == 3 and fields[0].isdigit() and fields[1].isdigit():
                processes.append({"pid": fields[0], "parent_pid": fields[1], "name": fields[2]})
        included = {str(root_pid)}
        while True:
            expanded = included | {p["pid"] for p in processes if p["parent_pid"] in included}
            if expanded == included:
                return [p for p in processes if p["pid"] in included]
            included = expanded

    def details(self):
        return {name: self.shell("getprop", prop) for name, prop in {
            "model": "ro.product.model", "manufacturer": "ro.product.manufacturer",
            "android": "ro.build.version.release", "api": "ro.build.version.sdk",
            "abi": "ro.product.cpu.abi", "fingerprint": "ro.build.fingerprint",
        }.items()}
