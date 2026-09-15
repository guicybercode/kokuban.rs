"""Failure-path checks that do not launch terminals or run performance loads."""

from contextlib import redirect_stdout
import hashlib
import io
import json
from pathlib import Path
import runpy
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch


RUNNER = runpy.run_path(str(Path(__file__).with_name("compare-terminal-performance.py")))
EXECUTE = RUNNER["execute_sample"]
GLOBALS = EXECUTE.__globals__


class SampleLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name) / "sample"
        self.args = SimpleNamespace(backend="x11", screen="alternate", settle_seconds=0, timeout=1)
        self.command = patch.dict(EXECUTE.__globals__, {
            "terminal_command": Mock(return_value=(["terminal"], {})),
            "stop_owned_child": Mock(),
        })
        self.command.start()
        self.addCleanup(self.command.stop)

    def execute(self):
        return EXECUTE("kokuban", Path("terminal"), "test", self.directory, {}, self.args)

    def test_case_write_failure_stops_terminal_and_owned_child(self):
        process = Mock(pid=123, returncode=None)
        process.poll.return_value = None
        real_record = RUNNER["record"]

        def fail_case(directory, name, value):
            if name == "case.json":
                raise OSError("injected case write failure")
            real_record(directory, name, value)

        with patch.object(RUNNER["subprocess"], "Popen", return_value=process), \
                patch.dict(EXECUTE.__globals__, {"record": fail_case}):
            sample = self.execute()
        process.terminate.assert_called_once_with()
        process.wait.assert_called_once_with(timeout=3)
        EXECUTE.__globals__["stop_owned_child"].assert_called_once_with(self.directory)
        self.assertEqual(sample["status"], "failed")
        self.assertIn("injected case write failure", sample["error"])
        self.assertEqual(json.loads((self.directory / "sample.json").read_text()), sample)

    def test_launch_failure_is_recorded_as_failed_sample(self):
        with patch.object(RUNNER["subprocess"], "Popen", side_effect=OSError("injected launch failure")):
            sample = self.execute()
        self.assertEqual(sample["status"], "failed")
        self.assertIn("injected launch failure", sample["error"])
        self.assertEqual(json.loads((self.directory / "sample.json").read_text()), sample)

    def test_thread_cpu_flag_is_retained_in_real_driver_case(self):
        self.args.thread_cpu = True
        process = Mock(pid=123, returncode=0)
        process.poll.return_value = 0
        process.wait.return_value = 0
        with patch.object(RUNNER["subprocess"], "Popen", return_value=process), \
                patch.dict(GLOBALS, wait_for=lambda *_args: {"workloads": {}}):
            sample = self.execute()
        self.assertTrue(sample["thread_cpu_enabled"])
        self.assertTrue(json.loads((self.directory / "case.json").read_text())["thread_cpu"])


class ThreadCpuTests(unittest.TestCase):
    @staticmethod
    def stat(tid, name, user=10, system=5, start=100):
        fields = ['S'] + ['0'] * 19
        fields[11], fields[12], fields[19] = str(user), str(system), str(start)
        return f'{tid} ({name}) ' + ' '.join(fields)

    def snapshot(self, rows, at):
        threads = []
        for tid, name, user, start in rows:
            row = RUNNER['parse_thread_stat'](self.stat(tid, name, user=user, start=start), 10, tid)
            threads.append({**row, 'status': 'read', 'read_started_seconds': at, 'read_finished_seconds': at + 0.01})
        return {'pid': 10, 'ticks_per_second': 100, 'enumeration_complete': True,
                'listed_tids': [r['tid'] for r in threads], 'threads': threads}

    def test_parser_preserves_spaces_and_parentheses_in_comm(self):
        parsed = RUNNER['parse_thread_stat'](self.stat(10, 'a (worker) ) x'), 10, 10)
        self.assertEqual(parsed['name'], 'a (worker) ) x')
        self.assertEqual((parsed['cpu_ticks'], parsed['start_ticks']), (15, 100))
        self.assertTrue(parsed['is_main_thread'])
        reader = RUNNER['parse_thread_stat'](self.stat(11, 'terminal-reader'), 10, 11)
        self.assertTrue(reader['is_terminal_reader'])
        self.assertFalse(reader['is_main_thread'])
        for text in ('broken', '11 (x) S', self.stat(11, 'x')):
            with self.subTest(text=text), self.assertRaises(ValueError):
                RUNNER['parse_thread_stat'](text, 10, 10)

    def test_snapshot_keeps_timestamps_and_unreadable_tasks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for tid in (10, 11):
                (root / '10' / 'task' / str(tid)).mkdir(parents=True)
            (root / '10' / 'task' / '10' / 'stat').write_text(self.stat(10, 'main (x)'))
            result = RUNNER['thread_cpu_snapshot'](10, root, ticks_per_second=100)
            self.assertEqual(result['status'], 'partial')
            self.assertEqual(result['listed_tids'], [10, 11])
            self.assertEqual(result['threads'][1]['status'], 'unavailable')
            self.assertIn('FileNotFoundError', result['threads'][1]['error'])
            for row in result['threads']:
                self.assertLessEqual(result['read_started_seconds'], row['read_started_seconds'])
                self.assertLessEqual(row['read_started_seconds'], row['read_finished_seconds'])
                self.assertLessEqual(row['read_finished_seconds'], result['read_finished_seconds'])
            missing = RUNNER['thread_cpu_snapshot'](99, root, ticks_per_second=100)
            self.assertFalse(missing['enumeration_complete'])
            self.assertIn('enumeration_error', missing)

    def test_identity_changes_and_disappeared_threads_have_null_deltas(self):
        before = self.snapshot([(10, 'main', 10, 100), (11, 'terminal-reader', 20, 101),
                                (12, 'worker', 30, 102)], 1)
        after = self.snapshot([(10, 'main', 15, 100), (11, 'replacement', 1, 201),
                               (13, 'new worker', 3, 103)], 2)
        result = RUNNER['thread_cpu_deltas'](before, after)
        rows = {r['tid']: r for r in result['threads']}
        self.assertEqual(rows[10]['cpu_ticks_delta'], 5)
        self.assertEqual(rows[10]['cpu_seconds_delta'], 0.05)
        self.assertAlmostEqual(rows[10]['read_window_seconds']['minimum'], 0.99)
        self.assertEqual(rows[11]['status'], 'identity_changed')
        self.assertEqual(rows[12]['status'], 'disappeared')
        self.assertEqual(rows[13]['status'], 'newly_observed')
        self.assertEqual(result['newly_observed_tids'], [13])
        self.assertEqual(result['disappeared_tids'], [12])
        for tid in (11, 12, 13):
            self.assertIsNone(rows[tid]['cpu_seconds_delta'])

    def test_failed_enumeration_does_not_invent_disappearance_or_zero_cpu(self):
        before = self.snapshot([(10, 'main', 10, 100)], 1)
        after = {'pid': 10, 'ticks_per_second': 100, 'threads': [], 'enumeration_complete': False}
        row = RUNNER['thread_cpu_deltas'](before, after)['threads'][0]
        self.assertEqual(row['status'], 'unavailable')
        self.assertIsNone(row['cpu_seconds_delta'])
        regressed = self.snapshot([(10, 'main', 9, 100)], 2)
        row = RUNNER['thread_cpu_deltas'](before, regressed)['threads'][0]
        self.assertEqual(row['status'], 'counter_regressed')
        self.assertIsNone(row['cpu_seconds_delta'])

    def test_terminal_command_passes_opt_in_only_to_the_pty_driver(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(RUNNER['sys'], 'platform', 'linux'):
            args = SimpleNamespace(screen='primary', scrollback_lines=10000, font_pixels=14,
                                   columns=80, rows=24, thread_cpu=True)
            command, _ = RUNNER['terminal_command']('kokuban', Path('kokuban'), 'test', Path(temporary), args)
            self.assertEqual(command[-1], '--thread-cpu')
            self.assertIn('--child-dir', command)
            args.thread_cpu = False
            command, _ = RUNNER['terminal_command']('kokuban', Path('kokuban'), 'test', Path(temporary), args)
            self.assertNotIn('--thread-cpu', command)

    def test_real_child_keeps_clock_boundaries_and_reads_no_threads_by_default(self):
        for enabled in (False, True):
            with self.subTest(enabled=enabled), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                payload = directory / 'payload'
                payload.write_bytes(b'x\r\n')
                (directory / 'case.json').write_text(json.dumps({'terminal_pid': 10, 'payloads': {'short': str(payload)},
                    'screen': 'primary', 'settle_seconds': 0, 'thread_cpu': enabled}))
                (directory / 'finish').touch()
                events, cpu_count, clock_count = [], [0], [0]

                def cpu(pid):
                    if pid == 10:
                        cpu_count[0] += 1
                        events.append('cpu_before' if cpu_count[0] == 1 else 'cpu_after')
                    return {'pid': pid, 'start_ticks': 100, 'cpu_seconds': cpu_count[0], 'rss_kib': 1}

                def clock():
                    clock_count[0] += 1
                    events.append('clock')
                    return clock_count[0]

                def barrier(marker):
                    events.append(marker.decode())
                    return 'reply'

                def threads(_pid):
                    self.assertTrue(enabled, 'thread collection must be completely disabled by default')
                    label = 'threads_before' if cpu_count[0] == 0 else 'threads_after'
                    events.append(label)
                    return self.snapshot([(10, 'main', cpu_count[0] + 10, 100)], cpu_count[0])

                with patch.dict(GLOBALS, process_stats=cpu, barrier=barrier, write_all=lambda _data: events.append('write'),
                                terminal_size=lambda: [24, 80, 720, 408], wait_for=lambda observe, *_args: observe(),
                                thread_cpu_snapshot=threads), \
                     patch.object(RUNNER['time'], 'perf_counter', side_effect=clock), \
                     patch.object(RUNNER['time'], 'monotonic', side_effect=AssertionError('no additional clock reads')), \
                     patch.object(RUNNER['time'], 'sleep'), patch.object(RUNNER['tty'], 'setraw'), \
                     patch.object(RUNNER['termios'], 'tcgetattr', return_value=[]), patch.object(RUNNER['termios'], 'tcsetattr'):
                    RUNNER['controlled_child'](directory, enabled)
                relevant = events[events.index('start') + 1:]
                expected = ['cpu_before', 'clock', 'write', 'clock', 'done', 'clock', 'cpu_after', 'write']
                if enabled:
                    expected.insert(0, 'threads_before')
                    expected.insert(-1, 'threads_after')
                self.assertEqual(relevant, expected)
                result = json.loads((directory / 'result.json').read_text())['workloads']['short']
                self.assertEqual(result['write_and_dsr_seconds'], 2)
                self.assertEqual('thread_cpu' in result, enabled)


class MacosObservationTests(unittest.TestCase):
    def test_ps_cpu_time_formats_and_rejects_garbage(self):
        parse = RUNNER['parse_ps_cpu_time']
        self.assertAlmostEqual(parse('0:01.25'), 1.25)
        self.assertAlmostEqual(parse('1:02:03.50'), 3723.5)
        self.assertAlmostEqual(parse('2-00:00:01.00'), 172801.0)
        stats = RUNNER['parse_ps_stats'](42, '  0:12.34  20480\n')
        self.assertEqual((stats['pid'], stats['rss_kib'], stats['source']), (42, 20480, 'ps'))
        self.assertAlmostEqual(stats['cpu_seconds'], 12.34)
        for text in ('', '12.34 1', '0:01.00'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                RUNNER['parse_ps_stats'](42, text)

    def test_top_idle_wakeups_ignore_headers_other_pids_and_plus_suffix(self):
        text = ('Processes: 1 total\nPID    IDLEW\n42     122\n420    9\n'
                'Processes: 1 total\nPID    IDLEW\n42     324+\n')
        self.assertEqual(RUNNER['parse_top_idle_wakeups'](text, 42), [122, 324])

    def test_idle_observation_reads_top_only_on_macos(self):
        stats = iter([{'cpu_seconds': 1.0}, {'cpu_seconds': 1.5}])
        completed = Mock(returncode=0, stdout='42 10\n42 30+\n', stderr='')
        with patch.dict(GLOBALS, process_stats=lambda _pid: next(stats)), \
                patch.object(RUNNER['sys'], 'platform', 'darwin'), \
                patch.object(RUNNER['subprocess'], 'run', return_value=completed) as run:
            idle = RUNNER['observe_idle'](42, 10)
        self.assertEqual(run.call_args.args[0][:5], ['top', '-l', '2', '-s', '10'])
        self.assertEqual((idle['idle_wakeups'], idle['idle_wakeups_per_second']), (20, 2.0))
        self.assertAlmostEqual(idle['terminal_cpu_seconds'], 0.5)
        stats = iter([{'cpu_seconds': 1.0}, {'cpu_seconds': 1.0}])
        with patch.dict(GLOBALS, process_stats=lambda _pid: next(stats)), \
                patch.object(RUNNER['sys'], 'platform', 'linux'), patch.object(RUNNER['time'], 'sleep') as sleep, \
                patch.object(RUNNER['subprocess'], 'run', side_effect=AssertionError('top is macOS only')):
            idle = RUNNER['observe_idle'](42, 3)
        sleep.assert_called_once_with(3)
        self.assertIsNone(idle['idle_wakeups'])
        self.assertEqual(idle['idle_wakeups_observation']['status'], 'unavailable')

    def test_macos_kokuban_launches_the_driver_through_an_executable_shell(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(RUNNER['sys'], 'platform', 'darwin'):
            directory = Path(temporary)
            args = SimpleNamespace(screen='alternate', scrollback_lines=10000, font_pixels=14,
                                   columns=80, rows=24, thread_cpu=False)
            command, config = RUNNER['terminal_command']('kokuban', Path('/bin/kokuban'), 'test', directory, args)
            self.assertEqual(command, ['/bin/kokuban'])
            shell = Path(config['launch_shell'])
            self.assertTrue(shell.stat().st_mode & 0o111)
            script = shell.read_text()
            self.assertTrue(script.startswith('#!/bin/sh\nexec '))
            self.assertIn('--child-dir', script)
            self.assertIn(str(directory), script)
            process = Mock(pid=123, returncode=0)
            process.poll.return_value = 0
            process.wait.return_value = 0
            args.backend, args.settle_seconds, args.timeout, args.idle_seconds = 'wayland', 0, 1, 7
            sample_directory = directory / 'sample'
            with patch.object(RUNNER['subprocess'], 'Popen', return_value=process) as popen, \
                    patch.dict(GLOBALS, wait_for=lambda *_args: {'workloads': {}}, stop_owned_child=Mock()):
                RUNNER['execute_sample']('kokuban', Path('/bin/kokuban'), 'test', sample_directory, {}, args)
            environment = popen.call_args.kwargs['env']
            self.assertEqual(environment['KOKUBAN_SHELL'], str(sample_directory / 'kokuban-shell.sh'))
            self.assertEqual(json.loads((sample_directory / 'case.json').read_text())['idle_seconds'], 7)


class CellCalibrationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.output = self.directory / "report"
        self.argv = ["--output-dir", str(self.output), "--samples", "3", "--bytes", "1024",
                     "--backend", "wayland"]
        self.versions = {"kokuban": "kokuban 0.1.0", "ghostty": "Ghostty 1.3.0-dev+0000000",
                         "alacritty": "alacritty 0.16.1", "kitty": "kitty 0.45.0 created by Kovid Goyal"}
        self.natural = {"kokuban": (9, 17), "ghostty": (8, 16), "alacritty": (8, 17), "kitty": (8, 17)}
        self.calls = []
        for name in RUNNER["TERMINALS"]:
            binary = self.directory / name
            binary.write_bytes(name.encode())
            binary.chmod(0o700)
            self.argv += ["--" + name, str(binary)]

    def sample(self, name, binary, version, directory, paths, args):
        directory.mkdir()
        command, config = RUNNER["terminal_command"](name, binary, version, directory, args)
        self.calls.append((directory.name, name))
        offsets = getattr(args, "cell_adjustments", {}).get(name, (0, 0))
        width, height = [self.natural[name][axis] + offsets[axis] for axis in (0, 1)]
        geometry = [args.rows, args.columns, args.columns * width, args.rows * height]
        # Very different preflight timing makes accidental inclusion observable.
        elapsed = 9999 if directory.name.startswith("preflight-") else 0.5
        workloads = {}
        for workload, path in paths.items():
            payload = Path(path).read_bytes()
            workloads[workload] = {
                "bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest(),
                "geometry_before": geometry, "geometry_after": geometry,
                "write_and_dsr_seconds": elapsed, "mib_per_second": len(payload) / 1048576 / elapsed,
                "terminal_cpu_seconds": 0,
            }
        return {"status": "passed", "command": command, "config": config,
                "measurements": {"initial_geometry": geometry, "workloads": workloads,
                                 "protocol_rtt_seconds": [0.001]}}

    def run_comparison(self, options=(), sample=None):
        def observation(command):
            value = self.versions[Path(command[0]).name] if command[-1] == "--version" else "observed"
            return {"command": command, "status": 0, "output": value}

        with patch.dict(GLOBALS, execute_sample=sample or self.sample, command_observation=observation), \
                patch.object(GLOBALS["sys"], "platform", "linux"), \
                patch.object(GLOBALS["os"], "sched_getaffinity", return_value={0}, create=True), \
                patch.dict(GLOBALS["os"].environ, {"WAYLAND_DISPLAY": "mock-wayland"}), \
                redirect_stdout(io.StringIO()):
            status = RUNNER["main"]([*self.argv, *options])
        return status, json.loads((self.output / "report.json").read_text())

    def test_calibration_selects_maximum_cells_and_excludes_both_preflights_from_statistics(self):
        status, report = self.run_comparison(["--match-cell-size"])
        self.assertEqual(status, 0)
        calibration = report["cell_size_calibration"]
        self.assertEqual(calibration["target_cell_pixels"], [9, 17])
        self.assertEqual(calibration["spacing_adjustments_pixels"], {
            "kokuban": [0, 0], "ghostty": [1, 1], "alacritty": [1, 0], "kitty": [1, 0]})
        self.assertEqual(calibration["status"], "passed")
        self.assertFalse(calibration["included_in_timed_statistics"])
        self.assertFalse(report["comparability"]["rendering_equivalence_verified"])
        self.assertEqual(len(self.calls), 8 + 12)
        self.assertEqual(len(report["execution_order"]), 12)
        for name, terminal in report["terminals"].items():
            self.assertEqual(len(terminal["samples"]), 3)
            summary = terminal["summary"]["ascii"]["write_and_dsr_seconds"]
            self.assertEqual((summary["count"], summary["median"]), (3, 0.5))
            for phase in ("baseline", "adjusted"):
                config = calibration["preflight"][phase][name]["config"]
                path = self.output / f"preflight-{phase}-{name}" / config["filename"]
                self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), config["sha256"])
        self.assertEqual(report["comparability"]["geometries_rows_cols_pixels"], [[24, 80, 720, 408]])

    def test_offsets_use_each_terminal_syntax_without_changing_font_size(self):
        status, report = self.run_comparison(["--match-cell-size"])
        self.assertEqual(status, 0)
        configs = {name: terminal["samples"][0] for name, terminal in report["terminals"].items()}
        self.assertIn('size = 14.0\n', configs["kokuban"]["config"]["text"])
        self.assertIn('font-size = 10.5\n', configs["ghostty"]["config"]["text"])
        self.assertIn('adjust-cell-width = 1\nadjust-cell-height = 1\n', configs["ghostty"]["config"]["text"])
        self.assertIn('[font.offset]\nx = 1\ny = 0\n', configs["alacritty"]["config"]["text"])
        self.assertIn('size = 10.5\n', configs["alacritty"]["config"]["text"])
        self.assertIn('font_size 10.5\n', configs["kitty"]["config"]["text"])
        command = configs["kitty"]["command"]
        self.assertIn("modify_font=cell_width 1px", command)
        self.assertIn("modify_font=cell_height 0px", command)
        self.assertEqual(command[command.index("modify_font=cell_width 1px") - 1], "--override")

    def test_legacy_alacritty_uses_yaml_font_offset(self):
        self.versions["alacritty"] = "alacritty 0.11.0"
        status, report = self.run_comparison(["--match-cell-size"])
        self.assertEqual(status, 0)
        config = report["terminals"]["alacritty"]["samples"][0]["config"]
        self.assertEqual(config["filename"], "alacritty.yml")
        self.assertTrue(config["text"].startswith('font:\n  offset:\n    x: 1\n    y: 0\n  normal:\n'))

    def test_without_opt_in_keeps_configs_but_returns_failure_for_different_geometry(self):
        status, report = self.run_comparison()
        self.assertEqual(status, 1)
        self.assertNotIn("cell_size_calibration", report)
        self.assertEqual(len(self.calls), 12)
        self.assertFalse(report["comparability"]["geometry_and_history_checks_passed"])
        for terminal in report["terminals"].values():
            self.assertNotRegex(terminal["samples"][0]["config"]["text"], r"adjust-cell|font.offset|modify_font")

    def test_ignored_spacing_stops_before_timed_samples_and_preserves_evidence(self):
        def ignore_ghostty_offset(name, binary, version, directory, paths, args):
            sample = self.sample(name, binary, version, directory, paths, args)
            if directory.name == "preflight-adjusted-ghostty":
                geometry = [24, 80, 640, 384]
                sample["measurements"]["initial_geometry"] = geometry
                sample["measurements"]["workloads"]["geometry_probe"].update(
                    geometry_before=geometry, geometry_after=geometry)
            return sample

        status, report = self.run_comparison(["--match-cell-size"], ignore_ghostty_offset)
        self.assertEqual(status, 1)
        self.assertIn("did not reach", report["error"])
        self.assertEqual(report["cell_size_calibration"]["status"], "failed")
        self.assertIn("ghostty", report["cell_size_calibration"]["preflight"]["adjusted"])
        self.assertEqual(report["execution_order"], [])
        self.assertTrue(all(not terminal["samples"] for terminal in report["terminals"].values()))

    def test_invalid_or_noninteger_geometry_is_rejected(self):
        args = SimpleNamespace(rows=24, columns=80)
        for geometry in ([24, 80, 721, 408], [24, 80, 720, 409], [24, 80, 0, 408],
                         [24, 80, 720.0, 408], [24, 81, 729, 408], [24, 80, 720], None):
            with self.subTest(geometry=geometry), self.assertRaises(ValueError):
                RUNNER["cell_dimensions"](geometry, args)

    def test_noninteger_preflight_retains_observation_and_never_starts_measurements(self):
        def invalid_geometry(name, binary, version, directory, paths, args):
            sample = self.sample(name, binary, version, directory, paths, args)
            geometry = [24, 80, 721, 408]
            sample["measurements"]["initial_geometry"] = geometry
            sample["measurements"]["workloads"]["geometry_probe"].update(
                geometry_before=geometry, geometry_after=geometry)
            return sample

        status, report = self.run_comparison(["--match-cell-size"], invalid_geometry)
        self.assertEqual(status, 1)
        self.assertIn("integer cell dimensions", report["error"])
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(report["execution_order"], [])
        self.assertIn("kokuban", report["cell_size_calibration"]["preflight"]["baseline"])

    def test_unknown_version_fails_before_launching(self):
        self.versions["ghostty"] = "custom unversioned build"
        status, report = self.run_comparison(["--match-cell-size"])
        self.assertEqual(status, 1)
        self.assertIn("unsupported or unknown version", report["error"])
        self.assertEqual(self.calls, [])

    def test_larger_competitor_cells_cannot_silently_change_kokuban_font(self):
        self.natural["kitty"] = (10, 18)
        status, report = self.run_comparison(["--match-cell-size"])
        self.assertEqual(status, 1)
        self.assertEqual(report["cell_size_calibration"]["target_cell_pixels"], [10, 18])
        self.assertIn("unsupported Kokuban spacing", report["error"])
        self.assertEqual(len(self.calls), 4)

    def test_measured_geometry_drift_after_successful_calibration_still_fails(self):
        def drift(name, binary, version, directory, paths, args):
            sample = self.sample(name, binary, version, directory, paths, args)
            if not directory.name.startswith("preflight-"):
                for workload in sample["measurements"]["workloads"].values():
                    workload.update(geometry_before=[24, 80, 800, 432], geometry_after=[24, 80, 800, 432])
            return sample

        status, report = self.run_comparison(["--match-cell-size"], drift)
        self.assertEqual(status, 1)
        self.assertEqual(report["cell_size_calibration"]["status"], "passed")
        self.assertTrue(any("calibrated target" in reason for reason in report["comparability"]["reasons"]))


if __name__ == "__main__":
    unittest.main()
