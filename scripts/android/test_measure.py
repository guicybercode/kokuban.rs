"""Realistic toybox top output: avoid interpreting unparseable output as idle."""

import unittest

from measure import cpu_samples


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


if __name__ == "__main__":
    unittest.main()
