import importlib.util
from pathlib import Path
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location("frame_repaint", Path(__file__).with_name("compare-frame-repaint.py"))
BENCHMARK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCHMARK)


def valid_output():
    lines = ["test result: ok. 1 passed; 0 failed; 0 ignored;"]
    for content in ("ascii", "ascii-after-emoji", "unicode"):
        for change in ("single-row", "full-screen"):
            lines.append(f"frame-repaint fixture content={content} change={change} "
                         "cols=120 rows=40 width=120 height=40 cell_width=1 cell_height=1 "
                         "warmup=30 samples=1 steps=3 mono_cells=4800 color_cells=0 "
                         "bands=[(0, 1), (0, 1)] frame0=0000000000000000 frame1=1111111111111111")
            for incremental in ("false", "true"):
                lines.append(f"frame-repaint sample content={content} change={change} sample=0 "
                             f"incremental={incremental} frames=3 elapsed_ns=100 ns_per_frame=33.333")
    return "\n".join(lines)


def write_reference_frames(directory):
    for side in ("before", "after"):
        folder = directory / f"{side}-frames"
        folder.mkdir()
        for content in BENCHMARK.CONTENTS:
            for change in BENCHMARK.CHANGES:
                for index in (0, 1):
                    (folder / f"{content}-{change}-{index}.xrgb8888le").write_bytes(
                        bytes([index]) * (120 * 40 * 4))


class FrameRepaintTests(unittest.TestCase):
    def test_retains_integer_clock_precision_and_requires_all_modes(self):
        output = valid_output()
        fixtures, records = BENCHMARK.parse_output(output, 3)
        self.assertEqual(len(fixtures), 6)
        self.assertEqual(len(records), 12)
        self.assertEqual(records[0]["ns_per_frame"], 100 / 3)
        for invalid in (output.rsplit("\n", 1)[0], output + "\n" + output.splitlines()[-1],
                        output.replace("frames=3", "frames=4", 1),
                        output.replace("1 passed; 0 failed", "0 passed; 1 failed")):
            with self.subTest(invalid=invalid[-100:]), self.assertRaises(ValueError):
                BENCHMARK.parse_output(invalid, 3)

    def test_rejects_duplicate_unknown_and_invalid_fixture_geometry(self):
        output = valid_output()
        for old, new in (("content=ascii-after-emoji", "content=ascii"),
                         ("content=ascii change=single-row", "content=unknown change=single-row"),
                         ("cols=120", "cols=119"), ("rows=40", "rows=41"),
                         ("width=120", "width=119"), ("height=40", "height=39"),
                         ("cell_width=1", "cell_width=0"), ("cell_height=1", "cell_height=0"),
                         ("cell_width=1", "cell_width=2"), ("cell_height=1", "cell_height=2"),
                         ("width=120", "width=-120"), ("height=40", "height=invalid")):
            with self.subTest(old=old, new=new), self.assertRaisesRegex(ValueError, "fixture"):
                BENCHMARK.parse_output(output.replace(old, new, 1), 3)

    def test_preserves_raw_fixtures_with_nontrivial_cell_dimensions(self):
        output = (valid_output().replace("width=120", "width=1080")
                  .replace("height=40", "height=680")
                  .replace("cell_width=1", "cell_width=9")
                  .replace("cell_height=1", "cell_height=17"))
        fixtures, _ = BENCHMARK.parse_output(output, 3)
        self.assertEqual(fixtures, [line for line in output.splitlines()
                                    if line.startswith("frame-repaint fixture")])
        for geometry in BENCHMARK.fixture_geometry(fixtures).values():
            self.assertEqual((geometry["width"], geometry["height"]), (1080, 680))

    def test_paired_summary_does_not_replace_pairs_with_ratio_of_medians(self):
        _, rows = BENCHMARK.parse_output(valid_output(), 3)
        records = []
        for pair, (before, after) in enumerate(((1, 2), (2, 100), (100, 1))):
            for side, value in (("before", before), ("after", after)):
                records.extend(dict(row, side=side, pair=pair, ns_per_frame=value) for row in rows)
        for summary in BENCHMARK.summaries(records, 3):
            self.assertEqual(summary["paired_speedups"], [.5, .02, 100])
            self.assertEqual(summary["median_paired_speedup"], .5)
            self.assertEqual(summary["median_paired_latency_change_percent"], 100)

    def test_pixel_verification_rejects_single_changed_byte_and_missing_frame(self):
        fixtures, _ = BENCHMARK.parse_output(valid_output(), 3)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            write_reference_frames(directory)
            self.assertEqual(len(BENCHMARK.verify_pixels(directory, fixtures)), 12)
            changed = directory / "after-frames/unicode-full-screen-1.xrgb8888le"
            changed.write_bytes(b"\0" + changed.read_bytes()[1:])
            with self.assertRaisesRegex(ValueError, "reference pixels differ"):
                BENCHMARK.verify_pixels(directory, fixtures)
            changed.unlink()
            with self.assertRaisesRegex(ValueError, "incomplete"):
                BENCHMARK.verify_pixels(directory, fixtures)

    def test_rejects_equally_truncated_or_extended_frames_on_both_sides(self):
        fixtures, _ = BENCHMARK.parse_output(valid_output(), 3)
        for size in (0, 4, 120 * 40 * 4 - 4, 120 * 40 * 4 + 4):
            with self.subTest(size=size), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                write_reference_frames(directory)
                for side in ("before", "after"):
                    (directory / f"{side}-frames/unicode-full-screen-1.xrgb8888le").write_bytes(b"\1" * size)
                with self.assertRaisesRegex(ValueError, "pixel byte count"):
                    BENCHMARK.verify_pixels(directory, fixtures)

    def test_rejects_full_sized_frames_without_visible_changes(self):
        fixtures, _ = BENCHMARK.parse_output(valid_output(), 3)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            write_reference_frames(directory)
            for side in ("before", "after"):
                folder = directory / f"{side}-frames"
                (folder / "unicode-full-screen-1.xrgb8888le").write_bytes(
                    (folder / "unicode-full-screen-0.xrgb8888le").read_bytes())
            with self.assertRaisesRegex(ValueError, "did not change visible pixels"):
                BENCHMARK.verify_pixels(directory, fixtures)


if __name__ == "__main__":
    unittest.main()
