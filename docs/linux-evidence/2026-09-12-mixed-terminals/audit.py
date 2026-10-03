#!/usr/bin/env python3
"""Audit the fixed four-terminal run without running a terminal or benchmark."""
import argparse
import ast
import hashlib
import json
import math
from pathlib import Path, PurePosixPath
import re
import shutil
import statistics
import subprocess
from types import SimpleNamespace

RUN = 34721599354
REV = '74eaad83dbf813cb15bd883b45d52adf0dc729bd'
TERMINALS = ['kokuban', 'ghostty', 'alacritty', 'kitty']
GEOMETRY = [24, 80, 720, 408]
WORKLOADS = ['ascii', 'ansi', 'unicode', 'short_lines']


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_json(path):
    return json.loads(path.read_bytes())


def distribution(values):
    median = statistics.median(values)
    return dict(count=len(values), median=median, min=min(values), max=max(values),
                median_absolute_deviation=statistics.median(abs(x-median) for x in values),
                samples=values)


def close(a, b, context):
    require(math.isfinite(a) and math.isfinite(b)
            and math.isclose(a, b, rel_tol=1e-10, abs_tol=1e-9), context)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifact', required=True, type=Path)
    parser.add_argument('--ci-run', required=True, type=Path)
    parser.add_argument('--ci-log', required=True, type=Path)
    parser.add_argument('--repo', type=Path, default=Path('/tmp/kokuban-mixed-integration'))
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    artifact = args.artifact.resolve()
    measurements = artifact / 'measurements'
    raw_path = measurements/'report.json'
    if not raw_path.exists():
        raw_path = artifact/'ci-report.json'
    raw_bytes = raw_path.read_bytes()
    report = json.loads(raw_bytes)
    run = read_json(args.ci_run)
    log_bytes = args.ci_log.read_bytes()
    log = log_bytes.decode(errors='replace')
    if (artifact/'manifest.json').exists():
        retained = read_json(artifact/'manifest.json')['evidence_sha256']
        actual = {str(p.relative_to(artifact)): digest(p.read_bytes())
                  for p in artifact.rglob('*') if p.is_file() and p != artifact/'manifest.json'}
        require(actual == retained, 'complete retained package hashes')
    require(run['databaseId'] == RUN and run['headSha'] == REV, 'CI identity')
    require(run['status'] == 'completed' and run['conclusion'] == 'success', 'CI terminal success')
    require(all(job['conclusion'] == 'success' for job in run['jobs']), 'CI jobs')
    require((artifact / 'source-revision.txt').read_text().strip() == REV, 'source revision')
    require((artifact / 'harness-revision.txt').read_text().strip() == REV, 'harness revision')

    def git_bytes(path):
        return subprocess.check_output(['git', '-C', str(args.repo), 'show', f'{REV}:{path}'])

    snapshots = {}
    for path in ['Cargo.toml', 'Cargo.lock', 'scripts/compare-terminal-performance.py',
                 'scripts/linux-resource-smoke.py', 'scripts/linux-launch-smoke.py',
                 '.github/workflows/linux-terminal-comparison.yml']:
        snapshots[path] = git_bytes(path)
    for name in ['Cargo.toml', 'Cargo.lock']:
        require((artifact / name).read_bytes() == snapshots[name], f'exact source {name}')
    workflow = snapshots['.github/workflows/linux-terminal-comparison.yml'].decode()
    for text in ['git archive "$revision"', 'dst=/source,readonly', '--workdir /source',
                 'CARGO_TARGET_DIR=/build/target', 'cargo build --release --locked',
                 '--screen alternate --samples 5 --bytes 33554432', '--match-cell-size']:
        require(text in workflow and text in log, f'build/run recipe: {text}')
    require(re.search(r'Compiling kokuban v[^\n]*\(/source\)', log), 'root crate built in /source')
    require(REV in log, 'revision appears in CI log')
    require('kokuban v0.1.0 (/source)' in (artifact/'cargo-tree.txt').read_text(), 'source cargo tree')
    require('rustc 1.94.1' in (artifact/'rustc.txt').read_text(), 'fixed Rust')
    require(report['script_sha256'] == digest(snapshots['scripts/compare-terminal-performance.py']), 'harness hash')

    # Execute only the audited pure payload/configuration functions. Replace the
    # config writer with an in-memory path; no terminal, subprocess or I/O runs.
    writes = {}
    class ConfigPath(PurePosixPath):
        def resolve(self):
            return self
        def write_text(self, value):
            writes[str(self)] = value
    syntax = ast.parse(snapshots['scripts/compare-terminal-performance.py'])
    selected = [node for node in syntax.body if isinstance(node, ast.FunctionDef)
                and node.name in {'payloads', 'terminal_command'}]
    require(len(selected) == 2, 'two pinned generator functions')
    namespace = {'Path': ConfigPath, '__file__': '/harness/compare-terminal-performance.py',
                 'sys': SimpleNamespace(executable='/usr/bin/python3'), 'FONT': 'DejaVu Sans Mono',
                 're': re, 'hashlib': hashlib}
    exec(compile(ast.Module(body=selected, type_ignores=[]), '<pinned generators>', 'exec'), namespace)
    payloads = {name: {'bytes': len(data), 'sha256': digest(data)}
                for name, data in namespace['payloads'](33554432).items()}
    probe_data = namespace['payloads'](1024)['ascii']
    probe = {'bytes': len(probe_data), 'sha256': digest(probe_data)}
    require(report['payloads'] == payloads, '32 MiB payloads reproduced from pinned Git')
    require(list(report['terminals']) == TERMINALS, 'four terminal order')
    require(report['status'] == 'passed' and report['in_progress'] is None, 'report completed')
    require(report['screen'] == 'alternate' and report['backend'] == 'wayland', 'screen/backend')
    require(report['requested_geometry'] == [24, 80] and report['font_pixels'] == 14.0, 'requested geometry/font')
    require(report['cpu_count'] == 4 and report['cpu_affinity'] == [0, 1, 2, 3], 'full CPU affinity')
    require(report['hardware']['status'] == 0 and report['hardware']['output'] == (artifact/'cpu.txt').read_text().strip(), 'CPU provenance')
    require(report['font_match']['status'] == 0 and report['font_match']['output'].startswith('DejaVu Sans Mono\n'), 'font match')
    require(report['environment']['DISPLAY'] is None and report['environment']['LIBGL_ALWAYS_SOFTWARE'] == '1', 'Wayland/software request')
    expected_order = [[index, name] for index in range(5)
                      for name in TERMINALS[index % 4:] + TERMINALS[:index % 4]]
    require(report['execution_order'] == expected_order, 'rotating process order')
    logged_order = [(int(index)-1, name) for index, name in re.findall(r'sample ([1-5])/5: (kokuban|ghostty|alacritty|kitty)', log)]
    require(logged_order == [tuple(x) for x in expected_order], 'CI recorded process order')
    comparable = report['comparability']
    require(comparable['geometry_and_history_checks_passed'] and not comparable['reasons'], 'comparability')
    require(comparable['geometries_rows_cols_pixels'] == [GEOMETRY], 'timed geometries')
    require(comparable['rendering_equivalence_verified'] is False and comparable['ranking'] is None, 'scope limits')
    calibration = report['cell_size_calibration']
    require(report['match_cell_size'] and calibration['status'] == 'passed', 'calibration success')
    observed_preflight_order = re.findall(r'cell calibration (baseline|adjusted): (kokuban|ghostty|alacritty|kitty)', log)
    require(observed_preflight_order == [(phase, name) for phase in ['baseline', 'adjusted'] for name in TERMINALS], 'CI preflight order')
    require(calibration['included_in_timed_statistics'] is False, 'preflights excluded')
    require(calibration['rendering_equivalence_verified'] is False, 'visual equivalence not inferred')
    require(calibration['target_cell_pixels'] == [9, 17], 'target cells')
    require({k: calibration['payload'][k] for k in probe} == probe, 'probe payload')
    offsets = calibration['spacing_adjustments_pixels']
    require(offsets == {'kokuban': [0, 0], 'ghostty': [1, 1], 'alacritty': [1, 0], 'kitty': [1, 0]}, 'spacing')

    process_audit = []
    process_files = []
    terminal_pids, child_pids = [], []
    binary_hashes = [terminal['sha256'] for terminal in report['terminals'].values()]
    require(len(set(binary_hashes)) == 4 and all(re.fullmatch('[0-9a-f]{64}', h) for h in binary_hashes), 'four distinct binary hashes')
    recorded_binary = (artifact/'kokuban-binary-sha256.txt').read_text().split()
    require(recorded_binary == [report['terminals']['kokuban']['sha256'], '/build/target/release/kokuban'], 'Kokuban binary record')
    old = read_json(args.repo/'docs/linux-evidence/2026-09-11-boundary-terminals/manifest.json')
    require(binary_hashes[0] != old['build']['binary_sha256'], 'new Kokuban differs from previous source binary')
    for phase in ['baseline', 'adjusted']:
        require(list(calibration['preflight'][phase]) == TERMINALS, f'{phase} four preflights')
    grouped = [(name, f'{index:02d}-{name}', sample, True, 'adjusted')
               for name in TERMINALS for index, sample in enumerate(report['terminals'][name]['samples'], 1)]
    grouped += [(name, f'preflight-{phase}-{name}', calibration['preflight'][phase][name], False, phase)
                for phase in ['baseline', 'adjusted'] for name in TERMINALS]
    require(len(grouped) == 28, '28 processes')
    for name, folder, sample, timed, phase in grouped:
        directory = measurements/folder
        require(read_json(directory/'sample.json') == sample and sample['status'] == 'passed', f'{folder} sample')
        result = read_json(directory/'result.json')
        require(result == sample['measurements'], f'{folder} raw measurements')
        require((directory/'finish').is_file() and not (directory/'child-error.json').exists(), f'{folder} completion')
        case = read_json(directory/'case.json')
        child = read_json(directory/'child.json')
        terminal_pids.append(case['terminal_pid'])
        child_pids.append(child['pid'])
        require(case['screen'] == 'alternate' and case['settle_seconds'] == 1.0, f'{folder} screen/settle')
        expected_payloads = {k: f'/evidence/measurements/{k}.bin' for k in WORKLOADS} if timed else {'geometry_probe': '/evidence/measurements/geometry-probe.bin'}
        require(case['payloads'] == expected_payloads, f'{folder} case payload paths')
        config_args = SimpleNamespace(screen='alternate', font_pixels=14.0, columns=80, rows=24,
                                      backend='wayland', cell_adjustments=offsets if phase == 'adjusted' else {})
        remote_dir = ConfigPath('/evidence/measurements')/folder
        expected_command, expected_config = namespace['terminal_command'](name, ConfigPath(report['terminals'][name]['path']), report['terminals'][name]['version'], remote_dir, config_args)
        require(sample['command'] == expected_command and sample['config'] == expected_config, f'{folder} pinned config/argv')
        require((directory/expected_config['filename']).read_bytes() == expected_config['text'].encode(), f'{folder} config bytes')
        expected_geometry = GEOMETRY if phase == 'adjusted' else [24, 80, (9-offsets[name][0])*80, (17-offsets[name][1])*24]
        require(result['initial_geometry'] == expected_geometry, f'{folder} initial geometry')
        require(len(result['protocol_rtt_seconds']) == 30 and all(math.isfinite(x) and x > 0 for x in result['protocol_rtt_seconds']), f'{folder} RTTs')
        expected_workloads = payloads if timed else {'geometry_probe': probe}
        require(list(result['workloads']) == list(expected_workloads), f'{folder} workload order')
        for workload, measured in result['workloads'].items():
            require({k: measured[k] for k in ['bytes', 'sha256']} == expected_workloads[workload], f'{folder}/{workload} payload')
            require(measured['geometry_before'] == expected_geometry == measured['geometry_after'], f'{folder}/{workload} stable geometry')
            require(measured['reply_hex'] == '1b5b313b3552', f'{folder}/{workload} DSR marker')
            close(measured['write_seconds'] + measured['drain_rtt_seconds'], measured['write_and_dsr_seconds'], f'{folder}/{workload} time sum')
            close(measured['bytes']/1048576/measured['write_and_dsr_seconds'], measured['mib_per_second'], f'{folder}/{workload} throughput')
            require(measured['write_seconds'] > 0 and measured['drain_rtt_seconds'] > 0 and measured['terminal_cpu_seconds'] >= 0, f'{folder}/{workload} timings')
            require(measured['terminal_rss_before_kib'] > 0 and measured['terminal_rss_after_kib'] > 0, f'{folder}/{workload} RSS')
        paths = [directory/x for x in ['case.json', 'child.json', 'sample.json', 'result.json', 'finish', 'terminal.log', expected_config['filename']]]
        process_files.extend(paths)
        process_audit.append({'directory': folder, 'timed': timed, 'initial_geometry': expected_geometry,
                              'files_sha256': {p.name: digest(p.read_bytes()) for p in paths}})
    require(len(set(terminal_pids)) == 28 and len(set(child_pids)) == 28, 'fresh terminal and child processes')
    summaries = {}
    for name, terminal in report['terminals'].items():
        require(len(terminal['samples']) == 5, f'{name} five timed samples')
        summaries[name] = {'path': terminal['path'], 'version': terminal['version'],
                           'sha256_recorded_ci': terminal['sha256'], 'workloads': {}}
        for workload in WORKLOADS:
            rows = [s['measurements']['workloads'][workload] for s in terminal['samples']]
            for metric, published in terminal['summary'][workload].items():
                require(distribution([row[metric] for row in rows]) == published, f'{name}/{workload}/{metric} recomputed')
            summaries[name]['workloads'][workload] = {metric: distribution([row[metric] for row in rows])
                                                     for metric in ['mib_per_second', 'terminal_cpu_seconds', 'write_and_dsr_seconds', 'drain_rtt_seconds']}
        rtts = [rtt for sample in terminal['samples'] for rtt in sample['measurements']['protocol_rtt_seconds']]
        require(distribution(rtts) == terminal['protocol_rtt_seconds'] and len(rtts) == 150, f'{name} RTT summary')
        summaries[name]['protocol_rtt'] = distribution(rtts)

    packages = (artifact/'system-packages.txt').read_text()
    for package in ['alacritty', 'ghostty', 'kitty', 'weston', 'fonts-dejavu-core', 'fonts-noto-cjk', 'fonts-noto-color-emoji']:
        require(re.search(rf'(?m)^{package}\s+\S+', packages), f'package {package}')
    image = read_json(artifact/'container-image.json')[0]
    require(image['Architecture'] == 'amd64' and image['Os'] == 'linux', 'container architecture')
    require((artifact/'image-reference.txt').read_text().strip() in image['RepoDigests'], 'image digest')
    weston = (artifact/'weston.log').read_text()
    require('pixman' in weston.lower() and re.search(r'weston\s+14\.0\.2', weston, re.I), 'observed Weston Pixman')
    fonts = (artifact/'font-fallbacks.txt').read_text()
    require('Noto Sans CJK' in fonts and 'Noto Color Emoji' in fonts, 'installed CJK/emoji fallbacks')
    font_hashes = (artifact/'font-files-sha256.txt').read_text().splitlines()
    require(all(re.fullmatch(r'[0-9a-f]{64}\s+/.+', line) for line in font_hashes), 'recorded font hashes')
    require(set(line.split('|', 1)[1] for line in fonts.splitlines()) == set(line.split(None, 1)[1] for line in font_hashes), 'font fallback/hash file coverage')
    native = read_json(artifact/'native-launch.txt')
    require(native['result'] == 'passed' and native['headless_cli'] == 'passed' and native['backend'] == 'wayland', 'native launch')
    child = native['child']
    require(child['cwd'] == child['pwd'] and 'cwd with spaces' in child['cwd'], 'native cwd')
    require(child['argv'] == ['file with spaces.txt', 'a;$(false)`false`', '--title=child argument', ''], 'literal argv')
    require(child['tty'] == [True]*3 and child['cursor_response'] == '\x1b[1;1R', 'native TTY/DSR')
    rust_bytes = (artifact/'rust-tests.txt').read_bytes()
    rust_log = rust_bytes.decode(errors='replace')
    rust_results = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored[^\n]*', rust_log)
    require(len(rust_results) == 3 and all(failed == '0' for _, failed, _ in rust_results), 'all Rust suites passed')
    require(rust_results == [('703', '0', '1'), ('0', '0', '0'), ('241', '0', '0')], 'fixed source test totals')
    warnings = {folder: (measurements/folder/'terminal.log').read_text(errors='replace').splitlines()
                for _, folder, _, _, _ in grouped if (measurements/folder/'terminal.log').stat().st_size}
    audit = {'schema': 1, 'date': '2026-09-12', 'run': {'id': RUN, 'url': run['url'],
             'source_revision': REV, 'harness_revision': REV, 'status': 'success',
             'artifact_name': 'linux-four-terminal-comparison', 'raw_report_byte_identical': True,
             'ci_log_sha256': digest(log_bytes), 'ci_log_bytes': len(log_bytes)},
             'build': {'cargo_inputs_verified_against_git': True, 'separate_container_build': True,
                       'root_crate_path': '/source', 'binary_sha256': binary_hashes[0],
                       'binary_hash_recorded_in_ci': True, 'binary_bytes_retained': False,
                       'binary_independently_rehashed': False, 'differs_from_previous_recorded_binary': True,
                       'pinned_source_sha256': {p: digest(b) for p, b in snapshots.items()}},
             'environment': {key: report[key] for key in ['platform', 'machine', 'cpu_count', 'cpu_affinity', 'hardware', 'font_match', 'environment_note', 'environment']},
             'measurement': {'screen': 'alternate', 'history': 0, 'geometry': GEOMETRY, 'cell_pixels': [9,17],
                             'spacing_adjustments': offsets, 'samples_per_terminal': 5, 'timed_processes': 20,
                             'workloads': 80, 'rtt_observations': 600, 'untimed_preflights': 8,
                             'preflights_in_statistics': False, 'execution_order': expected_order,
                             'payloads_reproduced_from_git': payloads, 'process_audit': process_audit},
             'terminal_summaries': summaries, 'validation': {'native_launch': native,
                 'rust_results': rust_results, 'rust_log_sha256': digest(rust_bytes),
                 'rust_warning_lines': sum(line.startswith('warning:') for line in rust_log.splitlines()),
                 'nonempty_terminal_logs': list(warnings)},
             'limits': ['Processing/DSR only; not frame presentation or input-to-photon.',
                        'Equal geometry and installed fallback fonts do not prove visual or Unicode rendering equivalence.',
                        'Weston Pixman observed; llvmpipe requested; effective GL_RENDERER per terminal not observed.',
                        'No physical Omarchy/Hyprland/GPU/monitor measurement.',
                        'Executable and font bytes not retained; their hashes are recorded by CI, not independently rehashed.',
                        'Earlier runs on other hosts are historical observations, not paired before/after evidence.']}
    if args.output:
        output = args.output
        require(not output.exists(), 'output must be new to avoid overwriting another audit')
        output.mkdir(parents=True)
        def keep(source, destination):
            target = output/destination
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            require(target.read_bytes() == source.read_bytes(), f'byte-identical retention {destination}')
        keep(raw_path, 'ci-report.json')
        keep(args.ci_run, 'ci-run.json')
        keep(args.ci_log, 'ci.log')
        for filename in ['source-revision.txt', 'harness-revision.txt', 'Cargo.toml', 'Cargo.lock',
                         'cargo-tree.txt', 'kokuban-binary-sha256.txt', 'rustc.txt', 'rust-tests.txt',
                         'system-packages.txt', 'all-system-packages.txt', 'font-fallbacks.txt',
                         'font-files-sha256.txt', 'cpu.txt', 'container-image.json', 'image-reference.txt',
                         'weston.log', 'native-launch.txt']:
            keep(artifact/filename, filename)
        for path in process_files:
            keep(path, Path('measurements')/path.relative_to(measurements))
        for path, data in snapshots.items():
            if path in ['Cargo.toml', 'Cargo.lock']:
                continue
            target = output/'harness'/path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        keep(Path(__file__), 'audit.py')
        (output/'terminal-log-observations.json').write_text(json.dumps(warnings, indent=2)+'\n')
        audit['retention'] = 'Raw report, CI log, Rust log, all 28 process configs/cases/results/child records/terminal logs retained byte-identically; no caches, payload or executable bytes.'
        audit['evidence_sha256'] = {str(path.relative_to(output)): digest(path.read_bytes())
                                  for path in sorted(output.rglob('*')) if path.is_file()}
        (output/'manifest.json').write_text(json.dumps(audit, indent=2)+'\n')
    print(json.dumps({'run': RUN, 'raw_sha256': digest(raw_bytes), 'processes': len(process_audit),
                      'cpu': re.search(r'Model name:\s*(.+)', report['hardware']['output']).group(1),
                      'rust_results': rust_results,
                      'throughput': {name: {w: summary['workloads'][w]['mib_per_second'] for w in WORKLOADS}
                                     for name, summary in summaries.items()},
                      'rtt_median_ms': {name: s['protocol_rtt']['median']*1000 for name,s in summaries.items()},
                      'output': str(args.output) if args.output else None}, indent=2))


if __name__ == '__main__':
    main()
