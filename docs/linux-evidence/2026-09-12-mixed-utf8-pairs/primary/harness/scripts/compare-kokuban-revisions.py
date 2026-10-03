#!/usr/bin/env python3
"""Measure two Kokuban binaries in alternating before/after pairs on Linux.

Reuses the cross-terminal runner's workloads, PTY driver and DSR barrier. This
measures processing throughput and protocol RTT, not rendered equivalence or
input-to-photon latency. Revision labels describe declared build provenance.
"""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import runpy
import sys
import time


COMPARATOR_PATH = Path(__file__).with_name("compare-terminal-performance.py")
COMPARATOR = runpy.run_path(str(COMPARATOR_PATH))
execute_sample = COMPARATOR["execute_sample"]
summarize = COMPARATOR["summarize"]
payloads = COMPARATOR["payloads"]
record = COMPARATOR["record"]
distribution = COMPARATOR["distribution"]
command_observation = COMPARATOR["command_observation"]
SIDES = ("before", "after")


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True, type=Path)
    parser.add_argument("--after", required=True, type=Path)
    parser.add_argument("--before-ref", required=True, help="declared source revision of the before binary")
    parser.add_argument("--after-ref", required=True, help="declared source revision of the after binary")
    parser.add_argument("--artifacts-dir", required=True, type=Path, help="new directory for retained evidence")
    parser.add_argument("--backend", choices=("x11", "wayland"), default="wayland")
    parser.add_argument("--samples", type=int, default=5, help="number of before/after pairs (minimum 3)")
    parser.add_argument("--bytes", type=int, default=32 * 1024 * 1024)
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--settle-seconds", type=float, default=1.0)
    parser.add_argument("--screen", choices=("alternate", "primary"), default="alternate")
    parser.add_argument("--columns", type=int, default=80)
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--font-pixels", type=float, default=14.0)
    parser.add_argument("--scrollback-lines", type=int, default=10000)
    parser.add_argument("--environment-note", default="")
    parser.add_argument("--allow-identical-binaries", action="store_true",
                        help="allow an intentional A/A variability control using the same executable")
    args = parser.parse_args(argv)
    if args.samples < 3 or args.bytes < 1024:
        parser.error("at least 3 pairs and at least 1024 bytes are required")
    if args.rows <= 0 or args.columns <= 0 or args.scrollback_lines < 0:
        parser.error("grid dimensions must be positive and history cannot be negative")
    if (not all(math.isfinite(value) for value in (args.font_pixels, args.timeout, args.settle_seconds))
            or args.font_pixels <= 0 or args.timeout <= 0 or args.settle_seconds < 0):
        parser.error("font size and timeout must be positive and finite; settle time must be nonnegative and finite")
    if not args.before_ref.strip() or not args.after_ref.strip():
        parser.error("source revision labels cannot be empty")
    return args


def initial_report(args) -> dict:
    return {
        "schema": 1, "kind": "paired-kokuban-revisions", "status": "initializing",
        "created_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "backend": args.backend, "screen": args.screen, "pairs_requested": args.samples,
        "bytes_requested_per_workload": args.bytes, "requested_geometry": [args.rows, args.columns],
        "font_pixels": args.font_pixels, "scrollback_lines": args.scrollback_lines,
        "timeout_seconds": args.timeout, "settle_seconds": args.settle_seconds,
        "environment_note": args.environment_note,
        "binaries_identical": None,
        "allow_identical_binaries": args.allow_identical_binaries,
        "comparison_mode": "revision-comparison",
        "source_refs_verified_by_runner": False,
        "rendering_equivalence_verified": False,
        "measurement_scope": "PTY processing throughput and DSR RTT; no presentation latency or terminal ranking",
        "terminals": {side: {"path": str(getattr(args, side)),
                             "source_ref": getattr(args, side + "_ref"), "samples": []}
                      for side in SIDES},
        "payloads": {}, "execution_order": [], "in_progress": None,
    }


def summarize_pairs(report: dict, args) -> bool:
    """Require matching inputs and geometry before exposing paired ratios."""
    summarize(report, args)
    comparability = report["comparability"]
    reasons = list(comparability["reasons"])
    if report["binaries_identical"] and not args.allow_identical_binaries:
        reasons.append("before/after executable hashes are identical")
    configs = set()
    for side, terminal in report["terminals"].items():
        for sample in terminal["samples"]:
            if sample["status"] != "passed":
                continue
            config = sample.get("config", {})
            if not config.get("sha256"):
                reasons.append(f"{side}: configuration hash missing")
            configs.add((config.get("sha256"), config.get("history_limit"), config.get("history_unit")))
            for name, observation in sample["measurements"]["workloads"].items():
                if observation["bytes"] != report["payloads"][name]["bytes"]:
                    reasons.append(f"{side}: byte count differs in {name}")
                for metric in ("write_and_dsr_seconds", "mib_per_second"):
                    value = observation[metric]
                    if not math.isfinite(value) or value <= 0:
                        reasons.append(f"{side}: invalid {metric} in {name}")
    if len(configs) != 1:
        reasons.append("before/after configuration or history differs or is missing")
    comparability["reasons"] = sorted(set(reasons))
    comparability["geometry_and_history_checks_passed"] = not reasons
    comparability["paired_inputs_validated"] = not reasons
    comparability["rendering_equivalence_verified"] = False
    comparability["ranking"] = None
    comparability["scope"] = report["measurement_scope"]
    report["paired_summary"] = None
    if reasons:
        return False
    report["paired_summary"] = {}
    for name in report["payloads"]:
        elapsed_ratios, throughput_ratios = [], []
        for before, after in zip(report["terminals"]["before"]["samples"],
                                 report["terminals"]["after"]["samples"]):
            first = before["measurements"]["workloads"][name]
            second = after["measurements"]["workloads"][name]
            elapsed_ratios.append(second["write_and_dsr_seconds"] / first["write_and_dsr_seconds"])
            throughput_ratios.append(second["mib_per_second"] / first["mib_per_second"])
        report["paired_summary"][name] = {
            "after_over_before_elapsed_ratio": distribution(elapsed_ratios),
            "after_over_before_throughput_ratio": distribution(throughput_ratios),
            "elapsed_change_percent": distribution([(ratio - 1) * 100 for ratio in elapsed_ratios]),
        }
    return True


def compare(args) -> int:
    output = args.artifacts_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = initial_report(args)
    record(output, "report.json", report)
    result = 1
    try:
        report.update(platform=platform.platform(), machine=platform.machine(), python=sys.version,
                      cpu_count=os.cpu_count(), cpu_affinity=sorted(os.sched_getaffinity(0)),
                      script_sha256=file_hash(Path(__file__)), comparator_sha256=file_hash(COMPARATOR_PATH),
                      pty_driver_helpers_sha256=file_hash(COMPARATOR_PATH.with_name("linux-resource-smoke.py")),
                      hardware=command_observation(["lscpu"]),
                      font_match=command_observation(["fc-match", "-f", "%{family}\n%{file}\n", COMPARATOR["FONT"]]),
                      environment={key: os.environ.get(key) for key in (
                          "DISPLAY", "WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE",
                          "LIBGL_ALWAYS_SOFTWARE", "MESA_LOADER_DRIVER_OVERRIDE", "GDK_SCALE",
                          "GDK_DPI_SCALE", "WINIT_X11_SCALE_FACTOR")})
        for side in SIDES:
            binary = getattr(args, side).resolve(strict=True)
            if not binary.is_file() or not os.access(binary, os.X_OK):
                raise ValueError(f"{side} binary is not an executable file: {binary}")
            terminal = report["terminals"][side]
            terminal.update(path=str(binary), sha256=file_hash(binary))
            version = command_observation([str(binary), "--version"])
            terminal["version_observation"] = version
            if version.get("status") != 0:
                raise ValueError(f"could not read {side} binary version: {version}")
            terminal["version"] = version["output"]
            record(output, "report.json", report)
        report["binaries_identical"] = (
            report["terminals"]["before"]["sha256"] == report["terminals"]["after"]["sha256"])
        if report["binaries_identical"]:
            report["comparison_mode"] = "same-executable-variability"
            report["measurement_scope"] = (
                "Same-executable A/A variability in PTY processing and DSR RTT; "
                "no revision speedup inference, presentation latency or terminal ranking")
            if not args.allow_identical_binaries:
                raise ValueError(
                    "before/after executable hashes are identical; verify the build outputs or use "
                    "--allow-identical-binaries for an intentional A/A variability control")
        for name, payload in payloads(args.bytes).items():
            path = output / (name + ".bin")
            path.write_bytes(payload)
            report["payloads"][name] = {"path": str(path), "bytes": len(payload),
                                        "sha256": hashlib.sha256(payload).hexdigest()}
        paths = {name: item["path"] for name, item in report["payloads"].items()}
        report["status"] = "running"
        record(output, "report.json", report)
        for pair in range(1, args.samples + 1):
            order = SIDES if pair % 2 else tuple(reversed(SIDES))
            for side in order:
                phase = {"pair": pair, "side": side}
                report["execution_order"].append(phase)
                report["in_progress"] = phase
                record(output, "report.json", report)
                terminal = report["terminals"][side]
                print(f"pair {pair}/{args.samples}: {side} ({terminal['source_ref']})", flush=True)
                try:
                    sample = execute_sample("kokuban", Path(terminal["path"]), terminal["version"],
                                            output / f"{pair:02d}-{side}", paths, args)
                except KeyboardInterrupt:
                    terminal["samples"].append({"pair": pair, "status": "interrupted"})
                    raise
                except Exception as error:
                    sample = {"status": "failed", "error": f"{type(error).__name__}: {error}"}
                terminal["samples"].append({**sample, "pair": pair})
                report["in_progress"] = None
                record(output, "report.json", report)
        result = 0 if summarize_pairs(report, args) else 1
        report["status"] = "passed" if result == 0 else "failed"
    except KeyboardInterrupt:
        report.update(status="interrupted", error="KeyboardInterrupt")
        result = 130
    except Exception as error:
        report.update(status="failed", error=f"{type(error).__name__}: {error}")
    finally:
        if report["status"] != "passed":
            try:
                summarize_pairs(report, args)
            except Exception as error:
                report["summary_error"] = f"{type(error).__name__}: {error}"
                report["paired_summary"] = None
        try:
            record(output, "report.json", report)
        except OSError as error:
            print(f"could not persist final report; earlier checkpoints may remain: {error}", file=sys.stderr)
            result = 1
    print(json.dumps({"report": str(output / "report.json"), "status": report["status"],
                      "comparability": report.get("comparability")}, indent=2))
    return result


def main(argv=None) -> int:
    args = parse_args(argv)
    if not sys.platform.startswith("linux"):
        print("the paired benchmark requires Linux", file=sys.stderr)
        return 2
    if args.backend == "x11" and not os.environ.get("DISPLAY"):
        print("X11 requires DISPLAY (use xvfb-run or an existing session)", file=sys.stderr)
        return 2
    if args.backend == "wayland" and not (os.environ.get("WAYLAND_DISPLAY") or os.environ.get("WAYLAND_SOCKET")):
        print("Wayland requires an existing native Wayland session", file=sys.stderr)
        return 2
    try:
        return compare(args)
    except OSError as error:
        print(f"could not create benchmark artifacts: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
