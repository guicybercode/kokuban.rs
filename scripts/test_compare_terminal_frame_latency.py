"""Protocol, pixel-oracle and lifecycle tests; no display or performance load."""

import fcntl
import json
import os
from pathlib import Path
import pty
import runpy
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch


PATH = Path(__file__).with_name("compare-terminal-frame-latency.py")
RUNNER = runpy.run_path(str(PATH))
EXECUTE = RUNNER["execute_sample"]
GLOBALS = EXECUTE.__globals__


def xwd(width=6, height=5, bits=32, order=0, color=(29, 173, 83), padding=4):
    stride = width * (bits // 8) + padding
    header = [100, 7, 2, 24, width, height, 0, order, 32, order, 32, bits,
              stride, 4, 0xff0000, 0xff00, 0xff, 8, 256, 0, width, height, 0, 0, 0]
    pixel = ((color[0] << 16) | (color[1] << 8) | color[2])
    if bits == 32:
        pixel |= 0xe7000000  # Unused byte must not be mistaken for opacity.
    data = pixel.to_bytes(bits // 8, "little" if order == 0 else "big") * width + b"!" * padding
    return struct.pack(">25I", *header) + data * height


class PixelOracleTests(unittest.TestCase):
    def test_rgb_validation_supports_both_orders_depths_padding_and_unused_byte(self):
        for bits in (24, 32):
            for order in (0, 1):
                with self.subTest(bits=bits, order=order):
                    frame = RUNNER["Frame"](xwd(bits=bits, order=order))
                    self.assertTrue(frame.matches([1, 1, 5, 4], RUNNER["COLORS"][0]))
                    self.assertFalse(frame.matches([1, 1, 5, 4], RUNNER["COLORS"][1]))

    def test_one_incorrect_pixel_rejects_a_partially_updated_rectangle(self):
        data = bytearray(xwd())
        frame = RUNNER["Frame"](data)
        data[frame.offset + 2 * frame.stride + 3 * frame.pixel_bytes] ^= 1
        self.assertFalse(frame.matches([1, 1, 5, 4], RUNNER["COLORS"][0]))

    def test_border_pixels_do_not_define_the_region_color(self):
        data = bytearray(xwd())
        data[100:104] = b"\0" * 4
        self.assertTrue(RUNNER["Frame"](data).matches([1, 1, 5, 4], RUNNER["COLORS"][0]))

    def test_truncation_and_unsupported_masks_fail_closed(self):
        for data in (b"", xwd()[:99], xwd()[:-1]):
            with self.assertRaises(ValueError):
                RUNNER["Frame"](data)
        data = bytearray(xwd())
        struct.pack_into(">I", data, 14 * 4, 0x3ff00000)
        with self.assertRaisesRegex(ValueError, "packed"):
            RUNNER["Frame"](data)
        with self.assertRaises(ValueError):
            RUNNER["Frame"](xwd()).matches([0, 0, 7, 5], RUNNER["COLORS"][0])


class TimingTests(unittest.TestCase):
    def test_percentiles_preserve_raw_order_and_disclose_weak_small_sample_tail(self):
        result = RUNNER["distribution"]([3, 1, 2])
        self.assertEqual((result["median"], result["p95"], result["p99"]), (2, 3, 3))
        self.assertEqual(result["samples"], [3, 1, 2])
        self.assertFalse(result["p99_has_at_least_100_samples"])
        result = RUNNER["distribution"](list(range(1, 101)))
        self.assertEqual((result["p95"], result["p99"]), (95, 99))
        for values in ([], [-1], [float("nan")], [float("inf")]):
            with self.assertRaises(ValueError):
                RUNNER["distribution"](values)

    def test_wrong_key_color_geometry_and_event_order_cannot_pass(self):
        geometry = [5, 6, 54, 85]
        event = {"index": 0, "input_hex": b"b".hex(), "color": list(RUNNER["COLORS"][1]),
                 "geometry_before": geometry, "geometry_after": geometry,
                 "received_ns": 110, "written_ns": 120}
        RUNNER["verify_event"](event, 0, geometry, 100, 130)
        # A parent may observe pixels while the child is preempted immediately
        # after write() returns, before it records its write acknowledgment.
        RUNNER["verify_event"]({**event, "written_ns": 140}, 0, geometry, 100, 130)
        for key, value in (("index", 1), ("input_hex", "61"), ("color", [0, 0, 0]),
                           ("geometry_after", [5, 6, 0, 0]), ("received_ns", 99),
                           ("received_ns", 131), ("written_ns", 109)):
            with self.subTest(key=key, value=value), self.assertRaises(AssertionError):
                RUNNER["verify_event"]({**event, key: value}, 0, geometry, 100, 130)

    def test_failed_processes_and_warmup_never_enter_statistics(self):
        observation = {"capture_started_ns": 10, "capture_finished_ns": 20, "validated_ns": 23}
        good = {"input_to_observed_frame_upper_bound_seconds": .1, "injection_seconds": .01,
                "observations": [observation]}
        bad = {**good, "input_to_observed_frame_upper_bound_seconds": 999}
        report = {"cell_size_calibration": {"status": "passed"}, "terminals": {name: {"samples": [
            {"status": "passed", "measurements": [good], "warmup": [bad]},
            {"status": "failed", "measurements": [bad], "warmup": []}]}
            for name in ("kokuban", "kitty")}}
        RUNNER["summarize"](report, SimpleNamespace(samples=2, events=1))
        self.assertFalse(report["comparability"]["checks_passed"])
        for terminal in report["terminals"].values():
            summary = terminal["summary"]["input_to_observed_frame_upper_bound_seconds"]
            self.assertEqual((summary["count"], summary["median"]), (1, .1))


class ChildProtocolTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.master, self.slave = pty.openpty()
        self.addCleanup(os.close, self.master)
        self.addCleanup(os.close, self.slave)
        self.geometry = [5, 6, 54, 85]
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("4H", *self.geometry))
        (self.directory / "case.json").write_text(json.dumps({"geometry": self.geometry, "timeout": 3}))
        (self.directory / "window-ready").touch()
        # Keep the actual PTY/protocol; only Linux /proc identity is substituted
        # so this test also runs on macOS. Cleanup uses the real Popen handle.
        code = ("import runpy,sys; from pathlib import Path; "
                "r=runpy.run_path(sys.argv[1]); f=r['controlled_child']; "
                "f.__globals__['process_stats']=lambda pid: {'pid':pid}; f(Path(sys.argv[2]))")
        self.process = subprocess.Popen([sys.executable, "-c", code, str(PATH.resolve()), str(self.directory)],
                                        stdin=self.slave, stdout=self.slave, stderr=subprocess.DEVNULL)
        self.addCleanup(self.stop)
        self.wait_json("ready.json")

    def stop(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=3)

    def wait_json(self, name):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], .01)[0]:
                os.read(self.master, 65536)
            path = self.directory / name
            if path.exists():
                return json.loads(path.read_text())
        self.fail(f"missing child record {name}; exit={self.process.poll()}")

    def test_alternating_bytes_generate_exact_events_and_clean_shutdown(self):
        for index, key in enumerate((b"b", b"a", b"b")):
            os.write(self.master, key)
            event = self.wait_json(f"event-{index:04d}.json")
            self.assertEqual(event["input_hex"], key.hex())
            self.assertEqual(event["color"], list(RUNNER["COLORS"][(index + 1) % 2]))
            self.assertEqual(event["geometry_after"], self.geometry)
        (self.directory / "finish").touch()
        self.assertEqual(self.process.wait(timeout=3), 0)
        self.assertEqual(json.loads((self.directory / "child-exit.json").read_text()),
                         {"events": 3, "status": "passed"})

    def test_duplicate_or_wrong_input_fails_instead_of_counting_a_frame(self):
        os.write(self.master, b"bb")
        error = self.wait_json("child-error.json")
        self.assertIn("received b'bb'", error["error"])
        self.assertNotEqual(self.process.wait(timeout=3), 0)
        self.assertFalse((self.directory / "event-0000.json").exists())

    def test_resize_between_keys_fails_before_emitting_a_measured_frame(self):
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("4H", 6, 6, 54, 102))
        os.write(self.master, b"b")
        error = self.wait_json("child-error.json")
        self.assertIn("geometry changed", error["error"])
        self.assertNotEqual(self.process.wait(timeout=3), 0)


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name) / "sample"
        self.args = SimpleNamespace(rows=5, columns=6, timeout=.01, screen="alternate", backend="x11")
        self.cleanup = Mock()
        self.command = Mock(return_value=(["terminal", str(RUNNER["COMPARISON_PATH"])], {}))

    def execute(self, process):
        with patch.dict(RUNNER["HELPERS"], terminal_command=self.command), \
                patch.dict(GLOBALS, stop_owned_child=self.cleanup), \
                patch.object(RUNNER["subprocess"], "Popen", return_value=process), \
                patch.object(RUNNER["subprocess"], "run", side_effect=OSError("injected display failure")):
            return EXECUTE("kokuban", Path("terminal"), "test", self.directory, self.args, [9, 17])

    def test_display_failure_retains_failure_and_stops_terminal_and_owned_child(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        sample = self.execute(process)
        self.assertEqual(sample["status"], "failed")
        self.assertIn("injected display failure", sample["error"])
        process.terminate.assert_called_once_with()
        self.cleanup.assert_called_once_with(self.directory)
        self.assertEqual(json.loads((self.directory / "sample.json").read_text()), sample)

    def test_unresponsive_terminal_is_killed_and_reaped(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired("terminal", 3), 0]
        sample = self.execute(process)
        self.assertEqual(sample["status"], "failed")
        process.kill.assert_called_once_with()
        self.assertEqual(process.wait.call_count, 2)
        self.cleanup.assert_called_once_with(self.directory)


if __name__ == "__main__":
    unittest.main()
