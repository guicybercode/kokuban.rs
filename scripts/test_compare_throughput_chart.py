import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "render-comparison-chart.py"
SPEC = importlib.util.spec_from_file_location("render_comparison_chart", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

EXPECTED_MEDIANS = {
    "kokuban": {"ascii": 70.015, "ansi": 55.677, "unicode": 36.853, "short_lines": 36.634},
    "ghostty": {"ascii": 28.333, "ansi": 22.088, "unicode": 26.646, "short_lines": 24.340},
    "alacritty": {"ascii": 50.854, "ansi": 53.846, "unicode": 51.779, "short_lines": 50.246},
    "kitty": {"ascii": 78.800, "ansi": 16.747, "unicode": 57.365, "short_lines": 28.917},
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
        self.assertIn(MODULE.TITLE, svg)
        self.assertIn("55.7", svg)
        self.assertIn('fill="#1f3a2a"', svg)


if __name__ == "__main__":
    unittest.main()
