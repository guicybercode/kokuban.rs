"""ADB input preparation preserves commands while refreshing event timestamps."""

import unittest
from unittest.mock import patch
import subprocess

from device import Device


class TextInjectionTests(unittest.TestCase):
    def test_long_commands_keep_spaces_across_short_bursts(self):
        device = Device.__new__(Device)
        calls = []
        device.shell = lambda *args: calls.append(args)
        command = "printf OPEN > /data/user/0/com.kokuban.terminal/files/proof"
        device.type_text(command)
        self.assertGreater(len(calls), 1)
        self.assertTrue(all(call[:2] == ("input", "text") for call in calls))
        chunks = [call[2].replace("%s", " ") for call in calls]
        self.assertEqual("".join(chunks), command)
        self.assertTrue(all(1 <= len(chunk) <= 8 for chunk in chunks))

    def test_rejects_literal_adb_escape_instead_of_corrupting_a_command(self):
        device = Device.__new__(Device)
        with self.assertRaises(ValueError):
            device.type_text("printf %s text")


class AdbFailureTests(unittest.TestCase):
    def test_large_failed_dump_keeps_exit_and_both_ends_readable(self):
        device = Device.__new__(Device)
        device.command = ["adb"]
        result = subprocess.CompletedProcess([], 1, "BEGIN" + "x" * 390000 + "END", "device offline")
        with patch("device.subprocess.run", return_value=result):
            with self.assertRaises(RuntimeError) as error:
                device.adb("shell", "dumpsys input_method")
        message = str(error.exception)
        self.assertLess(len(message), 5000)
        for expected in ("exit 1", "device offline", "BEGIN", "END", "omitted"):
            self.assertIn(expected, message)

    def test_binary_capture_failure_retains_the_transport_error(self):
        device = Device.__new__(Device)
        device.command = ["adb"]
        result = subprocess.CompletedProcess([], 1, b"", b"error: device offline\n")
        with patch("device.subprocess.run", return_value=result):
            with self.assertRaisesRegex(RuntimeError, "device offline"):
                device.adb("exec-out", "screencap", "-p", binary=True)


if __name__ == "__main__":
    unittest.main()
