"""Regression tests for paired reports; no display, PTY, or benchmark is run."""

from contextlib import redirect_stderr, redirect_stdout
import hashlib
import io
import json
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch


RUNNER = runpy.run_path(str(Path(__file__).with_name("compare-kokuban-revisions.py")))
GLOBALS = RUNNER["compare"].__globals__


class PairedRevisionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="kokuban-paired-test-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.binaries = {}
        for side in ("before", "after"):
            path = self.directory / (side + " binary")
            path.write_bytes(side.encode())
            path.chmod(0o700)
            self.binaries[side] = path.resolve()
        self.argv = ["--before", str(self.binaries["before"]), "--after", str(self.binaries["after"]),
                     "--before-ref", "refs/heads/before", "--after-ref", "after-revision",
                     "--artifacts-dir", str(self.directory / "artifacts")]
        self.args = RUNNER["parse_args"]([*self.argv, "--samples", "3", "--bytes", "1024"])
        self.calls = []

    def sample(self, name, binary, version, directory, paths, args):
        self.assertEqual(name, "kokuban")
        side = "before" if binary == self.binaries["before"] else "after"
        self.calls.append((directory.name, side))
        directory.mkdir()
        elapsed = 2.0 if side == "before" else 1.0
        measured = {}
        for workload, path in paths.items():
            payload = Path(path).read_bytes()
            measured[workload] = {
                "bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest(),
                "geometry_before": [24, 80, 720, 408], "geometry_after": [24, 80, 720, 408],
                "write_and_dsr_seconds": elapsed, "mib_per_second": len(payload) / 1048576 / elapsed,
                "terminal_cpu_seconds": 0.0,
            }
        return {"status": "passed", "config": {"sha256": "identical-config", "history_limit": 0,
                                                 "history_unit": "lines"},
                "measurements": {"workloads": measured, "protocol_rtt_seconds": [0.001, 0.002]}}

    def run_comparison(self, execute=None, observation=None):
        observation = observation or (lambda command: {"command": command, "status": 0, "output": "observed"})
        with patch.dict(GLOBALS, execute_sample=execute or self.sample, command_observation=observation), \
                patch.object(GLOBALS["os"], "sched_getaffinity", return_value={0, 2}, create=True), \
                redirect_stdout(io.StringIO()):
            status = RUNNER["compare"](self.args)
        report = json.loads((self.args.artifacts_dir / "report.json").read_text())
        return status, report

    def test_defaults_and_invalid_arguments(self):
        args = RUNNER["parse_args"](self.argv)
        self.assertEqual((args.samples, args.bytes, args.timeout, args.settle_seconds),
                         (5, 32 * 1024 * 1024, 120, 1))
        self.assertEqual((args.backend, args.screen, args.columns, args.rows, args.font_pixels,
                          args.scrollback_lines), ("wayland", "alternate", 80, 24, 14, 10000))
        self.assertEqual(args.before, self.binaries["before"])
        for options in (["--samples", "2"], ["--bytes", "10"], ["--columns", "0"],
                        ["--font-pixels", "nan"], ["--timeout", "inf"], ["--settle-seconds", "-1"],
                        ["--scrollback-lines", "-1"], ["--before-ref", " "]):
            with self.subTest(options=options), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                RUNNER["parse_args"]([*self.argv, *options])

    def test_alternates_pairs_and_reports_matched_ratios_and_provenance(self):
        status, report = self.run_comparison()
        self.assertEqual(status, 0)
        self.assertEqual(self.calls, [("01-before", "before"), ("01-after", "after"),
                                     ("02-after", "after"), ("02-before", "before"),
                                     ("03-before", "before"), ("03-after", "after")])
        self.assertEqual(report["status"], "passed")
        self.assertEqual(report["cpu_affinity"], [0, 2])
        self.assertFalse(report["rendering_equivalence_verified"])
        self.assertFalse(report["source_refs_verified_by_runner"])
        self.assertIsNone(report["comparability"]["ranking"])
        self.assertTrue(report["comparability"]["paired_inputs_validated"])
        for side in ("before", "after"):
            terminal = report["terminals"][side]
            self.assertEqual(terminal["sha256"], hashlib.sha256(side.encode()).hexdigest())
            self.assertEqual([sample["pair"] for sample in terminal["samples"]], [1, 2, 3])
        self.assertEqual(report["terminals"]["before"]["source_ref"], "refs/heads/before")
        for key in ("script_sha256", "comparator_sha256", "pty_driver_helpers_sha256"):
            self.assertEqual(len(report[key]), 64)
        for summary in report["paired_summary"].values():
            self.assertEqual(summary["after_over_before_elapsed_ratio"]["median"], 0.5)
            self.assertEqual(summary["after_over_before_throughput_ratio"]["median"], 2.0)
            self.assertEqual(summary["elapsed_change_percent"]["samples"], [-50.0] * 3)

    def test_launch_exception_retains_checkpoint_and_continues_other_pairs(self):
        def execute(*arguments):
            if arguments[3].name == "01-after":
                checkpoint = json.loads((self.args.artifacts_dir / "report.json").read_text())
                self.assertEqual(len(checkpoint["terminals"]["before"]["samples"]), 1)
                self.assertEqual(checkpoint["in_progress"], {"pair": 1, "side": "after"})
                raise OSError("injected launch failure")
            return self.sample(*arguments)
        status, report = self.run_comparison(execute)
        self.assertEqual(status, 1)
        self.assertEqual(report["status"], "failed")
        self.assertEqual(len(report["execution_order"]), 6)
        self.assertIn("injected launch failure", report["terminals"]["after"]["samples"][0]["error"])
        self.assertIsNone(report["paired_summary"])
        self.assertFalse(report["comparability"]["paired_inputs_validated"])

    def test_interrupt_preserves_completed_sample_and_stops_scheduling(self):
        def execute(*arguments):
            if arguments[3].name == "01-after":
                raise KeyboardInterrupt
            return self.sample(*arguments)
        status, report = self.run_comparison(execute)
        self.assertEqual(status, 130)
        self.assertEqual(report["status"], "interrupted")
        self.assertEqual(len(report["execution_order"]), 2)
        self.assertEqual(report["terminals"]["before"]["samples"][0]["status"], "passed")
        self.assertEqual(report["terminals"]["after"]["samples"], [{"pair": 1, "status": "interrupted"}])
        self.assertIsNone(report["paired_summary"])

    def test_incomparable_geometry_suppresses_ratios_and_fails(self):
        def execute(*arguments):
            sample = self.sample(*arguments)
            if arguments[1] == self.binaries["after"]:
                sample["measurements"]["workloads"]["ascii"]["geometry_after"] = [24, 81, 729, 408]
            return sample
        status, report = self.run_comparison(execute)
        self.assertEqual(status, 1)
        self.assertIsNone(report["paired_summary"])
        self.assertTrue(any("geometry" in reason for reason in report["comparability"]["reasons"]))

    def test_payload_mismatch_suppresses_ratios(self):
        def execute(*arguments):
            sample = self.sample(*arguments)
            sample["measurements"]["workloads"]["ascii"]["sha256"] = "wrong-payload"
            return sample
        status, report = self.run_comparison(execute)
        self.assertEqual(status, 1)
        self.assertIsNone(report["paired_summary"])

    def test_failed_version_stores_binary_metadata_without_launching(self):
        def observation(command):
            return {"command": command, "status": 1 if command[-1] == "--version" else 0, "output": "failure"}
        status, report = self.run_comparison(observation=observation)
        self.assertEqual(status, 1)
        self.assertFalse(self.calls)
        self.assertIn("version", report["error"])
        self.assertIn("sha256", report["terminals"]["before"])

    def test_identical_binaries_are_rejected_before_generating_workloads(self):
        self.binaries["after"].write_bytes(self.binaries["before"].read_bytes())
        with patch.dict(GLOBALS, payloads=lambda _count: self.fail("workloads must not be generated")):
            status, report = self.run_comparison()
        self.assertEqual(status, 1)
        self.assertEqual(report["status"], "failed")
        self.assertTrue(report["binaries_identical"])
        self.assertFalse(report["allow_identical_binaries"])
        self.assertIn("--allow-identical-binaries", report["error"])
        self.assertFalse(self.calls)
        self.assertEqual(report["payloads"], {})
        self.assertIsNone(report["paired_summary"])
        self.assertFalse(report["comparability"]["paired_inputs_validated"])

    def test_identical_binaries_can_run_only_as_explicit_variability_control(self):
        self.binaries["after"].write_bytes(self.binaries["before"].read_bytes())
        self.args = RUNNER["parse_args"]([*self.argv, "--samples", "3", "--bytes", "1024",
                                         "--allow-identical-binaries"])
        status, report = self.run_comparison()
        self.assertEqual(status, 0)
        self.assertEqual(len(self.calls), 6)
        self.assertTrue(report["binaries_identical"])
        self.assertTrue(report["allow_identical_binaries"])
        self.assertEqual(report["comparison_mode"], "same-executable-variability")
        self.assertIn("Same-executable A/A variability", report["measurement_scope"])
        self.assertIn("no revision speedup inference", report["comparability"]["scope"])
        self.assertFalse(report["rendering_equivalence_verified"])
        self.assertIsNone(report["comparability"]["ranking"])

    def test_existing_artifacts_are_not_overwritten(self):
        self.args.artifacts_dir.mkdir()
        preserved = self.args.artifacts_dir / "report.json"
        preserved.write_text("existing evidence")
        with self.assertRaises(FileExistsError):
            self.run_comparison()
        self.assertEqual(preserved.read_text(), "existing evidence")

    def test_missing_display_does_not_start_or_create_artifacts(self):
        with patch.dict(GLOBALS["os"].environ, {}, clear=True), \
                patch.object(GLOBALS["sys"], "platform", "linux"), redirect_stderr(io.StringIO()):
            self.assertEqual(RUNNER["main"]([*self.argv, "--backend", "x11"]), 2)
            self.assertEqual(RUNNER["main"]([*self.argv, "--backend", "wayland"]), 2)
        self.assertFalse(self.args.artifacts_dir.exists())


if __name__ == "__main__":
    unittest.main()
