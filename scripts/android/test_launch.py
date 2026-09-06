"""First-frame evidence belongs to the current native process and native tag."""

import unittest

from launch import has_first_frame


class FirstFrameEvidenceTests(unittest.TestCase):
    def test_accepts_native_frame_for_current_process(self):
        line = "09-06 02:40:14.052  2293  2356 I Kokuban : kokuban::android_window: first frame presented\n"
        self.assertTrue(has_first_frame(line, "2293"))

    def test_old_process_or_harness_marker_cannot_pass(self):
        log = ("09-06 03:22:01.123  1234  1235 I Kokuban : kokuban::android_window: first frame presented\n"
               "09-06 03:22:01.124  4321  4322 I KokubanHarness: kokuban::android_window: first frame presented\n"
               "09-06 03:22:01.124  4321  4322 I Kokuban : other::module: first frame presented\n"
               "09-06 03:22:01.125  4321  4322 I Kokuban : resumed\n")
        self.assertFalse(has_first_frame(log, "4321"))


if __name__ == "__main__":
    unittest.main()
