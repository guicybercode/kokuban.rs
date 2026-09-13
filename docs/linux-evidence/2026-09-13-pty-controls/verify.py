#!/usr/bin/env python3
"""Verify retained PTY evidence without network, Git, builds or terminal execution."""
from pathlib import Path, PurePosixPath
import hashlib
import io
import json
import lzma
import math
import statistics
import tarfile

ROOT = Path(__file__).resolve().parent
ARTIFACT = 'linux-paired-revision-measurements/'
RUNS = ('34749172188', '34749322755', '34749492893', '34749802898')
BEFORE = 'd54f801e9dec41a6eb4f159397e45939c367531d'
AFTER = '62c99adc6fc1dbf12d89ee717f5823644c162d47'
WORKLOADS = {
    'ascii': b'abcdefghijklmnopqrstuvwxyz0123456789 ' * 2 + b'\r\n',
    'ansi': b'\x1b[31mred\x1b[0m \x1b[1;34mblue\x1b[0m \x1b[38;2;80;200;90mtruecolor\x1b[0m\r\n',
    'unicode': ('café 日本 λ e\u0301 ' * 3 + '\r\n').encode(),
    'short_lines': b'x\r\n',
}


def require(condition, explanation):
    if not condition:
        raise ValueError(explanation)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def close(actual, expected):
    require(math.isfinite(actual) and math.isfinite(expected)
            and math.isclose(actual, expected, rel_tol=1e-10, abs_tol=1e-10),
            f'numeric mismatch: {actual} != {expected}')


def proof(data, record):
    require(len(data) == record['bytes'] and sha(data) == record['sha256'],
            'size/SHA256 mismatch')


def distribution(values, reported):
    median = statistics.median(values)
    expected = {'count': len(values), 'median': median, 'min': min(values),
                'max': max(values), 'median_absolute_deviation':
                statistics.median(abs(v - median) for v in values)}
    for key, value in expected.items():
        close(value, reported[key])
    require(len(values) == len(reported['samples']), 'distribution length')
    for actual, saved in zip(values, reported['samples']):
        close(actual, saved)


def unpack(record):
    packed = (ROOT / record['archive']).read_bytes()
    proof(packed, {'bytes': record['archive_bytes'], 'sha256': record['archive_sha256']})
    raw = lzma.decompress(packed)
    proof(raw, {'bytes': record['uncompressed_tar_bytes'],
                'sha256': record['uncompressed_tar_sha256']})
    files = {}
    with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
        for item in archive:
            path = PurePosixPath(item.name)
            require(item.isfile() and not path.is_absolute() and '..' not in path.parts
                    and item.name not in files, 'unsafe/duplicate archive member')
            files[item.name] = archive.extractfile(item).read()
    require(files.keys() == record['raw_files'].keys(), 'archive file-set mismatch')
    for name, data in files.items():
        proof(data, record['raw_files'][name])
    return files


def main():
    manifest = json.loads((ROOT / 'manifest.json').read_text())
    summary = json.loads((ROOT / 'summary.json').read_text())
    require(set(manifest['runs']) == set(summary['runs']) == set(RUNS), 'four-run set')
    for name, record in manifest['audit_source'].items():
        proof((ROOT / name).read_bytes(), record)
    payloads = {}
    for name, line in WORKLOADS.items():
        count = 33554432 // len(line)
        chunks, remainder = divmod(count, 4096)
        digest = hashlib.sha256()
        block = line * 4096
        for _ in range(chunks):
            digest.update(block)
        digest.update(line * remainder)
        payloads[name] = {'bytes': count * len(line), 'sha256': digest.hexdigest()}
    totals = {'processes': 0, 'workload_observations': 0, 'protocol_rtts': 0}
    for run_id in RUNS:
        record = manifest['runs'][run_id]
        files = unpack(record)
        load = lambda name: json.loads(files[name])
        report = load(ARTIFACT + 'measurements/report.json')
        audit = load(summary['runs'][run_id]['audit_file'])
        github = load('github-run.json')
        control = run_id == '34749802898'
        alternate = run_id in ('34749322755', '34749492893')
        history = 0 if alternate else 10000
        screen = 'alternate' if alternate else 'primary'
        require(github['status'] == 'completed' and github['conclusion'] == 'success'
                and github['databaseId'] == int(run_id), 'successful run identity')
        require(github['headSha'] == audit['comparison']['harness'], 'harness revision')
        require(report['status'] == 'passed' and report['in_progress'] is None
                and report['pairs_requested'] == 5, 'completed five-pair report')
        require(report['backend'] == 'wayland' and report['screen'] == screen
                and report['cpu_affinity'] == [0] and report['requested_geometry'] == [24, 80]
                and report['font_pixels'] == 14 and report['bytes_requested_per_workload'] == 33554432,
                'measurement configuration')
        require(report['environment']['DISPLAY'] is None
                and report['comparability']['paired_inputs_validated']
                and report['comparability']['reasons'] == []
                and report['comparability']['geometries_rows_cols_pixels'] == [[24, 80, 720, 408]],
                'native Wayland / calibrated geometry')
        require(report['binaries_identical'] == control and report['allow_identical_binaries'] == control,
                'A/A versus A/B identity')
        expected_order = [(pair, side) for pair in range(1, 6)
                          for side in (('before', 'after') if pair % 2 else ('after', 'before'))]
        require([(s['pair'], s['side']) for s in report['execution_order']] == expected_order,
                'AB/BA order')
        for key, name in [('script_sha256', 'compare-kokuban-revisions.py'),
                          ('comparator_sha256', 'compare-terminal-performance.py'),
                          ('pty_driver_helpers_sha256', 'linux-resource-smoke.py')]:
            require(report[key] == sha(files['source-reference/' + name]), 'harness source SHA')
        require(files[ARTIFACT + 'harness-revision.txt'].decode().strip() == github['headSha'],
                'recorded harness ref')
        integrity = load('download-integrity.json')
        artifact = load('github-artifacts.json')['artifacts'][0]
        zip_record = record['omitted_files']['artifact.zip']
        require(artifact['id'] == integrity['artifact_id']
                and artifact['digest'] == 'sha256:' + zip_record['sha256']
                and integrity['zip_sha256'] == zip_record['sha256']
                and integrity['zip_bytes'] == zip_record['bytes'], 'original ZIP API digest')
        for name, original_proof in integrity['files'].items():
            full = ARTIFACT + name
            if full in files:
                proof(files[full], original_proof)
            else:
                omitted = record['omitted_files'][full]
                require(all(omitted[key] == original_proof[key] for key in ('bytes', 'sha256')),
                        'explicit omitted source/ELF digest')
        observations = {}
        process_rtts = {}
        configs = set()
        for side in ('before', 'after'):
            terminal = report['terminals'][side]
            revision = BEFORE if side == 'before' or control else AFTER
            require(terminal['source_ref'] == revision == audit['comparison'][side]
                    and files[ARTIFACT + side + '-revision.txt'].decode().strip() == revision,
                    'application revision')
            build_side = 'before' if control else side
            build = ARTIFACT + build_side + '-build/'
            require(files[build + 'binary-sha256.txt'].decode().split() ==
                    [terminal['sha256'], terminal['path']], 'reported executable path and hash')
            for cargo in ('Cargo.toml', 'Cargo.lock'):
                require(sha(files[build + cargo]) == audit['builds'][side]['cargo_inputs_sha256'][cargo],
                        'Cargo input hash')
            require(len(terminal['samples']) == 5, 'five samples per label')
            observations[side] = []
            process_rtts[side] = []
            for pair, sample in enumerate(terminal['samples'], 1):
                prefix = ARTIFACT + f'measurements/{pair:02d}-{side}/'
                require(sample['pair'] == pair and sample['status'] == 'passed', 'sample identity')
                require(load(prefix + 'sample.json') == {k: v for k, v in sample.items() if k != 'pair'},
                        'report/sample match')
                measurement = load(prefix + 'result.json')
                require(measurement == sample['measurements'], 'sample/result match')
                config = files[prefix + 'kokuban.toml']
                require(sha(config) == sample['config']['sha256']
                        and config.decode() == sample['config']['text']
                        and sample['config']['history_limit'] == history
                        and f'scrollback_lines = {history}'.encode() in config,
                        'saved configuration and effective history')
                configs.add(sha(config))
                case = load(prefix + 'case.json')
                require(case['screen'] == screen and case['settle_seconds'] == 1,
                        'child screen and settle period')
                require(sample['command'][0] == terminal['path'], 'actual executable selection')
                require(measurement['initial_geometry'] == [24, 80, 720, 408]
                        and measurement['term'] == 'xterm-256color', 'initial PTY geometry')
                rtts = measurement['protocol_rtt_seconds']
                require(len(rtts) == 30 and all(math.isfinite(x) and x > 0 for x in rtts),
                        '30 finite positive recorded RTTs per process')
                process_rtts[side].append(rtts)
                require(list(measurement['workloads']) == list(WORKLOADS), 'four fixed workloads')
                for name, item in measurement['workloads'].items():
                    require(all(item[key] == report['payloads'][name][key] == payloads[name][key]
                                for key in ('bytes', 'sha256')), 'deterministic payload digest')
                    require(item['geometry_before'] == item['geometry_after'] == [24, 80, 720, 408]
                            and item['reply_hex'] == '1b5b313b3552', 'geometry and completed DSR barrier')
                    require(item['write_seconds'] > 0 and item['drain_rtt_seconds'] > 0
                            and item['terminal_cpu_seconds'] >= 0, 'nonnegative elapsed/CPU')
                    close(item['write_and_dsr_seconds'], item['write_seconds'] + item['drain_rtt_seconds'])
                    close(item['mib_per_second'], item['bytes'] / 1024 ** 2 / item['write_and_dsr_seconds'])
                    totals['workload_observations'] += 1
                observations[side].append(measurement['workloads'])
                totals['processes'] += 1
                totals['protocol_rtts'] += len(rtts)
            pooled = [v for sample in process_rtts[side] for v in sample]
            distribution(pooled, terminal['protocol_rtt_seconds'])
            distribution(pooled, audit['protocol_rtt'][side]['pooled_seconds'])
        require(len(configs) == 1, 'one configuration within run')
        medians = {side: [statistics.median(values) for values in process_rtts[side]]
                   for side in ('before', 'after')}
        for side in ('before', 'after'):
            for actual, saved in zip(medians[side], audit['protocol_rtt'][side]['per_process_median_seconds']):
                close(actual, saved)
        distribution([(a / b - 1) * 100 for b, a in zip(medians['before'], medians['after'])],
                     audit['protocol_rtt']['paired_process_median_change_percent'])
        for name in WORKLOADS:
            before = [m[name] for m in observations['before']]
            after = [m[name] for m in observations['after']]
            ratios = [a['write_and_dsr_seconds'] / b['write_and_dsr_seconds'] for b, a in zip(before, after)]
            throughput = [a['mib_per_second'] / b['mib_per_second'] for b, a in zip(before, after)]
            changes = [(v - 1) * 100 for v in ratios]
            stats = audit['workloads'][name]
            distribution(changes, stats['paired_elapsed_change_percent'])
            distribution([(v - 1) * 100 for v in throughput], stats['paired_throughput_change_percent'])
            for metric, values in [('after_over_before_elapsed_ratio', ratios),
                                   ('after_over_before_throughput_ratio', throughput),
                                   ('elapsed_change_percent', changes)]:
                distribution(values, report['paired_summary'][name][metric])
            close(statistics.median(changes[::2]), stats['AB_median_elapsed_change_percent'])
            close(statistics.median(changes[1::2]), stats['BA_median_elapsed_change_percent'])
            throughput_changes = [(value - 1) * 100 for value in throughput]
            close(statistics.median(throughput_changes[::2]), stats['AB_median_throughput_change_percent'])
            close(statistics.median(throughput_changes[1::2]), stats['BA_median_throughput_change_percent'])
            close(statistics.median((a['write_and_dsr_seconds'] - b['write_and_dsr_seconds']) * 1000
                                    for b, a in zip(before, after)), stats['median_paired_elapsed_delta_ms'])
            require(stats['slower_pairs'] == sum(v > 0 for v in changes)
                    and stats['faster_pairs'] == sum(v < 0 for v in changes), 'all pair signs retained')
            for side in ('before', 'after'):
                for metric, saved in stats['by_revision'][side].items():
                    distribution([m[name][metric] for m in observations[side]], saved)
                for metric, saved in report['terminals'][side]['summary'][name].items():
                    distribution([m[name][metric] for m in observations[side]], saved)
            require(stats == summary['runs'][run_id]['workloads'][name], 'summary/raw audit agreement')
        if control:
            require(report['comparison_mode'] == 'same-executable-variability'
                    and report['terminals']['before']['path'] == report['terminals']['after']['path']
                    and report['terminals']['before']['sha256'] == report['terminals']['after']['sha256'],
                    'same-executable A/A')
            messages = [json.loads(line) for line in files[ARTIFACT + 'before-build/cargo-messages.jsonl'].splitlines()]
            app = [m for m in messages if m['reason'] == 'compiler-artifact' and m['target']['name'] == 'kokuban']
            require(len(app) == 1 and not app[0]['fresh'] and app[0]['profile']['opt_level'] == '3'
                    and not app[0]['profile']['test'] and app[0]['executable'].endswith('/kokuban-paired-build-before/release/kokuban'),
                    'one fresh optimized non-test Cargo application artifact')
            require([m for m in messages if m['reason'] == 'build-finished'] ==
                    [{'reason': 'build-finished', 'success': True}], 'successful Cargo build')
            require(not any(name.startswith(ARTIFACT + 'after-build/') for name in files), 'only one build')
            source = audit['source_proof']
            require(len(load(ARTIFACT + 'source-git-manifest.json')) == source['git_blobs_verified'], 'source blob inventory')
            require(record['omitted_files'][ARTIFACT + 'before-build/kokuban']['sha256'] == source['ELF_sha256']
                    and record['omitted_files'][ARTIFACT + 'before-source.tar.gz']['sha256'] == source['compressed_archive_sha256'],
                    'original independently audited source/ELF hashes')
        short = audit['workloads']['short_lines']['paired_elapsed_change_percent']
        print(f"{run_id}: PASS; short_lines {short['median']:+.6f}% ({short['min']:+.6f}%..{short['max']:+.6f}%)")
    require(totals == {'processes': 40, 'workload_observations': 160, 'protocol_rtts': 1200}, 'total retained observations')
    print('PASS:', totals, '; original ELF/source bytes omitted, so their original audit cannot be repeated from this package alone.')


if __name__ == '__main__':
    main()
