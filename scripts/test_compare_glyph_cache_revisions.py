"""Glyph-cache comparator integrity tests; no Rust builds or timing runs."""

from contextlib import redirect_stderr
import io
import json
from pathlib import Path
import runpy
import tempfile
from types import SimpleNamespace
import unittest


RUNNER = runpy.run_path(str(Path(__file__).with_name("compare-glyph-cache-revisions.py")))


class PreparationTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
