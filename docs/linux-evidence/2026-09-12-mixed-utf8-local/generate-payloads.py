#!/usr/bin/env python3
"""Regenerate the four exact local-screening inputs, outside timed intervals."""
import argparse
import ast
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def payloads():
    source = (ROOT / "reference/compare-terminal-performance.py").read_bytes()
    provenance = json.loads((ROOT / "source-provenance.json").read_text())
    expected = provenance["payload_generation"]
    if hashlib.sha256(source).hexdigest() != expected["source_sha256"]:
        raise ValueError("Reference source hash mismatch")
    function = next(
        node for node in ast.parse(source).body
        if isinstance(node, ast.FunctionDef) and node.name == "payloads"
    )
    namespace = {}
    exec(compile(ast.Module(body=[function], type_ignores=[]), "reference", "exec"), namespace)
    return namespace["payloads"](expected["requested_bytes"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    expected = json.loads((ROOT / "measurements/payloads.json").read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    for name, data in payloads().items():
        actual = {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        if actual != expected[name]:
            raise ValueError(f"Payload mismatch: {name}")
        (args.output / f"{name}.bin").write_bytes(data)
    (args.output / "payloads.json").write_bytes((ROOT / "measurements/payloads.json").read_bytes())
    print("Generated and verified four payloads")


if __name__ == "__main__":
    main()
