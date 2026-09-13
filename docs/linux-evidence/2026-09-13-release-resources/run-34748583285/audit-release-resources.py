"""Audit retained release-resource observations; never launches Kokuban or mpv."""
from pathlib import Path
import hashlib
import json
import math
import statistics
import struct
import subprocess

ROOT = Path(__file__).resolve().parent
ARTIFACT = ROOT / 'linux-release-resource-measurements'
REVISION = '72dd68a1262d360d119fb9459ea12a47303bae5d'


def load(path):
    return json.loads(path.read_text())


def sha(data):
    return hashlib.sha256(data).hexdigest()


def equal_number(actual, expected):
    assert math.isclose(actual, expected, rel_tol=1e-12, abs_tol=1e-12), (actual, expected)


def rgb_frame(frame):
    width, height = 320, 180
    body = (30 + frame * 37 % 190, 30 + frame * 59 % 190, 30 + frame * 83 % 190)
    pixels = bytearray(bytes(body) * width * height)
    zero, one = (20, 40, 240), (240, 220, 20)

    def rectangle(x, y, w, h, rgb):
        for row in range(y, y + h):
            start = (row * width + x) * 3
            pixels[start:start + w * 3] = bytes(rgb) * w

    rectangle(0, 0, 32, 20, (240, 20, 40))
    rectangle(288, 0, 32, 20, (20, 220, 230))
    for bit in range(8):
        flag = frame & (1 << bit)
        rectangle(32 + bit * 32, 24, 32, 48, one if flag else zero)
        rectangle(32 + bit * 32, 80, 32, 48, zero if flag else one)
    rectangle(8, 148, 304, 24, zero)
    rectangle(8, 148, max(1, (frame + 1) * 304 // 72), 24, one)
    return bytes(pixels)


def decode_rgb(path):
    return subprocess.check_output(['ffmpeg', '-v', 'error', '-threads', '1', '-i', str(path),
                                    '-threads', '1', '-f', 'rawvideo', '-pix_fmt', 'rgb24', 'pipe:1'], timeout=10)


def audit():
    github = load(ROOT / 'github-run.json')
    assert github['conclusion'] == 'success' and github['headSha'] == REVISION
    assert all(job['conclusion'] == 'success' for job in github['jobs'])
    resource = load(ARTIFACT / 'resources/report.json')
    video = load(ARTIFACT / 'video/report.json')
    assert resource['status'] == video['status'] == 'passed'
    assert resource['build_profile'] == video['build_profile'] == 'release'
    assert resource['workflow_environment']['GITHUB_SHA'] == REVISION
    assert resource['workflow_environment']['GITHUB_RUN_ID'] == '34748583285'
    inputs = {}
    for name in ('Cargo.toml', 'Cargo.lock'):
        expected = subprocess.check_output(['git', 'show', REVISION + ':' + name])
        assert (ARTIFACT / name).read_bytes() == expected
        inputs[name] = sha(expected)
    assert resource['ticks_per_second'] == 100
    phases = {}
    identity = resource['identity']['pid'], resource['identity']['start_ticks']
    previous_time, previous_ticks = -1, -1
    for name, phase in resource['phases'].items():
        samples = phase['samples']
        assert len(samples) >= 2
        for sample in samples:
            assert (sample['pid'], sample['start_ticks']) == identity
            assert sample['monotonic_seconds'] > previous_time and sample['cpu_ticks'] >= previous_ticks
            assert 0 < sample['rss_kib'] <= sample['hwm_kib'] and sample['threads'] > 0
            equal_number(sample['cpu_seconds'], sample['cpu_ticks'] / 100)
            previous_time, previous_ticks = sample['monotonic_seconds'], sample['cpu_ticks']
        wall = samples[-1]['monotonic_seconds'] - samples[0]['monotonic_seconds']
        cpu = (samples[-1]['cpu_ticks'] - samples[0]['cpu_ticks']) / 100
        equal_number(phase['wall_seconds'], wall)
        equal_number(phase['cpu_seconds'], cpu)
        equal_number(phase['cpu_percent_one_core'], cpu / wall * 100)
        assert phase['rss_start_kib'] == samples[0]['rss_kib']
        assert phase['rss_end_kib'] == samples[-1]['rss_kib']
        assert phase['rss_observed_max_kib'] == max(s['rss_kib'] for s in samples)
        assert phase['hwm_process_lifetime_kib'] == max(s['hwm_kib'] for s in samples)
        phases[name] = {key: value for key, value in phase.items() if key != 'samples'}
        phases[name]['sample_count'] = len(samples)
    for name in ('idle', 'post_output_idle'):
        assert phases[name]['wall_seconds'] >= 5
    for key, file in (('ready', 'ready.json'), ('output', 'output-done.json')):
        assert resource[key] == load(ARTIFACT / 'resources' / file)
        data = resource[key]
        assert bytes.fromhex(data['reply_hex']) == f"\x1b[1;{len(data['marker']) + 1}R".encode()
        assert data['winsize_rows_cols_pixels'] == [24, 80, 720, 408]
    payload = b''.join(f'{line:06d} '.encode() + (b'abcdefghijklmnopqrstuvwxyz0123456789' * 2)[:71] + b'\r\n'
                       for line in range(32768))
    output = resource['output']
    assert output['bytes'] == len(payload) == 2621440 and output['lines'] == 32768
    assert output['sha256'] == sha(payload)
    equal_number(output['write_and_terminal_roundtrip_seconds'], output['write_seconds'] + output['roundtrip_seconds'])
    equal_number(output['bytes_per_second_including_terminal_roundtrip'], len(payload) / output['write_and_terminal_roundtrip_seconds'])
    assert resource['window_pixels'] == video['window_pixels'] == [720, 408]
    assert resource['clean_exit'] == {'kokuban': 0, 'controlled_pty_app': 0}
    assert load(ARTIFACT / 'resources/child-exit.json') == {'status': 0}
    assert video['clean_exit'] == {'kokuban': 0, 'mpv': 0}
    assert load(ARTIFACT / 'video/mpv-exit.json') == {'status': 0}
    process_samples = video['process_samples']
    identities = {name: (process_samples[0][name]['pid'], process_samples[0][name]['start_ticks'])
                  for name in ('kokuban', 'mpv')}
    assert len(set(identities.values())) == 2
    for name, expected in identities.items():
        assert all((sample[name]['pid'], sample[name]['start_ticks']) == expected for sample in process_samples)
        assert all(a[name]['cpu_seconds'] <= b[name]['cpu_seconds'] for a, b in zip(process_samples, process_samples[1:]))
        for key in ('rss_kib', 'hwm_kib'):
            assert video['observations']['process_peaks_kib'][name][key] == max(s[name][key] for s in process_samples)
    player = load(ARTIFACT / 'video/mpv.json')
    assert (player['pid'], player['start_ticks']) == identities['mpv']
    for phase in video['active_measurements']:
        before = next(s for s in process_samples if s['phase'] == phase['phase'] + '-start')
        after = next(s for s in process_samples if s['phase'] == phase['phase'] + '-end')
        wall = after['seconds'] - before['seconds']
        equal_number(phase['wall_seconds'], wall)
        for name in identities:
            cpu = after[name]['cpu_seconds'] - before[name]['cpu_seconds']
            equal_number(phase[name]['cpu_seconds'], cpu)
            equal_number(phase[name]['cpu_percent_one_core'], cpu / wall * 100)
    captures = video['visible_samples']
    assert all(a['seconds'] < b['seconds'] for a, b in zip(captures, captures[1:]))
    ids = [s['frame_id'] for s in captures if s['frame_id'] is not None]
    assert ids == sorted(ids) and sorted(set(ids)) == list(range(72))
    active = [s for s in captures if s['phase'].startswith('playing-')]
    intervals = [b['seconds'] - a['seconds'] for a, b in zip(active, active[1:]) if a['phase'] == b['phase']]
    observations = video['observations']
    assert observations['distinct_visible_frame_ids'] == list(range(72))
    assert observations['active_capture_count'] == len(active)
    assert observations['invalid_active_captures'] == sum(s['frame_id'] is None for s in active) == 0
    transitions = sum(len({s['frame_id'] for s in active if s['phase'] == phase['phase']}) - 1
                      for phase in video['active_measurements'])
    active_seconds = sum(p['wall_seconds'] for p in video['active_measurements'])
    equal_number(observations['observed_frame_rate_lower_bound_hz'], transitions / active_seconds)
    assert observations['observed_active_transitions'] == transitions == 71
    for key, expected in [('min', min(intervals)), ('median', statistics.median(intervals)), ('max', max(intervals))]:
        equal_number(observations['capture_interval_seconds'][key], expected)
    assert all(s['frame_id'] == video['pause']['frame_id'] == 18 for s in captures if s['phase'] == 'pause')
    assert all(s['frame_id'] == 71 for s in captures if s['phase'] == 'eof')
    assert video['eof']['eof_reached'] is True and video['mpv_duration_seconds'] == 6
    command = load(ARTIFACT / 'video/mpv-command.json')
    assert all(flag in command for flag in ('--vo=kitty', '--vo-kitty-use-shm=no', '--audio=no', '--pause=yes'))
    probe = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-count_frames',
        '-show_entries', 'stream=codec_name,width,height,r_frame_rate,nb_read_frames', '-of', 'json',
        str(ARTIFACT / 'video/fixture.mkv')], timeout=10))['streams'][0]
    assert all(probe[key] == value for key, value in video['encoded_stream'].items())
    decoded = decode_rgb(ARTIFACT / 'video/fixture.mkv')
    expected_video = b''.join(rgb_frame(i) for i in range(72))
    assert decoded == expected_video
    screenshots = []
    for name, frame in [('frame-00.png', 0), ('paused.png', 18), ('frame-71.png', 71)]:
        path = ARTIFACT / 'video' / name
        width, height = struct.unpack('>II', path.read_bytes()[16:24])
        assert [width, height] == [720, 408]
        decoded = decode_rgb(path)
        cropped = b''.join(decoded[y * width * 3:y * width * 3 + 320 * 3] for y in range(180))
        assert cropped == rgb_frame(frame), name
        screenshots.append({'file': name, 'frame_id': frame, 'window_pixels': [width, height],
                            'full_video_region_rgb_exact': True, 'sha256': sha(path.read_bytes())})
    return {'status': 'PASS', 'scope': 'One absolute release-build observation, no baseline or cache-attributed performance delta.',
            'run': '34748583285', 'revision': REVISION, 'hardware': resource['hardware'],
            'build_inputs_sha256': inputs, 'reported_binary': resource['binary'], 'resource_identity': identity,
            'phases_recomputed': phases, 'output_recomputed': output,
            'video': {'identities': identities, 'active_measurements_recomputed': video['active_measurements'],
                      'observations_recomputed': observations, 'screenshots': screenshots,
                      'fixture_decoded_frames_exact': 72, 'fixture_sha256': sha((ARTIFACT / 'video/fixture.mkv').read_bytes()),
                      'decoded_rgb_sha256': sha(expected_video)},
            'limitations': ['Kokuban binary is not retained; its size/SHA/ELF metadata are reported by CI, not independently rehashed here.',
                'Video reuses the same release path in a subsequent workflow step but has no separate binary hash.',
                'CPU uses 100 Hz process utime+stime, excluding children/display/observer; zero sampled ticks is not proof of zero work.',
                'RSS is sampled; VmHWM covers process lifetime. Output sampling lasts 101 ms while PTY write plus DSR lasts 78.49 ms.',
                'DSR proves parsing/terminal response, not presentation of every line; video captures add observer overhead.',
                'Only three screenshots are retained; other 298 capture classifications are validated from report records.',
                'GitHub ZIP digest is retained as API metadata; original ZIP bytes were not retained by gh download. Local file hashes are recorded.']}


if __name__ == '__main__':
    result = audit()
    (ROOT / 'release-resource-audit.json').write_text(json.dumps(result, indent=2) + '\n')
    files = {p.relative_to(ROOT).as_posix(): {'sha256': sha(p.read_bytes()), 'bytes': p.stat().st_size}
             for p in sorted(ROOT.rglob('*')) if p.is_file() and p.name != 'local-manifest.json'}
    (ROOT / 'local-manifest.json').write_text(json.dumps(files, indent=2) + '\n')
    print('PASS: functional barriers, identities, 5 resource phases, 2 video phases, 72 decoded frames, 3 exact PNG regions.')
