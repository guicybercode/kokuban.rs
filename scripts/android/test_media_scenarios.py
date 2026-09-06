import unittest

from media_scenarios import locate_photo, rectangle


class PixelEvidenceTests(unittest.TestCase):
    def test_rectangle_requires_rows_and_does_not_wrap(self):
        red = bytes((255, 0, 0))
        self.assertTrue(rectangle(red * 20, (5, 4), (255, 0, 0), 3, 3))
        self.assertFalse(rectangle(red * 20, (5, 4), (255, 0, 0), 6, 1))
        self.assertFalse(rectangle(red * 20, (5, 4), (255, 0, 0), 3, 5))

    def test_photo_landmarks_locate_source_and_reject_changed_pixels(self):
        photo = bytes((index % 251 for index in range(24 * 24 * 3)))
        screen = bytearray(40 * 40 * 3)
        for row in range(24):
            start = ((row + 5) * 40 + 7) * 3
            screen[start:start + 24 * 3] = photo[row * 24 * 3:(row + 1) * 24 * 3]
        self.assertEqual(locate_photo(screen, (40, 40), photo, (24, 24)), (7, 5))
        screen[((5 + 4) * 40 + 7 + 4) * 3] ^= 255
        self.assertIsNone(locate_photo(screen, (40, 40), photo, (24, 24)))


if __name__ == "__main__":
    unittest.main()
