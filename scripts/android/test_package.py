"""APK rewrite behavior: retained native bytes, new payload and no stale signatures."""

from pathlib import Path
import tempfile
import unittest
import zipfile

from package import add_payload


class ApkPayloadTests(unittest.TestCase):
    def test_replaces_bridge_preserves_native_code_and_removes_invalid_signatures(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / "original.apk"
            updated = root / "updated.apk"
            dex = root / "classes.dex"
            ssh = root / "ssh"
            dex.write_bytes(b"new dex")
            ssh.write_bytes(b"\x7fELFssh executable")
            with zipfile.ZipFile(original, "w") as apk:
                apk.writestr("AndroidManifest.xml", b"compiled manifest")
                apk.writestr("lib/arm64-v8a/libkokuban.so", b"native code")
                apk.writestr("META-INF/LICENSE", b"license must remain")
                apk.writestr("META-INF/MANIFEST.MF", b"stale signature manifest")
                apk.writestr("META-INF/CERT.RSA", b"stale signature")
                apk.writestr("META-INF/CERT.SF", b"stale signature")
                apk.writestr("classes.dex", b"old dex")
            add_payload(original, updated, dex, ssh, "arm64-v8a")
            with zipfile.ZipFile(updated) as apk:
                self.assertEqual(apk.read("AndroidManifest.xml"), b"compiled manifest")
                self.assertEqual(apk.read("lib/arm64-v8a/libkokuban.so"), b"native code")
                self.assertEqual(apk.read("META-INF/LICENSE"), b"license must remain")
                self.assertEqual(apk.read("classes.dex"), b"new dex")
                self.assertEqual(apk.read("lib/arm64-v8a/libkokuban_ssh.so"), b"\x7fELFssh executable")
                self.assertEqual(apk.namelist().count("classes.dex"), 1)
                self.assertNotIn("META-INF/MANIFEST.MF", apk.namelist())
                self.assertNotIn("META-INF/CERT.RSA", apk.namelist())
                self.assertNotIn("META-INF/CERT.SF", apk.namelist())


if __name__ == "__main__":
    unittest.main()
