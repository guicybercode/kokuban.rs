"""ADB input preparation preserves commands while refreshing event timestamps."""

import unittest

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


if __name__ == "__main__":
    unittest.main()
