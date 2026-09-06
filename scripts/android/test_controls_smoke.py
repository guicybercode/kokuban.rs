import os
from pathlib import Path
import pty
import subprocess
import tempfile
import time
import unittest
import xml.etree.ElementTree as ET

from controls_smoke import input_capabilities, latest_geometry, screen_origin, shell_fixture


class ControlsEvidenceTests(unittest.TestCase):
    def test_unknown_help_is_not_misclassified_as_missing_mouse_support(self):
        with self.assertRaises(ValueError):
            input_capabilities("Unknown command: --help")
        self.assertEqual(input_capabilities("Usage: input <command>\nThe sources are:\n    mouse\n"
                                            "    keyboard\n    keycombination -t duration"),
                         {"mouse": True, "combinations": True})
        self.assertEqual(input_capabilities("Usage: input <command>\nThe sources are:\n    keyboard\n"),
                         {"mouse": False, "combinations": False})

    def test_coordinates_follow_last_presented_geometry_and_screen_origin(self):
        first = ("metric geometry left=0 top=20 right=400 bottom=900 "
                 "cell_width=10 cell_height=20 columns=40 rows=44 scroll_offset=0")
        current = ("metric geometry left=10 top=30 right=410 bottom=910 "
                   "cell_width=10 cell_height=20 columns=40 rows=44 scroll_offset=8")
        layout = latest_geometry(first + "\nmetric frame=9\n" + current)
        root = ET.fromstring('''<hierarchy>
          <node package="app" class="android.widget.Button" bounds="[17,963][117,1063]" />
          <node package="app" class="android.widget.Button" bounds="[117,963][217,1063]" />
          <node package="keyboard" class="android.widget.Button" bounds="[0,0][50,50]" />
        </hierarchy>''')
        origin = screen_origin(root, "app", layout)
        self.assertEqual(origin, (7, 53))
        self.assertEqual(layout.cell_center(2, 2, origin), (42, 133))
        self.assertEqual(layout.scroll_offset, 8)
        with self.assertRaises(ValueError):
            layout.cell_center(layout.columns, 0)

    def test_missing_or_inconsistent_geometry_cannot_generate_injected_taps(self):
        with self.assertRaises(LookupError):
            latest_geometry("first frame presented")
        with self.assertRaises(ValueError):
            latest_geometry("metric geometry left=0 top=0 right=40 bottom=80 "
                            "cell_width=10 cell_height=20 columns=8 rows=4 scroll_offset=0")

    @unittest.skipUnless(os.name == "posix", "Fixture requires a Unix PTY, as on Android")
    def test_shell_fixture_reads_real_raw_pty_bytes_and_detects_extra_events(self):
        # Host-only verification of the harness: this does not prove Android
        # routing. Extra input must be retained, not silently truncated to pass.
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "directory with spaces"
            directory.mkdir()
            script = directory / "session.sh"
            script.write_text(shell_fixture(str(directory), [("sample", b"\x03\x1b\t")]))
            master, slave = pty.openpty()
            process = subprocess.Popen(["/bin/sh", str(script)], stdin=slave, stdout=slave, stderr=slave,
                                       start_new_session=True)
            os.close(slave)
            try:
                deadline = time.monotonic() + 5
                while not (directory / "sample.ready").exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue((directory / "sample.ready").exists())
                payload = b"\x03\x1b\tEXTRA"
                os.write(master, payload)
                deadline = time.monotonic() + 5
                while not (directory / "sample.done").exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue((directory / "sample.done").exists())
                self.assertEqual((directory / "sample.bin").read_bytes(), payload)
                self.assertFalse((directory / "finished").exists())
                (directory / "sample.release").touch()
                self.assertEqual(process.wait(timeout=5), 0)
                self.assertEqual((directory / "finished").read_text(), "DONE")
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)
                os.close(master)


if __name__ == "__main__":
    unittest.main()
