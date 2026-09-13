"""Glyph-cache comparator integrity tests; no Rust builds or timing runs."""

from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
import runpy
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch


RUNNER = runpy.run_path(str(Path(__file__).with_name("compare-glyph-cache-revisions.py")))
GLOBALS = RUNNER["measure"].__globals__


def sample_output(elapsed=768, footprint=100):
    lines = ["test glyph_atlas::lookup_benchmark::benchmark_warmed_lookups ... "]
    for workload in RUNNER["WORKLOADS"]:
        lines.append(f"glyph-cache fixture workload={workload} lookups_per_iteration=384 "
                     "iterations=2 warmup=1 samples=1 atlas_width=1024 atlas_height=1024 "
                     "scalar_glyphs=95 grapheme_glyphs=32 fingerprint=1234567890abcdef "
                     "pixels_fingerprint=abcdef1234567890 font_name_hex=4d6f6e6f "
                     f"atlas_bytes={footprint} scalar_cache_bytes=48")
        lines.append(f"glyph-cache sample workload={workload} sample=0 lookups=768 elapsed_ns={elapsed}")
    return "\n".join(lines) + "\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n"


class FixtureSetup(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="glyph-cache-controls-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.fixture = self.root / "fixture.rs"
        self.fixture.write_text("#[test]\n#[ignore]\nfn benchmark_warmed_lookups() {}\n")
        for side in ("before", "after"):
            source = self.root / side
            (source / "src").mkdir(parents=True)
            (source / "src/glyph_atlas.rs").write_text(f"// {side} production implementation\n")
            (source / "Cargo.toml").write_text("manifest\n")
            (source / "Cargo.lock").write_text("lock\n")
        self.args = SimpleNamespace(before_source=self.root / "before", after_source=self.root / "after",
                                    fixture=self.fixture, output=self.root / "injection", comparison="revisions")


class PreparationTests(FixtureSetup):
    def test_identical_injection_preserves_production_prefix_and_records_source_differences(self):
        original = (self.args.before_source / RUNNER["SOURCE"]).read_bytes()
        RUNNER["prepare"](self.args)
        report = json.loads((self.args.output / "injection.json").read_text())
        self.assertEqual(report["original_source_differences"], ["src/glyph_atlas.rs"])
        for side in ("before", "after"):
            source = getattr(self.args, side + "_source")
            self.assertEqual((source / RUNNER["FIXTURE"]).read_bytes(), self.fixture.read_bytes())
            self.assertEqual(RUNNER["manifest"](source), report["sources"][side]["manifest"])
        self.assertEqual((self.args.before_source / RUNNER["SOURCE"]).read_bytes(), original + RUNNER["MODULE"])
        self.assertEqual((self.args.output / "before/original-glyph_atlas.rs").read_bytes(), original)

    def test_existing_module_is_rejected_before_either_source_changes(self):
        original = (self.args.before_source / RUNNER["SOURCE"]).read_bytes()
        (self.args.after_source / RUNNER["SOURCE"]).write_bytes(RUNNER["MODULE"])
        with self.assertRaisesRegex(ValueError, "already present"):
            RUNNER["prepare"](self.args)
        self.assertFalse(self.args.output.exists())
        self.assertEqual((self.args.before_source / RUNNER["SOURCE"]).read_bytes(), original)

    def test_same_binary_prepares_only_one_source(self):
        self.args.comparison = "same-binary"
        self.args.after_source = None
        RUNNER["prepare"](self.args)
        report = json.loads((self.args.output / "injection.json").read_text())
        self.assertEqual(set(report["sources"]), {"before"})
        self.assertFalse((self.root / "after" / RUNNER["FIXTURE"]).exists())

    def test_cli_rejects_incomplete_or_ambiguous_comparison(self):
        common = ["prepare", "--before-source", "before", "--fixture", "fixture", "--output", "out"]
        for argv in (common, common + ["--comparison", "same-binary", "--after-source", "after"]):
            with self.subTest(argv=argv), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                RUNNER["parse_args"](argv)
        self.assertEqual(RUNNER["parse_args"](common + ["--comparison", "same-binary"]).comparison, "same-binary")


class MeasurementTests(FixtureSetup):
    def builds(self, comparison="revisions"):
        self.args.comparison = comparison
        if comparison == "same-binary":
            self.args.after_source = None
        RUNNER["prepare"](self.args)
        args = SimpleNamespace(comparison=comparison, before_source=self.args.before_source,
                               after_source=self.args.after_source, injection=self.args.output / "injection.json",
                               output=self.root / "measurements", pairs=3, iterations=2, warmup=1, timeout=1)
        for side in RUNNER["build_sides"](comparison):
            target = self.root / (side + " target")
            (target / "release/deps").mkdir(parents=True)
            binary = target / "release/deps/kokuban-fixture"
            binary.write_bytes(side.encode())
            binary.chmod(0o700)
            messages = self.root / (side + "-messages.jsonl")
            messages.write_text(json.dumps({"reason": "compiler-artifact", "fresh": False,
                "target": {"name": "kokuban", "kind": ["bin"]}, "profile": {"test": True, "opt_level": "3"},
                "manifest_path": str(getattr(args, side + "_source") / "Cargo.toml"), "executable": str(binary)}) + "\n")
            for name, value in (("target", target), ("messages", messages), ("ref", side + "-revision")):
                setattr(args, side + "_" + name, value)
        return args

    def execute(self, args, callback):
        with patch.object(RUNNER["platform"], "system", return_value="Darwin"), \
                patch.object(RUNNER["platform"], "platform", return_value="test macOS"), \
                patch.object(RUNNER["subprocess"], "check_output", return_value="rustc 1.94.1"), \
                patch.object(RUNNER["subprocess"], "run", side_effect=callback), \
                redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            status = RUNNER["measure"](args)
        return status, json.loads((args.output / "report.json").read_text())

    def test_same_binary_has_one_build_and_one_exact_executable_path(self):
        args = self.builds("same-binary")
        calls = []
        def execute(command, stdout, env, **options):
            calls.append(command)
            self.assertEqual(env["KOKUBAN_GLYPH_LOOKUP_SAMPLES"], "1")
            self.assertEqual(command[1:], ["--exact", RUNNER["TEST"], "--ignored", "--nocapture", "--test-threads=1"])
            stdout.write(sample_output())
            return SimpleNamespace(returncode=0)
        status, report = self.execute(args, execute)
        self.assertEqual(status, 0)
        self.assertEqual(len(calls), 6)
        self.assertEqual(len({tuple(call) for call in calls}), 1)
        self.assertEqual(set(report["builds"]), {"before"})
        self.assertEqual(report["execution_binaries"]["before"], report["execution_binaries"]["after"])
        self.assertEqual(len(report["records"]), 30)
        self.assertIsNone(report["cpu_affinity"])
        self.assertIn("not enforced", report["affinity_note"])
        self.assertTrue(all(row["median_paired_time_change_percent"] == 0 for row in report["summary"]))

    def test_alternates_revisions_and_allows_only_footprint_differences(self):
        args = self.builds()
        calls = []
        def execute(command, stdout, **options):
            side = "before" if "before target" in command[0] else "after"
            calls.append(side)
            stdout.write(sample_output(768 if side == "before" else 384, 100 if side == "before" else 200))
            return SimpleNamespace(returncode=0)
        status, report = self.execute(args, execute)
        self.assertEqual(status, 0)
        self.assertEqual(calls, ["before", "after", "after", "before", "before", "after"])
        self.assertTrue(all(row["median_paired_time_change_percent"] == -50 for row in report["summary"]))
        self.assertNotEqual(report["fixtures"]["before"]["ascii-regular"]["atlas_bytes"],
                            report["fixtures"]["after"]["ascii-regular"]["atlas_bytes"])

    def test_fingerprint_mismatch_stops_without_publishing_ratios(self):
        args = self.builds()
        calls = []
        def execute(command, stdout, **options):
            calls.append(command)
            output = sample_output()
            if len(calls) == 2:
                output = output.replace("fingerprint=1234567890abcdef", "fingerprint=1111111111111111", 1)
            stdout.write(output)
            return SimpleNamespace(returncode=0)
        status, report = self.execute(args, execute)
        self.assertEqual(status, 1)
        self.assertEqual(len(calls), 2)
        self.assertIn("fingerprints", report["error"])
        self.assertIsNone(report["summary"])
        self.assertFalse((args.output / "02-before.log").exists())

    def test_reused_root_artifact_and_late_source_changes_are_rejected(self):
        args = self.builds()
        message = json.loads(args.before_messages.read_text())
        args.before_messages.write_text(json.dumps({**message, "fresh": True}) + "\n")
        with self.assertRaisesRegex(ValueError, "reused"):
            RUNNER["test_artifact"](args.before_source, args.before_target, args.before_messages)
        args.before_messages.write_text(json.dumps(message) + "\n")
        (args.before_source / RUNNER["SOURCE"]).write_bytes(b"changed after injection")
        callback = Mock()
        status, report = self.execute(args, callback)
        self.assertEqual(status, 1)
        callback.assert_not_called()
        self.assertIn("source changed", report["error"])


class ProtocolTests(unittest.TestCase):
    def test_keeps_integer_duration_and_requires_every_workload(self):
        output = sample_output(elapsed=769)
        fixtures, samples = RUNNER["parse_output"](output, 2, 1)
        self.assertEqual(len(fixtures), 5)
        self.assertEqual(samples[0]["elapsed_ns"], 769)
        self.assertEqual(samples[0]["ns_per_lookup"], 769 / 768)
        self.assertEqual(fixtures["ascii-regular"]["font_name"], "Mono")
        invalid = [output.replace("1 passed", "0 passed"), output.replace("workload=unicode-scalars", "workload=ascii-regular"),
                   output.replace("lookups=768", "lookups=767", 1), output.replace("elapsed_ns=769", "elapsed_ns=0", 1),
                   output.replace("iterations=2", "iterations=3", 1), output.replace("atlas_width=1024", "atlas_width=0", 1),
                   output.replace("font_name_hex=4d6f6e6f", "font_name_hex=oops", 1),
                   output.replace("atlas_bytes=100", "atlas_bytes=0", 1),
                   output.replace("fingerprint=1234567890abcdef", "fingerprint=bad", 1),
                   output + output.splitlines()[1] + "\n"]
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(ValueError):
                RUNNER["parse_output"](value, 2, 1)

    def test_median_is_computed_from_pairs_not_ratio_of_independent_medians(self):
        _, samples = RUNNER["parse_output"](sample_output(), 2, 1)
        records = []
        for pair, (before, after) in enumerate(((1, 2), (2, 100), (100, 1)), 1):
            for side, elapsed in (("before", before), ("after", after)):
                records.extend(dict(row, pair=pair, side=side, elapsed_ns=elapsed, ns_per_lookup=elapsed / 768)
                               for row in samples)
        for row in RUNNER["summarize"](records, 3):
            self.assertEqual(row["paired_time_change_percent"], [100, 4900, -99])
            self.assertEqual(row["median_paired_time_change_percent"], 100)


if __name__ == "__main__":
    unittest.main()
