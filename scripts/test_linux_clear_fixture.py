"""Reject false blank-screen evidence and execute the real clear in a host PTY."""

import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest

from linux_clear_fixture import BACKGROUND, Capture


def xwd(width=3, height=2, changed=None):
    header = [0] * 25
    header[0:8] = [100, 7, 2, 24, width, height, 0, 0]
    header[11:17] = [32, width * 4, 4, 0xff0000, 0xff00, 0xff]
    pixel = bytes((*reversed(BACKGROUND), 0))
    body = bytearray(pixel * width * height)
    if changed is not None:
        body[changed * 4:changed * 4 + 4] = bytes((0, 0, 255, 0))
    return struct.pack('>25I', *header) + body


class ClearEvidenceTests(unittest.TestCase):
    def test_every_pixel_must_be_background(self):
        self.assertTrue(Capture(xwd()).blank())
        for position in range(6):
            self.assertFalse(Capture(xwd(changed=position)).blank())

    def test_unused_truecolor_padding_does_not_change_visible_pixels(self):
        data = bytearray(xwd())
        data[-1] = 255
        self.assertTrue(Capture(bytes(data)).blank())

    def test_truncated_screenshot_is_not_blank_evidence(self):
        with self.assertRaises(AssertionError):
            Capture(xwd()[:-1])

    @unittest.skipUnless(shutil.which('clear') and shutil.which('infocmp'), 'requires ncurses tools')
    def test_actual_clear_and_input_complete_on_a_real_pty(self):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
        with tempfile.TemporaryDirectory(prefix='kokuban-clear-test-') as temporary:
            directory = Path(temporary)
            code = 'from pathlib import Path; from linux_clear_fixture import remote_clear; import sys; remote_clear(Path(sys.argv[1]))'
            environment = os.environ.copy()
            environment.update(TERM='xterm-256color', PYTHONPATH=str(Path(__file__).resolve().parent))
            child = subprocess.Popen([sys.executable, '-c', code, str(directory)], stdin=slave,
                                     stdout=slave, stderr=slave, env=environment)
            os.close(slave)
            output = bytearray()
            deadline = time.monotonic() + 8
            sent = False
            try:
                while time.monotonic() < deadline and child.poll() is None:
                    if (directory / 'clear-ready.json').exists():
                        (directory / 'clear-go').touch()
                    if (directory / 'clear-done.json').exists():
                        (directory / 'clear-continue').touch()
                    if (directory / 'clear-input-ready.json').exists() and not sent:
                        os.write(master, b'after-clear\n')
                        sent = True
                    if select.select([master], [], [], 0.025)[0]:
                        try:
                            output.extend(os.read(master, 65536))
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                self.assertEqual(child.wait(timeout=1), 0, output.decode(errors='replace')[-2000:])
                metadata = json.loads((directory / 'clear-metadata.json').read_text())
                self.assertEqual(metadata['tty_fds'], [True, True, True])
                actual = (directory / 'clear.bin').read_bytes()
                self.assertIn(b'\x1b[3J', actual)
                self.assertIn(actual, output)
                self.assertEqual(json.loads((directory / 'clear-input.json').read_text()), 'after-clear')
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=2)
                os.close(master)


if __name__ == '__main__':
    unittest.main()
