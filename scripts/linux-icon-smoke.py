#!/usr/bin/env python3
"""Verify the embedded application icon on a real X11 window under xvfb-run.

Requires xdotool and xprop (x11-utils), with no Python packages. Starting in a
temporary directory without assets verifies that the icon travels with the
binary rather than depending on the working directory.
"""

import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile
import time


APP_ID = "io.github.guicybercode.kokuban"


def run(*arguments: str) -> str:
    return subprocess.run(
        arguments, check=True, capture_output=True, text=True, timeout=5,
    ).stdout


def verify_icon(property_text: str, source_size: tuple[int, int]) -> list[tuple[int, int]]:
    # EWMH stores width, height and width * height ARGB CARDINALs per image.
    match = re.fullmatch(r"_NET_WM_ICON\(CARDINAL\) = ([\d,\s]+)", property_text)
    if match is None:
        raise AssertionError(f"missing or invalid _NET_WM_ICON: {property_text[:160]!r}")
    values = [int(value.strip()) for value in match.group(1).split(",")]
    offset = 0
    sizes = []
    while offset < len(values):
        if len(values) - offset < 2:
            raise AssertionError("truncated icon dimensions")
        width, height = values[offset:offset + 2]
        offset += 2
        end = offset + width * height
        if width == 0 or height == 0 or end > len(values):
            raise AssertionError(f"invalid or truncated {width}x{height} icon")
        pixels = values[offset:end]
        if any(pixel > 0xFFFFFFFF for pixel in pixels):
            raise AssertionError("icon pixel exceeds the 32-bit ARGB format")
        visible_colors = {pixel & 0xFFFFFF for pixel in pixels if pixel >> 24}
        if len(visible_colors) < 2:
            raise AssertionError("icon is transparent or contains only one visible color")
        # Resampling may round either dimension by one pixel; do not couple
        # this integration check to the chosen runtime icon resolution.
        source_width, source_height = source_size
        if abs(width * source_height - height * source_width) > max(source_size):
            raise AssertionError(f"{width}x{height} icon does not preserve the source aspect ratio")
        sizes.append((width, height))
        offset = end
    if not sizes:
        raise AssertionError("window icon contains no images")
    return sizes


def check(binary: Path) -> None:
    source = Path(__file__).resolve().parents[1] / "assets" / "kokuban-icon.png"
    header = source.read_bytes()[:24]
    if header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
        raise AssertionError("source application icon is not a PNG")
    source_size = struct.unpack(">II", header[16:24])

    with tempfile.TemporaryDirectory(prefix="kokuban-icon-") as temporary:
        directory = Path(temporary)
        (directory / "kokuban.toml").write_text(
            '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
            '[window]\ncolumns = 40\nrows = 20\n', encoding="utf-8",
        )
        shell = directory / "shell"
        shell.write_text("#!/bin/sh\nexec /bin/sleep 30\n", encoding="utf-8")
        shell.chmod(0o700)
        environment = os.environ.copy()
        for name in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR", "KOKUBAN_EXIT_AFTER_FIRST_FRAME"):
            environment.pop(name, None)
        environment.update(KOKUBAN_SHELL=str(shell), WINIT_X11_SCALE_FACTOR="1")
        log_path = directory / "terminal.log"
        with log_path.open("wb") as log:
            process = subprocess.Popen(
                [str(binary)], cwd=directory, env=environment,
                stdin=subprocess.DEVNULL, stdout=log, stderr=log,
            )
            try:
                deadline = time.monotonic() + 10
                window = None
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise AssertionError(f"terminal exited early: {process.returncode}")
                    result = subprocess.run(
                        ["xdotool", "search", "--onlyvisible", "--pid", str(process.pid)],
                        capture_output=True, text=True, timeout=2,
                    )
                    if result.returncode == 0 and result.stdout.strip():
                        window = result.stdout.splitlines()[0]
                        break
                    time.sleep(0.05)
                if window is None:
                    raise AssertionError("timed out waiting for the Kokuban X11 window")
                wm_class = run("xprop", "-id", window, "WM_CLASS")
                if re.findall(r'"([^"\\]*)"', wm_class) != ["kokuban", APP_ID]:
                    raise AssertionError(f"unexpected WM_CLASS: {wm_class.strip()}")
                # Explicitly override xprop's built-in icon visualization.
                icon = run(
                    "xprop", "-id", window, "-f", "_NET_WM_ICON", "32c",
                    r" = $0+\n", "_NET_WM_ICON",
                )
                sizes = verify_icon(icon, source_size)
                if process.poll() is not None:
                    raise AssertionError(f"terminal exited during icon verification: {process.returncode}")
            except Exception as error:
                raise AssertionError(
                    f"{error}\n" + log_path.read_text(errors="replace")[-4000:]
                ) from error
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=3)
    dimensions = ", ".join(f"{width}x{height}" for width, height in sizes)
    print(f"PASS X11 icon: {dimensions} ARGB, WM_CLASS={APP_ID}, CWD without assets")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: xvfb-run python3 {sys.argv[0]} /path/to/kokuban")
    check(Path(sys.argv[1]).resolve(strict=True))
