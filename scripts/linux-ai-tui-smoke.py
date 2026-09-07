#!/usr/bin/env python3
"""Exercise real AI CLIs over SSH/X11 against a deterministic localhost stream.

This is a terminal compatibility test, not a model-quality or paid-service test.
Only fixture state and dummy credentials are passed to clients. No upstream
requests are made by the HTTP fixture; plugins and external integrations are off.
"""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import termios

from tui_fixture_server import Fixture, cli_invocation
from linux_clear_fixture import Capture

spec = importlib.util.spec_from_file_location('linux_apps_fixture', Path(__file__).with_name('linux-apps-smoke.py'))
apps = importlib.util.module_from_spec(spec)
spec.loader.exec_module(apps)


def plain(data):
    text = data.decode('utf-8', errors='replace')
    text = re.sub(r'\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)', '', text)
    text = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', text)
    return re.sub(r'\x1b[ -/]*[@-Z\\-_]', '', text)


def child(client, directory, endpoint):
    os.chdir(directory)
    apps.record(directory, 'remote-process.json', apps.process_identity(os.getpid()))
    dimensions = lambda: list(os.get_terminal_size(0))
    signal.signal(signal.SIGWINCH, lambda *_: apps.record(directory, 'resize.json', dimensions()))
    args, environment = cli_invocation(client, directory, endpoint)
    version = subprocess.run([args[0], '--version'], env=environment, check=True, capture_output=True, text=True, timeout=5)
    before = termios.tcgetattr(0)
    apps.record(directory, 'client-start.json', {'client': client, 'version': version.stdout.strip(),
                'size': dimensions(), 'term': environment['TERM'], 'ssh': bool(os.environ.get('SSH_CONNECTION'))})
    # util-linux script provides a transparent real PTY and a bounded-by-time
    # output observer. Input still originates at the terminal window via SSH.
    result = subprocess.run(['script', '--quiet', '--return', '--flush', '--command', shlex.join(args),
                             '--log-out', str(directory / 'tui-output.bin')], env=environment, timeout=80)
    restored = before == termios.tcgetattr(0)
    apps.record(directory, 'client-exit.json', {'status': result.returncode, 'termios_restored': restored})
    if result.returncode or not restored:
        raise AssertionError('CLI failed or PTY attributes were not restored')
    subprocess.run(['clear'], check=True, timeout=5)
    print('TUI EXIT OK', flush=True)
    apps.record(directory, 'post-exit-ready.json', True)
    if input() != 'after-tui':
        raise AssertionError('ordinary input after TUI exit differs')
    apps.record(directory, 'post-exit.json', {'status': 'passed', 'input': 'after-tui', 'real_clear_after_exit': True})



def inspect_presented_text(directory, window, phase, markers, forbidden, processes):
    """Read actual selected grid text and require visible ink in its marker rows."""
    geometry = apps.run(['xdotool', 'getwindowgeometry', '--shell', window]).stdout.decode()
    fields = dict(line.split('=', 1) for line in geometry.splitlines() if '=' in line)
    width, height = int(fields['WIDTH']), int(fields['HEIGHT'])
    size = apps.read_json(directory / 'resize.json') or apps.read_json(directory / 'client-start.json')['size']
    cw, ch = width // size[0], height // size[1]
    sentinel = ('clipboard-not-copied-' + phase).encode()
    subprocess.run(['xclip', '-selection', 'clipboard', '-in', '-loops', '1'], input=sentinel,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True, timeout=5)
    try:
        apps.run(['xdotool', 'mousemove', '--window', window, '1', '1', 'keydown', 'Shift', 'mousedown', '1',
                  'mousemove', '--window', window, str(width - 2), str(height - 2), 'mouseup', '1'])
    finally:
        apps.run(['xdotool', 'keyup', 'Shift'])
    apps.key('ctrl+shift+c')
    def selected():
        value = apps.run(['xclip', '-selection', 'clipboard', '-out'], check=False).stdout.decode(errors='replace')
        return value if all(value.count(item) == 1 for item in markers) and not any(item in value for item in forbidden) else None
    copied = apps.wait_for('presented ' + phase + ' text copied from the terminal', selected, processes)
    (directory / (phase + '.txt')).write_text(copied)
    # A Shift-click collapses selection without sending Escape (which would
    # interrupt a still-running CLI response) or typing into the prompt.
    try:
        apps.run(['xdotool', 'mousemove', '--window', window, '1', '1', 'keydown', 'Shift', 'click', '1'])
    finally:
        apps.run(['xdotool', 'keyup', 'Shift'])
    lines = copied.split('\n')
    regions = []
    for marker in markers:
        row, line = next((row, line) for row, line in enumerate(lines) if marker in line)
        col = line.index(marker)
        regions.append((max(0, (col - 1) * cw), row * ch, min(width, (col + len(marker) + 1) * cw), min(height, (row + 1) * ch)))
    def visible():
        frame = Capture(apps.run(['xwd', '-id', window, '-silent']).stdout)
        for left, top, right, bottom in regions:
            colors = {}
            for y in range(top, bottom):
                for x in range(left, right):
                    color = frame.pixel(x, y)
                    colors[color] = colors.get(color, 0) + 1
            # A blank area (or solid selection highlight) cannot satisfy this.
            if len(colors) < 2 or sum(colors.values()) - max(colors.values()) < 12:
                return None
        return frame
    frame = apps.wait_for('visible glyph pixels in ' + phase + ' marker rows', visible, processes)
    frame.save(directory / (phase + '.png'))
    return {'markers_copied_once': list(markers), 'forbidden_markers_absent': list(forbidden), 'visible_marker_regions': regions}


def exercise(binary, client, artifacts):
    report = {'client': client, 'status': 'incomplete', 'scope': 'real CLI / localhost SSE / loopback SSH / XTest; no model service'}
    artifacts.mkdir(parents=True, exist_ok=True)
    def checkpoint(stage):
        report['stage'] = stage
        apps.record(artifacts, 'results.json', report)
        print(client + ': ' + stage, flush=True)
    checkpoint('starting')
    with tempfile.TemporaryDirectory(prefix='kokuban-ai-tui-', dir=Path.home()) as temporary:
        directory = Path(temporary)
        work = directory / 'work'
        work.mkdir()
        daemon = terminal = None
        window = None
        try:
            with Fixture(work) as endpoint, (directory / 'sshd.log').open('wb') as server_log:
                config, port = apps.prepare_server(directory)
                daemon = subprocess.Popen([str(Path(shutil.which('sshd')).resolve()), '-D', '-e', '-f', str(config)], stdout=server_log, stderr=server_log)
                apps.wait_for('isolated SSH daemon', lambda: apps.server_listening(port), (daemon,))
                remote = shlex.join(['env', 'PATH=' + os.environ['PATH'], 'LC_ALL=C.UTF-8',
                                     sys.executable, str(Path(__file__).resolve()), '--child', client,
                                     '--directory', str(work), '--endpoint', endpoint.url])
                command = apps.ssh_arguments(directory, port, directory / 'known_hosts') + ['-tt', '127.0.0.1', remote]
                shell = directory / 'shell'
                shell.write_text('#!/bin/sh\nexec ' + shlex.join(command) + '\n')
                shell.chmod(0o700)
                (directory / 'kokuban.toml').write_text('[font]\nfamily="DejaVu Sans Mono"\nsize=14.0\n[window]\ncolumns=100\nrows=30\n')
                env = os.environ.copy()
                for name in ('WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'XDG_RUNTIME_DIR', 'KOKUBAN_EXIT_AFTER_FIRST_FRAME'):
                    env.pop(name, None)
                env.update(KOKUBAN_SHELL=str(shell), WINIT_X11_SCALE_FACTOR='1', LC_ALL='C.UTF-8')
                with (directory / 'terminal.log').open('wb') as terminal_log:
                    terminal = subprocess.Popen([str(binary)], cwd=directory, env=env, stdout=terminal_log, stderr=terminal_log)
                    processes = (daemon, terminal)
                    window = apps.wait_for('terminal window', lambda: apps.find_window(terminal), processes)
                    apps.run(['xdotool', 'windowfocus', '--sync', window])
                    report['startup'] = apps.wait_for('CLI with SSH PTY', lambda: apps.read_json(work / 'client-start.json'), processes)
                    if not report['startup']['ssh'] or report['startup']['size'] != [100, 30]:
                        raise AssertionError('CLI did not start at 100x30 through SSH')
                    transcript = work / 'tui-output.bin'
                    def text():
                        if not transcript.exists():
                            return ''
                        if transcript.stat().st_size > 2 * 1024 * 1024:
                            raise AssertionError('TUI trace exceeded 2 MiB fixture limit')
                        return plain(transcript.read_bytes())
                    dialogs = set()
                    def ready():
                        normalized = ''.join(text().split()).lower()
                        choices = [('choosethetextstyle', ()), ('doyouwanttousethisapikey?', ('Up',)),
                                   ('securitynotes:', ()), ('yes,itrustthisfolder', ('Down',))]
                        for marker, movement in choices:
                            if marker in normalized and marker not in dialogs:
                                dialogs.add(marker)
                                if movement:
                                    apps.key(*movement)
                                apps.key('Return')
                                return False
                        return 'forshortcuts' in normalized and 'compat-fixture' in normalized
                    apps.wait_for('interactive CLI prompt after fixture onboarding', ready, processes, timeout=20)
                    report['onboarding_dialogs'] = sorted(dialogs)
                    checkpoint('interactive prompt ready')
                    apps.screenshot(window, work / 'startup.png')
                    # Correct a character through real key events before Enter.
                    apps.type_text('compat-input-4x')
                    apps.key('BackSpace')
                    apps.type_text('2')
                    apps.key('Return')
                    apps.wait_for('user prompt reaches local streaming endpoint', endpoint.partial.is_set, processes)
                    if not endpoint.requests or not endpoint.requests[0]['prompt_received']:
                        raise AssertionError('CLI request did not contain the edited keyboard prompt')
                    apps.wait_for('CLI emits first streamed text', lambda: 'COMPAT_BEGIN' in text(), processes)
                    if 'COMPAT_END' in text():
                        raise AssertionError('final text appeared before server released it')
                    report['partial'] = inspect_presented_text(work, window, 'partial', ['COMPAT_BEGIN'], ['COMPAT_END'], processes)
                    checkpoint('partial response presented')
                    geometry = apps.run(['xdotool', 'getwindowgeometry', '--shell', window]).stdout.decode()
                    fields = dict(line.split('=', 1) for line in geometry.splitlines() if '=' in line)
                    cw, ch = int(fields['WIDTH']) // 100, int(fields['HEIGHT']) // 30
                    apps.run(['xdotool', 'windowsize', '--sync', window, str(cw * 88), str(ch * 26)])
                    apps.wait_for('remote PTY resize during incomplete stream', lambda: apps.read_json(work / 'resize.json') == [88, 26], processes)
                    report['partial_resized'] = inspect_presented_text(work, window, 'partial-resized', ['COMPAT_BEGIN'], ['COMPAT_END'], processes)
                    checkpoint('partial response presented after resize')
                    endpoint.release.set()
                    apps.wait_for('CLI emits final streamed text', lambda: endpoint.completed.is_set() and 'COMPAT_END' in text(), processes)
                    report['complete'] = inspect_presented_text(work, window, 'complete', ['COMPAT_BEGIN', 'COMPAT_END'], [], processes)
                    checkpoint('complete response presented')
                    report['stream'] = {'partial_before_release': True, 'final_after_release': True, 'resized_pty': [88, 26],
                                        'requests': len(endpoint.requests), 'output_marker_counts': {x: text().count(x) for x in ('COMPAT_BEGIN', 'COMPAT_END')}}
                    if len(endpoint.requests) != 1 or endpoint.errors:
                        raise AssertionError('unexpected repeated model request or local endpoint error')
                    apps.type_text('/exit')
                    apps.key('Return')
                    report['exit'] = apps.wait_for('normal CLI exit', lambda: apps.read_json(work / 'client-exit.json'), processes)
                    apps.wait_for('ordinary terminal after CLI exit', lambda: apps.read_json(work / 'post-exit-ready.json'), processes)
                    apps.screenshot(window, work / 'exited.png')
                    apps.type_text('after-tui')
                    apps.key('Return')
                    report['post_exit'] = apps.wait_for('post-TUI keyboard input', lambda: apps.read_json(work / 'post-exit.json'), processes)
                    terminal.wait(timeout=8)
                    if terminal.returncode:
                        raise AssertionError('SSH/terminal did not exit cleanly')
                    report['status'] = 'passed'
                    checkpoint('normal exit and subsequent input passed')
        except BaseException as error:
            report.update(status='failed', error=f'{type(error).__name__}: {error}')
            if window and terminal and terminal.poll() is None:
                try:
                    apps.screenshot(window, work / 'failure.png')
                except Exception as diagnostic:
                    report['screenshot_error'] = str(diagnostic)
            raise
        finally:
            cleanup_errors = []
            for description, cleanup in (
                    ('remote fixture', lambda: apps.stop_remote_group(work)),
                    ('terminal', lambda: apps.stop_process(terminal)),
                    ('SSH daemon', lambda: apps.stop_process(daemon))):
                try:
                    cleanup()
                except Exception as error:
                    cleanup_errors.append(description + ': ' + str(error))
            # Explicit safe outputs: never upload client state, SSH keys, or request headers.
            for name in ('endpoint.json', 'client-start.json', 'client-exit.json', 'resize.json', 'post-exit.json',
                         'tui-output.bin', 'partial.txt', 'partial-resized.txt', 'complete.txt', 'startup.png', 'partial.png', 'partial-resized.png', 'complete.png', 'exited.png', 'failure.png'):
                path = work / name
                if path.is_file() and path.stat().st_size <= 2 * 1024 * 1024:
                    shutil.copyfile(path, artifacts / name)
            for name in ('terminal.log', 'sshd.log'):
                if (directory / name).exists():
                    shutil.copyfile(directory / name, artifacts / name)
            apps.record(artifacts, 'results.json', report)
            if cleanup_errors:
                report.update(status='failed', cleanup_errors=cleanup_errors)
                apps.record(artifacts, 'results.json', report)
                if sys.exc_info()[0] is None:
                    raise AssertionError('fixture cleanup failed: ' + '; '.join(cleanup_errors))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path, nargs='?')
    parser.add_argument('--client', choices=['claude', 'codex'], default='codex')
    parser.add_argument('--artifacts', type=Path)
    parser.add_argument('--child', choices=['claude', 'codex'], help=argparse.SUPPRESS)
    parser.add_argument('--directory', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--endpoint', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.child:
        child(args.child, args.directory, args.endpoint)
        return
    if sys.platform != 'linux' or os.geteuid() == 0:
        parser.error('run as an ordinary Linux user under Xvfb')
    if not args.binary or not args.artifacts:
        parser.error('binary and --artifacts are required')
    for program in ('ssh', 'sshd', 'ssh-keygen', 'xdotool', 'xwd', 'xclip', 'script', args.client):
        if not shutil.which(program):
            parser.error(f'missing {program}')
    def interrupted(*_):
        raise TimeoutError('AI TUI smoke exceeded its 110-second deadline')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGALRM, interrupted)
    signal.alarm(110)
    try:
        exercise(args.binary.resolve(), args.client, args.artifacts.resolve())
    finally:
        signal.alarm(0)


if __name__ == '__main__':
    main()
