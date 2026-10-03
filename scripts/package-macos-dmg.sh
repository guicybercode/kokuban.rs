#!/usr/bin/env bash
# Build Kokuban.app and a drag-to-install disk image from an existing binary.
# This does not compile, sign with a Developer ID, or notarize.
# Usage: bash scripts/package-macos-dmg.sh TAG TARGET --binary PATH [--notices FILE] [--output-dir DIR]
# KOKUBAN_PYTHON selects the interpreter (3.10+, needs a working plistlib and venv).
set -euo pipefail

release_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
"${KOKUBAN_PYTHON:-python3}" - "$release_root" "$@" <<'PY'
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

root = Path(sys.argv[1]).resolve()
parser = argparse.ArgumentParser(description="Package Kokuban.app and its disk image")
parser.add_argument("tag")
parser.add_argument("target")
parser.add_argument("--binary", required=True, type=Path)
parser.add_argument("--notices", type=Path, help="third-party license text to bundle")
parser.add_argument("--output-dir", type=Path, default=Path("dist"))
arguments = parser.parse_args(sys.argv[2:])

if sys.platform != "darwin":
    raise SystemExit("the disk image can only be built on macOS")
if not re.fullmatch(r"v[0-9]+\.[0-9]+(?:\.[0-9]+)?(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", arguments.tag):
    raise SystemExit("unsupported release tag format")
binary = arguments.binary.resolve(strict=True)
if not binary.is_file() or not os.access(binary, os.X_OK):
    raise SystemExit(f"not an executable file: {binary}")


def run(*command, **kwargs):
    subprocess.run(command, check=True, cwd=kwargs.pop("cwd", root), **kwargs)


metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=root, text=True))
package = next(item for item in metadata["packages"]
               if Path(item["manifest_path"]).resolve() == root / "Cargo.toml")
version = package["version"]

output = (root / arguments.output_dir if not arguments.output_dir.is_absolute() else arguments.output_dir)
output.mkdir(parents=True, exist_ok=True)
name = f"kokuban-{arguments.tag}-{arguments.target}"
image_path = output / f"{name}.dmg"
image_path.unlink(missing_ok=True)

with tempfile.TemporaryDirectory(prefix="kokuban-dmg-") as temporary:
    work = Path(temporary)
    app = work / "Kokuban.app"
    macos_dir = app / "Contents" / "MacOS"
    resources = app / "Contents" / "Resources"
    macos_dir.mkdir(parents=True)
    resources.mkdir(parents=True)
    shutil.copy2(binary, macos_dir / "kokuban")
    (macos_dir / "kokuban").chmod(0o755)

    # Finder shows this icon; the running app also sets it from the embedded PNG.
    iconset = work / "Kokuban.iconset"
    iconset.mkdir()
    artwork = root / "assets" / "kokuban-icon.png"
    for size in (16, 32, 128, 256, 512):
        for scale, suffix in ((1, ""), (2, "@2x")):
            pixels = size * scale
            run("sips", "-z", str(pixels), str(pixels), str(artwork),
                "--out", str(iconset / f"icon_{size}x{size}{suffix}.png"), stdout=subprocess.DEVNULL)
    run("iconutil", "-c", "icns", str(iconset), "-o", str(resources / "Kokuban.icns"))

    shutil.copy2(root / "LICENSE", resources / "LICENSE")
    if arguments.notices:
        shutil.copy2(arguments.notices.resolve(strict=True), resources / "THIRD_PARTY_LICENSES.txt")

    info = {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleDisplayName": "Kokuban",
        "CFBundleExecutable": "kokuban",
        "CFBundleIconFile": "Kokuban",
        "CFBundleIdentifier": "io.github.guicybercode.kokuban",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": "Kokuban",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        "LSApplicationCategoryType": "public.app-category.developer-tools",
        "LSMinimumSystemVersion": "11.0",
        "NSHighResolutionCapable": True,
        "NSHumanReadableCopyright": "BSD-4-Clause. See Resources/LICENSE.",
    }
    (app / "Contents" / "Info.plist").write_bytes(plistlib.dumps(info))

    # Ad-hoc signature only: without it macOS refuses to launch arm64 bundles
    # whose contents changed after linking. This is not Developer ID signing.
    run("codesign", "--force", "--sign", "-", "--identifier", "io.github.guicybercode.kokuban.terminal",
        str(macos_dir / "kokuban"))
    run("codesign", "--force", "--sign", "-", str(app))
    run("codesign", "--verify", "--deep", "--strict", str(app))

    # dmgbuild writes the window layout directly into .DS_Store, so no Finder
    # scripting is needed and the image builds the same way in CI.
    venv = work / "venv"
    run(sys.executable, "-m", "venv", str(venv))
    run(str(venv / "bin" / "python"), "-m", "pip", "install", "--quiet", "--disable-pip-version-check",
        "--require-hashes", "-r", str(root / "scripts" / "dmg-requirements.txt"))
    run(str(venv / "bin" / "dmgbuild"), "-s", str(root / "scripts" / "dmg-settings.py"),
        "-D", f"app={app}", "-D", f"root={root}", "Kokuban", str(image_path))

    # Give the disk image file itself the Kokuban icon. This lives in the
    # resource fork, so it survives local copies but not uploads that keep only
    # the data fork; the SHA-256 below covers the data fork and is unaffected.
    icon_tools = [shutil.which(tool) for tool in ("sips", "DeRez", "Rez", "SetFile")]
    file_icon = all(icon_tools)
    if file_icon:
        icns = work / "file-icon.icns"
        shutil.copy2(resources / "Kokuban.icns", icns)
        run("sips", "-i", str(icns), stdout=subprocess.DEVNULL)
        resource = work / "file-icon.rsrc"
        resource.write_bytes(subprocess.check_output(["DeRez", "-only", "icns", str(icns)], cwd=root))
        run("Rez", "-append", str(resource), "-o", str(image_path))
        run("SetFile", "-a", "C", str(image_path))
    else:
        print("skipping the disk image file icon: Xcode command line tools are unavailable",
              file=sys.stderr)

digest = hashlib.sha256(image_path.read_bytes()).hexdigest()
(output / f"{name}.dmg.sha256").write_text(f"{digest}  {name}.dmg\n")
print(json.dumps({"image": str(image_path), "sha256": digest, "bytes": image_path.stat().st_size,
                  "version": version, "file_icon": file_icon}, indent=2))
PY
