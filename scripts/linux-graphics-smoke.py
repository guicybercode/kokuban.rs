#!/usr/bin/env python3
"""Check presented Kitty/Sixel pixels: xvfb-run python3 <script> <kokuban>.

Uses only the standard library and xwd from x11-apps. The blue text background
is a rendering barrier, so disabled-image checks cannot pass on an empty screen.
The native-animation case emits a synthetic trace modeled on released icat;
it does not execute icat or decode a GIF. Playback must continue without output
from the emitter and stop on the final frame after two complete loops.
"""

import base64
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time
import zlib


Color = tuple[int, int, int]
Region = tuple[Color, int, int]
WIDTH, HEIGHT = 120, 36
RED, GREEN, BLUE = (255, 0, 0), (0, 255, 0), (0, 0, 255)
CYAN, YELLOW = (0, 255, 255), (255, 255, 0)
ANIMATION_GAP_MS = 1500
READY = b"\x1b[10;1H\x1b[48;2;0;0;255m" + b" " * 20 + b"\r\n" + b" " * 20 + b"\x1b[0m"


def png(color):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    rows = (b"\0" + bytes((*color, 255)) * WIDTH) * HEIGHT
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">2I5B", WIDTH, HEIGHT, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(rows))
        + chunk(b"IEND", b"")
    )


def payload(protocol, color):
    if protocol == "kitty":
        image = b"\x1b_Ga=T,f=100,i=42,q=2,C=1;" + base64.b64encode(png(color)) + b"\x1b\\"
    else:
        components = ";".join(str(component * 100 // 255) for component in color)
        image = (
            f'\x1bPq"1;1;{WIDTH};{HEIGHT}#1;2;{components}#1'.encode()
            + f"!{WIDTH}~-".encode() * (HEIGHT // 6)
            + b"\x1b\\"
        )
    ready = READY if color == RED else READY.replace(b"0;0;255m", b"255;255;0m")
    return b"\x1b[?25l\x1b[H" + image + ready


def animation_payloads() -> tuple[bytes, bytes]:
    """Build root/patch frames, then a separate command to start two loops."""
    image_number = 84

    def command(keys: str, pixels: bytes = b"") -> bytes:
        encoded = base64.b64encode(zlib.compress(pixels)).rstrip(b"=")
        data = b";" + encoded if pixels else b""
        return b"\x1b_G" + keys.encode("ascii") + data + b"\x1b\\"

    def frame(action: str, color: Color, width: int, keys: str) -> bytes:
        return command(
            f"a={action},I={image_number},q=2,f=32,o=z,"
            f"s={width},v={HEIGHT},C=1{keys}",
            bytes((*color, 255)) * width * HEIGHT,
        )

    # C=1, I selectors and unpadded payloads mirror released icat output.
    # Each patch retains a 20-pixel stripe from its explicit base frame.
    initial = (
        b"\x1b[?25l\x1b[H"
        + frame("T", RED, WIDTH, "")
        + command(f"a=a,I={image_number},r=1,z={ANIMATION_GAP_MS},v=3")
        + frame("f", GREEN, WIDTH - 20, f",x=20,c=1,z={ANIMATION_GAP_MS}")
        + command(f"a=a,I={image_number},s=2")
        + frame("f", CYAN, WIDTH - 20, f",c=2,z={ANIMATION_GAP_MS}")
        # Pause only to let the observer prove the root pixels before starting.
        + command(f"a=a,I={image_number},s=1,c=1")
        + READY
    )
    start = (
        command(f"a=a,I={image_number},s=3,c=1,v=3")
        + READY.replace(b"0;0;255m", b"255;255;0m")
    )
    return initial, start


def contains_rectangle(data, color, min_width=80, min_height=20):
    # XWDFile.h: 25 network-order CARD32s; each XWDColor occupies 12 bytes.
    # Pixel byte order is independent of header byte order.
    if len(data) < 100:
        raise ValueError("truncated XWD header")
    header = struct.unpack_from(">25I", data)
    size, version, image_format, _, width, height, xoffset, byte_order = header[:8]
    bits, stride, visual = header[11:14]
    masks = header[14:17]
    if version != 7 or image_format != 2 or visual != 4 or xoffset != 0:
        raise ValueError("expected a version 7 TrueColor ZPixmap XWD")
    if bits not in (24, 32) or byte_order not in (0, 1):
        raise ValueError("expected 24-bit or 32-bit XWD pixels")
    pixel_bytes = bits // 8
    offset = size + header[19] * 12
    if size < 100 or stride < width * pixel_bytes or len(data) < offset + stride * height:
        raise ValueError("invalid or truncated XWD pixel storage")
    mask = masks[0] | masks[1] | masks[2]
    desired = 0
    for component, channel_mask in zip(color, masks):
        if not channel_mask:
            raise ValueError("missing XWD color mask")
        shift = (channel_mask & -channel_mask).bit_length() - 1
        desired |= (component * (channel_mask >> shift) // 255) << shift
    order = "little" if byte_order == 0 else "big"
    heights = [0] * width
    for y in range(height):
        run = 0
        row_start = offset + y * stride
        for x in range(width):
            start = row_start + x * pixel_bytes
            pixel = int.from_bytes(data[start:start + pixel_bytes], order)
            heights[x] = heights[x] + 1 if pixel & mask == desired else 0
            run = run + 1 if heights[x] >= min_height else 0
            if run >= min_width:
                return True
    return False


def capture(directory):
    screenshot = directory / "frame.xwd"
    subprocess.run(
        ["xwd", "-root", "-silent", "-out", str(screenshot)],
        check=True, capture_output=True, timeout=3,
    )
    return screenshot.read_bytes()


def matches_regions(
    data: bytes, required: tuple[Region, ...], absent: tuple[Region, ...] = ()
) -> bool:
    return all(contains_rectangle(data, *region) for region in required) and not any(
        contains_rectangle(data, *region) for region in absent
    )


def wait_for_regions(
    process: subprocess.Popen, directory: Path,
    required: tuple[Region, ...], absent: tuple[Region, ...] = (),
) -> None:
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise AssertionError(f"terminal exited early with {process.returncode}")
        frame = capture(directory)
        if matches_regions(frame, required, absent):
            return
        time.sleep(0.05)
    raise AssertionError(f"no presented rectangles {required} without {absent}")


def wait_for_frame(process, directory, expected, absent=()):
    wait_for_regions(
        process, directory, ((expected, 80, 20),),
        tuple((color, 80, 20) for color in absent),
    )


def check_native_animation(process: subprocess.Popen, directory: Path) -> None:
    wait_for_frame(process, directory, RED, absent=(GREEN, CYAN))
    (directory / "next-frame").touch()
    wait_for_frame(process, directory, YELLOW)
    for cycle in range(2):
        # Frame two retains the red root stripe, frame three the green stripe
        # from frame two. A blank canvas or wrong base-frame index fails here.
        wait_for_regions(
            process, directory, ((GREEN, 80, 20), (RED, 12, 20)),
            absent=((CYAN, 80, 20),),
        )
        wait_for_regions(
            process, directory, ((CYAN, 80, 20), (GREEN, 12, 20)),
            absent=((RED, 12, 20),),
        )
        if cycle == 0:
            wait_for_frame(process, directory, RED, absent=(GREEN, CYAN))

    if not (directory / "emission-finished").exists():
        raise AssertionError("animation frames were observed before the emitter finished")
    # Staying on the last frame for more than a whole cycle detects accidental
    # infinite playback, including implementations that temporarily pause there.
    stable_until = time.monotonic() + 3 * ANIMATION_GAP_MS / 1000 + 0.5
    samples = 0
    while time.monotonic() < stable_until:
        if process.poll() is not None:
            raise AssertionError(f"terminal exited during final frame: {process.returncode}")
        if not matches_regions(
            capture(directory), ((CYAN, 80, 20), (GREEN, 12, 20)),
            absent=((RED, 12, 20),),
        ):
            raise AssertionError("finite animation did not retain its final frame")
        samples += 1
        time.sleep(0.05)
    if samples < 3:
        raise AssertionError("too few screenshots to verify the final animation frame")


def check(binary, protocol, enabled):
    with tempfile.TemporaryDirectory(prefix="kokuban-graphics-") as temporary:
        directory = Path(temporary)
        (directory / "kokuban.toml").write_text(
            '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
            '[window]\ncolumns = 40\nrows = 20\n'
            f'[images]\nenabled = {str(enabled).lower()}\n'
        )
        initial, update = animation_payloads() if protocol == "kitty-animation" else (
            payload(protocol, RED), payload(protocol, GREEN)
        )
        (directory / "red.bin").write_bytes(initial)
        (directory / "green.bin").write_bytes(update)
        shell = directory / "shell"
        shell.write_text(
            "#!/bin/sh\nstty -echo\ncat red.bin\n"
            "while [ ! -e next-frame ]; do sleep 0.05; done\n"
            "cat green.bin\ntouch emission-finished\nexec sleep 30\n"
        )
        shell.chmod(0o700)
        environment = os.environ.copy()
        for name in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR", "KOKUBAN_EXIT_AFTER_FIRST_FRAME"):
            environment.pop(name, None)
        environment.update(KOKUBAN_SHELL=str(shell), WINIT_X11_SCALE_FACTOR="1")
        log_path = directory / "terminal.log"
        with log_path.open("wb") as log:
            process = subprocess.Popen([str(binary)], cwd=directory, env=environment, stdout=log, stderr=log)
            try:
                wait_for_frame(process, directory, BLUE)
                if protocol == "kitty-animation":
                    check_native_animation(process, directory)
                elif enabled:
                    wait_for_frame(process, directory, RED)
                else:
                    wait_for_frame(process, directory, BLUE, absent=(RED, GREEN))
                if protocol != "kitty-animation":
                    (directory / "next-frame").touch()
                    if enabled:
                        wait_for_frame(process, directory, GREEN, absent=(RED,))
                    else:
                        # Yellow replaces the blue readiness block only after the
                        # second payload has passed through the terminal decoder.
                        wait_for_frame(process, directory, YELLOW, absent=(RED, GREEN))
            except Exception as error:
                raise AssertionError(
                    f"{protocol} enabled={enabled}: {error}\n"
                    + log_path.read_text(errors="replace")[-4000:]
                ) from error
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        detail = (
            "synthetic native trace, inherited frame pixels, two loops, stable final frame"
            if protocol == "kitty-animation"
            else f"images enabled={enabled}, initial and updated frame"
        )
        print(f"PASS {protocol}: {detail}", flush=True)


def main():
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: xvfb-run python3 {sys.argv[0]} /path/to/kokuban")
    binary = Path(sys.argv[1]).resolve(strict=True)
    for protocol in ("kitty", "sixel"):
        for enabled in (True, False):
            check(binary, protocol, enabled)
    check(binary, "kitty-animation", True)


if __name__ == "__main__":
    main()
