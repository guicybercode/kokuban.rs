"""Real ncurses clear over the existing SSH PTY, with presented-pixel checks."""

import base64
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import time
import zlib

BACKGROUND = (26, 26, 46)
HISTORY = (96, 32, 32)
LIVE = (32, 64, 96)
IMAGE = (255, 0, 0)


def record(directory, name, value):
    path = directory / (name + '.pending')
    path.write_text(json.dumps(value, indent=2) + '\n')
    path.replace(directory / name)


def wait_file(path, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.025)
    raise AssertionError(f'timed out waiting for clear fixture barrier {path.name}')


def remote_clear(directory):
    """The actual clear subprocess inherits the SSH controlling terminal."""
    term = os.environ.get('TERM', '')
    info = subprocess.run(['infocmp', '-x', '-1', term], capture_output=True, timeout=5)
    (directory / 'clear-terminfo.txt').write_bytes(info.stdout + info.stderr)
    info.check_returncode()
    emitted = subprocess.run(['clear'], capture_output=True, timeout=5, check=True)
    (directory / 'clear.bin').write_bytes(emitted.stdout)
    if not emitted.stdout or len(emitted.stdout) > 4096:
        raise AssertionError('clear did not emit a bounded terminal sequence')
    version = subprocess.run(['clear', '-V'], capture_output=True, timeout=5)
    record(directory, 'clear-metadata.json', {
        'term': term, 'clear_version': (version.stdout + version.stderr).decode(errors='replace').strip(),
        'clear_stdout_hex': emitted.stdout.hex(),
        'clear_capture_note': 'Metadata invocation captured separately; tested invocation inherits the SSH PTY.',
        'has_scrollback_erase': b'\x1b[3J' in emitted.stdout,
        'tty_fds': [os.isatty(fd) for fd in (0, 1, 2)],
    })
    if b'\x1b[3J' not in emitted.stdout:
        raise AssertionError('this scrollback-clear fixture requires remote terminfo E3 support')
    columns, rows = os.get_terminal_size()
    output = sys.stdout.buffer
    output.write(b'\x1b[0m\x1b[?25l')
    for _ in range(rows * 4):
        output.write(b'\x1b[48;2;96;32;32m' + b' ' * (columns - 1) + b'\r\n')
    output.write(b'\x1b[48;2;32;64;96m\x1b[2J\x1b[H\x1b[0m\x1b[3;10H')
    image = base64.b64encode(zlib.compress(bytes((*IMAGE, 255)) * 120 * 36))
    output.write(b'\x1b_Ga=T,f=32,o=z,s=120,v=36,i=901,q=2,C=1;' + image + b'\x1b\\')
    output.flush()
    record(directory, 'clear-ready.json', {'columns': columns, 'rows': rows})
    wait_file(directory / 'clear-go')
    subprocess.run(['clear'], check=True, timeout=5)
    record(directory, 'clear-done.json', True)
    wait_file(directory / 'clear-continue')
    print('\x1b[?25hCLEAR INPUT>', flush=True)
    record(directory, 'clear-input-ready.json', True)
    received = input()
    if received != 'after-clear':
        raise AssertionError(f'input after clear differs: {received!r}')
    record(directory, 'clear-input.json', received)


class Capture:
    """Bounded Xvfb TrueColor capture; inspect pixels without encoding every poll."""

    def __init__(self, data):
        if len(data) < 100:
            raise AssertionError('truncated XWD')
        h = struct.unpack_from('>25I', data)
        self.width, self.height = h[4:6]
        self.bytes_per_pixel, self.stride = h[11] // 8, h[12]
        self.offset = h[0] + h[19] * 12
        if (h[1] != 7 or h[2] != 2 or h[6] or h[7] not in (0, 1)
                or h[11] not in (24, 32) or h[13] != 4 or not all(h[14:17])
                or not (1 <= self.width <= 4096 and 1 <= self.height <= 4096)
                or self.stride < self.width * self.bytes_per_pixel
                or len(data) < self.offset + self.stride * self.height):
            raise AssertionError('expected complete bounded Xvfb TrueColor XWD')
        self.data = data
        self.order = 'little' if h[7] == 0 else 'big'
        self.channels = [(mask, (mask & -mask).bit_length() - 1) for mask in h[14:17]]

    def pixel(self, x, y):
        offset = self.offset + y * self.stride + x * self.bytes_per_pixel
        value = int.from_bytes(self.data[offset:offset + self.bytes_per_pixel], self.order)
        return tuple(((value & mask) >> shift) * 255 // (mask >> shift) for mask, shift in self.channels)

    def blank(self):
        # All visible pixels must equal the configured background, not merely
        # a few sample points which could miss stale glyphs or image fragments.
        reference = self.data[self.offset:self.offset + self.bytes_per_pixel]
        if self.pixel(0, 0) != BACKGROUND:
            return False
        row = reference * self.width
        for y in range(self.height):
            actual = self.data[self.offset + y * self.stride:self.offset + y * self.stride + len(row)]
            if actual != row and any(self.pixel(x, y) != BACKGROUND for x in range(self.width)):
                return False
        return True

    def save(self, path):
        pixels = bytearray()
        for y in range(self.height):
            pixels.append(0)
            for x in range(self.width):
                pixels.extend(self.pixel(x, y))
        def chunk(kind, content):
            return struct.pack('>I', len(content)) + kind + content + struct.pack('>I', zlib.crc32(kind + content))
        path.write_bytes(b'\x89PNG\r\n\x1a\n'
                         + chunk(b'IHDR', struct.pack('>2I5B', self.width, self.height, 8, 2, 0, 0, 0))
                         + chunk(b'IDAT', zlib.compress(pixels)) + chunk(b'IEND', b''))


def _observe_clear(directory, window, processes, run, wait_for, read_json, type_text, key):
    ready = wait_for('remote clear preparation', lambda: read_json(directory / 'clear-ready.json'), processes)
    def capture():
        return Capture(run(['xwd', '-id', window, '-silent']).stdout)
    def showing_before():
        frame = capture()
        cw, ch = frame.width // ready['columns'], frame.height // ready['rows']
        return frame if frame.pixel(9 * cw + 10, 2 * ch + 10) == IMAGE and frame.pixel(cw, ch) == LIVE else None
    before = wait_for('presented text background and Kitty image before clear', showing_before, processes)
    before.save(directory / 'clear-before.png')
    run(['xdotool', 'mousemove', '--window', window, '20', '20'])
    run(['xdotool', 'click', '--repeat', '8', '--delay', '30', '4'])
    def showing_history():
        frame = capture()
        return frame if frame.pixel(10, 10) == HISTORY else None
    history = wait_for('visible scrollback before clear', showing_history, processes)
    history.save(directory / 'clear-history-before.png')
    (directory / 'clear-go').write_text('run actual clear\n')
    wait_for('clear process completion', lambda: read_json(directory / 'clear-done.json'), processes)
    def blank():
        frame = capture()
        return frame if frame.blank() else None
    cleared = wait_for('clear presents an entirely blank viewport', blank, processes)
    cleared.save(directory / 'clear-after.png')
    run(['xdotool', 'click', '--repeat', '8', '--delay', '30', '4'])
    # Observe a bounded stable interval after injected scroll events. No input
    # key is sent here: a key could reset scroll offset and conceal stale history.
    deadline = time.monotonic() + 0.6
    samples = 0
    while time.monotonic() < deadline:
        frame = capture()
        if not frame.blank():
            frame.save(directory / 'clear-history-failure.png')
            raise AssertionError('old history or image became visible after clear and scrolling')
        samples += 1
        time.sleep(0.05)
    frame.save(directory / 'clear-history-after.png')
    (directory / 'clear-continue').write_text('test normal input\n')
    wait_for('post-clear input readiness', lambda: read_json(directory / 'clear-input-ready.json'), processes)
    type_text('after-clear')
    key('Return')
    received = wait_for('post-clear text over SSH', lambda: read_json(directory / 'clear-input.json'), processes)
    record(directory, 'clear-result.json', {'status': 'passed', 'post_clear_input': received,
        'blank_after_scroll_samples': samples, 'history_erased': True, 'image_removed': True,
        'clear_executed_with_inherited_ssh_pty': True})


def observe_clear(directory, window, processes, run, wait_for, read_json, type_text, key):
    try:
        _observe_clear(directory, window, processes, run, wait_for, read_json, type_text, key)
    except BaseException as error:
        record(directory, 'clear-result.json', {'status': 'failed', 'error': f'{type(error).__name__}: {error}'})
        raise
