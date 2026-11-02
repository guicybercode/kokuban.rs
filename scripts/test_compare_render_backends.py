import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "compare_render_backends", Path(__file__).with_name("compare-render-backends.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class RendererLogTests(unittest.TestCase):
    def test_parses_backend_description_and_frame_count(self):
        stderr = (
            "[INFO kokuban] starting\n"
            "[INFO kokuban::linux_window::presenter] Linux renderer: wgpu ([render] backend = gpu, "
            "partial damage off; Vulkan adapter \"llvmpipe\" Cpu, driver llvmpipe 24.2, surface Bgra8Unorm, readback off)\n"
            "[INFO kokuban::linux_window::presenter] Linux renderer wgpu: presented 42 frames\n"
        )
        self.assertEqual(
            MODULE.parse_renderer_log(stderr),
            ("wgpu", "[render] backend = gpu, partial damage off; Vulkan adapter \"llvmpipe\" Cpu, "
             "driver llvmpipe 24.2, surface Bgra8Unorm, readback off", 42))

    def test_missing_lines_yield_none(self):
        self.assertEqual(MODULE.parse_renderer_log("nothing"), (None, None, None))


class SummaryTests(unittest.TestCase):
    def test_medians_and_cpu_per_frame_only_use_successful_runs(self):
        runs = [
            {"requested_backend": "software", "backend": "software", "exit_code": 0,
             "frames": 100, "cpu_seconds": 1.0, "wall_seconds": 2.0, "peak_rss_kib": 1024},
            {"requested_backend": "software", "backend": "software", "exit_code": 0,
             "frames": 50, "cpu_seconds": 2.0, "wall_seconds": 3.0, "peak_rss_kib": 2048},
            {"requested_backend": "gpu", "backend": None, "exit_code": 1,
             "frames": None, "cpu_seconds": 0.1, "wall_seconds": 0.2, "peak_rss_kib": None},
        ]
        summary = MODULE.summarize(runs)
        self.assertEqual(summary["software"]["successful_runs"], 2)
        self.assertAlmostEqual(summary["software"]["median_cpu_seconds"], 1.5)
        self.assertAlmostEqual(summary["software"]["median_cpu_ms_per_frame"], 25.0)
        self.assertEqual(summary["gpu"]["successful_runs"], 0)
        self.assertNotIn("median_cpu_seconds", summary["gpu"])
        table = MODULE.format_table(summary)
        self.assertIn("software", table)
        self.assertIn("gpu", table)


if __name__ == "__main__":
    unittest.main()
