#!/usr/bin/env python3
"""Verify/extract retained evidence. No builds, benchmarks, network or Git mutations."""
import argparse, hashlib, io, json, lzma, math, re, statistics, subprocess, tarfile
from pathlib import Path, PurePosixPath


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def main(args):
    root = Path(__file__).resolve().parent
    for line in (root / 'evidence-sha256.txt').read_text().splitlines():
        expected, name = line.split('  ', 1)
        path = PurePosixPath(name)
        require(not path.is_absolute() and '..' not in path.parts, 'unsafe package path')
        require(digest((root / name).read_bytes()) == expected, 'package checksum: ' + name)
    metadata = json.loads((root / 'archive.json').read_text())
    inventory = json.loads((root / 'artifact-inventory.json').read_text())
    packed = (root / 'artifact.tar.xz').read_bytes()
    require(len(packed) == metadata['compressed_bytes'] and digest(packed) == metadata['compressed_sha256'], 'XZ identity')
    plain = lzma.decompress(packed)
    require(len(plain) == metadata['tar_bytes'] and digest(plain) == metadata['tar_sha256'], 'outer TAR identity')
    files = {}
    with tarfile.open(fileobj=io.BytesIO(plain), mode='r:') as archive:
        for member in archive:
            path = PurePosixPath(member.name)
            require(member.isfile() and not path.is_absolute() and '..' not in path.parts, 'unsafe archive member')
            require(member.name not in files and member.name in inventory, 'unexpected or duplicate member')
            data = archive.extractfile(member).read()
            require(len(data) == inventory[member.name]['bytes'] and digest(data) == inventory[member.name]['sha256'], 'original identity: ' + member.name)
            files[member.name] = data
    require(set(files) == set(inventory) and len(files) == 67, 'original inventory')
    load = lambda name: json.loads(files[name])
    report = load('measurements/report.json')
    injection = load('injection/injection.json')
    audit = json.loads((root / 'audit.json').read_text())
    provenance = json.loads((root / 'provenance.json').read_text())
    refs = provenance['revisions']
    ci = json.loads((root / 'ci-run.json').read_text())
    require(ci['databaseId'] == 34744896010 and ci['headSha'] == refs['harness'] and ci['conclusion'] == 'success', 'CI identity')
    require(report['status'] == 'completed' and report['machine'] == 'x86_64' and report['cpu_affinity'] == [0], 'completed native x86 run')
    require(report['settings'] == {'pairs': 5, 'steps': 300, 'warmup': 30, 'samples_per_process': 1}, 'settings')
    marker = b'    #[test]\n    #[ignore = "CPU raster microbenchmark; run release on an idle Linux host with --nocapture"]\n    fn benchmark_frame_repaint() {\n'
    benchmark = files['injection/benchmark.rs.txt']
    require(digest(benchmark) == injection['benchmark_sha256'] == report['benchmark_sha256'], 'common benchmark')
    require(digest(files['injection/injection.json']) == report['injection_sha256'], 'injection identity')
    require(digest(files['compare-frame-revisions.py']) == report['runner_sha256'], 'runner identity')
    git = lambda *parts: subprocess.check_output(['git', '-C', str(args.repo), *parts])
    if args.repo:
        for local, path in [('compare-frame-revisions.py', 'scripts/compare-frame-revisions.py'),
                            ('test_compare_frame_revisions.py', 'scripts/test_compare_frame_revisions.py'),
                            ('linux-render-performance.yml', '.github/workflows/linux-render-performance.yml')]:
            require(files[local] == git('show', refs['harness'] + ':' + path), 'pinned harness ' + path)
        harness = git('show', refs['harness'] + ':src/linux_window.rs')
        require(harness.count(marker) == 1 and marker + harness.split(marker)[1] == benchmark, 'pinned benchmark function')
    blob_counts = {}
    for side in ['before', 'after']:
        require(files[side + '-revision.txt'].decode().strip() == refs[side], 'source revision')
        raw_tar = files[side + '-original-source.tar']
        require(digest(raw_tar) == files[side + '-source-sha256.txt'].decode().split()[0], 'original TAR checksum')
        contents = {}
        with tarfile.open(fileobj=io.BytesIO(raw_tar), mode='r:') as archive:
            require(archive.pax_headers.get('comment') == refs[side], 'original TAR commit')
            for member in archive:
                if member.isdir():
                    continue
                require(member.isfile() or member.issym(), 'unsupported source member')
                require(member.name not in contents, 'duplicate source member')
                contents[member.name] = member.linkname.encode() if member.issym() else archive.extractfile(member).read()
        if args.repo:
            tree = {}
            for item in git('ls-tree', '-rz', refs[side]).split(b'\0'):
                if item:
                    attrs, name = item.split(b'\t', 1)
                    mode, kind, blob = attrs.decode().split()
                    require(kind == 'blob', 'unsupported Git tree entry')
                    tree[name.decode()] = blob
            require(set(contents) == set(tree), 'original source inventory')
            for name, data in contents.items():
                blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
                require(blob == tree[name], 'Git source content: ' + name)
            blob_counts[side] = len(tree)
        original = files['injection/' + side + '/original-linux_window.rs']
        injected = files['injection/' + side + '/injected-linux_window.rs']
        require(original == contents['src/linux_window.rs'] and original.count(marker) == 1, 'original runtime source')
        prefix = original.split(marker)[0]
        require(injected == prefix + benchmark, 'preserved runtime prefix')
        prov = injection['sources'][side]
        require(digest(prefix) == prov['preserved_prefix_sha256'] and digest(injected) == prov['injected_sha256'] and digest(original) == prov['original_sha256'], 'injection hashes')
        require({name: digest(contents[name]) for name in ['Cargo.toml', 'Cargo.lock']} == prov['cargo_sha256'], 'Cargo source inputs')
        build = report['builds'][side]
        require(build == provenance['sources'][side]['build'], 'retained build proof')
        messages_data = files[side + '-cargo-messages.jsonl']
        require(digest(messages_data) == build['cargo_messages_sha256'], 'Cargo JSON identity')
        messages = [json.loads(line) for line in messages_data.splitlines()]
        roots = [m for m in messages if m.get('reason') == 'compiler-artifact' and m['target']['name'] == 'kokuban' and m['profile'].get('test')]
        require(len(roots) == 1, 'root artifact count')
        item = roots[0]
        require(item['fresh'] is False and item['profile']['opt_level'] == '3' and item['manifest_path'] == build['source'] + '/Cargo.toml', 'fresh release source build')
        require(item['executable'] == build['binary'] and str(PurePosixPath(build['binary']).parent) == build['target'] + '/release/deps', 'separate binary target')
        require(messages[-1] == {'reason': 'build-finished', 'success': True}, 'build success')
    require(report['builds']['before']['target'] != report['builds']['after']['target'] and report['builds']['before']['binary_sha256'] != report['builds']['after']['binary_sha256'], 'distinct targets/binaries')
    sample = re.compile(r'frame-repaint sample content=(\S+) change=(\S+) sample=(\d+) incremental=(true|false) frames=(\d+) elapsed_ns=(\d+) ns_per_frame=([0-9.]+)')
    order = [(p, s) for p in range(1, 6) for s in (['before', 'after'] if p % 2 else ['after', 'before'])]
    require([(p['pair'], p['side']) for p in report['execution_order']] == order, 'alternating process pairs')
    records, fixtures = [], None
    for pair, side in order:
        text = files[f'measurements/{pair:02d}-{side}.log'].decode()
        require('test result: ok. 1 passed; 0 failed;' in text, 'benchmark correctness gate')
        current = [line[line.index('frame-repaint fixture'):] for line in text.splitlines() if 'frame-repaint fixture' in line]
        fixtures = current if fixtures is None else fixtures
        require(len(current) == 6 and current == fixtures, 'identical six fixtures')
        found = sample.findall(text)
        require(len(found) == 12, 'samples per process')
        for content, change, index, inc, frames, elapsed, ns in found:
            require(index == '0' and frames == '300' and int(elapsed) > 0, 'sample settings')
            exact = int(elapsed) / int(frames)
            require(abs(exact - float(ns)) <= .00051, 'sample arithmetic')
            records.append({'pair': pair, 'side': side, 'content': content, 'change': change, 'incremental': inc == 'true', 'frames': int(frames), 'elapsed_ns': int(elapsed), 'ns_per_frame': exact})
    require(records == report['records'] and len(records) == 120, '120 raw records')
    for claimed in report['summary']:
        key = claimed['content'], claimed['change'], claimed['incremental']
        values = {s: [next(r['ns_per_frame'] for r in records if r['pair'] == p and r['side'] == s and (r['content'], r['change'], r['incremental']) == key) for p in range(1, 6)] for s in ['before', 'after']}
        medians = {s: statistics.median(v) for s, v in values.items()}
        ranges = {s: [min(v), max(v)] for s, v in values.items()}
        deltas = [(a / b - 1) * 100 for a, b in zip(values['after'], values['before'])]
        require(values == claimed['samples_ns'] and medians == claimed['median_ns'] and ranges == claimed['range_ns'] and deltas == claimed['paired_latency_change_percent'], 'recomputed statistics')
        require((medians['after'] / medians['before'] - 1) * 100 == claimed['latency_change_percent'] and sum(v > 0 for v in deltas) == claimed['slower_pairs'], 'recomputed deltas/regressions')
    names = {f'{c}-{m}-{i}.xrgb8888le' for c in ['ascii', 'ascii-after-emoji', 'unicode'] for m in ['single-row', 'full-screen'] for i in [0, 1]}
    for name in names:
        a, b = [files['measurements/' + s + '-frames/' + name] for s in ['before', 'after']]
        require(a == b and len(a) == 1080 * 680 * 4, 'exact reference frame pair')
        require(digest(a) == report['frame_sha256']['before'][name] == report['frame_sha256']['after'][name], 'reference pixel hashes')
    for side in ['before', 'after']:
        for change in ['single-row', 'full-screen']:
            for index in [0, 1]:
                prefix = 'measurements/' + side + '-frames/'
                require(files[prefix + f'ascii-{change}-{index}.xrgb8888le'] == files[prefix + f'ascii-after-emoji-{change}-{index}.xrgb8888le'], 'offscreen emoji preserves ASCII pixels')
    if args.restore:
        destination = args.restore.resolve()
        require(not destination.exists() and destination != root and root not in destination.parents, 'restore destination must be new and outside package')
        destination.mkdir(parents=True)
        for name, data in files.items():
            path = destination / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
    print(json.dumps({'status': 'passed', 'original_files': len(files), 'git_blobs_verified': blob_counts,
                      'runtime_prefixes': 'preserved', 'samples': len(records), 'pixel_pairs': 12,
                      'ascii_pre_post_emoji_pairs': 8, 'restore': str(args.restore) if args.restore else None}))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, help='Local Git repository with all three revisions; enables exact blob/harness verification')
    parser.add_argument('--restore', type=Path, help='Optional new directory for all67 original files')
    main(parser.parse_args())
