"""First-pass read-only artifact audit; never executes retained benchmark binaries."""
from pathlib import Path
from fractions import Fraction
import hashlib
import json
import math
import statistics
import tarfile

ROOT = Path('/tmp/kokuban-perf-20260913/root-evidence')
WORKLOADS = ('ascii-regular', 'ascii-four-styles', 'mixed-unicode', 'unicode-scalars', 'graphemes')
FOOTPRINTS = ('atlas_bytes', 'scalar_cache_bytes')
FIXTURE = 'src/glyph_atlas/lookup_benchmark.rs'
SOURCE = 'src/glyph_atlas.rs'
MODULE = b'\n#[cfg(test)]\nmod lookup_benchmark;\n'


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def archive(path):
    manifest, selected = {}, {}
    with tarfile.open(path, 'r:gz') as stream:
        for member in stream:
            if member.isfile():
                name = member.name.removeprefix('./')
                assert name not in manifest
                data = stream.extractfile(member).read()
                manifest[name] = hashlib.sha256(data).hexdigest()
                if name in (SOURCE, FIXTURE):
                    selected[name] = data
    return manifest, selected


def audit(directory):
    report = json.loads((directory / 'measurements/report.json').read_text())
    assert report['status'] == 'completed'
    settings = report['settings']
    assert settings['pairs'] == 6
    assert (settings['iterations'], settings['warmup']) in ((1000, 100), (10000, 1000))
    expected_lookups = 384 * settings['iterations']
    hashes = json.loads((directory / 'artifact-sha256.json').read_text())
    assert all(sha(directory / name) == expected for name, expected in hashes.items())
    assert set(hashes) == {p.relative_to(directory).as_posix() for p in directory.rglob('*')
                           if p.is_file() and p.name != 'artifact-sha256.json'}
    assert sha(directory / 'compare-glyph-cache-revisions.py') == report['runner_sha256']
    assert sha(directory / 'glyph-cache-benchmark.rs') == report['fixture_sha256']
    assert report['fixture_sha256'] == '1aef5bccb4dc8592d9d4675315578fa0ef97db3ce488a3445569622ef065491c'
    assert sha(directory / 'injection/injection.json') == report['injection_sha256']
    injection = json.loads((directory / 'injection/injection.json').read_text())
    comparison = report['comparison']
    sides = ('before',) if comparison == 'same-binary' else ('before', 'after')
    assert set(report['builds']) == set(injection['sources']) == set(sides)
    original_manifests = {}
    for side in sides:
        original, original_bytes = archive(directory / f'{side}-original-source.tar.gz')
        prepared, prepared_bytes = archive(directory / f'{side}-prepared-source.tar.gz')
        provenance = injection['sources'][side]
        assert prepared == provenance['manifest'] == report['builds'][side]['manifest']
        assert original[SOURCE] == provenance['original_sha256']
        assert prepared_bytes[SOURCE] == original_bytes[SOURCE] + MODULE
        assert prepared_bytes[FIXTURE] == (directory / 'glyph-cache-benchmark.rs').read_bytes()
        assert set(prepared) - set(original) == {FIXTURE}
        assert {key for key in original if original[key] != prepared.get(key)} == {SOURCE}
        assert sha(directory / f'{side}-benchmark') == report['builds'][side]['binary_sha256']
        assert sha(directory / f'{side}-cargo-messages.jsonl') == report['builds'][side]['cargo_messages_sha256']
        assert (directory / f'{side}-revision.txt').read_text().strip() == report['builds'][side]['source_revision']
        assert report['builds'][side]['source_revision'] == {
            'before': 'b6cd9ad8b1204a4cb67587ecb042bd5cb8986266',
            'after': 'ddccc9bc4146629c34c089486d56897b0b3e3515',
        }[side]
        cargo = [json.loads(line) for line in (directory / f'{side}-cargo-messages.jsonl').read_text().splitlines()]
        matches = [row for row in cargo if row.get('reason') == 'compiler-artifact'
                   and row.get('executable') == report['builds'][side]['binary']]
        assert len(matches) == 1 and matches[0]['fresh'] is False
        assert matches[0]['profile']['test'] is True and matches[0]['profile']['opt_level'] == '3'
        original_manifests[side] = original
    if comparison == 'same-binary':
        assert report['execution_binaries']['before'] == report['execution_binaries']['after']
        assert not list(directory.glob('after-*'))
        differences = []
    else:
        a, b = original_manifests.values()
        differences = sorted(name for name in a.keys() | b.keys() if a.get(name) != b.get(name))
        assert differences == [SOURCE] == report['original_source_differences']
        assert report['execution_binaries']['before']['sha256'] != report['execution_binaries']['after']['sha256']
    order = [(pair, side) for pair in range(1, 7)
             for side in (('before', 'after') if pair % 2 else ('after', 'before'))]
    assert [(row['pair'], row['side']) for row in report['execution_order']] == order
    assert len(report['records']) == 60
    indexed = {(row['pair'], row['side'], row['workload']): row for row in report['records']}
    assert len(indexed) == 60
    for entry in report['execution_order']:
        pair, side = entry['pair'], entry['side']
        assert entry['command'][0] == report['execution_binaries'][side]['path']
        fixtures, samples = {}, {}
        for line in (directory / f'measurements/{pair:02d}-{side}.log').read_text().splitlines():
            if 'glyph-cache ' in line:
                line = 'glyph-cache ' + line.split('glyph-cache ', 1)[1]
            if line.startswith(('glyph-cache fixture ', 'glyph-cache sample ')):
                fields = dict(word.split('=', 1) for word in line.split()[2:])
                target = fixtures if line.startswith('glyph-cache fixture ') else samples
                assert fields['workload'] not in target
                target[fields['workload']] = fields
        assert set(fixtures) == set(samples) == set(WORKLOADS)
        for workload in WORKLOADS:
            fixture, sample = fixtures[workload], samples[workload]
            expected = report['fixtures'][side][workload]
            assert expected['iterations'] == settings['iterations']
            assert expected['warmup'] == settings['warmup']
            assert expected['lookups_per_iteration'] == 384 and expected['samples'] == 1
            assert fixture == {key: str(value) for key, value in expected.items() if key != 'font_name'}
            row = indexed[pair, side, workload]
            assert int(sample['elapsed_ns']) == row['elapsed_ns'] > 0
            assert int(sample['lookups']) == row['lookups'] == expected_lookups
            assert int(sample['sample']) == row['sample'] == 0
            assert math.isclose(row['ns_per_lookup'], row['elapsed_ns'] / row['lookups'], abs_tol=1e-12)
            assert bytes.fromhex(fixture['font_name_hex']).decode() == expected['font_name']
    fixtures = report['fixtures']
    assert {w: {k: v for k, v in f.items() if k not in FOOTPRINTS} for w, f in fixtures['before'].items()} == {
        w: {k: v for k, v in f.items() if k not in FOOTPRINTS} for w, f in fixtures['after'].items()}
    workloads = []
    for workload in WORKLOADS:
        changes, values = [], {side: [] for side in ('before', 'after')}
        for pair in range(1, 7):
            before, after = (indexed[pair, side, workload] for side in ('before', 'after'))
            changes.append(float((Fraction(after['elapsed_ns'], before['elapsed_ns']) - 1) * 100))
            for side in values:
                row = indexed[pair, side, workload]
                values[side].append(row['elapsed_ns'] / row['lookups'])
        summary = next(row for row in report['summary'] if row['workload'] == workload)
        median = statistics.median(changes)
        assert math.isclose(summary['median_paired_time_change_percent'], median, abs_tol=1e-12)
        assert all(math.isclose(a, b, abs_tol=1e-12) for a, b in zip(changes, summary['paired_time_change_percent']))
        workloads.append({'workload': workload, 'median_paired_time_change_percent': median,
                          'paired_time_change_percent': changes,
                          'median_AB_percent': statistics.median(changes[::2]),
                          'median_BA_percent': statistics.median(changes[1::2]),
                          'faster_pairs': sum(v < 0 for v in changes),
                          'median_ns_per_lookup': {side: statistics.median(v) for side, v in values.items()},
                          'median_elapsed_ms': {side: statistics.median(v) * expected_lookups / 1e6 for side, v in values.items()},
                          'elapsed_ms_range': {side: [min(v) * expected_lookups / 1e6, max(v) * expected_lookups / 1e6] for side, v in values.items()},
                          'footprints': {side: {key: fixtures[side][workload][key] for key in FOOTPRINTS} for side in values},
                          'glyph_counts': {key: fixtures['before'][workload][key] for key in ('scalar_glyphs', 'grapheme_glyphs')},
                          'font_name': fixtures['before'][workload]['font_name']})
    return {'artifact': directory.name, 'integrity': 'PASS', 'comparison': comparison,
            'hashed_artifact_files': len(hashes), 'original_source_differences': differences,
            'records': 60, 'pairs': 6, 'cpu_affinity': report['cpu_affinity'],
            'settings': settings,
            'affinity_note': report['affinity_note'], 'execution_binaries': report['execution_binaries'],
            'source_revisions': {side: report['builds'][side]['source_revision'] for side in sides},
            'binary_sizes': {side: (directory / f'{side}-benchmark').stat().st_size for side in sides},
            'font_manifest_sha256': sha(directory / 'font-files.json'),
            'footprint_scope': report['footprint_scope'], 'workloads': workloads}


if __name__ == '__main__':
    import sys
    for run in sys.argv[1:]:
        directory = ROOT / f'run-{run}'
        results = [audit(path) for path in sorted(directory.glob('glyph-cache-*')) if path.is_dir()]
        assert len(results) == 3
        target = directory / 'glyph-cache-first-pass-audit.json'
        target.write_text(json.dumps({'run': run, 'integrity': 'PASS', 'artifacts': results}, indent=2) + '\n')
        for result in results:
            print(run, result['artifact'], result['integrity'])
            for row in result['workloads']:
                print(row['workload'], f"{row['median_paired_time_change_percent']:+.3f}%",
                      f"faster={row['faster_pairs']}/6", 'AB/BA',
                      f"{row['median_AB_percent']:+.3f}/{row['median_BA_percent']:+.3f}%",
                      'ns', row['median_ns_per_lookup'], 'footprint', row['footprints'])
