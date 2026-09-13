#!/usr/bin/env python3
"""Observe one Linux Kokuban/mpv pair playing a looping clip under Xvfb.

This is a bounded functional/resource soak, not a GPU benchmark or an A/B test.
CPU is process utime+stime / elapsed time; 100% means one core. RSS is sampled,
VmHWM is lifetime peak, and image-cache accounting is not process RSS.
"""
import argparse
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import resource
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import time

HELPER_PATH = Path(__file__).with_name('linux-video-smoke.py')
SPEC = importlib.util.spec_from_file_location('kokuban_video_smoke', HELPER_PATH)
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)
CAPTURE_INTERVAL, RESOURCE_INTERVAL = 0.5, 1.0
MAX_OBSERVER_GAP, PROGRESS_DEADLINE, WINDOW_SECONDS = 2.5, 5.0, 10.0
LOG_BYTES, ARTIFACT_BYTES = 1024 * 1024, 64 * 1024 * 1024
MAX_CAPTURES, MAX_PROCESS_SAMPLES = 4000, 2000
CONFIG = ('[font]\nfamily = "DejaVu Sans Mono"\nsize = 14.0\n'
          '[window]\ncolumns = 80\nrows = 24\n'
          '[images]\nenabled = true\nmax_memory_mb = 256\n'
          '[images.kitty]\nallow_file_transfer = false\n')


def require(condition, message):
    if not condition:
        raise AssertionError(message)


class Progress:
    """Validate cyclic pixel IDs without using IPC to renew progress deadlines."""
    def __init__(self, started=0.0):
        self.last_observation = None
        self.last_valid = None
        self.last_progress = started
        self.window_start = started
        self.window_ids = set()
        self.window_total = self.window_invalid = 0
        self.total = self.invalid = self.transitions = self.wraps = 0
        self.windows = []

    def observe(self, seconds, frame_id):
        require(math.isfinite(seconds), 'invalid observation timestamp')
        if self.last_observation is not None:
            gap = seconds - self.last_observation
            require(0 < gap <= MAX_OBSERVER_GAP, f'observer gap invalid or above {MAX_OBSERVER_GAP}s: {gap}')
        require(seconds - self.last_progress <= PROGRESS_DEADLINE, 'no new valid visible frame within 5s')
        self.last_observation = seconds
        self.total += 1
        self.window_total += 1
        delta = None
        if frame_id is None:
            self.invalid += 1
            self.window_invalid += 1
        else:
            require(type(frame_id) is int and 0 <= frame_id < 72, 'frame ID outside cyclic fixture')
            self.window_ids.add(frame_id)
            if self.last_valid is not None:
                previous_time, previous_id = self.last_valid
                delta = (frame_id - previous_id) % 72
                maximum = math.ceil(12 * (seconds - previous_time)) + 4
                require(delta <= maximum, f'implausible cyclic frame advance: {delta} > {maximum}')
                if delta:
                    self.last_progress = seconds
                    self.transitions += 1
                    self.wraps += frame_id < previous_id
            self.last_valid = (seconds, frame_id)
        if seconds - self.window_start >= WINDOW_SECONDS:
            self.close_window(seconds)
        return delta

    def close_window(self, seconds):
        window = {'start_seconds': self.window_start, 'end_seconds': seconds,
                  'distinct_ids': sorted(self.window_ids), 'captures': self.window_total,
                  'invalid': self.window_invalid}
        self.windows.append(window)
        require(len(self.window_ids) >= 4, 'fewer than four valid visible IDs in observation window')
        require(self.window_invalid <= self.window_total * 0.1, 'more than 10% invalid captures in observation window')
        self.window_start = seconds
        self.window_ids = set()
        self.window_total = self.window_invalid = 0

    def finish(self, seconds):
        require(self.last_observation is not None and seconds - self.last_observation <= MAX_OBSERVER_GAP,
                'observation ended with an excessive capture gap')
        require(seconds - self.last_progress <= PROGRESS_DEADLINE, 'video ended without recent visible progress')
        if seconds - self.window_start >= 5:
            self.close_window(seconds)
        require(self.total > 0 and self.invalid <= self.total * 0.1, 'more than 10% invalid active captures')
        return {'captures': self.total, 'invalid': self.invalid, 'observed_transitions': self.transitions,
                'observed_wraps': self.wraps, 'windows': self.windows}


def validate_stats(current, identity, previous, max_rss_kib):
    require(current is not None, 'owned process disappeared')
    require((current['pid'], current['start_ticks']) == identity, 'owned PID identity changed')
    require(math.isfinite(current['cpu_seconds']) and current['cpu_seconds'] >= 0, 'invalid process CPU sample')
    require(0 < current['rss_kib'] <= current['hwm_kib'], 'missing or invalid process RSS/HWM')
    require(current['rss_kib'] <= max_rss_kib, 'process RSS exceeded configured soak guard')
    if previous is not None:
        require(current['cpu_seconds'] >= previous['cpu_seconds'], 'process CPU counter moved backwards')


def resource_window(samples, start, end):
    selected = [s for s in samples if start <= s['seconds'] <= end]
    if len(selected) < 2:
        return {'available': False, 'reason': 'fewer than two resource samples'}
    first, last = selected[0], selected[-1]
    wall = last['seconds'] - first['seconds']
    require(wall > 0, 'resource timestamps did not advance')
    result = {'available': True, 'start_seconds': first['seconds'], 'end_seconds': last['seconds'],
              'wall_seconds': wall, 'samples': len(selected)}
    times = [s['seconds'] for s in selected]
    mean_time = statistics.mean(times)
    denominator = sum((t - mean_time) ** 2 for t in times)
    for name in ('kokuban', 'mpv'):
        rss = [s[name]['rss_kib'] for s in selected]
        mean_rss = statistics.mean(rss)
        cpu = last[name]['cpu_seconds'] - first[name]['cpu_seconds']
        slope = sum((t - mean_time) * (value - mean_rss) for t, value in zip(times, rss)) / denominator
        result[name] = {'cpu_seconds': cpu, 'cpu_percent_one_core': cpu / wall * 100,
                        'rss_start_kib': rss[0], 'rss_end_kib': rss[-1], 'rss_median_kib': statistics.median(rss),
                        'rss_max_kib': max(rss), 'hwm_lifetime_kib': max(s[name]['hwm_kib'] for s in selected),
                        'rss_linear_slope_mib_per_minute': slope * 60 / 1024}
    return result


def summarize_resources(samples):
    end = samples[-1]['seconds']
    result = {'whole_playback': resource_window(samples, 0, end),
              'minutes': [resource_window(samples, start, min(start + 60, end))
                          for start in range(0, math.ceil(end), 60)],
              'after_180_seconds': resource_window(samples, 180, end),
              'late_window_comparison': {'available': False,
                  'reason': 'requires at least 420s for nonoverlapping post-180s and final-120s windows'}}
    if end >= 420:
        early, late = resource_window(samples, 180, 300), resource_window(samples, end - 120, end)
        require(early['available'] and late['available'], 'incomplete late resource windows')
        result['late_window_comparison'] = {'available': True, 'post_180_to_300_seconds': early,
            'final_120_seconds': late,
            'rss_median_delta_kib': {name: late[name]['rss_median_kib'] - early[name]['rss_median_kib']
                                     for name in ('kokuban', 'mpv')}}
    return result


def limit_log_files():
    resource.setrlimit(resource.RLIMIT_FSIZE, (LOG_BYTES, LOG_BYTES))


def hard_timeout(_signum, _frame):
    raise TimeoutError('soak hard deadline exceeded')


def child(directory, duration):
    player = None
    try:
        smoke.record(directory, 'child.json', smoke.process_stats(os.getpid()))
        with (directory / 'mpv.log').open('wb') as log:
            player = subprocess.Popen(json.loads((directory / 'mpv-command.json').read_text()), stderr=log)
            deadline = time.monotonic() + 3
            while True:
                identity = smoke.process_stats(player.pid)
                if identity and identity['rss_kib'] > 0:
                    break
                require(player.poll() is None and time.monotonic() < deadline, 'mpv failed before identity capture')
                time.sleep(0.01)
            smoke.record(directory, 'mpv.json', identity)
            status = player.wait(timeout=duration + 60)
            smoke.record(directory, 'mpv-exit.json', {'status': status})
            return status
    except BaseException as error:
        smoke.record(directory, 'child-error.json', {'error': f'{type(error).__name__}: {error}'})
        raise
    finally:
        smoke.stop_process(player)


def cleanup(ipc, terminal, directory):
    errors = []
    actions = [('ipc', lambda: ipc.close() if ipc is not None else None),
               ('mpv', lambda: smoke.stop_owned(smoke.read_json(directory / 'mpv.json'))),
               ('driver', lambda: smoke.stop_owned(smoke.read_json(directory / 'child.json'))),
               ('terminal', lambda: smoke.stop_process(terminal)),
               ('socket', lambda: (directory / 'mpv.sock').unlink()
                if (directory / 'mpv.sock').is_socket() else None)]
    for name, action in actions:
        try:
            action()
        except Exception as error:
            errors.append(f'{name}: {type(error).__name__}: {error}')
    return errors


def exercise(args, report):
    directory = args.artifacts_dir
    terminal = ipc = capture = None
    try:
        report['encoded_stream'] = smoke.make_video(directory)
        command = ['mpv', '--no-config', '--load-scripts=no', '--vo=kitty', '--vo-kitty-use-shm=no',
                   '--profile=sw-fast', '--audio=no', '--osc=no', '--osd-level=0', '--keep-open=yes',
                   '--pause=yes', '--loop-file=inf', '--vo-kitty-width=320', '--vo-kitty-height=180',
                   '--vo-kitty-cols=80', '--vo-kitty-rows=24', '--vo-kitty-left=1', '--vo-kitty-top=1',
                   '--input-ipc-server=' + str(directory / 'mpv.sock'), str(directory / 'fixture.mkv')]
        smoke.record(directory, 'mpv-command.json', command)
        shell = directory / 'shell'
        shell.write_text('#!/bin/sh\nexec ' + shlex.join([sys.executable, str(Path(__file__).resolve()),
            '--child-dir', str(directory), '--duration-seconds', str(args.duration_seconds)]) + '\n')
        shell.chmod(0o700)
        (directory / 'kokuban.toml').write_text(CONFIG)
        env = os.environ.copy()
        for key in ('WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'XDG_RUNTIME_DIR', 'KOKUBAN_EXIT_AFTER_FIRST_FRAME'):
            env.pop(key, None)
        env.update(KOKUBAN_SHELL=str(shell), WINIT_X11_SCALE_FACTOR='1', LC_ALL='C.UTF-8')
        with (directory / 'terminal.log').open('wb') as log:
            terminal = subprocess.Popen([str(args.binary)], cwd=directory, env=env, stdout=log, stderr=log,
                                        preexec_fn=limit_log_files)
        started = time.monotonic()

        def alive():
            require(terminal.poll() is None, f'Kokuban exited early: {terminal.returncode}')
            failure = smoke.read_json(directory / 'child-error.json')
            require(failure is None, f'video driver failed: {failure}')
            require(not (directory / 'mpv-exit.json').exists(), 'mpv exited before requested soak completion')
            for name in ('terminal.log', 'mpv.log'):
                path = directory / name
                require(not path.exists() or path.stat().st_size < LOG_BYTES, f'{name} exceeded log guard')

        def wait(description, observe, timeout=12):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                alive()
                value = observe()
                if value is not None and value is not False:
                    return value
                time.sleep(0.04)
            raise TimeoutError(f'timed out waiting for {description}')

        window = wait('visible window', lambda: smoke.find_window(terminal))
        smoke.run(['xdotool', 'windowfocus', '--sync', window])
        player = wait('mpv identity', lambda: smoke.read_json(directory / 'mpv.json'))
        wait('IPC socket', lambda: (directory / 'mpv.sock').exists())
        ipc = smoke.IPC(directory / 'mpv.sock')
        identities = {'kokuban': None, 'mpv': (player['pid'], player['start_ticks'])}

        def snapshot(phase, origin):
            nonlocal capture
            alive()
            before = time.monotonic()
            capture = smoke.Capture(smoke.run(['xwd', '-id', window, '-silent']).stdout)
            after = time.monotonic()
            require([capture.width, capture.height] == [720, 408],
                    f'window geometry differs from fixture: {capture.width}x{capture.height}')
            row = {'seconds': (before + after) / 2 - origin, 'capture_seconds': after - before,
                   'frame_id': smoke.visible_frame(capture), 'phase': phase}
            report['visible_samples'].append(row)
            require(len(report['visible_samples']) <= MAX_CAPTURES, 'capture count exceeded artifact guard')
            require(after - before <= MAX_OBSERVER_GAP, 'single capture exceeded observer deadline')
            return row

        wait('initial pause', lambda: ipc.get('pause') is True)
        wait('frame zero pixels', lambda: snapshot('startup', started)['frame_id'] == 0)
        capture.save_png(directory / 'start.png')
        report['window_pixels'] = [capture.width, capture.height]
        report['mpv_duration_seconds'] = ipc.get('duration')
        require(abs(report['mpv_duration_seconds'] - 6) < 0.05, 'mpv loaded an unexpected fixture duration')
        require(ipc.get('loop-file') == 'inf', 'mpv loop-file is not infinite')
        terminal_identity = smoke.process_stats(terminal.pid)
        require(terminal_identity is not None, 'terminal disappeared before playback')
        identities['kokuban'] = (terminal_identity['pid'], terminal_identity['start_ticks'])
        report['identities'] = identities
        previous = {'kokuban': None, 'mpv': None}

        def metrics(origin):
            alive()
            row = {'seconds': time.monotonic() - origin,
                   'kokuban': smoke.process_stats(terminal.pid), 'mpv': smoke.process_stats(player['pid'])}
            report['process_samples'].append(row)
            require(len(report['process_samples']) <= MAX_PROCESS_SAMPLES, 'resource count exceeded artifact guard')
            for name in identities:
                validate_stats(row[name], identities[name], previous[name], args.max_rss_mib * 1024)
                previous[name] = row[name]
            return row

        smoke.space()
        wait('playback unpaused', lambda: ipc.get('pause') is False)
        active_started = time.monotonic()
        metrics(active_started)
        progress = Progress()
        next_capture = next_metrics = active_started
        next_checkpoint = active_started + 10
        deadline = active_started + args.duration_seconds
        middle_saved = False
        while time.monotonic() < deadline:
            now = time.monotonic()
            alive()
            if now >= next_capture:
                row = snapshot('playing', active_started)
                row['cyclic_advance'] = progress.observe(row['seconds'], row['frame_id'])
                next_capture = time.monotonic() + CAPTURE_INTERVAL
                if not middle_saved and row['seconds'] >= args.duration_seconds / 2 and row['frame_id'] is not None:
                    capture.save_png(directory / 'middle.png')
                    middle_saved = True
            if now >= next_metrics:
                metrics(active_started)
                next_metrics = time.monotonic() + RESOURCE_INTERVAL
            if now >= next_checkpoint:
                report['elapsed_playback_seconds'] = time.monotonic() - active_started
                smoke.record(directory, 'report.json', report)
                next_checkpoint = time.monotonic() + 10
            time.sleep(max(0, min(next_capture, next_metrics, deadline) - time.monotonic()))
        final_metrics = metrics(active_started)
        report['elapsed_playback_seconds'] = final_metrics['seconds']
        report['progress'] = progress.finish(final_metrics['seconds'])
        report['resources'] = summarize_resources(report['process_samples'])
        require(middle_saved, 'midpoint screenshot was not observed')
        smoke.space()
        wait('final pause', lambda: ipc.get('pause') is True)
        time.sleep(0.25)
        paused = snapshot('final-pause', active_started)
        require(paused['frame_id'] is not None, 'final paused pixels are invalid')
        capture.save_png(directory / 'end.png')
        position = ipc.get('time-pos')
        stable_start = time.monotonic()
        while time.monotonic() - stable_start < 1:
            row = snapshot('final-pause', active_started)
            require(row['frame_id'] == paused['frame_id'], 'final paused image advanced')
            time.sleep(0.1)
        require(abs(ipc.get('time-pos') - position) <= 0.001, 'final paused playback time advanced')
        report['final_pause'] = {'frame_id': paused['frame_id'], 'stable_seconds': time.monotonic() - stable_start}
        ipc.quit()
        terminal.wait(timeout=8)
        require(terminal.returncode == 0 and smoke.read_json(directory / 'mpv-exit.json') == {'status': 0},
                'unclean Kokuban/mpv shutdown')
        report['clean_exit'] = {'kokuban': 0, 'mpv': 0}
    except BaseException:
        if capture is not None and [capture.width, capture.height] == [720, 408]:
            try:
                capture.save_png(directory / 'failure-last-capture.png')
            except Exception as error:
                report['failure_capture_error'] = f'{type(error).__name__}: {error}'
        raise
    finally:
        report['cleanup_errors'] = cleanup(ipc, terminal, directory)


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', nargs='?', type=Path)
    parser.add_argument('--artifacts-dir', type=Path)
    parser.add_argument('--duration-seconds', type=int, default=900)
    parser.add_argument('--max-rss-mib', type=int, default=1024, help='per-process soak guard, not a product memory limit')
    parser.add_argument('--child-dir', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if not 30 <= args.duration_seconds <= 1800:
        parser.error('--duration-seconds must be 30..1800')
    if not 64 <= args.max_rss_mib <= 4096:
        parser.error('--max-rss-mib must be 64..4096')
    if args.child_dir is None and (args.binary is None or args.artifacts_dir is None):
        parser.error('binary and --artifacts-dir are required')
    return args


def check(args):
    require(sys.platform == 'linux', 'run under Linux Xvfb')
    args.binary = args.binary.resolve(strict=True)
    args.artifacts_dir = args.artifacts_dir.resolve()
    require(args.binary.is_file(), 'binary is not a file')
    for name in ('ffmpeg', 'ffprobe', 'mpv', 'xdotool', 'xwd', 'rustc'):
        require(shutil.which(name) is not None, f'required program is missing: {name}')
    args.artifacts_dir.mkdir(parents=True, exist_ok=False)
    report = {'schema': 1, 'status': 'running', 'scope': __doc__,
              'settings': {'duration_seconds': args.duration_seconds, 'max_rss_mib': args.max_rss_mib,
                  'capture_interval_seconds': CAPTURE_INTERVAL, 'resource_interval_seconds': RESOURCE_INTERVAL,
                  'observer_gap_limit_seconds': MAX_OBSERVER_GAP, 'progress_deadline_seconds': PROGRESS_DEADLINE,
                  'log_guard_bytes': LOG_BYTES, 'artifact_guard_bytes': ARTIFACT_BYTES},
              'configuration_toml': CONFIG, 'visible_samples': [], 'process_samples': [],
              'limits': ['Absolute elapsed-time observation on Xvfb; no A/B or GPU claim.',
                  'Capture/PNG/checkpoint work adds observer overhead; excessive observer gaps fail separately.',
                  '1 MiB RLIMIT_FSIZE is inherited by owned processes to bound logs; RSS guard is an experiment limit.',
                  '256 MiB image-cache setting is not an RSS cap; snapshots may temporarily retain buffers.',
                  'No cache occupancy is measured; short pilots cannot establish eviction or a memory plateau.',
                  'CPU tick resolution limits short intervals; RSS samples can miss peaks; VmHWM is lifetime.']}
    started = time.monotonic()
    previous_handler = signal.signal(signal.SIGALRM, hard_timeout)
    signal.alarm(args.duration_seconds + 90)
    try:
        report['binary'] = {'path': str(args.binary), 'bytes': args.binary.stat().st_size,
                            'sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest()}
        report['helper_sha256'] = hashlib.sha256(HELPER_PATH.read_bytes()).hexdigest()
        report['runner_sha256'] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        report['workflow_environment'] = {key: os.environ.get(key) for key in ('GITHUB_SHA', 'GITHUB_RUN_ID', 'ImageOS', 'ImageVersion')}
        report['ticks_per_second'] = os.sysconf('SC_CLK_TCK')
        report['hardware'] = {'kernel': os.uname().release, 'machine': os.uname().machine,
            'cpu_affinity': sorted(os.sched_getaffinity(0)),
            'cpu_models': sorted({line.split(':', 1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines()
                                 if line.startswith(('model name', 'Hardware'))})}
        report['versions'] = {name: smoke.run(command).stdout.decode(errors='replace').splitlines()[:3]
                              for name, command in [('mpv', ['mpv', '--version']), ('ffmpeg', ['ffmpeg', '-version']),
                                                    ('rustc', ['rustc', '--version', '--verbose'])]}
        smoke.record(args.artifacts_dir, 'report.json', report)
        exercise(args, report)
        require(not report['cleanup_errors'], 'owned process cleanup failed')
        require(sum(p.stat().st_size for p in args.artifacts_dir.iterdir() if p.is_file()) < ARTIFACT_BYTES,
                'artifact size exceeded soak guard')
        report['status'] = 'passed'
        print('PASS: one Kokuban/mpv pair, looping pixel progress, resource sampling and clean shutdown')
        return 0
    except (Exception, KeyboardInterrupt) as error:
        report.update(status='failed', error=f'{type(error).__name__}: {error}')
        print(report['error'], file=sys.stderr)
        return 1
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGALRM, previous_handler)
        report['total_seconds'] = time.monotonic() - started
        smoke.record(args.artifacts_dir, 'report.json', report)
        hashes = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in args.artifacts_dir.iterdir()
                  if p.is_file() and not p.is_socket() and p.name != 'artifact-sha256.json'}
        smoke.record(args.artifacts_dir, 'artifact-sha256.json', hashes)


if __name__ == '__main__':
    arguments = parse_args()
    sys.exit(child(arguments.child_dir.resolve(), arguments.duration_seconds)
             if arguments.child_dir is not None else check(arguments))
