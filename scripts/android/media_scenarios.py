"""Presented-media checks inside the SSH session owned by ssh_smoke.py.

Requires FFmpeg on the isolated runner, never in the APK. The photographic
fixture is fetched by hash; native-animation bytes reuse the Linux fixture.
Screenshots prove pixels. Application timestamps measure presentation calls,
not display scanout. Generated credentials are not part of this evidence.
"""
import hashlib
import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import sys
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


def decode_rgb(path, width=None, height=None):
    command = ["ffmpeg", "-nostdin", "-v", "error", "-i", str(path)]
    if width is not None:
        # Match kitty-media.py's RGBA scaling path; scaling directly to rgb24
        # can round differently and would create false screenshot failures.
        command += ["-vf", f"scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,format=rgba"]
    pixels = subprocess.check_output(command + ["-frames:v", "1", "-threads", "1", "-f", "rawvideo", "-pix_fmt", "rgba" if width is not None else "rgb24", "pipe:1"])
    return bytes(channel for index, channel in enumerate(pixels) if index % 4 != 3) if width is not None else pixels


def rectangle(rgb, size, color, width=80, height=20):
    """Find an exact rectangle, checking complete rows and RGB alignment."""
    stride = size[0] * 3
    pattern = bytes(color) * width
    start = 0
    while (position := rgb.find(pattern, start)) >= 0:
        start = position + 1
        y, x_bytes = divmod(position, stride)
        if position % 3 or x_bytes + len(pattern) > stride or y + height > size[1]:
            continue
        if all(rgb[position + row * stride:position + row * stride + len(pattern)] == pattern for row in range(height)):
            return True
    return False


def locate_photo(rgb, size, expected, photo_size=(320, 320)):
    """Locate a distinctive source row and verify landmarks across the photo."""
    width, height = photo_size
    anchor_x, anchor_y = width // 4, height // 2
    source = (anchor_y * width + anchor_x) * 3
    pattern = expected[source:source + min(96, (width - anchor_x) * 3)]
    start = 0
    while (position := rgb.find(pattern, start)) >= 0:
        start = position + 1
        if position % 3:
            continue
        y, x = divmod(position // 3, size[0])
        x, y = x - anchor_x, y - anchor_y
        if x < 0 or y < 0 or x + width > size[0] or y + height > size[1]:
            continue
        points = [(width * col // 6, height * row // 6) for col in range(1, 6) for row in range(1, 6)]
        if all(rgb[((y + dy) * size[0] + x + dx) * 3:((y + dy) * size[0] + x + dx) * 3 + 3]
               == expected[(dy * width + dx) * 3:(dy * width + dx) * 3 + 3] for dx, dy in points):
            return x, y
    return None


def video_frame_hashes(path, width, height, fps, count):
    """Hash the actual decoded RGBA frames using the producer's conversion."""
    raw = subprocess.check_output(["ffmpeg", "-nostdin", "-v", "error", "-i", str(path), "-an", "-vf",
        f"fps={fps},scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,format=rgba",
        "-frames:v", str(count), "-threads", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
    frame_bytes = width * height * 4
    if len(raw) != frame_bytes * count:
        raise AssertionError("Reference MP4 decode did not produce the expected frames")
    hashes = {}
    for index in range(count):
        rgba = raw[index * frame_bytes:(index + 1) * frame_bytes]
        rgb = bytes(channel for offset, channel in enumerate(rgba) if offset % 4 != 3)
        hashes.setdefault(hashlib.sha256(rgb).hexdigest(), []).append(index)
    return hashes


def run_media_scenarios(device, enter, server_root, output, serial, package):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = json.loads((ROOT / "tools/media-photo.json").read_text())
    photo = server_root / "apollo17.jpg"
    with urllib.request.urlopen(fixture["url"], timeout=30) as response:
        data = response.read(fixture["bytes"] + 1)
    if len(data) != fixture["bytes"] or hashlib.sha256(data).hexdigest() != fixture["sha256"]:
        raise AssertionError("NASA photograph changed; review the source before updating its hash")
    photo.write_bytes(data)
    expected = decode_rgb(photo, 320, 320)
    results = {"photo": fixture, "checks": [], "status": "incomplete"}
    sequence = 0

    def capture(name):
        nonlocal sequence
        sequence += 1
        path = output / f"{sequence:02d}-{name}.png"
        size = device.screenshot(path)
        rgb = decode_rgb(path)
        if len(rgb) != size[0] * size[1] * 3:
            raise AssertionError("Unexpected decoded screenshot dimensions")
        return rgb, size

    def wait_pixels(name, predicate, timeout=8):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            rgb, size = capture(name)
            match = predicate(rgb, size)
            if match is not None and match is not False:
                return match
            time.sleep(0.15)
        raise AssertionError(f"Expected presented pixels absent: {name}")

    clear = b"\x1b_Ga=d,d=A,q=2;\x1b\\\x1b[2J\x1b[H\x1b[?25l"

    def send_bytes(name, data):
        path = server_root / f"{name}.kitty"
        path.write_bytes(data)
        enter(f"cat {shlex.quote(str(path))}")

    def remote_script(name, body):
        path = server_root / f"{name}.sh"
        path.write_text(body + "\n")
        enter(f"sh {shlex.quote(str(path))}")

    producer = shlex.quote(str(ROOT / "tools/kitty-media.py"))
    try:
        clear_file = server_root / "clear.kitty"
        clear_file.write_bytes(clear)
        photo_done = server_root / "photo.done"
        remote_script("photo", f"cat {shlex.quote(str(clear_file))}; python3 {producer} {shlex.quote(str(photo))} --photo --width 320 --height 320 --hold 8; touch {shlex.quote(str(photo_done))}")
        origin = wait_pixels("photo", lambda rgb, size: locate_photo(rgb, size, expected))
        results["photo_origin_pixels"] = origin
        results["checks"].append("NASA photograph landmarks match pixels presented by Android")
        deadline = time.monotonic() + 12
        while not photo_done.exists() and time.monotonic() < deadline:
            time.sleep(0.2)
        if not photo_done.exists():
            raise AssertionError("Photograph producer did not finish")
        wait_pixels("photo-removed", lambda rgb, size: locate_photo(rgb, size, expected) is None)
        results["checks"].append("photograph deletion removes its presented pixels")

        spec = importlib.util.spec_from_file_location("linux_graphics_fixture", ROOT / "scripts/linux-graphics-smoke.py")
        linux = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(linux)
        send_bytes("sixel", clear + linux.payload("sixel", linux.RED))
        wait_pixels("sixel", lambda rgb, size: rectangle(rgb, size, linux.RED))
        results["checks"].append("shared Sixel decoder presents a red image rectangle")

        initial, start = linux.animation_payloads()
        send_bytes("animation-init", clear + initial)
        wait_pixels("animation-root", lambda rgb, size: rectangle(rgb, size, linux.RED))
        start_file, emitter_done = server_root / "animation-start.kitty", server_root / "animation-emitter.done"
        start_file.write_bytes(start)
        remote_script("start-animation", f"cat {shlex.quote(str(start_file))}; touch {shlex.quote(str(emitter_done))}")
        wait_pixels("animation-patch", lambda rgb, size: emitter_done.exists()
                    and rectangle(rgb, size, linux.GREEN) and rectangle(rgb, size, linux.RED, 12, 20))
        wait_pixels("animation-composed", lambda rgb, size: emitter_done.exists()
                    and rectangle(rgb, size, linux.CYAN) and rectangle(rgb, size, linux.GREEN, 12, 20))
        results["checks"].append("native Kitty animation advances and composes patches after emitter exits")
        send_bytes("animation-delete", clear)

        video = server_root / "sample.mp4"
        subprocess.run(["ffmpeg", "-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=12",
                        "-t", "15", "-c:v", "libx264", "-threads", "1", "-pix_fmt", "yuv420p", str(video)], check=True)
        references = video_frame_hashes(video, 320, 180, 12, 180)

        def recognize_video(rgb, size):
            x, y = origin
            region = b"".join(rgb[((y + row) * size[0] + x) * 3:((y + row) * size[0] + x + 320) * 3] for row in range(180))
            digest = hashlib.sha256(region).hexdigest()
            return {"sha256": digest, "decoded_frame_indices": references[digest]} if digest in references else None

        producer_stats = output / "video-producer.json"
        video_done = server_root / "video.done"
        remote_script("video", f"cat {shlex.quote(str(clear_file))}; python3 {producer} {shlex.quote(str(video))} --width 320 --height 180 --fps 12 --duration 15 --hold 0 --stats {shlex.quote(str(producer_stats))}; touch {shlex.quote(str(video_done))}")
        # A real decoded frame is the barrier: blank -> first frame cannot be
        # counted as playback, and memory sampling starts after presentation.
        first_video_frame = wait_pixels("video-first-frame", recognize_video)
        with (output / "measurement-output.txt").open("w") as log:
            measurement = subprocess.Popen([sys.executable, str(ROOT / "scripts/android/measure.py"), "--serial", serial,
                "--package", package, "--scenario", "ssh-mp4-320x180-12fps", "--seconds", "8", "--output", str(output / "measurements")], stdout=log, stderr=subprocess.STDOUT)
            presented = [first_video_frame]
            try:
                for _ in range(3):
                    time.sleep(0.8)
                    presented.append(wait_pixels("video", recognize_video))
                if measurement.wait(timeout=30) != 0:
                    raise AssertionError("Video CPU/PSS collection failed")
            finally:
                if measurement.poll() is None:
                    measurement.terminate()
                    measurement.wait(timeout=5)
        if len({frame["sha256"] for frame in presented}) < 3:
            raise AssertionError("MP4 decoding did not produce changing pixels in the image region")
        results["presented_video_frames"] = presented
        deadline = time.monotonic() + 20
        while not video_done.exists() and time.monotonic() < deadline:
            time.sleep(0.2)
        if not video_done.exists() or json.loads(producer_stats.read_text())["frames_sent"] != 180:
            raise AssertionError("MP4 producer did not decode all 180 requested frames")
        results["checks"].append("FFmpeg decodes H.264 MP4 over SSH with changing presented Android image pixels")
        results["video_note"] = "Synthetic motion encoded in a real H.264 file; silent playback. Producer FPS and presentation-call rate are distinct."
        results["status"] = "passed"
        return results
    finally:
        (output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
