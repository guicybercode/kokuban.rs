"""Verify this compact record without executing benchmarks or extracting ZIPs."""
from pathlib import Path
from fractions import Fraction
import hashlib
import json
import math
import statistics
import zipfile

ROOT = Path(__file__).resolve().parent
WORKLOADS = {'ascii-regular', 'ascii-four-styles', 'mixed-unicode', 'unicode-scalars', 'graphemes'}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def close(actual, expected):
    assert math.isclose(actual, expected, rel_tol=1e-12, abs_tol=1e-12), (actual, expected)


def verify():
    manifest = json.loads((ROOT / 'manifest.json').read_text())
    actual = {p.relative_to(ROOT).as_posix() for p in ROOT.rglob('*')
              if p.is_file() and p.name != 'manifest.json' and '__pycache__' not in p.parts}
    assert actual == set(manifest['files'])
    for name, expected in manifest['files'].items():
        data = (ROOT / name).read_bytes()
        assert len(data) == expected['bytes'] and digest(data) == expected['sha256'], name
    total = 0
    for run, metadata in manifest['runs'].items():
        run_dir = ROOT / f'run-{run}'
        github = json.loads((run_dir / 'github-run.json').read_text())
        assert github['conclusion'] == 'success' and len(github['jobs']) == 3
        assert all(job['conclusion'] == 'success' for job in github['jobs'])
        audit = json.loads((run_dir / 'audit.json').read_text())
        assert audit['integrity'] == 'PASS'
        with zipfile.ZipFile(ROOT / metadata['package']) as archive:
            assert set(archive.namelist()) == set(metadata['included'])
            assert len(archive.namelist()) == len(metadata['included'])
            for name, expected in metadata['included'].items():
                data = archive.read(name)
                assert len(data) == expected['bytes'] and digest(data) == expected['sha256'], name
                assert not name.endswith(('.tar', '.tar.gz', '-benchmark'))
            for result in audit['artifacts']:
                prefix = result['artifact'] + '/'
                report = json.loads(archive.read(prefix + 'measurements/report.json'))
                assert report['status'] == 'completed' and len(report['records']) == 60
                assert archive.read(prefix + 'harness-revision.txt').decode().strip() == github['headSha']
                original_hashes = json.loads(archive.read(prefix + 'artifact-sha256.json'))
                for name, expected in original_hashes.items():
                    selected = metadata['included'] if prefix + name in metadata['included'] else metadata['omitted']
                    assert selected[prefix + name]['sha256'] == expected
                assert digest(archive.read(prefix + 'glyph-cache-benchmark.rs')) == report['fixture_sha256']
                assert report['fixture_sha256'] == '1aef5bccb4dc8592d9d4675315578fa0ef97db3ce488a3445569622ef065491c'
                pairs = [(pair, side) for pair in range(1, 7)
                         for side in (('before', 'after') if pair % 2 else ('after', 'before'))]
                assert [(x['pair'], x['side']) for x in report['execution_order']] == pairs
                records = {(row['pair'], row['side'], row['workload']): row for row in report['records']}
                assert len(records) == 60
                if report['comparison'] == 'same-binary':
                    assert set(report['builds']) == {'before'}
                    assert report['execution_binaries']['before'] == report['execution_binaries']['after']
                for pair, side in pairs:
                    raw = archive.read(prefix + f'measurements/{pair:02d}-{side}.log').decode()
                    samples, fixtures = {}, {}
                    for line in raw.splitlines():
                        marker = line.find('glyph-cache ')
                        if marker < 0:
                            continue
                        words = line[marker:].split()
                        row = dict(word.split('=', 1) for word in words[2:])
                        target = samples if words[1] == 'sample' else fixtures
                        assert row['workload'] not in target
                        target[row['workload']] = row
                    assert set(samples) == set(fixtures) == WORKLOADS
                    for workload in WORKLOADS:
                        row, sample = records[pair, side, workload], samples[workload]
                        expected = report['fixtures'][side][workload]
                        assert fixtures[workload] == {k: str(v) for k, v in expected.items() if k != 'font_name'}
                        assert int(sample['lookups']) == row['lookups'] == 384 * report['settings']['iterations']
                        assert int(sample['elapsed_ns']) == row['elapsed_ns'] > 0
                        close(row['ns_per_lookup'], row['elapsed_ns'] / row['lookups'])
                for workload in WORKLOADS:
                    changes = [float((Fraction(records[pair, 'after', workload]['elapsed_ns'],
                                               records[pair, 'before', workload]['elapsed_ns']) - 1) * 100)
                               for pair in range(1, 7)]
                    summary = next(row for row in report['summary'] if row['workload'] == workload)
                    checked = next(row for row in result['workloads'] if row['workload'] == workload)
                    for actual, expected in zip(summary['paired_time_change_percent'], changes):
                        close(actual, expected)
                    close(summary['median_paired_time_change_percent'], statistics.median(changes))
                    close(checked['median_paired_time_change_percent'], statistics.median(changes))
                total += len(records)
    assert total == 720
    print('PASS: 4 independent runs, 12 artifacts, 512 original text files, 720 raw records; hashes and paired medians verified.')
    print('Retained metadata identifies omitted binaries/TARs; this check cannot rehash those omitted bytes.')


if __name__ == '__main__':
    verify()
