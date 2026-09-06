"""Remote resize evidence must contain two complete, different PTY dimensions."""

import unittest

from ssh_smoke import parse_pty_size, validate_pty_resize


class PtyResizeEvidenceTests(unittest.TestCase):
    def test_empty_or_partial_output_cannot_pass_as_a_resize(self):
        for malformed in ("", "25", "25 ", "0 46", "25 0", "-1 46", "25 46 extra", "25\n46"):
            with self.subTest(output=malformed):
                self.assertIsNone(parse_pty_size(malformed))
                with self.assertRaisesRegex(AssertionError, "Invalid remote PTY"):
                    validate_pty_resize("25 46\n", malformed)

    def test_complete_positive_dimensions_preserve_rows_and_columns(self):
        self.assertEqual(parse_pty_size("25 46\n"), (25, 46))
        self.assertEqual(validate_pty_resize("25 46\n", "18 106\n"),
                         {"before": "25 46", "after": "18 106"})

    def test_unchanged_dimensions_fail(self):
        with self.assertRaisesRegex(AssertionError, "did not change"):
            validate_pty_resize("25 46\n", "25 46\n")


if __name__ == "__main__":
    unittest.main()
