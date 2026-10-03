"""Frame runner regression tests; no Rust build, display or timing benchmark."""

from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
import runpy
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

RUNNER = runpy.run_path(str(Path(__file__).with_name("compare-frame-revisions.py")))
GLOBALS = RUNNER["measure"].__globals__
PREFIX = b"// preserved runtime\n#[cfg(test)]\nmod damage_tests {\n"
BENCHMARK = RUNNER["MARKER"].encode() + b"        let fixture = 1;\n    }\n}\n"
FRAME_CHECKSUMS = [RUNNER["frame_checksum"](bytes([index]) * 120 * 40 * 4) for index in (0, 1)]


def sample_output(elapsed=600, steps=300, prefix=True):
    lines = []
    for content in RUNNER["CONTENTS"]:
        for change in RUNNER["CHANGES"]:
            bands = "[(20, 21), (20, 21)]" if change == "single-row" else "[(0, 40), (0, 40)]"
            color = 800 if content == "unicode" else 0
            lines.append(f"frame-repaint fixture content={content} change={change} cols=120 rows=40 "
                         f"width=120 height=40 cell_width=1 cell_height=1 warmup=30 samples=1 steps={steps} "
                         f"mono_cells=1000 color_cells={color} bands={bands} "
                         f"frame0={FRAME_CHECKSUMS[0]} frame1={FRAME_CHECKSUMS[1]}")
            for incremental in ("false", "true"):
                lines.append(f"frame-repaint sample content={content} change={change} sample=0 "
                             f"incremental={incremental} frames={steps} elapsed_ns={elapsed} "
                             f"ns_per_frame={elapsed / steps:.3f}")
    if prefix:
        lines[0] = "test linux_window::damage_tests::benchmark_frame_repaint ... " + lines[0]
    return "\n".join(lines) + "\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n"


class FrameRevisionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="kokuban-frame-runner-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()

    def sources(self):
        roots = {}
        for side in ("harness", "before", "after"):
            root = self.root / side
            (root / "src").mkdir(parents=True)
            (root / "src/linux_window.rs").write_bytes(PREFIX + BENCHMARK)
            (root / "src/software_raster.rs").write_text("common raster\n")
            (root / "Cargo.toml").write_text("fixture manifest\n")
            (root / "Cargo.lock").write_text("fixture lock\n")
            roots[side] = root
        return roots

    def prepared(self, comparison="revisions"):
        roots = self.sources()
        output = self.root / "injection"
        if comparison == "raster-only":
            (roots["after"] / "src/software_raster.rs").write_text("candidate raster\n")
        args = SimpleNamespace(before_source=roots["before"], after_source=roots["after"],
                               harness_source=roots["harness"], output=output, comparison=comparison,
                               common_source=roots["harness"], common_ref="common-revision")
        RUNNER["prepare"](args)
        return roots, output

    def builds(self, comparison="revisions"):
        roots, injection = self.prepared(comparison)
        args = SimpleNamespace(before_source=roots["before"], after_source=roots["after"],
                               injection=injection / "injection.json", output=self.root / "measurements",
                               pairs=3, steps=300, warmup=30, timeout=1, environment_note="synthetic test",
                               comparison=comparison)
        for side in RUNNER["build_sides"](comparison):
            target = self.root / (side + " target")
            (target / "release/deps").mkdir(parents=True)
            binary = target / "release/deps/kokuban-fixture"
            binary.write_bytes(side.encode())
            binary.chmod(0o700)
            messages = self.root / (side + "-cargo.jsonl")
            messages.write_text(json.dumps({"reason": "compiler-artifact", "fresh": False,
                                           "target": {"name": "kokuban", "kind": ["bin"]},
                                           "profile": {"test": True, "opt_level": "3"},
                                           "manifest_path": str(roots[side] / "Cargo.toml"),
                                           "executable": str(binary)}) + "\n")
            for suffix, value in (("target", target), ("messages", messages), ("ref", side + "-revision")):
                setattr(args, side + "_" + suffix, value)
        return args

    def images(self, directory):
        directory.mkdir()
        for content in RUNNER["CONTENTS"]:
            for change in RUNNER["CHANGES"]:
                for index in (0, 1):
                    (directory / f"{content}-{change}-{index}.xrgb8888le").write_bytes(bytes([index]) * 120 * 40 * 4)

    def test_current_harness_layout_is_supported(self):
        source = Path(__file__).resolve().parents[1] / "src/linux_window.rs"
        prefix, benchmark = RUNNER["split_benchmark"](source.read_bytes())
        self.assertEqual(prefix + benchmark, source.read_bytes())
        self.assertIn(b"KOKUBAN_FRAME_OUTPUT_DIR", benchmark)

    def test_injection_preserves_runtime_prefix_and_records_originals(self):
        roots = self.sources()
        original = PREFIX + BENCHMARK.replace(b"fixture = 1", b"fixture = 2")
        (roots["before"] / "src/linux_window.rs").write_bytes(original)
        output = self.root / "injection"
        RUNNER["prepare"](SimpleNamespace(before_source=roots["before"], after_source=roots["after"],
                                           harness_source=roots["harness"], output=output, comparison="revisions"))
        self.assertEqual((output / "before/original-linux_window.rs").read_bytes(), original)
        self.assertEqual((roots["before"] / "src/linux_window.rs").read_bytes(), PREFIX + BENCHMARK)
        self.assertEqual((output / "benchmark.rs.txt").read_bytes(), BENCHMARK)
        report = json.loads((output / "injection.json").read_text())
        self.assertNotEqual(report["sources"]["before"]["original_sha256"],
                            report["sources"]["before"]["injected_sha256"])

    def test_unsupported_layout_is_rejected_before_any_source_is_modified(self):
        roots = self.sources()
        malformed = (PREFIX + BENCHMARK)[:-2] + b"    fn unrelated() {}\n}\n"
        (roots["after"] / "src/linux_window.rs").write_bytes(malformed)
        args = SimpleNamespace(before_source=roots["before"], after_source=roots["after"],
                               harness_source=roots["harness"], output=self.root / "injection", comparison="revisions")
        with self.assertRaisesRegex(ValueError, "final function"):
            RUNNER["prepare"](args)
        self.assertFalse(args.output.exists())
        self.assertEqual((roots["before"] / "src/linux_window.rs").read_bytes(), PREFIX + BENCHMARK)
        for source in (PREFIX, PREFIX + BENCHMARK + b"// after module\n", PREFIX + BENCHMARK + BENCHMARK,
                       PREFIX + b"}\nmod unrelated {\n" + BENCHMARK):
            with self.subTest(source=source), self.assertRaises(ValueError):
                RUNNER["split_benchmark"](source)

    def test_overlapping_source_roots_are_rejected(self):
        child = self.root / "child"
        child.mkdir()
        for roots in ([self.root, self.root], [self.root, child]):
            with self.subTest(roots=roots), self.assertRaisesRegex(ValueError, "non-overlapping"):
                RUNNER["distinct_roots"](roots)

    def test_raster_only_verifies_common_application_and_records_every_source(self):
        roots, output = self.prepared("raster-only")
        report = json.loads((output / "injection.json").read_text())
        self.assertEqual(report["comparison"], "raster-only")
        self.assertEqual(report["changed_source_files"], ["src/software_raster.rs"])
        self.assertEqual(report["common_source"]["revision"], "common-revision")
        for side in ("before", "after"):
            manifest = json.loads((output / side / "prepared-source-manifest.json").read_text())
            self.assertEqual(manifest, RUNNER["source_manifest"](roots[side]))
            self.assertEqual(manifest, report["sources"][side]["manifest"])

    def test_raster_only_rejects_unrelated_changes_before_injection(self):
        roots = self.sources()
        original = (roots["before"] / "src/linux_window.rs").read_bytes()
        for changed in ("Cargo.lock", "src/software_raster.rs"):
            path = roots["after"] / changed
            if changed.endswith(".rs"):
                path = roots["after"] / "src/unrelated.rs"
            path.write_text("unrelated modification")
            args = SimpleNamespace(comparison="raster-only", before_source=roots["before"],
                                   after_source=roots["after"], harness_source=roots["harness"],
                                   common_source=roots["harness"], common_ref="common-revision",
                                   output=self.root / "injection")
            with self.subTest(changed=path), self.assertRaisesRegex(ValueError, "outside software_raster"):
                RUNNER["prepare"](args)
            self.assertFalse(args.output.exists())
            self.assertEqual((roots["before"] / "src/linux_window.rs").read_bytes(), original)
            if changed == "Cargo.lock":
                path.write_text("fixture lock\n")

    def test_cli_rejects_ambiguous_modes_and_accepts_one_build_control(self):
        base = ["prepare", "--harness-source", "harness", "--before-source", "before", "--output", "out"]
        self.assertEqual(RUNNER["parse_args"](base + ["--after-source", "after"]).comparison, "revisions")
        self.assertEqual(RUNNER["parse_args"](base + ["--comparison", "same-binary"]).comparison, "same-binary")
        cases = [base, base + ["--comparison", "same-binary", "--after-source", "after"],
                 base + ["--comparison", "raster-only", "--after-source", "after"],
                 base + ["--after-source", "after", "--common-ref", "unrelated"]]
        for argv in cases:
            with self.subTest(argv=argv), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                RUNNER["parse_args"](argv)

    def test_accepts_first_fixture_with_libtest_prefix(self):
        fixtures, samples = RUNNER["parse_output"](sample_output(), 300, 30)
        self.assertEqual((len(fixtures), len(samples)), (6, 12))
        self.assertTrue(all(sample["ns_per_frame"] == 2 for sample in samples))

    def test_rejects_missing_duplicate_invalid_and_inconsistent_samples(self):
        output = sample_output()
        cases = [output.replace("1 passed", "0 passed"),
                 output.replace("content=unicode", "content=ascii"),
                 output.replace("width=120", "width=121", 1),
                 output.replace("frames=300", "frames=299", 1),
                 output.replace("ns_per_frame=2.000", "ns_per_frame=3.000", 1),
                 output.replace("color_cells=800", "color_cells=0", 1),
                 output.replace("samples=1", "samples=2", 1),
                 output.replace("sample=0", "sample=1", 1),
                 output.replace("[(20, 21), (20, 21)]", "[(0, 40), (0, 40)]", 1),
                 output.replace(f"frame0={FRAME_CHECKSUMS[0]}", f"frame0={FRAME_CHECKSUMS[1]}", 1),
                 "\n".join(line for line in output.splitlines() if "incremental=true" not in line)]
        for case in cases:
            with self.subTest(case=case), self.assertRaises(ValueError):
                RUNNER["parse_output"](case, 300, 30)

    def test_rejects_fresh_wrong_manifest_and_wrong_target_builds(self):
        args = self.builds()
        messages = args.before_messages
        original = json.loads(messages.read_text())
        for key, value in (("fresh", True), ("manifest_path", str(args.after_source / "Cargo.toml")),
                           ("executable", str(args.after_target / "release/deps/kokuban-fixture")),
                           ("profile", {"test": True, "opt_level": "0"})):
            messages.write_text(json.dumps({**original, key: value}) + "\n")
            with self.subTest(key=key), self.assertRaises(ValueError):
                RUNNER["test_artifact"](args.before_source, args.before_target, messages)

    def test_frame_gate_rejects_mismatch_truncation_and_missing_image(self):
        fixtures, _ = RUNNER["parse_output"](sample_output(), 300, 30)
        for side in ("before", "after"):
            self.images(self.root / (side + "-frames"))
        hashes = RUNNER["verify_frames"](self.root, fixtures)
        self.assertEqual(hashes["before"], hashes["after"])
        image = self.root / "after-frames/unicode-single-row-0.xrgb8888le"
        original = image.read_bytes()
        for content in (bytes([2]) + original[1:], original[:-1]):
            image.write_bytes(content)
            with self.assertRaises(ValueError):
                RUNNER["verify_frames"](self.root, fixtures)
        image.unlink()
        with self.assertRaises(ValueError):
            RUNNER["verify_frames"](self.root, fixtures)

    def test_frame_checksums_match_known_fnv1a_vectors(self):
        self.assertEqual(RUNNER["frame_checksum"](b""), "cbf29ce484222325")
        self.assertEqual(RUNNER["frame_checksum"](b"foobar"), "85944171f73967e8")

    def test_frame_gate_rejects_identical_states_and_false_printed_checksums(self):
        fixtures, _ = RUNNER["parse_output"](sample_output(), 300, 30)
        for side in ("before", "after"):
            self.images(self.root / (side + "-frames"))
        first = self.root / "before-frames/ascii-single-row-0.xrgb8888le"
        second = self.root / "before-frames/ascii-single-row-1.xrgb8888le"
        original = second.read_bytes()
        second.write_bytes(first.read_bytes())
        with self.assertRaisesRegex(ValueError, "did not change visible pixels"):
            RUNNER["verify_frames"](self.root, fixtures)
        second.write_bytes(original)
        fixtures[("ascii", "single-row")]["checksums"][0] = "0000000000000000"
        with self.assertRaisesRegex(ValueError, "checksum does not match"):
            RUNNER["verify_frames"](self.root, fixtures)

    def test_paired_median_is_distinct_from_ratio_of_medians(self):
        _, samples = RUNNER["parse_output"](sample_output(), 300, 30)
        records = []
        for pair, (before, after) in enumerate(((1, 2), (2, 100), (100, 1)), 1):
            for side, value in (("before", before), ("after", after)):
                records.extend({**sample, "side": side, "pair": pair, "ns_per_frame": value}
                               for sample in samples)
        for row in RUNNER["summarize"](records, 3):
            self.assertEqual(row["latency_change_percent"], 0)
            self.assertEqual(row["paired_latency_change_percent"], [100, 4900, -99])
            self.assertEqual(row["median_paired_latency_change_percent"], 100)

    def test_alternating_pairs_publish_ratios_only_after_pixel_equivalence(self):
        args = self.builds()
        calls = []

        def execute(command, env, stdout, **options):
            side = "before" if "before target" in command[0] else "after"
            calls.append(side)
            self.assertNotIn("KOKUBAN_FRAME_UNRELATED", env)
            self.assertEqual(command[1:], ["--exact", RUNNER["TEST"], "--ignored", "--nocapture", "--test-threads=1"])
            if "KOKUBAN_FRAME_OUTPUT_DIR" in env:
                self.images(Path(env["KOKUBAN_FRAME_OUTPUT_DIR"]))
            stdout.write(sample_output(600 if side == "before" else 300))
            return SimpleNamespace(returncode=0)

        with patch.dict(GLOBALS["os"].environ, KOKUBAN_FRAME_UNRELATED="inherited"), \
                patch.object(GLOBALS["platform"], "system", return_value="Linux"), \
                patch.object(GLOBALS["platform"], "platform", return_value="test Linux"), \
                patch.object(GLOBALS["os"], "sched_getaffinity", return_value={2}, create=True), \
                patch.object(GLOBALS["subprocess"], "check_output", return_value="rustc 1.94.1"), \
                patch.object(GLOBALS["subprocess"], "run", side_effect=execute), redirect_stdout(io.StringIO()):
            self.assertEqual(RUNNER["measure"](args), 0)
        report = json.loads((args.output / "report.json").read_text())
        self.assertEqual(calls, ["before", "after", "after", "before", "before", "after"])
        self.assertEqual(report["status"], "completed")
        self.assertTrue(report["pixel_equivalence_verified"])
        self.assertEqual(len(report["records"]), 72)
        self.assertEqual(len(report["summary"]), 12)
        self.assertTrue(all(row["paired_latency_change_percent"] == [-50.0] * 3 for row in report["summary"]))

    def test_failed_process_retains_log_and_checkpoint_without_summary(self):
        args = self.builds()

        def execute(command, stdout, **options):
            stdout.write("injected failure\n")
            return SimpleNamespace(returncode=1)

        with patch.object(GLOBALS["platform"], "system", return_value="Linux"), \
                patch.object(GLOBALS["platform"], "platform", return_value="test Linux"), \
                patch.object(GLOBALS["os"], "sched_getaffinity", return_value={2}, create=True), \
                patch.object(GLOBALS["subprocess"], "check_output", return_value="rustc 1.94.1"), \
                patch.object(GLOBALS["subprocess"], "run", side_effect=execute), redirect_stderr(io.StringIO()):
            self.assertEqual(RUNNER["measure"](args), 1)
        report = json.loads((args.output / "report.json").read_text())
        self.assertEqual(report["status"], "failed")
        self.assertIsNone(report["summary"])
        self.assertEqual((args.output / "01-before.log").read_text(), "injected failure\n")

    def test_same_binary_control_uses_one_build_and_exactly_one_executable_path(self):
        args = self.builds("same-binary")
        calls = []

        def execute(command, env, stdout, **options):
            calls.append(command[0])
            if "KOKUBAN_FRAME_OUTPUT_DIR" in env:
                self.images(Path(env["KOKUBAN_FRAME_OUTPUT_DIR"]))
            stdout.write(sample_output())
            return SimpleNamespace(returncode=0)

        with patch.object(GLOBALS["platform"], "system", return_value="Linux"), \
                patch.object(GLOBALS["platform"], "platform", return_value="test Linux"), \
                patch.object(GLOBALS["os"], "sched_getaffinity", return_value={2}, create=True), \
                patch.object(GLOBALS["subprocess"], "check_output", return_value="rustc 1.94.1"), \
                patch.object(GLOBALS["subprocess"], "run", side_effect=execute), redirect_stdout(io.StringIO()):
            self.assertEqual(RUNNER["measure"](args), 0)
        report = json.loads((args.output / "report.json").read_text())
        self.assertEqual(report["comparison"], "same-binary")
        self.assertEqual(set(report["builds"]), {"before"})
        self.assertEqual(len(calls), 6)
        self.assertEqual(len(set(calls)), 1)
        self.assertEqual(report["execution_binaries"]["before"], report["execution_binaries"]["after"])
        self.assertTrue(all(row["median_paired_latency_change_percent"] == 0 for row in report["summary"]))

    def test_post_preparation_raster_change_is_rejected_before_execution(self):
        args = self.builds("raster-only")
        (args.before_source / "src/software_raster.rs").write_text("late modification")
        with patch.object(GLOBALS["platform"], "system", return_value="Linux"), \
                patch.object(GLOBALS["platform"], "platform", return_value="test Linux"), \
                patch.object(GLOBALS["os"], "sched_getaffinity", return_value={2}, create=True), \
                patch.object(GLOBALS["subprocess"], "check_output", return_value="rustc 1.94.1"), \
                patch.object(GLOBALS["subprocess"], "run") as execute, redirect_stderr(io.StringIO()):
            self.assertEqual(RUNNER["measure"](args), 1)
        execute.assert_not_called()
        report = json.loads((args.output / "report.json").read_text())
        self.assertIn("application source changed", report["error"])

    def test_raster_measure_records_common_application_and_kernel_revisions(self):
        args = self.builds("raster-only")
        args.pairs = 1

        def execute(command, env, stdout, **options):
            self.images(Path(env["KOKUBAN_FRAME_OUTPUT_DIR"]))
            stdout.write(sample_output())
            return SimpleNamespace(returncode=0)

        with patch.object(GLOBALS["platform"], "system", return_value="Linux"), \
                patch.object(GLOBALS["platform"], "platform", return_value="test Linux"), \
                patch.object(GLOBALS["os"], "sched_getaffinity", return_value={2}, create=True), \
                patch.object(GLOBALS["subprocess"], "check_output", return_value="rustc 1.94.1"), \
                patch.object(GLOBALS["subprocess"], "run", side_effect=execute), redirect_stdout(io.StringIO()):
            self.assertEqual(RUNNER["measure"](args), 0)
        report = json.loads((args.output / "report.json").read_text())
        for side in ("before", "after"):
            self.assertEqual(report["builds"][side]["source_revision"], "common-revision")
            self.assertEqual(report["builds"][side]["kernel_revision"], side + "-revision")


if __name__ == "__main__":
    unittest.main()
