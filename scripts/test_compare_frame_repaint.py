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
            lines.append(f"frame-repaint fixture content={content} change={change}")
            for incremental in ("false", "true"):
                lines.append(f"frame-repaint sample content={content} change={change} sample=0 "
                             f"incremental={incremental} frames=3 elapsed_ns=100 ns_per_frame=33.333")
    return "\n".join(lines)


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
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            for side in ("before", "after"):
                folder = directory / f"{side}-frames"
                folder.mkdir()
                for content in ("ascii", "ascii-after-emoji", "unicode"):
                    for change in ("single-row", "full-screen"):
                        for index in (0, 1):
                            (folder / f"{content}-{change}-{index}.xrgb8888le").write_bytes(bytes([index]) * 16)
            self.assertEqual(len(BENCHMARK.verify_pixels(directory)), 12)
            changed = directory / "after-frames/unicode-full-screen-1.xrgb8888le"
            changed.write_bytes(b"\0" + b"\1" * 15)
            with self.assertRaisesRegex(ValueError, "reference pixels differ"):
                BENCHMARK.verify_pixels(directory)
            changed.unlink()
            with self.assertRaisesRegex(ValueError, "incomplete"):
                BENCHMARK.verify_pixels(directory)


if __name__ == "__main__":
    unittest.main()
