from contextlib import redirect_stdout
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "render-comparison-chart.py"
SPEC = importlib.util.spec_from_file_location("render_comparison_chart", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

EXPECTED_MEDIANS = {
    "kokuban": {"ascii": 92.361, "ansi": 63.624, "unicode": 47.644, "short_lines": 39.634},
    "ghostty": {"ascii": 25.551, "ansi": 20.745, "unicode": 26.094, "short_lines": 22.384},
    "alacritty": {"ascii": 53.755, "ansi": 57.965, "unicode": 55.750, "short_lines": 53.128},
    "kitty": {"ascii": 84.496, "ansi": 16.455, "unicode": 59.503, "short_lines": 27.788},
}


class RenderComparisonChartTests(unittest.TestCase):
    def setUp(self):
        self.medians = MODULE.load_medians(MODULE.DEFAULT_SUMMARY)

    def test_medians_match_audited_summary(self):
        for terminal, workloads in EXPECTED_MEDIANS.items():
            for workload, expected in workloads.items():
                self.assertAlmostEqual(
                    self.medians[terminal][workload], expected, places=3,
                    msg=f"{terminal}/{workload}",
                )

    def test_svg_names_every_terminal(self):
        svg = MODULE.render_svg(self.medians)
        self.assertTrue(svg.startswith("<svg"))
        for name in ("Kokuban", "Ghostty", "Alacritty", "Kitty"):
            self.assertIn(name, svg)
        self.assertNotIn("Terminal.app", svg)
        self.assertIn(MODULE.TITLE, svg)
        self.assertIn("55.7", svg)
        self.assertIn('fill="#1f3a2a"', svg)

    def test_default_arguments_reproduce_the_committed_chart(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "chart.svg"
            with redirect_stdout(io.StringIO()):
                self.assertEqual(MODULE.main(["--output", str(output)]), 0)
            self.assertEqual(output.read_text(encoding="utf-8"), MODULE.DEFAULT_OUTPUT.read_text(encoding="utf-8"))

    def test_other_summary_title_and_subtitle_render_the_terminals_present(self):
        def workloads(scale):
            return {workload: {"mib_per_second": {"median": scale * (index + 1)}}
                    for index, workload in enumerate(MODULE.WORKLOADS)}

        summary = {"terminal_summaries": {"terminal": {"workloads": workloads(10.0)},
                                          "kokuban": {"workloads": workloads(30.0)},
                                          "alacritty": {"workloads": workloads(20.0)}}}
        with tempfile.TemporaryDirectory() as temporary:
            summary_path = Path(temporary) / "summary.json"
            summary_path.write_text(json.dumps(summary), encoding="utf-8")
            output = Path(temporary) / "macos.svg"
            with redirect_stdout(io.StringIO()):
                status = MODULE.main(["--summary", str(summary_path), "--output", str(output),
                                      "--title", "PTY throughput, macOS host", "--subtitle", "line one",
                                      "--subtitle", "line two"])
            self.assertEqual(status, 0)
            svg = output.read_text(encoding="utf-8")
            medians = MODULE.load_medians(summary_path)
            with self.assertRaises(KeyError):
                MODULE.load_medians(summary_path, ("kokuban", "kitty"))
        self.assertEqual(list(medians), ["kokuban", "alacritty", "terminal"])
        self.assertEqual(medians["terminal"]["short_lines"], 40.0)
        self.assertIn("<title id=\"chart-title\">PTY throughput, macOS host</title>", svg)
        self.assertIn("line one", svg)
        self.assertIn("line two", svg)
        self.assertNotIn(MODULE.TITLE, svg)
        self.assertNotIn("Ghostty", svg)
        self.assertNotIn("Kitty", svg)
        self.assertIn("Terminal.app", svg)
        self.assertIn('fill="#3a434c"', svg)


if __name__ == "__main__":
    unittest.main()
