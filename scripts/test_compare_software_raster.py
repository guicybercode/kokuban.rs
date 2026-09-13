#!/usr/bin/env python3
"""Cheap checks for benchmark integrity, including an intentionally faulty kernel."""

import argparse
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location(
    "software_raster_benchmark", Path(__file__).with_name("benchmark-software-raster.py"))
BENCHMARK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCHMARK)


def observations() -> list[dict]:
    records = [{"kind": "validation", "mode": "glyphs", "width": 9,
                "immediate_frame_comparisons": 19, "passed": True}]
    for index, (before, after) in enumerate(((100, 100), (200, 400), (900, 300))):
        records.append({"kind": "sample", "mode": "glyphs", "width": 9,
                        "sample": index, "order": "BA" if index % 2 else "AB",
                        "frames": 2, "before_elapsed_ns": before, "after_elapsed_ns": after,
                        "final_pixels_equal": True})
    return records


def summarize(records: list[dict]) -> list[dict]:
    return BENCHMARK.summarize_results(
        "\n".join(json.dumps(record) for record in records), [9], ["glyphs"], 3, 2)


class SummaryTests(unittest.TestCase):
    def test_uses_median_of_paired_ratios_and_keeps_raw_nanoseconds(self):
        case = summarize(observations())[0]
        self.assertEqual(case["summary"]["median_paired_speedup"], 1.0)
        self.assertEqual(case["summary"]["median_paired_throughput_change_pct"], 0.0)
        self.assertEqual(case["summary"]["after_faster_pairs"], 1)
        self.assertEqual(case["summary"]["equal_pairs"], 1)
        self.assertEqual(case["samples"][2]["before_elapsed_ns"], 900)
        self.assertEqual(case["samples"][2]["before_ms_per_frame"], 0.00045)

    def test_rejects_partial_duplicate_and_unvalidated_results(self):
        complete = observations()
        for records in (complete[:-1], complete[1:], complete + [complete[-1]],
                        [complete[0]] + complete, complete[:1]):
            with self.subTest(records=records), self.assertRaises(ValueError):
                summarize(records)

    def test_rejects_pixel_failure_and_invalid_measurements(self):
        for field, value in (("final_pixels_equal", False), ("before_elapsed_ns", 0),
                             ("after_elapsed_ns", -1), ("before_elapsed_ns", 1.5),
                             ("after_elapsed_ns", True), ("frames", 3), ("order", "BA")):
            records = observations()
            records[1][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                summarize(records)

    def test_artifacts_cannot_overwrite_existing_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            self.assertEqual(BENCHMARK.artifact_directory(temporary), directory.resolve())
            (directory / "results.json").write_text("existing evidence", encoding="utf-8")
            with self.assertRaises(argparse.ArgumentTypeError):
                BENCHMARK.artifact_directory(temporary)


FAULT_FIXTURE = r'''
use crate::glyph_atlas::GlyphEntry;
pub(crate) fn draw_glyph_a8(frame: &mut [u32], _: (u32,u32), _: &[u8], _: (u32,u32),
    _: GlyphEntry, _: (i32,i32), _: u32) {
    frame[0] = 0x506070;
}
pub(crate) fn draw_glyph_rgba(_: &mut [u32], _: (u32,u32), _: &[u8], _: (u32,u32),
    _: GlyphEntry, _: (i32,i32)) {}
pub(crate) fn fill_rect(_: &mut [u32], _: (u32,u32), _: (i32,i32), _: (u32,u32), _: u32, _: u8) {}
pub(crate) fn draw_image_rgba(_: &mut [u32], _: (u32,u32), _: &[u8], _: (u32,u32),
    _: (f32,f32,f32,f32)) {}
'''


class HarnessTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("rustc"), "rustc is needed for the fault-injection check")
    def test_immediate_check_catches_fault_that_repeated_passes_hide(self):
        faulty = FAULT_FIXTURE.replace(
            "frame[0] = 0x506070;",
            "frame[0] = if frame[0] == 0x203040 { 0x506071 } else { 0x506070 };")
        with tempfile.TemporaryDirectory(prefix="raster-benchmark-fault-") as temporary:
            directory = Path(temporary)
            (directory / "before.rs").write_text(FAULT_FIXTURE, encoding="utf-8")
            (directory / "after.rs").write_text(faulty, encoding="utf-8")
            (directory / "main.rs").write_text(BENCHMARK.HARNESS, encoding="utf-8")
            subprocess.run(["rustc", "--edition=2021", "-O", "main.rs", "-o", "benchmark"],
                           cwd=directory, check=True, capture_output=True, text=True, timeout=60)
            result = subprocess.run([str(directory / "benchmark"), "1", "1", "1", "9", "glyphs"],
                                    capture_output=True, text=True, timeout=20)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("immediate sequence initial=00203040 step=0: pixel 0", result.stderr)
            self.assertNotIn('"kind":"sample"', result.stdout)


if __name__ == "__main__":
    unittest.main()
