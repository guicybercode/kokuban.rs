import base64
import importlib.util
import io
from pathlib import Path
import random
import unittest
import zlib

spec = importlib.util.spec_from_file_location("kitty_media", Path(__file__).with_name("kitty-media.py"))
media = importlib.util.module_from_spec(spec)
spec.loader.exec_module(media)


class MediaTests(unittest.TestCase):
    def test_chunked_frame_roundtrips_and_finishes_once(self):
        pixels = random.Random(72).randbytes(64 * 64 * 4)
        chunks = list(media.kitty_frame(pixels, 64, 64))
        self.assertGreater(len(chunks), 1)
        payload = []
        for index, chunk in enumerate(chunks):
            control, encoded = chunk[3:-2].split(b";", 1)
            self.assertLessEqual(len(encoded), 4096)
            self.assertIn(b"m=0" if index == len(chunks) - 1 else b"m=1", control)
            payload.append(encoded)
        self.assertIn(b"a=T,f=32,s=64,v=64", chunks[0])
        self.assertEqual(zlib.decompress(base64.b64decode(b"".join(payload))), pixels)

    def test_partial_frame_fails_instead_of_showing_corruption(self):
        self.assertIsNone(media.read_frame(io.BytesIO(b""), 4))
        with self.assertRaises(ValueError):
            media.read_frame(io.BytesIO(b"abc"), 4)
        with self.assertRaises(ValueError):
            list(media.kitty_frame(b"abc", 1, 1))


if __name__ == "__main__":
    unittest.main()
