#!/usr/bin/env python3
"""Verify the retained evidence without a build, display, network or benchmark.

With --extract DIR, reconstruct the retained original files beneath DIR.
Executable/source-tar bytes and installed font bytes are not included.
"""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import re
import statistics


def require(condition, message):
    if not condition:
        raise ValueError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--extract', type=Path, help='new directory for the retained original files')
    args = parser.parse_args()
    base = Path(__file__).resolve().parent
    manifest = json.loads((base / 'manifest.json').read_text())
    summary = json.loads((base / 'summary.json').read_text())
    sha = lambda value: hashlib.sha256(value).hexdigest()
    objects = {}
    for key, item in manifest['objects'].items():
        require(item['path'] == f'objects/{key}.xz', 'unexpected object path')
        packed = (base / item['path']).read_bytes()
        raw = lzma.decompress(packed)
        require(len(packed) == item['bytes'] and sha(packed) == item['sha256'], 'compressed checksum')
        require(len(raw) == item['raw_bytes'] and sha(raw) == key == item['raw_sha256'], 'raw checksum')
        objects[key] = raw
    files = {}
    for item in manifest['files']:
        path = Path(item['source_path'])
        require(not path.is_absolute() and '..' not in path.parts, 'unsafe source path')
        key = (item['run'], item['architecture'], item['source_path'])
        require(key not in files, 'duplicate retained path')
        files[key] = objects[item['object']]
    require(set(objects) == {item['object'] for item in manifest['files']}, 'unreferenced object')
    require({str(p.relative_to(base)) for p in (base / 'objects').iterdir()} == {v['path'] for v in manifest['objects'].values()}, 'unexpected object file')
    modes = {(c, g, i) for c in ('ascii', 'ascii-after-emoji', 'unicode')
             for g in ('single-row', 'full-screen') for i in (False, True)}
    sample_pattern = re.compile(r'^frame-repaint sample content=(\S+) change=(\S+) sample=0 incremental=(true|false) frames=300 elapsed_ns=(\d+) ns_per_frame=([0-9.]+)$', re.M)
    fnv_cache = {}
    log_count = record_count = image_count = 0
    for run in manifest['runs']:
        run_id = run['id']
        metadata = json.loads(files[(run_id, None, 'run-metadata.json')])
        require(metadata['conclusion'] == 'success' and metadata['headSha'] == run['harness_revision'], 'run provenance')
        fonts = {arch: json.loads(files[(run_id, arch, 'font-files.json')]) for arch in run['architectures']}
        shared = fonts['x86_64'].keys() & fonts['aarch64'].keys()
        require(len(shared) == 45 and all(fonts['x86_64'][p] == fonts['aarch64'][p] for p in shared), 'shared font hashes')
        for arch in run['architectures']:
            raw = lambda name: files[(run_id, arch, name)]
            read = lambda name: json.loads(raw(name))
            report = read('measurements/report.json')
            require(report['status'] == 'completed' and report['cpu_affinity'] == [0], 'incomplete run')
            require(report['settings'] == {'pairs': 6, 'steps': 300, 'warmup': 30, 'samples_per_process': 1}, 'settings')
            require(sha(raw('compare-frame-revisions.py')) == report['runner_sha256'], 'runner hash')
            require(sha(raw('injection/injection.json')) == report['injection_sha256'], 'injection hash')
            require(sha(raw('injection/benchmark.rs.txt')) == report['benchmark_sha256'], 'benchmark hash')
            for side, build in report['builds'].items():
                require(sha(raw(f'{side}-cargo-messages.jsonl')) == build['cargo_messages_sha256'], 'Cargo hash')
                messages = [json.loads(line) for line in raw(f'{side}-cargo-messages.jsonl').splitlines()]
                require([m for m in messages if m['reason'] == 'build-finished'] == [{'reason': 'build-finished', 'success': True}], 'Cargo completion')
                apps = [m for m in messages if m['reason'] == 'compiler-artifact' and m['target']['name'] == 'kokuban']
                require(len(apps) == 1 and apps[0]['fresh'] is False and apps[0]['profile']['opt_level'] == '3' and apps[0]['profile']['test'] is True, 'fresh optimized build')
                require(apps[0]['executable'] == build['binary'], 'executable provenance')
                require(read(f'injection/{side}/prepared-source-manifest.json') == build['manifest'], 'prepared manifest')
            if report['comparison'] == 'same-binary':
                require(report['execution_binaries']['before'] == report['execution_binaries']['after'], 'A/A must use one binary/path')
            order = [(p, s) for p in range(1, 7) for s in (('before', 'after') if p % 2 else ('after', 'before'))]
            require([(e['pair'], e['side']) for e in report['execution_order']] == order, 'balanced execution order')
            parsed = []
            for pair, side in order:
                text = raw(f'measurements/{pair:02d}-{side}.log').decode()
                require(len(re.findall(r'^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; \d+ filtered out;', text, re.M)) == 1, 'incomplete process log')
                fixtures = [line[line.index('frame-repaint fixture'):] for line in text.splitlines() if 'frame-repaint fixture' in line]
                require(fixtures == [f['line'] for f in report['fixtures']], 'fixture mismatch')
                seen = set()
                for content, change, incremental, elapsed, rounded in sample_pattern.findall(text):
                    key = (content, change, incremental == 'true')
                    require(key in modes and key not in seen, 'duplicate or unexpected sample')
                    seen.add(key)
                    value = int(elapsed) / 300
                    require(abs(value - float(rounded)) <= 0.000501, 'rounded timing mismatch')
                    parsed.append(dict(pair=pair, side=side, content=content, change=change, incremental=key[2], frames=300, elapsed_ns=int(elapsed), ns_per_frame=value))
                require(seen == modes, 'missing timing mode')
                log_count += 1
            require(parsed == report['records'] and len(parsed) == 144, 'raw records mismatch')
            record_count += len(parsed)
            exported = next(s['cases'] for s in summary['cases'] if s['run'] == run_id and s['architecture'] == arch)
            for case in report['summary']:
                key = (case['content'], case['change'], case['incremental'])
                values = {side: [x['ns_per_frame'] for x in parsed if x['side'] == side and (x['content'], x['change'], x['incremental']) == key] for side in ('before', 'after')}
                changes = [100 * (a / b - 1) for b, a in zip(values['before'], values['after'])]
                retained = next(s for s in exported if (s['content'], s['change'], s['incremental']) == key)
                require(values == case['samples_ns'] and changes == case['paired_latency_change_percent'], 'paired raw statistics')
                require(statistics.median(changes) == case['median_paired_latency_change_percent'] == retained['median_paired_time_change_percent'], 'paired median')
                require(changes == retained['paired_time_change_percent'] and sum(v > 0 for v in changes) == retained['slower_pairs'], 'retained statistics')
            for fixture in report['fixtures']:
                line = fixture['line']
                content, change = re.search(r'content=(\S+) change=(\S+)', line).groups()
                require((fixture['width'], fixture['height']) == (1080, 680), 'pixel dimensions')
                checksums = re.search(r'frame0=([0-9a-f]{16}) frame1=([0-9a-f]{16})$', line).groups()
                hashes = {}
                for side in ('before', 'after'):
                    for index in (0, 1):
                        name = f'{content}-{change}-{index}.xrgb8888le'
                        data = raw(f'measurements/{side}-frames/{name}')
                        digest = sha(data)
                        require(len(data) == 1080 * 680 * 4 and digest == report['frame_sha256'][side][name], 'pixel hash/size')
                        if digest not in fnv_cache:
                            value = 0xcbf29ce484222325
                            for byte in data:
                                value = ((value ^ byte) * 0x100000001b3) & 0xffffffffffffffff
                            fnv_cache[digest] = f'{value:016x}'
                        require(fnv_cache[digest] == checksums[index], 'pixel FNV')
                        hashes[(side, index)] = digest
                        image_count += 1
                require(hashes[('before', 0)] != hashes[('before', 1)], 'unchanged visible states')
                require(all(hashes[('before', i)] == hashes[('after', i)] for i in (0, 1)), 'unequal candidate pixels')
    require((log_count, record_count, image_count) == (120, 1440, 240), 'evidence count')
    if args.extract is not None:
        args.extract.mkdir(parents=True, exist_ok=False)
        for (run, arch, name), raw in files.items():
            target = args.extract / str(run) / (arch or 'run') / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(raw)
    print(f'PASS: {len(objects)} lossless objects, {len(files)} original paths, {log_count} logs, {record_count} timing records, {image_count} frame references.')
    print('This verifies retained evidence, not omitted executable/source-tar/font bytes or physical display latency.')


if __name__ == '__main__':
    main()
