#!/usr/bin/env python3
"""Display a photo or decoded video through Kokuban's shared Kitty protocol.

Run on a desktop or SSH host with FFmpeg; no FFmpeg is required in the APK.
Video is silent. This is a bounded frame-replacement route, not Kitty's native
animation extension. Timing statistics describe the producer, not display FPS.
"""
import argparse
import base64
import json
from pathlib import Path
import subprocess
import sys
import time
import zlib

ESC = b"\x1b"
CHUNK = 4096


def kitty_frame(pixels, width, height, image_id=19001):
    if len(pixels) != width * height * 4:
        raise ValueError("RGBA frame has an unexpected length")
    payload = base64.b64encode(zlib.compress(pixels, level=1))
    for offset in range(0, len(payload), CHUNK):
        chunk = payload[offset:offset + CHUNK]
        more = int(offset + CHUNK < len(payload))
        if offset == 0:
            control = f"a=T,f=32,s={width},v={height},i={image_id},p=1,C=1,q=2,o=z,m={more}"
        else:
            control = f"m={more}"
        yield ESC + b"_G" + control.encode("ascii") + b";" + chunk + ESC + b"\\"


def read_frame(stream, byte_count):
    frame = bytearray()
    while len(frame) < byte_count:
        block = stream.read(byte_count - len(frame))
        if not block:
            if frame:
                raise ValueError("FFmpeg ended in the middle of an RGBA frame")
            return None
        frame.extend(block)
    return frame


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("--width", type=int, default=320)
    parser.add_argument("--height", type=int, default=180)
    parser.add_argument("--fps", type=float, default=12)
    parser.add_argument("--duration", type=float, default=10)
    parser.add_argument("--hold", type=float, default=2)
    parser.add_argument("--photo", action="store_true")
    parser.add_argument("--image-id", type=int, default=19001)
    parser.add_argument("--stats", type=Path, help="producer JSON, separate from terminal output")
    args = parser.parse_args()
    if not (1 <= args.width <= 1280 and 1 <= args.height <= 720 and 0 < args.fps <= 30
            and 0 < args.duration <= 600 and 0 <= args.hold <= 600 and 1 <= args.image_id <= 2**32 - 1):
        parser.error("size <=1280x720, 0<fps<=30, duration<=600s, hold<=600s, nonzero uint32 image ID required")
    filters = (("" if args.photo else f"fps={args.fps},")
               + f"scale={args.width}:{args.height}:force_original_aspect_ratio=decrease,"
               f"pad={args.width}:{args.height}:(ow-iw)/2:(oh-ih)/2,format=rgba")
    command = ["ffmpeg", "-nostdin", "-v", "error", "-i", str(args.input), "-an", "-vf", filters,
               "-frames:v", "1" if args.photo else str(max(1, int(args.fps * args.duration))),
               "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"]
    sent = 0
    frame_count = 0
    start = time.monotonic()
    output = sys.stdout.buffer
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as decoder:
        try:
            output.write(ESC + b"7")
            while True:
                pixels = read_frame(decoder.stdout, args.width * args.height * 4)
                if pixels is None:
                    break
                deadline = start + frame_count / args.fps
                delay = deadline - time.monotonic()
                if delay > 0:
                    time.sleep(delay)
                output.write(ESC + b"[1;1H")
                for chunk in kitty_frame(pixels, args.width, args.height, args.image_id):
                    output.write(chunk)
                    sent += len(chunk)
                output.flush()
                frame_count += 1
            if decoder.wait() != 0:
                raise RuntimeError("FFmpeg could not decode the input")
            if not frame_count:
                raise RuntimeError("FFmpeg produced no frames")
            elapsed = time.monotonic() - start
            if args.stats:
                args.stats.write_text(json.dumps({
                    "frames_sent": frame_count, "protocol_bytes": sent, "producer_seconds": elapsed,
                    "requested_fps": args.fps, "display_fps": None, "audio": False,
                    "width": args.width, "height": args.height,
                }, indent=2) + "\n")
            time.sleep(args.hold)
        finally:
            if decoder.poll() is None:
                decoder.terminate()
                try:
                    decoder.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    decoder.kill()
                    decoder.wait()
            output.write(ESC + f"_Ga=d,d=I,i={args.image_id},q=2;".encode() + ESC + b"\\" + ESC + b"8")
            output.flush()


if __name__ == "__main__":
    try:
        main()
    except (KeyboardInterrupt, BrokenPipeError):
        pass
