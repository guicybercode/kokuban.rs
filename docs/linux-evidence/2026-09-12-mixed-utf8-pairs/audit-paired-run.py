#!/usr/bin/env python3
"""Audit one completed Linux paired run using only already downloaded files.

Required layout in --source-dir: the extracted paired artifact (87 files),
ci-run.json from gh run view --json databaseId,headSha,status,conclusion,url,jobs,
and ci.log from gh run view --log. --ci-run/--ci-log can point elsewhere.
Optional --artifact-zip and --artifact-metadata verify the actual archive against
the GitHub API digest; neither is fetched by this program. --repo selects the
local Git repository or worktree containing all three exact revisions.

The output directory must not exist. All auditing completes before it is created.
It retains original artifact bytes, complete supplied CI log and metadata, pinned
harness snapshots, recomputed statistics, diagnostics and a checksum inventory.
It never builds code, launches terminals, downloads files or invokes workflows.
Payload regeneration DOES run during a real audit, so run outside local timings.
"""

import argparse
import ast
import hashlib
import json
import math
from pathlib import Path
import re
import statistics
import subprocess
import sys
import tomllib
import zipfile


SIDES = ('before', 'after')
SCRIPT_FIELDS = {
    'scripts/compare-kokuban-revisions.py': 'script_sha256',
    'scripts/compare-terminal-performance.py': 'comparator_sha256',
    'scripts/linux-resource-smoke.py': 'pty_driver_helpers_sha256',
}
WORKFLOW = '.github/workflows/linux-paired-performance.yml'
ARTIFACT_NAME = 'linux-paired-revision-measurements'
ROOT_FILES = ('before-revision.txt', 'after-revision.txt', 'harness-revision.txt',
              'cpu.txt', 'kernel.txt', 'rustc.txt', 'system-packages.txt', 'weston.log')
BUILD_FILES = ('Cargo.toml', 'Cargo.lock', 'cargo-tree.txt', 'binary-sha256.txt')
PROCESS_FILES = ('case.json', 'sample.json', 'result.json', 'child.json',
                 'finish', 'kokuban.toml', 'terminal.log')
GEOMETRY = [24, 80, 720, 408]


class AuditError(Exception):
    pass


def require(condition, message):
    # Explicit checks remain enabled under Python -O.
    if not condition:
        raise AuditError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def reject_duplicates(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f'Duplicate JSON key: {key}')
        result[key] = value
    return result


def parse_json(data):
    return json.loads(data, object_pairs_hook=reject_duplicates,
                      parse_constant=lambda value: fail(f'Nonfinite JSON constant: {value}'))


def fail(message):
    raise AuditError(message)


def finite(value, context, positive=False):
    require(type(value) in (float, int) and math.isfinite(value), f'{context}: invalid number')
    require(value > 0 if positive else value >= 0, f'{context}: invalid sign')


def distribution(values):
    require(bool(values), 'Empty distribution')
    require(all(type(v) in (float, int) and math.isfinite(v) for v in values), 'Nonfinite distribution')
    median = statistics.median(values)
    return {'count': len(values), 'median': median, 'min': min(values), 'max': max(values),
            'median_absolute_deviation': statistics.median(abs(v - median) for v in values),
            'samples': values}


def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args], stderr=subprocess.PIPE)


def revision(repo, value):
    resolved = git(repo, 'rev-parse', '--verify', '--end-of-options', value + '^{commit}').decode().strip()
    require(re.fullmatch(r'[0-9a-f]{40}', resolved), 'Expected a SHA-1 Git commit')
    return resolved


def payloads_from_pinned_source(data, count):
    """Evaluate only the pinned pure function with restricted builtins/nodes."""
    tree = ast.parse(data)
    functions = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == 'payloads']
    require(len(functions) == 1, 'Pinned comparator must define exactly one payloads function')
    function = functions[0]
    allowed_nodes = (ast.FunctionDef, ast.arguments, ast.arg, ast.Assign, ast.Dict,
                     ast.Constant, ast.BinOp, ast.Add, ast.Mult, ast.DictComp,
                     ast.comprehension, ast.Name, ast.Load, ast.Store, ast.Return,
                     ast.Call, ast.Attribute, ast.Subscript, ast.FloorDiv, ast.Tuple)
    allowed_names = {'payloads', 'byte_count', 'lines', 'name', 'line', 'max', 'len', 'dict', 'str', 'bytes', 'int'}
    for node in ast.walk(function):
        require(isinstance(node, allowed_nodes), f'Payload generator needs review: {type(node).__name__}')
        if isinstance(node, ast.Name):
            require(node.id in allowed_names, f'Unexpected payload generator name: {node.id}')
        if isinstance(node, ast.Call):
            require(not node.keywords, 'Unexpected payload generator keyword argument')
            if isinstance(node.func, ast.Name):
                require(node.func.id in ('max', 'len'), 'Unexpected payload generator call')
            else:
                require(isinstance(node.func, ast.Attribute) and node.func.attr in ('encode', 'items')
                        and not node.args, 'Unexpected payload generator method')
        if isinstance(node, ast.Attribute):
            require(node.attr in ('encode', 'items'), 'Unexpected payload generator attribute')
    builtins = {'max': max, 'len': len, 'dict': dict, 'str': str, 'bytes': bytes, 'int': int}
    scope = {'__builtins__': builtins}
    exec(compile(ast.Module(body=[function], type_ignores=[]), '<pinned-payload-generator>', 'exec'), scope)
    generated = scope['payloads'](count)
    require(set(generated) == {'ascii', 'ansi', 'unicode', 'short_lines'}, 'Unexpected payload set')
    return {name: {'bytes': len(value), 'sha256': sha(value)} for name, value in generated.items()}


def audit_workflow_and_log(workflow, log, run, harness):
    require(run['status'] == 'completed' and run['conclusion'] == 'success', 'CI is not completed successfully')
    require(run['headSha'] == harness, 'CI head differs from harness revision')
    require(run.get('jobs') and all(j['status'] == 'completed' and j['conclusion'] == 'success'
                                  for j in run['jobs']), 'CI job is missing, unfinished or unsuccessful')
    steps = [step for job in run['jobs'] for step in job['steps']]
    for name in ('Validate measurement harness',
                 'Build both exact revisions sequentially with the same toolchain',
                 'Measure five alternating pairs in one native Wayland session',
                 'Upload reports, configurations, logs and build provenance'):
        matches = [s for s in steps if s['name'] == name]
        require(len(matches) == 1 and matches[0]['conclusion'] == 'success', f'CI step not successful: {name}')
    # Fail closed if the pinned workflow changes this provenance contract.
    for marker in ('for label in before after; do',
                   'source_directory="$RUNNER_TEMP/kokuban-paired-source-$label"',
                   'git archive "$revision" | tar -x -C "$source_directory"',
                   'cargo build --release --locked --manifest-path "$source_directory/Cargo.toml"',
                   '--target-dir "$RUNNER_TEMP/kokuban-paired-build-$label"',
                   'cp "$RUNNER_TEMP/kokuban-paired-build-$label/release/kokuban" "$RUNNER_TEMP/kokuban-paired-binaries/$label"',
                   'CARGO_INCREMENTAL: "0"', 'CARGO_PROFILE_RELEASE_DEBUG: "0"'):
        require(marker in workflow, f'Workflow provenance contract changed: {marker}')
    matches = list(re.finditer(r'Compiling kokuban v[^\s]+ \((/[^\n\r)]+/kokuban-paired-source-(before|after))\)', log))
    require([m[2] for m in matches] == ['before', 'after'], 'CI must record distinct sequential before/after root builds')
    require('CARGO_INCREMENTAL: 0' in log and 'CARGO_PROFILE_RELEASE_DEBUG: 0' in log, 'CI build environment missing')
    require('--target-dir "$RUNNER_TEMP/kokuban-paired-build-$label"' in log and 'git archive "$revision"' in log,
            'CI log lacks separate targets/archive command')
    tests = re.findall(r'Ran (\d+) tests in ', log)
    require(len(tests) == 1 and int(tests[0]) > 0, 'CI harness test result missing or ambiguous')
    return {'source_directories_recorded_ci': {m[2]: m[1] for m in matches},
            'separate_targets_verified_in_workflow_and_ci_command': True,
            'python_harness_tests_passed': int(tests[0]), 'rust_correctness_tests_run_by_paired_workflow': False}


def original_paths():
    return (set(ROOT_FILES) | {'measurements/report.json'}
            | {f'{side}-build/{name}' for side in SIDES for name in BUILD_FILES}
            | {f'measurements/{pair:02d}-{side}/{name}' for side in SIDES
               for pair in range(1, 6) for name in PROCESS_FILES})


def audit(args):
    source = args.source_dir.resolve(strict=True)
    output = args.output_dir.resolve()
    repo = args.repo.resolve(strict=True)
    require(source.is_dir(), '--source-dir is not a directory')
    require(not output.exists(), '--output-dir must be new')
    require(source not in output.parents and output not in source.parents, 'Input and output directories overlap')
    before, after, harness = (revision(repo, value) for value in (args.before, args.after, args.harness))
    require(before != after, 'Before/after source revisions must differ')
    revisions = {'before': before, 'after': after}
    ci_path = (args.ci_run or source / 'ci-run.json').resolve(strict=True)
    log_path = (args.ci_log or source / 'ci.log').resolve(strict=True)
    ci_bytes, log_bytes = ci_path.read_bytes(), log_path.read_bytes()
    ci, log = parse_json(ci_bytes), log_bytes.decode('utf-8')
    require(ci['databaseId'] == args.run, 'CI metadata run ID mismatch')
    require(ci['url'].startswith('https://github.com/') and ci['url'].endswith(f'/actions/runs/{args.run}'), 'CI run URL mismatch')
    names = original_paths()
    annotations = {ci_path, log_path}
    if args.artifact_zip:
        annotations.add(args.artifact_zip.resolve(strict=True))
        annotations.add(args.artifact_metadata.resolve(strict=True))
    found = set()
    for path in source.rglob('*'):
        require(not path.is_symlink(), f'Symlink in source directory: {path}')
        if path.is_file() and path.resolve() not in annotations:
            found.add(str(path.relative_to(source)))
    require(found == names, f'Artifact layout mismatch; missing={sorted(names-found)}, extra={sorted(found-names)}')
    original = {name: (source / name).read_bytes() for name in sorted(names)}
    inventory = {name: {'bytes': len(data), 'sha256': sha(data)} for name, data in original.items()}
    report = parse_json(original['measurements/report.json'])
    pinned, snapshots = {}, {}
    for path in [*SCRIPT_FIELDS, WORKFLOW]:
        data = git(repo, 'show', harness + ':' + path)
        blob = git(repo, 'rev-parse', harness + ':' + path).decode().strip()
        snapshots[path] = data
        pinned[path] = {'revision': harness, 'git_blob': blob, 'sha256': sha(data),
                        'bytes': len(data), 'snapshot': 'harness/' + path}
        field = SCRIPT_FIELDS.get(path)
        if field:
            require(report[field] == sha(data), f'Observed harness hash mismatch: {path}')
    build_provenance = audit_workflow_and_log(snapshots[WORKFLOW].decode(), log, ci, harness)
    for name, expected in [('BEFORE_REF', before), ('AFTER_REF', after), ('SCREEN', args.profile)]:
        observed = set(re.findall(r'\b' + name + r': ([^\s\r\n]+)', log))
        require(observed == {expected}, f'CI {name} must record the exact requested value, found {sorted(observed)}')
    for side, rev in {**revisions, 'harness': harness}.items():
        require(original[side + '-revision.txt'].decode().strip() == rev, f'Artifact {side} revision mismatch')
    require(report['status'] == 'passed' and report['in_progress'] is None, 'Incomplete or failing report')
    require(report['screen'] == args.profile and report['backend'] == 'wayland', 'Screen/backend mismatch')
    require(report['pairs_requested'] == 5 and report['bytes_requested_per_workload'] == 33554432,
            'Expected five pairs with 32 MiB requested per workload')
    require(report['requested_geometry'] == [24, 80] and report['font_pixels'] == 14.0, 'Requested geometry/font changed')
    require(report['settle_seconds'] == 1.0, 'Expected one-second stabilization')
    require(report['binaries_identical'] is False and report['allow_identical_binaries'] is False
            and report['comparison_mode'] == 'revision-comparison', 'Not a distinct-binary revision comparison')
    require(report['rendering_equivalence_verified'] is False and report['source_refs_verified_by_runner'] is False,
            'Runner scope changed; manual review required')
    require(report['comparability']['paired_inputs_validated']
            and report['comparability']['geometry_and_history_checks_passed']
            and not report['comparability']['reasons'], 'Reported comparability failed')
    expected_order = [{'pair': pair, 'side': side} for pair in range(1, 6)
                      for side in (SIDES if pair % 2 else SIDES[::-1])]
    require(report['execution_order'] == expected_order, 'Pair order mismatch')
    require(report['environment']['DISPLAY'] is None and report['environment']['WAYLAND_DISPLAY'], 'Not native Wayland')
    require(isinstance(report['cpu_affinity'], list) and len(report['cpu_affinity']) == 1
            and type(report['cpu_affinity'][0]) is int and 0 <= report['cpu_affinity'][0] < report['cpu_count'],
            'Expected one observed harness CPU')
    require(report['hardware']['status'] == 0
            and report['hardware']['output'] == original['cpu.txt'].decode().strip(), 'CPU provenance mismatch')
    require(report['font_match']['status'] == 0 and report['font_match']['output'].startswith('DejaVu Sans Mono\n'), 'Font match mismatch')
    require('Using Pixman renderer' in original['weston.log'].decode(), 'Pixman renderer not observed')
    require(original['rustc.txt'].decode().startswith('rustc 1.94.1 '), 'Compiler version changed')
    payloads = payloads_from_pinned_source(snapshots['scripts/compare-terminal-performance.py'], 33554432)
    require({n: {k: v[k] for k in ('bytes', 'sha256')} for n, v in report['payloads'].items()} == payloads,
            'Payload reproduction mismatch')
    require(set(report['terminals']) == set(SIDES), 'Unexpected terminal sides')
    build_inputs, statistics_by_side, configs, diagnostics = {}, {}, set(), {}
    for side, rev in revisions.items():
        terminal = report['terminals'][side]
        require(terminal['source_ref'] == rev and len(terminal['samples']) == 5, f'{side}: revision/sample count mismatch')
        require(terminal['version_observation']['status'] == 0
                and terminal['version_observation']['output'] == terminal['version'], f'{side}: version not observed')
        require(re.fullmatch(r'[0-9a-f]{64}', terminal['sha256']), f'{side}: invalid executable SHA-256')
        record = original[f'{side}-build/binary-sha256.txt'].decode().strip().split(maxsplit=1)
        require(len(record) == 2 and record[0] == terminal['sha256'] and record[1] == terminal['path'], f'{side}: executable hash/path mismatch')
        inputs = {}
        for filename in ('Cargo.toml', 'Cargo.lock'):
            data = original[f'{side}-build/{filename}']
            require(data == git(repo, 'show', rev + ':' + filename), f'{side}: {filename} differs from exact Git revision')
            inputs[filename] = {'sha256': sha(data), 'bytes': len(data), 'snapshot': f'artifact/{side}-build/{filename}'}
        tree = original[f'{side}-build/cargo-tree.txt'].decode()
        require(tree.splitlines()[0].endswith('(' + build_provenance['source_directories_recorded_ci'][side] + ')'), f'{side}: Cargo tree build root mismatch')
        build_inputs[side] = {'revision': rev, 'cargo_inputs': inputs, 'binary_sha256_observed_ci': record[0],
                             'cargo_tree_sha256': sha(original[f'{side}-build/cargo-tree.txt'])}
        rtts = []
        for pair, sample in enumerate(terminal['samples'], 1):
            prefix = f'measurements/{pair:02d}-{side}/'
            standalone = parse_json(original[prefix + 'sample.json'])
            require(sample == {**standalone, 'pair': pair} and sample['status'] == 'passed', f'{prefix} sample mismatch/status')
            result = parse_json(original[prefix + 'result.json'])
            case = parse_json(original[prefix + 'case.json'])
            child = parse_json(original[prefix + 'child.json'])
            require(sample['measurements'] == result, f'{prefix} result mismatch')
            command = sample['command']
            require(len(command) == 6 and command[0] == terminal['path'] and command[1] == '-e'
                    and command[2] == '/usr/bin/python3'
                    and command[3].endswith('/scripts/compare-terminal-performance.py')
                    and command[4] == '--child-dir' and command[5].endswith('/' + prefix.rstrip('/')), f'{prefix} command mismatch')
            require(case['screen'] == args.profile and case['settle_seconds'] == 1.0, f'{prefix} case screen/settle mismatch')
            require(case['payloads'] == {n: v['path'] for n, v in report['payloads'].items()}, f'{prefix} payload paths mismatch')
            require(type(child['pid']) is int and child['pid'] > 0 and type(case['terminal_pid']) is int
                    and case['terminal_pid'] > 0 and child['pid'] != case['terminal_pid'], f'{prefix} process metadata mismatch')
            require(original[prefix + 'finish'] == b'', f'{prefix} completion marker mismatch')
            config = sample['config']
            require(original[prefix + 'kokuban.toml'] == config['text'].encode()
                    and sha(original[prefix + 'kokuban.toml']) == config['sha256'], f'{prefix} config bytes/hash mismatch')
            history = 0 if args.profile == 'alternate' else 10000
            require(config['history_limit'] == history and config['history_unit'] == 'lines'
                    and f'scrollback_lines = {history}\n' in config['text'], f'{prefix} history mismatch')
            parsed_config = tomllib.loads(config['text'])
            require(parsed_config['font']['family'] == 'DejaVu Sans Mono' and parsed_config['font']['size'] == 14.0,
                    f'{prefix} effective configured font mismatch')
            require(parsed_config['window']['columns'] == 80 and parsed_config['window']['rows'] == 24
                    and parsed_config['window']['scrollback_lines'] == history
                    and parsed_config['images']['enabled'] is False, f'{prefix} effective configured window/images mismatch')
            configs.add(config['sha256'])
            require(result['initial_geometry'] == GEOMETRY and len(result['protocol_rtt_seconds']) == 30, f'{prefix} geometry/RTT count mismatch')
            for value in result['protocol_rtt_seconds']:
                finite(value, prefix + 'RTT', positive=True)
            rtts.extend(result['protocol_rtt_seconds'])
            require(set(result['workloads']) == set(payloads), f'{prefix} incomplete workloads')
            for name, item in result['workloads'].items():
                require({k: item[k] for k in ('bytes', 'sha256')} == payloads[name], f'{prefix}{name} payload mismatch')
                require(item['geometry_before'] == item['geometry_after'] == GEOMETRY, f'{prefix}{name} geometry changed')
                require(item['reply_hex'] == '1b5b313b3552', f'{prefix}{name} final DSR reply mismatch')
                for metric in ('write_seconds', 'drain_rtt_seconds', 'write_and_dsr_seconds', 'mib_per_second'):
                    finite(item[metric], prefix + name + '/' + metric, positive=True)
                finite(item['terminal_cpu_seconds'], prefix + name + '/CPU')
                require(math.isclose(item['write_seconds'] + item['drain_rtt_seconds'], item['write_and_dsr_seconds'], rel_tol=1e-12), f'{prefix}{name} elapsed arithmetic mismatch')
                require(math.isclose(item['bytes'] / 1048576 / item['write_and_dsr_seconds'], item['mib_per_second'], rel_tol=1e-12), f'{prefix}{name} throughput arithmetic mismatch')
            lines = original[prefix + 'terminal.log'].decode(errors='replace').splitlines()
            diagnostics[prefix + 'terminal.log'] = [line for line in lines if re.search(r'\b(?:WARN(?:ING)?|ERROR|panic|failed|timeout)\b', line, re.I)]
        require(distribution(rtts) == terminal['protocol_rtt_seconds'], f'{side}: RTT summary mismatch')
        statistics_by_side[side] = {}
        for name in payloads:
            statistics_by_side[side][name] = {}
            for metric in ('write_and_dsr_seconds', 'mib_per_second', 'terminal_cpu_seconds'):
                values = [s['measurements']['workloads'][name][metric] for s in terminal['samples']]
                calculated = distribution(values)
                require(calculated == terminal['summary'][name][metric], f'{side}/{name}/{metric}: summary mismatch')
                statistics_by_side[side][name][metric] = calculated
    require(len(configs) == 1, 'Configuration differs across sides/samples')
    require(build_inputs['before']['binary_sha256_observed_ci'] != build_inputs['after']['binary_sha256_observed_ci'], 'Binary hashes are identical')
    throughput = {}
    for name in payloads:
        b, a = (statistics_by_side[s][name]['mib_per_second'] for s in SIDES)
        ratios = [x / y for x, y in zip(a['samples'], b['samples'])]
        elapsed_ratios = [x / y for x, y in zip(statistics_by_side['after'][name]['write_and_dsr_seconds']['samples'],
                                              statistics_by_side['before'][name]['write_and_dsr_seconds']['samples'])]
        claimed = report['paired_summary'][name]
        for key, values in [('after_over_before_throughput_ratio', ratios),
                            ('after_over_before_elapsed_ratio', elapsed_ratios),
                            ('elapsed_change_percent', [(v - 1) * 100 for v in elapsed_ratios])]:
            require(distribution(values) == claimed[key], f'{name}/{key}: paired summary mismatch')
        throughput[name] = {'before_mib_per_second': b, 'after_mib_per_second': a,
            'ratio_of_medians_change_percent': (a['median'] / b['median'] - 1) * 100,
            'paired_throughput_change_percent': distribution([(v - 1) * 100 for v in ratios]),
            'slower_after_pairs': sum(v < 1 for v in ratios),
            'min_max_ranges_overlap': max(b['min'], a['min']) <= min(b['max'], a['max'])}
    archive_audit = {'archive_bytes_supplied': False, 'archive_digest_independently_verified': False}
    if args.artifact_zip:
        archive_bytes = args.artifact_zip.read_bytes()
        metadata_bytes = args.artifact_metadata.read_bytes()
        metadata = parse_json(metadata_bytes)
        if 'artifacts' in metadata:
            matches = [v for v in metadata['artifacts'] if v['name'] == ARTIFACT_NAME]
            require(len(matches) == 1, 'Artifact API response is ambiguous')
            metadata = matches[0]
        require(metadata['name'] == ARTIFACT_NAME and not metadata['expired'], 'Artifact metadata mismatch/expired')
        require(metadata['digest'] == 'sha256:' + sha(archive_bytes), 'ZIP digest differs from supplied GitHub metadata')
        require(metadata['size_in_bytes'] == len(archive_bytes), 'ZIP byte count differs from supplied metadata')
        if 'workflow_run' in metadata:
            require(metadata['workflow_run']['id'] == args.run and metadata['workflow_run']['head_sha'] == harness, 'ZIP belongs to another run/head')
        with zipfile.ZipFile(args.artifact_zip) as archive:
            members = [n for n in archive.namelist() if not n.endswith('/')]
            require(len(members) == len(names) and set(members) == names, 'ZIP member set differs from extracted artifact')
            require(all(archive.read(name) == original[name] for name in names), 'ZIP bytes differ from extracted artifact')
        archive_audit = {'archive_bytes_supplied': True, 'archive_digest_independently_verified': True,
                         'id': metadata['id'], 'bytes': len(archive_bytes), 'sha256': sha(archive_bytes),
                         'comparison_source': 'Supplied GitHub metadata; this auditor made no network request',
                         'archive_retained': False}
    source_diagnostics = [f'{n}: {line}' for n, line in enumerate(log.splitlines(), 1)
                          if re.search(r'\b(?:WARN(?:ING)?|ERROR|panic|failed|timeout)\b', line, re.I)]
    trees = [original[f'{side}-build/cargo-tree.txt'].decode().replace(
        build_provenance['source_directories_recorded_ci'][side], '<ROOT>') for side in SIDES]
    manifest = {'schema': 1, 'kind': 'audited-paired-mixed-utf8-spans', 'before_revision': before,
        'after_revision': after, 'harness_revision': harness, 'profile': args.profile, 'run_id': args.run,
        'source_files_changed_between_revisions': git(repo, 'diff', '--name-only', '-z', before, after).decode().rstrip('\0').split('\0'),
        'run_url': ci['url'], 'run_conclusion': ci['conclusion'], 'original_files': len(inventory),
        'raw_report_sha256': sha(original['measurements/report.json']), 'raw_report_byte_identical': True,
        'original_inventory': 'artifact-inventory.json', 'harness': pinned, 'builds': build_inputs,
        'build_provenance': build_provenance, 'cargo_lock_equal': original['before-build/Cargo.lock'] == original['after-build/Cargo.lock'],
        'cargo_trees_equal_after_root_path_normalization': trees[0] == trees[1],
        'cpu': report['hardware'], 'cpu_count': report['cpu_count'], 'harness_cpu_affinity': report['cpu_affinity'],
        'environment': report['environment'], 'platform': report['platform'], 'font_match': report['font_match'],
        'font_pixels': report['font_pixels'], 'geometry': GEOMETRY, 'history_limit_lines': history,
        'processes': 10, 'workloads': 40, 'rtt_observations': 300, 'regenerated_payloads': payloads,
        'throughput': throughput, 'statistics_by_side': statistics_by_side,
        'protocol_rtt_seconds': {s: report['terminals'][s]['protocol_rtt_seconds'] for s in SIDES},
        'archive': archive_audit, 'ci_log': {'bytes': len(log_bytes), 'sha256': sha(log_bytes), 'retained_full': True},
        'diagnostics_file': 'diagnostics.json', 'rendering_equivalence_verified': False,
        'auditor_sha256': sha(Path(__file__).read_bytes()),
        'limitations': [
            'Only PTY processing throughput and DSR RTT; no frame presentation or input-to-photon measurement.',
            'DSR validates a final marker, not workload text, grapheme rendering or pixels.',
            'No physical Omarchy/Hyprland/GPU/monitor validation or ranking against other terminals.',
            'Five pairs describe this run; small changes and overlapping ranges do not establish noise or equivalence.',
            'Compare source revisions within this run. Other profiles may use different CPU models and environments.',
            'CI metadata and logs were supplied by the operator; no API refresh was performed by this auditor.',
            'Binary hashes were recorded in CI; executable bytes were not independently rehashed or retained.',
            'Per-child CPU affinity was inherited from the observed harness affinity, not read back separately.',
            'Coarse process CPU counters are not instruction counts.',
            'Paired CI builds revisions and tests the Python harness; source Rust correctness CI is separate.',
            'The raw runner source_refs_verified_by_runner remains false; Git/workflow/build checks are external audit evidence.',
            'Warnings and error diagnostics are preserved in original logs, including successful process runs.']}
    # Only now publish; no output exists if any validation above fails.
    output.mkdir(parents=True, exist_ok=False)
    def write(name, data):
        target = output / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    def save(name, value):
        write(name, (json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + '\n').encode())
    for name, data in original.items():
        write('artifact/' + name, data)
        require((output / 'artifact' / name).read_bytes() == data, 'Retained artifact byte mismatch')
    for name, data in snapshots.items():
        write('harness/' + name, data)
    write('ci-run.json', ci_bytes)
    write('ci.log', log_bytes)
    if args.artifact_zip:
        write('artifact-metadata.json', metadata_bytes)
    save('artifact-inventory.json', inventory)
    save('diagnostics.json', {'terminal_logs': diagnostics, 'ci_log_matching_lines': source_diagnostics,
                              'notes': 'Pattern-based excerpts for review; full original logs retained. Diagnostics are not inferred test failures.'})
    save('manifest.json', manifest)
    files = sorted(p for p in output.rglob('*') if p.is_file())
    write('evidence-sha256.txt', ''.join(sha(p.read_bytes()) + '  ' + str(p.relative_to(output)) + '\n' for p in files).encode())
    return {'passed': True, 'output_directory': str(output), 'profile': args.profile, 'run': args.run,
            'original_files_verified': len(inventory), 'throughput_change_percent':
            {name: value['ratio_of_medians_change_percent'] for name, value in throughput.items()}}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('before', 'after', 'harness'):
        parser.add_argument('--' + name, required=True, help='Exact Git commit or locally resolvable ref')
    parser.add_argument('--run', type=int, required=True, help='Completed paired GitHub run ID')
    parser.add_argument('--profile', choices=('alternate', 'primary'), required=True)
    parser.add_argument('--source-dir', type=Path, required=True, help='Extracted original artifact plus supplied ci-run.json and ci.log')
    parser.add_argument('--output-dir', type=Path, required=True, help='New output directory; never overwrites an existing audit')
    parser.add_argument('--repo', type=Path, default=Path.cwd(), help='Local Git repository/worktree, default current directory')
    parser.add_argument('--ci-run', type=Path, help='Supplied gh run view JSON, defaults to SOURCE/ci-run.json')
    parser.add_argument('--ci-log', type=Path, help='Supplied full gh run view --log output, defaults to SOURCE/ci.log')
    parser.add_argument('--artifact-zip', type=Path, help='Optional downloaded original artifact ZIP')
    parser.add_argument('--artifact-metadata', type=Path, help='Required with ZIP: supplied GitHub artifact API JSON')
    args = parser.parse_args(argv)
    if bool(args.artifact_zip) != bool(args.artifact_metadata):
        parser.error('--artifact-zip and --artifact-metadata must be supplied together')
    if args.run <= 0:
        parser.error('--run must be positive')
    try:
        print(json.dumps(audit(args), indent=2))
    except (AuditError, OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
        print(f'AUDIT FAILED: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
