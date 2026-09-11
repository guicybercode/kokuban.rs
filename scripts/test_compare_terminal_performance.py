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
