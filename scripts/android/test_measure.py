"""Realistic toybox top output: avoid interpreting unparseable output as idle."""

import unittest

from device import Device
from measure import cpu_samples, frame_metrics, pss_kib


class CpuSamplesTests(unittest.TestCase):
    def test_discards_historical_first_sample_and_preserves_one_core_percent(self):
        output = """
  PID USER         PR  NI VIRT  RES  SHR S[%CPU] %MEM     TIME+ ARGS
  123 u0_a100      10 -10  10G  30M  20M S  5.0  1.0   0:00.30 com.kokuban.terminal
  PID USER         PR  NI VIRT  RES  SHR S %CPU %MEM     TIME+ ARGS
  123 u0_a100      10 -10  10G  30M  20M S  150  1.0   0:01.80 com.kokuban.terminal
  PID USER         PR  NI VIRT  RES  SHR S %CPU %MEM     TIME+ ARGS
  123 u0_a100      10 -10  10G  30M  20M S  0.0  1.0   0:01.80 com.kokuban.terminal
"""
        self.assertEqual(cpu_samples(output, "123"), [150.0, 0.0])

    def test_does_not_treat_unavailable_cpu_as_zero(self):
        self.assertEqual(cpu_samples("top: permission denied", "123"), [])

    def test_does_not_mix_another_process_into_results(self):
        output = """PID USER %CPU ARGS
123 app 20 com.kokuban.terminal
999 other 80 another.app
123 app 1.5 com.kokuban.terminal
"""
        self.assertEqual(cpu_samples(output, "123"), [1.5])

    def test_pss_supports_application_summary_and_native_process_totals(self):
        self.assertEqual(pss_kib("TOTAL PSS:    24576    TOTAL RSS: 32000"), 24576)
        self.assertEqual(pss_kib("      TOTAL     4096      2048       2048"), 4096)
        self.assertIsNone(pss_kib("No process found for: 123"))

    def test_process_tree_includes_shell_and_ssh_without_other_apps(self):
        device = Device.__new__(Device)
        device.shell = lambda *args: """PID PPID NAME
1 0 init
10 1 com.kokuban.terminal
11 10 sh
13 11 libkokuban_ssh.so
20 1 unrelated.app
21 20 sh
"""
        self.assertEqual([p["pid"] for p in device.process_tree("10")], ["10", "11", "13"])

    def test_frame_traces_preserve_missing_latency_and_measure_present_intervals(self):
        output = """I Kokuban: frame=1 monotonic_us=1000000 render_us=3000 input_to_output_present_us=null
I Kokuban: frame=2 monotonic_us=1100000 render_us=5000 input_to_output_present_us=12000
I Kokuban: frame=3 monotonic_us=1300000 render_us=1000 input_to_output_present_us=null
"""
        result = frame_metrics(output)
        self.assertEqual(result["input_to_output_present_ms_samples"], [12.0])
        self.assertEqual(result["present_intervals_us"], [100000, 200000])
        self.assertEqual(result["max_present_gap_ms"], 200.0)
        self.assertEqual(result["mean_render_ms"], 3.0)
        self.assertIsNone(frame_metrics("")["presentation_call_rate_hz"])

    def test_excludes_previously_observed_frames_even_with_same_logcat_second(self):
        output = """09-06 01:02:03.100 I Kokuban: frame=40 monotonic_us=100 render_us=90000 input_to_output_present_us=95000
09-06 01:02:03.900 I Kokuban: frame=41 monotonic_us=800100 render_us=2000 input_to_output_present_us=null
"""
        result = frame_metrics(output, after_frame=40)
        self.assertEqual([frame["frame"] for frame in result["frames"]], [41])
        self.assertEqual(result["mean_render_ms"], 2.0)
        self.assertEqual(result["input_to_output_present_ms_samples"], [])
        self.assertEqual(frame_metrics(output, after_frame=41)["frames"], [])


if __name__ == "__main__":
    unittest.main()
