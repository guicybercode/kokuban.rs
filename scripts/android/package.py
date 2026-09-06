#!/usr/bin/env python3
"""Add Android OS bridge bytecode and an optional SSH executable, then re-sign.

Application behavior remains in Rust. Java sources implement the Android input
connection contract that NativeActivity alone does not expose.
"""

import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import zipfile


def run(*command):
    subprocess.run([str(part) for part in command], check=True)


def java_tool(name):
    java_home = os.environ.get("JAVA_HOME")
    if java_home and (Path(java_home) / "bin" / name).is_file():
        return str(Path(java_home) / "bin" / name)
    found = shutil.which(name)
    if not found:
        raise RuntimeError(f"Java 17 tool is missing: {name}")
    return found


def is_signature(name):
    name = name.upper()
    return name == "META-INF/MANIFEST.MF" or (name.startswith("META-INF/") and name.endswith((".SF", ".RSA", ".DSA", ".EC")))


def add_payload(source, destination, dex, ssh_binary=None, abi=None):
    additions = {"classes.dex": dex}
    if ssh_binary:
        additions[f"lib/{abi}/libkokuban_ssh.so"] = ssh_binary
    with zipfile.ZipFile(source) as original, zipfile.ZipFile(destination, "w") as result:
        for item in original.infolist():
            if item.filename in additions or is_signature(item.filename):
                continue
            with original.open(item) as reader, result.open(item, "w") as writer:
                shutil.copyfileobj(reader, writer)
        for name, path in additions.items():
            result.write(path, name, compress_type=zipfile.ZIP_DEFLATED)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apk", type=Path, required=True)
    parser.add_argument("--profile", choices=("debug", "release"), required=True)
    parser.add_argument("--target", choices=("aarch64-linux-android", "x86_64-linux-android"), required=True)
    parser.add_argument("--ssh-binary", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    sdk = Path(os.environ["ANDROID_HOME"])
    build_tools = sdk / "build-tools/35.0.0"
    android_jar = sdk / "platforms/android-35/android.jar"
    sources = sorted((root / "android/java").rglob("*.java"))
    if not sources:
        raise RuntimeError("Android IME bridge sources are missing")
    manifest = subprocess.check_output([str(build_tools / "aapt"), "dump", "xmltree", str(args.apk), "AndroidManifest.xml"], text=True)
    launcher = subprocess.check_output([str(build_tools / "aapt"), "dump", "badging", str(args.apk)], text=True)
    if "launchable-activity: name='com.kokuban.terminal.KokubanActivity'" not in launcher:
        raise RuntimeError("APK launcher must be com.kokuban.terminal.KokubanActivity")
    if not re.search(r"android:hasCode\([^)]*\)=\(type 0x12\)0xffffffff", manifest):
        raise RuntimeError("APK application.has_code must be true for the IME bridge")
    if args.ssh_binary and not re.search(r"android:extractNativeLibs\([^)]*\)=\(type 0x12\)0xffffffff", manifest):
        raise RuntimeError("APK must extract native libraries to execute the SSH client")
    if args.ssh_binary:
        with args.ssh_binary.open("rb") as executable:
            if executable.read(4) != b"\x7fELF":
                raise RuntimeError("SSH executable is not an ELF binary")
    if args.profile == "debug":
        user_dir = Path(os.environ.get("ANDROID_USER_HOME", str(Path.home() / ".android")))
        keystore = os.environ.get("CARGO_APK_DEV_KEYSTORE", str(user_dir / "debug.keystore"))
        password = os.environ.get("CARGO_APK_DEV_KEYSTORE_PASSWORD", "android")
    else:
        keystore = os.environ["CARGO_APK_RELEASE_KEYSTORE"]
        password = os.environ["CARGO_APK_RELEASE_KEYSTORE_PASSWORD"]
    # Pass passwords by environment, never as process arguments or printed text.
    os.environ["KOKUBAN_APK_SIGNING_PASSWORD"] = password
    abi = {"aarch64-linux-android": "arm64-v8a", "x86_64-linux-android": "x86_64"}[args.target]
    with tempfile.TemporaryDirectory(prefix="android-package-", dir=root / "target") as directory:
        staging = Path(directory)
        classes = staging / "classes"
        dex = staging / "dex"
        classes.mkdir()
        dex.mkdir()
        run(java_tool("javac"), "--release", "8", "-classpath", android_jar,
            "-d", classes, *sources)
        run(build_tools / "d8", "--min-api", "26", "--lib", android_jar,
            "--output", dex, *sorted(classes.rglob("*.class")))
        unsigned = staging / "unsigned.apk"
        aligned = staging / "aligned.apk"
        signed = staging / "signed.apk"
        add_payload(args.apk, unsigned, dex / "classes.dex", args.ssh_binary, abi)
        run(build_tools / "zipalign", "-P", "16", "-f", "4", unsigned, aligned)
        run(build_tools / "apksigner", "sign", "--ks", keystore,
            "--ks-pass", "env:KOKUBAN_APK_SIGNING_PASSWORD",
            "--key-pass", "env:KOKUBAN_APK_SIGNING_PASSWORD", "--out", signed, aligned)
        run(build_tools / "apksigner", "verify", "--verbose", signed)
        run(build_tools / "zipalign", "-c", "-P", "16", "4", signed)
        with zipfile.ZipFile(signed) as apk:
            if "classes.dex" not in apk.namelist():
                raise RuntimeError("Signed APK is missing the IME bridge")
            if args.ssh_binary and f"lib/{abi}/libkokuban_ssh.so" not in apk.namelist():
                raise RuntimeError("Signed APK is missing the SSH executable")
        os.replace(signed, args.apk)


if __name__ == "__main__":
    main()
