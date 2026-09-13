"""Protocol, pixel-oracle and lifecycle tests; no display or performance load."""

import fcntl
import ctypes
import json
import os
from pathlib import Path
import pty
import runpy
import select
import shutil
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
XLIB = runpy.run_path(str(PATH.with_name("terminal-frame-xlib.py")))


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
    def test_startup_spare_pixels_can_be_normalized_without_accepting_a_different_grid(self):
        geometry = [24, 80, 720, 408]
        remainder = {"window": "11", "pixels": [721, 409]}
        helper = {"window": "10", "pixels": [1, 1]}
        choose = RUNNER["cell_remainder_window"]
        self.assertEqual(choose([helper, remainder], geometry, [9, 17]), remainder)
        for pixels in ([729, 409], [721, 425], [719, 409], [721, 407], [720, 408]):
            with self.subTest(pixels=pixels):
                self.assertIsNone(choose([{**remainder, "pixels": pixels}], geometry, [9, 17]))
        self.assertIsNone(choose([remainder, {**remainder, "window": "12"}], geometry, [9, 17]))
        self.assertIsNone(choose([remainder, {"window": "13", "pixels": [720, 408]}], geometry, [9, 17]))

    def test_auxiliary_windows_do_not_hide_the_unique_calibrated_terminal_window(self):
        geometry = [24, 80, 720, 408]
        candidates = [{"window": "10", "pixels": [1, 1]},
                      {"window": "11", "pixels": [720, 408]},
                      {"window": "12", "capture_error": "window disappeared"}]
        self.assertEqual(RUNNER["matching_window"](candidates, geometry), "11")
        self.assertIsNone(RUNNER["matching_window"](candidates[:1], geometry))
        self.assertIsNone(RUNNER["matching_window"](
            candidates + [{"window": "13", "pixels": [720, 408]}], geometry))

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


class XlibObserverTests(unittest.TestCase):
    def setUp(self):
        self.x11, self.xtst = Mock(), Mock()
        self.x11.XOpenDisplay.return_value = 1234
        self.x11.XSetErrorHandler.return_value = 99
        self.x11.XKeysymToKeycode.side_effect = lambda display, symbol: {97: 38, 98: 56}[symbol]
        self.x11.XServerVendor.return_value = b"test X11"
        self.x11.XVendorRelease.return_value = 1
        self.xtst.XTestQueryExtension.return_value = 1
        self.xtst.XTestFakeKeyEvent.return_value = 1
        self.x11.XQueryPointer.return_value = 1
        self.x11.XGetInputFocus.side_effect = lambda display, focus, revert: setattr(focus._obj, "value", 77)
        self.visual = XLIB["Visual"]()
        self.visual.visual_class = 4

        def attributes(display, window, pointer):
            pointer._obj.width, pointer._obj.height = 6, 5
            pointer._obj.visual = ctypes.pointer(self.visual)
            return 1

        self.x11.XGetWindowAttributes.side_effect = attributes
        raw = xwd()[100:]
        self.pixels = ctypes.create_string_buffer(raw)
        self.image = XLIB["XImagePrefix"]()
        for name, value in {"width": 6, "height": 5, "xoffset": 0, "format": 2,
                            "data": ctypes.addressof(self.pixels), "byte_order": 0,
                            "bitmap_unit": 32, "bitmap_bit_order": 0, "bitmap_pad": 32,
                            "depth": 24, "bytes_per_line": 28, "bits_per_pixel": 32,
                            "red_mask": 0xff0000, "green_mask": 0xff00, "blue_mask": 0xff}.items():
            setattr(self.image, name, value)
        self.x11.XGetImage.return_value = ctypes.pointer(self.image)

    def observer(self):
        observer = XLIB["XlibObserver"](77, RUNNER["Frame"], api=(self.x11, self.xtst))
        self.addCleanup(observer.close)
        return observer

    def test_persistent_capture_uses_the_same_rgb_oracle_and_frees_every_image(self):
        observer = self.observer()
        for _ in range(2):
            frame, started, finished = observer.capture(1)
            self.assertTrue(frame.matches([1, 1, 5, 4], RUNNER["COLORS"][0]))
            self.assertFalse(frame.matches([1, 1, 5, 4], RUNNER["COLORS"][1]))
            self.assertLessEqual(started, finished)
            self.assertEqual(struct.unpack_from(">I", frame.data)[0], 101)
        self.assertEqual(self.x11.XGetImage.call_count, 2)
        self.assertEqual(self.x11.XDestroyImage.call_count, 2)
        self.x11.XOpenDisplay.assert_called_once_with(None)

    def test_invalid_image_is_destroyed_without_reading_its_data_pointer(self):
        observer = self.observer()
        self.image.data = None
        with patch.object(ctypes, "string_at") as read, self.assertRaises(ValueError):
            observer.capture(1)
        read.assert_not_called()
        self.x11.XDestroyImage.assert_called_once()

    def test_null_image_is_reported_without_destroying_a_null_pointer(self):
        observer = self.observer()
        self.x11.XGetImage.return_value = ctypes.POINTER(XLIB["XImagePrefix"])()
        with self.assertRaisesRegex(RuntimeError, "no image"):
            observer.capture(1)
        self.x11.XDestroyImage.assert_not_called()

    def test_protocol_error_during_image_capture_releases_image_and_restores_handler(self):
        observer = self.observer()

        def image_error(*args):
            error = XLIB["XErrorEvent"]()
            error.error_code, error.request_code = 9, 73
            observer.handler(1234, ctypes.pointer(error))
            return ctypes.pointer(self.image)

        self.x11.XGetImage.side_effect = image_error
        with self.assertRaisesRegex(RuntimeError, "X11 protocol error"):
            observer.capture(1)
        self.x11.XDestroyImage.assert_called_once()
        observer.close()
        observer.close()
        self.x11.XCloseDisplay.assert_called_once_with(1234)
        self.assertEqual(self.x11.XSetErrorHandler.call_args.args, (99,))

    def test_focus_and_key_state_are_checked_before_injection(self):
        observer = self.observer()
        observer.prepare_input()
        observer.inject("b")
        self.assertEqual([call.args for call in self.xtst.XTestFakeKeyEvent.call_args_list],
                         [(1234, 56, 1, 0), (1234, 56, 0, 0)])
        self.x11.XFlush.assert_called_once_with(1234)
        self.x11.XGetInputFocus.side_effect = lambda display, focus, revert: setattr(focus._obj, "value", 78)
        with self.assertRaisesRegex(AssertionError, "focus"):
            observer.prepare_input()

    def test_locked_modifiers_are_rejected_without_changing_user_keyboard_state(self):
        observer = self.observer()

        def locked(*args):
            args[-1]._obj.value = 2  # LockMask, even with no physically pressed key.
            return 1

        self.x11.XQueryPointer.side_effect = locked
        with self.assertRaisesRegex(AssertionError, "locked modifiers"):
            observer.prepare_input()
        self.xtst.XTestFakeKeyEvent.assert_not_called()

    def test_failed_key_release_is_retried_on_close_and_connection_is_closed(self):
        observer = self.observer()
        self.xtst.XTestFakeKeyEvent.side_effect = [1, 0, 1]
        with self.assertRaisesRegex(RuntimeError, "release failed"):
            observer.inject("a")
        observer.close()
        self.assertEqual(self.xtst.XTestFakeKeyEvent.call_args.args, (1234, 38, 0, 0))
        self.x11.XCloseDisplay.assert_called_once_with(1234)

    def test_constructor_failure_closes_display_and_restores_previous_handler(self):
        self.xtst.XTestQueryExtension.return_value = 0
        with self.assertRaisesRegex(RuntimeError, "unavailable"):
            self.observer()
        self.x11.XCloseDisplay.assert_called_once_with(1234)
        self.assertEqual(self.x11.XSetErrorHandler.call_args.args, (99,))

    @unittest.skipUnless(sys.platform.startswith("linux") and shutil.which("cc")
                         and Path("/usr/include/X11/Xlib.h").exists(), "native Linux Xlib headers unavailable")
    def test_ctypes_layout_matches_the_installed_linux_public_headers(self):
        # Tiny ABI probe only: no X server, library linking or performance load.
        checks = []
        for cls_name, c_name in (("XImagePrefix", "XImage"), ("Visual", "Visual"),
                                 ("WindowAttributes", "XWindowAttributes"), ("XErrorEvent", "XErrorEvent")):
            cls = XLIB[cls_name]
            for field, _ in cls._fields_:
                native = "class" if field in ("visual_class", "window_class") else field
                checks.append((f"offsetof({c_name}, {native})", getattr(cls, field).offset))
            if cls_name != "XImagePrefix":
                checks.append((f"sizeof({c_name})", ctypes.sizeof(cls)))
        source = '#include <stdio.h>\n#include <stddef.h>\n#include <X11/Xlib.h>\nint main(void) {\n'
        source += "\n".join(f'printf("%zu\\n", {expression});' for expression, _ in checks) + "\nreturn 0;}\n"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "abi.c").write_text(source)
            subprocess.run(["cc", "-std=c11", str(path / "abi.c"), "-o", str(path / "abi")],
                           check=True, capture_output=True, timeout=10)
            result = subprocess.run([str(path / "abi")], check=True, capture_output=True, text=True, timeout=10)
        self.assertEqual(list(map(int, result.stdout.split())), [expected for _, expected in checks])


class XlibIntegrationTests(unittest.TestCase):
    def exercise(self, fail_capture=False):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "sample"
            args = SimpleNamespace(rows=5, columns=6, timeout=1, observer="xlib", warmup=1,
                                   events=1, settle_seconds=0, poll_interval=0)
            geometry, state = [5, 6, 54, 85], {"index": 0, "color": 0, "timed": False}
            observer, process, cleanup = Mock(), Mock(pid=123), Mock()
            process.poll.return_value, process.wait.return_value = None, 0
            observer.library_provenance.return_value = {"type": "xlib"}
            observer.prepare_input.side_effect = lambda: state.update(timed=True)

            def inject(key):
                index = state["index"]
                self.assertEqual(key, "b" if index % 2 == 0 else "a")
                stamp = time.perf_counter_ns()
                state.update(index=index + 1, color=(index + 1) % 2)
                RUNNER["record"](directory, f"event-{index:04d}.json", {
                    "index": index, "input_hex": key.encode().hex(), "color": list(RUNNER["COLORS"][state["color"]]),
                    "received_ns": stamp, "written_ns": stamp,
                    "geometry_before": geometry, "geometry_after": geometry})
                RUNNER["record"](directory, "child-exit.json", {"events": index + 1, "status": "passed"})

            def observed_capture(timeout):
                if fail_capture:
                    raise RuntimeError("injected Xlib capture failure")
                started = time.perf_counter_ns()
                frame = RUNNER["Frame"](xwd(width=54, height=85, color=RUNNER["COLORS"][state["color"]]))
                finished = time.perf_counter_ns()
                state["timed"] = False
                return frame, started, finished

            def external_command(*args, **kwargs):
                self.assertFalse(state["timed"], "subprocess inside the Xlib input/capture interval")
                return subprocess.CompletedProcess(args[0], 0, b"77\n", b"")

            observer.inject.side_effect, observer.capture.side_effect = inject, observed_capture
            command = Mock(return_value=(["terminal", str(RUNNER["COMPARISON_PATH"])], {}))
            with patch.dict(RUNNER["HELPERS"], terminal_command=command), \
                    patch.dict(GLOBALS, stop_owned_child=cleanup, capture=lambda *args: (
                        RUNNER["Frame"](xwd(width=54, height=85)), 0, 0),
                        command_observation=lambda *args: {"status": 0, "output": "test libraries"}), \
                    patch.object(RUNNER["subprocess"], "Popen", return_value=process), \
                    patch.object(RUNNER["subprocess"], "run", side_effect=external_command), \
                    patch.object(RUNNER["runpy"], "run_path", return_value={"XlibObserver": Mock(return_value=observer)}):
                sample = EXECUTE("kokuban", Path("terminal"), "test", directory, args, [9, 17])
            observer.close.assert_called_once_with()
            cleanup.assert_called_once_with(directory)
            self.assertEqual(json.loads((directory / "sample.json").read_text()), sample)
            if not fail_capture:
                self.assertTrue((directory / "first-transition.xwd").exists())
            return sample

    def test_xlib_path_does_not_launch_subprocesses_inside_input_to_capture_intervals(self):
        sample = self.exercise()
        self.assertEqual(sample.get("status"), "passed", sample.get("error"))
        self.assertEqual((len(sample["warmup"]), len(sample["measurements"])), (1, 1))
        self.assertEqual(sample["observer"]["type"], "xlib")

    def test_capture_failure_closes_observer_and_terminal_and_retains_the_error(self):
        sample = self.exercise(fail_capture=True)
        self.assertEqual(sample["status"], "failed")
        self.assertIn("injected Xlib capture failure", sample["error"])


if __name__ == "__main__":
    unittest.main()
