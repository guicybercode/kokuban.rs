#!/usr/bin/env python3
"""Compare software raster kernels against a Git revision using rustc -O.

Run on an otherwise idle host, for example:
  python3 scripts/benchmark-software-raster.py --baseline 9fb819d

This measures CPU rasterization of 120x40 cells, not terminal throughput,
presentation latency or performance relative to other terminals. No thresholds.
Both versions receive identical pixels; final accumulated frames are compared.
Untimed correctness tests are also needed because repeated blending converges.
"""

import argparse
from contextlib import nullcontext
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import tempfile


HARNESS = r'''
#[allow(dead_code)]
mod glyph_atlas {
    #[derive(Clone, Copy)]
    pub struct GlyphEntry {
        pub atlas_x: u32, pub atlas_y: u32, pub pixel_w: u32, pub pixel_h: u32,
        pub bearing_x: i32, pub bearing_y: i32,
    }
}
#[allow(dead_code)]
#[path = "before.rs"] mod before;
#[allow(dead_code)]
#[path = "after.rs"] mod after;
use std::{hint::black_box, time::Instant};
type Draw = fn(&mut [u32], (u32, u32), &[u8], (u32, u32),
    glyph_atlas::GlyphEntry, (i32, i32), u32);
type Fill = fn(&mut [u32], (u32, u32), (i32, i32), (u32, u32), u32, u8);

#[inline(never)]
fn measure(draw: Draw, fill: Fill, mode: &str, frames: usize) -> (f64, Vec<u32>) {
    let mut frame = vec![0x203040; 1200 * 800];
    let atlas: Vec<u8> = (0..1024 * 32).map(|i| (i * 73 % 256) as u8).collect();
    let glyph = glyph_atlas::GlyphEntry {
        atlas_x: 13, atlas_y: 5, pixel_w: 9, pixel_h: 17, bearing_x: 0, bearing_y: 0,
    };
    let start = Instant::now();
    for _ in 0..frames {
        for row in 0..40 {
            for column in 0..120 {
                let origin = (column * 10, row * 20);
                if mode == "glyphs" {
                    draw(black_box(&mut frame), (1200, 800), black_box(&atlas),
                        (1024, 32), glyph, origin, black_box(0xf0e0d0));
                } else {
                    fill(black_box(&mut frame), (1200, 800), origin, (10, 20),
                        black_box(0x123456), if mode == "opaque" { 255 } else { 180 });
                }
            }
        }
        black_box(&frame);
    }
    (start.elapsed().as_secs_f64() * 1000.0 / frames as f64, frame)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let samples: usize = args[1].parse().unwrap();
    let frames: usize = args[2].parse().unwrap();
    for mode in ["glyphs", "opaque", "alpha"] {
        // Hide function identities so LLVM cannot inline/specialize just one
        // side after a kernel changes its inlining cost. Both versions use
        // the same indirect-call measurement loop, including opaque controls.
        let before = || measure(black_box(before::draw_glyph_a8 as Draw),
            black_box(before::fill_rect as Fill), mode, frames);
        let after = || measure(black_box(after::draw_glyph_a8 as Draw),
            black_box(after::fill_rect as Fill), mode, frames);
        // Warm both versions, then alternate order to reduce thermal/order bias.
        assert_eq!(before().1, after().1);
        for sample in 0..samples {
            let (old, new) = if sample % 2 == 0 {
                (before(), after())
            } else {
                let new = after();
                (before(), new)
            };
            assert_eq!(old.1, new.1, "{mode} sample {sample}");
            println!("{mode} sample={sample} before_ms={:.4} after_ms={:.4} speedup={:.3}",
                old.0, new.0, old.0 / new.0);
        }
    }
}
'''


def positive_integer(value: str) -> int:
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return number


def resolve_revision(root: Path, reference: str) -> str:
    """Resolve a trusted local reference before using it in git show."""
    return subprocess.check_output(
        ["git", "rev-parse", "--verify", "--end-of-options", f"{reference}^{{commit}}"],
        cwd=root, text=True,
    ).strip()


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def artifact_directory(value: str) -> Path:
    directory = Path(value).expanduser().resolve()
    if directory.exists() and (not directory.is_dir() or any(directory.iterdir())):
        raise argparse.ArgumentTypeError("artifacts directory must be absent or empty")
    return directory


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, help="trusted local Git revision")
    parser.add_argument("--candidate", help="trusted local Git revision; default: working tree")
    parser.add_argument("--artifacts-dir", type=artifact_directory,
                        help="preserve sources, harness, binary, logs and provenance here")
    parser.add_argument("--samples", type=positive_integer, default=5)
    parser.add_argument("--frames", type=positive_integer, default=100)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    revision = resolve_revision(root, args.baseline)
    candidate_revision = resolve_revision(root, args.candidate) if args.candidate else None
    baseline = subprocess.check_output(
        ["git", "show", f"{revision}:src/software_raster.rs"], cwd=root,
    )
    candidate = (subprocess.check_output(
        ["git", "show", f"{candidate_revision}:src/software_raster.rs"], cwd=root,
    ) if candidate_revision else (root / "src/software_raster.rs").read_bytes())
    rustc_version = subprocess.check_output(["rustc", "--version", "--verbose"], text=True)
    print(f"baseline={revision}; candidate={candidate_revision or 'working-tree'}", flush=True)
    print(rustc_version, end="", flush=True)
    context = (nullcontext(args.artifacts_dir) if args.artifacts_dir else
               tempfile.TemporaryDirectory(prefix="kokuban-raster-bench-"))
    with context as temporary:
        directory = Path(temporary)
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "before.rs").write_bytes(baseline)
        (directory / "after.rs").write_bytes(candidate)
        (directory / "main.rs").write_text(HARNESS, encoding="utf-8")
        executable = directory / "benchmark"
        compile_command = ["rustc", "--edition=2021", "-O", "main.rs", "-o", "benchmark"]
        run_command = [str(executable), str(args.samples), str(args.frames)]
        provenance = {
            "schema_version": 1,
            "started_at_utc": datetime.now(timezone.utc).isoformat(),
            "baseline": {"requested_ref": args.baseline, "revision": revision},
            "candidate": {"requested_ref": args.candidate, "revision": candidate_revision,
                          "source": "git" if candidate_revision else "working-tree"},
            "driver_revision": resolve_revision(root, "HEAD"),
            "source_files_identical": baseline == candidate,
            "comparison_mode": "same-source-control" if baseline == candidate else "different-sources",
            "rustc_version": rustc_version,
            "platform": platform.platform(),
            "machine": platform.machine(),
            "compile_command": compile_command,
            "run_command": run_command,
            "configuration": {"samples": args.samples, "frames": args.frames},
            "sha256": {name: sha256((directory / name).read_bytes())
                       for name in ("before.rs", "after.rs", "main.rs")},
            "driver_sha256": sha256(Path(__file__).read_bytes()),
            "status": "prepared",
        }
        result_file = directory / "results.json"
        result_file.write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")
        try:
            compiled = subprocess.run(compile_command, cwd=directory, capture_output=True, text=True)
            (directory / "compile.stdout.log").write_text(compiled.stdout, encoding="utf-8")
            (directory / "compile.stderr.log").write_text(compiled.stderr, encoding="utf-8")
            compiled.check_returncode()
            provenance["sha256"]["benchmark"] = sha256(executable.read_bytes())
            measured = subprocess.run(run_command, capture_output=True, text=True)
            (directory / "benchmark.stdout.log").write_text(measured.stdout, encoding="utf-8")
            (directory / "benchmark.stderr.log").write_text(measured.stderr, encoding="utf-8")
            print(measured.stdout, end="", flush=True)
            measured.check_returncode()
            provenance["status"] = "complete"
        except subprocess.CalledProcessError as error:
            provenance["status"] = "failed"
            provenance["error"] = str(error)
            print(error.stderr or "", end="", flush=True)
            raise
        finally:
            result_file.write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
