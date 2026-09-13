#!/usr/bin/env python3
"""Exercise released mpv's direct Kitty video output in a real Xvfb window.

The 72-frame FFV1 clip is decoded by mpv, never emitted by this script as Kitty
commands. Window pixels establish visible frame IDs; IPC only observes player
properties (and requests quit). Space is delivered through XTest. Sampling is
an observed-rate lower bound, not a source-FPS or release-performance claim.

Flags are supported by the released mpv v0.37.0 implementation:
https://github.com/mpv-player/mpv/blob/v0.37.0/video/out/vo_kitty.c
https://github.com/mpv-player/mpv/blob/v0.37.0/etc/builtin.conf
https://github.com/mpv-player/mpv/blob/v0.37.0/DOCS/man/ipc.rst
"""

import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable, Optional
import zlib


WIDTH, HEIGHT, FRAME_COUNT, SOURCE_FPS = 320, 180, 72, 12
SAMPLE_INTERVAL = 0.025
ZERO, ONE = (20, 40, 240), (240, 220, 20)
LEFT_MARKER, RIGHT_MARKER = (240, 20, 40), (20, 220, 230)


def record(directory: Path, name: str, value: Any) -> None:
    pending = directory / (name + ".pending")
    pending.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    pending.replace(directory / name)


def run(arguments: list[str], **kwargs: Any) -> subprocess.CompletedProcess:
    kwargs.setdefault("timeout", 8)
    kwargs.setdefault("check", True)
    kwargs.setdefault("capture_output", True)
    return subprocess.run(arguments, **kwargs)


def body_color(frame_id: int) -> tuple[int, int, int]:
    return (30 + frame_id * 37 % 190, 30 + frame_id * 59 % 190,
            30 + frame_id * 83 % 190)


def source_frame(frame_id: int) -> bytes:
    """Large eight-bit barcode, inverse barcode and independent color checksum."""
    pixels = bytearray(bytes(body_color(frame_id)) * WIDTH * HEIGHT)

    def rectangle(x: int, y: int, width: int, height: int,
                  color: tuple[int, int, int]) -> None:
        row = bytes(color) * width
        for line in range(y, y + height):
            start = (line * WIDTH + x) * 3
            pixels[start:start + len(row)] = row

    rectangle(0, 0, 32, 20, LEFT_MARKER)
    rectangle(288, 0, 32, 20, RIGHT_MARKER)
    for bit in range(8):
        value = bool(frame_id & (1 << bit))
        rectangle(32 + bit * 32, 24, 32, 48, ONE if value else ZERO)
        rectangle(32 + bit * 32, 80, 32, 48, ZERO if value else ONE)
    # A moving progress bar makes the fixture legible in the retained PNGs.
    rectangle(8, 148, 304, 24, ZERO)
    rectangle(8, 148, max(1, (frame_id + 1) * 304 // FRAME_COUNT), 24, ONE)
    return bytes(pixels)


def make_video(directory: Path) -> dict[str, Any]:
    arguments = [
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
        "-f", "rawvideo", "-pixel_format", "rgb24", "-video_size", "320x180",
        "-framerate", str(SOURCE_FPS), "-i", "pipe:0", "-frames:v",
        str(FRAME_COUNT), "-an", "-c:v", "ffv1", "-pix_fmt", "bgr0",
        str(directory / "fixture.mkv"),
    ]
    record(directory, "ffmpeg-command.json", arguments)
    encoded = run(arguments,
                  input=b"".join(source_frame(i) for i in range(FRAME_COUNT)),
                  timeout=20, check=False)
    (directory / "ffmpeg.log").write_bytes(encoded.stderr)
    encoded.check_returncode()
    result = run([
        "ffprobe", "-v", "error", "-select_streams", "v:0", "-count_frames",
        "-show_entries", "stream=codec_name,width,height,r_frame_rate,nb_read_frames",
        "-of", "json", str(directory / "fixture.mkv"),
    ])
    stream = json.loads(result.stdout)["streams"][0]
    expected = {"codec_name": "ffv1", "width": WIDTH, "height": HEIGHT,
                "r_frame_rate": "12/1", "nb_read_frames": str(FRAME_COUNT)}
    if any(stream.get(name) != value for name, value in expected.items()):
        raise AssertionError(f"incorrect encoded video: {stream}")
    return stream


class Capture:
    """Xvfb TrueColor screenshot; sample only a few pixels during playback."""

    def __init__(self, data: bytes):
        if len(data) < 100:
            raise AssertionError("truncated XWD screenshot")
        header = struct.unpack_from(">25I", data)
        size, version, image_format, _, width, height, xoffset, order = header[:8]
        bits, stride, visual = header[11:14]
        masks = header[14:17]
        offset = size + header[19] * 12
        if (version != 7 or image_format != 2 or visual != 4 or xoffset != 0
                or bits not in (24, 32) or order not in (0, 1) or not all(masks)
                or not (WIDTH <= width <= 4096 and HEIGHT <= height <= 4096)
                or stride < width * (bits // 8)
                or len(data) < offset + stride * height):
            raise AssertionError("expected a complete Xvfb TrueColor XWD")
        self.data, self.width, self.height = data, width, height
        self.offset, self.stride, self.pixel_bytes = offset, stride, bits // 8
        self.order = "little" if order == 0 else "big"
        self.channels = [(mask, (mask & -mask).bit_length() - 1) for mask in masks]

    def pixel(self, x: int, y: int) -> tuple[int, ...]:
        start = self.offset + y * self.stride + x * self.pixel_bytes
        value = int.from_bytes(self.data[start:start + self.pixel_bytes], self.order)
        return tuple(((value & mask) >> shift) * 255 // (mask >> shift)
                     for mask, shift in self.channels)

    def save_png(self, destination: Path) -> None:
        pixels = bytearray()
        for y in range(self.height):
            pixels.append(0)
            for x in range(self.width):
                pixels.extend(self.pixel(x, y))

        def chunk(kind: bytes, content: bytes) -> bytes:
            return (struct.pack(">I", len(content)) + kind + content
                    + struct.pack(">I", zlib.crc32(kind + content)))

        destination.write_bytes(
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">2I5B", self.width, self.height,
                                         8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b"")
        )


def near(actual: tuple[int, ...], expected: tuple[int, ...]) -> bool:
    return all(abs(a - b) <= 25 for a, b in zip(actual, expected))


def visible_frame(capture: Capture) -> Optional[int]:
    # Explicit mpv pixel dimensions and 1-based top/left put a 320x180 image at
    # (0, 0). The fixture therefore checks actual native pixels without relying
    # on the old zero-valued PTY pixel-size fallback.
    if (not near(capture.pixel(16, 10), LEFT_MARKER)
            or not near(capture.pixel(304, 10), RIGHT_MARKER)):
        return None
    frame_id = 0
    for bit in range(8):
        upper = capture.pixel(48 + bit * 32, 48)
        lower = capture.pixel(48 + bit * 32, 104)
        if near(upper, ONE) and near(lower, ZERO):
            frame_id |= 1 << bit
        elif not (near(upper, ZERO) and near(lower, ONE)):
            return None
    if frame_id >= FRAME_COUNT:
        return None
    color = body_color(frame_id)
    if not all(near(capture.pixel(x, y), color) for x, y in ((16, 90), (304, 136))):
        return None
    return frame_id


def process_stats(pid: int) -> Optional[dict[str, Any]]:
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        status = Path(f"/proc/{pid}/status").read_text().splitlines()
        memory = {line.split(":", 1)[0]: int(line.split()[1])
                  for line in status if line.startswith(("VmRSS:", "VmHWM:"))}
        ticks = int(fields[11]) + int(fields[12])
        return {"pid": pid, "start_ticks": int(fields[19]),
                "cpu_seconds": ticks / os.sysconf("SC_CLK_TCK"),
                "rss_kib": memory.get("VmRSS", 0),
                "hwm_kib": memory.get("VmHWM", 0)}
    except (OSError, ValueError, IndexError):
        return None


def stop_owned(identity: Optional[dict[str, Any]]) -> None:
    if identity is None:
        return
    for signum in (signal.SIGTERM, signal.SIGKILL):
        current = process_stats(identity["pid"])
        if current is None or current["start_ticks"] != identity["start_ticks"]:
            return
        try:
            os.kill(identity["pid"], signum)
        except ProcessLookupError:
            return
        time.sleep(0.1)


def stop_process(process: Optional[subprocess.Popen]) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=2)


def child(directory: Path) -> None:
    record(directory, "child.json", process_stats(os.getpid()))
    arguments = json.loads((directory / "mpv-command.json").read_text())
    with (directory / "mpv.log").open("wb") as log:
        player = subprocess.Popen(arguments, stderr=log)
        record(directory, "mpv.json", process_stats(player.pid))
        try:
            status = player.wait(timeout=70)
            record(directory, "mpv-exit.json", {"status": status})
        finally:
            stop_process(player)
    raise SystemExit(status)


class IPC:
    def __init__(self, path: Path):
        self.connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.connection.settimeout(2)
        self.connection.connect(str(path))
        self.buffer = b""
        self.request_id = 0

    def get(self, property_name: str) -> Any:
        self.request_id += 1
        message = {"command": ["get_property", property_name],
                   "request_id": self.request_id}
        self.connection.sendall(json.dumps(message).encode() + b"\n")
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if b"\n" not in self.buffer:
                data = self.connection.recv(65536)
                if not data:
                    raise AssertionError("mpv IPC disconnected unexpectedly")
                self.buffer += data
                if len(self.buffer) > 1024 * 1024:
                    raise AssertionError("unbounded mpv IPC response")
                continue
            line, self.buffer = self.buffer.split(b"\n", 1)
            reply = json.loads(line)
            if reply.get("request_id") == self.request_id:
                if reply.get("error") == "property unavailable":
                    return None
                if reply.get("error") != "success":
                    raise AssertionError(f"mpv IPC property failure: {reply}")
                return reply.get("data")
        raise AssertionError(f"mpv IPC did not answer {property_name}")

    def quit(self) -> None:
        self.connection.sendall(b'{"command":["quit",0]}\n')

    def close(self) -> None:
        self.connection.close()


def wait_for(description: str, observe: Callable[[], Any],
             terminal: subprocess.Popen, timeout: float = 8) -> Any:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = observe()
        if value is not None and value is not False:
            return value
        if terminal.poll() is not None:
            raise AssertionError(f"terminal exited while waiting for {description}")
        time.sleep(0.04)
    raise AssertionError(f"timed out waiting for {description}")


def read_json(path: Path) -> Any:
    return json.loads(path.read_text()) if path.exists() else None


def find_window(terminal: subprocess.Popen) -> Optional[str]:
    result = run(["xdotool", "search", "--onlyvisible", "--pid", str(terminal.pid)],
                 check=False)
    windows = result.stdout.decode().splitlines()
    return windows[0] if result.returncode == 0 and windows else None


def space() -> None:
    # No --window: XTest events go through winit and the PTY keyboard path.
    run(["xdotool", "key", "--clearmodifiers", "space"])


def exercise(binary: Path, directory: Path, report: dict[str, Any]) -> None:
    arguments = [
        "mpv", "--no-config", "--load-scripts=no", "--vo=kitty",
        "--vo-kitty-use-shm=no", "--profile=sw-fast", "--audio=no", "--osc=no",
        "--osd-level=0", "--keep-open=yes", "--pause=yes",
        "--vo-kitty-width=320", "--vo-kitty-height=180", "--vo-kitty-cols=80",
        "--vo-kitty-rows=24", "--vo-kitty-left=1", "--vo-kitty-top=1",
        "--input-ipc-server=" + str(directory / "mpv.sock"),
        str(directory / "fixture.mkv"),
    ]
    record(directory, "mpv-command.json", arguments)
    shell = directory / "shell"
    shell.write_text("#!/bin/sh\nexec " + shlex.join([
        sys.executable, str(Path(__file__).resolve()), "--child-dir", str(directory),
    ]) + "\n")
    shell.chmod(0o700)
    (directory / "kokuban.toml").write_text(
        '[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
        '[window]\ncolumns = 80\nrows = 24\n'
        '[images]\nenabled = true\n[images.kitty]\nallow_file_transfer = false\n'
    )
    environment = os.environ.copy()
    for name in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XDG_RUNTIME_DIR",
                 "KOKUBAN_EXIT_AFTER_FIRST_FRAME"):
        environment.pop(name, None)
    environment.update(KOKUBAN_SHELL=str(shell), WINIT_X11_SCALE_FACTOR="1",
                       LC_ALL="C.UTF-8")
    terminal, ipc = None, None
    started = time.monotonic()
    samples: list[dict[str, Any]] = report["visible_samples"]
    process_samples: list[dict[str, Any]] = report["process_samples"]
    try:
        with (directory / "terminal.log").open("wb") as log:
            terminal = subprocess.Popen([str(binary)], cwd=directory,
                                        env=environment, stdout=log, stderr=log)
        window = wait_for("visible Linux window", lambda: find_window(terminal), terminal)
        run(["xdotool", "windowfocus", "--sync", window])
        player = wait_for("mpv process", lambda: read_json(directory / "mpv.json"), terminal)
        wait_for("private mpv IPC socket", lambda: (directory / "mpv.sock").exists(), terminal)
        ipc = IPC(directory / "mpv.sock")

        def sample(phase: str) -> tuple[Capture, Optional[int]]:
            before = time.monotonic()
            capture = Capture(run(["xwd", "-id", window, "-silent"]).stdout)
            after = time.monotonic()
            frame_id = visible_frame(capture)
            samples.append({"seconds": (before + after) / 2 - started,
                            "capture_seconds": after - before,
                            "frame_id": frame_id, "phase": phase})
            return capture, frame_id

        def metrics(phase: str) -> dict[str, Any]:
            result = {"seconds": time.monotonic() - started, "phase": phase,
                      "kokuban": process_stats(terminal.pid),
                      "mpv": process_stats(player["pid"])}
            if result["kokuban"] is None or result["mpv"] is None:
                raise AssertionError("video process disappeared during measurement")
            process_samples.append(result)
            return result

        def property_is(name: str, expected: Any) -> None:
            wait_for(f"mpv {name}={expected}", lambda: ipc.get(name) == expected, terminal)

        def initial_frame() -> Optional[Capture]:
            capture, frame_id = sample("startup")
            return capture if frame_id == 0 else None

        property_is("pause", True)
        first = wait_for("actual frame 0 pixels", initial_frame, terminal)
        first.save_png(directory / "frame-00.png")
        report["window_pixels"] = [first.width, first.height]
        report["mpv_duration_seconds"] = ipc.get("duration")
        if abs(report["mpv_duration_seconds"] - FRAME_COUNT / SOURCE_FPS) > 0.05:
            raise AssertionError("mpv did not open the expected six-second fixture")

        def play_until(phase: str, done: Callable[[Optional[int], bool], bool]) -> None:
            before = metrics(phase + "-start")
            space()
            property_is("pause", False)
            deadline, next_status, eof = time.monotonic() + 20, 0.0, False
            while time.monotonic() < deadline:
                if terminal.poll() is not None:
                    raise AssertionError("terminal exited during video playback")
                iteration = time.monotonic()
                _, frame_id = sample(phase)
                if iteration >= next_status:
                    eof = ipc.get("eof-reached") is True
                    status = metrics(phase)
                    status["time_pos"] = ipc.get("time-pos")
                    status["eof_reached"] = eof
                    next_status = iteration + 0.25
                if done(frame_id, eof):
                    break
                time.sleep(max(0, SAMPLE_INTERVAL - (time.monotonic() - iteration)))
            else:
                raise AssertionError(f"no meaningful video progress during {phase}")
            after = metrics(phase + "-end")
            elapsed = after["seconds"] - before["seconds"]
            report["active_measurements"].append({
                "phase": phase, "wall_seconds": elapsed,
                **{name: {"cpu_seconds": after[name]["cpu_seconds"] - before[name]["cpu_seconds"],
                          "cpu_percent_one_core": 100 * (after[name]["cpu_seconds"]
                                                         - before[name]["cpu_seconds"]) / elapsed}
                   for name in ("kokuban", "mpv")},
            })

        play_until("playing-before-pause", lambda frame_id, _: frame_id is not None and frame_id >= 18)
        space()
        property_is("pause", True)
        # Let any frame already in the PTY/presentation queue finish before
        # measuring the stable paused image.
        time.sleep(0.25)
        paused, paused_id = sample("pause")
        if paused_id is None or not 18 <= paused_id < 60:
            raise AssertionError(f"unexpected paused frame {paused_id}")
        paused.save_png(directory / "paused.png")

        def assert_stable(phase: str, expected_id: int, duration: float) -> None:
            position = ipc.get("time-pos")
            deadline = time.monotonic() + duration
            while time.monotonic() < deadline:
                if sample(phase)[1] != expected_id:
                    raise AssertionError(f"visible image changed during {phase}")
                time.sleep(SAMPLE_INTERVAL)
            if abs(ipc.get("time-pos") - position) > 0.001:
                raise AssertionError(f"mpv playback time advanced during {phase}")
            metrics(phase)

        assert_stable("pause", paused_id, 1.0)
        if ipc.get("eof-reached") is True:
            raise AssertionError("pause was tested only after EOF")
        report["pause"] = {"frame_id": paused_id, "stable_seconds": 1.0,
                           "input": "XTest Space through winit and PTY"}
        play_until("playing-after-resume", lambda frame_id, eof: frame_id == 71 and eof)
        assert_stable("eof", 71, 0.6)
        sample("eof")[0].save_png(directory / "frame-71.png")
        report["eof"] = {"frame_id": 71, "eof_reached": ipc.get("eof-reached"),
                         "time_pos": ipc.get("time-pos"), "stable_seconds": 0.6}

        active = [item for item in samples if item["phase"].startswith("playing-")]
        ids = [item["frame_id"] for item in samples if item["frame_id"] is not None]
        if ids != sorted(ids):
            raise AssertionError(f"visible frames moved backwards: {ids}")
        observed_transitions = 0
        for phase in ("playing-before-pause", "playing-after-resume"):
            distinct = {item["frame_id"] for item in active
                        if item["phase"] == phase and item["frame_id"] is not None}
            if len(distinct) < 4:
                raise AssertionError(f"too few distinct visible frames in {phase}: {distinct}")
            observed_transitions += len(distinct) - 1
        invalid = sum(item["frame_id"] is None for item in active)
        if len(set(ids)) < 12 or invalid > max(3, len(active) // 10):
            raise AssertionError("insufficient intact video frames in Xvfb captures")
        intervals = [b["seconds"] - a["seconds"] for a, b in zip(active, active[1:])
                     if a["phase"] == b["phase"]]
        active_seconds = sum(item["wall_seconds"] for item in report["active_measurements"])
        report["observations"] = {
            "distinct_visible_frame_ids": sorted(set(ids)),
            "active_capture_count": len(active), "invalid_active_captures": invalid,
            "observed_active_transitions": observed_transitions,
            "observed_frame_rate_lower_bound_hz": observed_transitions / active_seconds,
            "rate_note": "Distinct observed transitions / measured active wall time; sampling lower bound, not source FPS.",
            "capture_interval_seconds": {"min": min(intervals), "median": statistics.median(intervals),
                                         "max": max(intervals)},
            "process_peaks_kib": {
                name: {field: max(item[name][field] for item in process_samples)
                       for field in ("rss_kib", "hwm_kib")}
                for name in ("kokuban", "mpv")},
        }
        ipc.quit()
        terminal.wait(timeout=8)
        exit_result = read_json(directory / "mpv-exit.json")
        if terminal.returncode != 0 or exit_result != {"status": 0}:
            raise AssertionError(f"unclean video exit: terminal={terminal.returncode}, mpv={exit_result}")
        report["clean_exit"] = {"kokuban": terminal.returncode, "mpv": exit_result["status"]}
    finally:
        if ipc is not None:
            ipc.close()
        # Validate /proc start times before signalling any PID. These are only
        # the mpv/driver children created in this private fixture directory.
        stop_owned(read_json(directory / "mpv.json"))
        stop_owned(read_json(directory / "child.json"))
        stop_process(terminal)


def check(binary: Path, artifacts: Optional[Path], profile: str) -> None:
    if sys.platform != "linux":
        raise SystemExit("run this fixture on Linux under Xvfb")
    missing = [name for name in ("ffmpeg", "ffprobe", "mpv", "xdotool", "xwd")
               if shutil.which(name) is None]
    if missing:
        raise SystemExit("missing test programs: " + ", ".join(missing))
    if not binary.is_file():
        raise SystemExit(f"terminal binary not found: {binary}")
    if artifacts:
        artifacts.mkdir(parents=True, exist_ok=True)

    def timed_out(_signum: int, _frame: Any) -> None:
        raise TimeoutError("video smoke exceeded its 75-second internal deadline")

    previous_handler = signal.signal(signal.SIGALRM, timed_out)
    signal.alarm(75)
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="kokuban-video-") as temporary:
        directory = Path(temporary)
        report: dict[str, Any] = {
            "status": "running", "build_profile": profile,
            "source": {"frames": FRAME_COUNT, "fps": SOURCE_FPS,
                       "width": WIDTH, "height": HEIGHT, "codec": "FFV1 Matroska"},
            "transport": "released mpv Kitty direct escapes; shared memory and file transfers disabled",
            "display": "Xvfb X11 software presentation", "requested_capture_interval_seconds": SAMPLE_INTERVAL,
            "measurement_limits": "Debug/declared profile, tiny lossless fixture, no audio or SSH. Capture subprocesses add observer overhead. CPU is process CPU / wall time (100% = one core), not host utilization. RSS/HWM are separate for Kokuban and mpv.",
            "visible_samples": [], "process_samples": [], "active_measurements": [],
        }
        try:
            report["versions"] = {
                name: run(arguments).stdout.decode(errors="replace").splitlines()[:3]
                for name, arguments in (("mpv", ["mpv", "--version"]),
                                        ("ffmpeg", ["ffmpeg", "-version"]),
                                        ("python", [sys.executable, "--version"]))
            }
            report["encoded_stream"] = make_video(directory)
            exercise(binary, directory, report)
            report["status"] = "passed"
            print("PASS: actual mpv Kitty video, visible frame progress, XTest pause/resume, final frame and clean exit")
            print(json.dumps(report["observations"], indent=2))
        except BaseException as error:
            report["status"] = "failed"
            report["error"] = f"{type(error).__name__}: {error}"
            raise
        finally:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, previous_handler)
            report["total_seconds"] = time.monotonic() - started
            record(directory, "report.json", report)
            if artifacts:
                for path in directory.iterdir():
                    if path.is_file() and not path.is_socket():
                        shutil.copy2(path, artifacts / path.name)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, nargs="?")
    parser.add_argument("--artifacts-dir", type=Path)
    parser.add_argument("--build-profile", default="debug")
    parser.add_argument("--child-dir", type=Path, help=argparse.SUPPRESS)
    arguments = parser.parse_args()
    if arguments.child_dir:
        child(arguments.child_dir)
    elif arguments.binary:
        check(arguments.binary.resolve(), arguments.artifacts_dir, arguments.build_profile)
    else:
        parser.error("a terminal binary is required")


if __name__ == "__main__":
    main()
